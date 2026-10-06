#!/usr/bin/env python3
"""Agent benchmark: Claude Code with vs without the Semantiq MCP server.

For every (task, configuration, repetition) the harness runs Claude Code
headless (`claude -p ... --output-format stream-json --verbose`) inside a
throw-away checkout of the target repository and records tokens, cost, tool
calls, duration and the final answer, which is then scored against the task's
expected files/symbols.

Configurations:
  baseline         no MCP server at all (--strict-mcp-config + empty --mcp-config)
  semantiq         only the semantiq MCP server (index built before the runs)
  semantiq-guided  (optional) semantiq + the CLAUDE.md guidance written by
                   `semantiq init`, passed via --append-system-prompt. It
                   changes the system prompt, so it is not part of the
                   default A/B comparison.
  cli-skill        no MCP server; the semantiq skill (--skill-path) is
                   installed in the checkout's .claude/skills/ and the
                   semantiq CLI (--cli-bin) is on PATH, called through Bash.
                   Project skills need the Skill tool and project settings,
                   so this config also lists Claude Code's bundled skills.

Both configurations share the model, the prompt, the default system prompt and
the same read-only native tools (Read, Grep, Glob and an allowlist of
read-only Bash commands). Nothing runs with --dangerously-skip-permissions:
anything outside the allowlist is denied (--permission-mode dontAsk).

Python 3 standard library only.
"""

from __future__ import annotations

import argparse
import concurrent.futures
import datetime as dt
import hashlib
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import threading
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import report  # noqa: E402
import scoring  # noqa: E402

BENCH_DIR = Path(__file__).resolve().parent
REPO_ROOT = BENCH_DIR.parent.parent
DEFAULT_TASKS = sorted((BENCH_DIR / "tasks").glob("*.json"))
DEFAULT_MODEL = "claude-sonnet-5-5"
CONFIGS = ("baseline", "semantiq", "semantiq-guided", "cli-skill")
DEFAULT_CONFIGS = "baseline,semantiq"

NATIVE_TOOLS = "Read,Grep,Glob,Bash"
# Read-only allowlist. Everything else is denied by --permission-mode dontAsk.
READ_ONLY_ALLOW = [
    "Read",
    "Grep",
    "Glob",
    "Bash(grep:*)",
    "Bash(rg:*)",
    "Bash(find:*)",
    "Bash(ls:*)",
    "Bash(cat:*)",
    "Bash(head:*)",
    "Bash(tail:*)",
    "Bash(wc:*)",
    "Bash(sed -n:*)",
    "Bash(awk:*)",
    "Bash(sort:*)",
    "Bash(uniq:*)",
    "Bash(tree:*)",
    "Bash(file:*)",
]
SEMANTIQ_ALLOW = ["mcp__semantiq"]
CLI_SKILL_ALLOW = ["Skill", "Bash(semantiq:*)", "Bash(jq:*)"]

# Files that hand the agent a map of the code base (identical in both configs,
# but they answer several questions verbatim and blur the comparison).
AGENT_DOCS = ["CLAUDE.md", "AGENTS.md", ".claude", ".cursor", ".mcp.json", ".semantiq.db"]

PROMPT_TEMPLATE = """You are answering a question about the code base in the current directory. \
Do not modify any file.

Question: {question}

Answer concisely. On the last line of your reply, write `FINAL:` followed by a JSON array of \
strings containing {kind}, with nothing else on that line."""

KIND_TEXT = {
    "files": 'the repo-relative paths of the source files that answer the question '
    '(e.g. ["src/foo.rs"])',
    "symbols": 'the names of the functions, methods or types that answer the question '
    '(e.g. ["Type::method", "free_function"])',
}

# Rough per-run cost used only for the --dry-run estimate (USD; the 2026-10-06
# pilot averaged ~0.025 with Sonnet).
EST_COST_PER_RUN = 0.05

_print_lock = threading.Lock()


