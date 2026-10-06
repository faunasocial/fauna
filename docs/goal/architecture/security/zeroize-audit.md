# Submission-auth credential lifetime — zeroize audit — target state

Owns: zeroize
Status: partially-specified — resolved by completing the tail of the Go-side credential-lifetime re-audit recorded in `## Implementation status today` (the named surfaces are re-audited as of 2026-07-07; the residuals and the unverified corners are listed there)
Authority: owns the credential-zeroize property and the audit site list (the Rust shared-crate keypair sites + the Go `fauna-mail-bridge`); `smtp-server.md` defers here for submission-auth credential lifetime.

> **Audience:** the nest area, plus rust/go work touching credential-handling code in `bins/fauna-bridges/` or shared `libs/fauna-core/{crypto,identity}.rs`.
> **Purpose:** every owned plaintext credential buffer in the bridge process (and equivalent sites in shared crates) is zeroed on scope exit, so a heap-dump or core file taken right after an AUTH call doesn't yield credentials.

## Goal

Every owned plaintext buffer carrying a submission token or actor secret-key is zeroed when its scope exits, so a heap-dump or core file taken right after a Login / AddUser call doesn't yield credentials.

**Scope:** every site in the mail bridge where a plaintext submission token or actor secret-key reaches owned memory, plus the equivalent owned-plaintext sites in shared `libs/fauna-core/{crypto,identity}.rs`. With the Rust `fauna-bridge-daemon` deleted at the I6 cutover, the bridge-side owned plaintext is now exclusively in the Go `fauna-mail-bridge` (see § Implementation status today); the Rust shared-crate actor-keypair sites are unchanged.

This is the inbound side of the same concern that `libs/fauna-core/{crypto,identity}.rs` already handles for actor-secret keys (`Zeroizing<[u8; 32]>` on `ActorKeypair`).

## Threat model

- **In scope:** an attacker who reads daemon process memory (heap dump, core file, `/proc/<pid>/mem`, swap-file, hibernate image). The defense is overwriting plaintext buffers as soon as we no longer need them, so the window where a memory snapshot leaks the credential is bounded.
- **Out of scope:** an attacker who runs code in the daemon's address space at the moment the credential is being verified — that attacker can read any memory anyway. Zeroize doesn't help. Mitigations are SystemCallFilter / ASLR / W^X (already covered by the systemd unit hardening from plan-session 13b).
- **Out of scope:** the kernel socket buffer that delivered the AUTH payload. The bridge doesn't own that memory and has no API to wipe it. The transport-level mitigation is WS-RPC over TLS plus the bridge's dedicated service-user keypair (`docs/goal/architecture/transport.md`).
- **Out of scope:** the JSON deserializer's intermediate buffers (`serde_json::Value`). They live for microseconds during deserialize and are then dropped; the freed pages may still contain plaintext until reused by other allocations. Mitigation would require a custom allocator or `MemfdSecret` — disproportionate for the threat model.

## Architectural rules

- **Every owned plaintext credential buffer carries a `Drop` impl that zeroes its plaintext fields** (or wraps the field in `zeroize::Zeroizing<...>`). Borrows do not need separate zeroization — their owner already covers them.
- **The Drop-impl pattern is the uniform credential-handling shape** across the bridge daemon and any future RPC param type carrying plaintext credentials. Per Engineering Priority #1, new credential-bearing types reuse this pattern rather than inventing a new one.
- **argon2's `hash_password_into` takes `&[u8]` borrows for both password and salt** (`libs/fauna-core/src/kdf.rs::derive_key_argon2id` — the only Argon2 call site in the chain; `libs/fauna-mls/src/wrapped_blob/kdf.rs` re-exports it), so wrapping the owner in a zeroize wrapper is sufficient — argon2 never owns the plaintext.

## Forward-direction note (realized at the I6 cutover)

The 2026-05-07 mail-bridge rearchitecture design (ratified 2026-05-07; tracked internally) has now landed: the Rust `fauna-bridge-daemon` is deleted, and the per-user keypair-authenticated Go `fauna-mail-bridge` + wrapped-blob crypto + MUA-AUTH credential unwrapping is the live shape. The `AddUserParams` / `LoginParams` / `db.rs::authenticate` Rust Drop-impl sites this audit covered no longer exist. The owned-plaintext credential concern moved to the Go bridge; the fresh Go-side audit ran 2026-07-07 (§ Implementation status today → Go-bridge credential/capability discipline). The shared `libs/fauna-core` actor-keypair zeroize sites stand.

