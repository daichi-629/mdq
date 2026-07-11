"""Evaluation and aggregation for mdq agent utility benchmark results."""

from __future__ import annotations

import csv
import json
import re
import statistics
from pathlib import Path
from typing import Any


def read_jsonl(path: Path, *, tolerate_non_json: bool = False) -> list[Any]:
    if not path.exists():
        return []
    rows = []
    with path.open(encoding="utf-8") as handle:
        for line in handle:
            line = line.strip()
            if not line:
                continue
            try:
                rows.append(json.loads(line))
            except json.JSONDecodeError:
                if not tolerate_non_json:
                    raise
    return rows


def write_json(path: Path, value: Any) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(value, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")


def parse_codex_jsonl(path: Path) -> dict[str, Any]:
    final_text = ""
    command_count = 0
    command_counts: dict[str, int] = {}
    visible_output_bytes = 0
    usage = {
        "input_tokens": 0,
        "cached_input_tokens": 0,
        "output_tokens": 0,
        "reasoning_output_tokens": 0,
    }
    for event in read_jsonl(path, tolerate_non_json=True):
        if event.get("type") == "item.completed":
            item = event.get("item", {})
            if item.get("type") == "agent_message":
                final_text = item.get("text", "")
            elif item.get("type") == "command_execution":
                command_count += 1
                command = item.get("command", "")
                exe = shell_executable_name(command)
                if exe:
                    command_counts[exe] = command_counts.get(exe, 0) + 1
                visible_output_bytes += len(item.get("aggregated_output", "").encode("utf-8"))
        elif event.get("type") == "turn.completed":
            for key in usage:
                usage[key] += int(event.get("usage", {}).get(key, 0) or 0)
    usage["total_tokens"] = usage["input_tokens"] + usage["output_tokens"]
    usage["non_cached_input_tokens"] = usage["input_tokens"] - usage["cached_input_tokens"]
    return {
        "final_text": final_text,
        "shell_command_count": command_count,
        "command_counts": command_counts,
        "agent_visible_output_bytes": visible_output_bytes,
        **usage,
    }


def shell_executable_name(command: str) -> str:
    parts = command.strip().split()
    if not parts:
        return ""
    if len(parts) >= 3 and Path(parts[0]).name in {"zsh", "bash", "sh"} and parts[1] == "-lc":
        return Path(parts[2].strip("'\"")).name
    return Path(parts[0]).name


def load_answer(path: Path, fallback_text: str = "") -> dict[str, Any]:
    text = path.read_text(encoding="utf-8") if path.exists() else fallback_text
    try:
        value = json.loads(text)
    except json.JSONDecodeError:
        return {"answer": [], "evidence": [], "_raw": text}
    if not isinstance(value, dict):
        return {"answer": [], "evidence": [], "_raw": text}
    answer = value.get("answer", [])
    evidence = value.get("evidence", [])
    return {
        "answer": [str(item) for item in answer] if isinstance(answer, list) else [],
        "evidence": [str(item) for item in evidence] if isinstance(evidence, list) else [],
    }


def score_answer(task: dict[str, Any], answer: dict[str, Any]) -> dict[str, Any]:
    expected_paths = {normalize_path(path) for path in task.get("expected_paths", [])}
    answer_values = observed_paths(answer.get("answer", []), expected_paths)
    evidence_values = observed_paths(answer.get("evidence", []), expected_paths)
    observed = answer_values | evidence_values
    if expected_paths:
        intersection = expected_paths & observed
        if task.get("expected_answer"):
            exact = expected_paths.issubset(observed)
        else:
            exact = expected_paths == answer_values or expected_paths == observed
        partial = len(intersection) / len(expected_paths)
    else:
        exact = False
        partial = 0.0
    expected_answer = task.get("expected_answer")
    if expected_answer:
        final_text = " ".join(answer.get("answer", []))
        reason_match = expected_answer.lower() in final_text.lower()
        exact = exact and reason_match
        partial = (partial + (1.0 if reason_match else 0.0)) / 2.0
    return {
        "exact_accuracy": 1.0 if exact else 0.0,
        "partial_accuracy": partial,
        "expected_paths": sorted(expected_paths),
        "observed_paths": sorted(observed),
    }


def normalize_path(value: str) -> str:
    value = value.strip().replace("\\", "/")
    if "/vault/" in value:
        value = value.split("/vault/", 1)[1]
    if "#" in value:
        value = value.split("#", 1)[0]
    match = re.search(r"((?:Projects|People|Meetings|Daily|Research|Tasks|Noise)/[^\s:#]+\.md)", value)
    if match:
        value = match.group(1)
    return value.lstrip("./")


def observed_paths(values: list[str], expected_paths: set[str] | None = None) -> set[str]:
    paths = set()
    expected_paths = expected_paths or set()
    expected_by_name = {
        key: path
        for path in expected_paths
        for key in (Path(path).name, Path(path).stem)
    }
    for item in values:
        for match in re.finditer(r"((?:Projects|People|Meetings|Daily|Research|Tasks|Noise)/[^\s:#)`]+\.md)", item):
            paths.add(normalize_path(match.group(1)))
        normalized = normalize_path(item)
        if normalized.endswith(".md"):
            paths.add(normalized)
        elif item.strip() in expected_by_name:
            paths.add(expected_by_name[item.strip()])
    return paths


def command_metrics(path: Path) -> dict[str, int]:
    rows = read_jsonl(path)
    return {
        "vault_files_read": sum(int(row.get("vault_files_seen", 0) or 0) for row in rows),
        "vault_bytes_read": sum(int(row.get("vault_bytes_seen", 0) or 0) for row in rows),
        "wrapper_command_count": len(rows),
        "mdq_command_count": sum(1 for row in rows if row.get("cmd") == "mdq"),
    }


def summarize(results_dir: Path) -> list[dict[str, Any]]:
    rows = []
    for score_path in results_dir.glob("*/*/*/score.json"):
        size = score_path.parents[2].name
        condition = score_path.parents[1].name
        task_id = score_path.parent.name
        score = json.loads(score_path.read_text(encoding="utf-8"))
        metrics_path = score_path.parent / "metrics.json"
        metrics = json.loads(metrics_path.read_text(encoding="utf-8")) if metrics_path.exists() else {}
        rows.append({"size": size, "condition": condition, "task_id": task_id, **score, **metrics})
    return rows


def write_summary(results_dir: Path) -> None:
    rows = summarize(results_dir)
    if not rows:
        return
    fields = [
        "size",
        "condition",
        "task_id",
        "exact_accuracy",
        "partial_accuracy",
        "input_tokens",
        "output_tokens",
        "total_tokens",
        "non_cached_input_tokens",
        "shell_command_count",
        "mdq_command_count",
        "vault_files_read",
        "vault_bytes_read",
        "agent_visible_output_bytes",
    ]
    with (results_dir / "summary.csv").open("w", encoding="utf-8", newline="") as handle:
        writer = csv.DictWriter(handle, fieldnames=fields, extrasaction="ignore")
        writer.writeheader()
        writer.writerows(rows)

    grouped: dict[tuple[str, str], list[dict[str, Any]]] = {}
    for row in rows:
        grouped.setdefault((row["size"], row["condition"]), []).append(row)
    lines = ["# mdq Agent Utility Benchmark Summary", ""]
    lines.append("| size | condition | exact | partial | total_tokens | files_read | vault_bytes | commands |")
    lines.append("| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: |")
    for (size, condition), group in sorted(grouped.items()):
        lines.append(
            "| {size} | {condition} | {exact:.2f} | {partial:.2f} | {tokens:.0f} | {files:.0f} | {bytes:.0f} | {commands:.0f} |".format(
                size=size,
                condition=condition,
                exact=statistics.mean(float(row.get("exact_accuracy", 0.0)) for row in group),
                partial=statistics.mean(float(row.get("partial_accuracy", 0.0)) for row in group),
                tokens=statistics.median(float(row.get("total_tokens", 0.0)) for row in group),
                files=statistics.median(float(row.get("vault_files_read", 0.0)) for row in group),
                bytes=statistics.median(float(row.get("vault_bytes_read", 0.0)) for row in group),
                commands=statistics.median(float(row.get("shell_command_count", 0.0)) for row in group),
            )
        )
    (results_dir / "summary.md").write_text("\n".join(lines) + "\n", encoding="utf-8")