def log(msg: str) -> None:
    with _print_lock:
        print(msg, file=sys.stderr, flush=True)


# --------------------------------------------------------------------------
# Task loading
# --------------------------------------------------------------------------


def load_suites(paths: list[Path]) -> list[dict]:
    suites = []
    for p in paths:
        data = json.loads(p.read_text())
        data["file"] = str(p)
        suites.append(data)
    return suites


def select_runs(suites, task_ids, categories, configs, reps):
    runs = []
    for rep in range(1, reps + 1):
        for suite in suites:
            for task in suite["tasks"]:
                if task_ids and task["id"] not in task_ids:
                    continue
                if categories and task["category"] not in categories:
                    continue
                for config in configs:
                    runs.append({"repo": suite["repo"], "task": task, "config": config, "rep": rep})
    return runs


def run_key(repo: str, task_id: str, config: str, rep: int) -> str:
    return f"{repo}/{task_id}/{config}/{rep}"


# --------------------------------------------------------------------------
# Checkouts and index
# --------------------------------------------------------------------------


def sh(cmd: list[str], **kw) -> subprocess.CompletedProcess:
    return subprocess.run(cmd, check=True, text=True, capture_output=True, **kw)


def prepare_checkout(
    repo: dict, work: Path, keep_agent_docs: bool, skill_dir: Path | None = None
) -> Path:
    """Export `repo.commit` into work/checkouts/<name>[-cli-skill] (no .git, agent docs
    stripped), then install `skill_dir` as .claude/skills/semantiq when given."""
    name, commit = repo["name"], repo["commit"]
    dest = work / "checkouts" / (f"{name}-cli-skill" if skill_dir else name)
    marker = dest / ".bench-commit"
    if not (dest.exists() and marker.exists() and marker.read_text().strip() == commit):
        _export(repo, work, dest, keep_agent_docs)
    if skill_dir:
        target = dest / ".claude" / "skills" / "semantiq"
        if target.exists():
            shutil.rmtree(target)
        shutil.copytree(skill_dir, target)
    return dest


def _export(repo: dict, work: Path, dest: Path, keep_agent_docs: bool) -> None:
    name, commit = repo["name"], repo["commit"]
    marker = dest / ".bench-commit"
    if dest.exists():
        shutil.rmtree(dest)

    if repo["source"] == "self":
        git_dir = REPO_ROOT
    else:
        git_dir = work / "sources" / name
        if not (git_dir / ".git").exists():
            git_dir.mkdir(parents=True, exist_ok=True)
            sh(["git", "init", "-q"], cwd=git_dir)
            sh(["git", "remote", "add", "origin", repo["source"]], cwd=git_dir)
        has = subprocess.run(
            ["git", "cat-file", "-e", f"{commit}^{{commit}}"], cwd=git_dir, capture_output=True
        )
        if has.returncode != 0:
            log(f"[checkout] fetching {repo['source']} @ {commit[:12]}")
            sh(["git", "fetch", "-q", "--depth", "1", "origin", commit], cwd=git_dir)

    dest.mkdir(parents=True)
    archive = subprocess.Popen(["git", "archive", commit], cwd=git_dir, stdout=subprocess.PIPE)
    subprocess.run(["tar", "-x", "-C", str(dest)], stdin=archive.stdout, check=True)
    if archive.wait() != 0:
        raise RuntimeError(f"git archive {commit} failed in {git_dir}")

    if not keep_agent_docs:
        for rel in AGENT_DOCS:
            for p in dest.rglob(rel):
                if p.is_dir():
                    shutil.rmtree(p)
                else:
                    p.unlink()
    marker.write_text(commit + "\n")


