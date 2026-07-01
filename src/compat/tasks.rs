use std::collections::{BTreeMap, HashSet};
use std::hash::{Hash, Hasher};
use std::sync::LazyLock;

use anyhow::{Context, Result, bail};
use chrono::{Datelike, Duration, Local, NaiveDate, Weekday};
use chrono_english::{Dialect, parse_date_string};
use pest::Parser;
use pest::iterators::Pair;
use pest_derive::Parser;
use regex::{Captures, Regex, RegexBuilder};
use serde_json::{Map, Value, json};

use crate::core::{QueryAdapter, QueryContext, RecordSet, Row};
use crate::markdown::extract_links;
use crate::model::ParsedLink;
use crate::script::{QuickJsEngine, ScriptEngine};

use super::expr::value_order;

#[derive(Parser)]
#[grammar = "compat/tasks.pest"]
struct TasksQueryParser;

static TASK: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(\s*)((?:[-*+]|\d+[.)]))\s+\[([^\]])\]\s+(.*)$").unwrap());
static INLINE_FIELD: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\[([^\[\]:]+)::\s*([^\]]*)\]").unwrap());
static TAG: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(^|[^\p{Alphabetic}\p{Number}_])#([\p{Alphabetic}\p{Number}_/-]+)").unwrap()
});
static HEADING: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s{0,3}#{1,6}\s+(.+?)(?:\s+#+)?\s*$").unwrap());
static ORDINAL_DATE_SUFFIX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\b(\d{1,2})(st|nd|rd|th)\b").unwrap());
static TASK_EMOJI_FIELD: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?:📅|✅|⏳|🛫|➕|❌)\s*(\d{4}-\d{2}-\d{2})|(?:🔁\s*([^📅✅⏳🛫➕❌🔺⏫🔼🔽⏬🆔⛔🏁]+))|(?:🏁\s*(delete|keep))|(?:🆔\s*([A-Za-z0-9_-]+))|(?:⛔\s*([A-Za-z0-9_, -]+))|(?:🔺|⏫|🔼|🔽|⏬)",
    )
    .unwrap()
});

pub struct TasksAdapter {
    status_overrides: Vec<TaskStatusOverride>,
    global_filter: Option<String>,
    global_query: Option<String>,
}

impl TasksAdapter {
    pub fn new() -> Self {
        Self {
            status_overrides: Vec::new(),
            global_filter: None,
            global_query: None,
        }
    }

    pub fn with_status_specs(specs: &[String]) -> Result<Self> {
        let status_overrides = specs
            .iter()
            .map(|spec| TaskStatusOverride::parse(spec))
            .collect::<Result<Vec<_>>>()?;
        Ok(Self {
            status_overrides,
            global_filter: None,
            global_query: None,
        })
    }

    pub fn with_settings(
        status_specs: &[String],
        global_filter: Option<String>,
        global_query: Option<String>,
    ) -> Result<Self> {
        let mut adapter = Self::with_status_specs(status_specs)?;
        adapter.global_filter = global_filter.filter(|value| !value.is_empty());
        adapter.global_query = global_query.filter(|value| !value.trim().is_empty());
        Ok(adapter)
    }
}

impl Default for TasksAdapter {
    fn default() -> Self {
        Self::new()
    }
}

impl QueryAdapter for TasksAdapter {
    fn name(&self) -> &'static str {
        "tasks"
    }

    fn execute(&self, context: &QueryContext<'_>, source: &str) -> Result<RecordSet> {
        let query_context = query_value(context);
        let source = strip_tasks_comments(&source);
        let source = expand_query_placeholders(&source, &query_context);
        let source = apply_global_query(&source, self.global_query.as_deref());
        let source = strip_tasks_comments(&source);
        let query = TaskQuery::parse(&source)?;
        let mut tasks = collect_tasks_with_settings(
            context,
            &self.status_overrides,
            self.global_filter.as_deref(),
        )?;
        enrich_dependency_state(&mut tasks);

        let script = QuickJsEngine::default();
        let mut filter_diagnostics = Vec::new();
        tasks.retain(|row| {
            query.filters.iter().all(|filter| {
                filter
                    .matches(row, &query_context, &script)
                    .unwrap_or_else(|error| {
                        filter_diagnostics.push(error.to_string());
                        false
                    })
            })
        });
        let has_filter_matches = !tasks.is_empty();
        let mut sort_diagnostics = Vec::new();
        for sort in query.sorts.iter().take(query.user_sort_count) {
            if let TaskSort::Field(field, _) = sort {
                if !tasks.is_empty() && tasks.iter().all(|t| value_at_path(t, field).is_none()) {
                    sort_diagnostics.push(format!(
                        "sort field '{}' does not exist on any task — possible typo",
                        field
                    ));
                }
            }
        }
        sort_tasks(&mut tasks, &query.sorts, &query_context, &script);
        if let Some(limit) = query.limit {
            tasks.truncate(limit);
        }
        let mut query_diagnostics = query.diagnostics;
        if query.group_limit.is_some() && query.groups.is_empty() {
            query_diagnostics
                .push("limit groups has no effect without a group by instruction".to_owned());
        }
        let rows = if query.groups.is_empty() {
            tasks
                .into_iter()
                .map(|value| value.as_object().unwrap().clone().into_iter().collect())
                .collect()
        } else {
            group_rows(
                tasks,
                &query.groups,
                query.group_limit,
                &query_context,
                &script,
            )
        };
        let mut result = RecordSet::new("tasks", rows);
        result.diagnostics = query_diagnostics;
        if !has_filter_matches {
            result
                .diagnostics
                .extend(unique_strings(filter_diagnostics));
        }
        result.diagnostics.extend(sort_diagnostics);
        Ok(result)
    }
}

pub(crate) fn collect_tasks(context: &QueryContext<'_>) -> Result<Vec<Value>> {
    collect_tasks_with_settings(context, &[], None)
}

fn collect_tasks_with_settings(
    context: &QueryContext<'_>,
    status_overrides: &[TaskStatusOverride],
    global_filter: Option<&str>,
) -> Result<Vec<Value>> {
    let mut tasks = Vec::new();
    for page in context.database.all_pages()? {
        let outlinks_in_body = link_values(extract_links(&page.body));
        let outlinks_in_properties = frontmatter_link_values(&page.metadata);
        let file_outlinks = merge_link_values(&outlinks_in_properties, &outlinks_in_body);
        let mut heading: Option<String> = None;
        let mut task_stack: Vec<(usize, Value)> = Vec::new();
        for (line_index, line) in page.body.lines().enumerate() {
            let source_line = page.body_start_line + line_index;
            let task_line = strip_blockquote_prefix(line);
            if let Some(captures) = HEADING.captures(task_line) {
                heading = Some(captures[1].to_owned());
                task_stack.clear();
                continue;
            }
            let Some(captures) = TASK.captures(task_line) else {
                continue;
            };
            let task_source = &captures[4];
            if global_filter.is_some_and(|filter| !task_source.contains(filter)) {
                continue;
            }
            let indentation = captures[1].chars().count();
            while task_stack
                .last()
                .is_some_and(|(parent_indentation, _)| *parent_indentation >= indentation)
            {
                task_stack.pop();
            }
            let parent = task_stack.last().map(|(_, task)| task);
            let task = task_row(
                &page.path,
                source_line,
                &captures[3],
                task_source,
                global_filter,
                &page.metadata,
                heading.as_deref(),
                &captures[1],
                &captures[2],
                &outlinks_in_body,
                &outlinks_in_properties,
                &file_outlinks,
                parent,
                status_overrides,
            );
            task_stack.push((indentation, task.clone()));
            tasks.push(task);
        }
    }
    Ok(tasks)
}

struct TaskQuery {
    filters: Vec<TaskFilter>,
    sorts: Vec<TaskSort>,
    user_sort_count: usize,
    groups: Vec<TaskGroup>,
    limit: Option<usize>,
    group_limit: Option<usize>,
    diagnostics: Vec<String>,
}

enum TaskFilter {
    Never,
    Done(bool),
    Date {
        field: String,
        relation: DateRelation,
        query: DateQuery,
    },
    InvalidDate(String),
    HasField {
        field: String,
        expected: bool,
    },
    Text {
        field: String,
        includes: bool,
        value: String,
    },
    Status {
        field: String,
        expected: String,
        equals: bool,
    },
    Regex {
        field: String,
        matches: bool,
        regex: Regex,
    },
    Priority {
        relation: PriorityRelation,
        priority: i64,
    },
    Not(Box<TaskFilter>),
    Any(Vec<TaskFilter>),
    All(Vec<TaskFilter>),
    Xor(Box<TaskFilter>, Box<TaskFilter>),
    Script(String),
}

#[derive(Clone, Copy)]
enum DateRelation {
    On,
    Before,
    After,
    OnOrBefore,
    OnOrAfter,
    In,
    InOrBefore,
    InOrAfter,
}

#[derive(Clone)]
enum DateQuery {
    Single(NaiveDate),
    Range { start: NaiveDate, end: NaiveDate },
}

enum PriorityRelation {
    Is,
    IsNot,
    Above,
    Below,
}

#[derive(Clone)]
struct TaskStatus {
    name: String,
    status_type: String,
    next_symbol: String,
}

struct TaskStatusOverride {
    symbol: String,
    status: TaskStatus,
}

impl TaskStatus {
    fn for_symbol(symbol: &str, overrides: &[TaskStatusOverride]) -> Self {
        if let Some(override_status) = overrides
            .iter()
            .find(|override_status| override_status.symbol == symbol)
        {
            return override_status.status.clone();
        }
        match symbol {
            " " => Self {
                name: "Todo".to_owned(),
                status_type: "TODO".to_owned(),
                next_symbol: "x".to_owned(),
            },
            "x" | "X" => Self {
                name: "Done".to_owned(),
                status_type: "DONE".to_owned(),
                next_symbol: " ".to_owned(),
            },
            "/" => Self {
                name: "In Progress".to_owned(),
                status_type: "IN_PROGRESS".to_owned(),
                next_symbol: "x".to_owned(),
            },
            "-" => Self {
                name: "Cancelled".to_owned(),
                status_type: "CANCELLED".to_owned(),
                next_symbol: " ".to_owned(),
            },
            "" => Self {
                name: "EMPTY".to_owned(),
                status_type: "EMPTY".to_owned(),
                next_symbol: String::new(),
            },
            _ => Self {
                name: "Unknown".to_owned(),
                status_type: "TODO".to_owned(),
                next_symbol: "x".to_owned(),
            },
        }
    }

