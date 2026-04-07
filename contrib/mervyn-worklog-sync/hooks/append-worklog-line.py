#!/usr/bin/env python3
"""Append one list line under the correct ## YYYY-MM-DD section (Mervyn vault/worklog.md format)."""

from __future__ import annotations

import os
import sys
from datetime import date, datetime, timezone
from pathlib import Path


def main() -> None:
    if len(sys.argv) != 3:
        print("usage: append-worklog-line.py WORKLOG_PATH LINE", file=sys.stderr)
        sys.exit(2)

    path = Path(sys.argv[1])
    bullet_body = sys.argv[2].replace("\n", " ").strip()
    if not bullet_body:
        sys.exit(0)

    use_utc = os_env_true("MERVYN_WORKLOG_UTC_DATE")
    today = (
        datetime.now(timezone.utc).date().isoformat()
        if use_utc
        else date.today().isoformat()
    )
    header = f"## {today}"
    new_line = f"- {bullet_body}\n"

    path.parent.mkdir(parents=True, exist_ok=True)
    text = path.read_text(encoding="utf-8") if path.exists() else ""
    lines = text.splitlines(keepends=True)

    # Use the last ## YYYY-MM-DD block for `today` so repeated headings still work.
    start: int | None = None
    for i, line in enumerate(lines):
        if line.strip() == header:
            start = i

    if start is None:
        if lines and not lines[-1].endswith("\n"):
            lines[-1] += "\n"
        if lines:
            lines.append("\n")
        lines.append(f"{header}\n")
        lines.append(new_line)
        path.write_text("".join(lines), encoding="utf-8")
        return

    end = len(lines)
    for i in range(start + 1, len(lines)):
        if lines[i].startswith("## ") and lines[i].strip() != header:
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

    if tags_idx is not None:
        lines.insert(tags_idx, new_line)
    elif last_item is not None:
        lines.insert(last_item + 1, new_line)
    else:
        lines.insert(start + 1, new_line)

    path.write_text("".join(lines), encoding="utf-8")


def os_env_true(name: str) -> bool:
    v = os.environ.get(name, "")
    return v.lower() in ("1", "true", "yes")


if __name__ == "__main__":
    main()
