use anyhow::{Result, bail};

use crate::schema;

include!(concat!(env!("OUT_DIR"), "/generated_manual.rs"));

pub const TOPICS: &[&str] = &[
    "overview",
    "index",
    "search",
    "query",
    "backlinks",
    "links",
    "unresolved-links",
    "graph",
    "pipeline",
    "status",
    "json-schemas",
    "native",
    "tasks",
    "base",
    "base-expr",
    "dataview",
    "dataview-expr",
    "dataviewjs",
    "extensions",
    "examples",
];

pub fn render(topic: Option<&str>) -> Result<String> {
    match topic {
        None | Some("all") => Ok(vec![
            OVERVIEW.to_owned(),
            index_manual(),
            search_manual(),
            query_manual(),
            backlinks_manual(),
            links_manual(),
            unresolved_links_manual(),
            graph_manual(),
            pipeline_manual(),
            status_manual(),
            json_schemas_manual(),
            GENERATED_NATIVE_MANUAL.to_owned(),
            GENERATED_TASKS_MANUAL.to_owned(),
            base_manual(),
            dataview_manual(),
            DATAVIEWJS.to_owned(),
            EXTENSIONS.to_owned(),
            EXAMPLES.to_owned(),
        ]
        .join("\n\n")),
        Some("overview") => Ok(OVERVIEW.to_owned()),
        Some("index") => Ok(index_manual()),
        Some("search") => Ok(search_manual()),
        Some("query") => Ok(query_manual()),
        Some("backlinks") => Ok(backlinks_manual()),
        Some("links") => Ok(links_manual()),
        Some("unresolved-links") | Some("unresolved") => Ok(unresolved_links_manual()),
        Some("graph") => Ok(graph_manual()),
        Some("pipeline") => Ok(pipeline_manual()),
        Some("status") => Ok(status_manual()),
        Some("json-schemas") | Some("schemas") => Ok(json_schemas_manual()),
        Some("native") | Some("filter") => Ok(GENERATED_NATIVE_MANUAL.to_owned()),
        Some("tasks") => Ok(GENERATED_TASKS_MANUAL.to_owned()),
        Some("base") => Ok(base_manual()),
        Some("base-expr") | Some("base-expression") => Ok(GENERATED_BASE_EXPR_MANUAL.to_owned()),
        Some("dataview") | Some("dql") => Ok(dataview_manual()),
        Some("dataview-expr") | Some("dataview-expression") => {
            Ok(GENERATED_DATAVIEW_EXPR_MANUAL.to_owned())
        }
        Some("dataviewjs") | Some("dvjs") => Ok(DATAVIEWJS.to_owned()),
        Some("extensions") | Some("api") => Ok(EXTENSIONS.to_owned()),
        Some("examples") => Ok(EXAMPLES.to_owned()),
        Some(topic) => bail!(
            "unknown manual topic: {topic}\navailable topics: {}",
            TOPICS.join(", ")
        ),
    }
}

/// Compact, task-agnostic reference intended for agents and automation.
pub fn quickref() -> String {
    format!(
        r#"# mdq quick reference

Quote query expressions containing spaces or operators with single quotes.
Use --json and jq to retain only fields needed by the caller.

Structured queries:
  mdq query '<frontmatter expression>' --json | jq -r '.[].path'
  mdq query 'TABLE file.path FROM "<folder>" WHERE <expr>' --language dataview --json
  mdq query 'not done
tags include #<tag>' --language tasks --json
Native expressions support comparisons, AND/OR/NOT, and nested fields.
Dataview queries must start with LIST, TABLE, TASK, or CALENDAR.

Search engines:
  mdq search '<terms>' --json                       # hybrid BM25 + RAG
  mdq search '<terms>' --only bm25 --json           # exact names, identifiers, rare terms
  mdq search '<concept>' --only rag --json           # concepts and paraphrases
Hybrid combines lexical and semantic evidence. No engine is universally best;
compare engines when ranking quality matters. Scope with --path REGEX and
repeatable --exclude REGEX. Search results are candidates, not guaranteed truth.

Links and graph:
  mdq links <note> --json
  mdq unresolved-links --json
  mdq graph <note> --direction outgoing --depth unlimited --json
  mdq graph <note-a> <note-b> --direction incoming --depth 1 --json
Direction is outgoing, incoming, or both; depth is a hop count or unlimited.
The graph JSON shape is {{direction, depth, starts, notes}}; select paths with
`jq -r '.notes[].path'`. `mdq backlinks <note>` is syntax sugar for incoming
depth 1, returns the same shape, and includes the start note.

Full context:
  mdq pipeline --stage 'bm25:<terms>' --context --json

Query output defaults to 100 rows; use --limit when exhaustive output is needed.
Help: mdq <command> --help; detailed reference: mdq manual <topic>.

{}"#,
        schema::compact_reference()
    )
}

