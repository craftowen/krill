#!/usr/bin/env python3
"""Canonical local signing keychain path."""
from pathlib import Path


def keychain_path(home: Path) -> Path:
    return home / "Library/Keychains/krill-codesign.keychain-db"


if __name__ == "__main__":
    print(keychain_path(Path.home()))
