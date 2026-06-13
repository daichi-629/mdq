# Architecture

`mdq` separates storage, query syntax, and retrieval execution so compatible
query languages can be added without changing the index schema or CLI engine.

## Layers

1. `markdown`: parses Markdown, generic YAML frontmatter, Wiki links, and
   standard Markdown links.
2. `db`: owns the application-neutral SQLite schema and retrieval primitives.
3. `query`: parses metadata filters into `MetadataFilter`.
4. `pipeline`: applies ordered stages to a `Vec<SearchHit>`.
5. `main`: maps CLI commands to pipeline stage specifications.

No frontmatter property name has built-in meaning.

## Query language extension

Implement `query::QueryLanguage`:

```rust
pub trait QueryLanguage: Send + Sync {
    fn name(&self) -> &'static str;
    fn parse(&self, source: &str) -> anyhow::Result<Box<dyn MetadataFilter>>;
}
```

Register it with `PipelineEngine::register_query_language`. A future
Dataview-compatible parser can then be selected with:

```text
filter@dataview:...
```

Tasks, Base, and Dataview compatibility should each live in a separate module
with its own grammar and adapter. Compatibility parsers must produce the
application-neutral `MetadataFilter` interface rather than adding syntax
branches to the native grammar.

This interface is intentionally for predicate-compatible portions of another
language. A full query language that changes the record domain, such as a
Tasks query returning task records, is a pipeline stage rather than a
frontmatter filter.

## Pipeline stage extension

Implement `pipeline::StageExecutor`:

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

Register it with `PipelineEngine::register_stage`. Stages receive the current
ordered candidate set and must return the next ordered candidate set. This
contract supports filters, graph expansion, retrieval, reranking, and
projection without coupling those operations to CLI parsing.

## Current stages

- `filter[@language]:EXPRESSION`: preserves order and removes candidates.
- `bm25:QUERY`: ranks matching candidates with FTS5 BM25.
- `rag:QUERY`: ranks current candidates by local embedding similarity.
- `bm25+rag:QUERY`: applies both rankers and merges them with RRF.

The CLI aliases `search`, `query`, `semantic`, `context`, and `rag` expand to
one-stage pipelines. `pipeline` is the canonical execution model.

## Compatibility implementation boundaries

Future compatibility work should follow these boundaries:

- `tasks`: add a task extractor and task index tables in a dedicated module,
  then implement a `tasks` `StageExecutor`. Do not reinterpret frontmatter
  keys as task fields.
- `base`: add a separate parser-generator grammar and compile its
  predicate-compatible subset to `MetadataFilter`; implement sorting,
  grouping, formulas, and projection as dedicated stages.
- `dataview`: add a separate parser-generator grammar. Compile `WHERE` to a
  filter, `SORT` to a rank/sort stage, and `TABLE`/`LIST` projection to output
  stages.

Cross-language behavior belongs in shared record, filter, sort, and projection
interfaces. Syntax-specific ASTs and compatibility quirks remain inside their
adapter modules. This avoids conditionals such as `if language == dataview`
inside the index, native filter parser, or CLI.
