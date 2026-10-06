#!/usr/bin/env bash
# Fetch the ipld/codec-fixtures corpus at the pinned commit named in
# bins/fauna-bridges/internal/dagcbor/CORPUS_COMMIT, and print the
# resolved fixture directory as the sole stdout line (callers capture it
# via command substitution; every other message goes to stderr).
#
# The corpus is canonical-encoding test data for IPLD codecs (DAG-CBOR,
# DAG-JSON, DAG-PB). We run it through the Go bridge's DAG-CBOR codec
# (Marshal/Unmarshal) and assert byte-for-byte round-trip canonicality.
# The dagcbor-fixtures{,-rust} just recipes wrap this script.
#
# Idempotent: if the resolved dir already exists at the pinned commit,
# the checkout is a no-op.

set -euo pipefail

REPO_ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
COMMIT_FILE="$REPO_ROOT/bins/fauna-bridges/internal/dagcbor/CORPUS_COMMIT"
COMMIT=$(tr -d '[:space:]' < "$COMMIT_FILE")

# Resolve the scratch dir portably — single source of truth so the
# justfile recipes never re-derive (and risk drifting from) this literal.
# An explicit override wins; else the primary Linux dev machine's dedicated
# scratch dataset when present (bit-for-bit the pre-existing dev-machine/CI
# behavior); else a plain platform tmp dir, since some platforms have no
# /work (`/` is read-only there).
if [[ -n "${DAGCBOR_FIXTURES_DIR:-}" ]]; then
    DEST="$DAGCBOR_FIXTURES_DIR"
elif [[ -d /work ]]; then
    DEST=/work/tmp/codec-fixtures
else
    DEST="${TMPDIR:-/tmp}/fauna-codec-fixtures"
fi

if [[ ! -d "$DEST/.git" ]]; then
    # Fresh clone — first run on this machine.
    mkdir -p "$(dirname -- "$DEST")"
    git clone --quiet https://github.com/ipld/codec-fixtures "$DEST"
fi

# Refresh refs in case the pinned commit was added since the last clone
# (cheap fetch; --quiet suppresses progress, the 2>/dev/null guards
# against fetching a commit that already exists locally).
git -C "$DEST" fetch --quiet origin "$COMMIT" 2>/dev/null || true

# Pin to the exact commit. If the checkout would lose local edits the
# command fails loudly; the corpus is read-only by contract, so this
# never legitimately fires.
git -C "$DEST" checkout --quiet "$COMMIT"

echo "codec-fixtures: $DEST @ $COMMIT" >&2
echo "$DEST"