    fn is_done(&self) -> bool {
        matches!(self.status_type.as_str(), "DONE" | "CANCELLED" | "NON_TASK")
    }

    fn group_text(&self) -> String {
        match self.status_type.as_str() {
            "IN_PROGRESS" => "%%1%%IN_PROGRESS",
            "TODO" => "%%2%%TODO",
            "ON_HOLD" => "%%3%%ON_HOLD",
            "DONE" => "%%4%%DONE",
            "CANCELLED" => "%%5%%CANCELLED",
            "NON_TASK" => "%%6%%NON_TASK",
            "EMPTY" => "%%7%%EMPTY",
            _ => &self.status_type,
        }
        .to_owned()
    }
}

impl TaskStatusOverride {
    fn parse(spec: &str) -> Result<Self> {
        let (symbol, value) = spec.split_once('=').with_context(|| {
            format!(
                "invalid --tasks-status '{spec}': expected SYMBOL=TYPE or SYMBOL=NAME:TYPE[:NEXT]"
            )
        })?;
        let symbol = parse_status_symbol(symbol.trim())
            .with_context(|| format!("invalid --tasks-status '{spec}': status symbol is empty"))?;
        let parts: Vec<&str> = value.splitn(3, ':').map(str::trim).collect();
        let (name, status_type, next_symbol) = match parts.as_slice() {
            [status_type] => {
                let status_type = normalize_status_type(status_type)?;
                let name = default_status_name(&status_type).to_owned();
                let next_symbol = default_next_symbol(&status_type).to_owned();
                (name, status_type, next_symbol)
            }
            [name, status_type] => {
                let status_type = normalize_status_type(status_type)?;
                let next_symbol = default_next_symbol(&status_type).to_owned();
                ((*name).to_owned(), status_type, next_symbol)
            }
            [name, status_type, next_symbol] => (
                (*name).to_owned(),
                normalize_status_type(status_type)?,
                parse_status_symbol(next_symbol).unwrap_or_default(),
            ),
            _ => bail!("invalid --tasks-status '{spec}'"),
        };
        if name.is_empty() {
            bail!("invalid --tasks-status '{spec}': status name is empty");
        }
        Ok(Self {
            symbol,
            status: TaskStatus {
                name,
                status_type,
                next_symbol,
            },
        })
    }
}

fn parse_status_symbol(value: &str) -> Option<String> {
    match value {
        "" => None,
        "space" | "<space>" | "␠" => Some(" ".to_owned()),
        value => Some(value.to_owned()),
    }
}

fn normalize_status_type(value: &str) -> Result<String> {
    let normalized = value.trim().replace(['-', ' '], "_").to_ascii_uppercase();
    match normalized.as_str() {
        "TODO" | "DONE" | "IN_PROGRESS" | "ON_HOLD" | "CANCELLED" | "NON_TASK" | "EMPTY" => {
            Ok(normalized)
        }
        _ => bail!(
            "invalid Tasks status type '{value}'; expected TODO, DONE, IN_PROGRESS, ON_HOLD, CANCELLED, NON_TASK, or EMPTY"
        ),
    }
}

fn default_status_name(status_type: &str) -> &str {
    match status_type {
        "TODO" => "Todo",
        "DONE" => "Done",
        "IN_PROGRESS" => "In Progress",
        "ON_HOLD" => "On Hold",
        "CANCELLED" => "Cancelled",
        "NON_TASK" => "Non-Task",
        "EMPTY" => "EMPTY",
        _ => "Unknown",
    }
}

fn default_next_symbol(status_type: &str) -> &str {
    match status_type {
        "DONE" | "CANCELLED" => " ",
        "EMPTY" => "",
        _ => "x",
    }
}

enum TaskSort {
    Field(String, bool),
    Script(String, bool),
}

enum TaskGroup {
    Field(String, bool),
    Script(String),
}

impl TaskQuery {
    fn parse(source: &str) -> Result<Self> {
        let mut query = Self {
            filters: Vec::new(),
            sorts: Vec::new(),
            user_sort_count: 0,
            groups: Vec::new(),
            limit: None,
            group_limit: None,
            diagnostics: Vec::new(),
        };
        for raw in logical_lines(source) {
            let line = raw.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let parsed = TasksQueryParser::parse(Rule::line, line)
                .with_context(|| format!("unsupported Tasks instruction: {line}"));
            match parsed {
                Ok(mut pairs) => {
                    let Some(line_pair) = pairs.next() else {
                        continue;
                    };
                    if apply_instruction(line_pair, &mut query).is_err() {
                        query
                            .diagnostics
                            .push(format!("unsupported Tasks instruction: {line}"));
                        query.filters.push(TaskFilter::Never);
                    }
                }
                Err(_) => {
                    query
                        .diagnostics
                        .push(format!("unsupported Tasks instruction: {line}"));
                    query.filters.push(TaskFilter::Never);
                }
            }
        }
        query.user_sort_count = query.sorts.len();
        query.sorts.extend(default_tasks_sorts());
        Ok(query)
    }
}

fn default_tasks_sorts() -> Vec<TaskSort> {
    vec![
        TaskSort::Field("statusTypeGroupText".to_owned(), false),
        TaskSort::Field("urgency".to_owned(), true),
        TaskSort::Field("due".to_owned(), false),
        TaskSort::Field("priorityNumber".to_owned(), false),
        TaskSort::Field("path".to_owned(), false),
    ]
}

fn apply_instruction(pair: Pair<'_, Rule>, query: &mut TaskQuery) -> Result<()> {
    let inner = first_significant(pair).context("empty Tasks instruction")?;
    match inner.as_rule() {
        Rule::function_filter => query.filters.push(TaskFilter::Script(last_text(inner))),
        Rule::function_sort => {
            let (source, reverse) = function_source_with_reverse(inner);
            query.sorts.push(TaskSort::Script(source, reverse));
        }
        Rule::function_group => query.groups.push(TaskGroup::Script(last_text(inner))),
        Rule::group => {
            let (field, reverse) = field_with_reverse(inner, FieldUse::Group);
            query.groups.push(TaskGroup::Field(field, reverse));
        }
        Rule::sort => {
            let (field, reverse) = field_with_reverse(inner, FieldUse::Sort);
            query.sorts.push(TaskSort::Field(field, reverse));
        }
        Rule::limit => {
            let limit = first_int(inner.clone())
                .context("missing Tasks limit")?
                .parse()
                .context("invalid Tasks limit")?;
            if inner.as_str().starts_with("limit groups") {
                query.group_limit = Some(limit);
            } else {
                query.limit = Some(limit);
            }
        }
        Rule::display => {}
        Rule::ignore_global_query => {}
        Rule::boolean_expr => query.filters.push(build_filter(inner)?),
        rule => query
            .diagnostics
            .push(format!("unsupported Tasks instruction rule: {rule:?}")),
    }
    Ok(())
}

fn apply_global_query(source: &str, global_query: Option<&str>) -> String {
    let Some(global_query) = global_query else {
        return source.to_owned();
    };
    let ignores_global_query = logical_lines(source)
        .into_iter()
        .any(|line| line.trim().eq_ignore_ascii_case("ignore global query"));
    if ignores_global_query {
        source.to_owned()
    } else if source.trim().is_empty() {
        global_query.to_owned()
    } else {
        format!("{global_query}\n{source}")
    }
}

