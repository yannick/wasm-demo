#!/usr/bin/env python3
"""Code metrics for agent-demo: collect a snapshot, gate ratchets, record history.

Producers:

- bca 2.1.0 (big-code-analysis): per-function cyclomatic / cognitive complexity and length of
  the Rust code (bca.toml); `bca check` gates thresholds against .bca-baseline.toml.
- tokei: lines of code per language (Rust, TypeScript, CSS, proto, …).
- cargo metadata: how many crates the server binary pulls in.
- unsafe: `unsafe` blocks, fns and impls in our own Rust sources (the workspace denies
  `unsafe_code`; every use is an explicit, commented `allow`).
- optional: the release binary's size, cargo-bloat's top crates, cargo-llvm-cov line coverage.

Commands (see metrics/README.md):

  collect    print the snapshot and write target/metrics/current.json
  check      fail on a regression: bca thresholds, unsafe count and long functions above the
             baseline (metrics/baseline.json)
  record     append the snapshot to metrics/history/<date>-<label>.json
  baseline   accept the current unsafe count and long functions as the new baseline
  coverage   run cargo llvm-cov and add line coverage to the snapshot
  tools-check  show which producers are installed, at which version
"""

from __future__ import annotations

import argparse
import datetime as dt
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path
from typing import NoReturn, Sequence

ROOT = Path(__file__).resolve().parent.parent
METRICS_DIR = ROOT / "metrics"
BASELINE = METRICS_DIR / "baseline.json"
HISTORY = METRICS_DIR / "history"
OUT = ROOT / "target" / "metrics"
RUST_SOURCES = [ROOT / "crates", ROOT / "wasm" / "sheet-web" / "src"]
PACKAGE = "agent-demo"
# Functions longer than this (logical lines) are tracked; a new or longer one fails `check`.
LONG_FUNCTION_LIMIT = 80

TOOLS: dict[str, tuple[Sequence[str], str]] = {
    "bca": (("bca", "--version"), "cargo install --locked big-code-analysis-cli@2.1.0"),
    "tokei": (("tokei", "--version"), "cargo install --locked tokei"),
    "cargo-llvm-cov": (("cargo", "llvm-cov", "--version"), "cargo install --locked cargo-llvm-cov@0.8.7"),
    "cargo-bloat": (("cargo", "bloat", "--version"), "cargo install --locked cargo-bloat@0.12.1"),
}
REQUIRED = ("bca", "tokei")


class MetricsError(Exception):
    pass


def run(command: Sequence[str], *, env: dict[str, str] | None = None) -> str:
    try:
        done = subprocess.run(command, cwd=ROOT, env=env, check=False, text=True, capture_output=True)
    except FileNotFoundError as e:
        raise MetricsError(f"{command[0]} is not installed (just tools-install)") from e
    if done.returncode != 0:
        raise MetricsError(f"{' '.join(command)} failed ({done.returncode}):\n{done.stderr.strip()[-2000:]}")
    return done.stdout


def write_json(path: Path, document: object) -> None:
    """Write then rename, so a reader never sees half a file."""
    path.parent.mkdir(parents=True, exist_ok=True)
    fd, tmp = tempfile.mkstemp(dir=path.parent, prefix=f".{path.name}.")
    with os.fdopen(fd, "w") as f:
        json.dump(document, f, indent=2, sort_keys=True)
        f.write("\n")
    os.replace(tmp, path)


def tool_versions() -> dict[str, str | None]:
    versions: dict[str, str | None] = {}
    for name, (command, _) in TOOLS.items():
        try:
            out = run(command)
            match = re.search(r"\d+\.\d+\.\d+", out)
            versions[name] = match.group(0) if match else out.strip()
        except MetricsError:
            versions[name] = None
    return versions


# ---------- producers ----------


def percentile(values: list[int], fraction: float) -> float:
    if not values:
        return 0
    ordered = sorted(values)
    k = (len(ordered) - 1) * fraction
    lo, hi = int(k), min(int(k) + 1, len(ordered) - 1)
    return ordered[lo] + (ordered[hi] - ordered[lo]) * (k - lo)


def distribution(values: list[int]) -> dict[str, float | int]:
    return {
        "max": max(values, default=0),
        "median": round(percentile(values, 0.5), 2),
        "p90": round(percentile(values, 0.9), 2),
    }


def bca_functions(spaces: list[dict], path: str, parents: tuple[str, ...] = ()) -> list[dict]:
    """bca's nested spaces flattened to one record per function."""
    out: list[dict] = []
    for space in spaces:
        qualified = (*parents, space["name"])
        if space["kind"] == "function":
            m = space["metrics"]
            out.append(
                {
                    "id": f"{path}::{'::'.join(qualified)}",
                    "cyclomatic": int(m["cyclomatic"]["value"]),
                    "cognitive": int(m["cognitive"]["value"]),
                    "lines": space["end_line"] - space["start_line"] + 1,
                }
            )
        out.extend(bca_functions(space.get("spaces", []), path, qualified))
    return out


