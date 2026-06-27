use std::collections::BTreeMap;
use std::sync::LazyLock;

use anyhow::{Context, Result};
use chrono::{Local, NaiveDate};
use regex::Regex;
use serde_json::{Map, Value, json};

use crate::core::{QueryAdapter, QueryContext, RecordSet, Row};
use crate::script::{QuickJsEngine, ScriptEngine};

use super::expr::value_order;

static TASK: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*[-*+]\s+\[([^\]])\]\s+(.*)$").unwrap());
static INLINE_FIELD: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\[([^\[\]:]+)::\s*([^\]]*)\]").unwrap());
static TAG: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?:^|\s)#([^\s#\[\],]+)").unwrap());

pub struct TasksAdapter;

impl QueryAdapter for TasksAdapter {
    fn name(&self) -> &'static str {
        "tasks"
    }

    fn execute(&self, context: &QueryContext<'_>, source: &str) -> Result<RecordSet> {
        let query_context = query_value(context);
        let source = expand_query_placeholders(source, &query_context);
        let query = TaskQuery::parse(&source)?;
        let mut tasks = collect_tasks(context)?;

        let script = QuickJsEngine::default();
        tasks.retain(|row| {
            query
                .filters
                .iter()
                .all(|filter| filter.matches(row, &query_context, &script))
        });
        let mut sort_diagnostics = Vec::new();
        for sort in &query.sorts {
            if let TaskSort::Field(field, _) = sort {
                if !tasks.is_empty() && tasks.iter().all(|t| t.get(field).is_none()) {
                    sort_diagnostics.push(format!(
                        "sort field '{}' does not exist on any task — possible typo",
                        field
                    ));
                }
            }
        }
        for sort in query.sorts.iter().rev() {
            tasks.sort_by(|left, right| sort.compare(left, right, &query_context, &script));
        }
        if let Some(limit) = query.limit {
            tasks.truncate(limit);
        }
        let rows = if let Some(group) = &query.group {
            group_rows(tasks, group, &query_context, &script)
        } else {
            tasks
                .into_iter()
                .map(|value| value.as_object().unwrap().clone().into_iter().collect())
                .collect()
        };
        let mut result = RecordSet::new("tasks", rows);
        result.diagnostics = query.diagnostics;
        result.diagnostics.extend(sort_diagnostics);
        Ok(result)
    }
}

pub(crate) fn collect_tasks(context: &QueryContext<'_>) -> Result<Vec<Value>> {
    let mut tasks = Vec::new();
    for page in context.database.all_pages()? {
        for (line_index, line) in page.body.lines().enumerate() {
            let Some(captures) = TASK.captures(line) else {
                continue;
            };
            tasks.push(task_row(
                &page.path,
                line_index + 1,
                &captures[1],
                &captures[2],
                &page.metadata,
            ));
        }
    }
    Ok(tasks)
}

struct TaskQuery {
    filters: Vec<TaskFilter>,
    sorts: Vec<TaskSort>,
    group: Option<TaskGroup>,
    limit: Option<usize>,
    diagnostics: Vec<String>,
}

enum TaskFilter {
    Never,
    Done(bool),
    Date {
        field: String,
        relation: DateRelation,
        value: String,
    },
    Text {
        field: String,
        includes: bool,
        value: String,
    },
    Status(String),
    Any(Vec<TaskFilter>),
    All(Vec<TaskFilter>),
    Script(String),
}

#[derive(Clone, Copy)]
enum DateRelation {
    On,
    Before,
    After,
    OnOrBefore,
    OnOrAfter,
}

enum TaskSort {
    Field(String, bool),
    Script(String, bool),
}

enum TaskGroup {
    Field(String),
    Script(String),
}

