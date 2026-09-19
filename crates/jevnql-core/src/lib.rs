//! JevNQL engine: the public API over the Rust core.
//!
//! Frontends hand the engine logical JevIR (JSON); the engine validates it
//! against the registered data and, in later steps, optimizes and executes it.

use std::path::Path;

pub use jev_executor::{ExecError, TableProfile};
use jev_executor::Session;
use jevir::{Catalog, IrError, ValidatedPlan};

pub struct Engine {
    session: Session,
}

impl Engine {
    /// Opens an engine over `.csv` / `.parquet` files (one table per file).
    pub async fn open(files: &[impl AsRef<Path>]) -> Result<Self, ExecError> {
        let mut session = Session::new();
        for file in files {
            session.register_file(file).await?;
        }
        Ok(Self { session })
    }

    pub fn session(&self) -> &Session {
        &self.session
    }

    /// Profiles of every registered table, sorted by name.
    pub async fn profiles(&self) -> Result<Vec<TableProfile>, ExecError> {
        let mut out = Vec::new();
        for table in self.session.table_names() {
            out.push(self.session.profile(&table).await?);
        }
        Ok(out)
    }

    /// Decodes and type-checks a logical JevIR plan document.
    pub fn validate(&self, plan_json: &str) -> Result<ValidatedPlan, IrError> {
        jevir::decode(plan_json, &self.session)
    }
}
