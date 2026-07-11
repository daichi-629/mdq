# Agent Utility Benchmark

This directory implements the benchmark described in
`docs/mdq-agent-benchmark-plan.md`.

## Generate a Fixture

```sh
python3 benchmarks/agent_utility.py generate benchmarks/generated/medium \
  --size medium \
  --seed 20260709 \
  --cases benchmarks/cases/default.json
```

The generator writes:

- `vault/`
- `tasks.jsonl`
- `oracle.json`

Benchmark cases live in `benchmarks/cases/default.json`. The generator resolves
each case's expected paths from structured fixture data before Markdown
rendering, so the scorer does not re-derive truth from the vault text and new
cases do not require Python edits.

Implementation is split by responsibility:

- `fixture_generator.py`: deterministic vault, `tasks.jsonl`, and `oracle.json`
  generation.
- `evaluator.py`: answer normalization, scoring, Codex JSONL metrics parsing,
  and result aggregation.
- `agent_utility.py`: CLI orchestration, command wrappers, Codex sessions, and
  raw mdq performance runs.

## Case Coverage

`benchmarks/cases/default.json` currently defines seven cases. The original
three cases covered useful basics, but were too narrow: one frontmatter/link
filter, one task query, and one search/reasoning lookup. That was not enough to
catch common agent failures around similarly named entities, Unicode paths,
nested frontmatter, or larger multi-result answer sets.

The default suite now covers:

- exact frontmatter and link filtering for active Alice-owned projects.
- Tasks-style due date and priority filtering.
- search plus evidence-grounded reasoning over a meeting conclusion.
- Alice / Alicia / Alice-Old disambiguation.
- nested frontmatter filtering via `nested.risk`.
- Unicode filename lookup for `People/藤原.md`.

This is still a benchmark seed suite, not an exhaustive correctness suite. Use
`medium` and `large` fixtures to evaluate scale effects; `small` is mainly for
smoke tests and runner validation.

## Run Codex Sessions

```sh
cargo build --release
python3 benchmarks/agent_utility.py run \
  --generated benchmarks/generated/medium \
  --mdq-bin target/release/mdq
```

The runner installs wrappers for `rg`, `find`, `cat`, `sed`, `awk`, `jq`, and `mdq`,
prepends them to `PATH`, and starts a fresh `codex exec --json --ephemeral`
session per task and condition.

Pass `--agent claude` or `--agent agy` to run the same task harness through
Claude Code or Antigravity CLI. Claude Code supports structured output through
`--json-schema`; Agy output is validated by the benchmark runner after
extracting the final JSON object. The quick reference includes mdq JSON output
shapes read from `mdq manual json-schemas`, which is generated from Rust output
types.

Outputs are written under `benchmarks/results/<timestamp>/` with one directory
per size, condition, and task. Each task result contains:

- `codex.jsonl`
- `commands.jsonl`
- `answer.json`
- `score.json`
- `metrics.json`

## Aggregate Existing Results

```sh
python3 benchmarks/agent_utility.py summarize benchmarks/results/<timestamp>
```

This writes `summary.csv` and `summary.md` with condition-level metrics.

## Run Raw mdq Performance Measurements

Keep this separate from the agent utility benchmark:

```sh
python3 benchmarks/agent_utility.py perf \
  --generated benchmarks/generated/medium \
  --mdq-bin target/release/mdq \
  --output benchmarks/results/perf-medium.json
```

The default performance run measures BM25 indexing, native filters, Tasks,
Base, Dataview, BM25 search, pipeline queries, and database size. Pass
`--include-embed` only when model download/cache initialization is acceptable.

## Conditions

- `without-mdq`: the `mdq` wrapper rejects `mdq` invocations.
- `with-mdq`: `mdq` and direct Markdown inspection are allowed.
- `with-mdq-quickref`: same as `with-mdq`, with a short task-agnostic mdq
  command reference in the prompt.
- `with-mdq-documented`: same as `with-mdq-quickref`, and explicitly points the
  agent at `mdq manual TOPIC` / `mdq <command> --help` as the authoritative
  syntax reference.
- `mdq-context-only`: wrappers reject direct vault inspection through `rg`,
  `find`, `cat`, `sed`, and `awk`. `jq` remains available for shaping JSON
  emitted by `mdq`.
- `mdq-context-only-quickref`: same as `mdq-context-only`, with the mdq quick
  reference.
- `mdq-context-only-documented`: same as `mdq-context-only-quickref`, with
  explicit manual/help guidance.
