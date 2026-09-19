//! Arrow <-> JevIR type mapping.

use datafusion::arrow::datatypes::{DataType as Arrow, Schema as ArrowSchema};
use jevir::{DataType, Field, Schema};

use crate::error::ExecError;

pub fn to_jevir(t: &Arrow) -> DataType {
    use Arrow::*;
    match t {
        Int8 | Int16 | Int32 | Int64 | UInt8 | UInt16 | UInt32 | UInt64 => DataType::Int64,
        Float16 | Float32 | Float64 | Decimal32(..) | Decimal64(..) | Decimal128(..) | Decimal256(..) => {
            DataType::Float64
        }
        Utf8 | LargeUtf8 | Utf8View => DataType::Utf8,
        Boolean => DataType::Boolean,
        Date32 | Date64 => DataType::Date,
        Timestamp(..) => DataType::Timestamp,
        Interval(_) | Duration(_) => DataType::Interval,
        Null => DataType::Null,
        List(item) | LargeList(item) | ListView(item) | LargeListView(item) | FixedSizeList(item, _) => {
            DataType::list(to_jevir(item.data_type()))
        }
        Struct(fields) => DataType::Struct {
            fields: fields.iter().map(|f| Field::new(f.name(), to_jevir(f.data_type()))).collect(),
        },
        Dictionary(_, value) => to_jevir(value),
        _ => DataType::Other,
    }
}

pub fn schema_to_jevir(schema: &ArrowSchema) -> Result<Schema, ExecError> {
    Ok(Schema::new(schema.fields().iter().map(|f| Field::new(f.name(), to_jevir(f.data_type()))).collect())?)
}
