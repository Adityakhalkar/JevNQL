//! JevIR type system.
//!
//! Every relation flowing between operators has a [`Schema`]: an ordered list of
//! typed fields. Semantic operators produce typed columns too (probabilities,
//! scores, labels), so downstream deterministic operators can consume them.
//! Names mirror Arrow so the executor can map them one-to-one.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::error::IrError;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DataType {
    Int64,
    Float64,
    Utf8,
    Boolean,
    Date,
    Timestamp,
    Interval,
    Null,
    List { item: Box<DataType> },
    Struct { fields: Vec<Field> },
    /// Source type JevIR does not model: passes through, unusable in expressions.
    Other,
}

impl DataType {
    pub fn is_numeric(&self) -> bool {
        matches!(self, DataType::Int64 | DataType::Float64)
    }

    pub fn is_temporal(&self) -> bool {
        matches!(self, DataType::Date | DataType::Timestamp)
    }

    /// Scalar types with a total order (usable in sort keys, min/max).
    pub fn is_orderable(&self) -> bool {
        self.is_numeric() || self.is_temporal() || matches!(self, DataType::Utf8 | DataType::Boolean)
    }

    pub fn list(item: DataType) -> DataType {
        DataType::List { item: Box::new(item) }
    }
}

impl fmt::Display for DataType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DataType::Int64 => f.write_str("int64"),
            DataType::Float64 => f.write_str("float64"),
            DataType::Utf8 => f.write_str("utf8"),
            DataType::Boolean => f.write_str("boolean"),
            DataType::Date => f.write_str("date"),
            DataType::Timestamp => f.write_str("timestamp"),
            DataType::Interval => f.write_str("interval"),
            DataType::Null => f.write_str("null"),
            DataType::Other => f.write_str("other"),
            DataType::List { item } => write!(f, "list<{item}>"),
            DataType::Struct { fields } => {
                f.write_str("struct<")?;
                for (i, field) in fields.iter().enumerate() {
                    if i > 0 {
                        f.write_str(", ")?;
                    }
                    write!(f, "{}: {}", field.name, field.data_type)?;
                }
                f.write_str(">")
            }
        }
    }
}

/// Whether `a <cmp> b` is a valid comparison.
pub fn comparable(a: &DataType, b: &DataType) -> bool {
    use DataType::*;
    match (a, b) {
        (Null, _) | (_, Null) => true,
        _ if a.is_numeric() && b.is_numeric() => true,
        _ if a.is_temporal() && b.is_temporal() => true,
        (Utf8, Utf8) | (Boolean, Boolean) => true,
        _ => false,
    }
}

/// Common supertype of two types (for `coalesce`), if any.
pub fn unify(a: &DataType, b: &DataType) -> Option<DataType> {
    use DataType::*;
    match (a, b) {
        (Null, t) | (t, Null) => Some(t.clone()),
        _ if a == b => Some(a.clone()),
        _ if a.is_numeric() && b.is_numeric() => Some(Float64),
        _ if a.is_temporal() && b.is_temporal() => Some(Timestamp),
        _ => None,
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Field {
    pub name: String,
    pub data_type: DataType,
}

impl Field {
    pub fn new(name: impl Into<String>, data_type: DataType) -> Self {
        Self { name: name.into(), data_type }
    }
}

/// Ordered, name-unique list of fields.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Schema {
    fields: Vec<Field>,
}

impl Schema {
    pub fn new(fields: Vec<Field>) -> Result<Self, IrError> {
        for (i, f) in fields.iter().enumerate() {
            if fields[..i].iter().any(|g| g.name == f.name) {
                return Err(IrError::invalid(format!("duplicate column name `{}`", f.name)));
            }
        }
        Ok(Self { fields })
    }

    pub fn fields(&self) -> &[Field] {
        &self.fields
    }

    pub fn field(&self, name: &str) -> Option<&Field> {
        self.fields.iter().find(|f| f.name == name)
    }

    pub fn contains(&self, name: &str) -> bool {
        self.field(name).is_some()
    }

    pub fn names(&self) -> Vec<&str> {
        self.fields.iter().map(|f| f.name.as_str()).collect()
    }

    /// Looks up a column, with an error listing the available ones.
    pub fn resolve(&self, name: &str) -> Result<&Field, IrError> {
        self.field(name).ok_or_else(|| {
            IrError::invalid(format!("unknown column `{name}`; available: {}", self.names().join(", ")))
        })
    }
}

impl fmt::Display for Schema {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("(")?;
        for (i, field) in self.fields.iter().enumerate() {
            if i > 0 {
                f.write_str(", ")?;
            }
            write!(f, "{}: {}", field.name, field.data_type)?;
        }
        f.write_str(")")
    }
}
