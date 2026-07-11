import json
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

import evaluator
import agent_utility
import fixture_generator


class AgentUtilityTests(unittest.TestCase):
    def test_generate_vault_writes_tasks_and_oracle(self):
        with tempfile.TemporaryDirectory() as tmp:
            output = Path(tmp) / "fixture"
            fixture_generator.generate_vault(output, "medium", 1_000, 7)
            case_config = fixture_generator.load_cases()

            self.assertTrue((output / "tasks.jsonl").exists())
            oracle = json.loads((output / "oracle.json").read_text(encoding="utf-8"))

            tasks = [json.loads(line) for line in (output / "tasks.jsonl").read_text(encoding="utf-8").splitlines()]
            self.assertEqual({case["id"] for case in case_config["tasks"]}, {task["id"] for task in tasks})
            self.assertEqual(1_000, oracle["note_count"])
            self.assertGreaterEqual(len(tasks), 7)
            for task in tasks:
                self.assertTrue(task["expected_paths"], task["id"])

    def test_score_requires_expected_paths_and_reasoning_answer(self):
        with tempfile.TemporaryDirectory() as tmp:
            output = Path(tmp) / "fixture"
            fixture_generator.generate_vault(output, "small", 120, 7)
            tasks = [json.loads(line) for line in (output / "tasks.jsonl").read_text(encoding="utf-8").splitlines()]
            task = next(item for item in tasks if item["type"] == "reasoning")
        answer = {"answer": [task["expected_answer"]], "evidence": [f"{task['expected_paths'][0]}#Conclusion"]}

        score = evaluator.score_answer(task, answer)

        self.assertEqual(1.0, score["exact_accuracy"])
        self.assertEqual(1.0, score["partial_accuracy"])

    def test_parse_codex_jsonl_extracts_usage_commands_and_output_bytes(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "codex.jsonl"
            events = [
                {
                    "type": "item.completed",
                    "item": {
                        "type": "command_execution",
                        "command": "/bin/zsh -lc rg Alice vault",
                        "aggregated_output": "Projects/example.md:Alice\n",
                        "exit_code": 0,
                    },
                },
                {"type": "item.completed", "item": {"type": "agent_message", "text": "{\"answer\":[],\"evidence\":[]}"}},
                {
                    "type": "turn.completed",
                    "usage": {
                        "input_tokens": 100,
                        "cached_input_tokens": 40,
                        "output_tokens": 10,
                        "reasoning_output_tokens": 3,
                    },
                },
            ]
            path.write_text("\n".join(json.dumps(event) for event in events), encoding="utf-8")

            metrics = evaluator.parse_codex_jsonl(path)

            self.assertEqual(1, metrics["shell_command_count"])
            self.assertEqual(1, metrics["command_counts"]["rg"])
            self.assertEqual(110, metrics["total_tokens"])
            self.assertEqual(60, metrics["non_cached_input_tokens"])
            self.assertEqual(len("Projects/example.md:Alice\n".encode("utf-8")), metrics["agent_visible_output_bytes"])

    def test_documented_prompt_mentions_jq_for_json_shaping(self):
        prompt = agent_utility.prompt_for(
            "mdq-context-only-documented",
            Path("/tmp/vault"),
            "Which notes match?",
            "JSON output schemas are type-generated from Rust Serialize DTOs:\n  backlinks: [{source, target}]",
        )

        self.assertIn("piped to `jq`", prompt)
        self.assertIn("mdq backlinks People/Alice.md --json | jq", prompt)
        self.assertIn("mdq manual json-schemas", prompt)
        self.assertIn("type-generated from Rust Serialize DTOs", prompt)
        self.assertIn("backlinks: [{source, target}]", prompt)
        self.assertIn("Do not inspect Markdown files directly", prompt)

    def test_extract_json_object_accepts_fenced_output(self):
        text = '```json\n{"answer":["Projects/A.md"],"evidence":[]}\n```'

        extracted = agent_utility.extract_json_object(text)

        self.assertEqual({"answer": ["Projects/A.md"], "evidence": []}, json.loads(extracted))


if __name__ == "__main__":
    unittest.main()
