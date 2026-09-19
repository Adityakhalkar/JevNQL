//! Human-readable operator trees for EXPLAIN.

use std::fmt;

use crate::logical::{JoinType, LogicalPlan, Op};

fn join<T: fmt::Display>(items: &[T]) -> String {
    items.iter().map(ToString::to_string).collect::<Vec<_>>().join(", ")
}

/// One-line description of an operator, e.g. `TopK[20: total_spend DESC]`.
pub fn label<I>(op: &Op<I>) -> String {
    let body = match op {
        Op::Scan(s) => match &s.columns {
            Some(cols) => format!("{}: {}", s.table, cols.join(", ")),
            None => s.table.clone(),
        },
        Op::Filter(f) => f.predicate.to_string(),
        Op::Project(p) => join(&p.exprs),
        Op::Join(j) => {
            let keys: Vec<String> = j.on.iter().map(|k| format!("{} = {}", k.left, k.right)).collect();
            let kind = match j.join_type {
                JoinType::Inner => "inner",
                JoinType::Left => "left",
            };
            format!("{kind}: {}", keys.join(" AND "))
        }
        Op::Aggregate(a) => match a.group_by.is_empty() {
            true => join(&a.aggregates),
            false => format!("by {} | {}", a.group_by.join(", "), join(&a.aggregates)),
        },
        Op::Sort(s) => join(&s.keys),
        Op::TopK(t) => format!("{}: {}", t.k, join(&t.keys)),
        Op::Fetch(f) => {
            let mut s = format!("{} <- {}.{}: {}", f.output, f.on.left, f.on.right, f.fields.join(", "));
            if !f.order_by.is_empty() {
                s += &format!(" | order {}", join(&f.order_by));
            }
            if let Some(limit) = f.limit {
                s += &format!(" | limit {limit}");
            }
            s
        }
        Op::SemanticFilter(s) => {
            let out = s.output.as_ref().map(|o| format!(" -> {o}")).unwrap_or_default();
            format!("p >= {}{out}: {:?} | context: {}", s.threshold, s.predicate, s.context.join(", "))
        }
        Op::SemanticScore(s) => {
            format!("{} ({} levels): {:?} | context: {}", s.output, s.levels.len(), s.question, s.context.join(", "))
        }
        Op::SemanticChoice(s) => {
            let labels: Vec<&str> = s.options.iter().map(|o| o.label.as_str()).collect();
            format!("{} in {{{}}}: {:?} | context: {}", s.output, labels.join(", "), s.question, s.context.join(", "))
        }
    };
    format!("{}[{body}]", op.name())
}

fn write_tree(node: &LogicalPlan, prefix: &str, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    writeln!(f, "{}", label(&node.op))?;
    let inputs = node.inputs();
    for (i, input) in inputs.iter().enumerate() {
        let last = i + 1 == inputs.len();
        write!(f, "{prefix}{}", if last { "└── " } else { "├── " })?;
        write_tree(input, &format!("{prefix}{}", if last { "    " } else { "│   " }), f)?;
    }
    Ok(())
}

impl fmt::Display for LogicalPlan {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write_tree(self, "", f)
    }
}
