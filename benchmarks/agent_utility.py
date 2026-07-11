#!/usr/bin/env python3
"""Agent utility benchmark tooling for mdq.

The benchmark compares fresh Codex sessions across three conditions:
without-mdq, with-mdq, and mdq-context-only. It generates deterministic vaults,
installs command wrappers, runs Codex JSONL sessions, scores answers, and
aggregates the resulting metrics.
"""

from __future__ import annotations

import argparse
import datetime as dt
import json
import os
import re
import shutil
import statistics
import subprocess
import sys
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Iterable

from evaluator import (
    command_metrics as evaluate_command_metrics,
    load_answer as evaluate_load_answer,
    parse_codex_jsonl as evaluate_parse_codex_jsonl,
    score_answer as evaluate_score_answer,
    write_summary as evaluate_write_summary,
)
from fixture_generator import (
    DEFAULT_CASES_PATH as FIXTURE_DEFAULT_CASES_PATH,
    DEFAULT_SIZES as FIXTURE_DEFAULT_SIZES,
    generate_vault as generate_fixture_vault,
)


CONDITIONS = (
    "without-mdq",
    "with-mdq",
    "with-mdq-quickref",
    "with-mdq-documented",
    "mdq-context-only",
    "mdq-context-only-quickref",
    "mdq-context-only-documented",
)
MDQ_CONTEXT_ONLY_CONDITIONS = {
    "mdq-context-only",
    "mdq-context-only-quickref",
    "mdq-context-only-documented",
}
WITHOUT_MDQ_CONDITIONS = {"without-mdq"}
WRAPPED_COMMANDS = ("rg", "find", "cat", "sed", "awk", "jq", "mdq")
ANSWER_SCHEMA = {
    "type": "object",
    "required": ["answer", "evidence"],
    "properties": {
        "answer": {"type": "array", "items": {"type": "string"}},
        "evidence": {"type": "array", "items": {"type": "string"}},
    },
    "additionalProperties": False,
}


@dataclass(frozen=True)
class CommandMetric:
    condition: str
    task_id: str
    cmd: str
    argv: list[str]
    exit_code: int
    stdout_bytes: int
    stderr_bytes: int
    vault_files_seen: int
    vault_bytes_seen: int


def utc_stamp() -> str:
    return dt.datetime.now(dt.timezone.utc).strftime("%Y-%m-%dT%H%M%SZ")