def build_index(bin_path: Path, checkout: Path, db: Path, reuse: bool) -> dict:
    if db.exists() and reuse:
        return {"seconds": None, "reused": True, "db_bytes": db.stat().st_size}
    for suffix in ("", "-wal", "-shm"):
        Path(str(db) + suffix).unlink(missing_ok=True)
    db.parent.mkdir(parents=True, exist_ok=True)
    env = dict(os.environ, SEMANTIQ_UPDATE_CHECK="0")
    start = time.monotonic()
    proc = subprocess.run(
        [str(bin_path), "index", str(checkout), "--database", str(db), "--force"],
        env=env,
        text=True,
        capture_output=True,
    )
    seconds = time.monotonic() - start
    if proc.returncode != 0:
        raise RuntimeError(f"semantiq index failed:\n{proc.stderr[-2000:]}")
    stats = subprocess.run(
        [str(bin_path), "stats", "--database", str(db)], env=env, text=True, capture_output=True
    )
    return {
        "seconds": round(seconds, 1),
        "reused": False,
        "db_bytes": db.stat().st_size,
        "stats": stats.stdout.strip(),
    }


def copy_db(src: Path, dest: Path) -> None:
    """Consistent copy of a SQLite index (WAL included) through the backup API."""
    import sqlite3

    source = sqlite3.connect(f"file:{src}?mode=ro", uri=True)
    target = sqlite3.connect(dest)
    try:
        source.backup(target)
    finally:
        target.close()
        source.close()


def write_mcp_configs(work: Path, bin_path: Path, checkout: Path, db: Path, name: str):
    mcp_dir = work / "mcp"
    mcp_dir.mkdir(parents=True, exist_ok=True)
    empty = mcp_dir / "empty.json"
    empty.write_text(json.dumps({"mcpServers": {}}))
    semantiq = mcp_dir / f"{name}-semantiq.json"
    semantiq.write_text(
        json.dumps(
            {
                "mcpServers": {
                    "semantiq": {
                        "command": str(bin_path),
                        "args": [
                            "serve",
                            "--project",
                            str(checkout),
                            "--database",
                            str(db),
                            "--no-update-check",
                        ],
                        "env": {"SEMANTIQ_UPDATE_CHECK": "0"},
                    }
                }
            },
            indent=2,
        )
    )
    return {"baseline": empty, "semantiq": semantiq, "semantiq-guided": semantiq,
            "cli-skill": empty}


# --------------------------------------------------------------------------
# One agent run
# --------------------------------------------------------------------------


def build_prompt(task: dict) -> str:
    return PROMPT_TEMPLATE.format(question=task["prompt"], kind=KIND_TEXT[task["answer_kind"]])


def uses_semantiq(config: str) -> bool:
    """Configs that talk to the semantiq MCP server."""
    return config.startswith("semantiq")


_guidance: dict[str, str] = {}
_guidance_lock = threading.Lock()


def init_guidance(bin_path: Path) -> str:
    """The CLAUDE.md block `semantiq init --no-skill` (MCP, no skill) writes, produced by
    the binary under test in a throw-away directory so it matches its version.

    Since 0.10 the block is built in code (`claude_md_block` in init.rs), which the
    former regex over a `claude_md_content` literal no longer finds; that path is kept
    for older binaries."""
    with _guidance_lock:
        if "text" in _guidance:
            return _guidance["text"]
        with tempfile.TemporaryDirectory() as tmp:
            sh(["git", "init", "-q"], cwd=tmp)
            env = dict(os.environ, SEMANTIQ_UPDATE_CHECK="0")
            proc = subprocess.run([str(bin_path), "init", "--no-skill", "--no-index"], cwd=tmp,
                                  env=env, text=True, capture_output=True)
            claude_md = Path(tmp) / "CLAUDE.md"
            text = claude_md.read_text() if proc.returncode == 0 and claude_md.exists() else ""
        m = re.search(r"<!-- semantiq:start -->\n(.*?)<!-- semantiq:end -->", text, re.DOTALL)
        if m:
            _guidance["text"] = m.group(1)
        else:
            src = (REPO_ROOT / "crates/semantiq/src/commands/init.rs").read_text()
            m = re.search(r'let claude_md_content = r#"(.*?)"#;', src, re.DOTALL)
            if not m:
                raise RuntimeError("could not obtain the CLAUDE.md guidance of semantiq init")
            _guidance["text"] = m.group(1)
        return _guidance["text"]


