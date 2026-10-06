#!/bin/bash
set -euo pipefail

# ── Parse CLI flags and FAUNA_* env vars ────────────────────────────
# NB: there is no FAUNA_DOMAIN. The box boots DOMAINLESS and learns its domain
# from the admin's claim handle (`nest_identity_domain` — the only configuration
# surface is the client; domains-and-tls-bootstrap.md § Env contract).
# Nor is there a pull target: which nest a private box syncs from, and whether
# a user's posts are forwarded there, are that user's in-app pairing row
# (private-mode.md § Implementation status today). `FAUNA_MODE` is read by the
# nest binary itself (the pre-claim NAT seed — see the NAT-mode note below).
PORT="${FAUNA_PORT:-3000}"
# Bind address for nest's internal client-facing listener. Default 0.0.0.0 (all
# interfaces): correct for the bridge-networked production image, where the
# container network namespace isolates :${PORT} — only the co-resident processes
# (SNI router, MTA, relay sidecar, all dialing 127.0.0.1:${PORT}) and the
# explicitly published router ports reach it. The host-networking home bundle
# (docker-compose.home.yml) sets FAUNA_BIND_ADDR=127.0.0.1 so nest's WS-RPC/HTTP
# surface is NOT bound on the host's LAN interfaces (only the SNI-router :443 and
# the MDA's IMAP on ${FAUNA_LAN_BIND_IP} are LAN-exposed there). Artifact-set IPC
# wiring, never a human config knob. First-run only (baked
# into the persisted nest.toml `listen`, like the port).
BIND_ADDR="${FAUNA_BIND_ADDR:-0.0.0.0}"
# Optional ACME directory-URL override. When set, the nest's HTTP-01 client
# orders against this CA instead of Let's Encrypt (a private/internal ACME CA,
# another public CA, or the in-network `pebble` of the tier_4 acceptance).
ACME_DIRECTORY_URL="${FAUNA_ACME_DIRECTORY_URL:-}"
CORS_ORIGINS="${FAUNA_CORS_ORIGINS:-}"

while [[ $# -gt 0 ]]; do
    case "$1" in
        --port)          PORT="$2"; shift 2 ;;
        *) echo "Unknown flag: $1" >&2; exit 1 ;;
    esac
done