def complexity() -> dict[str, object]:
    path = OUT / "bca.json"
    OUT.mkdir(parents=True, exist_ok=True)
    run(("bca", "metrics", "--format", "json", "--pretty", "--metrics", "cyclomatic,cognitive,lloc", "--output", str(path)))
    files = json.loads(path.read_text())
    functions = [f for file in files for f in bca_functions(file["spaces"], file["name"])]
    baseline = ROOT / ".bca-baseline.toml"
    exceptions = baseline.read_text().count("[[entry]]") if baseline.exists() else 0
    return {
        "functions": len(functions),
        "cyclomatic": distribution([f["cyclomatic"] for f in functions]),
        "cognitive": distribution([f["cognitive"] for f in functions]),
        "function_lines": distribution([f["lines"] for f in functions]),
        "baseline_exceptions": exceptions,
        "long_functions": {f["id"]: f["lines"] for f in sorted(functions, key=lambda f: f["id"]) if f["lines"] > LONG_FUNCTION_LIMIT},
    }


def lines() -> dict[str, dict[str, int]]:
    out = json.loads(run(("tokei", "--output", "json", "--exclude", "web/src/gen", "--exclude", "web/public/sheet", "crates", "proto", "web/src", "web/public", "wasm/sheet-web/src", "tools")))
    return {
        lang: {"code": data["code"], "comments": data["comments"], "files": len(data.get("reports", []))}
        for lang, data in sorted(out.items())
        if lang != "Total" and data["code"] > 0
    }


UNSAFE = re.compile(r"\bunsafe\s+(?:(fn)|(impl)|(trait)|\{)")


def unsafe_uses() -> dict[str, int]:
    counts = {"blocks": 0, "fns": 0, "impls": 0, "traits": 0}
    for root in RUST_SOURCES:
        for path in root.rglob("*.rs"):
            if "target" in path.parts:
                continue
            for line in path.read_text().splitlines():
                if line.lstrip().startswith("//"):
                    continue
                for m in UNSAFE.finditer(line):
                    key = "fns" if m.group(1) else "impls" if m.group(2) else "traits" if m.group(3) else "blocks"
                    counts[key] += 1
    return counts


def dependencies() -> dict[str, int]:
    meta = json.loads(run(("cargo", "metadata", "--format-version", "1", "--locked")))
    nodes = {n["id"]: n for n in meta["resolve"]["nodes"]}
    root = next(p["id"] for p in meta["packages"] if p["name"] == PACKAGE)
    seen: set[str] = set()
    stack = [root]
    while stack:
        node = nodes[stack.pop()]
        for dep in node["deps"]:
            # Normal dependencies only: what ships in the binary.
            if any(k["kind"] is None for k in dep["dep_kinds"]) and dep["pkg"] not in seen:
                seen.add(dep["pkg"])
                stack.append(dep["pkg"])
    direct = sum(1 for d in nodes[root]["deps"] if any(k["kind"] is None for k in d["dep_kinds"]))
    return {"direct": direct, "total": len(seen)}


def target_dir() -> Path:
    meta = json.loads(run(("cargo", "metadata", "--format-version", "1", "--no-deps")))
    return Path(meta["target_directory"])


def binary() -> dict[str, int] | None:
    path = target_dir() / "release" / PACKAGE
    return {"release_bytes": path.stat().st_size} if path.exists() else None


# ---------- snapshot, check, baseline ----------


def git() -> dict[str, object]:
    try:
        revision = run(("git", "rev-parse", "HEAD")).strip()
        dirty = bool(run(("git", "status", "--porcelain")).strip())
    except MetricsError:
        return {"revision": None, "dirty": None}
    return {"revision": revision, "dirty": dirty}


def ensure(names: Sequence[str], versions: dict[str, str | None]) -> None:
    missing = [n for n in names if versions.get(n) is None]
    if missing:
        raise MetricsError(f"missing tools: {', '.join(missing)} (just tools-install)")