fn schema_section(schema: &str) -> String {
    format!(
        "JSON output schema (--json), generated from Rust output types:\n{}",
        schema_code_block(schema)
    )
}

fn schema_code_block(schema: &str) -> String {
    format!("```json\n{schema}\n```")
}

fn search_manual() -> String {
    SEARCH.replace(
        "{search_json_schema}",
        &schema_section(&schema::command_schema_json("search").expect("search schema exists")),
    )
}

fn index_manual() -> String {
    INDEX.replace(
        "{index_json_schema}",
        &schema_section(&schema::command_schema_json("index").expect("index schema exists")),
    )
}

fn query_manual() -> String {
    QUERY
        .replace(
            "{query_native_json_schema}",
            &schema_code_block(
                &schema::command_schema_json("query native").expect("query schema exists"),
            ),
        )
        .replace(
            "{query_record_set_json_schema}",
            &schema_code_block(
                &schema::command_schema_json("query record-set").expect("record set schema exists"),
            ),
        )
}

fn backlinks_manual() -> String {
    BACKLINKS.replace(
        "{graph_json_schema}",
        &schema_section(
            &schema::command_schema_json("backlinks").expect("backlinks schema exists"),
        ),
    )
}

fn links_manual() -> String {
    LINKS.replace(
        "{link_ref_json_schema}",
        &schema_section(&schema::command_schema_json("links").expect("links schema exists")),
    )
}

fn unresolved_links_manual() -> String {
    UNRESOLVED_LINKS.replace(
        "{link_ref_json_schema}",
        &schema_section(
            &schema::command_schema_json("unresolved-links")
                .expect("unresolved-links schema exists"),
        ),
    )
}

fn graph_manual() -> String {
    GRAPH.replace(
        "{graph_json_schema}",
        &schema_section(&schema::command_schema_json("graph").expect("graph schema exists")),
    )
}

fn pipeline_manual() -> String {
    PIPELINE
        .replace(
            "{pipeline_hits_json_schema}",
            &schema_code_block(
                &schema::command_schema_json("pipeline").expect("pipeline schema exists"),
            ),
        )
        .replace(
            "{pipeline_context_json_schema}",
            &schema_code_block(
                &schema::command_schema_json("pipeline --context")
                    .expect("pipeline context schema exists"),
            ),
        )
}

fn status_manual() -> String {
    STATUS.replace(
        "{status_json_schema}",
        &schema_section(&schema::command_schema_json("status").expect("status schema exists")),
    )
}

fn json_schemas_manual() -> String {
    format!(
        "# JSON output schemas\n\nThese schemas are generated from the Rust output DTOs used by `--json`.\n\n{}\n\nFull schemas:\n```json\n{}\n```",
        schema::compact_reference(),
        schema::all_command_schemas_json()
    )
}

const OVERVIEW: &str = r#"# mdq manual

mdq is an application-independent Markdown query and retrieval CLI. It does
not require Obsidian and does not assign meaning to vault-specific frontmatter
property names.

Commands (see `mdq manual COMMAND` for each):
  quickref     compact task-agnostic reference for agents
  index        build the BM25 index and local embeddings
  search       hybrid BM25 + semantic context retrieval
  query        native, Tasks, Base, Dataview, or DataviewJS query
  backlinks    notes linking to a note
  links        links from a note
  unresolved-links
               links that do not resolve to a vault note or file
  graph        traverse resolved links in both directions
  pipeline     run filters and rankers in a supplied order
  status       index metadata and counts
  json-schemas JSON schemas generated from Rust output types

Query languages used by `query` and `pipeline` (see `mdq manual TOPIC`):
  native       generic frontmatter predicate language
  tasks        Obsidian Tasks-compatible task query subset
  base         Obsidian Base-compatible YAML query
  base-expr    expression grammar used inside Base YAML fields
  dataview     Dataview DQL-compatible page/task query subset
  dataview-expr expression grammar used inside Dataview clauses
  dataviewjs   read-only DataviewJS-compatible runtime