def claude_command(args, prompt: str, mcp_config: Path, config: str) -> list[str]:
    allow = READ_ONLY_ALLOW + (SEMANTIQ_ALLOW if uses_semantiq(config) else [])
    if config == "cli-skill":
        allow = READ_ONLY_ALLOW + CLI_SKILL_ALLOW
    skill = config == "cli-skill"
    cmd = [
        args.claude_bin,
        "-p",
        prompt,
        "--output-format",
        "stream-json",
        "--verbose",
        "--model",
        args.model,
        "--strict-mcp-config",
        "--mcp-config",
        str(mcp_config),
        "--tools",
        NATIVE_TOOLS + (",Skill" if skill else ""),
        "--allowedTools",
        ",".join(allow),
        "--permission-mode",
        "dontAsk",
        "--setting-sources",
        "project" if skill else "",
        "--no-session-persistence",
    ]
    if not skill:
        cmd.append("--disable-slash-commands")
    if config == "semantiq-guided":
        cmd += ["--append-system-prompt", init_guidance(args.semantiq_bin)]
    if args.max_budget_usd:
        cmd += ["--max-budget-usd", str(args.max_budget_usd)]
    if args.effort:
        cmd += ["--effort", args.effort]
    return cmd


def parse_transcript(path: Path) -> dict:
    tool_calls: dict[str, dict] = {}
    tool_errors = 0
    init: dict = {}
    result: dict = {}
    texts: list[str] = []
    first_context = None
    for line in path.read_text(errors="replace").splitlines():
        try:
            ev = json.loads(line)
        except json.JSONDecodeError:
            continue
        t = ev.get("type")
        if t == "system" and ev.get("subtype") == "init":
            init = ev
        elif t == "assistant":
            u = ev.get("message", {}).get("usage") or {}
            if first_context is None and u:
                # Context of the first request: system prompt + tool definitions + prompt.
                first_context = (u.get("input_tokens", 0) + u.get("cache_creation_input_tokens", 0)
                                 + u.get("cache_read_input_tokens", 0))
            for block in ev.get("message", {}).get("content", []):
                if block.get("type") == "tool_use":
                    inp = json.dumps(block.get("input", {}), ensure_ascii=False)
                    tool_calls[block["id"]] = {"name": block["name"], "input": inp[:300]}
                elif block.get("type") == "text":
                    texts.append(block.get("text", ""))
        elif t == "user":
            content = ev.get("message", {}).get("content", [])
            if isinstance(content, list):
                for block in content:
                    if block.get("type") == "tool_result" and block.get("is_error"):
                        tool_errors += 1
        elif t == "result":
            result = ev

    mcp_status = {s.get("name"): s.get("status") for s in init.get("mcp_servers", [])}
    usage = result.get("usage", {}) or {}
    final_text = result.get("result")
    if not isinstance(final_text, str):
        final_text = texts[-1] if texts else ""
    return {
        "mcp_servers": mcp_status,
        "tools_available": init.get("tools", []),
        "tool_calls": list(tool_calls.values()),
        "tool_errors": tool_errors,
        "permission_denials": len(result.get("permission_denials", []) or []),
        "result_subtype": result.get("subtype"),
        "is_error": bool(result.get("is_error")) if result else True,
        "num_turns": result.get("num_turns"),
        "duration_ms": result.get("duration_ms"),
        "duration_api_ms": result.get("duration_api_ms"),
        "cost_usd": result.get("total_cost_usd"),
        "usage": {
            "input_tokens": usage.get("input_tokens", 0),
            "cache_creation_input_tokens": usage.get("cache_creation_input_tokens", 0),
            "cache_read_input_tokens": usage.get("cache_read_input_tokens", 0),
            "output_tokens": usage.get("output_tokens", 0),
        },
        "first_context_tokens": first_context,
        "final_answer": final_text,
    }


