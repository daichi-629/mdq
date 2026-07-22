use std::collections::BTreeMap;

use schemars::schema_for;
use serde::Serialize;
use serde_json::Value;

use crate::core::RecordSet;
use crate::db::Status;
use crate::model::{ContextItem, GraphOutput, IndexOutput, LinkRef, NoteRef, SearchHit};

#[derive(Serialize)]
pub struct CommandJsonSchemas {
    pub index: Value,
    pub search: Value,
    pub query_native: Value,
    pub query_record_set: Value,
    pub backlinks: Value,
    pub links: Value,
    pub unresolved_links: Value,
    pub graph: Value,
    pub pipeline_hits: Value,
    pub pipeline_context: Value,
    pub status: Value,
}

pub fn command_json_schemas() -> CommandJsonSchemas {
    CommandJsonSchemas {
        index: schema_value::<IndexOutput>(),
        search: schema_value::<Vec<ContextItem>>(),
        query_native: schema_value::<Vec<NoteRef>>(),
        query_record_set: schema_value::<RecordSet>(),
        backlinks: schema_value::<GraphOutput>(),
        links: schema_value::<Vec<LinkRef>>(),
        unresolved_links: schema_value::<Vec<LinkRef>>(),
        graph: schema_value::<GraphOutput>(),
        pipeline_hits: schema_value::<Vec<SearchHit>>(),
        pipeline_context: schema_value::<Vec<ContextItem>>(),
        status: schema_value::<Status>(),
    }
}

pub fn command_schema_json(name: &str) -> Option<String> {
    let schemas = schema_map();
    schemas
        .get(name)
        .map(|schema| serde_json::to_string_pretty(schema).expect("schema must serialize"))
}

pub fn all_command_schemas_json() -> String {
    serde_json::to_string_pretty(&command_json_schemas()).expect("schemas must serialize")
}

pub fn compact_reference() -> String {
    let schemas = schema_map();
    let mut lines =
        vec!["JSON output schemas are type-generated from Rust Serialize DTOs:".to_owned()];
    for (name, schema) in schemas {
        lines.push(format!("  {name}: {}", compact_schema(&schema)));
    }
    lines.join("\n")
}

fn schema_value<T: schemars::JsonSchema>() -> Value {
    serde_json::to_value(schema_for!(T)).expect("schema must serialize")
}

fn schema_map() -> BTreeMap<&'static str, Value> {
    let schemas = command_json_schemas();
    BTreeMap::from([
        ("backlinks", schemas.backlinks),
        ("graph", schemas.graph),
        ("index", schemas.index),
        ("links", schemas.links),
        ("pipeline --context", schemas.pipeline_context),
        ("pipeline", schemas.pipeline_hits),
        ("query native", schemas.query_native),
        ("query record-set", schemas.query_record_set),
        ("search", schemas.search),
        ("status", schemas.status),
        ("unresolved-links", schemas.unresolved_links),
    ])
}

fn compact_schema(schema: &Value) -> String {
    compact_schema_with_root(schema, schema)
}

fn compact_schema_with_root(schema: &Value, root: &Value) -> String {
    if let Some(reference) = schema.get("$ref").and_then(Value::as_str) {
        if let Some(resolved) = resolve_ref(root, reference) {
            return compact_schema_with_root(resolved, root);
        }
    }
    if schema.get("type").and_then(Value::as_str) == Some("array") {
        let Some(items) = schema.get("items") else {
            return "array".to_owned();
        };
        return format!("[{}]", compact_schema_with_root(items, root));
    }
    if schema.get("type").and_then(Value::as_str) == Some("object") {
        let keys = schema
            .get("properties")
            .and_then(Value::as_object)
            .map(|properties| properties.keys().cloned().collect::<Vec<_>>())
            .unwrap_or_default();
        return format!("{{{}}}", keys.join(", "));
    }
    schema
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("value")
        .to_owned()
}

fn resolve_ref<'a>(root: &'a Value, reference: &str) -> Option<&'a Value> {
    let path = reference.strip_prefix("#/")?;
    let mut value = root;
    for part in path.split('/') {
        value = value.get(part)?;
    }
    Some(value)
}