# ── First-run setup (no existing config) ───────────────────────────
if [ ! -f /data/nest.toml ]; then
    echo "=== First run: initializing nest ==="

    mkdir -p /data/blobs /data/acme

    # Start from canonical default config
    cp /etc/fauna/default.toml /data/nest.toml

    # ── Apply listen (bind address + port) override ────────────────
    # Both default to the canonical `0.0.0.0:3000` already in default.toml, so
    # this is a no-op (byte-identical nest.toml) when neither env is set — the
    # bridge-networked production image is unchanged. The home bundle sets
    # FAUNA_BIND_ADDR=127.0.0.1 (loopback) to drop the LAN exposure under host
    # networking.
    if [ "$PORT" != "3000" ] || [ "$BIND_ADDR" != "0.0.0.0" ]; then
        sed -i "s/listen = \"0.0.0.0:3000\"/listen = \"${BIND_ADDR}:${PORT}\"/" /data/nest.toml
    fi

    # ── NAT mode: no longer baked into nest.toml ───────────────────
    # The NAT axis (public/private) is now CLIENT-SET: a mutable `nest_nat_mode`
    # DB row written from the onboarding wizard / admin panel via
    # `fauna.setup.nat_mode`, authoritative over the config seed
    # (`2026-06-15-nest-nat-mode-client-set-design.md`). `FAUNA_MODE` is kept as
    # the **pre-claim seed** (so a home-relay box boots private BEFORE any client
    # connects — no IMAP-exposure / ACME window), but the binary reads it at boot
    # (`main.rs`, `Arc::get_mut` seed) rather than this entrypoint baking
    # `mode = "private"` permanently into the persistent config file. So the
    # former `sed 's/mode = "public"/mode = "private"/'` is intentionally gone:
    # the policy lives in nest DB state, not on disk.

    # ── [nest] overlay: CORS seed (NO domain) ──────────────────────
    # The box boots DOMAINLESS and learns its domain from the admin's claim handle
    # (`nest_identity_domain` is the primary `mail_domains` row — the only config
    # surface is the client; domains-and-tls-bootstrap.md § Env contract). So NO
    # `[nest].domain` / `[email]` is written from any env var. What IS written is
    # deployment wiring, not a user choice: the artifact's trusted browser origins,
    # a boot SEED the client-set `nest_cors_origins` row later wins over
    # (installers/docker.md § Environment Variables). First run only, for that
    # reason. `static_dir` used to be written here too; it is an artifact CONSTANT,
    # so it moved to the every-boot reconcile after this block — see there.
    # A plain key inside the existing [nest] table (not a dotted key, which would
    # land as nest.nest.*).
    CORS_TOML="[]"
    if [ -n "$CORS_ORIGINS" ]; then
        CORS_TOML=$(echo "$CORS_ORIGINS" | tr ',' '\n' | sed 's/^/"/;s/$/"/' | paste -sd, | sed 's/^/[/;s/$/]/')
    fi
    /usr/local/bin/nest-toml-overlay.sh seed-cors-origins /data/nest.toml "${CORS_TOML}"

    # config/default.toml already ships an [acme] table (dir + HTTP-01 mode); the
    # nest DERIVES whether to *order* (public NAT axis + a real orderable apex,
    # which it learns at claim — the challenge listener + lifecycle task now spawn
    # domain-less and issue once the apex lands; acme::build_acme_config + main.rs +
    # acme_http01 cert_lifecycle_loop). The nest ALWAYS writes an always-live
    # self-signed floor on boot (self_signed_cert::ensure_floor_present) and
    # self-heals it to cover the claimed domain, so TLS serves *immediately* and a
    # not-yet-orderable box never strands the listener. So the entrypoint only
    # OVERRIDES the shipped [acme] table for the one artifact-set value left — a
    # private ACME CA / the tier_4 pebble (`FAUNA_ACME_DIRECTORY_URL`, test IPC).
    # The CA (Let's Encrypt production) and the account contact (none) are
    # constants, not choices: docs/goal/architecture/nest/tls-certificates.md
    # § ACME settings — constants, not choices. Insert the line right after the
    # [acme] header so default.toml's dir/mode are preserved (no duplicate-table
    # rewrite). An empty value is skipped (an empty directory_url would point at a
    # bogus CA).
    if [ -n "$ACME_DIRECTORY_URL" ]; then
        sed -i '/^\[acme\]/a directory_url = "'"${ACME_DIRECTORY_URL}"'"' /data/nest.toml
    fi

    # ── Inject or let nest generate claim code ─────────────────────
    # Two non-interactive paths land the operator's claim code in the WRITABLE
    # volume so the uid-1000 nest can both read it (at claim) and delete it
    # (single-use); we run them in the first-run block ONLY, so a post-claim
    # restart can never resurrect a code the claim already consumed:
    #   1. FAUNA_CLAIM_CODE env — the env-driven deploys / tier_4 env path.
    #   2. A read-only seed staged OUTSIDE /data at /run/fauna/claim-code-seed —
    #      the cloud-init mount (cloud_init.rs). It is kept off /data so the
    #      `chown -R /data` below never hits a read-only bind, and copied here as
    #      root into the writable volume so the nest can read + post-claim-delete
    #      it. The mount-secrecy intent (the code never lands in `environment:`)
    #      is preserved; only the seed's *location* moved off /data.
    if [ -n "${FAUNA_CLAIM_CODE:-}" ]; then
        echo "$FAUNA_CLAIM_CODE" > /data/claim-code
        echo "Claim code injected from FAUNA_CLAIM_CODE env"
    elif [ -f /run/fauna/claim-code-seed ]; then
        cp /run/fauna/claim-code-seed /data/claim-code
        echo "Claim code staged from /run/fauna/claim-code-seed mount"
    fi
    # (If neither path provided one, the nest binary generates one on startup.)
    if [ -f /data/claim-code ]; then
        chown fauna:fauna /data/claim-code
        chmod 0600 /data/claim-code
    fi

    # (No services.json seeding here.) Nothing in the image reads the file: the
    # relay sidecar is always up and takes its cue from the nest over its own
    # channel, and mail enablement is the client toggle → supervisor socket,
    # never an env var. The nest creates the file on startup for its own flags.

    # Fix ownership before running user-level setup. Exclude the cloud-init
    # maintenance-host mount: it is a read-only, root-owned bind by design (the
    # nest only *reads* host status from it — see the sealed-store block below
    # and cloud_init.rs), so a blanket recursive chown would EROFS-crash the boot
    # under `set -e`. Pruning it leaves it correctly root-owned; everything else
    # under /data (the writable volume + the rw maintenance channel) is chowned.
    find /data -path /data/maintenance-host -prune -o -exec chown fauna:fauna {} +

    echo "=== Nest initialized ==="