def execute_run(args, spec: dict, checkout: Path, mcp_configs: dict, transcripts: Path) -> dict:
    repo, task, config, rep = spec["repo"]["name"], spec["task"], spec["config"], spec["rep"]
    key = run_key(repo, task["id"], config, rep)
    transcript = transcripts / f"{repo}__{task['id']}__{config}__{rep}.jsonl"
    cmd = claude_command(args, build_prompt(task), mcp_configs[config], config)
    env = {k: v for k, v in os.environ.items() if k != "CLAUDECODE"}
    env["SEMANTIQ_UPDATE_CHECK"] = "0"
    if config == "cli-skill":
        env["PATH"] = f"{args.cli_path_dir}{os.pathsep}{env.get('PATH', '')}"

    log(f"[run] {key} ...")
    start = time.monotonic()
    timed_out = False
    with open(transcript, "w") as out, open(str(transcript) + ".stderr", "w") as err:
        proc = subprocess.Popen(cmd, cwd=checkout, stdout=out, stderr=err, env=env)
        try:
            proc.wait(timeout=args.timeout)
        except subprocess.TimeoutExpired:
            timed_out = True
            proc.kill()
            proc.wait()
    wall = time.monotonic() - start

    parsed = parse_transcript(transcript)
    score = scoring.score(task, parsed["final_answer"], checkout)
    usage = parsed["usage"]
    record = {
        "key": key,
        "repo": repo,
        "task_id": task["id"],
        "category": task["category"],
        "config": config,
        "rep": rep,
        "model": args.model,
        "exit_code": proc.returncode,
        "timed_out": timed_out,
        "wall_seconds": round(wall, 1),
        "tokens_in": usage["input_tokens"]
        + usage["cache_creation_input_tokens"]
        + usage["cache_read_input_tokens"],
        "tokens_out": usage["output_tokens"],
        **parsed,
        "score": score,
        "transcript": str(transcript),
    }
    if uses_semantiq(config) and parsed["mcp_servers"].get("semantiq") != "connected":
        record["invalid"] = f"semantiq MCP status: {parsed['mcp_servers'].get('semantiq')}"
    n_sq = sum(1 for c in parsed["tool_calls"] if c["name"].startswith("mcp__semantiq__"))
    log(
        f"[done] {key}: success={score['success']} recall={score['recall']:.2f} "
        f"precision={score['precision'] if score['precision'] is not None else '-'} "
        f"tools={len(parsed['tool_calls'])} (semantiq {n_sq}) "
        f"tokens_in={record['tokens_in']} cost={parsed['cost_usd']} {wall:.0f}s"
    )
    return record


# --------------------------------------------------------------------------
# Main
# --------------------------------------------------------------------------


def write_results(out: Path, meta: dict, records: list[dict], suites: list[dict]) -> None:
    # Keep only the latest record per key (a resumed run supersedes a failed one).
    latest = {}
    for rec in records:
        latest[rec["key"]] = rec
    results = {"meta": meta, "runs": sorted(latest.values(), key=lambda r: r["key"])}
    (out / "results.json").write_text(json.dumps(results, indent=2, ensure_ascii=False) + "\n")
    (out / "report.md").write_text(report.render(results, suites))
    print(f"report: {out / 'report.md'}")


