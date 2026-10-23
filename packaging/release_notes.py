#!/usr/bin/env python3
"""Render a local English release header; never contact GitHub or publish anything."""

import argparse
from pathlib import Path
import re
import sys


def render(root, tag, repository):
    """Prefer exact-tag notes while always retaining the public support boundaries."""
    if not re.fullmatch(r"v[0-9]+\.[0-9]+\.[0-9]+(?:-[A-Za-z0-9.-]+)?", tag):
        raise ValueError("expected a release tag such as v0.1.38")
    if not re.fullmatch(r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+", repository):
        raise ValueError("expected an OWNER/REPO repository")
    if any(part in (".", "..") for part in repository.split("/")):
        raise ValueError("invalid repository name")

    url = f"https://github.com/{repository}"
    notes = Path(root) / "docs" / "releases" / f"{tag}.md"
    if notes.is_symlink():
        raise ValueError("tag-specific release notes must not be a symlink")
    if notes.exists():
        highlights = notes.read_text(encoding="utf-8").strip()
        if not highlights:
            raise ValueError(f"tag-specific release notes are empty: {notes}")
    else:
        highlights = (
            "No release-specific highlights were supplied for this tag. "
            "Use the generated changelog below to review the included changes; "
            "this summary does not claim any particular fix or performance improvement."
        )

    return f"""# Krill {tag}

A native Rust workspace for parallel AI coding agents, persistent terminal sessions, and Git worktrees.

## Platform scope and known limits

- **macOS Apple Silicon (arm64):** primary desktop platform. The app is not notarized; a local/ad-hoc signature is not Apple Developer ID approval. The installer removes the downloaded app's quarantine attribute.
- **Windows x64:** native desktop preview; Authenticode signing remains pending. Windows Server VM results do not establish desktop GUI, interactive-logon, sleep, or reboot validation.
- Intel Macs, Linux, Windows ARM64, and 32-bit Windows are outside this public preview's supported desktop scope. Check the assets attached to this release for actual download availability; source or packaging code is not a download promise.
- **Session continuity:** app quit/restart can reconnect to running sessions while the holder and daemon remain alive. A daemon crash can lose bytes read but not yet recorded and mark a Gap. Rebooting or losing both session services ends the old processes; saved history or agent conversation resume is a separate form of recovery.

[Installation and requirements]({url}/blob/{tag}/README.md) · [Failure guarantees]({url}/blob/{tag}/docs/ARCHITECTURE.md#41-장애별-보장-범위-규범) · [Security reporting]({url}/blob/{tag}/SECURITY.md)

## Release highlights

{highlights}

## Changelog

GitHub-generated changes are appended below. [Full changelog]({url}/releases/tag/{tag}#whats-changed) · [Commit history]({url}/commits/{tag}/).

"""


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--tag", required=True)
    parser.add_argument("--repo", required=True)
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parent.parent)
    args = parser.parse_args(argv)
    try:
        text = render(args.root, args.tag, args.repo)
    except (OSError, UnicodeError, ValueError) as error:
        parser.error(str(error))
    sys.stdout.write(text)


if __name__ == "__main__":
    main()