else
    echo "=== Starting nest (existing config) ==="
fi

# ── [nest].static_dir reconcile — EVERY boot, not first-run ─────────
# Where this image put the bundled web SPA. A hard-coded artifact constant, not
# a human's choice (product-invariant bucket 1), so there is nothing on a box
# for a first-run-only write to preserve — and reconciling unconditionally is
# what lets a box first-booted with a BROKEN overlay self-heal on image pull.
# That is not hypothetical: from 2026-07-13 to 2026-07-24 this write was a
# silent no-op (its `sed` anchored on a `require_registration` line
# config/default.toml had stopped shipping), so every nest first-booted in that
# window has a nest.toml with no `static_dir`, no `/app` route, and the nest
# info page answering where the SPA should be. First-run-only would strand all
# of them permanently. The overlay verifies its own write and fails the boot
# loudly rather than proceeding half-configured.
/usr/local/bin/nest-toml-overlay.sh ensure-static-dir /data/nest.toml

# ── Inbound deliver key ─────────────────────────────────────────────
# Shared secret between fauna-nest (FAUNA_INBOUND_DELIVER_KEY) and
# fauna-bridge-daemon (FAUNA_BRIDGE_DELIVER_KEY). Without it,
# /api/v1/email/deliver returns 503 (inbox_routes.rs:521) and inbound
# mail never reaches the user's mailbox.
if [ ! -f /data/inbound-deliver-key ]; then
    head -c 32 /dev/urandom | od -A n -t x1 | tr -d ' \n\t' \
        > /data/inbound-deliver-key
    echo "Generated /data/inbound-deliver-key (32 random bytes)"
fi
chown fauna:fauna /data/inbound-deliver-key
chmod 0600 /data/inbound-deliver-key

# ── Mail-bridge keypair dirs (per-role UID isolation) ───────────────
# Co-resident process trust boundary (security.md § UID isolation): the MTA and
# MDA run under DISTINCT non-root UIDs (fauna-mta / fauna-mda) and each
# auto-generates its Ed25519 service-user keypair into its OWN 0700 subdir
# (mail-bridge-lifecycle.md § Cold boot). A per-role subdir (not a flat file in a
# shared dir) is required because the bridge rewrites the keyfile atomically via a
# temp-file + rename in the keyfile's directory (keyfile.go writeAtomic), so each
# bridge needs write access to its key's dir — and giving each its own 0700 dir is
# exactly what stops fauna-mda from reading fauna-mta's key and vice-versa.
# `/data/keys` itself is root:root 0711 (traverse-only): a bridge UID can reach
# its own subdir by name but cannot list the parent or enter the peer's 0700 dir.
mkdir -p /data/keys/mta /data/keys/mda
chown root:root /data/keys
chmod 0711 /data/keys
chown fauna-mta:fauna-mta /data/keys/mta
chmod 0700 /data/keys/mta
chown fauna-mda:fauna-mda /data/keys/mda
chmod 0700 /data/keys/mda
# The iroh P2P relay sidecar's OWN 0700 subdir (same boundary as the bridges): it
# generates + persists its X25519 keypair (relay.key, the seal recipient) here on
# first boot, running as the non-root fauna-relay UID. This is the relay's ONLY
# /data access — it never reads /data/acme (it fetches its TLS cert as a sealed
# blob it opens with this X25519). `/data/keys` stays root:root 0711 (traverse-only).
mkdir -p /data/keys/relay
chown fauna-relay:fauna-relay /data/keys/relay
chmod 0700 /data/keys/relay
# The ATProto PDS bridge's OWN 0700 subdir + non-root UID (same boundary as the
# mail bridges). Its keyfile is named for the role (atproto.pds.key). The ARTIFACT
# mints it AS ROOT here (mint-if-absent via --print-pubkey, so the enrolled
# identity is STABLE across reboots — a re-mint would orphan the enrolled row nest
# keys by pubkey), then re-owns it to fauna-atproto so the load-only bridge can
# read it. In S1 the bridge enrolls LENIENT (loopback-gated only — nest provisions
# NO blessed key for atproto.pds yet, blessed_bridge_pubkey→AtprotoPds is None),
# so NO /data/keys/blessed/atproto.pub is published; S2 adds strict-mode blessing
# when nest reads it. docs/goal/behavior/atproto-pds-bridge.md § Bridge lifecycle.
mkdir -p /data/keys/atproto
chown fauna-atproto:fauna-atproto /data/keys/atproto
chmod 0700 /data/keys/atproto
_atproto_keyfile="/data/keys/atproto/atproto.pds.key"
if ! fauna-atproto-bridge --keypair-file "$_atproto_keyfile" --print-pubkey >/dev/null; then
    echo "FATAL: failed to mint/read atproto.pds keypair ($_atproto_keyfile)" >&2
    exit 1
