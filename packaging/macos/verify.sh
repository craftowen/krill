#!/bin/bash
# Verify local signatures by default; --notarized also checks distribution policy.
set -euo pipefail
notarized=0
if [ "${1:-}" = --notarized ]; then
    notarized=1
    shift
fi
[ "$#" -eq 1 ] || { echo "usage: $0 [--notarized] Krill.app" >&2; exit 1; }
app=$1
/usr/bin/plutil -lint "$app/Contents/Info.plist"
[ "$(/usr/bin/plutil -extract CFBundleIdentifier raw -o - "$app/Contents/Info.plist")" = dev.krill.app ]
[ "$(/usr/bin/plutil -extract CFBundleExecutable raw -o - "$app/Contents/Info.plist")" = krill-app ]
team=
for entry in 'Helpers/krill-holder holder' 'Helpers/krilld krilld' 'Helpers/krill-link link' 'MacOS/krill cli' 'MacOS/krill-app app'; do
    read -r path suffix <<< "$entry"
    file="$app/Contents/$path"
    /usr/bin/codesign --verify --strict --verbose=2 "$file"
    identifier=dev.krill.app
    [ "$suffix" = app ] || identifier="$identifier.$suffix"
    /usr/bin/codesign -d --verbose=2 "$file" 2>&1 | /usr/bin/grep -Fx "Identifier=$identifier"
    if [ "$notarized" -eq 1 ]; then
        # Require an Apple certificate chain as well as the Developer ID label.
        /usr/bin/codesign --verify --strict '-R=anchor apple generic' "$file"
        info=$(/usr/bin/codesign -d --verbose=4 "$file" 2>&1)
        /usr/bin/grep -q '^Authority=Developer ID Application:' <<< "$info"
        /usr/bin/grep -Eq '^CodeDirectory .*flags=.*\(.*runtime.*\)' <<< "$info"
        /usr/bin/grep -Eq '^Timestamp=.+' <<< "$info"
        file_team=$(/usr/bin/sed -n 's/^TeamIdentifier=//p' <<< "$info")
        [[ "$file_team" =~ ^[A-Z0-9]{10}$ ]] || { echo "missing Developer ID team: $file" >&2; exit 1; }
        if [ -z "$team" ]; then team=$file_team; fi
        [ "$team" = "$file_team" ] || { echo "Developer ID team mismatch: $file" >&2; exit 1; }
    fi
done
/usr/bin/codesign --verify --strict --verbose=2 "$app"
if [ "$notarized" -eq 1 ]; then
    /usr/sbin/spctl --assess --type execute --verbose=2 "$app"
    /usr/bin/xcrun stapler validate "$app"
fi