def collect() -> dict[str, object]:
    versions = tool_versions()
    ensure(REQUIRED, versions)
    snapshot = {
        "schema": "agent-demo.metrics.v1",
        "at": dt.datetime.now(dt.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
        "git": git(),
        "tools": {k: v for k, v in versions.items() if v},
        "complexity": complexity(),
        "lines": lines(),
        "unsafe": unsafe_uses(),
        "dependencies": dependencies(),
        "binary": binary(),
    }
    coverage = OUT / "coverage.json"
    if coverage.exists():
        snapshot["coverage"] = json.loads(coverage.read_text())
    write_json(OUT / "current.json", snapshot)
    return snapshot


def show(s: dict) -> None:
    c = s["complexity"]
    print(f"functions        {c['functions']}  (long > {LONG_FUNCTION_LIMIT} lines: {len(c['long_functions'])}, bca baseline exceptions: {c['baseline_exceptions']})")
    for k in ("cyclomatic", "cognitive", "function_lines"):
        d = c[k]
        print(f"{k:<16} median {d['median']:<6} p90 {d['p90']:<6} max {d['max']}")
    print("lines of code    " + ", ".join(f"{lang} {v['code']}" for lang, v in s["lines"].items()))
    print("unsafe           " + ", ".join(f"{k} {v}" for k, v in s["unsafe"].items()))
    print(f"dependencies     {s['dependencies']['direct']} direct, {s['dependencies']['total']} total")
    if s.get("binary"):
        print(f"release binary   {s['binary']['release_bytes'] / 1e6:.1f} MB")
    if s.get("coverage"):
        print(f"line coverage    {s['coverage']['lines_percent']} %")
    print(f"\nwritten to {OUT / 'current.json'}")


def check() -> int:
    failures: list[str] = []
    done = subprocess.run(("bca", "check", "--no-suppress"), cwd=ROOT, check=False)
    if done.returncode == 2:
        failures.append("bca: a function exceeds the complexity thresholds (bca.toml) beyond its baseline")
    elif done.returncode != 0:
        raise MetricsError(f"bca check failed ({done.returncode})")
    if not BASELINE.exists():
        raise MetricsError(f"{BASELINE.relative_to(ROOT)} is missing: run `just metrics-baseline`")
    base = json.loads(BASELINE.read_text())
    current = {"unsafe": unsafe_uses(), "long_functions": complexity()["long_functions"]}
    for kind, count in current["unsafe"].items():
        if count > base["unsafe"].get(kind, 0):
            failures.append(f"unsafe {kind}: {count} (baseline {base['unsafe'].get(kind, 0)})")
    for fn, length in current["long_functions"].items():
        allowed = base["long_functions"].get(fn)
        if allowed is None or length > allowed:
            failures.append(f"long function {fn}: {length} lines (baseline {allowed or f'none, limit {LONG_FUNCTION_LIMIT}'})")
    if failures:
        print("metrics regressions:\n  " + "\n  ".join(failures), file=sys.stderr)
        return 2
    print("metrics: no regressions")
    return 0


def baseline() -> None:
    c = complexity()
    write_json(BASELINE, {
        "schema": "agent-demo.metrics-baseline.v1",
        "updated_at": dt.datetime.now(dt.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
        "git": git(),
        "unsafe": unsafe_uses(),
        "long_functions": c["long_functions"],
    })
    print(f"wrote {BASELINE.relative_to(ROOT)}; refresh the bca baseline with `bca check --write-baseline` if needed")


def record(label: str) -> None:
    safe = re.sub(r"[^A-Za-z0-9._-]+", "-", label).strip("-") or "snapshot"
    snapshot = collect()
    path = HISTORY / f"{dt.date.today().isoformat()}-{safe}.json"
    write_json(path, snapshot)
    print(f"recorded {path.relative_to(ROOT)}")


def coverage(open_report: bool) -> None:
    ensure(("cargo-llvm-cov",), tool_versions())
    env = {**os.environ, "AGENT_DEMO_SKIP_WEB_BUILD": os.environ.get("AGENT_DEMO_SKIP_WEB_BUILD", "1")}
    path = OUT / "llvm-cov.json"
    OUT.mkdir(parents=True, exist_ok=True)
    run(("cargo", "llvm-cov", "--workspace", "--json", "--summary-only", "--output-path", str(path)), env=env)
    totals = json.loads(path.read_text())["data"][0]["totals"]
    summary = {
        "lines_percent": round(totals["lines"]["percent"], 2),
        "functions_percent": round(totals["functions"]["percent"], 2),
        "regions_percent": round(totals["regions"]["percent"], 2),
    }
    write_json(OUT / "coverage.json", summary)
    print(f"line coverage {summary['lines_percent']} %, functions {summary['functions_percent']} %")
    if open_report:
        run(("cargo", "llvm-cov", "--workspace", "--html", "--open"), env=env)


def tools_check() -> int:
    versions = tool_versions()
    for name, (_, install) in TOOLS.items():
        state = versions[name] or "missing"
        required = " (required)" if name in REQUIRED else ""
        print(f"{name:<16} {state:<10}{required}" + (f"  → {install}" if versions[name] is None else ""))
    return 0 if all(versions[n] for n in REQUIRED) else 1


def fail(message: str, code: int = 1) -> NoReturn:
    print(f"metrics: {message}", file=sys.stderr)
    sys.exit(code)


def main(argv: Sequence[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = parser.add_subparsers(dest="command", required=True)
    sub.add_parser("collect")
    sub.add_parser("check")
    sub.add_parser("record").add_argument("label")
    sub.add_parser("baseline")
    sub.add_parser("coverage").add_argument("--open", action="store_true")
    sub.add_parser("tools-check")
    args = parser.parse_args(argv)
    if shutil.which("cargo") is None:
        fail("cargo is not on PATH")
    try:
        if args.command == "collect":
            show(collect())
        elif args.command == "check":
            return check()
        elif args.command == "record":
            record(args.label)
        elif args.command == "baseline":
            baseline()
        elif args.command == "coverage":
            coverage(args.open)
        elif args.command == "tools-check":
            return tools_check()
    except MetricsError as e:
        fail(str(e))
    return 0


if __name__ == "__main__":
    sys.exit(main())
