#!/usr/bin/env bash
# 최신 origin/dev 기준으로 /Volumes/krill-build/targets/shared 캐시를 안전하게 워밍/갱신한다.
# 새 워크트리는 이 디렉터리를 `cp -cR /Volumes/krill-build/targets/shared /Volumes/krill-build/targets/<워크트리>`
# (APFS CoW 클론)로 시드해 초기 빌드 시간을 크게 줄인다.
#
# 주의:
# - CARGO_TARGET_DIR 환경변수를 쓰지 않고 항상 `--target-dir` 플래그를 전달한다(sccache 히트율 보존).
# - 현재 작업 트리를 건드리지 않고 임시 detached git worktree에서 빌드한다.
# - 전용 락(/tmp/krill-refresh-shared-target.lock)으로 중복 갱신을 막고, 기존 워크트리 빌드 락과 충돌하지 않는다.
set -euo pipefail

usage() {
    cat <<'EOF'
Usage: scripts/refresh-shared-target.sh [options]
  --ref REF          Git ref to warm from (default: origin/dev)
  --target-dir DIR   Shared target directory (default: /Volumes/krill-build/targets/shared)
  --clean            Rebuild into a clean directory on the build volume and swap into place
  --no-fetch         Skip `git fetch origin` before creating the temporary worktree
  --dry-run          Validate mount/ref and print commands without building
  -h, --help         Show this help
EOF
}

die() { printf 'refresh-shared-target: %s\n' "$*" >&2; exit 1; }

orig_args=("$@")
ref="origin/dev"
target_dir="/Volumes/krill-build/targets/shared"
clean=false
do_fetch=true
dry_run=false

while [[ $# -gt 0 ]]; do
    case "$1" in
        --ref)
            [[ $# -ge 2 && -n "$2" && "$2" != -* ]] || die "--ref needs a git ref"
            ref=$2; shift 2 ;;
        --target-dir)
            [[ $# -ge 2 && -n "$2" && "$2" != -* ]] || die "--target-dir needs a directory"
            target_dir=$2; shift 2 ;;
        --clean) clean=true; shift ;;
        --no-fetch) do_fetch=false; shift ;;
        --dry-run) dry_run=true; shift ;;
        -h|--help) usage; exit 0 ;;
        *) usage >&2; die "unknown argument: $1" ;;
    esac
done

[[ $(uname -s) == Darwin ]] || die "requires macOS"
volume="/Volumes/krill-build"
targets_root="$volume/targets"
mount | grep -Fq " on $volume (" || die "external build volume is not mounted: $volume"

case "$target_dir" in
    "$targets_root"/*) ;;
    *) die "target-dir must be inside $targets_root: $target_dir" ;;
esac
[[ "$target_dir" != *..* ]] || die "target-dir must not contain '..': $target_dir"

repo_root=$(git -C "$(dirname "$0")/.." rev-parse --show-toplevel)
lock_file="/tmp/krill-refresh-shared-target.lock"

# 중복 실행 방지(비블로킹 락). 다른 워크트리의 Cargo 빌드 락과는 별개다.
if [[ -z "${KRILL_REFRESH_LOCKED:-}" ]]; then
    if command -v lockf >/dev/null 2>&1; then
        export KRILL_REFRESH_LOCKED=1
        rc=0
        lockf -s -t 0 "$lock_file" "$0" ${orig_args[@]+"${orig_args[@]}"} || rc=$?
        if [[ $rc -eq 75 ]]; then
            printf 'refresh-shared-target: another refresh holds %s; skipped\n' "$lock_file" >&2
            exit 0
        fi
        exit "$rc"
    fi
fi

# sccache 키 오염 및 잘못된 경로 상속을 막기 위해 target-dir 환경변수는 비운다.
unset CARGO_TARGET_DIR CARGO_BUILD_TARGET_DIR
unset KRILL_STATE_DIR CLAUDE_CONFIG_DIR CODEX_HOME XDG_CONFIG_HOME

if $do_fetch && [[ "$ref" == origin/* ]]; then
    branch="${ref#origin/}"
    if $dry_run; then
        printf '[dry-run] git -C %q fetch origin %q\n' "$repo_root" "$branch"
    else
        git -C "$repo_root" fetch origin "$branch"
    fi
fi

commit=$(git -C "$repo_root" rev-parse --verify "${ref}^{commit}") \
    || die "cannot resolve git ref: $ref"

worktree_dir=""
staging_dir=""
old_dir=""
cleanup() {
    local rc=$?
    if [[ -n "$worktree_dir" && -d "$worktree_dir" ]]; then
        git -C "$repo_root" worktree remove --force "$worktree_dir" >/dev/null 2>&1 || rm -rf "$worktree_dir"
    fi
    if [[ -n "$staging_dir" && -d "$staging_dir" ]]; then
        rm -rf "$staging_dir"
    fi
    if [[ -n "$old_dir" && -d "$old_dir" ]]; then
        rm -rf "$old_dir"
    fi
    return "$rc"
}
trap cleanup EXIT INT TERM HUP

build_target="$target_dir"
if $clean; then
    staging_dir="${target_dir}.staging.$$"
    build_target="$staging_dir"
fi

if $dry_run; then
    printf '[dry-run] ref=%s commit=%s\n' "$ref" "$commit"
    printf '[dry-run] target=%s (build=%s, clean=%s)\n' "$target_dir" "$build_target" "$clean"
    printf '[dry-run] cargo build --workspace --bins --locked --target-dir %q\n' "$build_target"
    printf '[dry-run] cargo nextest run --workspace --locked --no-tests=pass --no-run --target-dir %q\n' "$build_target"
    exit 0
fi

mkdir -p "$targets_root" "$volume/tmp"
worktree_dir=$(mktemp -d "$volume/tmp/refresh-shared-wt.XXXXXX")
rmdir "$worktree_dir"
git -C "$repo_root" worktree add --detach "$worktree_dir" "$commit"

mkdir -p "$build_target"

printf 'refresh-shared-target: warming %s from %s (%s)\n' "$build_target" "$ref" "${commit:0:12}"
(
    cd "$worktree_dir"
    cargo build --workspace --bins --locked --target-dir "$build_target"
    if command -v cargo-nextest >/dev/null 2>&1; then
        cargo nextest run --workspace --locked --no-tests=pass --no-run --target-dir "$build_target"
    else
        cargo test --workspace --locked --no-run --target-dir "$build_target"
    fi
)

printf 'ref=%s\ncommit=%s\nupdated_at=%s\n' \
    "$ref" "$commit" "$(date -u +%Y-%m-%dT%H:%M:%SZ)" > "$build_target/.shared-head"

if $clean; then
    if [[ -d "$target_dir" ]]; then
        old_dir="${target_dir}.old.$$"
        mv "$target_dir" "$old_dir"
    fi
    mv "$staging_dir" "$target_dir"
    staging_dir=""
    if [[ -n "$old_dir" && -d "$old_dir" ]]; then
        rm -rf "$old_dir"
        old_dir=""
    fi
fi

printf 'refresh-shared-target: done (%s @ %s)\n' "$target_dir" "${commit:0:12}"
