# Architecture

`mdq` separates generic Markdown storage, retrieval pipelines, structured
query languages, and script execution. Compatibility behavior never assigns
global meaning to a vault's frontmatter names.

## Layers

1. `markdown`: parses Markdown, arbitrary YAML frontmatter, Wiki links, and
   Markdown links.
2. `db`: owns the application-neutral SQLite schema and exposes page, chunk,
   link, BM25, and embedding primitives.
3. `pipeline`: executes ordered retrieval stages over search chunks.
4. `core`: defines `QueryContext`, `RecordSet`, and the complete-query adapter
   contract.
5. `compat`: owns Tasks, Base, Dataview DQL, and DataviewJS syntax and
   compatibility behavior.
6. `script`: owns the replaceable JavaScript execution boundary and resource
   limits.
7. `main`: maps CLI commands to the two engines without implementing query
   semantics.

## Complete Query Languages

Languages whose result domain can change implement `core::QueryAdapter`:

```rust
pub trait QueryAdapter: Send + Sync {
    fn name(&self) -> &'static str;
    fn execute(
        &self,
        context: &QueryContext<'_>,
        source: &str,
    ) -> anyhow::Result<RecordSet>;
}
```

Register adapters with `CompatibilityEngine::register`. `RecordSet` supports
ordered columns, structured rows, and diagnostics. This single boundary
handles page tables, task records, grouped results, and captured DataviewJS
render calls.

Each compatibility language owns its parser and AST under `compat/`. Shared
expression evaluation is in `compat::expr`; language-specific commands and
quirks do not enter the DB or native filter parser.

## Retrieval Pipeline

Retrieval stages implement `pipeline::StageExecutor`:

```rust
pub trait StageExecutor: Send + Sync {
    fn name(&self) -> &'static str;
    fn execute(
        &self,
        context: &StageContext<'_>,
        input: Vec<SearchHit>,
        argument: &str,
    ) -> anyhow::Result<Vec<SearchHit>>;
}
```

Stages receive the current ordered candidate set and return the next set.
`filter`, `bm25`, `rag`, and `bm25+rag` may be repeated in any order. This
contract remains search-chunk-specific; task and table queries use
`QueryAdapter` instead of forcing unrelated records into `SearchHit`.

## Predicate Languages

Metadata-only predicate syntaxes implement `query::QueryLanguage` and compile
to `MetadataFilter`. They can be selected by retrieval filter stages without
changing the native grammar.

```rust
pub trait QueryLanguage: Send + Sync {
    fn name(&self) -> &'static str;
    fn parse(&self, source: &str) -> anyhow::Result<Box<dyn MetadataFilter>>;
}
```

## Script Runtime

Syntax adapters depend on `script::ScriptEngine`, not directly on QuickJS.
The standard `QuickJsEngine` enforces memory, stack, and time limits and does
not expose filesystem, network, Node, DOM, or Obsidian hosts. DataviewJS and
Tasks function clauses receive serialized read-only records.

## Extension Rules

- Add generic indexed facts to `db`; never add a column for one vault's
  property name.
- Add language syntax and compatibility quirks under one `compat/<language>`
  module.
- Reuse `RecordSet`, `QueryContext`, and the shared expression value model.
- Add a new core abstraction only when multiple adapters need the behavior.
- Keep host I/O behind an explicit trait and deny it by default.
- Document supported syntax and limits in `src/manual.rs`, which powers
  `mdq manual`.
