#!/usr/bin/env python3
"""Insert missed hook-style worklog bullets from git history under the correct ## YYYY-MM-DD headings.

Reads the same ~/.config/mervyn-worklog/env as post-commit (export VAR=value).
Skips commits already represented in worklog.md (backtick hex hashes).
Skips the worklog repo itself and paths under .terraform / node_modules / vendor / .venv.

Usage:
  backfill_worklog_from_git.py              # apply (see --since default below)
  backfill_worklog_from_git.py --dry-run    # print counts only

By default only commits on or after the earliest ## YYYY-MM-DD heading in worklog.md
are considered (so you do not dump decades of ~/Code into the file). Override with
`--since 2014-01-01` or `--since none` for all history.
"""

from __future__ import annotations

import argparse
import os
import re
import subprocess
import sys
from collections import defaultdict
from datetime import datetime, timezone
from pathlib import Path


HASH_IN_TICKS = re.compile(r"`([0-9a-f]{7,40})`", re.IGNORECASE)
H2_DATE = re.compile(r"^## (\d{4}-\d{2}-\d{2})\b", re.MULTILINE)
SKIP_DIR_PARTS = frozenset(
    {".terraform", "node_modules", "vendor", ".venv", "__pycache__"}
)


def load_env_file(path: Path) -> None:
    if not path.is_file():
        print(f"missing env file: {path}", file=sys.stderr)
        sys.exit(1)
    for line in path.read_text(encoding="utf-8").splitlines():
        line = line.strip()
        if not line or line.startswith("#"):
            continue
        if line.startswith("export "):
            line = line[7:]
        if "=" not in line:
            continue
        key, _, val = line.partition("=")
        key = key.strip()
        val = val.strip().strip('"').strip("'")
        os.environ.setdefault(key, val)


def collect_logged_hash_prefixes(worklog_text: str) -> set[str]:
    return {m.group(1).lower() for m in HASH_IN_TICKS.finditer(worklog_text)}


def already_logged(full_hash: str, logged: set[str]) -> bool:
    fl = full_hash.lower()
    for t in logged:
        tl = t.lower()
        if len(tl) >= 40:
            if fl == tl:
                return True
        elif fl.startswith(tl):
            return True
    return False


def iter_git_repo_roots(prefixes: list[Path]) -> list[Path]:
    roots: list[Path] = []
    seen: set[Path] = set()
    for prefix in prefixes:
        p = prefix.resolve()
        if not p.is_dir():
            continue
        for git_path in p.rglob(".git"):
            if any(part in SKIP_DIR_PARTS for part in git_path.parts):
                continue
            top = git_path.parent.resolve()
            if top in seen:
                continue
            seen.add(top)
            roots.append(top)
    roots.sort(key=lambda x: str(x))
    return roots


def earliest_worklog_section_ymd(worklog_text: str) -> str | None:
    dates = H2_DATE.findall(worklog_text)
    if not dates:
        return None
    return min(dates)


def author_clock_label(author_iso: str) -> str:
    """Wall-clock HH:MM from git %ai prefix (author-local)."""
    try:
        return datetime.strptime(author_iso[:19], "%Y-%m-%d %H:%M:%S").strftime("%H:%M")
    except ValueError:
        return ""


def git_log_commits(repo: Path) -> list[tuple[int, str, str, str, str, str]]:
    """Return list of (author_unix, full_hash, short_hash, ymd_author, author_iso, subject)."""
    r = subprocess.run(
        [
            "git",
            "-C",
            str(repo),
            "log",
            "--all",
            "--format=%H%x1f%h%x1f%at%x1f%as%x1f%ai%x1f%s%x1e",
        ],
        capture_output=True,
        text=True,
        check=False,
    )
    if r.returncode != 0:
        return []
    out: list[tuple[int, str, str, str, str, str]] = []
    for rec in r.stdout.split("\x1e"):
        rec = rec.strip()
        if not rec:
            continue
        parts = rec.split("\x1f")
        if len(parts) != 6:
            continue
        full_h, short_h, at_s, ymd, ai, subj = parts
        try:
            at = int(at_s)
        except ValueError:
            continue
        subj_one = subj.replace("\n", " ").strip()
        if not subj_one:
            subj_one = "(no subject)"
        out.append((at, full_h, short_h, ymd, ai, subj_one))
    return out


def _h2_date_key(line: str) -> str | None:
    if not line.startswith("## "):
        return None
    head = line[3:].strip()
    day_part = head.split()[0] if head else ""
    if len(day_part) == 10 and day_part[4:5] == "-" and day_part[7:8] == "-":
        return day_part
    return None


def find_insert_index_for_new_day_section(lines: list[str], ymd: str) -> int:
    """First line index where an existing ## date heading sorts after ymd (ISO); else end of file."""
    for i, line in enumerate(lines):
        dk = _h2_date_key(line)
        if dk is not None and dk > ymd:
            return i
    return len(lines)


