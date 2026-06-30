use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Component, Path};

use anyhow::{Context, Result, bail};
use regex::Regex;
use serde_json::{Value, json};

use crate::core::{QueryAdapter, QueryContext, RecordSet, Row};
use crate::script::{QuickJsEngine, ScriptEngine};

use super::expr::{Expr, value_order};
use super::tasks::collect_tasks;
use super::{LinkIndex, page_value};

pub struct DataviewAdapter;
pub struct DataviewJsAdapter;

impl QueryAdapter for DataviewAdapter {
    fn name(&self) -> &'static str {
        "dataview"
    }

    fn execute(&self, context: &QueryContext<'_>, source: &str) -> Result<RecordSet> {
        let query = DqlQuery::parse(source)?;
        let mut values = if query.kind == "task" {
            collect_tasks(context)?
        } else {
            let links = LinkIndex::build(context.database)?;
            context
                .database
                .all_pages()?
                .iter()
                .map(|page| page_value(page, &links))
                .collect()
        };
        values.retain(|value| query.source.matches(value));
        for operation in &query.operations {
            match operation {
                DqlOperation::Where(expression) => {
                    values.retain(|value| expression.test(value));
                }
                DqlOperation::Sort(sorts) => {
                    for sort in sorts.iter().rev() {
                        values.sort_by(|left, right| {
                            let ordering =
                                value_order(&sort.expr.eval(left), &sort.expr.eval(right))
                                    .unwrap_or(std::cmp::Ordering::Equal);
                            if sort.descending {
                                ordering.reverse()
                            } else {
                                ordering
                            }
                        });
                    }
                }
                DqlOperation::Flatten(flatten) => {
                    values = flatten_values(values, flatten);
                }
                DqlOperation::Group(expression) => {
                    values = group_values(values, expression);
                }
                DqlOperation::Limit(limit) => values.truncate(*limit),
            }
        }
        let rows = values
            .into_iter()
            .map(|value| query.project(&value))
            .collect();
        Ok(RecordSet::new(query.kind, rows))
    }
}

struct DqlQuery {
    kind: String,
    fields: Vec<(String, Expr)>,
    source: DqlSource,
    operations: Vec<DqlOperation>,
}

enum DqlSource {
    All,
    Folder(String),
    Tag(String),
}

struct DqlSort {
    expr: Expr,
    descending: bool,
}

struct DqlFlatten {
    name: String,
    expr: Expr,
}

enum DqlOperation {
    Where(Expr),
    Sort(Vec<DqlSort>),
    Flatten(DqlFlatten),
    Group(Expr),
    Limit(usize),
}

impl DqlQuery {
    fn parse(source: &str) -> Result<Self> {
        let normalized = split_dql_clauses(source);
        let first = normalized.first().context("empty Dataview query")?;
        let first_lower = first.to_ascii_lowercase();
        let (kind, projection) = ["table", "list", "task", "calendar"]
            .into_iter()
            .find_map(|kind| {
                first_lower
                    .strip_prefix(kind)
                    .map(|_| (kind.to_owned(), first[kind.len()..].trim()))
            })
            .context("Dataview query must start with TABLE, LIST, TASK, or CALENDAR")?;
        let projection = projection
            .strip_prefix("WITHOUT ID")
            .or_else(|| projection.strip_prefix("without id"))
            .unwrap_or(projection)
            .trim();
        let fields = parse_projection(projection)?;
        let mut query = Self {
            kind,
            fields,
            source: DqlSource::All,
            operations: Vec::new(),
        };
        for line in normalized.into_iter().skip(1) {
            let lower = line.to_ascii_lowercase();
            if let Some(value) = lower.strip_prefix("from ") {
                query.source = parse_source(&line[5..], value);
            } else if lower.starts_with("where ") {
                query
                    .operations
                    .push(DqlOperation::Where(Expr::parse(&line[6..])?));
            } else if lower.starts_with("sort ") {
                let value = &line[5..];
                let mut sorts = Vec::new();
                for sort in split_top_level(value, ',') {
                    let sort = sort.trim();
                    let descending = sort.to_ascii_lowercase().ends_with(" desc");
                    let expression = sort
                        .strip_suffix(" DESC")
                        .or_else(|| sort.strip_suffix(" desc"))
                        .or_else(|| sort.strip_suffix(" ASC"))
                        .or_else(|| sort.strip_suffix(" asc"))
                        .unwrap_or(sort);
                    sorts.push(DqlSort {
                        expr: Expr::parse(expression)?,
                        descending,
                    });
                }
                query.operations.push(DqlOperation::Sort(sorts));
            } else if let Some(value) = lower.strip_prefix("limit ") {
                query.operations.push(DqlOperation::Limit(
                    value.parse().context("invalid Dataview LIMIT")?,
                ));
            } else if lower.starts_with("flatten ") {
                let source = &line[8..];
                let (expression, name) = split_alias(source);
                query.operations.push(DqlOperation::Flatten(DqlFlatten {
                    name: name.unwrap_or(expression).to_owned(),
                    expr: Expr::parse(expression)?,
                }));
            } else if lower.starts_with("group by ") {
                query
                    .operations
                    .push(DqlOperation::Group(Expr::parse(&line[9..])?));
            }
        }
        Ok(query)
    }

