//! Type checking and schema inference for logical JevIR.

use std::collections::{HashMap, HashSet};

use crate::error::IrError;
use crate::expr::SortKey;
use crate::logical::{LogicalPlan, Op};
use crate::types::{DataType, Field, Schema, comparable};

/// Source of base-table schemas.
pub trait Catalog {
    fn table_schema(&self, table: &str) -> Option<Schema>;

    fn table_names(&self) -> Vec<String>;
}

impl Catalog for HashMap<String, Schema> {
    fn table_schema(&self, table: &str) -> Option<Schema> {
        self.get(table).cloned()
    }

    fn table_names(&self) -> Vec<String> {
        let mut names: Vec<_> = self.keys().cloned().collect();
        names.sort();
        names
    }
}

/// Validates a whole plan tree and returns its output schema.
pub fn infer_schema(plan: &LogicalPlan, catalog: &dyn Catalog) -> Result<Schema, IrError> {
    let inputs = plan.inputs().into_iter().map(|p| infer_schema(p, catalog)).collect::<Result<Vec<_>, _>>()?;
    derive_schema(&plan.op, &inputs, catalog).map_err(|e| match e {
        IrError::Invalid(m) => IrError::Invalid(format!("{}: {m}", plan.op.name())),
        e => e,
    })
}

/// Validates one operator given its (already validated) input schemas, in
/// [`Op::inputs`] order, and returns its output schema.
pub fn derive_schema<I>(op: &Op<I>, inputs: &[Schema], catalog: &dyn Catalog) -> Result<Schema, IrError> {
    let input = || &inputs[0];
    match op {
        Op::Scan(scan) => {
            let table = catalog.table_schema(&scan.table).ok_or_else(|| {
                IrError::invalid(format!("unknown table `{}`; available: {}", scan.table, catalog.table_names().join(", ")))
            })?;
            match &scan.columns {
                None => Ok(table),
                Some(cols) => Schema::new(cols.iter().map(|c| table.resolve(c).cloned()).collect::<Result<_, _>>()?),
            }
        }
        Op::Filter(f) => {
            match f.predicate.data_type(input())? {
                DataType::Boolean => Ok(input().clone()),
                t => Err(IrError::invalid(format!("predicate `{}` must be boolean, got {t}", f.predicate))),
            }
        }
        Op::Project(p) => {
            non_empty(&p.exprs, "exprs")?;
            let fields = p.exprs.iter().map(|e| Ok(Field::new(&e.name, e.expr.data_type(input())?))).collect::<Result<_, IrError>>()?;
            Schema::new(fields)
        }
        Op::Join(j) => {
            let (left, right) = (&inputs[0], &inputs[1]);
            non_empty(&j.on, "on")?;
            for key in &j.on {
                let (l, r) = (&left.resolve(&key.left)?.data_type, &right.resolve(&key.right)?.data_type);
                if !comparable(l, r) {
                    return Err(IrError::invalid(format!("join key `{}` ({l}) is not comparable with `{}` ({r})", key.left, key.right)));
                }
            }
            let right_keys: HashSet<&str> = j.on.iter().map(|k| k.right.as_str()).collect();
            let mut fields = left.fields().to_vec();
            for f in right.fields().iter().filter(|f| !right_keys.contains(f.name.as_str())) {
                if left.contains(&f.name) {
                    return Err(IrError::invalid(format!(
                        "column `{}` exists on both sides of the join; rename or drop it with a project step first",
                        f.name
                    )));
                }
                fields.push(f.clone());
            }
            Schema::new(fields)
        }
        Op::Aggregate(a) => {
            if a.group_by.is_empty() && a.aggregates.is_empty() {
                return Err(IrError::invalid("needs group_by columns or aggregates"));
            }
            let mut fields = a.group_by.iter().map(|g| input().resolve(g).cloned()).collect::<Result<Vec<_>, _>>()?;
            for agg in &a.aggregates {
                fields.push(Field::new(&agg.output, agg.data_type(input())?));
            }
            Schema::new(fields)
        }
        Op::Sort(s) => {
            check_sort_keys(&s.keys, input())?;
            Ok(input().clone())
        }
        Op::TopK(t) => {
            check_sort_keys(&t.keys, input())?;
            if t.k == 0 {
                return Err(IrError::invalid("k must be positive"));
            }
            Ok(input().clone())
        }
        Op::Fetch(f) => {
            let (input, source) = (&inputs[0], &inputs[1]);
            let (l, r) = (&input.resolve(&f.on.left)?.data_type, &source.resolve(&f.on.right)?.data_type);
            if !comparable(l, r) {
                return Err(IrError::invalid(format!("key `{}` ({l}) is not comparable with `{}` ({r})", f.on.left, f.on.right)));
            }
            non_empty(&f.fields, "fields")?;
            let items = f.fields.iter().map(|c| source.resolve(c).cloned()).collect::<Result<Vec<_>, _>>()?;
            Schema::new(items.clone())?; // rejects duplicate fields
            check_sort_keys_opt(&f.order_by, source)?;
            if f.limit == Some(0) {
                return Err(IrError::invalid("limit must be positive"));
            }
            append(input, &f.output, DataType::list(DataType::Struct { fields: items }))
        }
        Op::SemanticFilter(s) => {
            check_semantic(input(), &s.context, &s.predicate, "predicate")?;
            if !(0.0..=1.0).contains(&s.threshold) {
                return Err(IrError::invalid(format!("threshold must be in [0, 1], got {}", s.threshold)));
            }
            match &s.output {
                Some(out) => append(input(), out, DataType::Float64),
                None => Ok(input().clone()),
            }
        }
        Op::SemanticScore(s) => {
            check_semantic(input(), &s.context, &s.question, "question")?;
            if s.levels.len() < 2 || s.levels.iter().any(|l| l.trim().is_empty()) {
                return Err(IrError::invalid("levels must list at least two non-empty descriptions, lowest first"));
            }
            append(input(), &s.output, DataType::Float64)
        }
        Op::SemanticChoice(s) => {
            check_semantic(input(), &s.context, &s.question, "question")?;
            let labels: HashSet<&str> = s.options.iter().map(|o| o.label.as_str()).collect();
            if s.options.len() < 2 || labels.len() != s.options.len() || labels.iter().any(|l| l.trim().is_empty()) {
                return Err(IrError::invalid("options must have at least two distinct, non-empty labels"));
            }
            append(input(), &s.output, DataType::Utf8)
        }
    }
}