impl TaskQuery {
    fn parse(source: &str) -> Result<Self> {
        let mut query = Self {
            filters: Vec::new(),
            sorts: Vec::new(),
            group: None,
            limit: None,
            diagnostics: Vec::new(),
        };
        for raw in logical_lines(source) {
            let line = raw.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let lower = line.to_ascii_lowercase();
            if matches!(
                lower.as_str(),
                "hide task count"
                    | "hide backlink"
                    | "hide edit button"
                    | "hide postpone button"
                    | "short mode"
            ) {
                continue;
            }
            if lower == "not done" {
                query.filters.push(TaskFilter::Done(false));
            } else if lower == "done" {
                query.filters.push(TaskFilter::Done(true));
            } else if let Some(source) = line.strip_prefix("filter by function ") {
                query.filters.push(TaskFilter::Script(source.to_owned()));
            } else if let Some(source) = line.strip_prefix("sort by function ") {
                query.sorts.push(TaskSort::Script(source.to_owned(), false));
            } else if let Some(source) = line.strip_prefix("group by function ") {
                query.group = Some(TaskGroup::Script(source.to_owned()));
            } else if let Some(field) = lower.strip_prefix("group by ") {
                query.group = Some(TaskGroup::Field(field.to_owned()));
            } else if let Some(rest) = lower.strip_prefix("sort by ") {
                let descending = rest.ends_with(" reverse");
                let field = rest.strip_suffix(" reverse").unwrap_or(rest).to_owned();
                query.sorts.push(TaskSort::Field(field, descending));
            } else if let Some(rest) = lower.strip_prefix("limit ") {
                query.limit = Some(rest.parse().context("invalid Tasks limit")?);
            } else if let Some(filter) = parse_or_filter(line) {
                query.filters.push(filter);
            } else {
                query
                    .diagnostics
                    .push(format!("unsupported Tasks instruction: {line}"));
                // Treat unrecognised instructions as never-matching filters so the query
                // returns zero results rather than silently bypassing the intent.
                query.filters.push(TaskFilter::Never);
            }
        }
        Ok(query)
    }
}

fn parse_or_filter(line: &str) -> Option<TaskFilter> {
    let stripped = line.trim().trim_start_matches('(').trim_end_matches(')');
    let branches: Vec<_> = Regex::new(r"(?i)\)\s+OR\s+\(")
        .unwrap()
        .split(stripped)
        .filter_map(parse_and_filter)
        .collect();
    if branches.len() > 1 {
        Some(TaskFilter::Any(branches))
    } else {
        parse_and_filter(stripped)
    }
}

fn parse_and_filter(line: &str) -> Option<TaskFilter> {
    let branches: Vec<_> = Regex::new(r"(?i)\)\s+AND\s+\(")
        .unwrap()
        .split(line)
        .filter_map(parse_filter)
        .collect();
    if branches.len() > 1 {
        Some(TaskFilter::All(branches))
    } else {
        parse_filter(line)
    }
}

fn parse_filter(line: &str) -> Option<TaskFilter> {
    let line = line.trim().trim_matches(|c| c == '(' || c == ')');
    let lower = line.to_ascii_lowercase();
    for (prefix, field) in [
        ("due ", "due"),
        ("scheduled ", "scheduled"),
        ("starts ", "start"),
        ("start ", "start"),
        ("done ", "completion"),
        ("created ", "created"),
    ] {
        if let Some(rest) = lower.strip_prefix(prefix) {
            for (operator, relation) in [
                ("on or before ", DateRelation::OnOrBefore),
                ("on or after ", DateRelation::OnOrAfter),
                ("before ", DateRelation::Before),
                ("after ", DateRelation::After),
                ("on ", DateRelation::On),
            ] {
                if let Some(value) = rest.strip_prefix(operator) {
                    return Some(TaskFilter::Date {
                        field: field.to_owned(),
                        relation,
                        value: value.to_owned(),
                    });
                }
            }
            if NaiveDate::parse_from_str(rest, "%Y-%m-%d").is_ok() {
                return Some(TaskFilter::Date {
                    field: field.to_owned(),
                    relation: DateRelation::On,
                    value: rest.to_owned(),
                });
            }
        }
    }
    for (prefix, field, includes) in [
        ("tags include ", "tags", true),
        ("tags do not include ", "tags", false),
        ("path includes ", "path", true),
        ("path does not include ", "path", false),
        ("description includes ", "description", true),
        ("description does not include ", "description", false),
    ] {
        if let Some(value) = lower.strip_prefix(prefix) {
            return Some(TaskFilter::Text {
                field: field.to_owned(),
                includes,
                value: value.to_owned(),
            });
        }
    }
    lower
        .strip_prefix("status is ")
        .map(|value| TaskFilter::Status(value.to_owned()))
}

