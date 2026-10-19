#!/bin/bash
# Local-only release assembly. MK-061 owns packaging/bundle.sh and app signing.
set -euo pipefail
cd "$(dirname "$0")/.."

usage() {
  echo 'usage: bash packaging/release.sh VERSION HOMEPAGE [SOURCE_URL DMG_URL] [APP OUTPUT_DIR]' >&2
  echo 'URL defaults are local file URLs. No upload, tap creation or push is performed.' >&2
  exit 2
}
[[ $# -ge 2 && $# -le 6 && $# -ne 3 && $# -ne 5 ]] || usage
version=$1
homepage=$2
[[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[A-Za-z0-9.-]+)?$ ]] || usage
app=${5:-packaging/dist/Krill.app}
output=${6:-packaging/dist}

# Validate everything before creating artifacts. A release always names a clean commit.
[[ "${KRILL_RELEASE_ALLOW_DIRTY:-0}" == "1" || -z "$(git status --porcelain --untracked-files=normal)" ]] || {
  echo 'release requires a clean worktree; commit the source first' >&2; exit 1;
}
git cat-file -e HEAD:packaging/bundle.sh 2>/dev/null || {
  echo 'MK-061 packaging/bundle.sh must be committed before preparing a source formula' >&2; exit 1;
}
python3 -B -c 'import sys; sys.path.insert(0, "packaging/homebrew"); from generate import artifact_url; [artifact_url(url) for url in sys.argv[1:]]' \
  "$homepage" "${3:-file:///local-source}" "${4:-file:///local-dmg}"
for executable in Contents/MacOS/krill-app Contents/MacOS/krill Contents/Helpers/krill-holder Contents/Helpers/krilld; do
  [[ -x "$app/$executable" ]] || { echo "missing executable: $app/$executable" >&2; exit 1; }
  codesign --verify --strict "$app/$executable"
done
codesign --verify --strict "$app"
[[ "$(/usr/libexec/PlistBuddy -c 'Print :CFBundleIdentifier' "$app/Contents/Info.plist")" == dev.krill.app ]] || {
  echo 'unexpected app bundle identifier' >&2; exit 1;
}
[[ "$(/usr/libexec/PlistBuddy -c 'Print :CFBundleShortVersionString' "$app/Contents/Info.plist")" == "$version" ]] || {
  echo 'bundle version does not match release version' >&2; exit 1;
}
source_version=$(git show HEAD:Cargo.toml | sed -n '/^\[workspace.package\]/,/^\[/s/^version = "\([^"]*\)"/\1/p')
[[ "$source_version" == "$version" ]] || { echo 'source version does not match release version' >&2; exit 1; }
mkdir -p "$output"
output=$(cd "$output" && pwd -P)
dmg="$output/Krill-$version.dmg"
source="$output/Krill-$version-source.tar.gz"
tap="$output/homebrew-krill"
[[ ! -e "$dmg" && ! -e "$source" && ! -e "$tap" ]] || {
  echo 'release artifacts already exist; choose a fresh output directory' >&2; exit 1;
}
stage=$(mktemp -d "${TMPDIR:-/tmp}/mk072-stage.XXXXXX")
rw_dmg="$stage-rw.dmg"
trap 'rm -rf "$stage" "$rw_dmg"' EXIT
ditto "$app" "$stage/Krill.app"
ln -s /Applications "$stage/Applications"
# Navy window with the coral krill and a drag hint. Needs Finder scripting (a logged-in GUI session);
# anywhere that fails (CI, headless, no Automation permission) the plain DMG below is built instead.
styled_dmg() {
  local mount ok=0
  mkdir "$stage/.background" && cp packaging/macos/dmg-background.png "$stage/.background/background.png" || return 1
  hdiutil create -quiet -volname Krill -srcfolder "$stage" -format UDRW "$rw_dmg" || return 1
  mount=$(hdiutil attach -readwrite -noverify -noautoopen "$rw_dmg" | awk -F '\t' '/\/Volumes\//{print $NF; exit}') || return 1
  [[ -n "$mount" ]] || return 1
  osascript - "$(basename "$mount")" >/dev/null <<'APPLESCRIPT' || ok=1
on run argv
  tell application "Finder"
    tell disk (item 1 of argv)
      open
      set current view of container window to icon view
      set toolbar visible of container window to false
      set statusbar visible of container window to false
      set bounds of container window to {200, 120, 860, 548}
      set viewOptions to icon view options of container window
      set arrangement of viewOptions to not arranged
      set icon size of viewOptions to 128
      set background picture of viewOptions to file ".background:background.png"
      set position of item "Krill.app" of container window to {165, 200}
      set position of item "Applications" of container window to {495, 200}
      update without registering applications
      delay 1
      close
    end tell
  end tell
end run
APPLESCRIPT
  sync
  hdiutil detach -quiet "$mount" || hdiutil detach -quiet -force "$mount" || return 1
  (( ok == 0 )) || return 1
  hdiutil convert -quiet "$rw_dmg" -format UDZO -o "$dmg"
}
if ! styled_dmg; then
  echo 'DMG background not applied (Finder scripting unavailable); building a plain DMG' >&2
  rm -rf "$stage/.background" "$dmg"
  hdiutil create -quiet -volname Krill -srcfolder "$stage" -format UDZO "$dmg"
fi
codesign --force --sign - --identifier dev.krill.app.dmg "$dmg"
codesign --verify --strict "$dmg"
hdiutil verify -quiet "$dmg"
git archive --format=tar --prefix="Krill-$version/" HEAD | gzip -n > "$source"
source_url=${3:-$(python3 -c 'import pathlib,sys; print(pathlib.Path(sys.argv[1]).as_uri())' "$source")}
dmg_url=${4:-$(python3 -c 'import pathlib,sys; print(pathlib.Path(sys.argv[1]).as_uri())' "$dmg")}
python3 packaging/homebrew/generate.py --version "$version" --homepage "$homepage" \
  --source "$source" --source-url "$source_url" --dmg "$dmg" --dmg-url "$dmg_url" --output "$tap"
git rev-parse HEAD > "$output/source-commit.txt"
echo "Local release prepared: $output"
echo 'Ad-hoc signing is not notarization; downloaded builds may be blocked by Gatekeeper.'
