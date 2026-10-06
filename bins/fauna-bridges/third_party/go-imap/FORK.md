# FAUNA-FORK of emersion/go-imap

This is a **vendored, minimal, additive fork** of
[`github.com/emersion/go-imap/v2`](https://github.com/emersion/go-imap),
wired into the mail-bridge build via a local `replace` directive in
`bins/fauna-bridges/go.mod`.

- **Forked from:** `v2.0.0-beta.8` (the upstream tag the module previously
  depended on; see `git log` for the bump).
- **Why a fork:** decision tracked internally.
  In short: the nest-side data layers + MDA router for the Dovecot-parity
  IMAP extension cluster (CONDSTORE/QRESYNC/QUOTA) are already built and
  idle; only the upstream **server-framework wire-emission seams** are
  missing, and upstream ships betas slowly. Pre-1.0 + plain-`replace` +
  additive-only patches make a temporary fork the low-regret play.
- **Discipline:** every patch is **append-only** and marked with a
  `// FAUNA-FORK:` comment so a future beta bump rebases cleanly and the
  diff is self-documenting. We open an upstream PR (or ride an existing
  one) for every patch; this fork is a bridge, not a destination, and
  shrinks toward zero as PRs land.

## Patch ledger

Each row: the seam added, the file(s), and the upstream PR tracking it.
Mark a patch ✅ landed-upstream when it can be dropped on the next bump.

| # | Seam | File(s) | Upstream PR | Status |
|---|------|---------|-------------|--------|
| 1 | `FetchResponseWriter.WriteModSeq` (emit `MODSEQ (<n>)`) | `imapserver/fetch.go` | #690 (MODSEQ) | forked |
| 2 | FETCH `MODSEQ` data item → `FetchOptions.ModSeq` | `imapserver/fetch.go` | #690 | forked |
| 3 | FETCH `(CHANGEDSINCE <n>)` modifier → `FetchOptions.ChangedSince` | `imapserver/fetch.go` | #690 | forked |
| 4 | STORE `(UNCHANGEDSINCE <n>)` modifier → `StoreOptions.UnchangedSince` | `imapserver/store.go` | #690 | forked |
| 5 | SELECT/EXAMINE emit `OK [HIGHESTMODSEQ <n>]` when CONDSTORE enabled | `imapserver/select.go` | #690 | forked |
| 6 | ENABLE routes `CONDSTORE` into `Conn.enabled` (echoed in `* ENABLED`) | `imapserver/enable.go` | #690 | forked |
| 7 | Advertise `CONDSTORE` post-auth | `imapserver/capability.go` | #690 | forked |
| 8 | `UpdateWriter.WriteMessageFlagsModSeq` (unsolicited FETCH FLAGS MODSEQ) | `imapserver/conn.go` | #690 | forked |
| 9 | ENABLE routes `QRESYNC` into `Conn.enabled` (implies CONDSTORE) | `imapserver/enable.go` | QRESYNC (no PR yet) | forked |
| 10 | Advertise `QRESYNC` post-auth | `imapserver/capability.go` | QRESYNC (no PR yet) | forked |
| 11 | `SelectOptions.QResync` + `SelectQResync` type | `select.go` | QRESYNC (no PR yet) | forked |
| 12 | SELECT `(QRESYNC (uidvalidity modseq …))` + `(CONDSTORE)` select-param parser | `imapserver/select.go` | QRESYNC (no PR yet) | forked |
| 13 | `FetchOptions.Vanished` | `fetch.go` | QRESYNC (no PR yet) | forked |
| 14 | FETCH `(CHANGEDSINCE n VANISHED)` modifier → `FetchOptions.Vanished` | `imapserver/fetch.go` | QRESYNC (no PR yet) | forked |
| 15 | `Conn.writeVanished` + `ExpungeWriter.WriteVanished` + `UpdateWriter.WriteVanished` + `FetchWriter.WriteVanishedEarlier` (`* VANISHED` / `* VANISHED (EARLIER)`) | `imapserver/{expunge,conn,fetch}.go` | QRESYNC (no PR yet) | forked |
| 16 | `SelectData.QResync` (+ `SelectQResyncData`/`SelectQResyncChange`) → inline SELECT (QRESYNC) `* VANISHED (EARLIER)` + changed-message `FETCH` in `handleSelect` | `select.go`, `imapserver/select.go` | QRESYNC (no PR yet) | forked |
| 17 | QUOTA (RFC 9208): `SessionQuota` interface + `GETQUOTA`/`GETQUOTAROOT` parsers/dispatch + `* QUOTA`/`* QUOTAROOT` writers (`QuotaData`/`QuotaResourceData`) + advertise `QUOTA` + `QUOTA=RES-STORAGE`/`QUOTA=RES-MESSAGE` | `imapserver/quota.go`, `imapserver/session.go`, `imapserver/conn.go`, `imapserver/capability.go` | QUOTA (no PR yet) | forked |
| 18 | Graceful `Server.Shutdown(ctx) (forced, pending)`: `* BYE` on new commands + prompt wake of idle/IDLE'ing conns + drain in-flight up to ctx, then force-close (RFC 9051 §7.1.5). `Conn.inFlight` flag + `Server.draining` + `errDraining` + `forceCloseConns`/`waitConnsDrained` helpers | `imapserver/server.go`, `imapserver/conn.go`, `imapserver/idle.go` | graceful-shutdown (no PR yet) | forked |
| 19 | Per-server IDLE-timeout teardown (RFC 2177 §3): `handleIdle` sets the IDLE read deadline from the session's optional `IdleTimeout() time.Duration` (falling back to the 35-min framework default) and ends an inactive IDLE with `* BYE` + close on expiry, signalled via `errIdleServerTimeout` (handed straight back like `errDraining`, no tagged response). Makes the `imap.idle_timeout_seconds` policy knob wire-observable + hot-reloadable per connection | `imapserver/idle.go`, `imapserver/conn.go` | idle-timeout (no PR yet) | forked |

The QRESYNC seams (rows 9–16) land the full **server-emit** half of RFC
7162 §3.2: the bridge parses `(QRESYNC …)` SELECT params and forwards
them to the nest restore-divergence seam (γ), emits `* VANISHED` for the
EXPUNGE command, the IDLE expunge push, and the `(CHANGEDSINCE n
VANISHED)` UID FETCH, and — with row 16 — emits the *inline* SELECT
(QRESYNC) fast-path: `* VANISHED (EARLIER)` + the changed-message FETCH
*within the SELECT response itself* (RFC 7162 §3.2.5 common case), driven
by the `SelectData.QResync` output extension and a changed-message stream
in `handleSelect`. The follow-up `UID FETCH … (CHANGEDSINCE n VANISHED)`
path remains the fallback a client uses when SELECT did not
carry `(QRESYNC …)`. No upstream PR exists for the QRESYNC server seams
yet (unlike #690 for MODSEQ); opening one is tracked as fork follow-up
work.

The QUOTA seam lands the server half of RFC 9208: the
`GETQUOTA` / `GETQUOTAROOT` command parsers, a `SessionQuota` dispatch
interface (`GetQuota` / `GetQuotaRoot`) the MDA implements, and the
`* QUOTA <root> (STORAGE used limit MESSAGE used limit)` /
`* QUOTAROOT <mailbox> <root>` response writers. `QUOTA` is advertised
post-auth alongside its `QUOTA=RES-STORAGE` / `QUOTA=RES-MESSAGE`
resource-type capabilities (RFC 9208 §6). Read-only — `SETQUOTA` is not
implemented (Fauna quota ceilings are nest configuration). No upstream PR
exists for the QUOTA server seam yet; opening one is tracked as fork
follow-up work.

The graceful-shutdown seam adds the server-side half of RFC 9051
§7.1.5 (`* BYE` may be sent at any time). Upstream beta.8 ships only a
force-only `Close`; `Shutdown(ctx)` lets the MDA role drain on SIGTERM up to
`mail.bridge.shutdown_grace_seconds` (the MTA's go-smtp half is already done —
see `internal/mta/drain.go`). The design mirrors `net/http.Server.Shutdown`'s
idle-vs-active split with one IMAP-specific twist: a connection parked in IDLE
(or between commands) is woken by a read-deadline poke and BYE'd *immediately*
rather than waited on for the full grace, because long-lived IDLE is the IMAP
norm and waiting it out every shutdown would be wrong. A connection mid-command
(draining an APPEND literal, building a FETCH response) is left to finish up to
the deadline — the `Conn.inFlight` flag is the discriminator. `errDraining`
keeps a woken IDLE read from emitting a spurious tagged response before the
BYE. No upstream PR exists yet; opening one is tracked as fork follow-up work.

The IDLE-timeout seam makes the per-server idle timeout actually end
an inactive IDLE on the wire. Upstream's `handleIdle` blocks on the client's
`DONE` and only consumes `Session.Idle`'s return *after* that read completes —
so a `Session.Idle` that returns early on its own timer disconnects nobody. The
fork sets the IDLE read deadline to the session's `IdleTimeout()` (an optional
method discovered by type assertion, so upstream `Session`s are unaffected) and,
on a deadline-exceeded read while not shutting down, sends `* BYE` + closes and
returns `errIdleServerTimeout` — which `readCommand` hands straight back and the
serve loop breaks on quietly, exactly like `errDraining`. The deadline value is
read per-IDLE, so a hot-reloaded `imap.idle_timeout_seconds` (mail-policy-config)
binds on the next connection without a bridge restart. No upstream PR yet.

## Upstream security watch

A vendored `replace`-directive fork is **invisible to automated dependency
scanning**: `govulncheck` and Dependabot only see versioned module deps in
`go.mod`, not the in-tree copy under `third_party/go-imap`. So an upstream
security fix to `emersion/go-imap` will **not** surface on its own — it must be
watched for by hand.

**Cadence — ride the existing weekly `supply-chain.yml` schedule** (Mon 06:00
UTC; `.github/workflows/supply-chain.yml`). On each weekly review, also check:

1. `emersion/go-imap` **GitHub releases / tags** for anything newer than the
   forked-from `v2.0.0-beta.8` (top of this file).
2. The repo's **GitHub Security Advisories** (and any `imapserver`-touching
   commits) since that tag — the server framework is the half we vendor.

**Action when a security-relevant fix landed upstream:** rebase it in per
*§ Rebasing onto a newer upstream beta* below (re-apply the `// FAUNA-FORK:`
blocks, drop any ledger row whose PR the bump now carries). A fix touching only
`imapclient/` or `cmd/` (we retain the former for tests, trimmed the latter) is
lower urgency but still worth the bump. Record the review in the bump's commit
message so the watch is auditable across sessions.

## Rebasing onto a newer upstream beta

1. Diff `third_party/go-imap` against the new tag's tree to find conflicts.
2. Re-apply each `// FAUNA-FORK:` block (they are isolated and additive).
3. Drop any row whose upstream PR has landed (delete the patch, keep the
   library's version).
4. Run `TMPDIR=/work/tmp just mail-bridge-test`.

## Trimmed from upstream

`cmd/` (upstream CLI helpers) was removed — not imported by the bridge and
not needed to build the fork. `imapclient/` is retained for test ergonomics.
