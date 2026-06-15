use anyhow::{Result, bail};

pub const TOPICS: &[&str] = &[
    "overview",
    "native",
    "pipeline",
    "tasks",
    "base",
    "dataview",
    "dataviewjs",
    "extensions",
];

pub fn render(topic: Option<&str>) -> Result<String> {
    match topic {
        None | Some("all") => Ok([
            OVERVIEW, NATIVE, PIPELINE, TASKS, BASE, DATAVIEW, DATAVIEWJS, EXTENSIONS,
        ]
        .join("\n\n")),
        Some("overview") => Ok(OVERVIEW.to_owned()),
        Some("native") => Ok(NATIVE.to_owned()),
        Some("pipeline") => Ok(PIPELINE.to_owned()),
        Some("tasks") => Ok(TASKS.to_owned()),
        Some("base") => Ok(BASE.to_owned()),
        Some("dataview") | Some("dql") => Ok(DATAVIEW.to_owned()),
        Some("dataviewjs") | Some("dvjs") => Ok(DATAVIEWJS.to_owned()),
        Some("extensions") | Some("api") => Ok(EXTENSIONS.to_owned()),
        Some(topic) => bail!(
            "unknown manual topic: {topic}\navailable topics: {}",
            TOPICS.join(", ")
        ),
    }
}

const OVERVIEW: &str = r#"# mdq manual

mdq is an application-independent Markdown query and retrieval CLI. It does
not require Obsidian and does not assign meaning to vault-specific frontmatter
property names.

Query languages:
  native       generic frontmatter predicate language
  tasks        Obsidian Tasks-compatible task query subset
  base         Obsidian Base-compatible YAML query
  dataview     Dataview DQL-compatible page/task query subset
  dataviewjs   read-only DataviewJS-compatible runtime

Commands:
  mdq --vault PATH query --language LANGUAGE 'SOURCE'
  mdq --vault PATH query --language base --file view.base
  mdq --vault PATH query --language dataviewjs --file query.js
  mdq manual [TOPIC]

Use `mdq manual TOPIC` for a focused reference. Use `mdq manual all` for the
complete manual."#;

const NATIVE: &str = r#"# Native frontmatter query

Native queries operate on arbitrary YAML frontmatter. Dotted paths and array
indices are structural only; no property name is reserved.

Boolean syntax:
  EXPR and EXPR
  EXPR or EXPR
  not EXPR
  (EXPR)

Operators:
  =  ==  !=  >  >=  <  <=
  contains        string substring, array element, or object key
  contains_all    every expected array element is present
  overlaps        at least one expected array element is present
  in              value occurs in expected array
  starts_with     string prefix
  ends_with       string suffix
  matches         regular expression
  exists          path exists
  missing         path does not exist

Values use YAML syntax: strings, numbers, booleans, null, flow lists, and flow
objects. Numbers compare numerically. ISO dates and RFC 3339 timestamps compare
chronologically.

Examples:
  mdq query 'status = active and score >= 3'
  mdq query 'arbitrary.items contains value'
  mdq query 'date >= 2026-01-01'
  mdq query 'nested.key exists'

This language is also available as `filter:` in retrieval pipelines."#;

const PIPELINE: &str = r#"# Retrieval pipeline

Pipeline stages execute exactly in the order supplied and may be repeated:
  filter[@native]:EXPRESSION
  bm25:QUERY
  rag:QUERY
  bm25+rag:QUERY

Example:
  mdq pipeline \
    --stage 'filter:created >= 2026-01-01' \
    --stage 'bm25:cryptography' \
    --stage 'rag:public key research'

`search`, native `query`, `semantic`, `context`, and `rag` are convenience
commands over this pipeline. Compatibility queries return structured
RecordSet output and use the common QueryAdapter API described under
`mdq manual extensions`."#;

const TASKS: &str = r#"# Tasks-compatible query

Input:
  mdq query --language tasks 'not done
  tags include #task
  sort by due
  limit 20'

Task extraction:
  - Reads Markdown checkbox list items: `- [ ]`, `- [x]`, `- [/]`, `- [-]`.
  - Extracts every `[name:: value]` inline field generically.
  - Extracts hashtags and source path/line.
  - Built-in normalized fields are path, line, file, status, status_name,
    status_type, status_symbol, done, completed, description, text, and tags.

Filters:
  done
  not done
  due|scheduled|starts|start|done|created
      on|before|after|on or before|on or after DATE
  tags include TEXT
  tags do not include TEXT
  path includes TEXT
  path does not include TEXT
  description includes TEXT
  description does not include TEXT
  status is NAME
  (FILTER) OR (FILTER)
  (FILTER) AND (FILTER)
  filter by function JAVASCRIPT_BODY_OR_ARROW_FUNCTION

Dates:
  YYYY-MM-DD, today, tomorrow, yesterday

Ordering and limits:
  sort by FIELD
  sort by FIELD reverse
  sort by function JAVASCRIPT_BODY_OR_ARROW_FUNCTION
  group by FIELD
  group by function JAVASCRIPT_BODY_OR_ARROW_FUNCTION
  limit NUMBER

Accepted display-only directives:
  hide task count, hide backlink, hide edit button, hide postpone button,
  short mode

