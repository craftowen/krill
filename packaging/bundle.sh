#!/bin/bash
# MK-061: 네이티브 macOS 빌드 → 로컬 고정 인증서 개별 서명(TCC 권한 유지) → 검증 → ZIP.
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
profile=release
output="$root/packaging/dist"
bin_dir=
bump=
install_app=
repo=
usage() {
    echo "usage: $0 [--profile release|dev] [--output DIR] [--bin-dir DIR] [--bump patch|minor|major] [--repo OWNER/REPO] [--install [APP_PATH]]"
    echo "--bin-dir: 빌드를 생략하고 지정한 디렉터리의 다섯 바이너리를 패키징한다."
    echo "--bump: Cargo.toml [workspace.package].version을 SemVer 규칙으로 올리고 Cargo.lock을 갱신한다."
    echo "--repo: GitHub Releases 자동 업데이트 대상 저장소(기본: origin 원격 또는 craftowen/krill)."
    echo "--install: 기존 .app 디렉터리 inode를 보존한 채 제자리 업데이트한다(기본: /Applications/Krill.app)."
}
fail() { echo "$*" >&2; exit 1; }
while [ "$#" -gt 0 ]; do
    case "$1" in
        --help|-h) usage; exit 0 ;;
        --install)
            if [ "$#" -ge 2 ] && [ -n "$2" ] && [[ "$2" != --* ]]; then
                install_app=$2
                shift 2
            else
                install_app="/Applications/Krill.app"
                shift 1
            fi
            ;;
        --profile|--output|--bin-dir|--bump|--repo)
            [ "$#" -ge 2 ] && [ -n "$2" ] || fail "$1 requires a value"
            case "$1" in
                --profile) profile=$2 ;;
                --output) output=$2 ;;
                --bin-dir) bin_dir=$2 ;;
                --bump) bump=$2 ;;
                --repo) repo=$2 ;;
            esac
            shift 2 ;;
        *) usage >&2; fail "unknown argument: $1" ;;
    esac
done
[ "$(uname -s)" = Darwin ] || fail "macOS is required"
case "$profile" in release|dev) ;; *) fail "profile must be release or dev" ;; esac
if [ -z "$repo" ]; then
    origin_url=$(git -C "$root" remote get-url origin 2>/dev/null || true)
    if [[ "$origin_url" =~ github\.com[:/]([^/]+/[^/.]+)(\.git)?$ ]]; then
        repo="${BASH_REMATCH[1]}"
    else
        repo="craftowen/krill"
    fi
fi
[[ "$repo" =~ ^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$ ]] || fail "invalid --repo format (expected OWNER/REPO): $repo"
version=$(sed -n '/^\[workspace.package\]/,/^\[/{s/^version = "\([^"]*\)".*/\1/p;}' "$root/Cargo.toml")
[[ "$version" =~ ^([0-9]+)\.([0-9]+)\.([0-9]+)$ ]] || fail "unsupported workspace version: $version"
if [ -n "$bump" ]; then
    major=${BASH_REMATCH[1]}
    minor=${BASH_REMATCH[2]}
    patch=${BASH_REMATCH[3]}
    case "$bump" in
        patch) version="$major.$minor.$((patch + 1))" ;;
        minor) version="$major.$((minor + 1)).0" ;;
        major) version="$((major + 1)).0.0" ;;
        *) fail "bump must be patch, minor, or major" ;;
    esac
    python3 -c '
import pathlib, re, sys
p = pathlib.Path(sys.argv[1])
v = sys.argv[2]
text = p.read_text()
updated, n = re.subn(r"(?ms)(^\[workspace\.package\]\n.*?^version = \")[^\"]+(\")", rf"\g<1>{v}\2", text, count=1)
assert n == 1, "failed to update [workspace.package].version"
p.write_text(updated)
' "$root/Cargo.toml" "$version"
    (cd "$root" && cargo update -w)
