//! NQL -> logical JevIR.
//!
//! Plans are emitted in reading order (fetch everything, then filter, then
//! judge, then rank); the optimizer reorders them for cost. Step ids are
//! named after clauses so validation errors point at the query text.

use jevir::expr::col;
use jevir::logical::{
    Aggregate, ChoiceOption, Fetch, Filter, Join, JoinKey, JoinType, Op, Project, Scan, SemanticChoice,
    SemanticFilter, SemanticScore, Sort, TopK,
};
use jevir::{AggregateExpr, Catalog, DataType, Expr, NamedExpr, Schema, ValidatedPlan};
use serde_json::{Map, Value, json};

use crate::NqlError;
use crate::lexer::lex;
use crate::parser::{Condition, Judge, Parser, Query, WithKind};

const DEFAULT_LEVELS: [&str; 3] = ["Not at all", "Somewhat", "Clearly and strongly"];

pub struct Compiled {
    /// The logical JevIR plan document (JSON).
    pub document: Value,
    pub plan: ValidatedPlan,
}

/// Compiles an NQL query against the tables in `catalog`.
pub fn compile(source: &str, catalog: &dyn Catalog) -> Result<Compiled, NqlError> {
    let query = Parser::new(lex(source)?).query()?;
    let document = Lowering { catalog, steps: Vec::new() }.lower(&query)?;
    let plan = jevir::decode(&document.to_string(), catalog).map_err(|e| NqlError::plan(e.to_string()))?;
    Ok(Compiled { document, plan })
}

struct Lowering<'a> {
    catalog: &'a dyn Catalog,
    steps: Vec<Value>,
}

