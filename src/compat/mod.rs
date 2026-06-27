mod base;
mod dataview;
mod expr;
mod tasks;

use std::collections::HashMap;

use anyhow::{Context, Result};
use chrono::{Local, TimeZone};

use crate::core::{QueryAdapter, QueryContext, RecordSet};
use crate::db::Database;
use crate::markdown::extract_tags;
use crate::model::PageRecord;
use serde_json::{Map, Value, json};

pub use base::BaseAdapter;
pub use dataview::{DataviewAdapter, DataviewJsAdapter};
pub use tasks::TasksAdapter;

pub struct CompatibilityEngine {
    adapters: HashMap<&'static str, Box<dyn QueryAdapter>>,
}

/// Vault-wide link lookup, grouped by source/target path, so building every
/// page's `file.links` / `file.backlinks` / `file.embeds` only costs one
/// query instead of one query per page.
pub(crate) struct LinkIndex {
    by_source: HashMap<String, Vec<Value>>,
    by_target: HashMap<String, Vec<Value>>,
}

impl LinkIndex {
    pub(crate) fn build(database: &Database) -> Result<Self> {
        let mut by_source: HashMap<String, Vec<Value>> = HashMap::new();
        let mut by_target: HashMap<String, Vec<Value>> = HashMap::new();
        for link in database.all_links()? {
            let path = link
                .target
                .as_ref()
                .map(|target| target.path.clone())
                .unwrap_or(link.raw_target.clone());
            let display = link
                .target
                .as_ref()
                .map(|target| target.title.clone())
                .unwrap_or_else(|| link.raw_target.clone());
            let value = json!({
                "__kind": "link",
                "path": path,
                "display": display,
                "embed": link.embed,
            });
            by_source
                .entry(link.source.path.clone())
                .or_default()
                .push(value.clone());
            if let Some(target) = &link.target {
                by_target
                    .entry(target.path.clone())
                    .or_default()
                    .push(json!({
                        "__kind": "link",
                        "path": link.source.path,
                        "display": link.source.title,
                        "embed": link.embed,
                    }));
            }
        }
        Ok(Self {
            by_source,
            by_target,
        })
    }

    fn links_for(&self, path: &str) -> Vec<Value> {
        self.by_source.get(path).cloned().unwrap_or_default()
    }

    fn embeds_for(&self, path: &str) -> Vec<Value> {
        self.links_for(path)
            .into_iter()
            .filter(|link| link.get("embed").and_then(Value::as_bool).unwrap_or(false))
            .collect()
    }

    fn backlinks_for(&self, path: &str) -> Vec<Value> {
        self.by_target.get(path).cloned().unwrap_or_default()
    }
}

fn date_field(epoch_seconds: i64) -> Value {
    let datetime = Local
        .timestamp_opt(epoch_seconds, 0)
        .single()
        .unwrap_or_else(Local::now)
        .naive_local();
    json!({"__kind": "date", "value": datetime.format("%Y-%m-%dT%H:%M:%S%.3f").to_string()})
}

fn page_value(page: &PageRecord, links: &LinkIndex) -> Value {
    let normalized_metadata = normalize_value(page.metadata.clone());
    let mut root = normalized_metadata
        .as_object()
        .cloned()
        .unwrap_or_else(Map::new);
    let mut tags = root
        .get("tags")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    for tag in extract_tags(&page.body) {
        let value = Value::String(tag);
        if !tags.contains(&value) {
            tags.push(value);
        }
    }
    let tags = Value::Array(tags);
    let name = page
        .path
        .rsplit('/')
        .next()
        .unwrap_or(&page.path)
        .trim_end_matches(".md")
        .to_owned();
    root.insert("note".to_owned(), Value::Object(root.clone()));
    root.insert(
        "file".to_owned(),
        json!({
            "__kind": "file",
            "path": page.path,
            "name": name,
            "basename": name,
            "folder": page.path.rsplit_once('/').map(|(folder, _)| folder).unwrap_or(""),
            "ext": "md",
            "link": {"__kind": "link", "path": page.path, "display": page.title},
            "size": page.size,
            "mtime": date_field(page.mtime),
            "ctime": date_field(page.ctime),
            "tags": tags,
            "properties": normalized_metadata,
            "frontmatter": normalized_metadata,
            "links": links.links_for(&page.path),
            "embeds": links.embeds_for(&page.path),
            "backlinks": links.backlinks_for(&page.path),
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