fi
# The private keyfile is born root-owned 0600 (CreateTemp) — re-own it to the
# bridge UID so the load-only bridge process can read it (its 0700 subdir already
# isolates it from the peer bridge UIDs).
chown fauna-atproto:fauna-atproto "$_atproto_keyfile"
chmod 0600 "$_atproto_keyfile"
echo "Provisioned atproto.pds keypair ($_atproto_keyfile)"
# The ATProto PDS bridge's OWN writable STATE dir (distinct from its 0700 key
# subdir above). `/data` itself is fauna:fauna 0711, so fauna-atproto is "other"
# with traverse-only rights and CANNOT create a file directly under /data — the
# repo store's SQLite open then fails SQLITE_CANTOPEN(14) and the bridge
# crash-loops before it ever serves. Its store is WAL (atprotorepo/store.go), so
# it needs to create `-wal`/`-shm` siblings too, i.e. real *directory* write —
# pre-creating the .db file alone would not be enough. Owned by the bridge UID at
# 0700, the same boundary as every other bridge's private state. Everything here
# is re-derivable from nest state (C6) — a cache, not precious.
mkdir -p /data/atproto
chown fauna-atproto:fauna-atproto /data/atproto
chmod 0700 /data/atproto
# Re-own an existing mail-bridge keyfile to its bridge UID on every boot, so the
# load-only bridge can read it whatever wrote it last.
for _role in mta mda; do
    if [ -f "/data/keys/${_role}/${_role}.key" ]; then
        chown "fauna-${_role}:fauna-${_role}" "/data/keys/${_role}/${_role}.key"
        chmod 0600 "/data/keys/${_role}/${_role}.key"
    fi
done