    fn project(&self, value: &Value) -> Row {
        if self.kind == "task" || self.fields.is_empty() {
            return value.as_object().unwrap().clone().into_iter().collect();
        }
        self.fields
            .iter()
            .map(|(name, expression)| (name.clone(), expression.eval(value)))
            .collect()
    }
}

fn flatten_values(values: Vec<Value>, flatten: &DqlFlatten) -> Vec<Value> {
    let mut output = Vec::new();
    for value in values {
        let flattened = flatten.expr.eval(&value);
        let items = flattened
            .as_array()
            .cloned()
            .unwrap_or_else(|| vec![flattened]);
        for item in items {
            let mut value = value.clone();
            value
                .as_object_mut()
                .unwrap()
                .insert(flatten.name.clone(), item);
            output.push(value);
        }
    }
    output
}

fn group_values(values: Vec<Value>, group: &Expr) -> Vec<Value> {
    let mut groups = BTreeMap::<String, (Value, Vec<Value>)>::new();
    for value in values {
        let key = group.eval(&value);
        groups
            .entry(key.to_string())
            .or_insert_with(|| (key, Vec::new()))
            .1
            .push(value);
    }
    groups
        .into_values()
        .map(|(key, values)| json!({"key": key, "rows": values}))
        .collect()
}

fn split_dql_clauses(source: &str) -> Vec<String> {
    let normalized = source.split_whitespace().collect::<Vec<_>>().join(" ");
    let pattern = Regex::new(r"(?i)\s+(FROM|WHERE|SORT|LIMIT|GROUP\s+BY|FLATTEN)\s+").unwrap();
    let mut clauses = Vec::new();
    let mut start = 0;
    for captures in pattern.captures_iter(&normalized) {
        let matched = captures.get(0).unwrap();
        let before = normalized[start..matched.start()].trim();
        if !before.is_empty() {
            clauses.push(before.to_owned());
        }
        start = matched.start() + matched.as_str().find(|c: char| !c.is_whitespace()).unwrap();
    }
    let tail = normalized[start..].trim();
    if !tail.is_empty() {
        clauses.push(tail.to_owned());
    }
    clauses
}

fn parse_projection(source: &str) -> Result<Vec<(String, Expr)>> {
    if source.is_empty() {
        return Ok(Vec::new());
    }
    split_top_level(source, ',')
        .into_iter()
        .map(|field| {
            let field = field.trim();
            let (expression, alias) = split_alias(field);
            let alias = alias.unwrap_or(field);
            Ok((alias.trim_matches('"').to_owned(), Expr::parse(expression)?))
        })
        .collect()
}

