mod base;
mod dataview;
mod expr;
mod tasks;

use std::collections::HashMap;

use anyhow::{Context, Result};

use crate::core::{QueryAdapter, QueryContext, RecordSet};
use crate::model::PageRecord;
use serde_json::{Map, Value, json};

pub use base::BaseAdapter;
pub use dataview::{DataviewAdapter, DataviewJsAdapter};
pub use tasks::TasksAdapter;

pub struct CompatibilityEngine {
    adapters: HashMap<&'static str, Box<dyn QueryAdapter>>,
}

fn page_value(page: &PageRecord) -> Value {
    let normalized_metadata = normalize_value(page.metadata.clone());
    let mut root = normalized_metadata
        .as_object()
        .cloned()
        .unwrap_or_else(Map::new);
    let tags = root
        .get("tags")
        .cloned()
        .unwrap_or_else(|| Value::Array(Vec::new()));
    root.insert(
        "file".to_owned(),
        json!({
            "path": page.path,
            "name": page.path.rsplit('/').next().unwrap_or(&page.path).trim_end_matches(".md"),
            "folder": page.path.rsplit_once('/').map(|(folder, _)| folder).unwrap_or(""),
            "ext": "md",
            "link": {"path": page.path, "display": page.title},
            "size": page.size,
            "mtime": page.mtime,
            "tags": tags,
            "frontmatter": normalized_metadata,
        }),
    );
    Value::Object(root)
}

fn normalize_value(value: Value) -> Value {
    match value {
        Value::String(value) => parse_wiki_link(&value).unwrap_or(Value::String(value)),
        Value::Array(values) => Value::Array(values.into_iter().map(normalize_value).collect()),
        Value::Object(values) => Value::Object(
            values
                .into_iter()
                .map(|(key, value)| (key, normalize_value(value)))
                .collect(),
        ),
        value => value,
    }
}

fn parse_wiki_link(value: &str) -> Option<Value> {
    let inner = value.trim().strip_prefix("[[")?.strip_suffix("]]")?;
    let (path, display) = inner.split_once('|').unwrap_or((inner, inner));
    Some(json!({"path": path, "display": display}))
}

impl CompatibilityEngine {
    pub fn standard() -> Self {
        let mut engine = Self {
            adapters: HashMap::new(),
        };
        engine.register(Box::new(TasksAdapter));
        engine.register(Box::new(BaseAdapter));
        engine.register(Box::new(DataviewAdapter));
        engine.register(Box::new(DataviewJsAdapter));
        engine
    }

    pub fn register(&mut self, adapter: Box<dyn QueryAdapter>) {
        self.adapters.insert(adapter.name(), adapter);
    }

    pub fn execute(
        &self,
        language: &str,
        context: &QueryContext<'_>,
        source: &str,
    ) -> Result<RecordSet> {
        self.adapters
            .get(language)
            .with_context(|| format!("unknown compatibility query language: {language}"))?
            .execute(context, source)
    }

    pub fn languages(&self) -> Vec<&'static str> {
        let mut languages: Vec<_> = self.adapters.keys().copied().collect();
        languages.sort_unstable();
        languages
    }
}