fn first_significant(pair: Pair<'_, Rule>) -> Option<Pair<'_, Rule>> {
    let mut current = pair;
    loop {
        match current.as_rule() {
            Rule::line | Rule::instruction => {
                current = current
                    .into_inner()
                    .find(|inner| !matches!(inner.as_rule(), Rule::EOI))?;
            }
            _ => return Some(current),
        }
    }
}

fn build_filter(pair: Pair<'_, Rule>) -> Result<TaskFilter> {
    match pair.as_rule() {
        Rule::boolean_expr | Rule::filter_primary => {
            build_filter(pair.into_inner().next().context("empty filter")?)
        }
        Rule::xor_expr => fold_xor_filter(pair),
        Rule::or_expr => fold_or_filter(pair),
        Rule::and_expr => fold_and_filter(pair),
        Rule::unary_expr => {
            let mut not_count = 0;
            let mut filter = None;
            for inner in pair.into_inner() {
                if inner.as_rule() == Rule::NOT {
                    not_count += 1;
                } else {
                    filter = Some(build_filter(inner)?);
                }
            }
            let mut filter = filter.context("NOT without filter")?;
            for _ in 0..not_count {
                filter = TaskFilter::Not(Box::new(filter));
            }
            Ok(filter)
        }
        Rule::primitive_filter => {
            build_filter(pair.into_inner().next().context("empty primitive")?)
        }
        Rule::done_filter => Ok(TaskFilter::Done(pair.as_str().eq_ignore_ascii_case("done"))),
        Rule::date_filter => build_date_filter(pair),
        Rule::date_invalid_filter => {
            let field = pair
                .into_inner()
                .find(|inner| inner.as_rule() == Rule::date_field_name)
                .map(|inner| date_field_name(inner.as_str()).to_owned())
                .context("missing invalid date field")?;
            Ok(TaskFilter::InvalidDate(field))
        }
        Rule::date_presence_filter => {
            let mut inner = pair.into_inner();
            let expected = inner
                .next()
                .context("missing presence")?
                .as_str()
                .eq_ignore_ascii_case("has");
            let field = date_field_name(inner.next().context("missing date field")?.as_str());
            Ok(TaskFilter::HasField {
                field: field.to_owned(),
                expected,
            })
        }
        Rule::state_filter => build_state_filter(pair),
        Rule::field_presence_filter => build_presence_filter(pair),
        Rule::priority_filter => build_priority_filter(pair),
        Rule::regex_filter => build_regex_filter(pair),
        Rule::text_filter => build_text_filter(pair),
        Rule::status_filter => build_status_filter(pair),
        Rule::exclude_sub_items => Ok(TaskFilter::HasField {
            field: "isSubItem".to_owned(),
            expected: false,
        }),
        rule => anyhow::bail!("unsupported Tasks filter rule: {rule:?}"),
    }
}

fn fold_xor_filter(pair: Pair<'_, Rule>) -> Result<TaskFilter> {
    let mut filters = Vec::new();
    for inner in pair.into_inner() {
        if inner.as_rule() != Rule::XOR {
            filters.push(build_filter(inner)?);
        }
    }
    Ok(if filters.len() == 1 {
        filters.remove(0)
    } else {
        let first = filters.remove(0);
        TaskFilter::Xor(Box::new(first), Box::new(TaskFilter::Any(filters)))
    })
}

fn fold_or_filter(pair: Pair<'_, Rule>) -> Result<TaskFilter> {
    let mut filters = Vec::new();
    let mut negate_next = false;
    for inner in pair.into_inner() {
        match inner.as_rule() {
            Rule::OR => negate_next = false,
            Rule::OR_NOT => negate_next = true,
            _ => {
                let mut filter = build_filter(inner)?;
                if negate_next {
                    filter = TaskFilter::Not(Box::new(filter));
                    negate_next = false;
                }
                filters.push(filter);
            }
        }
    }
    Ok(if filters.len() == 1 {
        filters.remove(0)
    } else {
        TaskFilter::Any(filters)
    })
}

fn fold_and_filter(pair: Pair<'_, Rule>) -> Result<TaskFilter> {
    let mut filters = Vec::new();
    let mut negate_next = false;
    for inner in pair.into_inner() {
        match inner.as_rule() {
            Rule::AND => negate_next = false,
            Rule::AND_NOT => negate_next = true,
            _ => {
                let mut filter = build_filter(inner)?;
                if negate_next {
                    filter = TaskFilter::Not(Box::new(filter));
                    negate_next = false;
                }
                filters.push(filter);
            }
        }
    }
    Ok(if filters.len() == 1 {
        filters.remove(0)
    } else {
        TaskFilter::All(filters)
    })
}

fn build_date_filter(pair: Pair<'_, Rule>) -> Result<TaskFilter> {
    let mut field = None;
    let mut relation = DateRelation::On;
    let mut spec = None;
    for inner in pair.into_inner() {
        match inner.as_rule() {
            Rule::date_field => field = Some(date_field_name(inner.as_str()).to_owned()),
            Rule::date_relation => relation = date_relation(inner.as_str()),
            Rule::date_spec => spec = Some(inner.as_str().to_owned()),
            _ => {}
        }
    }
    let query = resolve_date_query(&spec.context("missing date spec")?)
        .context("unsupported Tasks date expression")?;
    Ok(TaskFilter::Date {
        field: field.context("missing date field")?,
        relation,
        query,
    })
}

fn build_presence_filter(pair: Pair<'_, Rule>) -> Result<TaskFilter> {
    let mut inner = pair.into_inner();
    let expected = inner
        .next()
        .context("missing presence")?
        .as_str()
        .eq_ignore_ascii_case("has");
    let field = match inner.next().context("missing presence field")?.as_str() {
        "tags" => "tags",
        "id" => "id",
        "depends on" => "depends_on",
        "recurring" => "recurrence",
        "blocking" => "isBlocking",
        "blocked" => "isBlocked",
        value => anyhow::bail!("unsupported presence field: {value}"),
    };
    Ok(TaskFilter::HasField {
        field: field.to_owned(),
        expected,
    })
}

fn build_state_filter(pair: Pair<'_, Rule>) -> Result<TaskFilter> {
    let source = pair.as_str();
    let expected = !source.contains("not ");
    let field = if source.contains("recurring") {
        "recurrence"
    } else if source.contains("blocking") {
        "isBlocking"
    } else {
        "isBlocked"
    };
    Ok(TaskFilter::HasField {
        field: field.to_owned(),
        expected,
    })
}

fn build_priority_filter(pair: Pair<'_, Rule>) -> Result<TaskFilter> {
    let mut relation = PriorityRelation::Is;
    let mut priority = None;
    for inner in pair.into_inner() {
        match inner.as_rule() {
            Rule::priority_relation => {
                relation = match inner.as_str().trim() {
                    "not" => PriorityRelation::IsNot,
                    "above" => PriorityRelation::Above,
                    "below" => PriorityRelation::Below,
                    _ => PriorityRelation::Is,
                }
            }
            Rule::priority_name => priority = priority_number(inner.as_str()),
            _ => {}
        }
    }
    Ok(TaskFilter::Priority {
        relation,
        priority: priority.context("missing priority")?,
    })
}

fn build_regex_filter(pair: Pair<'_, Rule>) -> Result<TaskFilter> {
    let mut field = None;
    let mut matches = true;
    let mut regex = None;
    for inner in pair.into_inner() {
        match inner.as_rule() {
            Rule::text_field => field = Some(text_field_name(inner.as_str()).to_owned()),
            Rule::regex_relation => matches = inner.as_str() == "matches",
            Rule::regex => regex = parse_js_regex(inner.as_str()),
            _ => {}
        }
    }
    Ok(TaskFilter::Regex {
        field: field.context("missing regex field")?,
        matches,
        regex: regex.context("invalid regex")?,
    })
}

fn build_text_filter(pair: Pair<'_, Rule>) -> Result<TaskFilter> {
    let mut field = None;
    let mut includes = true;
    let mut value = None;
    for inner in pair.into_inner() {
        match inner.as_rule() {
            Rule::text_field => field = Some(text_field_name(inner.as_str()).to_owned()),
            Rule::text_relation => {
                includes = inner.as_str().contains("include") && !inner.as_str().contains("not")
            }
            Rule::text_value => value = Some(inner.as_str().to_ascii_lowercase()),
            _ => {}
        }
    }
    Ok(TaskFilter::Text {
        field: field.context("missing text field")?,
        includes,
        value: value.context("missing text value")?,
    })
}

fn build_status_filter(pair: Pair<'_, Rule>) -> Result<TaskFilter> {
    let mut field = None;
    let mut relation = None;
    let mut value = None;
    for inner in pair.into_inner() {
        match inner.as_rule() {
            Rule::status_field => field = Some(status_field_name(inner.as_str()).to_owned()),
            Rule::status_relation => relation = Some(inner.as_str().to_owned()),
            Rule::status_value => value = Some(inner.as_str().to_ascii_lowercase()),
            _ => {}
        }
    }
    let field = field.context("missing status field")?;
    let relation = relation.context("missing status relation")?;
    let mut value = value.context("missing status value")?;
    if field == "status.type" && !relation.contains("include") {
        value = normalize_status_type(&value)?.to_ascii_lowercase();
    }
    if relation.contains("include") {
        Ok(TaskFilter::Text {
            field,
            includes: !relation.contains("not"),
            value,
        })
    } else {
        Ok(TaskFilter::Status {
            field,
            expected: value,
            equals: !relation.contains("not"),
        })
    }
}

#[derive(Clone, Copy)]
enum FieldUse {
    Sort,
    Group,
}

fn field_with_reverse(pair: Pair<'_, Rule>, field_use: FieldUse) -> (String, bool) {
    let mut field = String::new();
    let mut user_reverse = false;
    for inner in pair.into_inner() {
        match inner.as_rule() {
            Rule::field_spec => field = normalize_sort_group_field(inner.as_str(), field_use),
            Rule::reverse => user_reverse = true,
            _ => {}
        }
    }
    let default_reverse = field == "urgency";
    let reverse = default_reverse ^ user_reverse;
    (field, reverse)
}

fn normalize_sort_group_field(value: &str, field_use: FieldUse) -> String {
    let value = value.trim();
    if let Some(index) = value.strip_prefix("tag ") {
        return format!("tags#{}", index.trim());
    }
    match value {
        "status" if matches!(field_use, FieldUse::Group) => "statusGroup".to_owned(),
        "status" => "done".to_owned(),
        "status.name" => "status.name".to_owned(),
        "status.type" if matches!(field_use, FieldUse::Sort) => "statusTypeGroupText".to_owned(),
        "status.type" => "status.type".to_owned(),
        "done" => "completion".to_owned(),
        "starts" => "start".to_owned(),
        "priority" if matches!(field_use, FieldUse::Group) => "priorityGroup".to_owned(),
        "recurring" if matches!(field_use, FieldUse::Group) => "recurringGroup".to_owned(),
        "recurring" => "isRecurring".to_owned(),
        "recurrence" if matches!(field_use, FieldUse::Group) => "recurrenceGroup".to_owned(),
        "tag" => "tags#1".to_owned(),
        "tags" => "tags".to_owned(),
        "root" => "file.root".to_owned(),
        "folder" => "file.folder".to_owned(),
        "filename" if matches!(field_use, FieldUse::Group) => {
            "file.filenameWithoutExtension".to_owned()
        }
        "filename" => "file.filename".to_owned(),
        "backlink" => "backlink".to_owned(),
        "random" => "random".to_owned(),
        "urgency" => "urgency".to_owned(),
        value => value.to_owned(),
    }
}

fn function_source_with_reverse(pair: Pair<'_, Rule>) -> (String, bool) {
    let mut reverse = false;
    let mut source = String::new();
    for inner in pair.into_inner() {
        match inner.as_rule() {
            Rule::function_reverse => reverse = true,
            Rule::js_source => source = inner.as_str().to_owned(),
            _ => {}
        }
    }
    (source, reverse)
}

fn last_text(pair: Pair<'_, Rule>) -> String {
    pair.into_inner()
        .last()
        .map(|inner| inner.as_str().to_owned())
        .unwrap_or_default()
}

fn first_int(pair: Pair<'_, Rule>) -> Option<String> {
    if pair.as_rule() == Rule::int {
        return Some(pair.as_str().to_owned());
    }
    pair.into_inner().find_map(first_int)
}

fn date_field_name(value: &str) -> &str {
    match value {
        "done" => "completion",
        "starts" => "start",
        value => value,
    }
}

fn text_field_name(value: &str) -> &str {
    match value {
        "tag" => "tags",
        "root" => "file.root",
        "folder" => "file.folder",
        "filename" => "file.filename",
        value => value,
    }
}

fn status_field_name(value: &str) -> &str {
    match value {
        "status" => "status.name",
        value => value,
    }
}

fn date_relation(value: &str) -> DateRelation {
    match value {
        "in or before" => DateRelation::InOrBefore,
        "in or after" => DateRelation::InOrAfter,
        "on or before" => DateRelation::OnOrBefore,
        "on or after" => DateRelation::OnOrAfter,
        "before" => DateRelation::Before,
        "after" => DateRelation::After,
        "in" => DateRelation::In,
        _ => DateRelation::On,
    }
}

impl TaskFilter {
    fn matches(&self, row: &Value, query: &Value, script: &dyn ScriptEngine) -> Result<bool> {
        Ok(match self {
            Self::Never => false,
            Self::Done(expected) => row["done"].as_bool() == Some(*expected),
            Self::Date {
                field,
                relation,
                query: date_query,
            } => {
                let dates = task_dates(row, field);
                if dates.is_empty() && start_filter_includes_undated(field, *relation) {
                    true
                } else {
                    dates
                        .into_iter()
                        .any(|actual| date_matches(actual, *relation, date_query))
                }
            }
            Self::InvalidDate(field) => task_date_strings(row, field).into_iter().any(|value| {
                !value.trim().is_empty()
                    && NaiveDate::parse_from_str(value.trim(), "%Y-%m-%d").is_err()
            }),
            Self::HasField { field, expected } => {
                let has_field = value_at_path(row, field).is_some_and(|value| match value {
                    Value::Bool(value) => *value,
                    value => has_meaningful_value(value),
                });
                has_field == *expected
            }
            Self::Text {
                field,
                includes,
                value,
            } => {
                let contains = match value_at_path(row, field).unwrap_or(&Value::Null) {
                    Value::String(actual) => actual.to_lowercase().contains(value),
                    Value::Array(actual) => actual.iter().any(|item| {
                        item.as_str()
                            .is_some_and(|item| item.to_lowercase().contains(value))
                    }),
                    _ => false,
                };
                contains == *includes
            }
            Self::Status {
                field,
                expected,
                equals,
            } => {
                let matched = value_at_path(row, field)
                    .and_then(Value::as_str)
                    .is_some_and(|actual| actual.eq_ignore_ascii_case(expected));
                matched == *equals
            }
            Self::Regex {
                field,
                matches,
                regex,
            } => {
                let matched = match value_at_path(row, field).unwrap_or(&Value::Null) {
                    Value::String(actual) => regex.is_match(actual),
                    Value::Array(actual) => actual
                        .iter()
                        .any(|item| item.as_str().is_some_and(|item| regex.is_match(item))),
                    _ => false,
                };
                matched == *matches
            }
            Self::Priority { relation, priority } => {
                let actual = row
                    .get("priorityNumber")
                    .and_then(Value::as_i64)
                    .unwrap_or(3);
                match relation {
                    PriorityRelation::Is => actual == *priority,
                    PriorityRelation::IsNot => actual != *priority,
                    PriorityRelation::Above => actual < *priority,
                    PriorityRelation::Below => actual > *priority,
                }
            }
            Self::Not(filter) => !filter.matches(row, query, script)?,
            Self::Any(filters) => filters.iter().try_fold(false, |matched, filter| {
                Ok::<bool, anyhow::Error>(matched || filter.matches(row, query, script)?)
            })?,
            Self::All(filters) => filters.iter().try_fold(true, |matched, filter| {
                Ok::<bool, anyhow::Error>(matched && filter.matches(row, query, script)?)
            })?,
            Self::Xor(left, right) => {
                left.matches(row, query, script)? ^ right.matches(row, query, script)?
            }
            Self::Script(source) => evaluate_task_script(script, source, row, query)
                .with_context(|| format!("Tasks function filter failed: {source}"))?
                .as_bool()
                .unwrap_or(false),
        })
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
        let (left_value, right_value, descending, field) = match self {
            Self::Field(field, descending) => (
                value_at_path(left, field).cloned().unwrap_or(Value::Null),
                value_at_path(right, field).cloned().unwrap_or(Value::Null),
                *descending,
                Some(field.as_str()),
            ),
            Self::Script(source, descending) => (
                evaluate_task_script(script, source, left, query).unwrap_or(Value::Null),
                evaluate_task_script(script, source, right, query).unwrap_or(Value::Null),
                *descending,
                None,
            ),
        };
        let ordering = field
            .filter(|field| is_date_sort_field(field))
            .map(|_| compare_task_date_values(&left_value, &right_value))
            .unwrap_or_else(|| total_value_order(&left_value, &right_value));
        if descending {
            ordering.reverse()
        } else {
            ordering
        }
    }
}

fn sort_tasks(
    tasks: &mut Vec<Value>,
    sorts: &[TaskSort],
    query: &Value,
    script: &dyn ScriptEngine,
) {
    for sort in sorts.iter().rev() {
        match sort {
            TaskSort::Field(_, _) => {
                tasks.sort_by(|left, right| sort.compare(left, right, query, script));
            }
            TaskSort::Script(source, descending) => {
                let mut keyed = std::mem::take(tasks)
                    .into_iter()
                    .map(|task| {
                        let key = evaluate_task_script(script, source, &task, query)
                            .unwrap_or(Value::Null);
                        (key, task)
                    })
                    .collect::<Vec<_>>();
                keyed.sort_by(|(left_key, _), (right_key, _)| {
                    let ordering = total_value_order(left_key, right_key);
                    if *descending {
                        ordering.reverse()
                    } else {
                        ordering
                    }
                });
                *tasks = keyed.into_iter().map(|(_, task)| task).collect();
            }
        }
    }
}

fn total_value_order(left: &Value, right: &Value) -> std::cmp::Ordering {
    value_order(left, right).unwrap_or_else(|| {
        value_rank(left)
            .cmp(&value_rank(right))
            .then_with(|| left.to_string().cmp(&right.to_string()))
    })
}

fn is_date_sort_field(field: &str) -> bool {
    matches!(
        field,
        "due" | "scheduled" | "start" | "completion" | "created" | "cancelled" | "happens"
    )
}

fn compare_task_date_values(left: &Value, right: &Value) -> std::cmp::Ordering {
    date_sort_key(left).cmp(&date_sort_key(right))
}

fn date_sort_key(value: &Value) -> (u8, String) {
    let Some(value) = value.as_str().filter(|value| !value.trim().is_empty()) else {
        return (2, String::new());
    };
    if NaiveDate::parse_from_str(value.trim(), "%Y-%m-%d").is_ok() {
        (1, value.to_owned())
    } else {
        (0, value.to_owned())
    }
}

fn value_rank(value: &Value) -> u8 {
    match value {
        Value::Null => 0,
        Value::Bool(_) => 1,
        Value::Number(_) => 2,
        Value::String(_) => 3,
        Value::Array(_) => 4,
        Value::Object(_) => 5,
    }
}

fn unique_strings(values: Vec<String>) -> Vec<String> {
    let mut seen = HashSet::new();
    values
        .into_iter()
        .filter(|value| seen.insert(value.clone()))
        .collect()
}

fn group_rows(
    tasks: Vec<Value>,
    groups: &[TaskGroup],
    group_limit: Option<usize>,
    query: &Value,
    script: &dyn ScriptEngine,
) -> Vec<Row> {
    let Some((group, rest)) = groups.split_first() else {
        return tasks
            .into_iter()
            .map(|value| value.as_object().unwrap().clone().into_iter().collect())
            .collect();
    };
    let mut groups = BTreeMap::<String, (Value, Vec<Value>)>::new();
    for task in tasks {
        let keys = match group {
            TaskGroup::Field(field, _) => group_values(&task, field),
            TaskGroup::Script(source) => group_values_from_value(
                evaluate_task_script(script, source, &task, query).unwrap_or(Value::Null),
            ),
        };
        for key in keys {
            let sort_key = match group {
                TaskGroup::Field(field, _) => group_sort_key(field, &key),
                TaskGroup::Script(_) => key.to_string(),
            };
            groups
                .entry(sort_key)
                .or_insert_with(|| (key, Vec::new()))
                .1
                .push(task.clone());
        }
    }
    let mut values: Vec<_> = groups
        .into_values()
        .map(|(key, mut tasks)| {
            let rows = if rest.is_empty() {
                if let Some(limit) = group_limit {
                    tasks.truncate(limit);
                }
                Value::Array(tasks)
            } else {
                Value::Array(
                    group_rows(tasks, rest, group_limit, query, script)
                        .into_iter()
                        .map(|row| Value::Object(row.into_iter().collect()))
                        .collect(),
                )
            };
            BTreeMap::from([("key".to_owned(), key), ("rows".to_owned(), rows)])
        })
        .collect();
    if matches!(group, TaskGroup::Field(_, true)) {
        values.reverse();
    }
    values
}

fn group_values(task: &Value, field: &str) -> Vec<Value> {
    if is_date_sort_field(field) {
        return vec![Value::String(date_group_label(
            field,
            value_at_path(task, field).unwrap_or(&Value::Null),
        ))];
    }
    group_values_from_value(value_at_path(task, field).cloned().unwrap_or(Value::Null))
}

fn date_group_label(field: &str, value: &Value) -> String {
    let display = match field {
        "completion" => "done",
        value => value,
    };
    let Some(value) = value.as_str().filter(|value| !value.trim().is_empty()) else {
        return format!("No {display} date");
    };
    if NaiveDate::parse_from_str(value.trim(), "%Y-%m-%d").is_ok() {
        value.to_owned()
    } else {
        format!("Invalid {display} date")
    }
}

fn group_values_from_value(value: Value) -> Vec<Value> {
    match value {
        Value::Array(values) if values.is_empty() => vec![Value::String("(No tags)".to_owned())],
        Value::Array(values) => values,
        Value::Null => vec![Value::String(String::new())],
        value => vec![value],
    }
}

fn group_sort_key(field: &str, value: &Value) -> String {
    match field {
        "status.type" => format!(
            "{:02}:{}",
            match value.as_str().unwrap_or_default() {
                "IN_PROGRESS" => 0,
                "TODO" => 1,
                "ON_HOLD" => 2,
                "DONE" => 3,
                "CANCELLED" => 4,
                "NON_TASK" => 5,
                _ => 99,
            },
            value
        ),
        "priorityGroup" => format!(
            "{:02}:{}",
            match value.as_str().unwrap_or_default() {
                "Highest priority" => 0,
                "High priority" => 1,
                "Medium priority" => 2,
                "Normal priority" => 3,
                "Low priority" => 4,
                "Lowest priority" => 5,
                _ => 99,
            },
            value
        ),
        "urgency" => format!(
            "{:020.5}:{}",
            value.as_f64().unwrap_or(f64::NEG_INFINITY) + 1_000_000_000.0,
            value
        ),
        "due" | "scheduled" | "start" | "completion" | "created" | "cancelled" | "happens" => {
            date_group_sort_key(field, value)
        }
        _ => value.to_string(),
    }
}

fn date_group_sort_key(field: &str, value: &Value) -> String {
    let value = value.as_str().unwrap_or_default();
    let no_date = date_group_label(field, &Value::Null);
    let invalid = format!(
        "Invalid {} date",
        if field == "completion" { "done" } else { field }
    );
    if value == invalid {
        format!("00:{value}")
    } else if value == no_date {
        format!("99:{value}")
    } else {
        format!("10:{value}")
    }
}

fn evaluate_task_script(
    script: &dyn ScriptEngine,
    source: &str,
    task: &Value,
    query: &Value,
) -> Result<Value> {
    let source = source.trim();
    let invocation = if looks_like_function_source(source) {
        format!("return ({source})(task);")
    } else if source.contains("return")
        || source.contains("const ")
        || source.contains("let ")
        || source.contains("var ")
    {
        source.to_owned()
    } else {
        format!("return {source};")
    };
    let runtime = r#"
            function pad(value) {
              return String(value).padStart(2, '0');
            }
            function parseDate(value) {
              if (typeof value !== 'string' || value.trim() === '') return null;
              const match = value.trim().match(/^(\d{4})-(\d{2})-(\d{2})$/);
              if (!match) return { raw: value, valid: false };
              const year = Number(match[1]);
              const month = Number(match[2]);
              const day = Number(match[3]);
              const date = new Date(Date.UTC(year, month - 1, day));
              const valid = date.getUTCFullYear() === year
                && date.getUTCMonth() === month - 1
                && date.getUTCDate() === day;
              return valid ? { raw: value, valid: true, date, year, month, day } : { raw: value, valid: false };
            }
            function dayOfYear(date) {
              return Math.floor((date - Date.UTC(date.getUTCFullYear(), 0, 1)) / 86400000) + 1;
            }
            function isoWeek(date) {
              const copy = new Date(Date.UTC(date.getUTCFullYear(), date.getUTCMonth(), date.getUTCDate()));
              const day = copy.getUTCDay() || 7;
              copy.setUTCDate(copy.getUTCDate() + 4 - day);
              const yearStart = new Date(Date.UTC(copy.getUTCFullYear(), 0, 1));
              return Math.ceil((((copy - yearStart) / 86400000) + 1) / 7);
            }
            function formatDate(parsed, pattern, fallback = '') {
              if (!parsed || !parsed.valid) return fallback;
              const date = parsed.date;
              const weekdays = ['Sunday', 'Monday', 'Tuesday', 'Wednesday', 'Thursday', 'Friday', 'Saturday'];
              const weekdaysShort = ['Sun', 'Mon', 'Tue', 'Wed', 'Thu', 'Fri', 'Sat'];
              const months = ['January', 'February', 'March', 'April', 'May', 'June', 'July', 'August', 'September', 'October', 'November', 'December'];
              const monthsShort = ['Jan', 'Feb', 'Mar', 'Apr', 'May', 'Jun', 'Jul', 'Aug', 'Sep', 'Oct', 'Nov', 'Dec'];
              let output = '';
              for (let i = 0; i < pattern.length;) {
                if (pattern[i] === '[') {
                  const end = pattern.indexOf(']', i + 1);
                  if (end !== -1) {
                    output += pattern.slice(i + 1, end);
                    i = end + 1;
                    continue;
                  }
                }
                const rest = pattern.slice(i);
                const token = ['YYYY', 'MMMM', 'MMM', 'dddd', 'ddd', 'DD', 'MM', 'WW', 'D', 'M']
                  .find(token => rest.startsWith(token));
                if (!token) {
                  output += pattern[i++];
                  continue;
                }
                output += ({
                  YYYY: String(parsed.year),
                  MMMM: months[parsed.month - 1],
                  MMM: monthsShort[parsed.month - 1],
                  dddd: weekdays[date.getUTCDay()],
                  ddd: weekdaysShort[date.getUTCDay()],
                  DD: pad(parsed.day),
                  D: String(parsed.day),
                  MM: pad(parsed.month),
                  M: String(parsed.month),
                  WW: pad(isoWeek(date))
                })[token];
                i += token.length;
              }
              return output;
            }
            function startOfToday() {
              const now = new Date();
              return new Date(Date.UTC(now.getFullYear(), now.getMonth(), now.getDate()));
            }
            function categoryFor(parsed) {
              if (!parsed) return { name: 'Undated', sortOrder: 4, groupText: '%%4%% Undated' };
              if (!parsed.valid) return { name: 'Invalid date', sortOrder: 0, groupText: '%%0%% Invalid date' };
              const today = startOfToday();
              const name = parsed.date < today ? 'Overdue' : parsed.date.getTime() === today.getTime() ? 'Today' : 'Future';
              const sortOrder = name === 'Overdue' ? 1 : name === 'Today' ? 2 : 3;
              return { name, sortOrder, groupText: `%%${sortOrder}%% ${name}` };
            }
            function fromNowFor(parsed) {
              if (!parsed || !parsed.valid) return { name: '', sortOrder: 0, groupText: '' };
              const days = Math.round((parsed.date - startOfToday()) / 86400000);
              const abs = Math.abs(days);
              const unit = abs === 1 ? 'day' : 'days';
              const name = days === 0 ? 'today' : days < 0 ? `${abs} ${unit} ago` : `in ${abs} ${unit}`;
              const sortOrder = Number(`${parsed.year}${pad(parsed.month)}${pad(parsed.day)}0000`);
              return { name, sortOrder, groupText: `%%${sortOrder}%% ${name}` };
            }
            function makeMoment(parsed) {
              if (!parsed) return null;
              return {
                __tasksDateParsed: parsed,
                isValid: () => parsed.valid,
                isSameOrBefore: (other, unit = 'millisecond') => parsed.valid && compareMoment(parsed, other, unit) <= 0,
                isSameOrAfter: (other, unit = 'millisecond') => parsed.valid && compareMoment(parsed, other, unit) >= 0,
                isSame: (other, unit = 'millisecond') => parsed.valid && compareMoment(parsed, other, unit) === 0,
                format: pattern => formatDate(parsed, pattern)
              };
            }
            function parseMomentInput(value) {
              if (value && value.__tasksDateParsed) return value.__tasksDateParsed;
              if (value && value.date instanceof Date) {
                const date = value.date;
                return { valid: true, date, year: date.getUTCFullYear(), month: date.getUTCMonth() + 1, day: date.getUTCDate() };
              }
              if (value instanceof Date) {
                return { valid: true, date: value, year: value.getUTCFullYear(), month: value.getUTCMonth() + 1, day: value.getUTCDate() };
              }
              return parseDate(String(value == null ? '' : value));
            }
            function compareMoment(left, right, unit) {
              const parsed = parseMomentInput(right);
              if (!left.valid || !parsed || !parsed.valid) return NaN;
              if (unit === 'day') return Date.UTC(left.year, left.month - 1, left.day) - Date.UTC(parsed.year, parsed.month - 1, parsed.day);
              if (unit === 'week') return (left.date.getUTCFullYear() * 100 + isoWeek(left.date)) - (parsed.date.getUTCFullYear() * 100 + isoWeek(parsed.date));
              return left.date - parsed.date;
            }
            function moment(value = undefined) {
              if (value === undefined) return makeMoment(parseDate(startOfToday().toISOString().slice(0, 10)));
              return makeMoment(parseMomentInput(value));
            }
            function makeTasksDate(value) {
              const parsed = parseDate(value);
              return {
                __tasksDateParsed: parsed,
                moment: makeMoment(parsed),
                formatAsDate: (fallback = '') => parsed && parsed.valid ? `${parsed.year}-${pad(parsed.month)}-${pad(parsed.day)}` : fallback,
                formatAsDateAndTime: (fallback = '') => parsed && parsed.valid ? `${parsed.year}-${pad(parsed.month)}-${pad(parsed.day)} 00:00` : fallback,
                format: (pattern, fallback = '') => formatDate(parsed, pattern, fallback),
                toISOString: (keepOffset = false) => parsed && parsed.valid
                  ? `${parsed.year}-${pad(parsed.month)}-${pad(parsed.day)}T00:00:00.000${keepOffset ? '+00:00' : 'Z'}`
                  : '',
                category: categoryFor(parsed),
                fromNow: fromNowFor(parsed),
                toString: () => parsed && parsed.valid ? `${parsed.year}-${pad(parsed.month)}-${pad(parsed.day)} 00:00` : ''
              };
            }
            function withTaskApi(raw) {
              const task = Object.assign({}, raw);
              for (const field of ['created', 'start', 'scheduled', 'due', 'cancelled', 'completion', 'happens']) {
                task[field === 'completion' ? 'done' : field] = makeTasksDate(raw[field]);
              }
              task.recurrenceRule = raw.recurrence == null ? '' : raw.recurrence;
              task.onCompletion = raw.onCompletion == null ? '' : raw.onCompletion;
              task.status = Object.assign({}, raw.status || {});
              task.status.nextSymbol = task.status.nextSymbol == null ? 'x' : task.status.nextSymbol;
              task.status.typeGroupText = raw.statusTypeGroupText == null ? (task.status.type || '') : raw.statusTypeGroupText;
              task.file = Object.assign({}, raw.file || {});
              task.file.hasProperty = name => raw.file && raw.file.frontmatter && raw.file.frontmatter[name] !== undefined && raw.file.frontmatter[name] !== null;
              task.file.property = name => raw[name] !== undefined ? raw[name] : raw.file && raw.file.frontmatter && raw.file.frontmatter[name] !== undefined ? raw.file.frontmatter[name] : null;
              task.isBlocked = () => !!raw.isBlocked;
              task.isBlocking = () => !!raw.isBlocking;
              task.findClosestParentTask = () => raw.parent ? withTaskApi(raw.parent) : null;
              return task;
            }
            const task = withTaskApi(__mdq.task);
            const query = __mdq.query;
            __MDQ_INVOCATION__
            "#
    .replace("__MDQ_INVOCATION__", &invocation);
    script.evaluate(&runtime, &json!({"task": task, "query": query}))
}

fn looks_like_function_source(source: &str) -> bool {
    if source.starts_with("function") {
        return true;
    }
    let Some(arrow) = source.find("=>") else {
        return false;
    };
    let params = source[..arrow].trim();
    if let Some(inner) = params
        .strip_prefix('(')
        .and_then(|value| value.strip_suffix(')'))
    {
        return inner.split(',').map(str::trim).all(is_js_identifier);
    }
    is_js_identifier(params)
}

fn is_js_identifier(value: &str) -> bool {
    let mut chars = value.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (first == '_' || first == '$' || first.is_ascii_alphabetic())
        && chars.all(|ch| ch == '_' || ch == '$' || ch.is_ascii_alphanumeric())
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

fn strip_tasks_comments(source: &str) -> String {
    static TASKS_COMMENT: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"\s*\{\{![\s\S]*?\}\}").unwrap());
    TASKS_COMMENT.replace_all(source, "").into_owned()
}

