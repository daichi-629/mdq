"""Fixture generation for the mdq agent utility benchmark."""

from __future__ import annotations

import hashlib
import json
import random
import shutil
from pathlib import Path
from typing import Any


DEFAULT_SIZES = {
    "small": 100,
    "medium": 1_000,
    "large": 10_000,
    "huge": 50_000,
}
DEFAULT_CASES_PATH = Path(__file__).resolve().parent / "cases" / "default.json"


def load_cases(path: Path = DEFAULT_CASES_PATH) -> dict[str, Any]:
    return json.loads(path.read_text(encoding="utf-8"))


def write_json(path: Path, value: Any) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(value, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")


def indexed_by(items: list[dict[str, Any]], key: str = "index") -> dict[int, dict[str, Any]]:
    return {int(item[key]): item for item in items}


def get_field(item: dict[str, Any], field: str) -> Any:
    value: Any = item
    for part in field.split("."):
        if not isinstance(value, dict):
            return None
        value = value.get(part)
    return value


def matches_selector(item: dict[str, Any], where: dict[str, Any]) -> bool:
    for key, expected in where.items():
        if key.endswith("_gte"):
            field = key.removesuffix("_gte")
            if get_field(item, field) < expected:
                return False
        elif key.endswith("_lte"):
            field = key.removesuffix("_lte")
            if get_field(item, field) > expected:
                return False
        elif key.endswith("_contains"):
            field = key.removesuffix("_contains")
            value = get_field(item, field)
            if isinstance(value, list):
                if expected not in value:
                    return False
            elif expected not in str(value or ""):
                return False
        elif get_field(item, key) != expected:
            return False
    return True


def select_items(collections: dict[str, list[dict[str, Any]]], selector: dict[str, Any]) -> list[dict[str, Any]]:
    items = collections[selector["collection"]]
    where = selector.get("where", {})
    selected = [item for item in items if matches_selector(item, where)]
    order_by = selector.get("order_by")
    if order_by:
        reverse = bool(selector.get("desc"))
        selected.sort(key=lambda item: get_field(item, order_by), reverse=reverse)
    limit = selector.get("limit")
    if limit is not None:
        selected = selected[: int(limit)]
    return selected


def build_tasks(case_config: dict[str, Any], collections: dict[str, list[dict[str, Any]]]) -> list[dict[str, Any]]:
    tasks = []
    for case in case_config["tasks"]:
        task = {
            "id": case["id"],
            "type": case["type"],
            "question": case["question"],
        }
        if "expected_paths" in case:
            task["expected_paths"] = sorted(case["expected_paths"])
        elif "expected_paths_from" in case:
            task["expected_paths"] = sorted(
                item["path"] for item in select_items(collections, case["expected_paths_from"])
            )
        if "expected_answer" in case:
            task["expected_answer"] = case["expected_answer"]
        elif "expected_answer_from" in case:
            selector = case["expected_answer_from"]
            matches = select_items(collections, selector)
            if not matches:
                raise ValueError(f"case {case['id']} expected_answer_from matched no items")
            task["expected_answer"] = str(get_field(matches[0], selector["field"]))
        tasks.append(task)
    return tasks


def stable_int(seed: int, text: str) -> int:
    digest = hashlib.sha256(f"{seed}:{text}".encode("utf-8")).digest()
    return int.from_bytes(digest[:8], "big")


def render_frontmatter(data: dict[str, Any]) -> str:
    lines = ["---"]
    for key, value in data.items():
        if isinstance(value, bool):
            rendered = "true" if value else "false"
        elif isinstance(value, (int, float)):
            rendered = str(value)
        elif isinstance(value, list):
            rendered = "[" + ", ".join(json.dumps(item, ensure_ascii=False) for item in value) + "]"
        elif isinstance(value, dict):
            rendered = json.dumps(value, ensure_ascii=False)
        else:
            rendered = json.dumps(value, ensure_ascii=False)
        lines.append(f"{key}: {rendered}")
    lines.append("---")
    return "\n".join(lines) + "\n\n"


def note(path: Path, frontmatter: dict[str, Any], body: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(render_frontmatter(frontmatter) + body.rstrip() + "\n", encoding="utf-8")


def vault_mix(note_count: int) -> dict[str, int]:
    weights = {
        "Daily": 0.40,
        "Projects": 0.20,
        "Meetings": 0.15,
        "Research": 0.10,
        "People": 0.05,
        "Tasks": 0.05,
        "Noise": 0.05,
    }
    counts = {key: int(note_count * weight) for key, weight in weights.items()}
    while sum(counts.values()) < note_count:
        for key in weights:
            counts[key] += 1
            if sum(counts.values()) == note_count:
                break
    return counts


def generate_vault(
    output: Path,
    size: str,
    note_count: int | None,
    seed: int,
    cases_path: Path = DEFAULT_CASES_PATH,
) -> None:
    total = note_count if note_count is not None else DEFAULT_SIZES[size]
    rng = random.Random(seed)
    case_config = load_cases(cases_path)
    vault = output / "vault"
    if vault.exists():
        shutil.rmtree(vault)
    vault.mkdir(parents=True)

    counts = vault_mix(total)
    people = case_config["people"]
    generated_people: list[dict[str, Any]] = []
    projects: list[dict[str, Any]] = []
    meetings: list[dict[str, Any]] = []
    generated_tasks: list[dict[str, Any]] = []

    for idx, person in enumerate(people):
        path = Path("People") / f"{person}.md"
        note(
            vault / path,
            {"type": "person", "aliases": [person.lower()], "score": idx + 1},
            f"# {person}\n\nContact note for [[{person}]].\n",
        )
        generated_people.append(
            {"path": str(path), "type": "person", "name": person, "aliases": [person.lower()], "score": idx + 1}
        )

    anchor_projects = indexed_by(case_config["anchor_projects"])
    project_count = max(4, counts["Projects"], max(anchor_projects, default=-1) + 1)
    for i in range(project_count):
        if i in anchor_projects:
            anchor = anchor_projects[i]
            name = anchor["name"]
            active = anchor["active"]
            score = anchor["score"]
            status = anchor["status"]
            owner = anchor["owner"]
            priority = anchor["priority"]
            risk = anchor.get("risk", rng.choice(["low", "medium", "high"]))
        else:
            name = rng.choice(["Atlas", "Beacon", "Cipher", "Delta", "Echo", "Flux"])
            active = rng.random() < 0.55
            score = rng.randint(1, 10)
            status = "active" if active else rng.choice(["paused", "archived", "stale"])
            owner = rng.choice(people)
            priority = rng.choice(["low", "medium", "high"])
            risk = rng.choice(["low", "medium", "high"])
        slug = f"{name}-{i:03d}"
        path = Path("Projects") / f"{slug}.md"
        due = anchor_projects.get(i, {}).get("due", f"2026-07-{1 + i % 28:02d}")
        body = f"""# {slug}

Owner: [[{owner}]]

- [ ] Draft milestone review [priority::{priority}] [due::{due}]
- [{' ' if i != 13 else 'x'}] Archive stale brief [priority::low] [due::2026-08-01]

The current status is {status}. Similar names include Alicia and Alice-Old.
"""
        fm = {
            "type": "project",
            "project": slug,
            "active": active,
            "status": status,
            "score": score,
            "owner": owner,
            "priority": priority,
            "tags": ["project", priority],
            "nested": {"risk": risk, "seed": seed},
        }
        note(vault / path, fm, body)
        projects.append({"path": str(path), "slug": slug, **fm, "due": due})

    decision_case = case_config["decision_meeting"]
    meeting_count = max(2, counts["Meetings"], decision_case["index"] + 1)
    for i in range(meeting_count):
        special = i == decision_case["index"]
        date = decision_case["date"] if special else f"2026-06-{1 + i % 28:02d}"
        title = decision_case["title"] if special else rng.choice(["planning", "sync", "review"])
        path = Path("Meetings") / f"{date}-{title}-{i:03d}.md"
        project = projects[i % len(projects)]
        conclusion = (
            decision_case["conclusion"]
            if special
            else "The team recorded routine project updates and deferred benchmark decisions."
        )
        body = f"""# {date} {title}

Participants: [[Alice]], [[Bob]]
Project: [[{project['slug']}]]

## Conclusion

{conclusion}

Japanese note: ベンチマーク設計の議論を記録する。
Unresolved reference: [[Future-Missing-{i}]]
"""
        note(
            vault / path,
            {
                "type": "meeting",
                "date": date,
                "topic": title,
                "project": project["slug"],
                "decision": special,
                "has_unresolved_link": True,
            },
            body,
        )
        meetings.append(
            {
                "path": str(path),
                "date": date,
                "topic": title,
                "project": project["slug"],
                "conclusion": conclusion,
                "decision": special,
                "has_unresolved_link": True,
            }
        )

    for folder in ("Daily", "Research", "Tasks", "Noise"):
        for i in range(max(0, counts[folder])):
            path = Path(folder) / f"{folder.lower()}-{i:04d}.md"
            owner = rng.choice(people)
            project = projects[stable_int(seed, f"{folder}:{i}") % len(projects)]
            body = f"""# {folder} {i}

Related: [[{project['slug']}]] and [[{owner}]]

- [ ] Follow up with {owner} [priority::{rng.choice(['low', 'medium', 'high'])}] [due::2026-07-{1 + i % 28:02d}]

This note contains realistic noise, stale statements, and links.
"""
            fm = {
                "type": folder.lower(),
                "created": f"2026-05-{1 + i % 28:02d}",
                "score": rng.randint(1, 10),
                "active": rng.random() < 0.5,
                "tags": [folder.lower(), rng.choice(["research", "paper", "ops"])],
                "owner": owner,
                "project": project["slug"],
            }
            note(vault / path, fm, body)
            generated_tasks.append({"path": str(path), **fm})

    tasks = build_tasks(
        case_config,
        {"projects": projects, "meetings": meetings, "tasks": generated_tasks, "people": generated_people},
    )

    with (output / "tasks.jsonl").open("w", encoding="utf-8") as handle:
        for task in tasks:
            handle.write(json.dumps(task, ensure_ascii=False, separators=(",", ":")) + "\n")
    write_json(
        output / "oracle.json",
        {
            "seed": seed,
            "size": size,
            "note_count": total,
            "cases": str(cases_path),
            "tasks": {task["id"]: task for task in tasks},
        },
    )
