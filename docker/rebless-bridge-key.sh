#!/bin/bash
# Re-bless a bridge role's pubkey from its CURRENT keyfile — the root-mediated
# half of service-user re-keying (mail-bridge-lifecycle.md § Service-user
# re-keying). Called AS ROOT by the fauna-mail-bridge-{mta,mda} s6 run-scripts
# before their privilege drop, on every service start.
#
# Why: on an admin key rotation the bridge archives its revoked keypair and
# generates a fresh one (bridge-UID write, inside its own 0700 key subdir), but
# the blessed registry /data/keys/blessed/<role>.pub is deliberately root-owned
# and bridge-UID-unwritable (security.md § Enrollment
# proof-of-possession contract), so the bridge process alone cannot complete a
# strict-mode re-key. This script closes that loop: at the supervisor restart
# that follows the regeneration, root re-derives the blessed pubkey from the
# keyfile at the UID-isolated path and republishes it. nest re-reads the
# registry file per enrollment (FAUNA_BLESSED_KEYS_DIR), so the fresh key
# enrolls without a nest restart.
#
# Trust anchor: "the key at the root-verified, UID-isolated path
# /data/keys/<role>/<role>.key". A co-resident NON-<role> UID cannot write that
# path (0700 subdir), so it still cannot get a rogue key blessed; the <role>
# UID itself already holds the role's private key, so re-deriving from its
# keyfile grants it no capability it lacks. --print-pubkey is load-only for an
# existing keyfile (never re-mints), so a normal restart is a no-op.
#
# Usage: rebless-bridge-key.sh <mta|mda>
set -eu
role="$1"
keyfile="/data/keys/${role}/${role}.key"
blessed="/data/keys/blessed/${role}.pub"

# No keyfile yet (first boot ordering, or a dev volume) — the entrypoint mint
# owns creation; nothing to re-bless.
[ -f "$keyfile" ] || exit 0

if ! pub="$(fauna-mail-bridge --keypair-file "$keyfile" --print-pubkey)"; then
    echo "WARN: rebless-bridge-key: could not read ${keyfile}; leaving ${blessed} as-is" >&2
    exit 0
fi
if [ "$pub" = "$(cat "$blessed" 2>/dev/null || true)" ]; then
    exit 0
fi
mkdir -p /data/keys/blessed
chown root:root /data/keys/blessed
chmod 0755 /data/keys/blessed
printf '%s\n' "$pub" > "$blessed"
chown root:root "$blessed"
chmod 0644 "$blessed"
echo "Re-blessed ${role} pubkey from its keyfile (service-user re-keying): ${pub}"