def insert_bullets_for_date(
    lines: list[str],
    ymd: str,
    bullets: list[str],
) -> None:
    """Insert hook-style lines (each ends with \\n) under ## ymd, before Tags: or after last list item."""
    header = f"## {ymd}\n"
    new_chunks = [b if b.endswith("\n") else b + "\n" for b in bullets]

    start: int | None = None
    for i, line in enumerate(lines):
        if line == header or line.strip() == header.strip():
            start = i
            break

    if start is None:
        insert_at = find_insert_index_for_new_day_section(lines, ymd)
        block: list[str] = []
        if insert_at > 0 and lines and not lines[insert_at - 1].endswith("\n"):
            lines[insert_at - 1] += "\n"
        if insert_at > 0 and lines[:insert_at] and lines[insert_at - 1].strip() != "":
            block.append("\n")
        block.append(header)
        block.extend(new_chunks)
        lines[insert_at:insert_at] = block
        return

    end = len(lines)
    for i in range(start + 1, len(lines)):
        if lines[i].startswith("## ") and lines[i].strip() != header.strip():
            end = i
            break

    tags_idx: int | None = None
    last_item: int | None = None
    for i in range(start + 1, end):
        s = lines[i].strip()
        if s.startswith("Tags:"):
            tags_idx = i
            break
        if s.startswith(("- ", "* ")):
            last_item = i

    insert_at: int
    if tags_idx is not None:
        insert_at = tags_idx
    elif last_item is not None:
        insert_at = last_item + 1
    else:
        insert_at = start + 1

    for j, chunk in enumerate(new_chunks):
        lines.insert(insert_at + j, chunk)


def parse_since_cutoff(s: str, worklog_text: str) -> int | None:
    """Unix time: include commits with author time >= this. None = no cutoff."""
    s = s.strip().lower()
    if s in ("none", "all", "0"):
        return None
    if s in ("auto", "worklog", ""):
        ymd = earliest_worklog_section_ymd(worklog_text)
        if ymd is None:
            print(
                "worklog.md has no ## YYYY-MM-DD headings; pass --since YYYY-MM-DD",
                file=sys.stderr,
            )
            sys.exit(1)
        dt = datetime.strptime(ymd, "%Y-%m-%d").replace(tzinfo=timezone.utc)
        return int(dt.timestamp())
    dt = datetime.strptime(s, "%Y-%m-%d").replace(tzinfo=timezone.utc)
    return int(dt.timestamp())


def main() -> None:
    parser = argparse.ArgumentParser(
        description=__doc__,
        formatter_class=argparse.RawDescriptionHelpFormatter,
    )
    parser.add_argument(
        "--dry-run",
        action="store_true",
        help="do not write worklog.md; print summary only",
    )
    parser.add_argument(
        "--since",
        default="auto",
        metavar="DATE",
        help="YYYY-MM-DD, 'auto' (earliest ## date in worklog), or 'none' for full history",
    )
    args = parser.parse_args()

    config_dir = Path(os.environ.get("XDG_CONFIG_HOME", Path.home() / ".config"))
    env_path = config_dir / "mervyn-worklog" / "env"
    load_env_file(env_path)

    repo = os.environ.get("MERVYN_WORKLOG_REPO", "").strip()
    rel = os.environ.get("MERVYN_WORKLOG_REL_PATH", "").strip()
    prefixes_raw = os.environ.get("MERVYN_WORKLOG_PROJECT_PREFIXES", "").strip()
    if not repo or not rel or not prefixes_raw:
        print("MERVYN_WORKLOG_REPO, MERVYN_WORKLOG_REL_PATH, MERVYN_WORKLOG_PROJECT_PREFIXES required", file=sys.stderr)
        sys.exit(1)

    worklog_root = Path(repo).resolve()
    worklog_file = worklog_root / rel
    prefixes = [Path(os.path.expanduser(p)) for p in prefixes_raw.split()]

    text = worklog_file.read_text(encoding="utf-8") if worklog_file.exists() else ""
    logged = collect_logged_hash_prefixes(text)
    since_ts = parse_since_cutoff(args.since, text)

    candidates: list[tuple[int, str, str, str, str, str]] = []
    # (at, ymd, full, short, reponame, line_body) line_body without leading "- "
    for root in iter_git_repo_roots(prefixes):
        if root.resolve() == worklog_root:
            continue
        name = root.name
        for at, full_h, short_h, ymd, ai, subj in git_log_commits(root):
            if since_ts is not None and at < since_ts:
                continue
            if already_logged(full_h, logged):
                continue
            clock = author_clock_label(ai)
            time_sfx = f" _{clock}_" if clock else ""
            body = f"**{name}** `{short_h}` {subj}{time_sfx}"
            candidates.append((at, ymd, full_h, short_h, name, body))
            logged.add(full_h.lower())

    if not candidates:
        print("No missed commits found.")
        return

    candidates.sort(key=lambda x: (x[1], x[0], x[4]))

    by_date: dict[str, list[str]] = defaultdict(list)
    for at, ymd, _full, _short, _name, body in candidates:
        by_date[ymd].append(body)

    if args.dry_run:
        print(
            f"Would add {len(candidates)} bullets across {len(by_date)} day sections "
            f"(since filter: {args.since!r})."
        )
        for ymd in sorted(by_date.keys()):
            print(f"  {ymd}: {len(by_date[ymd])}")
        return

    lines = text.splitlines(keepends=True)
    for ymd in sorted(by_date.keys()):
        bullets = [f"- {b}\n" for b in by_date[ymd]]
        insert_bullets_for_date(lines, ymd, bullets)

    worklog_file.write_text("".join(lines), encoding="utf-8")
    print(f"Wrote {len(candidates)} bullets to {worklog_file}")


if __name__ == "__main__":
    main()