`search`, `query`, `backlinks`, `links`, `unresolved-links`, `graph`, and
`pipeline` automatically refresh a small amount of index drift (see
`--auto-threshold`) and otherwise require an explicit `index` run.

Use `mdq manual TOPIC` for a focused reference, `mdq manual examples` for
ready-made use cases, or `mdq manual all` for the complete manual."#;

const INDEX: &str = r#"# index command

  mdq --vault PATH index [--only bm25|embed] [--batch-size N]

Builds the BM25 full-text index and local semantic embeddings together.

Flags:
  --only bm25      build only the BM25 index, skip embeddings
  --only embed     build only embeddings, skip the BM25 index
  --batch-size N   embedding batch size (default 64)

{index_json_schema}

The first run that builds embeddings downloads the multilingual-e5-small
model (about 500 MB, cached under the OS cache directory). Re-running `index`
only recomputes content that changed; embeddings are cached by content hash,
so an unchanged chunk is never re-embedded."#;

const SEARCH: &str = r#"# search command

  mdq --vault PATH search QUERY [--only bm25|rag] [--limit N]
    [--path REGEX] [--exclude REGEX] [--max-chars N] [--verbose]

Retrieves ranked source context for QUERY. Hybrid BM25 + semantic (RRF) by
default.

Flags:
  --only bm25      BM25 full-text retrieval only
  --only rag       semantic embedding retrieval only
  --limit N        maximum results (default 8)
  --exclude REGEX  exclude note paths matching a regex; repeatable
  --path REGEX     include only matching note paths; repeatable
  --max-chars N    total context character budget (default: unlimited); counts
                   path, heading label, and body text per result
  --verbose        include score and ranking detail in the output

{search_json_schema}

JSON results include `match_reasons` such as `title`, `path`, and `heading`.
Search results are candidates; combine `--path` and `--exclude` with a focused
query when the requested folder or document type is known.

`search` is a one-stage convenience over `pipeline` (`bm25+rag:QUERY` by
default, or `bm25:QUERY` / `rag:QUERY` with `--only`).

When to use:
  Use `search` when you know terms or concepts but not exact metadata fields.
  Use `query` when the answer depends on structured frontmatter, Tasks, Base,
  or Dataview predicates.

Examples:
  mdq search "benchmark deterministic metrics" --only bm25 --json
  mdq search "why did we choose the benchmark design" --only rag --json
  mdq search "public key encryption" --limit 5 --max-chars 3000 --json

Related manuals:
  mdq manual pipeline
  mdq manual query"#;

const QUERY: &str = r#"# query command

  mdq --vault PATH query [EXPRESSION] --language LANGUAGE
    [--file PATH] [--current PATH] [--tasks-status SPEC]
    [--tasks-global-filter TEXT] [--tasks-global-query QUERY] [--limit N] [--verbose]

Runs one of five query languages against the indexed vault:
  native (default)   see `mdq manual native`
  tasks               see `mdq manual tasks`
  base                see `mdq manual base`
  dataview            see `mdq manual dataview`
  dataviewjs          see `mdq manual dataviewjs`

Provide EXPRESSION inline, or `--file` for `.base` documents and longer
scripts; inline source and `--file` are mutually exclusive. `--current` sets
the note used by `this.file` (base) and `dv.current()` (dataviewjs).
`--limit` truncates the result rows (default 100). For Tasks queries this is
an additional CLI limit applied after any `limit` instruction inside the Tasks
query; if it truncates rows, mdq reports that in diagnostics.
For Tasks queries, repeat `--tasks-status` to define vault-independent status
names and types: `SYMBOL=TYPE` or `SYMBOL=NAME:TYPE[:NEXT]`.
Use `--tasks-global-filter` to require a marker string on task lines, and
`--tasks-global-query` to prepend a default Tasks query.
Tasks output is compact by default: each row contains only `path`, the
one-based `line`, and the original Markdown task line as `task`. Pass
`--verbose` to include the full Tasks compatibility record. This applies to
both JSONL output and the `--json` RecordSet.

`native` queries return matching notes. Every other language returns
structured RecordSet rows (see `mdq manual extensions`).

