use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::Value;

use crate::db::Database;

pub type Row = BTreeMap<String, Value>;

#[derive(Clone, Debug, Default, Serialize)]
pub struct RecordSet {
    pub kind: String,
    pub columns: Vec<String>,
    pub rows: Vec<Row>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub diagnostics: Vec<String>,
    /// Per-property aggregate values from a Base view's `summaries` mapping.
    #[serde(skip_serializing_if = "Row::is_empty")]
    pub summaries: Row,
}

impl RecordSet {
    pub fn new(kind: impl Into<String>, rows: Vec<Row>) -> Self {
        let mut columns = Vec::new();
        for row in &rows {
            for key in row.keys() {
                if !columns.contains(key) {
                    columns.push(key.clone());
                }
            }
        }
        Self {
            kind: kind.into(),
            columns,
            rows,
            diagnostics: Vec::new(),
            summaries: Row::new(),
        }
    }
}

pub struct QueryContext<'a> {
    pub database: &'a Database,
    pub vault: &'a Path,
    pub current_file: Option<PathBuf>,
}

pub trait QueryAdapter: Send + Sync {
    fn name(&self) -> &'static str;
    fn execute(&self, context: &QueryContext<'_>, source: &str) -> anyhow::Result<RecordSet>;
}
