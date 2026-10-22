#!/usr/bin/env python3
"""Canonical local signing paths, preserving existing certificates and update keys."""
from pathlib import Path
import os


def keychain_path(home: Path) -> Path:
    parent = home / "Library/Keychains"
    name = "krill-codesign.keychain-db"
    legacy = "minikrill-codesign.keychain-db"
    # Never move an unlocked keychain or regenerate a released Ed25519 key.
    # Relative aliases preserve the bytes and old processes' paths.
    for suffix in ("", ".pass", ".ed25519.key", ".ed25519.key.pub"):
        old, new = parent / (legacy + suffix), parent / (name + suffix)
        if old.is_file() and not os.path.lexists(new):
            try:
                new.symlink_to(old.name)
            except FileExistsError:
                pass
    return parent / name


if __name__ == "__main__":
    print(keychain_path(Path.home()))
