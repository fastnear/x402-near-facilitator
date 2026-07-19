#!/usr/bin/env python3
"""Check local Markdown links and obvious tracked-secret mistakes."""

from __future__ import annotations

import os
import re
import subprocess
import sys
from pathlib import Path
from urllib.parse import unquote

ROOT = Path(__file__).resolve().parent.parent
LINK = re.compile(r"(?<!!)\[[^\]]*\]\(([^)]+)\)")
PRIVATE_KEY = re.compile(r"-----BEGIN [A-Z0-9 ]*PRIVATE KEY-----")
DATABASE_PASSWORD = re.compile(r"postgres(?:ql)?://[^/\s:@]+:[^@\s]+@", re.IGNORECASE)
RAW_API_KEY = re.compile(r"x402_(?:live|test)_[A-Za-z0-9_-]{4,}\.[A-Za-z0-9_-]{20,}")


def tracked_files() -> list[Path]:
    output = subprocess.check_output(
        ["git", "ls-files", "-z", "--cached", "--others", "--exclude-standard"],
        cwd=ROOT,
    )
    return [ROOT / item.decode() for item in output.split(b"\0") if item]


def markdown_link_errors(paths: list[Path]) -> list[str]:
    errors: list[str] = []
    for path in paths:
        if path.suffix.lower() != ".md":
            continue
        text = path.read_text(encoding="utf-8")
        for match in LINK.finditer(text):
            destination = match.group(1).strip()
            if destination.startswith("<") and destination.endswith(">"):
                destination = destination[1:-1]
            destination = destination.split(maxsplit=1)[0]
            if (
                not destination
                or destination.startswith(("#", "http://", "https://", "mailto:"))
            ):
                continue
            destination = unquote(destination.split("#", 1)[0])
            target = (path.parent / destination).resolve()
            if not target.exists():
                line = text.count("\n", 0, match.start()) + 1
                errors.append(
                    f"{path.relative_to(ROOT)}:{line}: missing local link {destination}"
                )
    return errors


def secret_errors(paths: list[Path]) -> list[str]:
    errors: list[str] = []
    forbidden_names = {".env", ".env.local", ".env.production"}
    forbidden_suffixes = {".credential", ".key", ".pem", ".secret"}
    for path in paths:
        relative = path.relative_to(ROOT)
        if path.name in forbidden_names or path.suffix.lower() in forbidden_suffixes:
            errors.append(f"{relative}: secret-like filename is tracked")
            continue
        if not path.is_file() or path.stat().st_size > 2_000_000:
            continue
        try:
            text = path.read_text(encoding="utf-8")
        except UnicodeDecodeError:
            continue
        for name, pattern in [
            ("private-key block", PRIVATE_KEY),
            ("database URL with password", DATABASE_PASSWORD),
            ("raw x402 API key", RAW_API_KEY),
        ]:
            if pattern.search(text):
                errors.append(f"{relative}: possible {name}")
    return errors


def main() -> int:
    os.chdir(ROOT)
    paths = tracked_files()
    errors = markdown_link_errors(paths) + secret_errors(paths)
    if errors:
        print("\n".join(errors), file=sys.stderr)
        return 1
    print("documentation links and secret-file guard passed")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