fi
if [ -z "$bin_dir" ]; then
    cd "$root"
    # Cargo가 보고하는 실행 파일 경로로 target 설정·CARGO_TARGET_DIR·공백 경로를 반영한다.
    target=$(cargo build --locked --profile "$profile" --bins \
        -p krill-app -p krill -p krill-holder -p krilld -p krill-link --message-format=json | \
        python3 -c 'import json,sys
for line in sys.stdin:
    item=json.loads(line)
    if item.get("reason")=="compiler-artifact" and item.get("target",{}).get("name")=="krill-app" and item.get("executable"):
        print(item["executable"])')
    [ -n "$target" ] || fail "cargo did not report krill-app executable"
    bin_dir=$(dirname "$target")
fi
for binary in krill-app krill krill-holder krilld krill-link; do
    [ -f "$bin_dir/$binary" ] && [ -x "$bin_dir/$binary" ] || fail "missing executable: $bin_dir/$binary"
    /usr/bin/file -b "$bin_dir/$binary" | /usr/bin/grep -q 'Mach-O' || fail "not a Mach-O executable: $binary"
done
arch=$(/usr/bin/lipo -archs "$bin_dir/krill-app")
case "$arch" in arm64) ;; *) fail "Apple Silicon (arm64) is required: $arch" ;; esac
for binary in krill krill-holder krilld krill-link; do
    [ "$(/usr/bin/lipo -archs "$bin_dir/$binary")" = "$arch" ] || fail "architecture mismatch: $binary"
done
archive="Krill-$version-$arch-adhoc.zip"
mkdir -p "$output"
output=$(cd "$output" && pwd)
if [ -n "$install_app" ]; then
    rm -rf "$output/Krill.app" "${output:?}/${archive:?}" "${output:?}/${archive:?}.sha256"
else
    for artifact in Krill.app "$archive" "$archive.sha256"; do
        [ ! -e "$output/$artifact" ] && [ ! -L "$output/$artifact" ] || fail "output already exists: $output/$artifact"
    done
fi

stage=$(mktemp -d "$output/.krill.XXXXXX")
trap 'rm -rf "$stage"' EXIT

# TCC 권한이 리빌드·데몬 업그레이드 뒤에도 유지되도록 로컬 고정 코드 서명 인증서를 준비한다.
# ad-hoc 서명(--sign -)은 매 빌드마다 cdhash가 바뀌어 TCC 승인이 무효화되고 구 데몬과 신 앱이 충돌한다.
kc_path="${KRILL_CODESIGN_KEYCHAIN:-$(python3 "$root/packaging/macos/identity_paths.py")}"
kc_pass_file="$kc_path.pass"
cs_identity="Krill Local Code Signing"
ensure_codesign_identity() {
    if [ -e "$kc_path" ] || [ -e "$kc_pass_file" ]; then
        [ -f "$kc_path" ] && [ -f "$kc_pass_file" ] || fail "incomplete existing signing identity; refusing to replace it"
        local pass
        pass=$(cat "$kc_pass_file")
        /usr/bin/security unlock-keychain -p "$pass" "$kc_path" 2>/dev/null || fail "cannot unlock existing signing keychain"
        if /usr/bin/security find-identity -p codesigning "$kc_path" 2>/dev/null | /usr/bin/grep -Fq "\"$cs_identity\""; then
            return 0
        fi
        fail "existing signing certificate unavailable; refusing to replace it"
    fi
    mkdir -p "$(dirname "$kc_path")" "$stage/codesign"
    local pass="krill-local-codesign"
    local old_umask
    old_umask=$(umask)
    umask 077
    printf '%s' "$pass" > "$kc_pass_file"
    umask "$old_umask"
    cat > "$stage/codesign/openssl.cnf" <<'CNF'
[req]
distinguished_name = dn
x509_extensions = v3_cs
prompt = no
[dn]
CN = Krill Local Code Signing
O = Krill
[v3_cs]
basicConstraints = critical, CA:FALSE
keyUsage = critical, digitalSignature
extendedKeyUsage = critical, codeSigning
subjectKeyIdentifier = hash
CNF
    /usr/bin/openssl req -x509 -newkey rsa:2048 -nodes \
        -keyout "$stage/codesign/key.pem" \
        -out "$stage/codesign/cert.pem" \
        -days 3650 -config "$stage/codesign/openssl.cnf" 2>/dev/null
    /usr/bin/openssl pkcs12 -export -legacy \
        -out "$stage/codesign/cert.p12" \
        -inkey "$stage/codesign/key.pem" \
        -in "$stage/codesign/cert.pem" \
        -name "$cs_identity" \
        -passout "pass:$pass" 2>/dev/null || \
    /usr/bin/openssl pkcs12 -export \
        -out "$stage/codesign/cert.p12" \
        -inkey "$stage/codesign/key.pem" \
        -in "$stage/codesign/cert.pem" \
        -name "$cs_identity" \
        -passout "pass:$pass" 2>/dev/null
    rm -f "$kc_path"
    /usr/bin/security create-keychain -p "$pass" "$kc_path"
    /usr/bin/security set-keychain-settings "$kc_path"
    /usr/bin/security unlock-keychain -p "$pass" "$kc_path"
    /usr/bin/security import "$stage/codesign/cert.p12" -k "$kc_path" -P "$pass" \
        -T /usr/bin/codesign -T /usr/bin/security >/dev/null
    /usr/bin/security set-key-partition-list \
        -S apple-tool:,apple:,codesign: -s -k "$pass" "$kc_path" >/dev/null
}
ensure_codesign_identity