fn split_alias(source: &str) -> (&str, Option<&str>) {
    let lower = source.to_ascii_lowercase();
    lower
        .rfind(" as ")
        .map(|position| (&source[..position], Some(source[position + 4..].trim())))
        .unwrap_or((source, None))
}

fn split_top_level(source: &str, delimiter: char) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut depth = 0;
    let mut quote = None;
    let mut start = 0;
    for (index, character) in source.char_indices() {
        if let Some(active) = quote {
            if character == active {
                quote = None;
            }
            continue;
        }
        match character {
            '\'' | '"' => quote = Some(character),
            '(' | '[' => depth += 1,
            ')' | ']' => depth = (depth - 1).max(0),
            value if value == delimiter && depth == 0 => {
                parts.push(&source[start..index]);
                start = index + character.len_utf8();
            }
            _ => {}
        }
    }
    parts.push(&source[start..]);
    parts
}

fn parse_source(original: &str, lower: &str) -> DqlSource {
    let source = original.trim();
    if lower.trim() == "\"\"" || source.is_empty() {
        DqlSource::All
    } else if source.starts_with('#') {
        DqlSource::Tag(source.to_owned())
    } else {
        DqlSource::Folder(source.trim_matches('"').to_owned())
    }
}

impl DqlSource {
    fn matches(&self, value: &Value) -> bool {
        match self {
            Self::All => true,
            Self::Folder(folder) => value["file"]["path"]
                .as_str()
                .is_some_and(|path| path.starts_with(folder)),
            Self::Tag(tag) => value["file"]["tags"].as_array().is_some_and(|tags| {
                tags.iter().any(|value| {
                    value
                        .as_str()
                        .is_some_and(|value| value == tag || format!("#{value}") == *tag)
                })
            }),
        }
    }
}

