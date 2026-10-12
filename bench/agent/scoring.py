"""Score an agent answer against a task's expected files/symbols.

The agent is asked to end its reply with `FINAL: [...]` (a JSON array). Each
entry is normalised then matched (case-insensitive regex search) against the
task's `expected` items and `allowed` patterns:

  recall    = expected items matched by at least one entry / expected items
  precision = entries matching an expected item or an allowed pattern / entries
  success   = recall >= min_recall (default 1.0) and precision >= min_precision (default 0.5)

An answer without a parsable FINAL line fails; its recall is still computed on
the whole text for diagnosis, with precision left as None.
"""

from __future__ import annotations

import json
import re
from pathlib import Path

FINAL_RE = re.compile(r"^[\s>*_`#-]*FINAL\s*:?[\s*_`]*(\[.*\])", re.IGNORECASE)
LINE_SUFFIX_RE = re.compile(r"(:\d+(-\d+)?)+$|#L\d+(-L?\d+)?$")

CATEGORIES = ("conceptual", "structural", "exact")
ANSWER_KINDS = ("files", "symbols")


class SuiteError(ValueError):
    """A task suite that cannot be scored: reported at load time, with the file
    and task id, instead of crashing `score()` after a paid run."""


def _check(condition: bool, where: str, message: str) -> None:
    if not condition:
        raise SuiteError(f"{where}: {message}")


def _check_patterns(patterns, where: str) -> None:
    _check(isinstance(patterns, list) and all(isinstance(p, str) for p in patterns),
           where, "must be a list of regex strings")
    for p in patterns:
        try:
            re.compile(p)
        except re.error as e:
            raise SuiteError(f"{where}: invalid regex {p!r} ({e})") from None


def validate_suite(suite: dict, source: str = "suite") -> None:
    """Check the shape `score()` and the runner rely on: a `repo` block, a
    non-empty `tasks` list with unique ids, a known `category` and
    `answer_kind`, a non-empty `expected` list of `{id, any: [regex...]}`,
    valid `allowed` regexes and thresholds in [0, 1]. Raises `SuiteError`."""
    _check(isinstance(suite, dict), source, "must be a JSON object")
    repo = suite.get("repo")
    _check(isinstance(repo, dict), source, "`repo` must be an object")
    for key in ("name", "source", "commit"):
        _check(isinstance(repo.get(key), str) and repo[key], source, f"`repo.{key}` must be a non-empty string")
    tasks = suite.get("tasks")
    _check(isinstance(tasks, list) and tasks, source, "`tasks` must be a non-empty list")
    seen: set[str] = set()
    for i, task in enumerate(tasks):
        where = f"{source}: task #{i + 1}"
        _check(isinstance(task, dict), where, "must be an object")
        task_id = task.get("id")
        _check(isinstance(task_id, str) and task_id, where, "`id` must be a non-empty string")
        where = f"{source}: task {task_id}"
        _check(task_id not in seen, where, "duplicate id")
        seen.add(task_id)
        _check(task.get("category") in CATEGORIES, where,
               f"`category` must be one of {', '.join(CATEGORIES)}")
        _check(task.get("answer_kind") in ANSWER_KINDS, where,
               f"`answer_kind` must be one of {', '.join(ANSWER_KINDS)}")
        _check(isinstance(task.get("prompt"), str) and task["prompt"].strip(), where,
               "`prompt` must be a non-empty string")
        expected = task.get("expected")
        _check(isinstance(expected, list) and expected, where,
               "`expected` must be a non-empty list (recall divides by its length)")
        expected_ids: set[str] = set()
        for j, item in enumerate(expected):
            item_where = f"{where}: expected #{j + 1}"
            _check(isinstance(item, dict), item_where, "must be an object")
            _check(isinstance(item.get("id"), str) and item["id"], item_where,
                   "`id` must be a non-empty string")
            _check(item["id"] not in expected_ids, item_where, f"duplicate expected id {item['id']!r}")
            expected_ids.add(item["id"])
            _check(isinstance(item.get("any"), list) and item["any"], item_where,
                   "`any` must be a non-empty list of regexes")
            _check_patterns(item["any"], f"{item_where} `any`")
        _check_patterns(task.get("allowed", []), f"{where} `allowed`")
        for key in ("min_recall", "min_precision"):
            if key in task:
                value = task[key]
                _check(isinstance(value, (int, float)) and not isinstance(value, bool)
                       and 0 <= value <= 1, where, f"`{key}` must be a number between 0 and 1")


def extract_final(text: str) -> list[str] | None:
    for line in reversed((text or "").splitlines()):
        m = FINAL_RE.match(line)
        if not m:
            continue
        raw = m.group(1).strip().rstrip("`").strip()
        try:
            items = json.loads(raw)
        except json.JSONDecodeError:
            items = re.findall(r'"([^"]+)"', raw)
        if isinstance(items, list):
            return [str(i) for i in items]
    return None


def normalise(entry: str, kind: str, checkout: Path | None) -> str:
    e = entry.strip().strip("`'\" ")
    if kind == "files":
        if checkout is not None:
            for prefix in (str(checkout), str(checkout.resolve())):
                if e.startswith(prefix):
                    e = e[len(prefix):]
        e = re.sub(r"^(\./|/)+", "", LINE_SUFFIX_RE.sub("", e)).strip()
    else:
        e = re.sub(r"\(.*\)$", "", e).strip()  # `foo()` -> `foo`
    return e


def _matches(patterns, entry: str) -> bool:
    return any(re.search(p, entry, re.IGNORECASE) for p in patterns)


def score(task: dict, answer: str, checkout: Path | None = None) -> dict:
    kind = task["answer_kind"]
    expected = task["expected"]
    if not expected:
        raise SuiteError(f"task {task.get('id')!r}: `expected` is empty, recall is undefined")
    allowed = task.get("allowed", [])
    min_recall = task.get("min_recall", 1.0)
    min_precision = task.get("min_precision", 0.5)

    final = extract_final(answer)
    if final is None:
        hit = [e["id"] for e in expected if _matches(e["any"], answer or "")]
        return {
            "final_found": False,
            "entries": [],
            "matched": hit,
            "missed": [e["id"] for e in expected if e["id"] not in hit],
            "extra": [],
            "recall": len(hit) / len(expected),
            "precision": None,
            "success": False,
        }

    entries = list(dict.fromkeys(normalise(e, kind, checkout) for e in final if e.strip()))
    matched = [e["id"] for e in expected if any(_matches(e["any"], x) for x in entries)]
    good = [x for x in entries if any(_matches(e["any"], x) for e in expected) or _matches(allowed, x)]
    extra = [x for x in entries if x not in good]
    recall = len(matched) / len(expected)
    precision = len(good) / len(entries) if entries else 0.0
    return {
        "final_found": True,
        "entries": entries,
        "matched": matched,
        "missed": [e["id"] for e in expected if e["id"] not in matched],
        "extra": extra,
        "recall": recall,
        "precision": round(precision, 3),
        "success": recall >= min_recall - 1e-9 and precision >= min_precision - 1e-9,
    }
