#!/bin/zsh
# 릴리스 후보의 전체 검사. 개발 중에는 AGENTS.md의 영향 범위별 검증을 따른다.
set -euo pipefail
cd "${0:A:h}/.."
run_memory=0
target_dir=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    -h|--help)
      print "사용: scripts/check.sh [--memory] [--target-dir DIR]
  비Cargo 회귀 · fmt+deny+NOTICES+public-surface(병렬) · clippy · nextest(fast+latency)
  --memory          끝에 scripts/memory-gate.sh(MK-066)까지 돈다
  --target-dir DIR  Cargo 산출물 경로를 환경변수 대신 플래그로 지정(sccache 호환)
  KRILL_TEST_SLOTS  머신 전체 동시 테스트 실행 수(기본: CPU 코어 수 / 4, 최소 1)"
      exit 0
      ;;
    --memory)
      run_memory=1
      shift
      ;;
    --target-dir)
      [[ $# -ge 2 && -n "$2" && "$2" != -* ]] || { print -u2 "check.sh: --target-dir requires a directory"; exit 2; }
      target_dir=$2
      shift 2
      ;;
    *)
      print -u2 "check.sh: unknown argument: $1"
      exit 2
      ;;
  esac
done
cargo_target_args=()
if [[ -n "$target_dir" ]]; then
  unset CARGO_TARGET_DIR
  python3 scripts/check-build-target.py --target-dir "$target_dir"
  cargo_target_args=(--target-dir "$target_dir")
else
  python3 scripts/check-build-target.py
fi
# 테스트 빌드 프로필은 CI(ci-macos.yml)·nightly와 같다. 시간 예산 테스트가 로컬과 CI에서 같은 최적화 수준을 본다.
export CARGO_PROFILE_TEST_OPT_LEVEL=${CARGO_PROFILE_TEST_OPT_LEVEL:-2} CARGO_PROFILE_TEST_DEBUG=${CARGO_PROFILE_TEST_DEBUG:-line-tables-only}
# Krill 세션에서 물려받은 운영 상태 폴더·에이전트 설정 경로를 지워 CI와 같은 환경에서 검사한다(F-01).
# 테스트 HOME 격리는 .config/nextest.toml setup 스크립트가 맡는다.
unset KRILL_STATE_DIR CLAUDE_CONFIG_DIR CODEX_HOME XDG_CONFIG_HOME KRILL_CLI KRILL_ROOT_PATH KRILL_WORKSPACE_NAME KRILL_WORKTREE_PATH
# 종료·타임아웃(perl alarm → SIGALRM)·Ctrl-C 때 이 스크립트가 띄운 cargo·테스트 프로세스 트리를 정리한다(고아 cargo가
# 공유 빌드 락을 쥔 채 남는 문제). `kill 0`은 호출자 셸까지 죽일 수 있어 자기 자식 트리만 찾아 끈다.
# zsh는 포그라운드 자식이 끝날 때까지 신호 trap을 미루므로 각 단계는 run(백그라운드 + wait)으로 돌린다.
kill_tree(){ local c; for c in $(pgrep -P $1); do kill_tree $c; done; kill -TERM $1 2>/dev/null || true; }
cleanup(){ local c; for c in $(pgrep -P $$); do kill_tree $c; done; }
trap cleanup EXIT
trap 'cleanup; exit 130' INT
trap 'cleanup; exit 142' ALRM
trap 'cleanup; exit 143' TERM HUP
run(){ "$@" & wait $!; }
step(){ print -P "%B== $1%b"; }
step "non-Cargo regressions"; run python3 scripts/check-regressions.py
# 병렬 1단계: 읽기 전용 검사 4종을 묶고, 하나라도 실패하면 전체 검사를 중단한다.
# clippy·nextest는 target/ 락을 공유하므로 순서를 유지한다.
step "fmt+deny+notices+public-surface (parallel)";
partlog=$(mktemp -d "${TMPDIR:-/tmp}/mk-check-part-XXXXXX")
typeset -A partpids
part(){ local name=$1; shift; "$@" > $partlog/$name.log 2>&1 & partpids[$!]=$name; }
part fmt cargo fmt --all --check
part deny cargo deny --locked check --hide-inclusion-graph
part notices python3 docs/licensing/generate_notices.py --check
part public-surface bash scripts/public-surface.sh
partfail=0
for pid name in ${(kv)partpids}; do
  if ! wait $pid; then print -P "%B!! $name 실패%b"; sed 's/^/  /' $partlog/$name.log; partfail=1; fi
done
rm -rf $partlog
[[ $partfail -eq 0 ]] || exit 1
step "clippy";  run cargo clippy "${cargo_target_args[@]}" --workspace --all-targets --locked -- -D warnings
# 테스트 실행 슬롯(2026-10-15): 워커 20개가 동시에 전체 테스트를 돌리면 load 150~200이 되어 지연 측정 테스트가
# 결정적으로 실패했다. 빌드는 각자 하고, 실행만 머신 전체에서 KRILL_TEST_SLOTS개까지 동시에 돈다.
# 기본값은 결정 11-①(2026-10-15)대로 CPU 코어 수 기반: 코어/4(12코어에서 예전 고정값 3과 같다).
cores=$(sysctl -n hw.ncpu 2>/dev/null || nproc 2>/dev/null || print 4)
slot(){ local n=${KRILL_TEST_SLOTS:-$(( cores / 4 > 0 ? cores / 4 : 1 ))} k rc; while true; do for k in $(seq 1 $n); do
  rc=0; run lockf -s -t 0 /tmp/mk-test-slot-$k "$@" || rc=$?; [[ $rc -ne 75 ]] && return $rc; done; sleep 3; done; }
# 테스트 분리: 부하에 민감한 저지연 테스트를 나머지 테스트와 별도 실행한다.
# 아래 필터는 .config/nextest.toml
# latency override와 같은 집합이어야 한다(어긋나면 저지연 테스트가 고속 페이즈에서 부하를 맞아 flake 가능).
step "test build";   run cargo nextest run "${cargo_target_args[@]}" --workspace --locked --no-tests=pass --no-run
step "test fast";    slot cargo nextest run "${cargo_target_args[@]}" --workspace --locked --no-tests=pass --no-fail-fast -E 'not (binary(survive) | binary(reconnect) | binary(wait) | binary(attach) | binary(holder) | test(survive) | test(/_ms$/) | test(within_budget) | test(krilld_kill) | test(exec_handoff))'
step "test latency"; slot cargo nextest run "${cargo_target_args[@]}" --workspace --locked --no-tests=pass --no-fail-fast -E 'binary(survive) | binary(reconnect) | binary(wait) | binary(attach) | binary(holder) | test(survive) | test(/_ms$/) | test(within_budget) | test(krilld_kill) | test(exec_handoff)'
# --memory: MK-066 메모리 게이트까지(nightly·선택). 병렬 부하에서 흔들려 기본 경로에는 넣지 않는다.
if [[ $run_memory -eq 1 ]]; then step "memory"; run scripts/memory-gate.sh; fi
print -P "%B== check.sh: 전부 통과%b"