# ── Mint each bridge's keypair + publish its blessed pubkey ──
# security.md § Enrollment proof-of-possession contract: the ARTIFACT mints each
# role's keypair here (mint-if-absent, AS ROOT) so the bridge becomes LOAD-ONLY,
# and writes the role's blessed Ed25519 pubkey to a root-owned, bridge-UID-
# UNWRITABLE registry that nest reads. nest then requires an enrolling bridge to
# BE this blessed key AND prove possession of it (sign the enrollment), so a
# co-resident attacker that cannot read the UID-isolated private keyfile cannot
# self-enroll a rogue/cross-role identity over loopback. `--print-pubkey` is
# idempotent: an existing keyfile is loaded, not regenerated, so the enrolled
# identity (and the published pubkey) is STABLE across reboots — a re-mint would
# orphan the enrolled row, which nest keys by pubkey. Only the PUBLIC half goes to
# the registry: integrity, not confidentiality, is the property — a bridge UID
# must not be able to SUBSTITUTE a pubkey, which a root-owned file + the registry
# dir's lack of write bit for non-root ensure (the pubkey itself is public). Done
# for BOTH roles unconditionally (cheap; harmless when a role never runs) so the
# registry is present whichever axis the admin later enables.
mkdir -p /data/keys/blessed
chown root:root /data/keys/blessed
chmod 0755 /data/keys/blessed
for _role in mta mda; do
    _keyfile="/data/keys/${_role}/${_role}.key"
    _blessed="/data/keys/blessed/${_role}.pub"
    if ! _pub="$(fauna-mail-bridge --keypair-file "$_keyfile" --print-pubkey)"; then
        echo "FATAL: failed to mint/read blessed pubkey for ${_role} (${_keyfile})" >&2
        exit 1
    fi
    printf '%s\n' "$_pub" > "$_blessed"
    # The private keyfile is born root-owned 0600 (CreateTemp) — re-own it to the
    # bridge UID so the load-only bridge process can read it (its 0700 subdir
    # already isolates it from the peer bridge UID).
    chown "fauna-${_role}:fauna-${_role}" "$_keyfile"
    chmod 0600 "$_keyfile"
    # The registry file is root-owned + non-writable by any bridge UID.
    chown root:root "$_blessed"
    chmod 0644 "$_blessed"
    echo "Provisioned blessed pubkey for ${_role}: ${_pub}"
done

# ── SNI-router auth secret ───
# security.md § Co-resident process trust boundary: a PROXY-v2 header is trusted
# (to convey the real client IP) only from the fauna-sni-router, NOT from a
# co-resident bridge UID that could forge one to spoof a source IP (evading
# per-source rate limits / poisoning the MDA AUTH-lockout key). SO_PEERCRED can't
# distinguish them — the router→backend hops are TCP loopback, where it is
# unavailable — so the router proves itself with a shared secret it appends as a
# PROXY-v2 TLV that every backend it fronts verifies. Each backend peels the
# header at its OWN peel point, so each must check: nest, the MDA's DAV-443
# listener, and the PDS bridge's XRPC listener. The secret lives in a root-only
# 0600 file that ALL FOUR of those run-scripts (nest, router, MDA, PDS bridge)
# read AS ROOT (before their s6-setuidgid drop) and
# pass to their process via the FAUNA_ROUTER_PROXY_SECRET env (env, never argv:
# /proc/<pid>/environ is readable only by the process's own UID, while
# /proc/<pid>/cmdline is world-readable). A bridge UID can read neither the
# root-owned file nor the router's/nest's environ, so it cannot learn the secret.
# Random per-deployment, generated once, persisted across redeploys.
mkdir -p /data/keys/router
chown root:root /data/keys/router
chmod 0700 /data/keys/router
if [ ! -s /data/keys/router/proxy-secret ]; then
    head -c 32 /dev/urandom | od -A n -t x1 | tr -d ' \n\t' \
        > /data/keys/router/proxy-secret
    echo "Generated /data/keys/router/proxy-secret (32 random bytes)"
fi
chown root:root /data/keys/router/proxy-secret
chmod 0600 /data/keys/router/proxy-secret

# ── /data traversal for the now-non-owner bridge UIDs ───────────────
# Since the split, the MTA/MDA are no longer the owner of /data (nest is), so
# they need the directory-traverse (x) bit to reach /data/keys/<role>/ and
# /data/operator-hatch.toml. Debian `useradd -m` may create the home at 0700,
# which would deny that traversal — pin it to 0711 (owner nest rwx; others
# traverse-only, cannot list). The sensitive files under /data stay individually
# 0600/0700 nest-owned, so listing being denied is belt-and-braces, not the
# load-bearing control.
chmod 0711 /data

