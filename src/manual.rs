use anyhow::{Result, bail};

pub const TOPICS: &[&str] = &[
    "overview",
    "index",
    "search",
    "query",
    "backlinks",
    "links",
    "graph",
    "pipeline",
    "status",
    "native",
    "tasks",
    "base",
    "dataview",
    "dataviewjs",
    "extensions",
    "examples",
];

pub fn render(topic: Option<&str>) -> Result<String> {
    match topic {
        None | Some("all") => Ok([
            OVERVIEW, INDEX, SEARCH, QUERY, BACKLINKS, LINKS, GRAPH, PIPELINE, STATUS, NATIVE,
            TASKS, BASE, DATAVIEW, DATAVIEWJS, EXTENSIONS, EXAMPLES,
        ]
        .join("\n\n")),
        Some("overview") => Ok(OVERVIEW.to_owned()),
        Some("index") => Ok(INDEX.to_owned()),
        Some("search") => Ok(SEARCH.to_owned()),
        Some("query") => Ok(QUERY.to_owned()),
        Some("backlinks") => Ok(BACKLINKS.to_owned()),
        Some("links") => Ok(LINKS.to_owned()),
        Some("graph") => Ok(GRAPH.to_owned()),
        Some("pipeline") => Ok(PIPELINE.to_owned()),
        Some("status") => Ok(STATUS.to_owned()),
        Some("native") => Ok(NATIVE.to_owned()),
        Some("tasks") => Ok(TASKS.to_owned()),
        Some("base") => Ok(BASE.to_owned()),
        Some("dataview") | Some("dql") => Ok(DATAVIEW.to_owned()),
        Some("dataviewjs") | Some("dvjs") => Ok(DATAVIEWJS.to_owned()),
        Some("extensions") | Some("api") => Ok(EXTENSIONS.to_owned()),
        Some("examples") => Ok(EXAMPLES.to_owned()),
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

Commands (see `mdq manual COMMAND` for each):
  index        build the BM25 index and local embeddings
  search       hybrid BM25 + semantic context retrieval
  query        native, Tasks, Base, Dataview, or DataviewJS query
  backlinks    notes linking to a note
  links        links from a note
  graph        traverse resolved links in both directions
  pipeline     run filters and rankers in a supplied order
  status       index metadata and counts

Query languages used by `query` and `pipeline` (see `mdq manual TOPIC`):
  native       generic frontmatter predicate language
  tasks        Obsidian Tasks-compatible task query subset
  base         Obsidian Base-compatible YAML query
  dataview     Dataview DQL-compatible page/task query subset
  dataviewjs   read-only DataviewJS-compatible runtime

`search`, `query`, `backlinks`, `links`, `graph`, and `pipeline` automatically
refresh a small amount of index drift (see `--auto-threshold`) and otherwise
require an explicit `index` run.

Use `mdq manual TOPIC` for a focused reference, `mdq manual examples` for
ready-made use cases, or `mdq manual all` for the complete manual."#;

const INDEX: &str = r#"# index command

  mdq --vault PATH index [--only bm25|embed] [--batch-size N]

Builds the BM25 full-text index and local semantic embeddings together.

Flags:
  --only bm25      build only the BM25 index, skip embeddings
  --only embed     build only embeddings, skip the BM25 index
  --batch-size N   embedding batch size (default 64)

The first run that builds embeddings downloads the multilingual-e5-small
model (about 500 MB, cached under the OS cache directory). Re-running `index`
only recomputes content that changed; embeddings are cached by content hash,
so an unchanged chunk is never re-embedded."#;

const SEARCH: &str = r#"# search command

  mdq --vault PATH search QUERY [--only bm25|rag] [--limit N]
    [--max-chars N] [--verbose]

Retrieves ranked source context for QUERY. Hybrid BM25 + semantic (RRF) by
default.

Flags:
  --only bm25      BM25 full-text retrieval only
  --only rag       semantic embedding retrieval only
  --limit N        maximum results (default 8)
  --max-chars N    total context character budget (default 2000)
  --verbose        include score and heading detail in the output

`search` is a one-stage convenience over `pipeline` (`bm25+rag:QUERY` by
default, or `bm25:QUERY` / `rag:QUERY` with `--only`)."#;

const QUERY: &str = r#"# query command

  mdq --vault PATH query [EXPRESSION] --language LANGUAGE
    [--file PATH] [--current PATH] [--limit N]

Runs one of five query languages against the indexed vault:
  native (default)   see `mdq manual native`
  tasks               see `mdq manual tasks`
  base                see `mdq manual base`
  dataview            see `mdq manual dataview`
  dataviewjs          see `mdq manual dataviewjs`

Provide EXPRESSION inline, or `--file` for `.base` documents and longer
scripts; inline source and `--file` are mutually exclusive. `--current` sets
the note used by `this.file` (base) and `dv.current()` (dataviewjs).
`--limit` truncates the result rows (default 100).

`native` queries return matching notes. Every other language returns
structured RecordSet rows (see `mdq manual extensions`)."#;

const BACKLINKS: &str = r#"# backlinks command

  mdq --vault PATH backlinks NOTE

Lists notes that link to NOTE through either a Wiki link or a Markdown link.
Text output is one `source_path<TAB>raw_target` pair per line; `--json`
returns the full structured link records."#;

const LINKS: &str = r#"# links command

  mdq --vault PATH links NOTE

Lists outgoing links from NOTE. Text output is one
`raw_target<TAB>resolved_path<TAB>embed` row per line; an unresolved target
prints `<unresolved>` in place of the resolved path. `--json` returns the
full structured link records."#;

const GRAPH: &str = r#"# graph command

  mdq --vault PATH graph NOTE [--depth N]

Traverses resolved links in both directions (outgoing links and backlinks)
starting from NOTE, up to `--depth` hops (default 2), and returns every note
reached, including NOTE itself."#;

const PIPELINE: &str = r#"# pipeline command

  mdq --vault PATH pipeline --stage STAGE [--stage STAGE ...]
    [--limit N] [--context] [--max-chars N] [--verbose]

Stages execute exactly in the order supplied and may be repeated:
  filter[@language]:EXPRESSION
  bm25:QUERY
  rag:QUERY
  bm25+rag:QUERY

Flags:
  --limit N        maximum results (default 10)
  --context        return full chunk context instead of search snippets
  --max-chars N    context character budget when `--context` is set
                   (default 12000)
  --verbose        include score and heading detail in the output

Example:
  mdq pipeline \
    --stage 'filter:created >= 2026-01-01' \
    --stage 'bm25:cryptography' \
    --stage 'rag:public key research'

`search` and native `query` are convenience commands over this pipeline."#;

const STATUS: &str = r#"# status command

  mdq --vault PATH status

Shows index metadata: vault path, `indexed_at` timestamp, note/chunk/link
counts, `unresolved_links`, `embeddings` and `cached_embeddings` counts, and
whether the index or embeddings are stale relative to the vault."#;

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

Supported document-level fields:
  filters          global filter applied before views
  formulas         map of name → expression; available as `formula.<name>`
  summaries        map of name → custom aggregation expression

Supported view fields (views[0]):
  filters          per-view filter
  order            list of property paths to project (column selection)
  sort             list of {property, direction} objects
  limit            integer row cap applied after sort
  groupBy          {property, direction} — emits {key, rows} records
  summaries        map of property → summary-type name

Expressions:
  Booleans:  and, or, not, &&, ||, !
  Operators: =, ==, !=, >, >=, <, <=
  Arithmetic: +, -, *, /, %
  Field paths: `file.name`, `formula.label`, `note.score`, arbitrary frontmatter
  Literals: string, number, boolean, null
  Literals: list [a, b, c], object {key: value}
  Index access: value[0], map["key"]
  Regex: /pattern/flags

Global functions:
  if(cond, yes, no)         conditional
  date(value)               parse to date object
  today(), now()            current date/datetime
  duration(string)          parse duration string (e.g. "7d", "2 weeks")
  list(a, b, ...)           build a list
  min(a, b, ...) / max(...) numeric min/max
  number(value)             coerce to number
  length(value)             string length, array length, or object key count
  contains(a, b)            substring, array element, or object key check
  icontains(a, b)           case-insensitive contains
  startswith(a, b)          string prefix
  endswith(a, b)            string suffix
  join(list, sep)           join array to string
  file(path)                build file stub from path
  link(value, display)      build a link value
  html(value)               wrap as HTML value
  icon(name)                build an icon value
  image(path)               build an image value

String methods (.method()):
  .lower() / .upper() / .title() / .trim() / .reverse()
  .contains(x) / .containsAll(a,b) / .containsAny(a,b)
  .startsWith(x) / .endsWith(x)
  .replace(pat, rep) — pat may be a regex literal
  .split(sep, limit?) / .slice(start, end?)
  .repeat(n) / .length
  .date() — parse string as date
  .format(pattern) — Moment.js format string

Number methods:
  .abs() / .ceil() / .floor() / .round(digits?) / .toFixed(n)

List methods:
  .contains(x) / .containsAll(a,b) / .containsAny(a,b)
  .filter(expr) / .map(expr) / .reduce(expr, init?)
  .sort() / .reverse() / .unique() / .flat()
  .slice(start, end?) / .join(sep) / .length

Date methods:
  .date() / .format(pattern) / .time() / .relative()
  .year / .month / .day / .hour / .minute / .second

File methods:
  .inFolder(path) / .hasTag(tag) / .hasProperty(name)
  .asLink(display?) / .hasLink(path)

Link methods:
  .asFile() / .linksTo(path)

Named summary types (for views[0].summaries):
  Count, Sum, Average, Min, Max, Range, Median, Stddev
  Earliest, Latest (date values)
  Checked, Unchecked (boolean values)
  Empty, Filled (null/empty-string check)
  Unique (distinct count)
  Any name defined in document-level `summaries` (custom formula with `values`)

`this.file` is available when `--current` is supplied. Formulas run multiple
passes so later formulas may reference `formula.<name>` from earlier ones.

Compatibility limits:
  - Only the first view entry is executed.
  - Rendering configuration and column sizes are ignored.
  - Link values are structured objects, not Obsidian UI wikilink objects."#;

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

const EXAMPLES: &str = r##"# Useful examples

Build and keep an index current:
  mdq --vault ~/notes index
  mdq --vault ~/notes index --only embed --batch-size 32

Compact RAG context for piping into an LLM prompt:
  mdq search "lattice cryptography" --max-chars 4000 --json

Filter by frontmatter, then rank by semantic relevance:
  mdq pipeline \
    --stage 'filter:project.state = active and created >= 2026-01-01' \
    --stage 'bm25+rag:public key encryption' \
    --context --limit 5

Audit notes missing a required frontmatter field:
  mdq query 'reviewed missing'

Find overdue, untagged-as-someday tasks sorted by due date:
  mdq query --language tasks $'not done\ndue before today\ntags do not include #someday\nsort by due'

Triage urgent tasks with a function filter:
  mdq query --language tasks 'filter by function (task) => task.tags.includes("#urgent") && !task.done'

Drive a saved Obsidian Base view from the CLI, scoped to today's note:
  mdq query --language base --file views/today.base --current Daily/2026-06-21.md

Export a Dataview-style table as JSON for scripting:
  mdq query --language dataview \
    'TABLE file.name, status FROM "Projects" WHERE status = "active"' --json

Run a DataviewJS snippet without opening Obsidian:
  mdq query --language dataviewjs \
    'dv.table(["Note", "Status"], dv.pages().map(p => [p.file.link, p.status]))'

Explore a note's link neighborhood before summarizing it:
  mdq graph "Projects/Atlas" --depth 2 --json

Check whether an index needs a manual rebuild before scripting against it:
  mdq status --json"##;