update_key_file="${KRILL_UPDATE_KEY_FILE:-$kc_path.ed25519.key}"
update_pub_key=$(python3 "$root/packaging/macos/ed25519_manifest.py" keygen --key-file "$update_key_file")
[[ "$update_pub_key" =~ ^[0-9a-f]{64}$ ]] || fail "failed to derive 64-hex Ed25519 public key"
cert_sha1=$(/usr/bin/security find-certificate -c "$cs_identity" -Z "$kc_path" | /usr/bin/awk '/SHA-1 hash:/{print tolower($3); exit}')
[[ "$cert_sha1" =~ ^[0-9a-f]{40}$ ]] || fail "failed to extract SHA-1 fingerprint for $cs_identity"
update_url="https://github.com/$repo/releases/latest/download/latest.json"

app="$stage/Krill.app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Helpers" "$app/Contents/Resources"
for binary in krill-app krill; do
    cp -X "$bin_dir/$binary" "$app/Contents/MacOS/$binary"
done
for binary in krill-holder krilld krill-link; do
    cp -X "$bin_dir/$binary" "$app/Contents/Helpers/$binary"
done
[ -f "$root/packaging/macos/AppIcon.icns" ] || fail "missing icon: $root/packaging/macos/AppIcon.icns"
cp -X "$root/packaging/macos/AppIcon.icns" "$app/Contents/Resources/AppIcon.icns"
[ -f "$root/packaging/macos/krill-chime.caf" ] || fail "missing sound: $root/packaging/macos/krill-chime.caf"
cp -X "$root/packaging/macos/krill-chime.caf" "$app/Contents/Resources/krill-chime.caf"
cat > "$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
    <key>CFBundleIdentifier</key><string>dev.krill.app</string>
    <key>CFBundleExecutable</key><string>krill-app</string>
    <key>CFBundleIconFile</key><string>AppIcon</string>
    <key>CFBundleName</key><string>Krill</string>
    <key>CFBundleDisplayName</key><string>Krill</string>
    <key>CFBundlePackageType</key><string>APPL</string>
    <key>CFBundleShortVersionString</key><string>$version</string>
    <key>CFBundleVersion</key><string>$version</string>
    <key>KrillUpdatePublicKey</key><string>$update_pub_key</string>
    <key>KrillUpdateURL</key><string>$update_url</string>
    <key>KrillUpdateDesignatedRequirement</key><string>identifier &quot;dev.krill.app&quot; and certificate root = H&quot;$cert_sha1&quot;</string>
    <key>NSHighResolutionCapable</key><true/>
    <key>NSDocumentsFolderUsageDescription</key><string>Krill이 워크스페이스 및 에이전트 세션의 문서 폴더 파일에 접근합니다.</string>
    <key>NSDesktopFolderUsageDescription</key><string>Krill이 워크스페이스 및 에이전트 세션의 데스크톱 폴더 파일에 접근합니다.</string>
    <key>NSDownloadsFolderUsageDescription</key><string>Krill이 워크스페이스 및 에이전트 세션의 다운로드 폴더 파일에 접근합니다.</string>