# ── Sealed-store filesystem isolation (the load-bearing control) ────
# nest's sealed store MUST be unreadable by a co-resident bridge UID: nest.db
# carries plaintext user data in the trust-the-box plaintext storage mode, and
# /data/acme holds TLS private keys. nest's run-script `umask 077` makes
# first-boot-created files private, but the blobs/acme DIRS are created here (by
# root, umask 022 → 0755) and nothing else tightens them —
# so pin the modes explicitly, every boot.
chmod 0700 /data/blobs /data/acme 2>/dev/null || true
for _f in /data/nest.db /data/nest.db-wal /data/nest.db-shm /data/claim-code; do
    # `2>/dev/null || true` (matching the blobs/acme line above) so a stray
    # read-only bind on any of these can never EROFS-crash the boot under
    # `set -e` — defense beyond the claim-code seed now staging off /data.
    #
    # chown too, not just chmod: the first-run block (the ONLY other place that
    # chowns the staged claim-code to fauna) is SKIPPED on every boot after
    # nest.toml exists, yet a claim-code can be present ROOT-owned on such a boot —
    # staged by a redeploy onto an existing /data volume or a
    # direct `:ro` bind of the root-owned seed. Left root-owned, the uid-1000
    # nest cannot READ its own claim code: the claim handler's read fails and is
    # (mis)reported to the client as `already_claimed` on a genuinely UNCLAIMED box
    # (also hardened nest-side in claim_core.rs to surface the true cause). Re-owning
    # to fauna every boot makes the nest's private state nest-readable however it got
    # there, so the box self-heals on restart (works-out-of-the-box invariant).
    [ -e "$_f" ] && { chown fauna:fauna "$_f" 2>/dev/null || true; chmod 0600 "$_f" 2>/dev/null || true; }
done

# ── Mail-bridge operator-hatch (CalDAV remap / clamd / rspamd / outbound relay) ──
# The bridge reads <data-dir>/operator-hatch.toml for deployment-topology nest
# cannot provide. The values this image writes:
#   - caldav_listen_https / caldav_bind_host: the MDA's CalDAV bind, by topology
#     (full split in the "MDA IMAP/CalDAV listener binds" block below). A PUBLIC /
#     domain box writes caldav_listen_https = "127.0.0.1:8444" so the
#     fauna-sni-router (which owns the container's public :443) routes SNI
#     `mail.<domain>` to the MDA over loopback — nest and MDA never share
#     plaintext. A HOME-RELAY / bare-IP box (FAUNA_LAN_BIND_IP set) instead writes
#     caldav_bind_host = "<LAN-IP>" so CalDAV binds <LAN-IP>:<admin-port>
#     directly (router-bypassing, reachable by bare IP — the home2 fix); the port
#     stays the admin-set nest-state value. Topology-only (no user-facing knob).
#     See docs/goal/behavior/caldav-server.md § Network exposure.
#   - FAUNA_CLAMD_ADDR / FAUNA_RSPAMD_URL: the co-located content-scan daemons.
#     The scan gate is default-on and FAIL-CLOSED: an unreachable clamd or
#     rspamd 451s every inbound message (internal/mta/scan_gate.go), so
#     docker-compose.yml runs clamd + rspamd sidecars and points these here.
#   - FAUNA_MTA_MX_OVERRIDE: a static outbound transport route for split-horizon
#     / air-gapped relay topologies (config.go § mta_mx_override) — a comma-
#     separated list of `domain=host[:port]` mappings the MTA outbound worker
#     resolves instead of querying the domain's public MX. Unset = real MX
#     resolution (the normal public-internet deploy).
# Regenerated from env on every start (deployment topology, not a hand-edited
# file). Always written now (the CalDAV remap is unconditional for this image),
# whether or not mail is enabled — harmless when the MDA is down (`just
# docker-run` single-container local dev): the loopback addr just goes unbound.
: > /data/operator-hatch.toml
# Top-level scalar keys must precede the [mta_mx_override] table header (a TOML
# table header ends the top-level section).
#
# ── MDA IMAP/CalDAV listener binds ──────────────────────────────────
# The MDA's CalDAV + IMAP listeners bind by deployment topology, driven by
# FAUNA_LAN_BIND_IP (the home box's host LAN IP; a bind address is artifact-set
# deployment topology, never nest state — product invariant). Two shapes:
#
#   • PUBLIC / domain box (FAUNA_LAN_BIND_IP unset). CalDAV is router-fronted:
#     the always-up fauna-sni-router owns :443 and routes SNI mail.<domain> to
#     the MDA's CalDAV over loopback 8444 (caldav_listen_https=127.0.0.1:8444),
#     so nest and the MDA each terminate their own TLS and no process sees the
#     union of plaintext. IMAP keeps the public-axis all-interfaces default.
#
#   • HOME-RELAY / LAN / bare-IP box (FAUNA_LAN_BIND_IP set). A box reached by a
#     bare IP has no usable mail.<domain> SNI (the user types the IP), so the
#     router's CalDAV route never fires and a loopback CalDAV bind is unreachable
#     — the home2 bug. Instead the MDA binds CalDAV DIRECTLY on the LAN IP at the
#     admin-set caldav_port: caldav_bind_host carries only the interface, the
#     port stays nest state, so the admin can still change it from any client
#     (caldav-server.md § Network exposure). This is the exact CalDAV analogue of
#     the IMAP LAN bind below — router-bypassing, reachable at
#     https://<LAN-IP>:<caldav-port>/. Under host networking (the
#     docker-compose.home.yml bundle) the LAN IP is the host's real interface, so
#     no port-publish is needed; the SNI router's now-dead mail.<domain> CalDAV
#     route is harmless (a mis-route is only a failed handshake, never a leak).
#
# An explicit operator-hatch bind always wins over the NAT-axis loopback default
# (the sanctioned OS-deployment-topology opt-out); on the private axis a specific
# LAN IP (not all-interfaces) avoids the plaintext-exposure warning.
LAN_BIND_IP="${FAUNA_LAN_BIND_IP:-}"
if [ -n "$LAN_BIND_IP" ]; then
    {
        echo "caldav_bind_host = \"${LAN_BIND_IP}\""
        echo "imap_listen_implicit_tls = \"${LAN_BIND_IP}:993\""
        echo "imap_listen_starttls = \"${LAN_BIND_IP}:143\""
    } >> /data/operator-hatch.toml