JSON output schema (--json):
  native:
{query_native_json_schema}
  tasks, base, dataview, dataviewjs:
{query_record_set_json_schema}

Choosing a language:
  native       simple frontmatter predicates over notes
  tasks        checkbox task lines and Tasks-compatible filters
  base         saved Obsidian Base YAML documents or Base YAML snippets
  dataview     Dataview DQL page/task tables, lists, filters, sorts
  dataviewjs   read-only DataviewJS snippets over serialized vault data

Examples:
  mdq query 'status = active and score >= 8' --json
  mdq query 'not done
due before today
sort by due' --language tasks --json
  mdq query 'filters: "status == \"active\""
views:
  - type: table
    order: [file.path, status]' --language base --json
  mdq query 'TABLE file.path, status FROM "Projects" WHERE status == "active"' --language dataview --json
  mdq query 'dv.table(["path"], dv.pages("\"Projects\"").map(p => [p.file.path]))' --language dataviewjs --json

Related manuals:
  mdq manual native
  mdq manual tasks
  mdq manual base
  mdq manual dataview
  mdq manual dataviewjs"#;

const BACKLINKS: &str = r#"# backlinks command

  mdq --vault PATH backlinks NOTE [NOTE ...]

Syntax sugar for `graph NOTE --direction incoming --depth 1`. It returns the
start note and notes which link to it. Multiple starts are accepted.

NOTE may be a path (`People/Alice.md`) or a note-like target (`Alice`) that
mdq can resolve through the indexed link resolver.
If an exact path is not found, mdq retains the unique basename/title fallback
and reports the selected indexed path on stderr.

{graph_json_schema}

When to use:
  Use `backlinks` when the question is about notes that refer to another note.
  Use `links` for outgoing links from a note, and `graph` for multi-hop link
  neighborhoods.

Examples:
  mdq backlinks People/Alice.md --json
  mdq backlinks Alice --json

Related manuals:
  mdq manual links
  mdq manual graph
  mdq manual query"#;

const LINKS: &str = r#"# links command

  mdq --vault PATH links NOTE

Lists outgoing links from NOTE. Text output is one
`raw_target<TAB>resolved_path<TAB>embed` row per line; an unresolved target
prints `<unresolved>` in place of the resolved path. `--json` returns the
full structured link records.
If NOTE requires the unique basename/title fallback, mdq reports the selected
indexed path on stderr while keeping the link output unchanged.

{link_ref_json_schema}

When to use:
  Use `links` to inspect references made by a note. Use `backlinks` to inspect
  notes that refer to a note.

Examples:
  mdq links Projects/Alpha.md --json
  mdq links "Daily/2026-07-09.md" --json

Related manuals:
  mdq manual backlinks
  mdq manual unresolved-links
  mdq manual graph"#;

const UNRESOLVED_LINKS: &str = r#"# unresolved-links command

  mdq --vault PATH unresolved-links

Lists vault-wide links that do not resolve to an indexed Markdown note or an
existing non-Markdown vault file. Text output is one
`source_path<TAB>raw_target<TAB>embed` row per line; `--json` returns the full
structured link records.

{link_ref_json_schema}

When to use:
  Use `unresolved-links` to audit missing or misspelled link targets.

Examples:
  mdq unresolved-links --json

Related manuals:
  mdq manual links
  mdq manual backlinks"#;

const GRAPH: &str = r#"# graph command

  mdq --vault PATH graph NOTE [NOTE ...]
    [--direction outgoing|incoming|both] [--depth N|unlimited]

Traverses resolved links from one or more starts. Direction defaults to `both`
and depth defaults to 2. Output is deterministic and includes every start.
JSON includes `direction`, nullable `depth`, `starts`, and `notes`.
Note arguments that require the unique basename/title fallback report their
selected indexed paths on stderr.

When to use:
  Use `graph` when link neighborhood matters more than a single incoming or
  outgoing edge list.

{graph_json_schema}

Examples:
  mdq graph Projects/Atlas.md --depth 1 --json
  mdq graph Alice Bob --direction outgoing --depth unlimited --json
  mdq backlinks Alice --json

Related manuals:
  mdq manual backlinks
  mdq manual links"#;

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
                   (default: unlimited)
  --verbose        include score and ranking detail in the output