def rebuild(out: Path, suites: list[dict]) -> int:
    tasks = {t["id"]: t for s in suites for t in s["tasks"]}
    meta = json.loads((out / "meta.json").read_text())
    records = []
    for line in (out / "runs.jsonl").read_text().splitlines():
        rec = json.loads(line)
        transcript = Path(rec["transcript"])
        if transcript.exists():
            rec.update(parse_transcript(transcript))
            usage = rec["usage"]
            rec["tokens_in"] = (usage["input_tokens"] + usage["cache_creation_input_tokens"]
                                + usage["cache_read_input_tokens"])
            rec["tokens_out"] = usage["output_tokens"]
        else:
            log(f"[rebuild] transcript missing, keeping stored data: {transcript}")
        checkout_name = rec["repo"] + ("-cli-skill" if rec["config"] == "cli-skill" else "")
        checkout = Path(rec["transcript"]).parents[2] / "checkouts" / checkout_name
        rec["score"] = scoring.score(tasks[rec["task_id"]], rec["final_answer"], checkout)
        records.append(rec)
    write_results(out, meta, records, suites)
    return 0


def parse_args(argv=None):
    p = argparse.ArgumentParser(
        description="Benchmark Claude Code with vs without the Semantiq MCP server.",
        formatter_class=argparse.ArgumentDefaultsHelpFormatter,
    )
    p.add_argument("--tasks", nargs="+", type=Path, default=None,
                   help="task suite JSON files, one repository each (default: bench/agent/tasks/*.json)")
    p.add_argument("--task", action="append", default=[], metavar="ID",
                   help="only run this task id (repeatable)")
    p.add_argument("--category", action="append", default=[],
                   choices=["conceptual", "structural", "exact"],
                   help="only run tasks of this category (repeatable)")
    p.add_argument("--configs", default=DEFAULT_CONFIGS,
                   help="comma-separated configurations: " + ", ".join(CONFIGS))
    p.add_argument("--model", default=DEFAULT_MODEL, help="model passed to claude --model")
    p.add_argument("--effort", default=None, help="optional claude --effort level")
    p.add_argument("--reps", type=int, default=1, help="repetitions per (task, config)")
    p.add_argument("--jobs", type=int, default=1, help="runs executed in parallel")
    p.add_argument("--timeout", type=int, default=900, help="per-run timeout in seconds")
    p.add_argument("--max-budget-usd", type=float, default=2.0,
                   help="per-run spending cap passed to claude (0 disables)")
    p.add_argument("--work-dir", type=Path,
                   default=Path(tempfile.gettempdir()) / "semantiq-agent-bench",
                   help="scratch dir for checkouts, index DBs and transcripts (outside the repo)")
    p.add_argument("--out", type=Path, default=None,
                   help="results dir (default: bench/agent/results/<YYYY-MM-DD>)")
    p.add_argument("--semantiq-bin", type=Path, default=REPO_ROOT / "target/release/semantiq",
                   help="semantiq binary (release, built with --features semantiq-embeddings/onnx)")
    p.add_argument("--cli-bin", type=Path, default=None,
                   help="semantiq CLI used by the cli-skill config (default: --semantiq-bin)")
    p.add_argument("--skill-path", type=Path, default=None,
                   help="semantiq skill (directory or SKILL.md) installed for the cli-skill config")
    p.add_argument("--build", action="store_true",
                   help="build the release binary with the onnx feature before running")
    p.add_argument("--reuse-index", action="store_true",
                   help="reuse an existing index DB instead of rebuilding (index time not measured)")
    p.add_argument("--keep-agent-docs", action="store_true",
                   help="keep CLAUDE.md/AGENTS.md/.claude in the checkouts")
    p.add_argument("--resume", action="store_true",
                   help="skip runs already recorded in <out>/runs.jsonl")
    p.add_argument("--claude-bin", default="claude", help="Claude Code executable")
    p.add_argument("--rebuild", action="store_true",
                   help="re-parse the transcripts listed in <out>/runs.jsonl, re-score them with "
                   "the current task files and rewrite results.json/report.md (no claude call)")
    p.add_argument("--dry-run", action="store_true",
                   help="list the planned runs and the cost estimate without calling claude")
    return p.parse_args(argv)