fn strip_blockquote_prefix(line: &str) -> &str {
    let mut rest = line.trim_start();
    let leading_spaces = line.len() - rest.len();
    if leading_spaces > 3 {
        return line;
    }
    let mut stripped = false;
    while let Some(after_marker) = rest.strip_prefix('>') {
        stripped = true;
        rest = after_marker.strip_prefix(' ').unwrap_or(after_marker);
    }
    if stripped { rest } else { line }
}

fn query_value(context: &QueryContext<'_>) -> Value {
    let path = context
        .current_file
        .as_ref()
        .and_then(|path| path.strip_prefix(context.vault).ok())
        .map(|path| path.to_string_lossy().replace('\\', "/"))
        .unwrap_or_default();
    let filename = path.rsplit('/').next().unwrap_or(&path);
    let path_without_extension = path.trim_end_matches(".md");
    let folder = path
        .rsplit_once('/')
        .map(|(folder, _)| format!("{folder}/"))
        .unwrap_or_else(|| "/".to_owned());
    let root = path
        .split_once('/')
        .map(|(root, _)| format!("{root}/"))
        .unwrap_or_else(|| "/".to_owned());
    json!({
        "file": {
            "path": path,
            "pathWithoutExtension": path_without_extension,
            "filename": filename,
            "filenameWithoutExtension": filename.trim_end_matches(".md"),
            "folder": folder,
            "root": root
        }
    })
}