else
    echo 'caldav_listen_https = "127.0.0.1:8444"' >> /data/operator-hatch.toml
fi
if [ -n "${FAUNA_CLAMD_ADDR:-}" ]; then
    echo "clamd_addr = \"${FAUNA_CLAMD_ADDR}\"" >> /data/operator-hatch.toml
fi
if [ -n "${FAUNA_RSPAMD_URL:-}" ]; then
    echo "rspamd_url = \"${FAUNA_RSPAMD_URL}\"" >> /data/operator-hatch.toml
fi
if [ -n "${FAUNA_MTA_MX_OVERRIDE:-}" ]; then
    echo "[mta_mx_override]" >> /data/operator-hatch.toml
    # Split the comma list into `domain=target` pairs; IFS='=' splits on the
    # first `=` only, so a `host:port` target (which contains no `=`) is safe.
    echo "${FAUNA_MTA_MX_OVERRIDE}" | tr ',' '\n' | while IFS='=' read -r _dom _target; do
        [ -n "$_dom" ] || continue
        echo "\"${_dom}\" = \"${_target}\"" >> /data/operator-hatch.toml
    done
fi
# Readable by BOTH bridge UIDs (fauna-mta needs clamd/rspamd/mx-override;
# fauna-mda needs the CalDAV/IMAP binds) — it is deployment TOPOLOGY, not a
# secret (bind addresses, scanner endpoints, MX overrides), so 0644 is correct.
chown fauna:fauna /data/operator-hatch.toml
chmod 0644 /data/operator-hatch.toml
if [ -n "$LAN_BIND_IP" ]; then
    _mda_binds="caldav_bind_host=${LAN_BIND_IP} (direct, :admin-port) imap=${LAN_BIND_IP}:993/143 (LAN)"
else
    _mda_binds="caldav_listen_https=127.0.0.1:8444 (sni-router-fronted) imap=<private-axis-default>"
fi
echo "Wrote /data/operator-hatch.toml (${_mda_binds} clamd_addr=${FAUNA_CLAMD_ADDR:-<unset>} rspamd_url=${FAUNA_RSPAMD_URL:-<unset>} mta_mx_override=${FAUNA_MTA_MX_OVERRIDE:-<unset>})"

# ── Hand off to s6-overlay ──────────────────────────────────────────
exec /init
