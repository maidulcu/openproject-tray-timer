#!/bin/bash
# Publish a release to the update server, so installed apps update themselves.
#
#   scripts/publish-update.sh ~/Downloads/dwl-timer_0.4.0_amd64.deb ["What changed"]
#
# 1. Download the .deb from the GitHub release (CI builds it from a v* tag).
# 2. This script signs it with the updater key (never leaves this computer), writes
#    latest.json and uploads both. Installed apps check latest.json every 6 hours.
#
# The key: ~/.tauri/dwl-timer.key (made with `npx tauri signer generate --ci -w ...`). Its public
# half is in src-tauri/tauri.conf.json (plugins.updater.pubkey). Lose the key and installed apps
# can't be updated any more: keep a backup somewhere safe.
#
# Settings (environment, all required):
#   UPDATE_SSH  ssh destination of the web server that hosts the updates (user@host or an alias)
#   UPDATE_DIR  folder on that server served at UPDATE_URL, e.g. /var/www/timer
#   UPDATE_URL  public URL of that folder, e.g. https://example.com/timer. The app checks
#               <UPDATE_URL>/latest.json, so it must match plugins.updater.endpoints in
#               src-tauri/tauri.conf.json.
#   TAURI_KEY   path to the signing key (default ~/.tauri/dwl-timer.key)
set -euo pipefail

DEB=${1:?usage: publish-update.sh <path to .deb> [notes]}
NOTES=${2:-}
KEY=${TAURI_KEY:-$HOME/.tauri/dwl-timer.key}
missing=()
for v in UPDATE_SSH UPDATE_DIR UPDATE_URL; do [ -n "${!v:-}" ] || missing+=("$v"); done
if [ ${#missing[@]} -gt 0 ]; then
  echo "Set ${missing[*]} first (see the top of this script). Example:" >&2
  echo "  UPDATE_SSH=deploy@example.com UPDATE_DIR=/var/www/timer UPDATE_URL=https://example.com/timer $0 $*" >&2
  exit 1
fi
SSH_HOST=$UPDATE_SSH
DIR=$UPDATE_DIR
URL=${UPDATE_URL%/}

name=$(basename "$DEB")
version=$(echo "$name" | sed -nE 's/^dwl-timer_([0-9]+\.[0-9]+\.[0-9]+[^_]*)_amd64\.deb$/\1/p')
[ -n "$version" ] || { echo "Expected a file named dwl-timer_<version>_amd64.deb, got $name" >&2; exit 1; }
[ -f "$KEY" ] || { echo "No signing key at $KEY" >&2; exit 1; }
head -c 4 "$DEB" | grep -q '!<ar' || { echo "$name is not a .deb package" >&2; exit 1; }

cd "$(dirname "$0")/.."
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
cp "$DEB" "$work/$name"
npx tauri signer sign --private-key-path "$KEY" --password "" "$work/$name" >/dev/null

python3 - "$work" "$name" "$version" "$URL" "$NOTES" <<'PY'
import json, sys, datetime, pathlib
work, name, version, url, notes = sys.argv[1:]
sig = pathlib.Path(work, name + ".sig").read_text().strip()
latest = {
    "version": version,
    "notes": notes,
    "pub_date": datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
    "platforms": {"linux-x86_64-deb": {"signature": sig, "url": f"{url}/{name}"}},
}
pathlib.Path(work, "latest.json").write_text(json.dumps(latest, indent=2) + "\n")
PY

ssh "$SSH_HOST" "mkdir -p '$DIR'"
# The package first, then latest.json, so it never points to a missing file.
scp -q "$work/$name" "$SSH_HOST:$DIR/$name"
scp -q "$work/latest.json" "$SSH_HOST:$DIR/latest.json.new"
ssh "$SSH_HOST" "mv '$DIR/latest.json.new' '$DIR/latest.json' && chmod 644 '$DIR/$name' '$DIR/latest.json'"
echo "Published $version: $URL/latest.json -> $URL/$name"