impl TaskFilter {
    fn matches(&self, row: &Value, query: &Value, script: &dyn ScriptEngine) -> bool {
        match self {
            Self::Never => false,
            Self::Done(expected) => row["done"].as_bool() == Some(*expected),
            Self::Date {
                field,
                relation,
                value,
            } => {
                let expected = resolve_date(value);
                let actual = row.get(field).and_then(Value::as_str);
                actual
                    .zip(expected.as_deref())
                    .and_then(|(actual, expected)| {
                        NaiveDate::parse_from_str(actual, "%Y-%m-%d")
                            .ok()
                            .zip(NaiveDate::parse_from_str(expected, "%Y-%m-%d").ok())
                    })
                    .is_some_and(|(actual, expected)| match relation {
                        DateRelation::On => actual == expected,
                        DateRelation::Before => actual < expected,
                        DateRelation::After => actual > expected,
                        DateRelation::OnOrBefore => actual <= expected,
                        DateRelation::OnOrAfter => actual >= expected,
                    })
            }
            Self::Text {
                field,
                includes,
                value,
            } => {
                let contains = match &row[field] {
                    Value::String(actual) => actual.to_lowercase().contains(value),
                    Value::Array(actual) => actual.iter().any(|item| {
                        item.as_str()
                            .is_some_and(|item| item.to_lowercase().contains(value))
                    }),
                    _ => false,
                };
                contains == *includes
            }
            Self::Status(status) => row["status"]
                .get("name")
                .and_then(Value::as_str)
                .is_some_and(|actual| actual.eq_ignore_ascii_case(status)),
            Self::Any(filters) => filters
                .iter()
                .any(|filter| filter.matches(row, query, script)),
            Self::All(filters) => filters
                .iter()
                .all(|filter| filter.matches(row, query, script)),
            Self::Script(source) => evaluate_task_script(script, source, row, query)
                .ok()
                .and_then(|value| value.as_bool())
                .unwrap_or(false),
        }
    }
}

impl TaskSort {
    fn compare(
        &self,
        left: &Value,
        right: &Value,
        query: &Value,
        script: &dyn ScriptEngine,
    ) -> std::cmp::Ordering {
        let (left_value, right_value, descending) = match self {
            Self::Field(field, descending) => (
                left.get(field).cloned().unwrap_or(Value::Null),
                right.get(field).cloned().unwrap_or(Value::Null),
                *descending,
            ),
            Self::Script(source, descending) => (
                evaluate_task_script(script, source, left, query).unwrap_or(Value::Null),
                evaluate_task_script(script, source, right, query).unwrap_or(Value::Null),
                *descending,
            ),
        };
        let ordering = value_order(&left_value, &right_value).unwrap_or(std::cmp::Ordering::Equal);
        if descending {
            ordering.reverse()
        } else {
            ordering
        }
    }
}

fn group_rows(
    tasks: Vec<Value>,
    group: &TaskGroup,
    query: &Value,
    script: &dyn ScriptEngine,
) -> Vec<Row> {
    let mut groups = BTreeMap::<String, (Value, Vec<Value>)>::new();
    for task in tasks {
        let key = match group {
            TaskGroup::Field(field) => value_at_path(&task, field).cloned().unwrap_or(Value::Null),
            TaskGroup::Script(source) => {
                evaluate_task_script(script, source, &task, query).unwrap_or(Value::Null)
            }
        };
        groups
            .entry(key.to_string())
            .or_insert_with(|| (key, Vec::new()))
            .1
            .push(task);
    }
    groups
        .into_values()
        .map(|(key, tasks)| {
            BTreeMap::from([
                ("key".to_owned(), key),
                ("rows".to_owned(), Value::Array(tasks)),
            ])
        })
        .collect()
}

fn evaluate_task_script(
    script: &dyn ScriptEngine,
    source: &str,
    task: &Value,
    query: &Value,
) -> Result<Value> {
    let source = source.trim();
    let invocation = if source.contains("=>") || source.starts_with("function") {
        format!("return ({source})(task);")
    } else {
        source.to_owned()
    };
    script.evaluate(
        &format!(
            r#"
            const task = {{
              ...__mdq.task,
              file: {{
                ...__mdq.task.file,
                property: name => __mdq.task[name] ?? __mdq.task.file.frontmatter?.[name] ?? null
              }}
            }};
            const query = __mdq.query;
            {invocation}
            "#
        ),
        &json!({"task": task, "query": query}),
    )
}

fn logical_lines(source: &str) -> Vec<String> {
    let mut lines = Vec::new();
    let mut current = String::new();
    for line in source.lines() {
        let trimmed = line.trim_end();
        let continued = trimmed.ends_with('\\');
        let content = trimmed.strip_suffix('\\').unwrap_or(trimmed);
        if !current.is_empty() {
            current.push('\n');
        }
        current.push_str(content);
        if !continued {
            lines.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        lines.push(current);
    }
    lines
}

fn query_value(context: &QueryContext<'_>) -> Value {
    let path = context
        .current_file
        .as_ref()
        .and_then(|path| path.strip_prefix(context.vault).ok())
        .map(|path| path.to_string_lossy().replace('\\', "/"))
        .unwrap_or_default();
    let filename = path.rsplit('/').next().unwrap_or(&path);
    json!({
        "file": {
            "path": path,
            "filename": filename,
            "filenameWithoutExtension": filename.trim_end_matches(".md")
        }
    })
}

fn expand_query_placeholders(source: &str, query: &Value) -> String {
    [
        ("{{query.file.path}}", query["file"]["path"].as_str()),
        (
            "{{query.file.filename}}",
            query["file"]["filename"].as_str(),
        ),
        (
            "{{query.file.filenameWithoutExtension}}",
            query["file"]["filenameWithoutExtension"].as_str(),
        ),
    ]
    .into_iter()
    .fold(source.to_owned(), |source, (placeholder, value)| {
        source.replace(placeholder, value.unwrap_or_default())
    })
}

fn value_at_path<'a>(value: &'a Value, path: &str) -> Option<&'a Value> {
    let mut current = value;
    for part in path.split('.') {
        current = current.get(part)?;
    }
    Some(current)
}

