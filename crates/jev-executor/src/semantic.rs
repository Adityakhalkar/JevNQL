//! Semantic batch execution (`JevBatchExec`).
//!
//! For each input row, the context columns are serialized as a JSON state.
//! Identical states are deduplicated, cached answers are reused, and one
//! request per remaining state carries every op's question. Answers become
//! typed columns; filters are applied last, preserving row order.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use datafusion::arrow::array::{ArrayRef, BooleanArray, Float64Array, RecordBatch, StringArray};
use datafusion::arrow::compute::filter_record_batch;
use datafusion::arrow::datatypes::{DataType as ArrowType, Field as ArrowField, Schema as ArrowSchema};
use datafusion::arrow::json::ArrayWriter;
use futures::{StreamExt, TryStreamExt};
use jev_provider::{Answer, ChoiceOption, Question, SemanticBackend, SemanticRequest, approx_tokens};
use jevir::physical::{CachePolicy, JevBatchExec, SemanticOp};
use serde_json::{Map, Value};

use crate::error::ExecError;
use crate::metrics::ExecMetrics;

type CacheKey = (String, String, Question);

/// In-memory answers keyed by (backend, state, question).
#[derive(Default)]
pub(crate) struct SemanticCache {
    entries: Mutex<HashMap<CacheKey, Answer>>,
}

impl SemanticCache {
    fn get(&self, key: &CacheKey) -> Option<Answer> {
        self.entries.lock().expect("cache lock").get(key).cloned()
    }

    fn put(&self, key: CacheKey, answer: Answer) {
        self.entries.lock().expect("cache lock").insert(key, answer);
    }
}

pub(crate) struct SemanticRuntime<'a> {
    pub backend: &'a dyn SemanticBackend,
    pub cache: &'a SemanticCache,
    pub max_rows: usize,
}