Example:
  mdq pipeline \
    --stage 'filter:created >= 2026-01-01' \
    --stage 'bm25:cryptography' \
    --stage 'rag:public key research'

When to use:
  Use `pipeline` when you want an explicit sequence such as structured filtering
  followed by BM25 or RAG ranking.

JSON output schema (--json):
  without --context:
{pipeline_hits_json_schema}
  with --context:
{pipeline_context_json_schema}

`search` and native `query` are convenience commands over this pipeline.

Related manuals:
  mdq manual native
  mdq manual search"#;

const STATUS: &str = r#"# status command

  mdq --vault PATH status

Shows index metadata: vault path, `indexed_at` timestamp, note/chunk/link
counts, `unresolved_links`, `embeddings` and `cached_embeddings` counts, and
whether the index or embeddings are stale relative to the vault.
`unresolved_links` excludes links to existing non-Markdown vault files such as
Base documents and attachments.

When to use:
  Use `status` before scripted runs to check whether an index exists, whether it
  is stale, and whether embeddings are available.

{status_json_schema}

Examples:
  mdq status
  mdq status --json"#;

const BASE: &str = r#"# Base-compatible query

Input:
  mdq query --language base --file path/to/view.base
  mdq query --language base --file view.base --current Daily/2026-06-14.md
  mdq query --language base 'filters: "status == \"active\""
views:
  - type: table
    order: [file.path, status]'

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

Named summary types (for views[0].summaries):
  Count, Sum, Average, Min, Max, Range, Median, Stddev
  Earliest, Latest (date values)
  Checked, Unchecked (boolean values)
  Empty, Filled (null/empty-string check)
  Unique (distinct count)
  Any name defined in document-level `summaries` (custom formula with `values`)

`this.file` is available when `--current` is supplied. Formulas run multiple
passes so later formulas may reference `formula.<name>` from earlier ones.

When to use:
  Use Base when you have a saved `.base` view, want YAML view configuration, or
  need Base-compatible formulas, filters, sorting, grouping, or summaries.

Examples:
  mdq query --language base --file views/projects.base --json
  mdq query --language base --file views/today.base --current Daily/2026-06-14.md --json
  mdq query --language base 'filters: "score >= 8"
views:
  - type: table
    order: [file.path, score]
    sort:
      - property: score
        direction: DESC' --json

Output:
  Base returns a RecordSet with columns, rows, optional summaries, and
  diagnostics. File metadata is available through `file.*`; frontmatter fields
  are available by their property names.

Compatibility limits:
  - Only the first view entry is executed.
  - Rendering configuration and column sizes are ignored.
  - Link values are structured objects, not Obsidian UI wikilink objects.

Related manuals:
  mdq manual base-expr
  mdq manual query"#;

const DATAVIEW: &str = r#"# Dataview DQL-compatible query

Input:
  mdq query --language dataview \
    'TABLE title AS Name, created FROM "Daily" WHERE created >= date(2026-01-01) SORT created DESC LIMIT 10'

A query must start with LIST, TABLE, TASK, or CALENDAR. A predicate alone is
not a complete DQL query; use, for example,
`LIST FROM "Worklog" WHERE approver != null`.

Page rows expose arbitrary frontmatter plus:
  file.path, file.name, file.folder, file.ext, file.link, file.size,
  file.mtime, file.tags, file.frontmatter

TASK queries operate on the same normalized task records as the Tasks adapter.

When to use:
  Use Dataview when you want DQL-style TABLE, LIST, TASK, or CALENDAR queries
  over pages or tasks.

Examples:
  mdq query --language dataview 'TABLE file.path, status FROM "Projects" WHERE status == "active"' --json
  mdq query --language dataview 'TABLE file.path, score FROM "Projects" WHERE score >= 8 SORT score DESC' --json
  mdq query --language dataview 'TABLE file.path FROM "Projects" WHERE file.hasLink("People/Name.md")' --json
  mdq query --language dataview 'TASK FROM "Projects" WHERE !completed LIMIT 20' --json

Output:
  Dataview returns a RecordSet. For TABLE queries, selected expressions become
  columns. Page rows expose frontmatter plus the `file.*` object. TASK queries
  expose normalized task fields.

Current compatibility limits:
  - AND/OR source combinations are not yet interpreted.
  - GROUP BY emits rows containing `key` and grouped `rows`.
  - FLATTEN expands array values and accepts `FLATTEN EXPR AS NAME`.
  - Dataview's complete function library, durations, regex literals, and link
    comparison semantics are only partially implemented.

