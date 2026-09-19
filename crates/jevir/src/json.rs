//! JSON boundary between frontends (e.g. the Python NL compiler) and the core.
//!
//! A plan document is a list of steps; each step names its inputs by the ids of
//! earlier steps, so documents are acyclic by construction:
//!
//! ```json
//! {"version": 1, "steps": [
//!   {"id": "orders", "op": "scan", "table": "orders"},
//!   {"id": "big", "op": "filter", "input": "orders",
//!    "predicate": {"kind": "binary", "op": ">", "left": {"kind": "column", "name": "amount"},
//!                  "right": {"kind": "literal", "value": 100}}}
//! ]}
//! ```
//!
//! The output is the step named by `"output"`, or else the last step.
//! Decoding validates every step; errors name the offending step id.

use std::collections::{HashMap, HashSet};
use std::convert::Infallible;
use std::sync::Arc;

use serde::Deserialize;
use serde_json::{Map, Value};

use crate::error::IrError;
use crate::logical::{LogicalPlan, Op, PlanRef};
use crate::types::Schema;
use crate::validate::{Catalog, derive_schema};

pub const VERSION: u32 = 1;

/// A decoded plan whose every step type-checked against the catalog.
#[derive(Debug, Clone)]
pub struct ValidatedPlan {
    pub plan: PlanRef,
    pub schema: Schema,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawDocument {
    #[serde(default = "default_version")]
    version: u32,
    steps: Vec<Map<String, Value>>,
    #[serde(default)]
    output: Option<String>,
}

fn default_version() -> u32 {
    VERSION
}

/// Parses and validates a plan document.
pub fn decode(text: &str, catalog: &dyn Catalog) -> Result<ValidatedPlan, IrError> {
    let doc: RawDocument = serde_json::from_str(text).map_err(|e| IrError::Malformed(e.to_string()))?;
    if doc.version != VERSION {
        return Err(IrError::Malformed(format!("unsupported version {}, expected {VERSION}", doc.version)));
    }
    if doc.steps.is_empty() {
        return Err(IrError::Malformed("plan has no steps".into()));
    }

    let mut built: HashMap<String, (PlanRef, Schema)> = HashMap::new();
    let mut used: HashSet<String> = HashSet::new();
    let mut order: Vec<String> = Vec::new();

    for (index, mut raw) in doc.steps.into_iter().enumerate() {
        let id = match raw.remove("id") {
            Some(Value::String(id)) if !id.trim().is_empty() => id,
            _ => return Err(IrError::Malformed(format!("step #{} needs a non-empty string `id`", index + 1))),
        };
        let step_err = |message: String| IrError::Step { step: id.clone(), message };
        if built.contains_key(&id) {
            return Err(step_err("duplicate step id".into()));
        }
        let op: Op<String> = serde_json::from_value(Value::Object(raw)).map_err(|e| step_err(e.to_string()))?;

        let mut schemas = Vec::new();
        for input in op.inputs() {
            let (_, schema) = built
                .get(input)
                .ok_or_else(|| step_err(format!("input `{input}` is not defined by an earlier step")))?;
            schemas.push(schema.clone());
            used.insert(input.clone());
        }
        let schema = derive_schema(&op, &schemas, catalog).map_err(|e| e.at_step(&id))?;
        let op = op
            .try_map_inputs(|input| Ok::<_, Infallible>(built[&input].0.clone()))
            .unwrap_or_else(|e| match e {});
        built.insert(id.clone(), (LogicalPlan::new(op), schema));
        order.push(id);
    }

    let output = doc.output.unwrap_or_else(|| order.last().cloned().expect("steps is non-empty"));
    if !built.contains_key(&output) {
        return Err(IrError::Malformed(format!("output `{output}` is not a step id")));
    }
    if let Some(dead) = order.iter().find(|id| **id != output && !used.contains(*id)) {
        return Err(IrError::Step { step: dead.clone(), message: "step is never used by the output".into() });
    }
    let (plan, schema) = built.remove(&output).expect("checked above");
    Ok(ValidatedPlan { plan, schema })
}

/// Serializes a plan tree as a plan document (steps in dependency order,
/// shared subplans emitted once).
pub fn encode(plan: &PlanRef) -> Value {
    fn visit(node: &PlanRef, ids: &mut HashMap<*const LogicalPlan, String>, steps: &mut Vec<Value>) {
        if ids.contains_key(&Arc::as_ptr(node)) {
            return;
        }
        for input in node.inputs() {
            visit(input, ids, steps);
        }
        let id = format!("s{}", steps.len() + 1);
        let op = node
            .op
            .clone()
            .try_map_inputs(|input| Ok::<_, Infallible>(ids[&Arc::as_ptr(&input)].clone()))
            .unwrap_or_else(|e| match e {});
        let Value::Object(fields) = serde_json::to_value(op).expect("IR always serializes") else {
            unreachable!("operators serialize as objects")
        };
        let mut step = Map::new();
        step.insert("id".into(), Value::String(id.clone()));
        step.extend(fields);
        steps.push(Value::Object(step));
        ids.insert(Arc::as_ptr(node), id);
    }

    let mut steps = Vec::new();
    visit(plan, &mut HashMap::new(), &mut steps);
    serde_json::json!({ "version": VERSION, "steps": steps })
}