fn expand_query_placeholders(source: &str, query: &Value) -> String {
    [
        ("{{query.file.path}}", query["file"]["path"].as_str()),
        (
            "{{query.file.pathWithoutExtension}}",
            query["file"]["pathWithoutExtension"].as_str(),
        ),
        (
            "{{query.file.filename}}",
            query["file"]["filename"].as_str(),
        ),
        (
            "{{query.file.filenameWithoutExtension}}",
            query["file"]["filenameWithoutExtension"].as_str(),
        ),
        ("{{query.file.folder}}", query["file"]["folder"].as_str()),
        ("{{query.file.root}}", query["file"]["root"].as_str()),
    ]
    .into_iter()
    .fold(source.to_owned(), |source, (placeholder, value)| {
        source.replace(placeholder, value.unwrap_or_default())
    })
}

fn value_at_path<'a>(value: &'a Value, path: &str) -> Option<&'a Value> {
    if let Some((field, index)) = path.split_once('#') {
        let index = index.parse::<usize>().ok()?.checked_sub(1)?;
        return value_at_path(value, field)
            .and_then(Value::as_array)
            .and_then(|items| items.get(index));
    }
    let mut current = value;
    for part in path.split('.') {
        current = current.get(part)?;
    }
    Some(current)
}

fn resolve_date_query(value: &str) -> Option<DateQuery> {
    let value = value.trim();
    if let Some((start, end)) = parse_period(value) {
        return Some(DateQuery::Range { start, end });
    }
    if let Some(query) = parse_absolute_date_range(value) {
        return Some(query);
    }
    if let Some(date) = parse_date_boundary(value) {
        return Some(DateQuery::Single(date));
    }
    None
}

