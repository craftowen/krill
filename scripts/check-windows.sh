#!/usr/bin/env bash
# macOS ARM 호스트의 Windows GNU 컴파일 검사. Homebrew cargo/LLVM은 그대로 사용한다.
set -euo pipefail

usage() {
    cat <<'EOF'
Usage: scripts/check-windows.sh [--bootstrap] [-p crate]... [--clippy] [--target-dir DIR]
  --bootstrap       Install isolated Rust 1.98.1 and MinGW headers, then exit.
  -p crate          Check selected packages (default: --workspace).
  --clippy          Run official Clippy with -D warnings instead of cargo check.
  --target-dir DIR  Override CARGO_TARGET_DIR (default: target/windows-gnu).

Requires Apple Silicon macOS, Homebrew cargo and LLVM (llvm@22 or llvm).
Installations and downloads live in ~/.cache/krill-win; rustup is not used.
This checks compilation only; Windows MSVC release shaders require Windows CI.
EOF
}

die() { printf 'check-windows: %s\n' "$*" >&2; exit 1; }
bootstrap=false
clippy=false
packages=()
target_args=()
while [[ $# -gt 0 ]]; do
    case "$1" in
        --bootstrap) bootstrap=true; shift ;;
        --clippy) clippy=true; shift ;;
        -p|--package)
            [[ $# -ge 2 && -n "$2" && "$2" != -* ]] || die "$1 needs a crate name"
            packages+=( -p "$2" ); shift 2 ;;
        --target-dir)
            [[ $# -ge 2 && -n "$2" && "$2" != -* ]] || die "$1 needs a directory"
            target_args=( --target-dir "$2" ); shift 2 ;;
        -h|--help) usage; exit 0 ;;
        *) usage >&2; die "unknown argument: $1" ;;
    esac
done
[[ $(uname -s) == Darwin && $(uname -m) == arm64 ]] || die 'requires Apple Silicon macOS'

cache="$HOME/.cache/krill-win"
version=1.98.1
host=aarch64-apple-darwin
toolchain="$cache/rust-$version"
headers="$cache/mingw64/include"
header_package=mingw-w64-x86_64-headers-git-13.0.0.r91.gfc0b67305-1-any.pkg.tar.zst
header_sha256=d1b3d8d86d1ddd2db0e205ad248ee1414ab21ee78f4b740c681fb9c96320611c

command -v brew >/dev/null || die 'Homebrew is required (cargo and LLVM)'
llvm_prefix=''
for formula in llvm@22 llvm; do
    candidate=$(brew --prefix "$formula" 2>/dev/null) || continue
    if [[ -x "$candidate/bin/clang" && -x "$candidate/bin/clang++" &&
          -x "$candidate/bin/llvm-ar" && -x "$candidate/bin/llvm-windres" ]]; then
        llvm_prefix=$candidate
        break
    fi
done
[[ -n "$llvm_prefix" ]] || die 'install Homebrew llvm@22 or llvm (clang, llvm-ar, llvm-windres)'
command -v cargo >/dev/null || die 'Homebrew cargo is required'

# 다운로드 중인 파일은 재사용하지 않고 검증된 아카이브만 캐시한다.
download() {
    local url=$1 destination=$2
    curl --fail --location --silent --show-error --retry 3 \
        --connect-timeout 30 --max-time 600 "$url" -o "$destination.part"
    mv "$destination.part" "$destination"
}

if $bootstrap; then
    mkdir -p "$cache/downloads"
    staging=$(mktemp -d "$cache/bootstrap.XXXXXX")
    trap 'rm -rf "$staging"' EXIT
    if [[ ! -f "$toolchain/.complete" ]]; then
        for component in "rustc-$version-$host" "rust-std-$version-$host" \
            "rust-std-$version-x86_64-pc-windows-gnu" \
            "rust-std-$version-x86_64-pc-windows-msvc" "clippy-$version-$host"; do
            archive="$component.tar.xz"
            url="https://static.rust-lang.org/dist/$archive"
            download "$url.sha256" "$cache/downloads/$archive.sha256"
            if [[ ! -f "$cache/downloads/$archive" ]]; then
                printf 'Downloading %s\n' "$archive"
                download "$url" "$cache/downloads/$archive"
            fi
            (cd "$cache/downloads" && shasum -a 256 -c "$archive.sha256")
            tar -xJf "$cache/downloads/$archive" -C "$staging"
            "$staging/$component/install.sh" --prefix="$staging/toolchain" --disable-ldconfig
            rm -rf "$staging/$component"
        done
        # 설치 중인 툴체인으로 검사를 실행하지 않는다.
        rm -rf "$toolchain"
        mv "$staging/toolchain" "$toolchain"
        touch "$toolchain/.complete"
    fi
    if [[ ! -f "$headers/_mingw.h" ]]; then
        archive="$cache/downloads/$header_package"
        [[ -f "$archive" ]] || download "https://repo.msys2.org/mingw/mingw64/$header_package" "$archive"
        printf '%s  %s\n' "$header_sha256" "$archive" | shasum -a 256 -c -
        # macOS bsdtar는 zstd를 읽는다. 라이브러리·실행 파일은 풀지 않는다.
        tar -xf "$archive" -C "$staging" mingw64/include
        mkdir -p "$cache/mingw64"
        mv "$staging/mingw64/include" "$headers"
    fi
    "$toolchain/bin/rustc" --version
    printf 'Windows check prerequisites installed in %s\n' "$cache"
    exit 0
fi

[[ -f "$toolchain/.complete" && -f "$headers/_mingw.h" ]] || \
    die 'prerequisites missing; run scripts/check-windows.sh --bootstrap first'

export PATH="$toolchain/bin:$llvm_prefix/bin:$PATH"
export RUSTC="$toolchain/bin/rustc"
export RUSTDOC="$toolchain/bin/rustdoc"
export CC_x86_64_pc_windows_gnu="$llvm_prefix/bin/clang"
export CXX_x86_64_pc_windows_gnu="$llvm_prefix/bin/clang++"
export AR_x86_64_pc_windows_gnu="$llvm_prefix/bin/llvm-ar"
export RC_x86_64_pc_windows_gnu="$llvm_prefix/bin/llvm-windres"
# cc-rs가 공백이 있는 캐시 경로의 따옴표를 해석하도록 한다.
export CC_SHELL_ESCAPED_FLAGS=1
export CFLAGS_x86_64_pc_windows_gnu="--target=x86_64-w64-windows-gnu -isystem \"$headers\""
export CXXFLAGS_x86_64_pc_windows_gnu="$CFLAGS_x86_64_pc_windows_gnu"
# boost 하네스와 같은 BLAKE3 pure 경로. MSVC 크로스 검사에서 어셈블러를 우회하며 GNU에도 유지한다.
export CARGO_FEATURE_PURE=1
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-target/windows-gnu}"
if [[ ${#packages[@]} -eq 0 ]]; then packages=( --workspace ); fi
subcommand=check
if $clippy; then subcommand=clippy; fi
cd "$(dirname "$0")/.."
# Bash 3.2 + nounset에서도 빈 배열을 안전하게 처리한다.
args=( "$subcommand" --locked --target x86_64-pc-windows-gnu --all-targets "${packages[@]}" )
if [[ ${#target_args[@]} -gt 0 ]]; then args+=( "${target_args[@]}" ); fi
if $clippy; then args+=( -- -D warnings ); fi
exec cargo "${args[@]}"