impl SemanticRuntime<'_> {
    pub async fn evaluate(
        &self,
        exec: &JevBatchExec,
        input: RecordBatch,
        metrics: &mut ExecMetrics,
    ) -> Result<RecordBatch, ExecError> {
        let start = Instant::now();
        let rows = input.num_rows();
        if rows > self.max_rows {
            return Err(ExecError::SemanticBudget { rows, max: self.max_rows });
        }
        metrics.semantic_rows += rows;

        // one JSON state per row, deduplicated
        let states = row_states(&input, &exec.context)?;
        let mut distinct: Vec<&str> = Vec::new();
        let mut index: HashMap<&str, usize> = HashMap::new();
        let row_state: Vec<usize> = states
            .iter()
            .map(|s| {
                *index.entry(s.as_str()).or_insert_with(|| {
                    distinct.push(s.as_str());
                    distinct.len() - 1
                })
            })
            .collect();
        metrics.distinct_states += distinct.len();

        let info = self.backend.info();
        if let Some(state) = distinct.iter().find(|s| approx_tokens(s) > info.max_state_tokens) {
            return Err(ExecError::StateTooLarge { tokens: approx_tokens(state), limit: info.max_state_tokens });
        }

        let questions: Vec<(String, Question)> =
            exec.ops.iter().enumerate().map(|(i, op)| (format!("q{i}"), question(op))).collect();
        let cached = exec.cache == CachePolicy::ReadWrite;
        let key = |state: &str, q: &Question| (info.name.clone(), state.to_string(), q.clone());

        // answers[state][question], filled from cache, then from the backend
        let mut answers: Vec<Vec<Option<Answer>>> = vec![vec![None; questions.len()]; distinct.len()];
        let mut pending: Vec<(usize, Vec<usize>)> = Vec::new();
        for (si, state) in distinct.iter().enumerate() {
            let mut missing = Vec::new();
            for (qi, (_, q)) in questions.iter().enumerate() {
                match cached.then(|| self.cache.get(&key(state, q))).flatten() {
                    Some(a) => {
                        answers[si][qi] = Some(a);
                        metrics.cache_hits += 1;
                    }
                    None => missing.push(qi),
                }
            }
            if !missing.is_empty() {
                pending.push((si, missing));
            }
        }

        let responses: Vec<_> = futures::stream::iter(pending.into_iter().map(|(si, missing)| {
            let request = SemanticRequest {
                state: serde_json::from_str(distinct[si]).expect("states are serialized JSON"),
                questions: missing.iter().map(|&qi| questions[qi].clone()).collect(),
            };
            async move { self.backend.evaluate(&request).await.map(|r| (si, missing, r)) }
        }))
        .buffer_unordered(exec.concurrency.max(1))
        .try_collect()
        .await?;

        for (si, missing, response) in responses {
            metrics.requests += 1;
            metrics.questions += missing.len();
            metrics.input_tokens += response.input_tokens;
            for qi in missing {
                let (id, q) = &questions[qi];
                let answer = response
                    .answers
                    .get(id)
                    .cloned()
                    .ok_or_else(|| ExecError::Internal(format!("backend returned no answer for `{id}`")))?;
                if cached {
                    self.cache.put(key(distinct[si], q), answer.clone());
                }
                answers[si][qi] = Some(answer);
            }
        }

        // answers -> columns and filter mask
        let answer = |row: usize, qi: usize| answers[row_state[row]][qi].as_ref().expect("every question answered");
        let mismatch = |op: &SemanticOp| ExecError::Internal(format!("wrong answer type for `{op}`"));
        let mut fields: Vec<ArrowField> = input.schema().fields().iter().map(|f| f.as_ref().clone()).collect();
        let mut columns: Vec<ArrayRef> = input.columns().to_vec();
        let mut keep = vec![true; rows];
        for (qi, op) in exec.ops.iter().enumerate() {
            match op {
                SemanticOp::Filter { threshold, output, .. } => {
                    let probs = (0..rows)
                        .map(|r| match answer(r, qi) {
                            Answer::Noul { probability } => Ok(*probability),
                            _ => Err(mismatch(op)),
                        })
                        .collect::<Result<Vec<_>, _>>()?;
                    for (r, p) in probs.iter().enumerate() {
                        keep[r] &= *p >= *threshold;
                    }
                    if let Some(name) = output {
                        fields.push(ArrowField::new(name, ArrowType::Float64, true));
                        columns.push(Arc::new(Float64Array::from(probs)));
                    }
                }
                SemanticOp::Score { output, .. } => {
                    let values = (0..rows)
                        .map(|r| match answer(r, qi) {
                            Answer::Score { value, .. } => Ok(*value),
                            _ => Err(mismatch(op)),
                        })
                        .collect::<Result<Vec<_>, _>>()?;
                    fields.push(ArrowField::new(output, ArrowType::Float64, true));
                    columns.push(Arc::new(Float64Array::from(values)));
                }
                SemanticOp::Choice { output, .. } => {
                    let labels = (0..rows)
                        .map(|r| match answer(r, qi) {
                            Answer::Choice { label, .. } => Ok(label.clone()),
                            _ => Err(mismatch(op)),
                        })
                        .collect::<Result<Vec<_>, _>>()?;
                    fields.push(ArrowField::new(output, ArrowType::Utf8, true));
                    columns.push(Arc::new(StringArray::from(labels)));
                }
            }
        }
        let batch = RecordBatch::try_new(Arc::new(ArrowSchema::new(fields)), columns)?;
        let out = filter_record_batch(&batch, &BooleanArray::from(keep))?;
        metrics.semantic_time += start.elapsed();
        Ok(out)
    }
}

fn question(op: &SemanticOp) -> Question {
    match op {
        SemanticOp::Filter { predicate, .. } => Question::Noul { instructions: predicate.clone() },
        SemanticOp::Score { question, levels, .. } => {
            Question::Score { instructions: question.clone(), levels: levels.clone() }
        }
        SemanticOp::Choice { question, options, .. } => Question::Choice {
            instructions: question.clone(),
            options: options
                .iter()
                .map(|o| ChoiceOption { label: o.label.clone(), description: o.description.clone() })
                .collect(),
        },
    }
}

/// Serializes each row's context columns as a JSON object (nulls omitted).
fn row_states(batch: &RecordBatch, context: &[String]) -> Result<Vec<String>, ExecError> {
    let schema = batch.schema();
    let indices = context
        .iter()
        .map(|c| schema.index_of(c).map_err(|_| ExecError::Internal(format!("context column `{c}` missing"))))
        .collect::<Result<Vec<_>, _>>()?;
    let projected = batch.project(&indices)?;
    let mut writer = ArrayWriter::new(Vec::new());
    writer.write(&projected)?;
    writer.finish()?;
    let bytes = writer.into_inner();
    if bytes.is_empty() {
        return Ok(Vec::new());
    }
    let rows: Vec<Map<String, Value>> =
        serde_json::from_slice(&bytes).map_err(|e| ExecError::Internal(format!("row serialization: {e}")))?;
    Ok(rows.into_iter().map(|r| Value::Object(r).to_string()).collect())
}