fn parse_absolute_date_range(value: &str) -> Option<DateQuery> {
    let mut parts = value.split_whitespace();
    let left = parts.next()?;
    let right = parts.next()?;
    if parts.next().is_some() {
        return None;
    }
    if !(looks_like_iso_date(left) && looks_like_iso_date(right)) {
        return None;
    }
    match (
        NaiveDate::parse_from_str(left, "%Y-%m-%d").ok(),
        NaiveDate::parse_from_str(right, "%Y-%m-%d").ok(),
    ) {
        (Some(start), Some(end)) => Some(DateQuery::Range { start, end }),
        (Some(date), None) | (None, Some(date)) => Some(DateQuery::Single(date)),
        (None, None) => None,
    }
}

fn looks_like_iso_date(value: &str) -> bool {
    value.len() == 10
        && value.chars().enumerate().all(|(index, c)| {
            if index == 4 || index == 7 {
                c == '-'
            } else {
                c.is_ascii_digit()
            }
        })
}

fn parse_date_boundary(value: &str) -> Option<NaiveDate> {
    match value.trim() {
        "today" => Some(Local::now().date_naive()),
        "tomorrow" => Some(Local::now().date_naive().succ_opt()?),
        "yesterday" => Some(Local::now().date_naive().pred_opt()?),
        value => NaiveDate::parse_from_str(value, "%Y-%m-%d")
            .ok()
            .or_else(|| {
                let normalized = ORDINAL_DATE_SUFFIX.replace_all(value, "$1");
                parse_date_string(&normalized, Local::now(), Dialect::Uk)
                    .ok()
                    .map(|date| date.date_naive())
            }),
    }
}

fn parse_period(value: &str) -> Option<(NaiveDate, NaiveDate)> {
    let value = value.trim();
    if let Some((offset, unit)) = relative_period(value) {
        let today = Local::now().date_naive();
        return Some(match unit {
            "week" => {
                let start = today - Duration::days(today.weekday().num_days_from_monday() as i64)
                    + Duration::weeks(offset);
                (start, start + Duration::days(6))
            }
            "month" => {
                let shifted = shift_month(today.year(), today.month(), offset)?;
                let start = NaiveDate::from_ymd_opt(shifted.0, shifted.1, 1)?;
                (start, month_end(start)?)
            }
            "quarter" => {
                let current_quarter = ((today.month() - 1) / 3) + 1;
                let month_index = today.year() * 12 + ((current_quarter - 1) * 3) as i32;
                let shifted = month_index + (offset as i32 * 3);
                let year = shifted.div_euclid(12);
                let month = shifted.rem_euclid(12) as u32 + 1;
                let start = NaiveDate::from_ymd_opt(year, month, 1)?;
                let next = shift_month(start.year(), start.month(), 3)?;
                let next = NaiveDate::from_ymd_opt(next.0, next.1, 1)?;
                (start, next.pred_opt()?)
            }
            "year" => {
                let year = today.year() + offset as i32;
                (
                    NaiveDate::from_ymd_opt(year, 1, 1)?,
                    NaiveDate::from_ymd_opt(year, 12, 31)?,
                )
            }
            _ => return None,
        });
    }
    if let Some((year, week)) = value.split_once("-W") {
        let year: i32 = year.parse().ok()?;
        let week: u32 = week.parse().ok()?;
        let start = NaiveDate::from_isoywd_opt(year, week, Weekday::Mon)?;
        return Some((start, start + Duration::days(6)));
    }
    if let Some((year, quarter)) = value.split_once("-Q") {
        let year: i32 = year.parse().ok()?;
        let quarter: u32 = quarter.parse().ok()?;
        let month = (quarter.checked_sub(1)? * 3) + 1;
        let start = NaiveDate::from_ymd_opt(year, month, 1)?;
        let next = if quarter == 4 {
            NaiveDate::from_ymd_opt(year + 1, 1, 1)?
        } else {
            NaiveDate::from_ymd_opt(year, month + 3, 1)?
        };
        return Some((start, next.pred_opt()?));
    }
    if let Ok(date) = NaiveDate::parse_from_str(&format!("{value}-01"), "%Y-%m-%d") {
        let next = if date.month() == 12 {
            NaiveDate::from_ymd_opt(date.year() + 1, 1, 1)?
        } else {
            NaiveDate::from_ymd_opt(date.year(), date.month() + 1, 1)?
        };
        return Some((date, next.pred_opt()?));
    }
    if value.len() == 4 && value.chars().all(|c| c.is_ascii_digit()) {
        let year: i32 = value.parse().ok()?;
        return Some((
            NaiveDate::from_ymd_opt(year, 1, 1)?,
            NaiveDate::from_ymd_opt(year, 12, 31)?,
        ));
    }
    None
}