JavaScript functions run in the restricted runtime described under
`mdq manual dataviewjs`. Unsupported instructions are reported as diagnostics
instead of silently changing task semantics. A trailing `\` joins the next
line, matching Tasks multiline function-query style. Function bodies receive
`task`, `query`, and `task.file.property(name)`."#;

const BASE: &str = r#"# Base-compatible query

Input:
  mdq query --language base --file path/to/view.base
  mdq query --language base --file view.base --current Daily/2026-06-14.md

Supported document fields:
  filters
  formulas
  views[0].filters
  views[0].order
  views[0].sort

Filter forms:
  filters:
    and: [EXPR, ...]
  filters:
    or: [EXPR, ...]
  filters:
    not: EXPR

Expressions:
  and, or, not
  =, ==, !=, >, >=, <, <=
  + and -
  field paths such as `file.path`, `formula.name`, and arbitrary frontmatter
  string, number, boolean, null, and list literals

Functions and methods:
  date(value), today(), if(condition, yes, no), length(value)
  contains(a, b), icontains(a, b), startswith(a, b), endswith(a, b)
  value.contains(x), value.startsWith(x), value.endsWith(x)
  file.inFolder(folder), file.hasProperty(name), file.hasTag(tag)
  value.date(), value.format(pattern), value.asLink()

`this.file` is available when `--current` is supplied. Formulas are evaluated
iteratively so later formulas may reference `formula.<name>`.

Compatibility limits:
  - The first view is executed.
  - Rendering configuration and column sizes are ignored.
  - Date arithmetic currently supports quoted day durations such as `'1d'`.
  - Link values are represented as structured/string values, not Obsidian UI
    objects."#;

const DATAVIEW: &str = r#"# Dataview DQL-compatible query

Input:
  mdq query --language dataview \
    'TABLE title AS Name, created FROM "Daily" WHERE created >= date(2026-01-01) SORT created DESC LIMIT 10'

Query forms:
  TABLE FIELD [AS NAME], ...
  LIST [FIELD]
  TASK
  CALENDAR [FIELD]

Clauses may be on one line or separate lines:
  FROM ""
  FROM "folder"
  FROM #tag
  WHERE EXPR
  SORT EXPR [ASC|DESC]
  LIMIT NUMBER

Expressions share the Base expression evaluator. Page rows expose arbitrary
frontmatter plus:
  file.path, file.name, file.folder, file.ext, file.link, file.size,
  file.mtime, file.tags, file.frontmatter

TASK queries operate on the same normalized task records as the Tasks adapter.

Current compatibility limits:
  - AND/OR source combinations are not yet interpreted.
  - GROUP BY emits rows containing `key` and grouped `rows`.
  - FLATTEN expands array values and accepts `FLATTEN EXPR AS NAME`.
  - Dataview's complete function library, durations, regex literals, and link
    comparison semantics are only partially implemented."#;

const DATAVIEWJS: &str = r#"# DataviewJS-compatible query

Input:
  mdq query --language dataviewjs --file query.js
  mdq query --language dataviewjs --current Note.md 'dv.list(dv.pages().map(p => p.file.link))'

Provided API:
  dv.pages(source)
  dv.page(path)
  dv.current()
  dv.date(value)
  dv.fileLink(path, embed, display)
  dv.list(values)
  dv.table(columns, rows)
  dv.taskList(values)
  dv.paragraph(value)
  dv.view(path, input)      expanded from a local vault JavaScript file

DataArray methods:
  where, map, flatMap, sort, groupBy, distinct, array

Security boundary:
  - QuickJS runs in-process with a 64 MiB memory limit, 512 KiB stack limit,
    and 500 ms interrupt deadline.
  - No Obsidian `app`, Node `require` or `process`, network `fetch`,
    XMLHttpRequest, WebSocket, DOM `document`, or `window`.
  - `dv.io` and DOM rendering are disabled.
  - The vault is exposed as serialized read-only page/task data.
  - `dv.view` may only load a `.js` file beneath the selected vault.

Output-producing calls are captured as structured RecordSet rows. This is a
CLI compatibility layer, not a browser or Obsidian renderer."#;

const EXTENSIONS: &str = r#"# Extension API

Compatibility query language:
  Implement `core::QueryAdapter`:

    trait QueryAdapter: Send + Sync {
        fn name(&self) -> &'static str;
        fn execute(
            &self,
            context: &QueryContext<'_>,
            source: &str,
        ) -> anyhow::Result<RecordSet>;
    }

  Register it with `CompatibilityEngine::register`. Adapters receive generic
  PageRecord data from `Database::all_pages` and return a RecordSet containing
  ordered columns, rows, and diagnostics.

JavaScript runtime:
  Implement `script::ScriptEngine`. Syntax adapters depend on this boundary,
  not directly on QuickJS.

Native metadata predicate:
  Implement `query::QueryLanguage` and `MetadataFilter`, then register it with
  `PipelineEngine::register_query_language`.

Retrieval stage:
  Implement `pipeline::StageExecutor`, then register it with
  `PipelineEngine::register_stage`.

Ownership rules:
  - `db` stores generic Markdown/page/link/chunk data only.
  - `compat/<language>` owns syntax ASTs and compatibility behavior.
  - `core` owns application-neutral records, context, results, and adapter
    contracts.
  - `script` owns execution limits and host exposure.
  - Vault-specific frontmatter names must never enter core or database code."#;