impl QueryAdapter for DataviewJsAdapter {
    fn name(&self) -> &'static str {
        "dataviewjs"
    }

    fn execute(&self, context: &QueryContext<'_>, source: &str) -> Result<RecordSet> {
        let tasks = collect_tasks(context)?;
        let links = LinkIndex::build(context.database)?;
        let mut pages: Vec<Value> = context
            .database
            .all_pages()?
            .iter()
            .map(|page| page_value(page, &links))
            .collect();
        for page in &mut pages {
            let path = page["file"]["path"].as_str().unwrap_or_default();
            let page_tasks = tasks
                .iter()
                .filter(|task| task["path"].as_str() == Some(path))
                .cloned()
                .collect();
            page["file"]["tasks"] = Value::Array(page_tasks);
        }
        resolve_page_links(&mut pages);
        let current_path = context
            .current_file
            .as_ref()
            .and_then(|path| path.strip_prefix(context.vault).ok())
            .map(|path| path.to_string_lossy().replace('\\', "/"));
        let current = current_path
            .as_ref()
            .and_then(|path| pages.iter().find(|page| page["file"]["path"] == *path))
            .cloned()
            .unwrap_or(Value::Null);
        let expanded = expand_views(context, source)?;
        let program = format!(
            r#"
            const __outputs = [];
            class MdqDate {{
              constructor(value) {{ this.value = value; }}
              toMillis() {{ return Date.parse(this.value); }}
              toString() {{ return this.value; }}
              valueOf() {{ return this.toMillis(); }}
              toJSON() {{ return this.value; }}
            }}
            class DataArray extends Array {{
              where(fn) {{ return DataArray.from(this.filter(fn)); }}
              map(fn) {{ return DataArray.from(super.map(fn)); }}
              flatMap(fn) {{ return DataArray.from(super.flatMap(fn)); }}
              sort(fn, direction='asc') {{
                if (!fn) return this;
                if (fn.length >= 2) super.sort(fn);
                else super.sort((a, b) => {{
                  const left = fn(a), right = fn(b);
                  const order = left < right ? -1 : left > right ? 1 : 0;
                  return String(direction).toLowerCase() === 'desc' ? -order : order;
                }});
                return this;
              }}
              groupBy(fn) {{
                const groups = new Map();
                for (const value of this) {{
                  const key = fn(value);
                  const encoded = JSON.stringify(key);
                  if (!groups.has(encoded)) groups.set(encoded, {{key, rows: DataArray.from([])}});
                  groups.get(encoded).rows.push(value);
                }}
                return DataArray.from(groups.values());
              }}
              distinct(fn=value => value) {{
                const seen = new Set();
                return DataArray.from(this.filter(value => {{
                  const key = JSON.stringify(fn(value));
                  if (seen.has(key)) return false;
                  seen.add(key);
                  return true;
                }}));
              }}
              array() {{ return Array.from(this); }}
            }}
            const __dateFields = ['due', 'scheduled', 'start', 'completion', 'created', 'cancelled'];
            const __pages = DataArray.from(__mdq.pages);
            for (const page of __pages) {{
              page.file.tasks = DataArray.from((page.file.tasks || []).map(task => {{
                for (const field of __dateFields) {{
                  if (typeof task[field] === 'string') task[field] = new MdqDate(task[field]);
                }}
                return task;
              }}));
            }}
            const __current = __pages.find(page => page.file.path === __mdq.current?.file?.path) || null;
            const dv = {{
              pages(source) {{
                if (!source || source === '""') return DataArray.from(__pages);
                if (source.startsWith('#')) return DataArray.from(__pages.filter(p => (p.file.tags || []).some(t => t === source || `#${{t}}` === source)));
                const folder = source.replace(/^"|"$/g, '');
                return DataArray.from(__pages.filter(p => p.file.path.startsWith(folder)));
              }},
              page(path) {{ return __pages.find(p => p.file.path === path || p.file.name === path) || null; }},
              current() {{ return __current; }},
              date(value) {{ return value instanceof MdqDate ? value : new MdqDate(String(value)); }},
              fileLink(path, embed=false, display=null) {{ return {{path, embed, display: display || path}}; }},
              list(values) {{ __outputs.push({{kind:'list', rows:Array.from(values)}}); }},
              table(columns, rows) {{ __outputs.push({{kind:'table', columns, rows:Array.from(rows)}}); }},
              taskList(values) {{ __outputs.push({{kind:'task', rows:Array.from(values)}}); }},
              paragraph(value) {{ __outputs.push({{kind:'paragraph', rows:[value]}}); }},
              el() {{ throw new Error('DOM rendering is disabled in mdq'); }},
              io: {{ load() {{ throw new Error('dv.io is disabled in mdq'); }} }},
              view() {{ throw new Error('unexpanded dv.view call'); }}
            }};
            {expanded}
            return __outputs;
            "#
        );
        let result = QuickJsEngine::default().evaluate(
            &program,
            &json!({"pages": pages, "tasks": tasks, "current": current}),
        )?;
        let outputs = result.as_array().cloned().unwrap_or_default();
        let mut rows = Vec::new();
        for output in outputs {
            let kind = output["kind"].as_str().unwrap_or("value");
            for value in output["rows"].as_array().cloned().unwrap_or_default() {
                let mut row = BTreeMap::new();
                row.insert("render".to_owned(), Value::String(kind.to_owned()));
                row.insert("value".to_owned(), value);
                rows.push(row);
            }
        }
        Ok(RecordSet::new("dataviewjs", rows))
    }
}

fn resolve_page_links(pages: &mut [Value]) {
    let by_name: HashMap<String, String> = pages
        .iter()
        .filter_map(|page| {
            page["file"]["name"]
                .as_str()
                .zip(page["file"]["path"].as_str())
                .map(|(name, path)| (name.to_owned(), path.to_owned()))
        })
        .collect();
    for page in pages {
        resolve_links_in_value(page, &by_name);
    }
}