fn relative_period(value: &str) -> Option<(i64, &str)> {
    let (offset, unit) = value.split_once(' ')?;
    let offset = match offset {
        "last" => -1,
        "this" => 0,
        "next" => 1,
        _ => return None,
    };
    match unit {
        "week" | "month" | "quarter" | "year" => Some((offset, unit)),
        _ => None,
    }
}

fn shift_month(year: i32, month: u32, offset: i64) -> Option<(i32, u32)> {
    let index = year as i64 * 12 + month as i64 - 1 + offset;
    Some((index.div_euclid(12) as i32, index.rem_euclid(12) as u32 + 1))
}

fn month_end(start: NaiveDate) -> Option<NaiveDate> {
    let next = shift_month(start.year(), start.month(), 1)?;
    NaiveDate::from_ymd_opt(next.0, next.1, 1)?.pred_opt()
}

fn task_date_strings<'a>(row: &'a Value, field: &str) -> Vec<&'a str> {
    if field == "happens" {
        ["start", "scheduled", "due"]
            .into_iter()
            .filter_map(|field| row.get(field).and_then(Value::as_str))
            .collect()
    } else {
        value_at_path(row, field)
            .and_then(Value::as_str)
            .into_iter()
            .collect()
    }
}

fn task_dates(row: &Value, field: &str) -> Vec<NaiveDate> {
    task_date_strings(row, field)
        .into_iter()
        .filter_map(|value| NaiveDate::parse_from_str(value.trim(), "%Y-%m-%d").ok())
        .collect()
}

fn start_filter_includes_undated(field: &str, relation: DateRelation) -> bool {
    field == "start"
        && matches!(
            relation,
            DateRelation::Before | DateRelation::OnOrBefore | DateRelation::InOrBefore
        )
}

fn date_matches(actual: NaiveDate, relation: DateRelation, query: &DateQuery) -> bool {
    let (start, end) = match query {
        DateQuery::Single(date) => (*date, *date),
        DateQuery::Range { start, end } => (*start, *end),
    };
    match relation {
        DateRelation::On | DateRelation::In => actual >= start && actual <= end,
        DateRelation::Before => actual < start,
        DateRelation::After => actual > end,
        DateRelation::OnOrBefore | DateRelation::InOrBefore => actual <= end,
        DateRelation::OnOrAfter | DateRelation::InOrAfter => actual >= start,
    }
}

fn parse_js_regex(source: &str) -> Option<Regex> {
    let source = source.trim();
    let rest = source.strip_prefix('/')?;
    let slash = rest.rfind('/')?;
    let pattern = &rest[..slash];
    let flags = &rest[slash + 1..];
    if flags.chars().any(|flag| !matches!(flag, 'i' | 'm')) {
        return None;
    }
    let mut builder = RegexBuilder::new(pattern);
    builder.case_insensitive(flags.contains('i'));
    builder.multi_line(flags.contains('m'));
    builder.build().ok()
}

fn priority_number(value: &str) -> Option<i64> {
    match value.trim() {
        "highest" => Some(0),
        "high" => Some(1),
        "medium" | "normal" => Some(2),
        "none" => Some(3),
        "low" => Some(4),
        "lowest" => Some(5),
        _ => None,
    }
}

fn enrich_dependency_state(tasks: &mut [Value]) {
    let mut ids = HashSet::new();
    let mut blockers = HashSet::new();
    for task in tasks.iter() {
        if is_actionable(task) {
            if let Some(id) = task.get("id").and_then(Value::as_str) {
                ids.insert(id.to_owned());
            }
        }
    }
    for task in tasks.iter() {
        if !is_actionable(task) {
            continue;
        }
        if let Some(depends_on) = task.get("depends_on").and_then(Value::as_array) {
            for dependency in depends_on.iter().filter_map(Value::as_str) {
                if ids.contains(dependency) {
                    blockers.insert(dependency.to_owned());
                }
            }
        }
    }
    for task in tasks.iter_mut() {
        let is_blocked = is_actionable(task)
            && task
                .get("depends_on")
                .and_then(Value::as_array)
                .is_some_and(|depends_on| {
                    depends_on
                        .iter()
                        .filter_map(Value::as_str)
                        .any(|dependency| ids.contains(dependency))
                });
        let is_blocking = is_actionable(task)
            && task
                .get("id")
                .and_then(Value::as_str)
                .is_some_and(|id| blockers.contains(id));
        if let Some(object) = task.as_object_mut() {
            object.insert("isBlocked".to_owned(), Value::Bool(is_blocked));
            object.insert("isBlocking".to_owned(), Value::Bool(is_blocking));
        }
    }
}

fn is_actionable(task: &Value) -> bool {
    task.get("status")
        .and_then(|status| status.get("type"))
        .and_then(Value::as_str)
        .is_some_and(|status_type| matches!(status_type, "TODO" | "IN_PROGRESS" | "ON_HOLD"))
}

fn random_sort_key(row: &Map<String, Value>) -> i64 {
    let today = Local::now().date_naive();
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    today.hash(&mut hasher);
    row.get("description")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .hash(&mut hasher);
    row.get("path")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .hash(&mut hasher);
    row.get("lineNumber")
        .and_then(Value::as_i64)
        .unwrap_or_default()
        .hash(&mut hasher);
    (hasher.finish() & i64::MAX as u64) as i64
}

fn urgency_score(row: &Map<String, Value>) -> f64 {
    due_urgency(row.get("due").and_then(Value::as_str))
        + priority_urgency(row.get("priorityNumber").and_then(Value::as_i64))
        + scheduled_urgency(row.get("scheduled").and_then(Value::as_str))
        + start_urgency(row.get("start").and_then(Value::as_str))
}

fn due_urgency(value: Option<&str>) -> f64 {
    let Some(date) = value.and_then(parse_task_date) else {
        return 0.0;
    };
    let days = (date - Local::now().date_naive()).num_days();
    if days <= -7 {
        12.0
    } else if days >= 14 {
        2.4
    } else {
        8.8 - (days as f64 * 0.457_142_857)
    }
}

fn priority_urgency(value: Option<i64>) -> f64 {
    match value.unwrap_or(3) {
        0 => 9.0,
        1 => 6.0,
        2 => 3.9,
        3 => 1.95,
        4 => 0.0,
        5 => -1.8,
        _ => 1.95,
    }
}

fn scheduled_urgency(value: Option<&str>) -> f64 {
    let Some(date) = value.and_then(parse_task_date) else {
        return 0.0;
    };
    if date <= Local::now().date_naive() {
        5.0
    } else {
        0.0
    }
}

fn start_urgency(value: Option<&str>) -> f64 {
    let Some(date) = value.and_then(parse_task_date) else {
        return 0.0;
    };
    if date > Local::now().date_naive() {
        -3.0
    } else {
        0.0
    }
}

fn parse_task_date(value: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(value.trim(), "%Y-%m-%d").ok()
}

fn has_meaningful_value(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::String(value) => !value.trim().is_empty(),
        Value::Array(value) => !value.is_empty(),
        _ => true,
    }
}

fn frontmatter_link_values(frontmatter: &Value) -> Vec<Value> {
    fn collect_strings(value: &Value, links: &mut Vec<Value>) {
        match value {
            Value::String(source) => links.extend(link_values(extract_links(source))),
            Value::Array(values) => {
                for value in values {
                    collect_strings(value, links);
                }
            }
            Value::Object(values) => {
                for value in values.values() {
                    collect_strings(value, links);
                }
            }
            _ => {}
        }
    }

    let mut links = Vec::new();
    collect_strings(frontmatter, &mut links);
    links
}

fn link_values(links: Vec<ParsedLink>) -> Vec<Value> {
    links.into_iter().map(link_value).collect()
}

fn link_value(link: ParsedLink) -> Value {
    let destination_path = link_destination_path(&link);
    json!({
        "__kind": "link",
        "path": destination_path,
        "display": link.raw_target,
        "destinationPath": destination_path,
        "rawTarget": link.raw_target,
        "heading": link.heading,
        "embed": link.is_embed
    })
}

fn link_destination_path(link: &ParsedLink) -> String {
    let raw = link
        .raw_target
        .split('|')
        .next()
        .unwrap_or(&link.raw_target)
        .split('#')
        .next()
        .unwrap_or(&link.raw_target)
        .trim()
        .trim_matches('"');
    let raw = raw
        .replace('\\', "/")
        .trim_start_matches("./")
        .trim_matches('/')
        .to_owned();
    if raw.is_empty() {
        return String::new();
    }
    if std::path::Path::new(&raw).extension().is_some() {
        raw
    } else {
        format!("{raw}.md")
    }
}

fn merge_link_values(first: &[Value], second: &[Value]) -> Vec<Value> {
    let mut seen = HashSet::new();
    first
        .iter()
        .chain(second)
        .filter_map(|link| {
            let key = (
                link.get("destinationPath")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
                link.get("heading")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
                link.get("embed").and_then(Value::as_bool).unwrap_or(false),
            );
            if seen.insert(key) {
                Some(link.clone())
            } else {
                None
            }
        })
        .collect()
}

