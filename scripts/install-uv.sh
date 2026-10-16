#!/usr/bin/env bash
# Install uv from a PINNED release whose installer script is checksum-verified
# before it runs.
#
# Why this exists: `curl -LsSf https://astral.sh/uv/install.sh | sh` fetches a
# mutable script on every run and executes it unread, so whoever controls that
# URL (or the network path to it) controls what the runner executes -- in the
# `build` job of `release-macos.yml` that is the job whose output becomes the
# SIGNED macOS artifact. `docs/goal/architecture/release-integrity.md` § Third-
# party GitHub Actions records the ruling; `test_public_workflows_shape.py`
# holds it (no `curl|sh` install in a shipped workflow).
#
# The installer is the VERSIONED `uv-installer.sh` of one release, and it embeds
# the checksums of that release's binaries, so pinning its digest pins the whole
# chain. Bumping uv is two edits in this file -- `v` and `digest` -- the digest
# being the release asset's published content digest:
#   gh api repos/astral-sh/uv/releases/tags/<v> \
#     --jq '.assets[] | select(.name=="uv-installer.sh") | .digest'
#
# The installer adds uv to the job's PATH through $GITHUB_PATH, exactly as the
# unpinned one did. Every workflow, shipped or internal, invokes this as
# `bash scripts/install-uv.sh` after its checkout, so it needs no executable bit
# and one path resolves in both trees (it ships via the allowlist).
set -euo pipefail

v=0.11.16
digest="b9f925505899533f36a3acfdf8684c661ff2d5c8735f759fca768367b5996123"

dir="$(mktemp -d)"
trap 'rm -rf "$dir"' EXIT
curl -fsSL -o "$dir/uv-installer.sh" \
  "https://github.com/astral-sh/uv/releases/download/${v}/uv-installer.sh"

if command -v sha256sum >/dev/null 2>&1; then
  echo "${digest}  $dir/uv-installer.sh" | sha256sum -c -
else
  echo "${digest}  $dir/uv-installer.sh" | shasum -a 256 -c -
fi

sh "$dir/uv-installer.sh"
