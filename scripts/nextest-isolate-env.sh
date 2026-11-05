#!/bin/sh
# nextest setup script (.config/nextest.toml): test processes get a fresh temporary HOME, and agent
# CLI and Krill state paths inherited from a Krill session move inside it. nextest can only set
# variables, so check.sh and nightly also unset them for CI parity.
set -eu
root=$(mktemp -d "${TMPDIR:-/tmp}/krill-test-home-XXXXXX")
if [ -f "$HOME/.gitconfig" ]; then cp "$HOME/.gitconfig" "$root/.gitconfig"; fi
{
  echo "HOME=$root"
  echo "CARGO_HOME=${CARGO_HOME:-$HOME/.cargo}"
  if [ -n "${KRILL_STATE_DIR-}" ]; then echo "KRILL_STATE_DIR=$root/krill-state"; fi
  if [ -n "${CLAUDE_CONFIG_DIR-}" ]; then echo "CLAUDE_CONFIG_DIR=$root/.claude"; fi
  if [ -n "${CODEX_HOME-}" ]; then echo "CODEX_HOME=$root/.codex"; fi
  if [ -n "${XDG_CONFIG_HOME-}" ]; then echo "XDG_CONFIG_HOME=$root/.config"; fi
} >> "$NEXTEST_ENV"
