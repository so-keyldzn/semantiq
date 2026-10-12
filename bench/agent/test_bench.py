"""Unit tests of the benchmark harness (no claude call, no checkout):
`python3 bench/agent/test_bench.py` or `python3 -m unittest bench/agent/test_bench.py`."""

from __future__ import annotations

import copy
import json
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import run  # noqa: E402
import scoring  # noqa: E402

TASKS_DIR = Path(__file__).resolve().parent / "tasks"

SUITE = {
    "repo": {"name": "demo", "source": "self", "commit": "0" * 40},
    "tasks": [
        {
            "id": "t1",
            "category": "structural",
            "answer_kind": "symbols",
            "prompt": "Who calls foo?",
            "expected": [{"id": "foo", "any": ["\\bfoo\\b"]}, {"id": "bar", "any": ["\\bbar\\b"]}],
            "allowed": ["\\bbaz\\b"],
            "min_recall": 0.5,
        }
    ],
}


def mutated(**changes) -> dict:
    """SUITE with fields of its task replaced (a value of `None` removes the field)."""
    suite = copy.deepcopy(SUITE)
    for key, value in changes.items():
        if value is None:
            suite["tasks"][0].pop(key, None)
        else:
            suite["tasks"][0][key] = value
    return suite


class ValidateSuite(unittest.TestCase):
    def test_shipped_suites_are_valid(self):
        for path in sorted(TASKS_DIR.glob("*.json")):
            scoring.validate_suite(json.loads(path.read_text()), str(path))

    def test_valid_suite_passes(self):
        scoring.validate_suite(SUITE, "demo.json")
        scoring.validate_suite(mutated(allowed=None, min_recall=None), "demo.json")

    def test_rejections_name_the_file_and_task(self):
        cases = [
            (mutated(expected=[]), "task t1: `expected` must be a non-empty list"),
            (mutated(expected=None), "task t1: `expected` must be a non-empty list"),
            (mutated(expected=[{"id": "x", "any": []}]), "expected #1: `any` must be a non-empty list"),
            (mutated(expected=[{"id": "x", "any": ["("]}]), "invalid regex '('"),
            (mutated(expected=[{"id": "x", "any": ["a"]}, {"id": "x", "any": ["b"]}]), "duplicate expected id 'x'"),
            (mutated(answer_kind="paths"), "`answer_kind` must be one of files, symbols"),
            (mutated(category="other"), "`category` must be one of"),
            (mutated(prompt=""), "`prompt` must be a non-empty string"),
            (mutated(allowed=["["]), "`allowed`: invalid regex '['"),
            (mutated(min_recall=1.5), "`min_recall` must be a number between 0 and 1"),
            (mutated(min_precision="0.5"), "`min_precision` must be a number"),
            (mutated(id=""), "task #1: `id` must be a non-empty string"),
        ]
        for suite, message in cases:
            with self.assertRaises(scoring.SuiteError, msg=message) as ctx:
                scoring.validate_suite(suite, "demo.json")
            self.assertIn("demo.json", str(ctx.exception))
            self.assertIn(message, str(ctx.exception))

        duplicate = copy.deepcopy(SUITE)
        duplicate["tasks"].append(copy.deepcopy(SUITE["tasks"][0]))
        with self.assertRaises(scoring.SuiteError) as ctx:
            scoring.validate_suite(duplicate, "demo.json")
        self.assertIn("task t1: duplicate id", str(ctx.exception))

        for broken, message in [
            ({"tasks": SUITE["tasks"]}, "`repo` must be an object"),
            ({"repo": {"name": "x", "source": "self"}, "tasks": SUITE["tasks"]}, "`repo.commit`"),
            ({"repo": SUITE["repo"], "tasks": []}, "`tasks` must be a non-empty list"),
            ([], "must be a JSON object"),
        ]:
            with self.assertRaises(scoring.SuiteError) as ctx:
                scoring.validate_suite(broken, "demo.json")
            self.assertIn(message, str(ctx.exception))

    def test_load_suites_rejects_a_bad_file_at_load_time(self):
        with tempfile.TemporaryDirectory() as tmp:
            bad = Path(tmp) / "bad.json"
            bad.write_text(json.dumps(mutated(expected=[])))
            with self.assertRaises(SystemExit) as ctx:
                run.load_suites([bad])
            self.assertIn("bad.json: task t1: `expected` must be a non-empty list", str(ctx.exception))

            invalid = Path(tmp) / "invalid.json"
            invalid.write_text("{")
            with self.assertRaises(SystemExit) as ctx:
                run.load_suites([invalid])
            self.assertIn("invalid.json: invalid JSON", str(ctx.exception))

            good = Path(tmp) / "good.json"
            good.write_text(json.dumps(SUITE))
            suites = run.load_suites([good])
            self.assertEqual(suites[0]["file"], str(good))
            self.assertEqual(suites[0]["tasks"][0]["id"], "t1")


class Score(unittest.TestCase):
    def test_scores_a_final_line(self):
        task = SUITE["tasks"][0]
        result = scoring.score(task, 'Some text.\nFINAL: ["foo()", "baz", "qux"]')
        self.assertTrue(result["final_found"])
        self.assertEqual(result["matched"], ["foo"])
        self.assertEqual(result["missed"], ["bar"])
        self.assertEqual(result["extra"], ["qux"])
        self.assertEqual(result["recall"], 0.5)
        self.assertAlmostEqual(result["precision"], 2 / 3, places=3)
        self.assertTrue(result["success"], "recall 0.5 >= min_recall 0.5, precision > 0.5")

    def test_missing_final_line_fails_with_diagnostic_recall(self):
        result = scoring.score(SUITE["tasks"][0], "It is foo and bar, I think.")
        self.assertFalse(result["final_found"])
        self.assertEqual(result["recall"], 1.0)
        self.assertIsNone(result["precision"])
        self.assertFalse(result["success"])

    def test_empty_expected_is_an_error_not_a_crash(self):
        task = dict(SUITE["tasks"][0], expected=[])
        with self.assertRaises(scoring.SuiteError):
            scoring.score(task, 'FINAL: ["foo"]')
        with self.assertRaises(scoring.SuiteError):
            scoring.score(task, "no final line")


if __name__ == "__main__":
    unittest.main()