fn non_empty<T>(items: &[T], what: &str) -> Result<(), IrError> {
    if items.is_empty() {
        return Err(IrError::invalid(format!("`{what}` must not be empty")));
    }
    Ok(())
}

fn check_sort_keys(keys: &[SortKey], schema: &Schema) -> Result<(), IrError> {
    non_empty(keys, "keys")?;
    check_sort_keys_opt(keys, schema)
}

fn check_sort_keys_opt(keys: &[SortKey], schema: &Schema) -> Result<(), IrError> {
    for key in keys {
        let t = key.expr.data_type(schema)?;
        if !t.is_orderable() {
            return Err(IrError::invalid(format!("cannot sort by `{}` of type {t}", key.expr)));
        }
    }
    Ok(())
}

fn check_semantic(input: &Schema, context: &[String], text: &str, what: &str) -> Result<(), IrError> {
    non_empty(context, "context")?;
    for (i, c) in context.iter().enumerate() {
        input.resolve(c)?;
        if context[..i].contains(c) {
            return Err(IrError::invalid(format!("context lists `{c}` twice")));
        }
    }
    if text.trim().is_empty() {
        return Err(IrError::invalid(format!("{what} must not be empty")));
    }
    Ok(())
}

/// Input schema plus one new column, rejecting name clashes.
fn append(input: &Schema, name: &str, data_type: DataType) -> Result<Schema, IrError> {
    if input.contains(name) {
        return Err(IrError::invalid(format!("output column `{name}` already exists in the input")));
    }
    let mut fields = input.fields().to_vec();
    fields.push(Field::new(name, data_type));
    Schema::new(fields)
}
