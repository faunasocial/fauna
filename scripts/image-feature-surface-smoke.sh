#!/usr/bin/env bash
# image-feature-surface-smoke.sh <host:port>
#
# Asserts a BOOTED nest image serves one route per compiled-in bridge plane —
# ActivityPub, Nostr, Bluesky — from that plane's own handler, never the
# router's landing-page fallback ("<h1>Fauna nest</h1>"). Run by build-nest-image.yml's
# smoke steps on the freshly built image, before anything is pushed.
#
# Why on the artifact: from 2026-09-02 to 2026-10-04 every image shipped without
# all three planes (the Dockerfile's `--features fauna-nest/<feat>` bound to
# fauna-nest's self dev-dependency and was dropped), while every source-tree
# test stayed green and the health probe said ok. Only the image can show which
# features its binary was built with. Probes https (the domainless box's
# self-signed floor) and falls back to http, like the health probe beside it.

set -euo pipefail

addr=${1:?usage: image-feature-surface-smoke.sh <host:port>}
landing='<h1>Fauna nest</h1>'
fail=0

fetch() {
  curl -sk --max-time 10 "https://$addr$1" 2>/dev/null \
    || curl -s --max-time 10 "http://$addr$1" 2>/dev/null \
    || true
}

check() { # <plane> <path> [<required substring>]
  local body
  body=$(fetch "$2")
  if [ -z "$body" ]; then
    echo "::error::$1: $2 returned nothing"
    fail=1
  elif printf '%s' "$body" | grep -qF "$landing"; then
    echo "::error::$1: $2 fell through to the landing page — the image was built without the $1 feature"
    fail=1
  elif [ -n "${3:-}" ] && ! printf '%s' "$body" | grep -qF "$3"; then
    echo "::error::$1: $2 lacks '$3': $(printf '%s' "$body" | head -c 300)"
    fail=1
  else
    echo "$1: $2 served by its handler"
  fi
}

check activitypub /.well-known/nodeinfo '/nodeinfo/2.1'
check nostr /.well-known/nostr.json '"names"'
# No required substring: a domainless box answers with its "Bluesky OAuth is
# unavailable" explanation, a domained one with the client-metadata JSON —
# both the plane's own handler, neither the landing page.
check bluesky /.well-known/atproto-oauth-client

exit "$fail"