def write_json(path: Path, value: Any) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(value, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")


def append_jsonl(path: Path, value: Any) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("a", encoding="utf-8") as handle:
        handle.write(json.dumps(value, ensure_ascii=False, separators=(",", ":")) + "\n")


def read_jsonl(path: Path, *, tolerate_non_json: bool = False) -> list[Any]:
    if not path.exists():
        return []
    rows = []
    with path.open(encoding="utf-8") as handle:
        for line in handle:
            line = line.strip()
            if line:
                try:
                    rows.append(json.loads(line))
                except json.JSONDecodeError:
                    if not tolerate_non_json:
                        raise
    return rows


def prompt_for(condition: str, vault_path: Path, question: str, schema_reference: str = "") -> str:
    base = f"""You are given a Markdown vault at {vault_path}.
Commands run from the vault root unless you change directories.
Answer the question exactly.

Rules:
"""
    if condition == "without-mdq":
        rules = """- Do not run `mdq`.
- You may use shell tools such as rg, find, sed, awk, and cat.
- Return only JSON matching the provided schema."""
    elif condition in {"with-mdq", "with-mdq-quickref", "with-mdq-documented"}:
        rules = """- You may use `mdq`.
- Prefer `mdq` when it can answer the question directly.
- You may inspect Markdown files if needed.
- Return only JSON matching the provided schema."""
    elif condition in MDQ_CONTEXT_ONLY_CONDITIONS:
        rules = """- You may run `mdq`.
- Do not inspect Markdown files directly with cat, sed, awk, rg, or similar tools.
- Return only JSON matching the provided schema."""
    else:
        raise ValueError(f"unknown condition: {condition}")
    quickref = ""
    if condition.endswith("-quickref") or condition.endswith("-documented"):
        quickref = f"""

mdq quick reference:
- Help: `mdq --help`, `mdq <command> --help`.
- Manuals: `mdq manual`, `mdq manual query`, `mdq manual dataview`, `mdq manual tasks`, `mdq manual base`, `mdq manual json-schemas`.
- Native frontmatter query: `mdq query '<expression>' --json`.
- Dataview query: `mdq query '<dataview query>' --language dataview --json`.
- Tasks query: `mdq query '<tasks query>' --language tasks --json`.
- Base JSON query: `mdq query '<base json>' --language base --json`.
- BM25 search: `mdq search '<terms>' --only bm25 --json`.
- RAG search: `mdq search '<terms>' --only rag --json`.
- Backlinks: `mdq backlinks <note-or-path> --json`.
- Graph: `mdq graph --json`.
- JSON output can be piped to `jq` to keep only needed paths or fields before reading it, for example `mdq backlinks People/Alice.md --json | jq '.[].source.path'`.
- Return note paths such as `Projects/example.md` in `answer` or `evidence`.
"""
        if schema_reference:
            quickref += f"\nType-generated mdq JSON schema reference:\n{schema_reference}\n"
    if condition.endswith("-documented"):
        quickref += """
Use `mdq manual TOPIC` or `mdq <command> --help` when syntax, output shape, or language choice is unclear. Treat the installed mdq manual as the authoritative reference.
"""
    return f"{base}{rules}{quickref}\n\nQuestion:\n{question}\n"


def mdq_schema_reference(mdq_bin: str | None) -> str:
    if not mdq_bin:
        return ""
    completed = subprocess.run(
        [mdq_bin, "manual", "json-schemas"],
        capture_output=True,
        text=True,
    )
    if completed.returncode != 0:
        return ""
    text = completed.stdout.strip()
    return text.split("\nFull schemas:", 1)[0].strip()


def extract_json_object(text: str) -> str:
    stripped = text.strip()
    if stripped.startswith("```"):
        stripped = re.sub(r"^```(?:json)?\s*", "", stripped)
        stripped = re.sub(r"\s*```$", "", stripped).strip()
    try:
        json.loads(stripped)
        return stripped
    except json.JSONDecodeError:
        pass
    start = stripped.find("{")
    end = stripped.rfind("}")
    if start != -1 and end != -1 and start < end:
        candidate = stripped[start : end + 1]
        try:
            json.loads(candidate)
            return candidate
        except json.JSONDecodeError:
            pass
    return stripped


def answer_schema_valid(answer: dict[str, Any]) -> bool:
    return (
        isinstance(answer.get("answer"), list)
        and all(isinstance(item, str) for item in answer.get("answer", []))
        and isinstance(answer.get("evidence"), list)
        and all(isinstance(item, str) for item in answer.get("evidence", []))
    )


def install_wrappers(output: Path) -> None:
    output.mkdir(parents=True, exist_ok=True)
    script = Path(__file__).resolve()
    for command in WRAPPED_COMMANDS:
        wrapper = output / command
        wrapper.write_text(
            f"#!/usr/bin/env sh\nexec {sh_quote(sys.executable)} {sh_quote(str(script))} wrap {sh_quote(command)} \"$@\"\n",
            encoding="utf-8",
        )
        wrapper.chmod(0o755)


def sh_quote(value: str) -> str:
    return "'" + value.replace("'", "'\"'\"'") + "'"


def resolve_real_command(command: str) -> str:
    wrapper_dir = Path(__file__).resolve().parent / "bin"
    for entry in os.environ.get("PATH", "").split(os.pathsep):
        if not entry:
            continue
        candidate = Path(entry) / command
        try:
            if candidate.exists() and os.access(candidate, os.X_OK) and candidate.parent.resolve() != wrapper_dir.resolve():
                return str(candidate)
        except OSError:
            continue
    found = shutil.which(command)
    if not found:
        raise SystemExit(f"cannot find real executable for {command}")
    return found


def is_in_vault(path: Path, vault: Path) -> bool:
    try:
        path.resolve().relative_to(vault.resolve())
        return True
    except ValueError:
        return False
    except OSError:
        return False


def file_metric(paths: Iterable[Path], vault: Path) -> tuple[int, int]:
    unique: set[Path] = set()
    total_bytes = 0
    for path in paths:
        try:
            resolved = path.resolve()
            if resolved.is_file() and is_in_vault(resolved, vault):
                unique.add(resolved)
        except OSError:
            continue
    for path in unique:
        try:
            total_bytes += path.stat().st_size
        except OSError:
            pass
    return len(unique), total_bytes


def direct_file_args(argv: list[str], cwd: Path) -> list[Path]:
    paths = []
    for arg in argv[1:]:
        if arg.startswith("-"):
            continue
        path = Path(arg)
        if not path.is_absolute():
            path = cwd / path
        if path.exists():
            paths.append(path)
    return paths


def is_codex_shell_bootstrap(command: str, args: list[str]) -> bool:
    joined = "\n".join(args)
    return (
        (command == "sed" and args in (["s/^/setopt /"], ["/^$/d"]))
        or (command == "awk" and "PWD|OLDPWD" in joined and "declare -x" in joined)
    )


def rg_paths(stdout: bytes, argv: list[str], cwd: Path) -> list[Path]:
    paths = []
    list_mode = any(arg in ("-l", "--files-with-matches", "--files") for arg in argv)
    for raw_line in stdout.splitlines():
        line = raw_line.decode("utf-8", errors="replace")
        if not line:
            continue
        if list_mode:
            candidate = line
        else:
            candidate = line.split(":", 1)[0]
        path = Path(candidate)
        if not path.is_absolute():
            path = cwd / path
        paths.append(path)
    return paths


def run_wrapper(command: str, args: list[str]) -> int:
    cwd = Path.cwd()
    vault = Path(os.environ.get("MDQ_BENCH_VAULT", cwd)).resolve()
    log_path = Path(os.environ.get("MDQ_BENCH_LOG", cwd / "commands.jsonl"))
    condition = os.environ.get("MDQ_BENCH_CONDITION", "")
    task_id = os.environ.get("MDQ_BENCH_TASK_ID", "")
    real = resolve_real_command(command)
    if is_codex_shell_bootstrap(command, args):
        return subprocess.run([real, *args], cwd=cwd).returncode
    forbidden = (
        (condition in WITHOUT_MDQ_CONDITIONS and command == "mdq")
        or (condition in MDQ_CONTEXT_ONLY_CONDITIONS and command in {"rg", "find", "cat", "sed", "awk"})
    )
    if forbidden:
        message = f"{command} is forbidden for benchmark condition {condition}\n"
        sys.stderr.write(message)
        append_jsonl(
            log_path,
            {
                "condition": condition,
                "task_id": task_id,
                "cmd": command,
                "argv": [command, *args],
                "cwd": str(cwd),
                "started_at": time.time(),
                "ended_at": time.time(),
                "exit_code": 126,
                "stdout_bytes": 0,
                "stderr_bytes": len(message.encode("utf-8")),
                "vault_files_seen": 0,
                "vault_bytes_seen": 0,
                "forbidden": True,
            },
        )
        return 126
    argv = [real, *args]
    started = time.time()
    completed = subprocess.run(argv, cwd=cwd, capture_output=True)
    ended = time.time()
    sys.stdout.buffer.write(completed.stdout)
    sys.stderr.buffer.write(completed.stderr)
    if command == "mdq":
        files_seen, bytes_seen = 0, 0
    elif command == "rg":
        files_seen, bytes_seen = file_metric(rg_paths(completed.stdout, [command, *args], cwd), vault)
    elif command in {"cat", "sed", "awk"}:
        files_seen, bytes_seen = file_metric(direct_file_args([command, *args], cwd), vault)
    elif command == "find":
        files_seen, bytes_seen = file_metric(
            (Path(line.decode("utf-8", errors="replace")) for line in completed.stdout.splitlines()),
            vault,
        )
    else:
        files_seen, bytes_seen = 0, 0
    append_jsonl(
        log_path,
        {
            "condition": os.environ.get("MDQ_BENCH_CONDITION", ""),
            "task_id": task_id,
            "cmd": command,
            "argv": [command, *args],
            "cwd": str(cwd),
            "started_at": started,
            "ended_at": ended,
            "exit_code": completed.returncode,
            "stdout_bytes": len(completed.stdout),
            "stderr_bytes": len(completed.stderr),
            "vault_files_seen": files_seen,
            "vault_bytes_seen": bytes_seen,
        },
    )
    return completed.returncode


def run_codex_task(
    generated_dir: Path,
    results_dir: Path,
    condition: str,
    task: dict[str, Any],
    codex_bin: str,
    mdq_bin: str | None,
    schema_reference: str = "",
    model: str | None = None,
    reasoning_effort: str | None = None,
) -> None:
    task_dir, schema_path, prompt, commands_log, env = prepare_agent_task(
        generated_dir, results_dir, "codex", condition, task, mdq_bin, schema_reference
    )
    codex_log = task_dir / "codex.jsonl"
    answer_path = task_dir / "answer.json"
    command = [codex_bin]
    if model:
        command.extend(["-m", model])
    if reasoning_effort:
        command.extend(["-c", f'model_reasoning_effort="{reasoning_effort}"'])
    command.extend(
        [
        "-a",
        "never",
        "-C",
        str(generated_dir / "vault"),
        "--add-dir",
        str(task_dir),
        "exec",
        "--json",
        "--ephemeral",
        "--skip-git-repo-check",
        "--sandbox",
        "workspace-write",
        "--output-schema",
        str(schema_path),
        "-o",
        str(answer_path),
        prompt,
        ]
    )
    with codex_log.open("w", encoding="utf-8") as handle:
        completed = subprocess.run(command, env=env, stdout=handle, stderr=subprocess.STDOUT, text=True)
    metrics = evaluate_parse_codex_jsonl(codex_log)
    metrics.update(evaluate_command_metrics(commands_log))
    metrics["agent"] = "codex"
    metrics["codex_exit_code"] = completed.returncode
    write_json(task_dir / "metrics.json", metrics)
    answer = evaluate_load_answer(answer_path, metrics.get("final_text", ""))
    write_json(answer_path, answer)
    write_json(task_dir / "score.json", evaluate_score_answer(task, answer))


def condition_label(agent: str, condition: str) -> str:
    return condition if agent == "codex" else f"{agent}-{condition}"


def prepare_agent_task(
    generated_dir: Path,
    results_dir: Path,
    agent: str,
    condition: str,
    task: dict[str, Any],
    mdq_bin: str | None,
    schema_reference: str,
) -> tuple[Path, Path, str, Path, dict[str, str]]:
    task_dir = results_dir / generated_dir.name / condition_label(agent, condition) / task["id"]
    task_dir.mkdir(parents=True, exist_ok=True)
    schema_path = task_dir / "answer.schema.json"
    write_json(schema_path, ANSWER_SCHEMA)
    prompt = prompt_for(condition, generated_dir / "vault", task["question"], schema_reference)
    (task_dir / "prompt.txt").write_text(prompt, encoding="utf-8")
    commands_log = task_dir / "commands.jsonl"
    env = os.environ.copy()
    wrapper_path = Path(__file__).resolve().parent / "bin"
    original_path = env.get("PATH", "")
    if mdq_bin:
        path_value = f"{wrapper_path}{os.pathsep}{Path(mdq_bin).resolve().parent}{os.pathsep}{original_path}"
    else:
        path_value = f"{wrapper_path}{os.pathsep}{original_path}"
    env.update(
        {
            "PATH": path_value,
            "MDQ_BENCH_LOG": str(commands_log),
            "MDQ_BENCH_CONDITION": condition,
            "MDQ_BENCH_TASK_ID": task["id"],
            "MDQ_BENCH_VAULT": str(generated_dir / "vault"),
        }
    )
    return task_dir, schema_path, prompt, commands_log, env


def parse_claude_stream(path: Path) -> dict[str, Any]:
    final_text = ""
    usage: dict[str, int] = {}
    for event in read_jsonl(path, tolerate_non_json=True):
        if event.get("type") == "result":
            final_text = str(event.get("result", final_text) or final_text)
            for key, value in (event.get("usage") or {}).items():
                if isinstance(value, int):
                    usage[key] = usage.get(key, 0) + value
        message = event.get("message") or {}
        for content in message.get("content") or []:
            if isinstance(content, dict) and content.get("type") == "text":
                final_text = content.get("text", final_text)
    input_tokens = int(usage.get("input_tokens", 0) or 0) + int(usage.get("cache_read_input_tokens", 0) or 0)
    output_tokens = int(usage.get("output_tokens", 0) or 0)
    return {
        "final_text": final_text,
        "shell_command_count": 0,
        "command_counts": {},
        "agent_visible_output_bytes": 0,
        "input_tokens": input_tokens,
        "cached_input_tokens": int(usage.get("cache_read_input_tokens", 0) or 0),
        "output_tokens": output_tokens,
        "reasoning_output_tokens": 0,
        "total_tokens": input_tokens + output_tokens,
        "non_cached_input_tokens": int(usage.get("input_tokens", 0) or 0),
    }


def write_extracted_answer(answer_path: Path, final_text: str) -> dict[str, Any]:
    raw_json = extract_json_object(final_text)
    schema_valid = False
    try:
        raw_value = json.loads(raw_json)
        schema_valid = isinstance(raw_value, dict) and answer_schema_valid(raw_value)
    except json.JSONDecodeError:
        pass
    answer_path.write_text(raw_json + "\n", encoding="utf-8")
    answer = evaluate_load_answer(answer_path, final_text)
    if not schema_valid:
        answer["_schema_valid"] = False
    write_json(answer_path, answer)
    return answer


def run_claude_task(
    generated_dir: Path,
    results_dir: Path,
    condition: str,
    task: dict[str, Any],
    claude_bin: str,
    mdq_bin: str | None,
    schema_reference: str,
    model: str | None = None,
    reasoning_effort: str | None = None,
) -> None:
    task_dir, schema_path, prompt, commands_log, env = prepare_agent_task(
        generated_dir, results_dir, "claude", condition, task, mdq_bin, schema_reference
    )
    claude_log = task_dir / "claude.jsonl"
    answer_path = task_dir / "answer.json"
    command = [
        claude_bin,
        "-p",
        "--output-format",
        "stream-json",
        "--json-schema",
        json.dumps(ANSWER_SCHEMA, separators=(",", ":")),
        "--permission-mode",
        "bypassPermissions",
        "--add-dir",
        str(task_dir),
    ]
    if model:
        command.extend(["--model", model])
    if reasoning_effort:
        command.extend(["--effort", reasoning_effort])
    command.append(prompt)
    with claude_log.open("w", encoding="utf-8") as handle:
        completed = subprocess.run(
            command,
            cwd=generated_dir / "vault",
            env=env,
            stdout=handle,
            stderr=subprocess.STDOUT,
            text=True,
        )
    metrics = parse_claude_stream(claude_log)
    metrics.update(evaluate_command_metrics(commands_log))
    metrics["agent"] = "claude"
    metrics["claude_exit_code"] = completed.returncode
    answer = write_extracted_answer(answer_path, metrics.get("final_text", ""))
    write_json(task_dir / "metrics.json", metrics)
    write_json(task_dir / "score.json", evaluate_score_answer(task, answer))


def run_agy_task(
    generated_dir: Path,
    results_dir: Path,
    condition: str,
    task: dict[str, Any],
    agy_bin: str,
    mdq_bin: str | None,
    schema_reference: str,
    model: str | None = None,
    reasoning_effort: str | None = None,
) -> None:
    task_dir, _schema_path, prompt, commands_log, env = prepare_agent_task(
        generated_dir, results_dir, "agy", condition, task, mdq_bin, schema_reference
    )
    agy_log = task_dir / "agy.txt"
    answer_path = task_dir / "answer.json"
    command = [
        agy_bin,
        "--print",
        prompt,
        "--print-timeout",
        "20m",
        "--log-file",
        str(task_dir / "agy-cli.log"),
        "--dangerously-skip-permissions",
    ]
    if model:
        command.extend(["--model", model])
    completed = subprocess.run(
        command,
        cwd=generated_dir / "vault",
        env=env,
        capture_output=True,
        text=True,
    )
    agy_log.write_text(completed.stdout + completed.stderr, encoding="utf-8")
    final_text = completed.stdout
    metrics = {
        "agent": "agy",
        "final_text": final_text,
        "agy_exit_code": completed.returncode,
        "shell_command_count": 0,
        "command_counts": {},
        "agent_visible_output_bytes": len((completed.stdout + completed.stderr).encode("utf-8")),
        "input_tokens": 0,
        "cached_input_tokens": 0,
        "output_tokens": 0,
        "reasoning_output_tokens": 0,
        "total_tokens": 0,
        "non_cached_input_tokens": 0,
    }
    metrics.update(evaluate_command_metrics(commands_log))
    answer = write_extracted_answer(answer_path, final_text)
    write_json(task_dir / "metrics.json", metrics)
    write_json(task_dir / "score.json", evaluate_score_answer(task, answer))


def index_vault(generated_dir: Path, mdq_bin: str) -> None:
    subprocess.run(
        [mdq_bin, "--vault", str(generated_dir / "vault"), "index", "--only", "bm25"],
        check=True,
    )


def percentile(values: list[float], pct: float) -> float:
    if not values:
        return 0.0
    ordered = sorted(values)
    index = (len(ordered) - 1) * pct
    lower = int(index)
    upper = min(lower + 1, len(ordered) - 1)
    if lower == upper:
        return ordered[lower]
    return ordered[lower] + (ordered[upper] - ordered[lower]) * (index - lower)


def time_command(argv: list[str], repeats: int) -> dict[str, Any]:
    durations = []
    exit_codes = []
    stdout_bytes = []
    stderr_bytes = []
    for _ in range(repeats):
        started = time.perf_counter()
        completed = subprocess.run(argv, capture_output=True)
        durations.append(time.perf_counter() - started)
        exit_codes.append(completed.returncode)
        stdout_bytes.append(len(completed.stdout))
        stderr_bytes.append(len(completed.stderr))
    return {
        "argv": argv,
        "repeats": repeats,
        "exit_codes": exit_codes,
        "p50_seconds": percentile(durations, 0.50),
        "p95_seconds": percentile(durations, 0.95),
        "stdout_bytes_median": statistics.median(stdout_bytes) if stdout_bytes else 0,
        "stderr_bytes_median": statistics.median(stderr_bytes) if stderr_bytes else 0,
    }


def run_performance_benchmark(args: argparse.Namespace) -> None:
    generated = args.generated.resolve()
    vault = generated / "vault"
    mdq_bin = args.mdq_bin or shutil.which("mdq")
    if not mdq_bin:
        raise SystemExit("mdq binary not found; pass --mdq-bin")
    db_path = args.db or (args.output.parent / "perf.sqlite")
    db_path.parent.mkdir(parents=True, exist_ok=True)
    if db_path.exists():
        db_path.unlink()

    markdown_bytes = sum(path.stat().st_size for path in vault.rglob("*.md"))
    markdown_count = sum(1 for _ in vault.rglob("*.md"))
    started = time.perf_counter()
    index = subprocess.run(
        [mdq_bin, "--vault", str(vault), "--db", str(db_path), "index", "--only", "bm25"],
        capture_output=True,
    )
    index_seconds = time.perf_counter() - started
    if index.returncode != 0:
        sys.stderr.buffer.write(index.stderr)
        raise SystemExit(index.returncode)

    commands = {
        "query_native_filter": [
            mdq_bin,
            "--vault",
            str(vault),
            "--db",
            str(db_path),
            "query",
            "active = true and score >= 8",
            "--limit",
            "20",
        ],
        "query_tasks": [
            mdq_bin,
            "--vault",
            str(vault),
            "--db",
            str(db_path),
            "query",
            "not done\npriority is high",
            "--language",
            "tasks",
            "--limit",
            "20",
        ],
        "query_base": [
            mdq_bin,
            "--vault",
            str(vault),
            "--db",
            str(db_path),
            "query",
            json.dumps({"filters": [{"property": "active", "op": "==", "value": True}], "limit": 20}),
            "--language",
            "base",
        ],
        "query_dataview": [
            mdq_bin,
            "--vault",
            str(vault),
            "--db",
            str(db_path),
            "query",
            'TABLE file.name, score FROM "Projects" WHERE active = true SORT score DESC',
            "--language",
            "dataview",
            "--limit",
            "20",
        ],
        "search_bm25": [
            mdq_bin,
            "--vault",
            str(vault),
            "--db",
            str(db_path),
            "search",
            "benchmark design deterministic metrics",
            "--only",
            "bm25",
            "--limit",
            "20",
        ],
        "pipeline": [
            mdq_bin,
            "--vault",
            str(vault),
            "--db",
            str(db_path),
            "pipeline",
            "--stage",
            "filter:active = true",
            "--stage",
            "bm25:Alice benchmark",
            "--limit",
            "20",
        ],
    }
    metrics: dict[str, Any] = {
        "vault": str(vault),
        "database": str(db_path),
        "markdown_notes": markdown_count,
        "markdown_bytes": markdown_bytes,
        "db_size_bytes": db_path.stat().st_size if db_path.exists() else 0,
        "bm25_only_index": {
            "seconds": index_seconds,
            "index_bm25_notes_per_second": markdown_count / index_seconds if index_seconds else 0.0,
            "index_bm25_mb_per_second": (markdown_bytes / 1_000_000) / index_seconds if index_seconds else 0.0,
        },
        "commands": {
            name: time_command(argv, args.repeats)
            for name, argv in commands.items()
        },
    }
    if args.include_embed:
        cold_db = args.output.parent / "perf-embed-cold.sqlite"
        warm_db = args.output.parent / "perf-embed-warm.sqlite"
        for path in (cold_db, warm_db):
            if path.exists():
                path.unlink()
        metrics["cold_embed_index"] = time_command(
            [mdq_bin, "--vault", str(vault), "--db", str(cold_db), "index", "--only", "embed"],
            1,
        )
        metrics["warm_embed_index"] = time_command(
            [mdq_bin, "--vault", str(vault), "--db", str(warm_db), "index", "--only", "embed"],
            1,
        )
    write_json(args.output, metrics)


def load_tasks(generated_dir: Path) -> list[dict[str, Any]]:
    return read_jsonl(generated_dir / "tasks.jsonl")


def run_benchmark(args: argparse.Namespace) -> None:
    generated = args.generated.resolve()
    results = args.results.resolve() / utc_stamp()
    install_wrappers(Path(__file__).resolve().parent / "bin")
    mdq_bin = args.mdq_bin or shutil.which("mdq")
    if not mdq_bin:
        raise SystemExit("mdq binary not found; pass --mdq-bin")
    write_json(
        results / "config.json",
        {
            "generated": str(generated),
            "agent": args.agent,
            "conditions": list(args.conditions),
            "codex_bin": args.codex_bin,
            "claude_bin": args.claude_bin,
            "agy_bin": args.agy_bin,
            "mdq_bin": mdq_bin,
        },
    )
    if any(condition not in WITHOUT_MDQ_CONDITIONS for condition in args.conditions):
        index_vault(generated, mdq_bin)
    schema_reference = mdq_schema_reference(mdq_bin)
    tasks = load_tasks(generated)
    if args.task_id:
        selected = set(args.task_id)
        tasks = [task for task in tasks if task["id"] in selected]
        missing = selected - {task["id"] for task in tasks}
        if missing:
            raise SystemExit(f"unknown task id(s): {', '.join(sorted(missing))}")
    for task in tasks:
        for condition in args.conditions:
            if args.agent == "codex":
                run_codex_task(
                    generated,
                    results,
                    condition,
                    task,
                    args.codex_bin,
                    mdq_bin,
                    schema_reference,
                    args.model,
                    args.reasoning_effort,
                )
            elif args.agent == "claude":
                run_claude_task(
                    generated,
                    results,
                    condition,
                    task,
                    args.claude_bin,
                    mdq_bin,
                    schema_reference,
                    args.model,
                    args.reasoning_effort,
                )
            elif args.agent == "agy":
                run_agy_task(
                    generated,
                    results,
                    condition,
                    task,
                    args.agy_bin,
                    mdq_bin,
                    schema_reference,
                    args.model,
                    args.reasoning_effort,
                )
            else:
                raise SystemExit(f"unknown agent: {args.agent}")
    evaluate_write_summary(results)
    print(results)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)

    generate = sub.add_parser("generate", help="generate a deterministic vault fixture")
    generate.add_argument("output", type=Path)
    generate.add_argument("--size", choices=sorted(FIXTURE_DEFAULT_SIZES), default="medium")
    generate.add_argument("--notes", type=int)
    generate.add_argument("--seed", type=int, default=20260709)
    generate.add_argument("--cases", type=Path, default=FIXTURE_DEFAULT_CASES_PATH)

    install = sub.add_parser("install-wrappers", help="write benchmark command wrappers")
    install.add_argument("--output", type=Path, default=Path(__file__).resolve().parent / "bin")

    wrap = sub.add_parser("wrap", help=argparse.SUPPRESS)
    wrap.add_argument("wrapped_command", choices=WRAPPED_COMMANDS)
    wrap.add_argument("args", nargs=argparse.REMAINDER)

    score = sub.add_parser("score", help="score an answer against a task")
    score.add_argument("--task", type=Path, required=True)
    score.add_argument("--answer", type=Path, required=True)
    score.add_argument("--output", type=Path, required=True)

    summarize_cmd = sub.add_parser("summarize", help="aggregate existing benchmark results")
    summarize_cmd.add_argument("results", type=Path)

    run = sub.add_parser("run", help="run Codex sessions for a generated fixture")
    run.add_argument("--generated", type=Path, required=True)
    run.add_argument("--results", type=Path, default=Path("benchmarks/results"))
    run.add_argument("--agent", choices=["codex", "claude", "agy"], default="codex")
    run.add_argument("--codex-bin", default="codex")
    run.add_argument("--claude-bin", default="claude")
    run.add_argument("--agy-bin", default="agy")
    run.add_argument("--mdq-bin")
    run.add_argument("--conditions", nargs="+", choices=CONDITIONS, default=list(CONDITIONS))
    run.add_argument("--task-id", action="append")
    run.add_argument("--model")
    run.add_argument("--reasoning-effort", choices=["low", "medium", "high", "xhigh"])

    perf = sub.add_parser("perf", help="run raw mdq CLI performance measurements")
    perf.add_argument("--generated", type=Path, required=True)
    perf.add_argument("--output", type=Path, default=Path("benchmarks/results/perf.json"))
    perf.add_argument("--db", type=Path)
    perf.add_argument("--mdq-bin")
    perf.add_argument("--repeats", type=int, default=5)
    perf.add_argument("--include-embed", action="store_true")

    args = parser.parse_args(argv)
    if args.command == "generate":
        generate_fixture_vault(args.output, args.size, args.notes, args.seed, args.cases)
    elif args.command == "install-wrappers":
        install_wrappers(args.output)
    elif args.command == "wrap":
        return run_wrapper(args.wrapped_command, args.args)
    elif args.command == "score":
        task = json.loads(args.task.read_text(encoding="utf-8"))
        answer = evaluate_load_answer(args.answer)
        write_json(args.output, evaluate_score_answer(task, answer))
    elif args.command == "summarize":
        evaluate_write_summary(args.results)
    elif args.command == "run":
        run_benchmark(args)
    elif args.command == "perf":
        run_performance_benchmark(args)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
