use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Component, Path};
use std::sync::{Arc, Mutex, OnceLock};

use anyhow::{Context, Result, anyhow, bail};
use pest::Parser;
use pest::iterators::Pair;
use pest_derive::Parser;
use regex::Regex;
use rquickjs::{Error as JsError, Function};
use serde_json::{Value, json};

use crate::core::{QueryAdapter, QueryContext, RecordSet, Row};
use crate::db::Database;
use crate::script::{QuickJsEngine, ScriptHostControl};

use super::expr::{Expr, total_value_order};
use super::tasks::{collect_tasks, collect_tasks_from_pages};
use super::{LinkIndex, page_value};

pub struct DataviewAdapter;
pub struct DataviewJsAdapter;

#[derive(Parser)]
#[grammar = "compat/dataview.pest"]
struct DqlParser;

impl QueryAdapter for DataviewAdapter {
    fn name(&self) -> &'static str {
        "dataview"
    }

    fn execute(&self, context: &QueryContext<'_>, source: &str) -> Result<RecordSet> {
        let query = DqlQuery::parse(source)?;
        let links = LinkIndex::build(context.database)?;
        let current_path = context
            .current_file
            .as_ref()
            .and_then(|path| path.strip_prefix(context.vault).ok())
            .map(|path| path.to_string_lossy().replace('\\', "/"));
        let current_value = if let Some(current_path) = current_path.as_ref() {
            let pages = context.database.all_pages()?;
            pages
                .iter()
                .find(|page| page.path == *current_path)
                .map(|page| page_value(page, &links))
        } else {
            None
        };

        let mut values = if query.kind == "task" {
            collect_tasks(context)?
        } else {
            let tasks = collect_tasks(context)?;
            let mut pages: Vec<Value> = context
                .database
                .all_pages()?
                .iter()
                .map(|page| page_value(page, &links))
                .collect();
            attach_file_tasks(&mut pages, &tasks);
            pages
        };
        if let Some(this_value) = &current_value {
            for value in &mut values {
                if let Some(object) = value.as_object_mut() {
                    object.insert("this".to_owned(), this_value.clone());
                }
            }
        }
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
                                total_value_order(&sort.expr.eval(left), &sort.expr.eval(right));
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
        let head = first.split_whitespace().next().unwrap_or_default();
        if !["LIST", "TABLE", "TASK", "CALENDAR"]
            .iter()
            .any(|kind| head.eq_ignore_ascii_case(kind))
        {
            bail!(
                "Dataview DQL requires a LIST, TABLE, TASK, or CALENDAR head; for example: LIST FROM \"Worklog\" WHERE {source}"
            );
        }
        let (kind, projection) = parse_head(first)?;
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
            apply_clause(&mut query, &line)?;
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

fn parse_head(source: &str) -> Result<(String, &str)> {
    let pair = DqlParser::parse(Rule::dql_head, source)
        .with_context(|| format!("invalid Dataview query head: {source}"))?
        .next()
        .context("empty Dataview query head")?;
    let mut kind = None;
    let mut projection = "";
    for inner in pair.into_inner() {
        match inner.as_rule() {
            Rule::dql_kind => kind = Some(inner.as_str().to_ascii_lowercase()),
            Rule::dql_rest => projection = inner.as_str().trim(),
            _ => {}
        }
    }
    let kind = kind.context("Dataview query must start with TABLE, LIST, TASK, or CALENDAR")?;
    Ok((kind, projection))
}

fn apply_clause(query: &mut DqlQuery, line: &str) -> Result<()> {
    let pair = DqlParser::parse(Rule::dql_line, line)
        .with_context(|| format!("invalid Dataview clause: {line}"))?
        .next()
        .context("empty Dataview clause")?;
    let clause = first_child(first_child(pair)?).context("empty Dataview clause")?;
    match clause.as_rule() {
        Rule::from_clause => {
            let source = clause_text(clause)?;
            query.source = parse_source(source, &source.to_ascii_lowercase());
        }
        Rule::where_clause => {
            query
                .operations
                .push(DqlOperation::Where(Expr::parse_dataview(clause_text(
                    clause,
                )?)?));
        }
        Rule::sort_clause => {
            let mut sorts = Vec::new();
            for sort in split_top_level(clause_text(clause)?, ',') {
                let sort = sort.trim();
                let descending = sort.to_ascii_lowercase().ends_with(" desc");
                let expression = sort
                    .strip_suffix(" DESC")
                    .or_else(|| sort.strip_suffix(" desc"))
                    .or_else(|| sort.strip_suffix(" ASC"))
                    .or_else(|| sort.strip_suffix(" asc"))
                    .unwrap_or(sort);
                sorts.push(DqlSort {
                    expr: Expr::parse_dataview(expression)?,
                    descending,
                });
            }
            query.operations.push(DqlOperation::Sort(sorts));
        }
        Rule::limit_clause => {
            let limit = first_child(clause)?
                .as_str()
                .parse()
                .context("invalid Dataview LIMIT")?;
            query.operations.push(DqlOperation::Limit(limit));
        }
        Rule::flatten_clause => {
            let source = clause_text(clause)?;
            let (expression, name) = split_alias(source);
            query.operations.push(DqlOperation::Flatten(DqlFlatten {
                name: name.unwrap_or(expression).to_owned(),
                expr: Expr::parse_dataview(expression)?,
            }));
        }
        Rule::group_clause => {
            query
                .operations
                .push(DqlOperation::Group(Expr::parse_dataview(clause_text(
                    clause,
                )?)?));
        }
        rule => bail!("unexpected Dataview clause rule: {rule:?}"),
    }
    Ok(())
}

fn first_child<R: pest::RuleType>(pair: Pair<'_, R>) -> Result<Pair<'_, R>> {
    pair.into_inner().next().context("empty parse node")
}

fn clause_text(pair: Pair<'_, Rule>) -> Result<&str> {
    Ok(pair
        .into_inner()
        .find(|inner| inner.as_rule() == Rule::dql_rest)
        .context("missing Dataview clause body")?
        .as_str()
        .trim())
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
            Ok((
                alias.trim_matches('"').to_owned(),
                Expr::parse_dataview(expression)?,
            ))
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
        let expanded = expand_views(context, source)?;
        let current_path = context
            .current_file
            .as_ref()
            .and_then(|path| path.strip_prefix(context.vault).ok())
            .map(|path| path.to_string_lossy().replace('\\', "/"));
        let database_path = context
            .database
            .path()
            .context("DataviewJS requires a file-backed database")?
            .to_path_buf();
        let host_memory_bytes = context
            .database
            .total_page_bytes()?
            .saturating_mul(8)
            .saturating_add(16 * 1024 * 1024);
        let host = DataviewJsHost::new(database_path)?;
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
            const __indexCache = new Map();
            const __pageCache = new Map();
            const __proxyCache = new Map();
            const __taskCache = new Map();
            const __linkCache = new Map();
            function __parseHost(load) {{
              try {{
                return JSON.parse(load());
              }} finally {{
                __mdq_end_host_phase();
              }}
            }}
            function __loadIndex(source) {{
              const key = source == null ? '' : String(source);
              if (!__indexCache.has(key)) {{
                __indexCache.set(key, __parseHost(() => __mdq_page_index(source)));
              }}
              return __indexCache.get(key);
            }}
            function __loadTasks(path) {{
              if (!__taskCache.has(path)) {{
                const tasks = __parseHost(() => __mdq_load_tasks(path));
                for (const field of __dateFields) {{
                  for (const task of tasks) {{
                    if (typeof task[field] === 'string') task[field] = new MdqDate(task[field]);
                  }}
                }}
                __taskCache.set(path, DataArray.from(tasks));
              }}
              return __taskCache.get(path);
            }}
            function __loadLinks(path) {{
              if (!__linkCache.has(path)) {{
                __linkCache.set(path, __parseHost(() => __mdq_load_links(path)));
              }}
              return __linkCache.get(path);
            }}
            function __loadPage(path) {{
              if (!__pageCache.has(path)) {{
                const page = __parseHost(() => __mdq_load_page(path));
                const metadata = Object.fromEntries(
                  Object.entries(page).filter(([key]) => key !== 'file')
                );
                page.note = metadata;
                page.file.properties = metadata;
                page.file.frontmatter = metadata;
                __pageCache.set(path, page);
              }}
              return __pageCache.get(path);
            }}
            function __pageProxy(descriptor) {{
              if (__proxyCache.has(descriptor.path)) return __proxyCache.get(descriptor.path);
              const slash = descriptor.path.lastIndexOf('/');
              const basicFile = {{
                path: descriptor.path,
                name: descriptor.name,
                basename: descriptor.name,
                folder: slash < 0 ? '' : descriptor.path.slice(0, slash),
                ext: 'md'
              }};
              const file = new Proxy(basicFile, {{
                get(target, key) {{
                  if (key === 'tasks') return __loadTasks(descriptor.path);
                  if (['links', 'outlinks', 'backlinks', 'inlinks', 'embeds'].includes(key)) {{
                    return Reflect.get(__loadLinks(descriptor.path), key);
                  }}
                  if (Reflect.has(target, key)) return Reflect.get(target, key);
                  return Reflect.get(__loadPage(descriptor.path).file, key);
                }},
                set(_target, key, value) {{
                  return Reflect.set(__loadPage(descriptor.path).file, key, value);
                }},
                has(target, key) {{
                  return Reflect.has(target, key) || key in __loadPage(descriptor.path).file;
                }},
                ownKeys() {{
                  return Reflect.ownKeys(__loadPage(descriptor.path).file);
                }},
                getOwnPropertyDescriptor(_target, key) {{
                  let value;
                  if (key === 'tasks') value = __loadTasks(descriptor.path);
                  else if (['links', 'outlinks', 'backlinks', 'inlinks', 'embeds'].includes(key)) {{
                    value = Reflect.get(__loadLinks(descriptor.path), key);
                  }} else value = Reflect.get(__loadPage(descriptor.path).file, key);
                  return {{value, enumerable: true, configurable: true, writable: true}};
                }}
              }});
              const page = new Proxy({{}}, {{
                get(_target, key) {{
                  if (key === 'file') return file;
                  return Reflect.get(__loadPage(descriptor.path), key);
                }},
                set(_target, key, value) {{
                  return Reflect.set(__loadPage(descriptor.path), key, value);
                }},
                has(_target, key) {{
                  return key === 'file' || key in __loadPage(descriptor.path);
                }},
                ownKeys() {{
                  return Reflect.ownKeys(__loadPage(descriptor.path));
                }},
                getOwnPropertyDescriptor(_target, key) {{
                  const value = key === 'file' ? file : Reflect.get(__loadPage(descriptor.path), key);
                  return {{value, enumerable: true, configurable: true, writable: true}};
                }}
              }});
              __proxyCache.set(descriptor.path, page);
              return page;
            }}
            function __pages(source) {{
              return DataArray.from(__loadIndex(source).map(__pageProxy));
            }}
            function __page(path) {{
              for (const descriptor of __loadIndex(null)) {{
                if (descriptor.path === path || descriptor.name === path) {{
                  return __pageProxy(descriptor);
                }}
              }}
              return null;
            }}
            const dv = {{
              pages(source) {{ return __pages(source); }},
              page(path) {{ return __page(path); }},
              current() {{ return __mdq.currentPath ? __page(__mdq.currentPath) : null; }},
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
        let result = QuickJsEngine::default().evaluate_with_setup(
            &program,
            &json!({"currentPath": current_path}),
            host_memory_bytes,
            move |control, ctx| {
                let index_host = host.clone();
                let index_control = control.clone();
                ctx.globals().set(
                    "__mdq_page_index",
                    Function::new(ctx.clone(), move |source: Option<String>| {
                        load_host_json(&index_control, || index_host.page_index_json(source))
                    })?,
                )?;

                let page_host = host.clone();
                let page_control = control.clone();
                ctx.globals().set(
                    "__mdq_load_page",
                    Function::new(ctx.clone(), move |path: String| {
                        load_host_json(&page_control, || page_host.page_json(path))
                    })?,
                )?;

                let tasks_host = host.clone();
                let tasks_control = control.clone();
                ctx.globals().set(
                    "__mdq_load_tasks",
                    Function::new(ctx.clone(), move |path: String| {
                        load_host_json(&tasks_control, || tasks_host.tasks_json(path))
                    })?,
                )?;

                let links_host = host;
                let links_control = control.clone();
                ctx.globals().set(
                    "__mdq_load_links",
                    Function::new(ctx.clone(), move |path: String| {
                        load_host_json(&links_control, || links_host.links_json(path))
                    })?,
                )?;

                let commit_control = control.clone();
                ctx.globals().set(
                    "__mdq_end_host_phase",
                    Function::new(ctx.clone(), move || commit_control.end_host_phase())?,
                )?;
                Ok(())
            },
        )?;
        Ok(dataviewjs_record_set(&result))
    }
}

fn dataviewjs_record_set(outputs: &Value) -> RecordSet {
    let outputs = outputs.as_array().cloned().unwrap_or_default();
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
    RecordSet::new("dataviewjs", rows)
}

#[derive(Clone)]
struct DataviewJsHost {
    database: Arc<Mutex<Database>>,
    indexes: Arc<Mutex<HashMap<String, std::result::Result<String, String>>>>,
    pages: Arc<Mutex<HashMap<String, std::result::Result<String, String>>>>,
    tasks: Arc<Mutex<HashMap<String, std::result::Result<String, String>>>>,
    links: Arc<Mutex<HashMap<String, std::result::Result<String, String>>>>,
    names: Arc<OnceLock<std::result::Result<HashMap<String, String>, String>>>,
}

impl DataviewJsHost {
    fn new(database_path: std::path::PathBuf) -> Result<Self> {
        Ok(Self {
            database: Arc::new(Mutex::new(Database::open_existing(&database_path)?)),
            indexes: Arc::new(Mutex::new(HashMap::new())),
            pages: Arc::new(Mutex::new(HashMap::new())),
            tasks: Arc::new(Mutex::new(HashMap::new())),
            links: Arc::new(Mutex::new(HashMap::new())),
            names: Arc::new(OnceLock::new()),
        })
    }

    fn page_names(&self) -> Result<&HashMap<String, String>> {
        match self.names.get_or_init(|| {
            (|| {
                let database = self
                    .database
                    .lock()
                    .map_err(|_| anyhow!("DataviewJS database lock is poisoned"))?;
                Ok(database
                    .all_page_refs()?
                    .into_iter()
                    .map(|page| {
                        let name = page
                            .path
                            .trim_end_matches(".md")
                            .rsplit('/')
                            .next()
                            .unwrap_or(&page.path)
                            .to_owned();
                        (name, page.path)
                    })
                    .collect())
            })()
            .map_err(|error: anyhow::Error| format!("{error:#}"))
        }) {
            Ok(names) => Ok(names),
            Err(error) => Err(anyhow!(error.clone())),
        }
    }

    fn page_index_json(&self, source: Option<String>) -> Result<String> {
        let key = source.unwrap_or_default();
        if let Some(result) = self
            .indexes
            .lock()
            .ok()
            .and_then(|indexes| indexes.get(&key).cloned())
        {
            return result.map_err(|error| anyhow!(error));
        }
        let result = (|| {
            let database = self
                .database
                .lock()
                .map_err(|_| anyhow!("DataviewJS database lock is poisoned"))?;
            let descriptors: Vec<Value> = if key.starts_with('#') {
                database
                    .all_pages()?
                    .into_iter()
                    .filter(|page| page_has_tag(page, &key))
                    .map(|page| page_descriptor(&page.path))
                    .collect()
            } else {
                let folder = key.trim_matches('"');
                database
                    .all_page_refs()?
                    .into_iter()
                    .filter(|page| folder.is_empty() || page.path.starts_with(folder))
                    .map(|page| page_descriptor(&page.path))
                    .collect()
            };
            serde_json::to_string(&descriptors).map_err(Into::into)
        })()
        .map_err(|error: anyhow::Error| format!("{error:#}"));
        if let Ok(mut indexes) = self.indexes.lock() {
            indexes.insert(key, result.clone());
        }
        result.map_err(|error| anyhow!(error))
    }

    fn page_json(&self, path: String) -> Result<String> {
        if let Some(result) = self
            .pages
            .lock()
            .ok()
            .and_then(|pages| pages.get(&path).cloned())
        {
            return result.map_err(|error| anyhow!(error));
        }
        let result = (|| {
            let page = {
                let database = self
                    .database
                    .lock()
                    .map_err(|_| anyhow!("DataviewJS database lock is poisoned"))?;
                database
                    .page_record(&path)?
                    .with_context(|| format!("DataviewJS page not found: {path}"))?
            };
            let mut value = page_value(&page, &LinkIndex::empty());
            compact_dataviewjs_pages(std::slice::from_mut(&mut value));
            resolve_links_in_value(&mut value, self.page_names()?);
            serde_json::to_string(&value).map_err(Into::into)
        })()
        .map_err(|error: anyhow::Error| format!("{error:#}"));
        if let Ok(mut pages) = self.pages.lock() {
            pages.insert(path, result.clone());
        }
        result.map_err(|error| anyhow!(error))
    }

    fn links_json(&self, path: String) -> Result<String> {
        if let Some(result) = self
            .links
            .lock()
            .ok()
            .and_then(|links| links.get(&path).cloned())
        {
            return result.map_err(|error| anyhow!(error));
        }
        let result = (|| {
            let database = self
                .database
                .lock()
                .map_err(|_| anyhow!("DataviewJS database lock is poisoned"))?;
            let page = database
                .page_record(&path)?
                .with_context(|| format!("DataviewJS page not found: {path}"))?;
            let links = LinkIndex::build_for_page(&database, &path)?;
            let value = page_value(&page, &links);
            let file = &value["file"];
            serde_json::to_string(&json!({
                "links": file["links"],
                "outlinks": file["outlinks"],
                "backlinks": file["backlinks"],
                "inlinks": file["inlinks"],
                "embeds": file["embeds"],
            }))
            .map_err(Into::into)
        })()
        .map_err(|error: anyhow::Error| format!("{error:#}"));
        if let Ok(mut links) = self.links.lock() {
            links.insert(path, result.clone());
        }
        result.map_err(|error| anyhow!(error))
    }

    fn tasks_json(&self, path: String) -> Result<String> {
        if let Some(result) = self
            .tasks
            .lock()
            .ok()
            .and_then(|tasks| tasks.get(&path).cloned())
        {
            return result.map_err(|error| anyhow!(error));
        }
        let result = (|| {
            let database = self
                .database
                .lock()
                .map_err(|_| anyhow!("DataviewJS database lock is poisoned"))?;
            let page = database
                .page_record(&path)?
                .with_context(|| format!("DataviewJS page not found: {path}"))?;
            let tasks = collect_tasks_from_pages(std::slice::from_ref(&page))?;
            serde_json::to_string(&tasks).map_err(Into::into)
        })()
        .map_err(|error: anyhow::Error| format!("{error:#}"));
        if let Ok(mut tasks) = self.tasks.lock() {
            tasks.insert(path, result.clone());
        }
        result.map_err(|error| anyhow!(error))
    }
}

fn load_host_json(
    control: &ScriptHostControl,
    load: impl FnOnce() -> Result<String>,
) -> rquickjs::Result<String> {
    control.begin_host_phase();
    match load() {
        Ok(json) => Ok(json),
        Err(error) => {
            control.end_host_phase();
            Err(JsError::new_from_js_message(
                "mdq host",
                "JavaScript",
                format!("{error:#}"),
            ))
        }
    }
}

fn page_descriptor(path: &str) -> Value {
    let name = path
        .trim_end_matches(".md")
        .rsplit('/')
        .next()
        .unwrap_or(path);
    json!({"path": path, "name": name})
}

fn page_has_tag(page: &crate::model::PageRecord, tag: &str) -> bool {
    let tag = tag.trim_start_matches('#');
    let frontmatter_match = match page.metadata.get("tags") {
        Some(Value::Array(tags)) => tags.iter().any(|value| {
            value
                .as_str()
                .is_some_and(|value| value.trim_start_matches('#') == tag)
        }),
        Some(Value::String(value)) => value.trim_start_matches('#') == tag,
        _ => false,
    };
    frontmatter_match
        || crate::markdown::extract_tags(&page.body)
            .iter()
            .any(|value| value == tag)
}

/// Removes aliases that duplicate every page's frontmatter before crossing the
/// QuickJS memory boundary. The JavaScript bootstrap restores these aliases as
/// shared objects before user code runs.
fn compact_dataviewjs_pages(pages: &mut [Value]) {
    for page in pages {
        let Some(page) = page.as_object_mut() else {
            continue;
        };
        page.remove("note");
        if let Some(file) = page.get_mut("file").and_then(Value::as_object_mut) {
            file.remove("properties");
            file.remove("frontmatter");
        }
    }
}

/// Groups collected tasks by their page path and attaches them as
/// `file.tasks` on each page value, mirroring Dataview's implicit
/// `file.tasks` page field.
fn attach_file_tasks(pages: &mut [Value], tasks: &[Value]) {
    let mut by_path: HashMap<&str, Vec<Value>> = HashMap::new();
    for task in tasks {
        if let Some(path) = task["path"].as_str() {
            by_path.entry(path).or_default().push(task.clone());
        }
    }
    for page in pages {
        let path = page["file"]["path"].as_str().unwrap_or_default();
        let page_tasks = by_path.remove(path).unwrap_or_default();
        page["file"]["tasks"] = Value::Array(page_tasks);
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
    use crate::core::{QueryAdapter, QueryContext};
    use crate::db::{Database, default_db_path};
    use std::fs;

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

    #[test]
    fn parses_uppercase_boolean_operators() {
        let query = DqlQuery::parse(
            "TABLE file.path FROM \"\" WHERE contains(file.name, \"Alpha\") OR file.name == \"Beta\"",
        )
        .unwrap();
        assert!(matches!(
            query.operations.as_slice(),
            [DqlOperation::Where(_)]
        ));
    }

    #[test]
    fn sort_uses_total_order_for_mixed_values() {
        let temp = tempfile::tempdir().unwrap();
        let vault = temp.path().join("vault");
        fs::create_dir_all(&vault).unwrap();
        fs::write(
            vault.join("インターン-a.md"),
            "---\nmodified: 2026-07-10\n---\n# A\n",
        )
        .unwrap();
        fs::write(
            vault.join("インターン-b.md"),
            "---\nmodified:\n  nested: true\n---\n# B\n",
        )
        .unwrap();
        fs::write(vault.join("other.md"), "# Other\n").unwrap();

        let db_path = default_db_path(&vault).unwrap();
        let mut database = Database::open(&db_path).unwrap();
        database.rebuild(&vault).unwrap();
        let context = QueryContext {
            database: &database,
            vault: &vault,
            current_file: None,
        };

        let result = DataviewAdapter
            .execute(
                &context,
                "TABLE file.name, modified FROM \"\" WHERE contains(file.name, \"インターン\") SORT modified DESC",
            )
            .unwrap();

        assert_eq!(result.rows.len(), 2);
    }

    #[test]
    fn page_query_exposes_file_tasks_to_lambda_predicates() {
        let temp = tempfile::tempdir().unwrap();
        let vault = temp.path().join("vault");
        fs::create_dir_all(&vault).unwrap();
        fs::write(
            vault.join("with-open-task.md"),
            "---\nstatus: \"active\"\n---\n# With open task\n\n- [ ] Pending work [due::2026-07-15]\n- [x] Finished work [due::2026-07-01]\n",
        )
        .unwrap();
        fs::write(
            vault.join("all-done.md"),
            "---\nstatus: \"active\"\n---\n# All done\n\n- [x] Finished work [due::2026-07-15]\n",
        )
        .unwrap();
        fs::write(
            vault.join("no-tasks.md"),
            "---\nstatus: \"active\"\n---\n# No tasks\n",
        )
        .unwrap();

        let db_path = default_db_path(&vault).unwrap();
        let mut database = Database::open(&db_path).unwrap();
        database.rebuild(&vault).unwrap();
        let context = QueryContext {
            database: &database,
            vault: &vault,
            current_file: None,
        };

        let result = DataviewAdapter
            .execute(
                &context,
                "TABLE file.path FROM \"\" WHERE status = \"active\" AND any(file.tasks, (t) => !t.completed AND t.due = date(\"2026-07-15\"))",
            )
            .unwrap();

        assert_eq!(result.rows.len(), 1);
        assert_eq!(
            result.rows[0].get("file.path"),
            Some(&Value::String("with-open-task.md".to_owned()))
        );
    }
}
