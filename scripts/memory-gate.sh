#!/bin/zsh
# MK-066 메모리 회귀 게이트(nightly·수동 선택 실행 — 릴리스 게시 경로에는 연결돼 있지 않다): release 앱+krilld+holder에
# 터미널 10개를 붙여 phys_footprint가 앱 ≤ 120MiB·데몬 ≤ 60MiB·합 ≤ 200MiB 중 하나라도 넘으면 0이 아닌 코드로 끝난다.
# 병렬 부하에서 값이 흔들리므로 check.sh 기본 경로가 아니라 `scripts/check.sh --memory`로 돈다.
# 로그인된 macOS GUI 세션이 필요하다. 시나리오·예산: crates/krill-app/tests/support/memory_gate.rs, Windows판: scripts/memory-gate-windows.ps1.
# --inject <MiB>: krilld가 그만큼 더 쥐게 해(KRILL_TEST_BALLAST_MB) 게이트가 회귀를 잡는지 확인한다. 이때는 실패가 정상이다.
set -euo pipefail
cd "${0:A:h}/.."
t(){ perl -e 'alarm shift; exec @ARGV' "$@"; }
if [[ ${1-} == --inject ]]; then export KRILL_TEST_BALLAST_MB=${2:?MiB}; fi
t 900 cargo build --release --locked -p krill-app -p krilld -p krill-holder
t 900 cargo test --release --locked -p krill-app --test pane_e2e --no-run
t 300 cargo test --release --locked -p krill-app --test pane_e2e memory_gate:: -- --ignored --nocapture --test-threads=1