fn resolve_links_in_value(value: &mut Value, by_name: &HashMap<String, String>) {
    match value {
        Value::Array(values) => {
            for value in values {
                resolve_links_in_value(value, by_name);
            }
        }
        Value::Object(object) => {
            if object.contains_key("display")
                && let Some(path) = object.get("path").and_then(Value::as_str)
            {
                let name = path
                    .trim_end_matches(".md")
                    .rsplit('/')
                    .next()
                    .unwrap_or(path);
                if let Some(resolved) = by_name.get(name) {
                    object.insert("path".to_owned(), Value::String(resolved.clone()));
                }
            }
            for value in object.values_mut() {
                resolve_links_in_value(value, by_name);
            }
        }
        _ => {}
    }
}

fn expand_views(context: &QueryContext<'_>, source: &str) -> Result<String> {
    let pattern =
        Regex::new(r#"(?s)(?:await\s+)?dv\.view\(\s*["']([^"']+)["']\s*(?:,\s*([^)]+))?\)"#)?;
    let mut expanded = source.to_owned();
    for captures in pattern.captures_iter(source) {
        let relative = captures[1].trim_end_matches(".js");
        let relative_path = Path::new(relative);
        if relative_path.is_absolute()
            || relative_path
                .components()
                .any(|component| !matches!(component, Component::Normal(_) | Component::CurDir))
        {
            bail!("dv.view path must stay beneath the vault: {relative}");
        }
        let directory_view = context.vault.join(relative_path).join("view.js");
        let file_view = context.vault.join(format!("{relative}.js"));
        let path = if directory_view.is_file() {
            directory_view
        } else {
            file_view
        };
        let canonical_vault = context.vault.canonicalize()?;
        let canonical_path = path.canonicalize().with_context(|| {
            format!(
                "dv.view(\"{relative}\"): file not found — mdq looks for \"{relative}.js\" \
                 or \"{relative}/view.js\" inside the vault"
            )
        })?;
        if !canonical_path.starts_with(&canonical_vault) {
            bail!("dv.view path escapes vault: {}", canonical_path.display());
        }
        let view = fs::read_to_string(&canonical_path)
            .with_context(|| format!("cannot load Dataview view {}", canonical_path.display()))?;
        let input = captures
            .get(2)
            .map(|value| value.as_str())
            .unwrap_or("null");
        let replacement = format!("(() => {{ const input = {input}; {view} }})()");
        expanded = expanded.replace(&captures[0], &replacement);
    }
    Ok(expanded)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_basic_dql() {
        let query = DqlQuery::parse(
            "TABLE title AS Name, created\nFROM \"Daily\"\nWHERE created >= date(2026-01-01)\nSORT created DESC\nLIMIT 10",
        )
        .unwrap();
        assert_eq!(query.kind, "table");
        assert_eq!(query.fields.len(), 2);
        assert!(matches!(
            query.operations.last(),
            Some(DqlOperation::Limit(10))
        ));
    }

    #[test]
    fn parses_single_line_dql() {
        let query = DqlQuery::parse("LIST title FROM \"\" WHERE created=date(2026-06-09)").unwrap();
        assert_eq!(query.kind, "list");
        assert_eq!(query.fields.len(), 1);
    }

    #[test]
    fn preserves_clause_order() {
        let query = DqlQuery::parse(
            "LIST\nFLATTEN tags AS tag\nWHERE contains(tag, 'AI')\nGROUP BY tag\nLIMIT 2",
        )
        .unwrap();
        assert!(matches!(
            query.operations.as_slice(),
            [
                DqlOperation::Flatten(_),
                DqlOperation::Where(_),
                DqlOperation::Group(_),
                DqlOperation::Limit(2)
            ]
        ));
    }

    #[test]
    fn parses_table_without_id_and_multiple_sorts() {
        let query = DqlQuery::parse(
            "TABLE WITHOUT ID status, file.name\nFROM #Book\nSORT status DESC, file.name ASC",
        )
        .unwrap();
        assert_eq!(query.fields.len(), 2);
        assert!(matches!(
            query.operations.last(),
            Some(DqlOperation::Sort(sorts)) if sorts.len() == 2
        ));
    }
}