fn resolve_date(value: &str) -> Option<String> {
    match value.trim() {
        "today" => Some(Local::now().date_naive().to_string()),
        "tomorrow" => Some(Local::now().date_naive().succ_opt()?.to_string()),
        "yesterday" => Some(Local::now().date_naive().pred_opt()?.to_string()),
        value if NaiveDate::parse_from_str(value, "%Y-%m-%d").is_ok() => Some(value.to_owned()),
        _ => None,
    }
}

fn task_row(path: &str, line: usize, status: &str, source: &str, frontmatter: &Value) -> Value {
    let mut fields = Map::new();
    let mut description = source.to_owned();
    for captures in INLINE_FIELD.captures_iter(source) {
        let key = captures[1].trim().to_ascii_lowercase();
        let value = captures[2].trim();
        fields.insert(key, Value::String(value.to_owned()));
        description = description.replace(&captures[0], "");
    }
    let tags: Vec<Value> = TAG
        .captures_iter(&description)
        .map(|capture| Value::String(format!("#{}", &capture[1])))
        .collect();
    let done = matches!(status, "x" | "X" | "-");
    let status_name = match status {
        "x" | "X" => "done",
        "-" => "cancelled",
        "/" => "in_progress",
        _ => "todo",
    };
    fields.insert("path".to_owned(), Value::String(path.to_owned()));
    let filename = path.rsplit('/').next().unwrap_or(path);
    fields.insert(
        "file".to_owned(),
        json!({
            "path": path,
            "filename": filename,
            "filenameWithoutExtension": filename.trim_end_matches(".md"),
            "frontmatter": frontmatter
        }),
    );
    fields.insert("line".to_owned(), Value::Number(line.into()));
    let status_type = if done {
        "DONE"
    } else if status == "/" {
        "IN_PROGRESS"
    } else {
        "TODO"
    };
    fields.insert(
        "status".to_owned(),
        json!({"name": status_name, "type": status_type, "symbol": status}),
    );
    fields.insert(
        "status_name".to_owned(),
        Value::String(status_name.to_owned()),
    );
    fields.insert(
        "status_type".to_owned(),
        Value::String(status_type.to_owned()),
    );
    fields.insert("status_symbol".to_owned(), Value::String(status.to_owned()));
    fields.insert("done".to_owned(), Value::Bool(done));
    fields.insert("completed".to_owned(), Value::Bool(done));
    fields.insert(
        "description".to_owned(),
        Value::String(description.trim().to_owned()),
    );
    fields.insert("text".to_owned(), Value::String(source.to_owned()));
    fields.insert("tags".to_owned(), Value::Array(tags));
    Value::Object(fields)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_task_fields_without_vault_specific_names() {
        let task = task_row(
            "Daily/test.md",
            7,
            " ",
            "#task write [due:: 2026-06-14] [priority:: high]",
            &json!({}),
        );
        assert_eq!(task["due"], "2026-06-14");
        assert_eq!(task["priority"], "high");
        assert_eq!(task["done"], false);
    }

    #[test]
    fn parses_boolean_date_query() {
        let query = TaskQuery::parse(
            "(due on 2026-06-14) OR (scheduled on 2026-06-14)\nnot done\nsort by due",
        )
        .unwrap();
        assert_eq!(query.filters.len(), 2);
        assert_eq!(query.sorts.len(), 1);
    }

    #[test]
    fn parses_and_filters_and_grouping() {
        let query = TaskQuery::parse(
            "(tags include important) AND (tags do not include urgent)\ngroup by status.type",
        )
        .unwrap();
        assert_eq!(query.filters.len(), 1);
        assert!(matches!(query.filters[0], TaskFilter::All(_)));
        assert!(matches!(query.group, Some(TaskGroup::Field(_))));
    }

    #[test]
    fn joins_tasks_function_continuations() {
        let lines = logical_lines(
            "not done\nfilter by function \\\nconst value = task.done; \\\nreturn !value",
        );
        assert_eq!(lines.len(), 2);
        assert!(lines[1].contains("return !value"));
    }
}
