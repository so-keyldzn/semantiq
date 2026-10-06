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