fn task_row(
    path: &str,
    line: usize,
    status: &str,
    source: &str,
    global_filter: Option<&str>,
    frontmatter: &Value,
    heading: Option<&str>,
    indentation: &str,
    list_marker: &str,
    outlinks_in_body: &[Value],
    outlinks_in_properties: &[Value],
    file_outlinks: &[Value],
    parent: Option<&Value>,
    status_overrides: &[TaskStatusOverride],
) -> Value {
    let mut fields = Map::new();
    let mut description = source.to_owned();
    if let Some(filter) = global_filter {
        description = description.replace(filter, "");
    }
    for captures in INLINE_FIELD.captures_iter(source) {
        let key = captures[1].trim().to_ascii_lowercase();
        let value = captures[2].trim();
        fields.insert(key, Value::String(value.to_owned()));
        description = description.replace(&captures[0], "");
    }
    for captures in TASK_EMOJI_FIELD.captures_iter(source) {
        parse_tasks_emoji_field(&captures, &mut fields);
        description = description.replace(&captures[0], "");
    }
    normalize_priority_fields(&mut fields);
    let tags: Vec<Value> = TAG
        .captures_iter(&description)
        .filter_map(|capture| {
            let tag = capture.get(2)?.as_str().trim_matches('/');
            if tag.is_empty() {
                None
            } else {
                Some(Value::String(format!("#{tag}")))
            }
        })
        .collect();
    let task_status = TaskStatus::for_symbol(status, status_overrides);
    let done = task_status.is_done();
    fields.insert("path".to_owned(), Value::String(path.to_owned()));
    let filename = path.rsplit('/').next().unwrap_or(path);
    let folder = path
        .rsplit_once('/')
        .map(|(folder, _)| format!("{folder}/"))
        .unwrap_or_else(|| "/".to_owned());
    let root = path
        .split_once('/')
        .map(|(root, _)| format!("{root}/"))
        .unwrap_or_else(|| "/".to_owned());
    fields.insert(
        "file".to_owned(),
        json!({
            "path": path,
            "pathWithoutExtension": path.trim_end_matches(".md"),
            "filename": filename,
            "filenameWithoutExtension": filename.trim_end_matches(".md"),
            "folder": folder,
            "root": root,
            "frontmatter": frontmatter,
            "outlinksInProperties": outlinks_in_properties,
            "outlinksInBody": outlinks_in_body,
            "outlinks": file_outlinks
        }),
    );
    fields.insert("line".to_owned(), Value::Number(line.into()));
    fields.insert("lineNumber".to_owned(), Value::Number((line - 1).into()));
    fields.insert(
        "originalMarkdown".to_owned(),
        Value::String(format!("{indentation}{list_marker} [{status}] {source}")),
    );
    fields.insert(
        "listMarker".to_owned(),
        Value::String(list_marker.to_owned()),
    );
    fields.insert(
        "indentation".to_owned(),
        Value::String(indentation.to_owned()),
    );
    fields.insert("isSubItem".to_owned(), Value::Bool(!indentation.is_empty()));
    fields.insert("parent".to_owned(), parent.cloned().unwrap_or(Value::Null));
    let status_type_group_text = task_status.group_text();
    fields.insert(
        "status".to_owned(),
        json!({
            "name": task_status.name,
            "type": task_status.status_type,
            "symbol": status,
            "nextSymbol": task_status.next_symbol,
            "typeGroupText": status_type_group_text
        }),
    );
    fields.insert(
        "status_name".to_owned(),
        Value::String(task_status.name.to_owned()),
    );
    fields.insert(
        "status_type".to_owned(),
        Value::String(task_status.status_type.to_owned()),
    );
    fields.insert("status_symbol".to_owned(), Value::String(status.to_owned()));
    fields.insert(
        "statusTypeGroupText".to_owned(),
        Value::String(status_type_group_text.to_owned()),
    );
    fields.insert("done".to_owned(), Value::Bool(done));
    fields.insert("completed".to_owned(), Value::Bool(done));
    fields.insert("isDone".to_owned(), Value::Bool(done));
    fields.insert(
        "statusGroup".to_owned(),
        Value::String(if done { "Done" } else { "Todo" }.to_owned()),
    );
    let backlink = if let Some(heading) = heading {
        format!("{} > {heading}", filename.trim_end_matches(".md"))
    } else {
        filename.trim_end_matches(".md").to_owned()
    };
    fields.insert("backlink".to_owned(), Value::String(backlink));
    fields.insert(
        "description".to_owned(),
        Value::String(description.trim().to_owned()),
    );
    let description_without_tags = TAG
        .replace_all(description.trim(), |captures: &Captures<'_>| {
            captures
                .get(1)
                .map(|matched| matched.as_str())
                .unwrap_or_default()
                .to_owned()
        })
        .trim()
        .to_owned();
    fields.insert(
        "descriptionWithoutTags".to_owned(),
        Value::String(description_without_tags),
    );
    fields.insert("text".to_owned(), Value::String(source.to_owned()));
    fields.insert("heading".to_owned(), json!(heading));
    fields.insert("tags".to_owned(), Value::Array(tags));
    fields.insert(
        "outlinks".to_owned(),
        Value::Array(link_values(extract_links(source))),
    );
    let happens = ["start", "scheduled", "due"]
        .into_iter()
        .filter_map(|field| fields.get(field).and_then(Value::as_str))
        .min()
        .map(|value| Value::String(value.to_owned()))
        .unwrap_or(Value::Null);
    fields.insert("happens".to_owned(), happens);
    let is_recurring = fields.get("recurrence").is_some_and(has_meaningful_value);
    fields.insert("isRecurring".to_owned(), Value::Bool(is_recurring));
    let recurrence_rule = fields
        .get("recurrence")
        .cloned()
        .unwrap_or_else(|| Value::String(String::new()));
    fields
        .entry("recurrenceRule".to_owned())
        .or_insert(recurrence_rule);
    fields
        .entry("onCompletion".to_owned())
        .or_insert_with(|| Value::String(String::new()));
    fields.insert(
        "recurringGroup".to_owned(),
        Value::String(
            if is_recurring {
                "Recurring"
            } else {
                "Not Recurring"
            }
            .to_owned(),
        ),
    );
    fields.insert(
        "recurrenceGroup".to_owned(),
        fields
            .get("recurrence")
            .filter(|value| has_meaningful_value(value))
            .cloned()
            .unwrap_or_else(|| Value::String("None".to_owned())),
    );
    normalize_priority_fields(&mut fields);
    let priority_group = match fields
        .get("priorityNumber")
        .and_then(Value::as_i64)
        .unwrap_or(3)
    {
        0 => "Highest priority",
        1 => "High priority",
        2 => "Medium priority",
        3 => "Normal priority",
        4 => "Low priority",
        5 => "Lowest priority",
        _ => "Normal priority",
    };
    fields.insert(
        "priorityGroup".to_owned(),
        Value::String(priority_group.to_owned()),
    );
    fields.insert(
        "priorityNameGroupText".to_owned(),
        Value::String(format!(
            "%%{}%%{}",
            fields
                .get("priorityNumber")
                .and_then(Value::as_i64)
                .unwrap_or(3),
            priority_group
        )),
    );
    fields.insert("urgency".to_owned(), json!(urgency_score(&fields)));
    fields.insert(
        "random".to_owned(),
        Value::Number(random_sort_key(&fields).into()),
    );
    Value::Object(fields)
}

fn normalize_priority_fields(fields: &mut Map<String, Value>) {
    let priority_name = fields
        .get("priorityName")
        .or_else(|| fields.get("priority"))
        .and_then(Value::as_str)
        .and_then(priority_name_and_number);
    let (name, number) = priority_name.unwrap_or(("normal", 3));
    fields.insert("priority".to_owned(), Value::String(name.to_owned()));
    fields.insert("priorityName".to_owned(), Value::String(name.to_owned()));
    fields.insert("priorityNumber".to_owned(), Value::Number(number.into()));
}

fn priority_name_and_number(value: &str) -> Option<(&'static str, i64)> {
    match value.trim().to_ascii_lowercase().as_str() {
        "highest" => Some(("highest", 0)),
        "high" => Some(("high", 1)),
        "medium" => Some(("medium", 2)),
        "normal" => Some(("normal", 3)),
        "none" => Some(("none", 3)),
        "low" => Some(("low", 4)),
        "lowest" => Some(("lowest", 5)),
        _ => None,
    }
}

fn parse_tasks_emoji_field(captures: &Captures<'_>, fields: &mut Map<String, Value>) {
    let matched = captures.get(0).map(|m| m.as_str()).unwrap_or_default();
    if let Some(date) = captures.get(1).map(|m| m.as_str().to_owned()) {
        let key = if matched.starts_with("📅") {
            "due"
        } else if matched.starts_with("✅") {
            "completion"
        } else if matched.starts_with("⏳") {
            "scheduled"
        } else if matched.starts_with("🛫") {
            "start"
        } else if matched.starts_with("➕") {
            "created"
        } else {
            "cancelled"
        };
        fields.insert(key.to_owned(), Value::String(date));
    } else if let Some(recurrence) = captures.get(2).map(|m| m.as_str().trim().to_owned()) {
        fields.insert("recurrence".to_owned(), Value::String(recurrence));
    } else if let Some(on_completion) = captures.get(3).map(|m| m.as_str().trim().to_owned()) {
        fields.insert("onCompletion".to_owned(), Value::String(on_completion));
    } else if let Some(id) = captures.get(4).map(|m| m.as_str().trim().to_owned()) {
        fields.insert("id".to_owned(), Value::String(id));
    } else if let Some(depends_on) = captures.get(5).map(|m| m.as_str().trim().to_owned()) {
        let values = depends_on
            .split(',')
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(|value| Value::String(value.to_owned()))
            .collect();
        let values = Value::Array(values);
        fields.insert("depends_on".to_owned(), values.clone());
        fields.insert("dependsOn".to_owned(), values);
    } else if let Some(priority) = match matched.trim() {
        "🔺" => Some(("highest", 0)),
        "⏫" => Some(("high", 1)),
        "🔼" => Some(("medium", 2)),
        "🔽" => Some(("low", 4)),
        "⏬" => Some(("lowest", 5)),
        _ => None,
    } {
        fields.insert("priority".to_owned(), Value::String(priority.0.to_owned()));
        fields.insert(
            "priorityName".to_owned(),
            Value::String(priority.0.to_owned()),
        );
        fields.insert(
            "priorityNumber".to_owned(),
            Value::Number(priority.1.into()),
        );
    }
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
            None,
            &json!({}),
            Some("Tasks"),
            "",
            "-",
            &[],
            &[],
            &[],
            None,
            &[],
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
        assert_eq!(query.sorts.len(), 6);
    }

    #[test]
    fn parses_and_filters_and_grouping() {
        let query = TaskQuery::parse(
            "(tags include important) AND (tags do not include urgent)\ngroup by status.type",
        )
        .unwrap();
        assert_eq!(query.filters.len(), 1);
        assert!(matches!(query.filters[0], TaskFilter::All(_)));
        assert!(matches!(
            query.groups.as_slice(),
            [TaskGroup::Field(_, false)]
        ));
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