Related manuals:
  mdq manual dataview-expr
  mdq manual tasks
  mdq manual query"#;

fn base_manual() -> String {
    [BASE, GENERATED_BASE_EXPR_MANUAL].join("\n\n")
}

fn dataview_manual() -> String {
    [DATAVIEW, GENERATED_DATAVIEW_EXPR_MANUAL].join("\n\n")
}

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
  - QuickJS runs in-process with a vault-sized read-only host-data allowance
    plus a 64 MiB script memory budget, 512 KiB stack limit, and 500 ms
    interrupt deadline.
  - No Obsidian `app`, Node `require` or `process`, network `fetch`,
    XMLHttpRequest, WebSocket, DOM `document`, `window`, or `eval`.
  - `dv.io` and DOM rendering are disabled.
  - The vault is exposed as serialized read-only page/task data.
  - Page indexes, page metadata, links, and per-page tasks are loaded lazily
    from Rust; user JavaScript executes exactly once.
  - `dv.view` may only load a `.js` file beneath the selected vault.
  - Treat DataviewJS and Tasks function scripts as trusted code — the sandbox
    restricts host access, not script capabilities within the vault data.

Output-producing calls are captured as structured RecordSet rows. This is a
CLI compatibility layer, not a browser or Obsidian renderer.

When to use:
  Use DataviewJS for read-only scripts that need mapping, grouping, or custom
  output beyond DQL.

Examples:
  mdq query --language dataviewjs 'dv.list(dv.pages("\"Projects\"").map(p => p.file.path))' --json
  mdq query --language dataviewjs 'dv.table(["path", "status"], dv.pages().map(p => [p.file.path, p.status]))' --json

Related manuals:
  mdq manual dataview
  mdq manual query"#;

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tasks_manual_is_generated_from_pest_doc_markers() {
        assert_generated_manual("tasks", include_str!("compat/tasks.pest"));
    }

    #[test]
    fn native_manual_is_generated_from_pest_doc_markers() {
        assert_generated_manual("native", include_str!("filter.pest"));
    }

    #[test]
    fn base_expression_manual_is_generated_from_pest_doc_markers() {
        assert_generated_manual("base-expr", include_str!("compat/base.pest"));
    }

    #[test]
    fn dataview_expression_manual_is_generated_from_pest_doc_markers() {
        assert_generated_manual("dataview-expr", include_str!("compat/dataview.pest"));
    }

    fn assert_generated_manual(topic: &str, source: &str) {
        let expected = source
            .lines()
            .filter_map(|line| line.trim_start().strip_prefix("// mdq-doc:"))
            .map(str::trim_start)
            .collect::<Vec<_>>()
            .join("\n");

        assert_eq!(render(Some(topic)).unwrap(), expected);
    }

    #[test]
    fn command_manuals_include_generated_json_schemas() {
        let graph = render(Some("graph")).unwrap();
        assert!(graph.contains("\"title\": \"GraphOutput\""));
        assert!(graph.contains("\"$ref\": \"#/$defs/NoteRef\""));
        assert!(!graph.contains("nodes: object[]"));

        let links = render(Some("links")).unwrap();
        assert!(links.contains("\"title\": \"Array_of_LinkRef\""));
        assert!(links.contains("\"resolved_path\""));

        let status = render(Some("status")).unwrap();
        assert!(status.contains("\"index_stale\""));
        assert!(status.contains("\"embeddings_stale\""));

        let all = render(Some("all")).unwrap();
        assert!(!all.contains("{link_ref_json_schema}"));
        assert!(!all.contains("{status_json_schema}"));
        assert!(!all.contains("{pipeline_hits_json_schema}"));
    }

    #[test]
    fn quickref_is_task_agnostic_and_covers_retrieval_and_graph_modes() {
        let reference = quickref();
        for text in [
            "--only bm25",
            "--only rag",
            "hybrid",
            "--direction",
            "--depth unlimited",
            ".notes[].path",
        ] {
            assert!(reference.contains(text), "missing {text}");
        }
        for fixture_specific in ["bm25_08", "Graph/billing-worker", "Worklog/"] {
            assert!(!reference.contains(fixture_specific));
        }
    }
}