## Implementation status today

### Go-bridge credential/capability discipline (re-audited 2026-07-07)

The "fresh Go-side audit" the I6 cutover left open has been run against the live tree; the discipline **exists and is the uniform shape**:

- **Session-scoped decryption capability (MDA).** The IMAP AUTH path zeroizes the unwrapped MLS capability on **every** exit path (`bins/fauna-bridges/internal/mda/imap/auth.go:193-330`, `finishAuth`'s `cap.Zeroize()` calls) and on session close (`internal/mda/imap/session.go:231`); the DAV surfaces follow the same shape (`internal/mda/davauth/{auth,session}.go`). The FFI contract is explicit: the bridge MUST call `MLSCapability.Zeroize()` on session close, and the generated finalizer runs `Zeroize` on GC as a backstop (`internal/mailfauna/mailfauna.go`).
- **Session-scoped record opener (MDA) — a second secret-holding type, on a simpler but equally sound discipline.** `recordOpener` (`*mailfauna.MailRecordOpener`), the per-connection mail-record opener, holds the AUTH'd actor's decrypted MLS-snapshot leaf X25519 HPKE secrets Rust-side (landed 2026-07-08, the Phase-3 S1-S3 fold — one day after this section's audit date, so it was never folded into the enumeration above). The 2026-07-18 MSEK-holder epoch-opener chain (content-sealing-epochs design § 4) additionally has the IMAP variant derive its epoch-root chain from a `Zeroizing`-wrapped copy of the capability's MSEK, dropped at construction. Unlike `cap`, `recordOpener` needs no every-exit-path handling of its own: it stays `nil` through every AUTH-failure branch that precedes its construction (`cap.Zeroize()` alone covers those — `imap/auth.go:229,242,262,270,303`; `davauth/auth.go:311`), and the one function that constructs it always returns success once construction succeeds, attaching it to the session (`s.recordOpener = recordOpener`/`sess.recordOpener = opener`) with no further auth-time exit path. Its only zeroize path is therefore session close (`imap/session.go:240`; `davauth/session.go:120`) — a narrower discipline than `cap`'s, sound because it's structurally impossible for `recordOpener` to be live at an early-exit point.
- **MTA submission session.** The post-AUTH session state deliberately holds **no** secret material (`internal/mta/submission.go` — it stashes `(actor_id, credential_id, *SubmissionTokenFfi)`, an FFI handle, never plaintext), so there is nothing to zeroize on session close by construction; the unwrap itself runs FFI-side in Rust `Zeroizing` memory.
- **DKIM signing key.** The Go bridge holds none — § Nest DKIM signing key below.

**Verified residuals (the honest remainder):**

- **Transient Go credential strings (un-closeable at the language level).** The AUTH credential itself arrives as an immutable Go `string` (`auth.ParsePlainPayload` / `ParseOAuthBearerPayload`, both the MDA `internal/mda/imap/auth.go` and the MTA `internal/mta/auth.go`), which Go cannot wipe. The window is bounded to the AUTH call plus GC lag — the same class as the deserializer-buffer item under § Threat model → out of scope. A `[]byte`-based parse path would shrink it further (deferred; low value against this threat model). The 2026-07-16 dummy-KDF timing fix (`internal/mailfauna/dummy_kdf.go::DummyCredentialKDF`; network-exposure.md § Rulings F3) is a new call site feeding this same already-transient credential into a real Argon2id run on the unknown-user path — it doesn't retain the credential past the call (and zeroizes the near-impossible-success dummy capability), so it's an additional site for this existing residual, not a new one.
- **Argon2 scratch buffers — still open, relocated.** The live Argon2id KDF for PLAIN credentials runs **Rust-side** in `libs/fauna-core/src/kdf.rs` (workspace `argon2 = "0.5"`), reached from the bridge via FFI. `argon2` 0.5 (0.5.3 pinned) documents `Block: Zeroize` behind its `zeroize` feature but says nothing about whether the blocks `hash_password_into` allocates internally are wiped — an observation about upstream's public API docs, not a defect finding; tracked here, not closed. No upstream issue is filed on it (checked 2026-08-24).
- **Unverified corners (need a look before this doc's Status can go `ratified`):** the CalDAV/CardDAV PUT/REPORT handlers' handling of any capability-derived intermediate buffers beyond the shared `davauth` session (spot-checked only via the session layer), and log paths — no Go call site is known to format a credential into a log line, but no lint enforces it (the Go analogue of the old Rust redacting-`Debug` gap below).

### Owned plaintext sites (Rust daemon — deleted at I6 cutover)

The Rust sites below were the audited targets; the `fauna-bridge-daemon` crate that held them was deleted at the I6 cutover (2026-05-24). They are retained here as the historical audit record — the live equivalents in the Go `fauna-mail-bridge` are covered by the 2026-07-07 re-audit section above.

| File:line (deleted) | Owner | Field | Mitigation (historical) |
|---|---|---|---|
| `bins/fauna-bridge-daemon/src/ipc.rs` `AddUserParams` | the `serde_json::from_value` deserialize at `handler.rs:143` | `token: String`, `secret_key: Option<String>` | `Drop` impl zeroed both on scope exit |
| `bins/fauna-bridge-daemon/src/ipc.rs` `LoginParams` | the `serde_json::from_value` deserialize at `handler.rs:160` | `token: String` | `Drop` impl zeroed on scope exit |
| `bins/fauna-bridge-daemon/src/db.rs::authenticate` | `plain_token_raw: String` returned from sqlite for pre-migration rows | the local binding | Wrapped in `zeroize::Zeroizing<String>` at the assignment site — buffer overwritten on every exit path including `?` and panic |

### Gaps recorded by the original (2026-05-08) audit — disposition as of the 2026-07-07 re-audit

- **Pre-Drop log paths.** ~~`AddUserParams` and `LoginParams` derive `Debug`…~~ Obsolete — the structs were deleted with the daemon. The live analogue (no lint stops a Go call site logging a credential string) is carried under § Go-bridge credential/capability discipline → *Unverified corners*.
- **Go bridge token handling.** ~~Not yet closed…~~ **Closed** for everything that outlives the AUTH call: session-scoped secrets ride FFI capability handles with explicit `Zeroize` on every exit path + session close + a GC-finalizer backstop, and the MTA submission session holds no secret post-AUTH (see § Go-bridge credential/capability discipline above). What remains is the transient immutable Go credential `string` — un-closeable at the language level, recorded as a residual above.
- **Password during HTTP / IMAP / SMTP-AUTH.** Same disposition as the previous bullet (the residual is the transient `string`, not the session).
- **Argon2 internal scratch buffers.** Still open — relocated from the deleted Rust daemon to the live Rust-side KDF in `libs/fauna-core/src/kdf.rs` (see the residuals list above).

### Nest DKIM signing key

The DKIM signing key is the nest's own (`../../behavior/mail-bridge-lifecycle.md` § DKIM provisioning (automatic)), and the Go `fauna-mail-bridge` holds no DKIM key material at any point — the bridge-side key cache the 2026-06-23 firewall-exposure review (§ F12) closed, and the per-call FFI key copy it left as a residual, went with the bridge's signer (removed 2026-10-04).

Nest-side, `bins/fauna-nest/src/mail_dkim_key.rs::OutboundSigner` opens each key into `Zeroizing` memory for one hand-out batch and zeroizes it on drop. The Rust signer (`fauna_mail::outbound::dkim::sign`) is **stateless/call-scoped** — it borrows the key, parses a local signer, signs, drops it at function return; no caching.

### Verification

(Historical, for the deleted Rust daemon.) Standard unit tests passed after the zeroize wiring; the Drop impls didn't change observable behavior, so the daemon's existing tests covered regression. A specific "the field is zero after drop" test was omitted because:

1. Asserting on memory after drop in safe Rust is impossible (the buffer is freed; reading it via `unsafe` is UB).
2. A test that calls `String::zeroize()` directly tests the `zeroize` crate, not our Drop impl.
3. The Drop impls are 3-line obviously-correct code; code review is the appropriate check.

The structural property (a `Drop` exists; it calls `Zeroize::zeroize()` on the right fields) is enforced by the compiler — change the field types and the Drop impl no longer compiles.

## Related rules

- Submission-auth credential lifetime is owned **here**: `docs/goal/behavior/smtp-server.md` defers to this doc for it (stated from this side; the deferral pointer on the smtp-server side lands with its own cluster review).
- Engineering priority #1 (`principles.md` § Engineering principles) — uniformity. The same Drop-impl / `Zeroize`-on-every-exit-path pattern fits any future RPC param or capability type carrying plaintext credentials.
