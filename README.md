# mdq

`mdq` is a local-first search and retrieval CLI for arbitrary Markdown
collections. It does not depend on a note-taking application or on a fixed
frontmatter schema.

## Features

- SQLite FTS5 BM25 search
- language-independent Latin word and CJK bigram indexing
- arbitrary YAML frontmatter path queries
- Wiki links (`[[Note]]`, `![[Note]]`) and Markdown links
  (`[label](Note.md)`, `![embed](file)`)
- backlink and graph traversal across both link styles
- compact JSON or text context retrieval for RAG pipelines
- local multilingual semantic embeddings and hybrid RRF retrieval
- indexes stored outside the collection in the user cache directory

## Build

```sh
cargo build --release
```

The first `embed` or `semantic` command downloads the
`multilingual-e5-small` model. Embeddings are cached in SQLite by content
hash, so rebuilding the Markdown index does not require recomputing unchanged
chunks. The downloaded model cache is approximately 500 MB and is stored
under the operating system cache directory.

## Usage

```sh
mdq --vault ~/notes index
mdq --vault ~/notes pipeline \
  --stage 'filter:created >= 2026-01-01 and labels contains research' \
  --stage 'bm25+rag:public key encryption'
mdq --vault ~/notes search "lattice cryptography"
mdq --vault ~/notes embed
mdq --vault ~/notes semantic "public key encryption research"
mdq --vault ~/notes query 'project.state = active'
mdq --vault ~/notes query 'custom.items contains value'
mdq --vault ~/notes backlinks "Folder/Note"
mdq --vault ~/notes links "Folder/Note"
mdq --vault ~/notes graph "Folder/Note" --depth 2
mdq --vault ~/notes --json context "authentication design"
mdq --vault ~/notes --json rag "authentication design"
mdq --vault ~/notes status
```

`pipeline` is the canonical execution model. Stages run in the exact order
given and may be repeated:

```text
filter[@language]:EXPRESSION
bm25:QUERY
rag:QUERY
bm25+rag:QUERY
```

`search`, `query`, `semantic`, `context`, and `rag` are convenience aliases for
one-stage pipelines.

## Native filter language

Frontmatter paths are generic dotted paths with optional array indices. No
property name has built-in meaning.

```text
arbitrary.nested.key = value
arbitrary.list[0] != value
score >= 3.5
date >= 2026-01-01
datetime < 2026-06-15T00:00:00+09:00
labels contains research
labels contains_all [research,cryptography]
labels overlaps [paper,project]
state in [active,paused]
name starts_with prefix
name ends_with suffix
name matches "^prefix-"
arbitrary.key exists
arbitrary.key missing
not archived = true
created >= 2026-01-01 and (score > 3 or labels contains urgent)
```

Supported scalar types come from YAML: strings, numbers, booleans, and null.
Lists and objects use YAML flow syntax. Ordering compares numbers numerically,
ISO dates and RFC 3339 timestamps chronologically, and other strings
lexicographically.

## Link resolution

`mdq` indexes both Wiki links and standard Markdown links. Relative Markdown
paths are resolved from the source file directory, percent-encoded paths are
decoded, and external URLs are ignored. A filename-only link is resolved only
when it identifies a single Markdown file; ambiguous links remain unresolved.

## Index location

By default, indexes are written under the operating system cache directory,
not inside the Markdown collection. Use `--db <path>` to override it.

## Extending mdq

The Rust library exposes `QueryLanguage`, `MetadataFilter`, `StageExecutor`,
and `PipelineEngine::register_*`. Tasks, Base, and Dataview compatibility can
therefore be implemented as separate parser adapters or stages without adding
special cases to the native frontmatter implementation. See
[`docs/architecture.md`](docs/architecture.md).