impl Lowering<'_> {
    fn table(&self, name: &str) -> Result<Schema, NqlError> {
        self.catalog.table_schema(name).ok_or_else(|| {
            NqlError::plan(format!("unknown table `{name}`; available: {}", self.catalog.table_names().join(", ")))
        })
    }

    /// Appends a step and returns its id.
    fn add(&mut self, id: &str, op: Op<String>) -> String {
        let Value::Object(fields) = serde_json::to_value(op).expect("IR serializes") else { unreachable!() };
        let mut step = Map::new();
        step.insert("id".into(), json!(id));
        step.extend(fields);
        self.steps.push(Value::Object(step));
        id.to_string()
    }

    /// The column two tables join on: `on`, else their only shared column,
    /// else their only shared `*_id` column.
    fn join_key(&self, from: &str, base: &Schema, other_name: &str, other: &Schema, on: &Option<String>) -> Result<String, NqlError> {
        if let Some(key) = on {
            return Ok(key.clone());
        }
        let shared: Vec<&str> = base.names().into_iter().filter(|n| other.contains(n)).collect();
        let ids: Vec<&str> = shared.iter().copied().filter(|n| n.ends_with("_id")).collect();
        match (shared.as_slice(), ids.as_slice()) {
            ([one], _) | (_, [one]) => Ok(one.to_string()),
            ([], _) => Err(NqlError::plan(format!("`{from}` and `{other_name}` share no column; add ON <column>"))),
            _ => Err(NqlError::plan(format!(
                "`{from}` and `{other_name}` share several columns ({}); add ON <column>",
                shared.join(", ")
            ))),
        }
    }

    fn lower(mut self, q: &Query) -> Result<Value, NqlError> {
        let base = self.table(&q.from)?;
        let mut cur = self.add("from", Op::Scan(Scan { table: q.from.clone(), columns: None }));
        // columns a default RETURN shows: base columns plus computed ones (not histories)
        let mut visible: Vec<String> = base.names().iter().map(|n| n.to_string()).collect();
        let mut histories: Vec<String> = Vec::new();

        for w in &q.withs {
            let other = self.table(&w.table)?;
            let key = self.join_key(&q.from, &base, &w.table, &other, &w.on)?;
            let prefix = format!("with_{}", w.alias);
            let source = self.add(&format!("{prefix}_source"), Op::Scan(Scan { table: w.table.clone(), columns: None }));
            match &w.kind {
                WithKind::History { order, limit, fields } => {
                    let fields = fields.clone().unwrap_or_else(|| {
                        other.names().into_iter().filter(|n| *n != key).map(String::from).collect()
                    });
                    cur = self.add(
                        &prefix,
                        Op::Fetch(Fetch {
                            input: cur,
                            source,
                            on: JoinKey { left: key.clone(), right: key },
                            fields,
                            order_by: order.clone(),
                            limit: *limit,
                            output: w.alias.clone(),
                        }),
                    );
                    histories.push(w.alias.clone());
                }
                WithKind::Aggregate { func, arg, filter } => {
                    let mut input = source;
                    if let Some(predicate) = filter {
                        input = self.add(&format!("{prefix}_where"), Op::Filter(Filter { input, predicate: predicate.clone() }));
                    }
                    let agg = self.add(
                        &format!("{prefix}_agg"),
                        Op::Aggregate(Aggregate {
                            input,
                            group_by: vec![key.clone()],
                            aggregates: vec![AggregateExpr { func: *func, arg: arg.clone(), output: w.alias.clone() }],
                        }),
                    );
                    cur = self.add(
                        &prefix,
                        Op::Join(Join {
                            left: cur,
                            right: agg,
                            on: vec![JoinKey { left: key.clone(), right: key }],
                            join_type: JoinType::Left,
                        }),
                    );
                    visible.push(w.alias.clone());
                }
            }
        }

        // judgments read the histories by default, else the base table's text
        let default_context = || -> Result<Vec<String>, NqlError> {
            if !histories.is_empty() {
                return Ok(histories.clone());
            }
            let text: Vec<String> =
                base.fields().iter().filter(|f| f.data_type == DataType::Utf8).map(|f| f.name.clone()).collect();
            match text.is_empty() {
                true => Err(NqlError::plan(format!(
                    "a judgment needs text to read: add `WITH <table> AS <name> (LAST n BY <column>)` or `USING <column>`; `{}` has no text columns",
                    q.from
                ))),
                false => Ok(text),
            }
        };

        let facts: Vec<Expr> =
            q.conditions.iter().filter_map(|c| if let Condition::Expr(e) = c { Some(e.clone()) } else { None }).collect();
        if let Some(predicate) = Expr::conjunction(facts) {
            cur = self.add("find", Op::Filter(Filter { input: cur, predicate }));
        }
        let mut judgment = 0;
        for c in &q.conditions {
            if let Condition::Judgment { text, using } = c {
                judgment += 1;
                let context = using.clone().map_or_else(default_context, Ok)?;
                cur = self.add(
                    &format!("judge_{judgment}"),
                    Op::SemanticFilter(SemanticFilter {
                        input: cur,
                        context,
                        predicate: text.clone(),
                        threshold: 0.5,
                        output: None,
                    }),
                );
            }
        }

        for j in &q.judges {
            match j {
                Judge::Score { name, question, levels, using } => {
                    let levels = levels.clone().unwrap_or_else(|| DEFAULT_LEVELS.map(String::from).to_vec());
                    let context = using.clone().map_or_else(default_context, Ok)?;
                    cur = self.add(
                        &format!("score_{name}"),
                        Op::SemanticScore(SemanticScore { input: cur, context, question: question.clone(), levels, output: name.clone() }),
                    );
                }
                Judge::Classify { name, question, labels, using } => {
                    let context = using.clone().map_or_else(default_context, Ok)?;
                    let options = labels.iter().map(|l| ChoiceOption { label: l.clone(), description: None }).collect();
                    cur = self.add(
                        &format!("classify_{name}"),
                        Op::SemanticChoice(SemanticChoice { input: cur, context, question: question.clone(), options, output: name.clone() }),
                    );
                }
            }
            let (Judge::Score { name, .. } | Judge::Classify { name, .. }) = j;
            visible.push(name.clone());
        }

        if let Some(rank) = &q.rank {
            cur = match rank.limit {
                Some(k) => self.add("rank", Op::TopK(TopK { input: cur, keys: rank.keys.clone(), k })),
                None => self.add("rank", Op::Sort(Sort { input: cur, keys: rank.keys.clone() })),
            };
        }

        let exprs = match &q.returns {
            Some(items) => items
                .iter()
                .map(|item| match (&item.alias, &item.expr) {
                    (Some(name), e) => Ok(NamedExpr { name: name.clone(), expr: e.clone() }),
                    (None, Expr::Column { name }) => Ok(NamedExpr { name: name.clone(), expr: item.expr.clone() }),
                    (None, e) => Err(NqlError::plan(format!("name the computed RETURN item `{e}` with AS <name>"))),
                })
                .collect::<Result<Vec<_>, _>>()?,
            None => visible.iter().map(|n| NamedExpr { name: n.clone(), expr: col(n.as_str()) }).collect(),
        };
        self.add("return", Op::Project(Project { input: cur, exprs }));
        Ok(json!({"version": 1, "steps": self.steps}))
    }
}
