#!/usr/bin/env bash
# Offer a new NautiK9 build to the phone from the fleet, over Tailscale, with no adb.
# The phone downloads and checks it; the owner taps once on the phone to install (Android requires
# that for an app that is not from a store, and this script does not try to get around it).
#
#   PHONE=<phone tailnet address> TOKEN=<app token> android/push-update.sh [apk]
#
# The token is the phone's own access token (config/token); it is never printed. The APK is served
# on this machine's tailnet address only, for as long as the script runs.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
: "${PHONE:?set PHONE to the phone's tailnet address}"
: "${TOKEN:?set TOKEN to the app's access token}"
PORT=${PORT:-8399}
apk=${1:-$(ls -t "$here"/build/nautik9-*.apk | head -1)}
host=$(tailscale ip -4 | head -1)
sha=$(sha256sum "$apk" | cut -d' ' -f1)
dir=$(mktemp -d)
cp "$apk" "$dir/app.apk"
(cd "$dir" && python3 -m http.server "$PORT" --bind "$host" >/dev/null 2>&1) &
srv=$!
trap 'kill $srv 2>/dev/null; rm -rf "$dir"' EXIT
sleep 1
api="http://$PHONE:8340/api/admin"
auth=(-H "Authorization: Bearer $TOKEN")
echo "offering $(basename "$apk") ($sha)"
curl -fsS "${auth[@]}" -H 'Content-Type: application/json' \
     -d "{\"url\": \"http://$host:$PORT/app.apk\", \"sha256\": \"$sha\"}" "$api/update"
echo
echo "queued. The phone shows 'NautiK9 update ready' within a few seconds: the owner taps to install."
want=$(basename "$apk" .apk | sed 's/^nautik9-//')   # versionName; the app reports "<name> (<code>)"
for _ in $(seq 1 60); do
    sleep 5
    cur=$(curl -fsS -m 5 "${auth[@]}" "$api/version" 2>/dev/null | python3 -c 'import sys,json;print(json.load(sys.stdin).get("app") or "")' 2>/dev/null || true)
    case "$cur" in "$want "*) echo "installed: $cur"; exit 0;; esac
done
echo "not installed yet (current: ${cur:-unreachable}); check GET $api/pending and the phone" >&2
exit 2
