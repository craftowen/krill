#!/usr/bin/env bash
# 저장소 git 훅(scripts/hooks/)을 켠다. 워크트리 전체가 같은 설정(.git/config)을 공유하므로 한 번만 하면 된다.
set -euo pipefail
cd "$(dirname "$0")/.."
git config core.hooksPath scripts/hooks
echo "core.hooksPath = $(git config core.hooksPath)"