</dict></plist>
PLIST
/usr/bin/plutil -lint "$app/Contents/Info.plist"

# 안쪽부터 개별 서명. 헬퍼는 고유 identifier, --deep은 사용하지 않는다.
for entry in 'Helpers/krill-holder holder' 'Helpers/krilld krilld' 'Helpers/krill-link link' 'MacOS/krill cli'; do
    read -r path suffix <<< "$entry"
    /usr/bin/codesign --force --keychain "$kc_path" --sign "$cs_identity" \
        --identifier "dev.krill.app.$suffix" "$app/Contents/$path"
done
/usr/bin/codesign --force --keychain "$kc_path" --sign "$cs_identity" \
    --identifier dev.krill.app "$app"
"$root/packaging/macos/verify.sh" "$app"
/usr/bin/ditto -c -k --sequesterRsrc --keepParent "$app" "$stage/$archive"
# ZIP에 서명이 보존되는지도 검사한 후 결과 디렉터리로 옮긴다.
mkdir "$stage/unpacked"
/usr/bin/ditto -x -k "$stage/$archive" "$stage/unpacked"
"$root/packaging/macos/verify.sh" "$stage/unpacked/Krill.app"
(cd "$stage" && /usr/bin/shasum -a 256 "$archive" > "$archive.sha256")
mv "$app" "$output/Krill.app"
mv "$stage/$archive" "$stage/$archive.sha256" "$output/"
if [ -n "$install_app" ]; then
    # XNU responsibility가 추적하는 번들·하위 디렉터리 inode는 유지하되,
    # Apple Silicon XNU UBC cs_blob 캐시 충돌(SIGKILL -9)을 막기 위해 일반 파일은 임시 파일 후 rename으로 새 vnode를 부여한다.
    python3 -c '
import os, pathlib, shutil, sys
src = pathlib.Path(sys.argv[1])
dst = pathlib.Path(sys.argv[2])
dst.mkdir(parents=True, exist_ok=True)
for root, dirs, files in os.walk(src):
    rel = pathlib.Path(root).relative_to(src)
    target_dir = dst / rel
    target_dir.mkdir(parents=True, exist_ok=True)
    for d in dirs:
        (target_dir / d).mkdir(exist_ok=True)
    for f in files:
        s = pathlib.Path(root) / f
        t = target_dir / f
        if s.is_symlink():
            # 링크를 따라가 복사하면 서명 봉인이 깨지고, 기존 링크면 대상 파일을 덮어쓴다.
            if t.is_symlink() or t.exists():
                t.unlink()
            os.symlink(os.readlink(s), t)
            continue
        tmp = target_dir / f".{f}.tmp.{os.getpid()}"
        shutil.copy2(s, tmp)
        os.replace(tmp, t)
for root, dirs, files in os.walk(dst, topdown=False):
    rel = pathlib.Path(root).relative_to(dst)
    src_dir = src / rel
    for f in files:
        if not (src_dir / f).exists():
            (pathlib.Path(root) / f).unlink()
    for d in dirs:
        if not (src_dir / d).exists():
            (pathlib.Path(root) / d).rmdir()
' "$output/Krill.app" "$install_app"
    "$root/packaging/macos/verify.sh" "$install_app"
    echo "installed in-place: $install_app"
fi
echo "local certificate signed (not notarized): $output/$archive"

