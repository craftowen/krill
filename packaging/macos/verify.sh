#!/bin/bash
# 번들 및 개별 실행 파일을 검사한다. Gatekeeper 공증 검사가 아니다.
set -euo pipefail
[ "$#" -eq 1 ] || { echo "usage: $0 Krill.app" >&2; exit 1; }
app=$1
/usr/bin/plutil -lint "$app/Contents/Info.plist"
[ "$(/usr/bin/plutil -extract CFBundleIdentifier raw -o - "$app/Contents/Info.plist")" = dev.krill.app ]
[ "$(/usr/bin/plutil -extract CFBundleExecutable raw -o - "$app/Contents/Info.plist")" = krill-app ]
for entry in 'Helpers/krill-holder holder' 'Helpers/krilld krilld' 'Helpers/krill-link link' 'MacOS/krill cli' 'MacOS/krill-app app'; do
    read -r path suffix <<< "$entry"
    file="$app/Contents/$path"
    /usr/bin/codesign --verify --strict --verbose=2 "$file"
    identifier=dev.krill.app
    [ "$suffix" = app ] || identifier="$identifier.$suffix"
    /usr/bin/codesign -d --verbose=2 "$file" 2>&1 | /usr/bin/grep -Fx "Identifier=$identifier"
done
/usr/bin/codesign --verify --strict --verbose=2 "$app"