def main(argv=None) -> int:
    args = parse_args(argv)
    configs = [c.strip() for c in args.configs.split(",") if c.strip()]
    unknown = set(configs) - set(CONFIGS)
    if unknown:
        sys.exit(f"unknown config(s): {', '.join(sorted(unknown))}")
    work = args.work_dir.resolve()
    if work == REPO_ROOT or REPO_ROOT in work.parents:
        sys.exit("--work-dir must be outside the repository")

    args.tasks = args.tasks or DEFAULT_TASKS
    suites = load_suites(args.tasks)
    runs = select_runs(suites, set(args.task), set(args.category), configs, args.reps)
    if not runs:
        sys.exit("no run selected")
    out = (args.out or BENCH_DIR / "results" / dt.date.today().isoformat()).resolve()
    if args.rebuild:
        return rebuild(out, suites)

    done: dict[str, dict] = {}
    runs_log = out / "runs.jsonl"
    if args.resume and runs_log.exists():
        for line in runs_log.read_text().splitlines():
            rec = json.loads(line)
            if not rec.get("is_error") and not rec.get("invalid"):
                done[rec["key"]] = rec

    todo = [r for r in runs if run_key(r["repo"]["name"], r["task"]["id"], r["config"], r["rep"]) not in done]
    print(f"{len(runs)} planned runs ({len(todo)} to execute), model={args.model}, "
          f"configs={','.join(configs)}, reps={args.reps}")
    for r in runs:
        key = run_key(r["repo"]["name"], r["task"]["id"], r["config"], r["rep"])
        flag = " (done)" if key in done else ""
        print(f"  {key:<55} [{r['task']['category']}]{flag}")
    print(f"estimated cost: ~{len(todo) * EST_COST_PER_RUN:.2f} USD "
          f"(~{EST_COST_PER_RUN} USD/run, API-equivalent)")
    print(f"work dir: {work}\nresults:  {out}")
    if args.dry_run:
        return 0

    if args.build:
        log("[build] cargo build --release --features semantiq-embeddings/onnx")
        subprocess.run(["cargo", "build", "--release", "--features", "semantiq-embeddings/onnx"],
                       cwd=REPO_ROOT, check=True)
    any_semantiq = any(uses_semantiq(c) for c in configs)
    if any_semantiq and not args.semantiq_bin.exists():
        sys.exit(f"{args.semantiq_bin} not found (use --build)")
    skill_dir = None
    if "cli-skill" in configs:
        if not args.skill_path:
            sys.exit("the cli-skill config needs --skill-path (skill dir or its SKILL.md)")
        skill_dir = args.skill_path.resolve()
        if skill_dir.is_file():
            skill_dir = skill_dir.parent
        if not (skill_dir / "SKILL.md").exists():
            sys.exit(f"no SKILL.md in {skill_dir}")
        args.cli_bin = (args.cli_bin or args.semantiq_bin).resolve()
        if not args.cli_bin.exists():
            sys.exit(f"{args.cli_bin} not found (--cli-bin)")
        # Expose the CLI as plain `semantiq` on PATH, whatever its file name.
        args.cli_path_dir = work / "cli-bin"
        args.cli_path_dir.mkdir(parents=True, exist_ok=True)
        link = args.cli_path_dir / "semantiq"
        link.unlink(missing_ok=True)
        link.symlink_to(args.cli_bin)

    out.mkdir(parents=True, exist_ok=True)
    transcripts = work / "transcripts" / out.name
    transcripts.mkdir(parents=True, exist_ok=True)

    repos = {s["repo"]["name"]: s["repo"] for s in suites}
    used = {r["repo"]["name"] for r in todo}
    checkouts, mcp_configs, index_info = {}, {}, {}
    for name in sorted(used):
        repo = repos[name]
        plain = prepare_checkout(repo, work, args.keep_agent_docs)
        db = work / "index" / f"{name}.db"
        if any_semantiq:
            log(f"[index] {name}: indexing {plain}")
            index_info[name] = build_index(args.semantiq_bin, plain, db, args.reuse_index)
            log(f"[index] {name}: {index_info[name]['seconds']} s")
        mcp_configs[name] = write_mcp_configs(work, args.semantiq_bin, plain, db, name)
        for c in configs:
            checkouts[(name, c)] = plain
        if skill_dir:
            skilled = prepare_checkout(repo, work, args.keep_agent_docs, skill_dir)
            key = f"{name} (cli-skill)"
            # The CLI looks for .semantiq.db in the working directory or a parent.
            skilled_db = skilled / ".semantiq.db"
            if (any_semantiq and db.exists()
                    and args.cli_bin.resolve() == args.semantiq_bin.resolve()):
                # Same binary, same files (the skill is Markdown, which is not
                # indexed) and paths stored relative to the project root: copy
                # the index instead of rebuilding it, which takes tens of
                # minutes on a large repository.
                log(f"[index] {key}: copying {db}")
                for suffix in ("-wal", "-shm"):
                    Path(str(skilled_db) + suffix).unlink(missing_ok=True)
                copy_db(db, skilled_db)
                index_info[key] = {"seconds": None, "reused": True, "copied_from": str(db),
                                   "db_bytes": skilled_db.stat().st_size}
            else:
                log(f"[index] {key}: indexing {skilled}")
                index_info[key] = build_index(args.cli_bin, skilled, skilled_db,
                                              args.reuse_index)
            log(f"[index] {key}: {index_info[key]['seconds']} s")
            checkouts[(name, "cli-skill")] = skilled

    meta = {
        "date": dt.datetime.now().isoformat(timespec="seconds"),
        "model": args.model,
        "effort": args.effort,
        "configs": configs,
        "reps": args.reps,
        "jobs": args.jobs,
        "keep_agent_docs": args.keep_agent_docs,
        "claude_version": subprocess.run([args.claude_bin, "--version"], text=True,
                                         capture_output=True).stdout.strip(),
        "semantiq_version": subprocess.run([str(args.semantiq_bin), "--version"], text=True,
                                           capture_output=True).stdout.strip()
        if args.semantiq_bin.exists() else None,
        "cli_skill": {
            "cli_bin": str(args.cli_bin),
            "cli_version": subprocess.run([str(args.cli_bin), "--version"], text=True,
                                          capture_output=True).stdout.strip(),
            "skill_path": str(skill_dir),
            "skill_sha256": hashlib.sha256((skill_dir / "SKILL.md").read_bytes()).hexdigest(),
        } if skill_dir else None,
        "repos": {n: {"commit": repos[n]["commit"], "source": repos[n]["source"]} for n in repos},
        "index": index_info,
    }
    meta_path = out / "meta.json"
    if args.resume and meta_path.exists():
        previous = json.loads(meta_path.read_text())
        previous_index = previous.get("index", {})
        meta["index"] = {**previous_index, **{k: v for k, v in index_info.items() if not v.get("reused")}}
        meta["configs"] = list(dict.fromkeys(previous.get("configs", []) + configs))
        meta["cli_skill"] = meta["cli_skill"] or previous.get("cli_skill")
    meta_path.write_text(json.dumps(meta, indent=2) + "\n")

    records = list(done.values())
    log_lock = threading.Lock()

    def job(spec):
        name = spec["repo"]["name"]
        rec = execute_run(args, spec, checkouts[(name, spec["config"])], mcp_configs[name],
                          transcripts)
        with log_lock, open(runs_log, "a") as f:
            f.write(json.dumps(rec, ensure_ascii=False) + "\n")
        return rec

    with concurrent.futures.ThreadPoolExecutor(max_workers=max(1, args.jobs)) as pool:
        for rec in pool.map(job, todo):
            records.append(rec)

    write_results(out, meta, records, suites)
    return 0


if __name__ == "__main__":
    sys.exit(main())
