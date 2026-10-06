# Dependency verification log — the dated history of § Dependency verification

Owns: dependency-verification-log
Status: ratified — a dated record; each entry carries its own date and, where it records a ruling, its own ratification; the target state these entries measure is owned by [`release-integrity.md`](release-integrity.md) § Dependency verification
Authority: owns the **dated history** of dependency verification only — what each scan, triage, disposition, drift, venue review and ledger seeding found, and when. It owns no rule: the scanners, the minimum release age, the review venue, the pre-scan ledger and every exception list are owned by [`release-integrity.md`](release-integrity.md); the bump process by [`dependency-bump-report.md`](dependency-bump-report.md).

Last verified: 2026-10-05 (split verbatim)

Split verbatim out of [`release-integrity.md`](release-integrity.md) § Dependency verification on 2026-10-05, when that doc stood at 207,687 B, 79% of the 262,144 B whole-file read ceiling and about six days from it at its growth rate. Nearly all of that growth was this section's dated `Correction` / `Update` entries — about forty, many several kilobytes — so the dated history is the seam: the section keeps its current-state target and the opening `READ FIRST` / `Repair` record of when scanning first ran; everything dated after it moved here, in its original order, unedited. **New dated entries are appended here, at the end, never in `release-integrity.md`.**

> **Reading this doc.** Its text was carried **verbatim**, so a word like *below*, *above* or *this section* may point at prose that stayed behind, and an unqualified `§ <name>` citation (§ *Reviewing untrusted source*, § *The pre-scan ledger*, § *Implementation status today*, § *Dependency currency*, § *Vendored forks*) names a section of [`release-integrity.md`](release-integrity.md), not of this page. Entries are in the order they were written: a later entry often corrects an earlier one, so read forward from any entry you land on. What holds **today** is `release-integrity.md` § *Implementation status today*, never an entry here.

## Section map

- **[The log](#the-log)** — the entries in order: the first real scan and its corrections (2026-07-17 / 07-22), the `quick-xml` and `rustls-webpki` closures and the durable lessons (2026-07-29 / 07-30 / 07-30b), the libcrux/openmls remediation and the seven dispositions (2026-07-30c), the re-runs (2026-08-10 → 08-26), the `cargo vet` drift and re-green (2026-09-08 / 09-10), RUSTSEC-2026-0285 (2026-09-16 / 09-18), the wasmtime tree's pre-scan and venue review (2026-10-02 / 10-04), the astro-7 npm REDs (2026-10-04), the pre-scan ledger's seeding, Cargo and npm (2026-10-05).

## The log

> ⚠️ **Correction (2026-07-17, from the first CI run): the `cargo vet` green below was STALE the
> moment it was written, and this is the durable lesson — a scanner result is valid only for the
> lockfile state it ran against.** It was measured *before* the same session's
> `crossbeam-epoch`→0.9.20 bump; that bump left the exemption pinned at 0.9.18, so `main` was
> actually **red** on vet (`crossbeam-epoch:0.9.20 missing ["safe-to-deploy"]`) from the bump
> until a later fix re-greened it. **Re-run the scanners AFTER the last dependency edit, never
> before.** CI caught it; no human did. (Also: `cargo audit` reports **19 denied warnings**
> alongside the vulnerability count — a second category the text below omits.) Per-job results,
> timings and the `govulncheck` fidelity defect: `merge-gates.md` § CI enforcement.
>
> **What the first real scan found** (run locally 2026-07-17 on the primary dev VM, where the
> tools are installed): `cargo vet --locked` ✅ green (196 audited / 9 partial / 975 exempted —
> *but see the correction above*), `cargo deny check bans licenses sources` ✅ green, **`cargo
> audit` ❌ RED — 19 vulnerabilities found, 18 remaining** (`crossbeam-epoch`→0.9.20 was the one fix a plain
> `cargo update` could reach, and was taken the same session), incl. two **8.2-high advisories
> in the MLS + PQ-KEM crypto path**
> (`libcrux-sha3` 0.0.8 under both `openmls_rust_crypto`→`hpke-rs` and
> `libcrux-ml-kem`→`fauna-pq-kem`; `libcrux-secrets` 0.0.5's **aarch64** constant-time
> swap/select bug, on a 100%-aarch64 fleet). Neither is reachable by `cargo update` — their
> parents pin them, so the fix is an openmls/libcrux stack upgrade. Remediation is a captured,
> tracked-elsewhere piece of work, **not** something this section may describe as handled.
> This is the concrete cost of the 3.5 months of inert scanning: real high-severity crypto
> advisories accrued unseen.
>
> **Update (2026-07-22):** a second `cargo update`-reachable fix landed — `rustls-webpki`
> 0.103.10→0.103.13, vetted via an already-imported Bytecode Alliance audit (no new local
> exemption needed) — bringing the count to **15 remaining**. The same re-verification pass
> found the 2026-07-17 scan's "absent from the default build graph" dismissal was **wrong** for
> `quick-xml` (reached via `c2pa`, unconditionally on `fauna-linux` and by default on
> `fauna-media`) — its two advisories are CVSS 7.5 unauthenticated-remote-DoS class, on a parser
> fed **user-uploaded media**, and the fix needs a `c2pa` version bump (untested compatibility,
> real breakage risk) rather than a bare `cargo update`. Full triage + what's still genuinely
> unreachable: the libcrux/openmls remediation track, 2026-07-22 correction.
>
> **Update (2026-07-29):** live re-verification, 6 days after the 2026-07-23 baseline below —
> `cargo vet --locked` (still green) and `cargo deny check bans licenses sources` (still `bans
> ok, licenses ok, sources ok`) hold, and `dep-inventory`'s counts (1330 total resolved /
> 1197 third-party / 133 Fauna-local, 916 shipped, 856 exemptions, 131-crate priority-1
> backlog, still-22 Fauna-audit count) are essentially unchanged from 2026-07-23. `cargo audit`
> moved the other way — **16 vulnerabilities (up from 15), 21 warnings (up from 19)** — on one
> new advisory disclosed *after* the 2026-07-22 check: RUSTSEC-2026-0216 (2026-07-25, CVSS 7.5,
> remote DoS via a malformed NIP-44 v2 payload) against `nostr` 0.44.4, pulled in via
> `nostr-sdk`/`nostr-connect`, which are **unconditional** dependencies of `bins/fauna-nest`
> (`bins/fauna-nest/Cargo.toml:345-346` — not gated by the crate's own `nostr` Cargo feature, which only
> toggles the separate `fauna-bridge-nostr` crate), so this ships in every nest binary. Unlike
> the libcrux/openmls advisories, a `cargo update -p nostr --dry-run` shows this one **is**
> `cargo update`-reachable (0.44.4 → 0.44.6, within `nostr-sdk`'s existing version constraint)
> — same shape as the crossbeam-epoch/rustls-webpki fixes above — but the update has not been
> taken as of this check. Not this section's job to apply; recorded here so the count stays
> honest. **Landed later the same day (2026-07-29):** `cargo update -p nostr` (0.44.4 → 0.44.6)
> taken as a standalone security fix; `cargo audit` back to **15 vulnerabilities / 20
> warnings** (the warning delta is 0.44.4's yanked-crate entry clearing alongside).
> RUSTSEC-2026-0216 is closed.
>
> **Update (2026-07-30) — the `quick-xml` pair from the 2026-07-22 correction above is CLOSED, and
> `quick-xml` is now absent from the audit entirely: 15 → 11 vulnerabilities, 20 → 19 warnings.**
> Three in-range dependency moves, each measured separately: (1) the workspace `c2pa` req
> 0.78 → 0.90 (resolved 0.78.4 → 0.90.3), which is what let `quick-xml` reach the fixed **0.41.0**
> on the user-uploaded-media path; (2) `cargo update -p plist` (1.9.0 → 1.10.0); (3) `cargo update -p
> tauri-winrt-notification` (0.7.2 → 0.7.3). `cargo vet --locked` green after
> `just vet-regen-exemptions` (190 fully audited / 10 partial / 989 exempted); `cargo deny check bans
> licenses sources` green. The `rand` 0.8.5 → 0.8.7 ripple cleared RUSTSEC-2026-0097 (unsound) as a
> by-product — that is the whole warning delta.
>
> ⚠️ **The durable lesson — bumping the identified parent alone moved the count by ZERO, and a
> session that stopped at "the fix is a `c2pa` bump" would have shipped a no-op.** `quick-xml@0.39.2`
> had **two independent parents** that cargo had *version-unified* into one lockfile entry: `c2pa`
> (the one the 2026-07-22 triage found) **and** `plist` → `netdev` → `netwatch` → `iroh` →
> `fauna-iroh` (Apple-target-gated, so invisible to a host-target `cargo tree -i`). Bumping `c2pa`
> gave it its own 0.41.0 entry and left 0.39.2 sitting under `plist` — same advisories, same count.
> Generalise: **an advisory on a shared transitive dep may have several independent parents hidden
> behind version unification; re-measure `cargo audit` after each bump instead of predicting the
> drop, and trace with `--target all`** (a host-only trace reported "nothing to print" for the very
> chain that kept the advisory alive). This compounds the 2026-07-22 lesson about bare
> `cargo tree -i` no-oping on an ambiguous name: both failure modes make a *quiet* trace read as
> proof of absence.
>
> A fourth `quick-xml` instance (0.37.5, ×2 advisories) sat under
> `tauri-winrt-notification` → `notify-rust` → `fauna-linux` — a **Windows-only** notification
> backend inside the Linux app's dep tree, so it compiled into no shipped artifact then (since
> 2026-09-26 fauna-tui's windows build compiles it — `apps/tui.md` § System integration). It was cleared
> anyway by move (3) above, a single-package zero-ripple update, rather than left as a
> reachability argument to re-derive.
>
> ⚠️ **`c2pa` 0.90.3 *requires* `wasm-bindgen` 0.2.126** (0.2.114 before), pulling `js-sys` 0.3.103 /
> `web-sys` 0.3.103 / `wasm-bindgen-futures` 0.4.76 with it — these are exact-pinned to each other,
> so the trio cannot be held back while `c2pa` 0.90 is in the tree. Confirmed *required*, not an
> opportunistic `cargo update -p` ripple: a **minimal** re-resolution (pristine lock + the new req +
> `cargo metadata`) produces the identical delta. Consequence for whoever next builds web: the
> `wasm-bindgen` CLI must match the crate version, and `wasm-pack`'s cache held the
> 0.2.114 CLI at the time of this bump. **Verified end-to-end the same session** rather than assumed:
> a real `just wasm-launch` build compiled the 0.2.126 crates, `wasm-pack` auto-installed the
> matching CLI, `wasm-opt` ran, and a valid artifact was produced — so no CLI/schema mismatch is
> outstanding for web.
>
> **Cross-compilation re-verified, because the bump put a load-bearing claim at risk.**
> `libs/fauna-ffi/Cargo.toml` asserts that `image`/`img-parts`/`c2pa` all cross-compile to the four
> Android ABIs + the Apple targets — written against 0.78, and **no gate anywhere builds either
> target set**. Android is now re-verified at 0.90.3 (`cargo check -p fauna-media --features
> c2pa-detect --target aarch64-linux-android`, clean, zero warnings; note this needs the NDK clang
> wrappers on `PATH` — a bare cross-`check` fails in the pre-existing, unrelated `zstd-sys` build
> script from `fauna-core`, not in the c2pa tree). The **Apple** half was verified separately and is
> GREEN at this `c2pa` version (tracked internally). Corroborating signal for both: the bump's graph delta *removes*
> `sha2-asm` (assembly — the cross-compile-hostile kind) and adds only the pure-Rust brotli stack.
>
> **The same `--target all` re-check was applied to EVERY other advisory the 2026-07-17/22 triage
> had dismissed as unreachable, not just the one that broke** — and it turned up a second
> mis-stated reachability, this time in the nest binary:
>
> * `quinn-proto`, `libcrux-chacha20poly1305`, `libcrux-aesgcm` — dismissals **hold**; `cargo tree -i
>   <name>@<ver> --target all` is empty for all three (they are *feature*-gated off, not
>   target-gated, so the host-only trace was not misleading here).
> * ⚠️ **`rustls-webpki@0.101.7` (RUSTSEC-2026-0104 / -0098 / -0099) IS production-reachable in
>   `bins/fauna-nest`** — `rustls 0.21.12` ← `aws-smithy-http-client` ← `aws-smithy-runtime` ←
>   `aws-runtime` ← `aws-sdk-s3` ← `fauna-nest`, an unconditional dependency, so it ships in every
>   nest binary. The 2026-07-22 note called this instance *"genuinely unreachable-by-`cargo
>   update`"* — accurate about **updatability** (there is no in-range fix for the 0.101.7 line) but
>   it reads as **unreachability**, which is false. Keep the two words apart: *no in-range fix* and
>   *not in the build graph* are different claims with different remediations.
>   **Shape of the fix (not taken here — a separate track):** the nest currently links **two rustls
>   major lines at once** (0.21.12 via the aws-sdk chain, 0.23.37 everywhere else; correspondingly
>   `rustls-webpki` 0.101.7 **and** 0.103.13), so the remediation is moving the aws-sdk chain onto a
>   rustls-0.23-based `aws-smithy-http-client`, not bumping `rustls-webpki`. In-range
>   `aws-smithy-runtime` updates do exist (1.10.3 → 1.11.3, latest 1.12.1); whether any of them
>   drops the 0.21 line is **unverified** (the crates.io index was returning 503s at check time).
>
> **Update (2026-07-30b) — `rustls-webpki@0.101.7` is CLOSED: 11 → 8 vulnerabilities, and the nest no
> longer links two TLS stacks.** It needed **no dependency version change at all** — not the
> `aws-smithy-runtime` bump the note above proposed, and not the `aws-sdk-s3` major bump it named as
> the fallback lever. The 0.21 line was pulled in by a *feature choice*, and both of `aws-sdk-s3`'s
> own TLS features are wrong for this workspace:
>
> | `aws-sdk-s3` feature | resolves to | what it links |
> |---|---|---|
> | `rustls` (what the nest had) | `aws-smithy-runtime/tls-rustls` → `aws-smithy-http-client/legacy-rustls-ring` | **rustls 0.21 + hyper 0.14** — the second stack, and the source of the three advisories |
> | `default-https-client` | `aws-smithy-runtime/default-https-client` → `aws-smithy-http-client/rustls-aws-lc` | rustls 0.23, but with **`aws_lc_rs`** |
>
> ⚠️ **`default-https-client` is the trap here, and it is the reading a hurried session would take** —
> it is the modern-sounding feature, it drops the legacy stack, and it *does* clear the CVEs. It would
> also have enabled rustls' `aws_lc_rs` feature **alongside** the workspace `ring` pin
> (`Cargo.toml`'s `rustls = { features = ["ring", …] }`) on one unified `rustls 0.23` entry. With both
> features on, rustls' `from_crate_features()` yields **no** provider, so every
> `ClientConfig`/`ServerConfig::builder()` that relies on the crate-feature default panics at runtime
> unless a process default was installed first — a latent panic reachable from TLS setup, in exchange
> for a CVE fix. (It would also add `aws-lc-rs`'s C/cmake build to the nest image and turn on
> `prefer-post-quantum`.) **A dependency remediation that silences a scanner while introducing a
> runtime panic is not a fix; check what a TLS feature does to the crypto *provider*, not just to the
> version number.**
>
> So the nest takes **neither** feature and supplies the SDK's HTTP client itself, from a direct
> `aws-smithy-http-client` dependency with **`rustls-ring`** — rustls 0.23 + `ring`, identical to the
> rest of the nest (`bins/fauna-nest/src/s3_blob_store.rs::https_client`, a shared `OnceLock` so the
> per-user stores share one connection pool). `aws-sdk-s3` stays at 1.119.0, so there is **no API-compat
> question** against the S3 call sites.
>
> Measured, not predicted (per the lesson above): `cargo audit` **11 → 8**, dropping exactly
> RUSTSEC-2026-0104 / -0098 / -0099 and nothing else; warnings unchanged at 19. The `Cargo.lock` delta
> is a **pure removal of 7 packages** — `rustls@0.21.12`, `rustls-webpki@0.101.7`, `hyper@0.14`,
> `h2@0.3`, `hyper-rustls@0.24`, `tokio-rustls@0.24`, `sct` — with **zero additions**, so the duplicate
> TLS stack is gone from the binary rather than merely upgraded. Absence is verified in the strong
> form: `cargo tree -i rustls@0.21.12 --target all --all-features` reports *no such package in the
> resolution*, which is the distinction this section keeps insisting on — not "absent from the default
> graph" but absent from the graph under every feature and target.
>
> `cargo vet --locked` stayed green with **no** `vet-regen-exemptions` run needed (190 fully audited /
> 10 partial / 982 exempted — the exemption count fell by exactly the 7 removed packages, since a
> pure removal cannot uncover a crate); `cargo deny check bans licenses sources` green; `cargo fmt`
> and `goal-lint` clean.
>
> The S3 blob store had **no test of any kind** before this change, which is why swapping its
> transport was riskier than the diff looked. It now has four (`s3_blob_store.rs` `mod tests`,
> wiremock-backed): the load-bearing one asserts a `put` actually **arrives at the endpoint**, because
> `aws-sdk-s3` accepts a config with no HTTP client at construction time and fails only at request
> time — so compilation proves nothing about whether the supplied client is wired. Those tests also
> pin that `build_https()` still serves plain-`http://` endpoints, which an S3-compatible store may
> sit behind (local MinIO) and an https-only connector would have broken silently.
>
> 🐛 **Writing those four tests immediately exposed a pre-existing bug that made the S3 path
> non-functional in its entirety, and it is the strongest argument in this section for testing a
> dependency path rather than reasoning about it.** `aws-sdk-s3` requires a *behavior major version*
> from either `Builder::behavior_version(..)` or its `behavior-version-latest` feature, and it
> enforces this with a **runtime panic**, not a compile error. The nest takes the SDK with
> `default-features = false` (so it never got the feature) and never called the setter — so **every**
> `put`/`get`/`head`/`list` panicked with *"A behavior major version must be set"*. All four tests
> failed on it before the fix, which is the red-first proof; the fix is one explicit
> `.behavior_version(BehaviorVersion::latest())` call, preferred over the feature so the requirement
> is visible at the call site. This was **independent of the TLS change** (the feature was absent
> before and after) and had shipped in every nest binary: the live callers then were the
> client-supplied backup destinations (`storage_handlers.rs::s3_dest_from`, `backup/service.rs`;
> both since retired — `S3BlobStore` has no production caller today, `nest/common.md` § Blob Store
> → Backend), not the server-config path, whose `from_config` ctor `config.rs:150` already records as never called. The
> panic unwinds the request task rather than killing the nest (the workspace deliberately avoids
> `panic = "abort"` for UniFFI's `catch_unwind`), so it presented as a broken feature rather than a
> crash — and with no test anywhere on this path, nothing was positioned to notice.
>
> 🐛 **Extending those tests over the rest of the file then found a second pre-existing defect, this
> one client-triggerable:** `list_all_hashes`'s pagination loop did not terminate on every response
> shape a server could return. The endpoint is **client-supplied** (a backup destination carries its
> own `s3_endpoint` over the wire), so the rule that now holds is that pagination always makes
> progress or stops, whatever an untrusted S3-compatible server answers: the loop breaks unless a
> continuation token is actually present. Pinned red-first: the guard test hung for its full
> 30 s budget before the fix and returns in 0.06 s after. **Both defects on this path were found by
> writing tests for a transport change, not by reading the code** — the second one in code the
> transport change never touched, which is the argument for covering a path's whole surface once you
> are in it.
>
> **Update (2026-07-30c) — the libcrux/openmls remediation track ran: 8 → 7 (`quinn-proto` fixed),
> and the two 8.2-high crypto advisories are proven STRUCTURALLY UNFIXABLE today — every remaining
> finding now carries a written disposition.** Measured 2026-07-30: `cargo audit` 7 vulnerabilities /
> 19 warnings; `cargo vet --locked` green after `just vet-regen-exemptions` (190/10/982 — the regen
> also pruned the 7 stale exemptions left by 2026-07-30b's package removals); `cargo deny` green.
> `quinn-proto` 0.11.14 → 0.11.15 was a single-package in-range move (the advisory's exact fix
> version); it was lock-ghost-only, but clearing beats explaining.
>
> **Why the 8.2-high pair cannot be fixed by any version choice (verified by a real failed
> resolution, not prediction — do not re-derive):** the upstream fix set is the 2026-07-15 release
> wave — `libcrux-sha3 0.0.10`, `libcrux-secrets 0.0.6`, `libcrux-ml-kem 0.0.10`, `hpke-rs 0.7.0` —
> and that entire wave requires **`hax-lib ^0.3.7`**, while `hpke-rs 0.6.1` (mandatory via
> `openmls_rust_crypto 0.5.1`, the **latest** released provider — reqs `hpke-rs ^0.6.0`; no
> `hpke-rs 0.6.2` exists; `openmls_libcrux_crypto 0.3.1` rides the same wave) pins
> `libcrux-sha3 =0.0.8` → **`hax-lib =0.3.6`**. Both reqs sit in the one 0.3.x line cargo must
> unify, and their intersection is empty — so `fauna-pq-kem` cannot take `libcrux-ml-kem 0.0.10`
> while the released openmls stack is in the graph. The unblock levers are exactly two: **upstream
> ships an openmls provider against hpke-rs 0.7** (watched monthly),
> or **Fauna forks `openmls_rust_crypto`** (a new vendored fork: user-gated). Prepared for the
> eventual bump: `fauna-pq-kem::tests::pinned_vectors_survive_libcrux_bumps` pins keygen / derand
> encaps / decaps / implicit-rejection digests captured on 0.0.8 (aarch64), so the bump, whenever it
> lands, proves derived-byte stability for persisted X-Wing envelopes
> (`version-compatibility.md` § the additive-everywhere contract) instead of asserting it.
>
> **Dispositions for the 7 remaining (dated 2026-07-30, per this section's
> fixed / exempted-with-reason / shown-unreachable bar):**
>
> * **`libcrux-secrets 0.0.5` — RUSTSEC-2026-0212 (8.2, aarch64 const-time swap/select), the one
>   real exposure: ACCEPTED pending upstream — USER-RATIFIED 2026-07-30**, with the user's rider that
>   the potential exploitation consequences be recorded, which is the paragraph after next.
>   Production-reachable via `libcrux-ml-kem 0.0.8` ← `fauna-pq-kem` in every shipped artifact, on an
>   all-aarch64 fleet. Accepted because: the misbehavior is environment-dependent incorrect *output*
>   (not key leakage); our aarch64 test fleet plus the wasm (portable-backend) e2e cross-checks
>   exercise these exact flows continuously with zero divergence observed; and the alternative —
>   forking `openmls_rust_crypto` today — trades a time-bounded upstream wait for a standing
>   supply-chain liability.
>
>   **Consequences if it were exploited (recorded at the user's direction):** the security-relevant
>   select in ML-KEM is the **implicit-rejection choice in decapsulation** — pick the real shared
>   secret for a valid ciphertext, the pseudo-random rejection secret for an invalid one. A misfire
>   in the benign direction (valid → rejected) is an availability bug: visible AEAD/unseal failures.
>   The dangerous direction (invalid → **not** rejected) would hand back the raw, unauthenticated
>   K-PKE decryption result, degrading the Fujisaki-Okamoto transform — exactly the
>   chosen-ciphertext decaps oracle that implicit rejection exists to close, and X-Wing decaps
>   surfaces do accept client-supplied ciphertexts (wrapped-blob envelopes, subscription KeyBlobs,
>   bridge routing). In the worst case that oracle class supports key-recovery-style attacks against
>   the **ML-KEM half** of X-Wing. Two structural containments bound the blast radius: **(1) the
>   hybrid combiner** — `ss = SHA3-256(ss_M ∥ ss_X ∥ …)` mixes the X25519 DH, so even a fully
>   compromised ML-KEM half exposes no plaintext unless the attacker *also* breaks X25519; the
>   consequence is degradation from PQ+classical to classical-only protection on affected
>   operations, which is today's pre-PQ baseline. **(2) The trigger is not attacker-controllable** —
>   whether the `cmp` sees garbage high bits is a property of the compiled binary (codegen), not of
>   any input; current binaries demonstrably select correctly on these flows (continuous aarch64
>   test + e2e coverage, plus `pinned_vectors_survive_libcrux_bumps` exercising the
>   implicit-rejection path on aarch64), and the pinned Rust toolchain means codegen changes arrive
>   only as deliberate, gate-tested pin-bump commits that re-run those tests. Residual risk: a
>   future codegen shift silently activating the hazard between pin bumps — bounded by the monthly
>   watch whose job is to take the upstream fix wave the moment it
>   is resolvable.
> * **`libcrux-sha3 0.0.8` — RUSTSEC-2026-0207/-0208 (8.2 ×2): EXEMPTED — no Fauna runtime path can
>   execute the affected functions.** Both advisories scope themselves out of ML-KEM/ML-DSA usage
>   (0207 = portable *incremental multi-squeeze* SHAKE only; 0208 = `avx2::x4::shake256` with output
>   >32 not divisible by 8, x86-64 only), so the `fauna-pq-kem` path is unaffected by the advisories'
>   own text. The only other build-graph parent is `hpke-rs 0.6.1`, whose SHAKE surface serves its
>   PQ/hybrid HPKE KEMs — and `fauna-mls` pins the fully classical
>   `MLS_128_DHKEMX25519_CHACHA20POLY1305_SHA256_Ed25519` (`libs/fauna-mls/src/engine.rs:25`), whose
>   RFC 9180 construction (DHKEM-X25519 / HKDF-SHA256 / ChaCha20-Poly1305) contains no SHA-3-family
>   primitive at all. Cleared for real when the openmls stack moves (watch above).
> * **`libcrux-chacha20poly1305 0.0.7` (0124) + `libcrux-aesgcm 0.0.7` (0211/0209): lock-ghost only.**
>   All parents sit under `hpke-rs 0.6.1`'s optional `hpke-rs-libcrux` backend, which no build
>   enables — `cargo tree -i <crate>@<ver> --target all --all-features` is empty for all three
>   (re-confirmed; first recorded 2026-07-30). Cargo locks optional subtrees unconditionally, so these
>   entries cannot leave `Cargo.lock` until `hpke-rs` moves; `aesgcm` additionally has **no fixed
>   release upstream**. Unreachable-in-graph, recorded.
> * **`rsa 0.9.10` — RUSTSEC-2023-0071 Marvin (5.9): EXEMPTED — no fix exists upstream** (advisory
>   open since 2023; the crate's maintainers have shipped none). Fauna's private-key RSA is PKCS#1
>   v1.5 *signing* (ActivityPub HTTP signatures — `fauna-bridge-activitypub/src/identity.rs:38` — and
>   DKIM material in `fauna-provisioning`); the Marvin oracle's primary surface, RSAES-PKCS#1-v1.5
>   *decryption*, does not exist in this codebase. Remote timing of signing ops across network jitter
>   is the residual exposure — accepted as famous/unfixable; re-check whenever the `rsa` crate ships
>   a constant-time release.
>
> With those five dispositions plus the two fixes (quinn-proto here, the 2026-07-30b removals), the
> `cargo audit` residue is 100% explained: nothing red is unexamined, and nothing was silenced to
> make the gate look green.
>
> **Update (2026-08-10):** re-verification found **15 vulnerabilities, up from the known-7
> baseline** — 8 fresh advisories, all disclosed 2026-08-01: six against `nostr`
> (RUSTSEC-2026-0225..0230, incl. a 5.5 debug-output credential leak exposing NIP-46/NIP-60
> secrets and four 7.5-high NIP-parser resource-exhaustion/panic bugs) and two against
> `nostr-relay-pool` (RUSTSEC-2026-0231/-0232, both 7.5-high — relay-auth-challenge memory
> exhaustion and processing of unverified relay events). Fixed with two in-range patch bumps,
> `cargo update -p nostr -p nostr-relay-pool` (0.44.6→0.44.8, 0.44.2→0.44.3, both within
> `nostr-sdk`/`nostr-connect`'s existing `^0.44.1` requirement — no `Cargo.toml` edit), taken as a
> standalone security fix; `cargo audit` back to **7 vulnerabilities / 21 warnings** (the exact
> dispositioned set above); `cargo vet --locked` green after `just vet-regen-exemptions`
> (193/10/979).
>
> ⚠️ **Correction to the 2026-07-29 entry above: `nostr-sdk`/`nostr-connect` are NOT unconditional
> dependencies of `bins/fauna-nest` — they are `[dev-dependencies]`, and always have been.**
> `git log --oneline -S nostr-sdk -- bins/fauna-nest/Cargo.toml` shows exactly one commit ever
> touched them, in a commit (2026-07-20, "add nostr-sdk dev-dep for the N2/S6 interop harness"),
> which added them directly under the `[dev-dependencies]` section header
> (`bins/fauna-nest/Cargo.toml:198`) — they never sat anywhere else. `cargo tree -i nostr@<ver>` /
> `nostr-relay-pool@<ver>` (re-run today) confirm the whole `nostr`/`nostr-connect`/
> `nostr-database`/`nostr-gossip`/`nostr-relay-pool`/`nostr-sdk` subtree reaches `fauna-nest`
> exclusively via `[dev-dependencies]` — it backs the Nostr interop/conformance test suite, not the
> shipped binary. The 2026-07-29 entry's "ships in every nest binary" framing was wrong when
> written (the fix taken that day was still correct, just not for the stated reason). **Lesson:
> verify a dependency's `[dependencies]` vs `[dev-dependencies]` placement directly in the
> manifest before asserting shipped-surface reachability — don't infer it from an advisory's
> severity or a crate's mere presence in `Cargo.lock`.**
>
> **Update (2026-08-13) — full live re-run, 3 days after the 2026-08-10 baseline: the
> vulnerability set is unchanged (still exactly the 7 dispositioned above), but the warning
> count moved 21 → 24.** `cargo vet --locked` unchanged green (193 fully audited / 10 partially
> / 979 exempted); `cargo deny check bans licenses sources` unchanged green (`bans ok, licenses
> ok, sources ok`); `dep-vendor-drift` unchanged (`OK — 2 vendored fork(s) match their
> recorded divergence`); the release credential's scopes unchanged (see
> § *Implementation status today* below). `cargo audit` found the identical 7-vulnerability
> set (both `libcrux-aesgcm` advisories, `libcrux-chacha20poly1305`, `libcrux-secrets`, both
> `libcrux-sha3` advisories, `rsa` — matching every disposition above verbatim), so none of
> those dispositions need revisiting. The 3 extra warnings are **not new dependency
> additions** — the 2026-08-10 fix commit's `Cargo.lock` confirms `lru`
> (0.12.5/0.16.4/0.18.0), `rand` (0.8.7/0.9.2/0.10.0), `spin` (0.9.8/0.10.0) and
> `async-utility` (0.3.1) were already resolved at those exact versions on 2026-08-10 —
> so the 21-count recorded that day undercounted against the advisory database as it stood,
> rather than the tree having grown. No action needed: these are
> `unmaintained`/`yanked`/`unsound` **warnings** (allowed, not gate-failing), not
> vulnerabilities, and every one traces to a crate version already present when the prior count
> was taken.
>
> **Update (2026-08-21) — full live re-run, 8 days after 2026-08-13: one NEW, undispositioned
> advisory.** `cargo audit` now finds **8 vulnerabilities (up from the known-7 dispositioned
> set), 24 warnings (unchanged)**; `cargo vet --locked` unchanged green (193/10/979); `cargo deny
> check bans licenses sources` unchanged green; `dep-vendor-drift` unchanged (2 forks match
> recorded divergence); the release credential's scopes unchanged; `just
> dep-inventory` essentially unchanged (1182 third-party / 905 shipped / 247 build-time-exec / 852
> exemptions / 132 priority-1 backlog — identical to 2026-08-13; only the Fauna-local split moved,
> from continued feature landings). **The new finding: RUSTSEC-2026-0258 (`h2` 0.4.13, "unbounded
> empty DATA frames" — unbounded memory / panic on undrained streams, low severity, disclosed
> 2026-08-17, patched in ≥0.4.16) — production-reachable**, confirmed via `cargo tree -i
> h2@0.4.13 --target all --all-features`: `h2` ← `aws-smithy-http-client` ← `fauna-nest`
> (unconditional, non-dev — the same direct S3 HTTP-client dependency the 2026-07-30b entry
> above introduced). Not fixed here (a docs sweep makes no dependency-version edits): the
> in-range fix is `cargo update -p h2` (0.4.13 → ≥0.4.16), a separate track.
>
> **RUSTSEC-2026-0258 is CLOSED (2026-08-21, same day).** `cargo update -p h2`
> (0.4.13 → 0.4.18, above the ≥0.4.16 floor); `cargo audit` back to the known-7-dispositioned set
> (8 → 7, `h2` no longer among them); `cargo vet --locked` re-green after `just
> vet-regen-exemptions` moved the `h2` exemption's version and pruned one stale entry
> (`toml_datetime`, no longer in the tree) — 194 fully audited / 10 partially / 978 exempted;
> `cargo deny check bans licenses sources` unchanged green; `dep-vendor-drift` unchanged (2
> forks match recorded divergence). Verified no regression: `cargo check --workspace --all-targets`
> is clean outside the pre-existing, unrelated wasm-family-on-native limitation (owned elsewhere —
> those crates require the `wasm32` target, not a native check); `cargo test -p fauna-nest --lib`
> (the confirmed h2 consumer) — 3733 passed / 0 failed.
>
> **Update (2026-08-26) — full live re-run, 5 days after 2026-08-21: the dispositioned
> 7-vulnerability set is unchanged, no new advisories.** `cargo audit` — identical 7
> vulnerabilities (both `libcrux-aesgcm`, `libcrux-chacha20poly1305`, `libcrux-secrets`, both
> `libcrux-sha3`, `rsa`, matching every disposition above verbatim) / 24 warnings (unchanged);
> `cargo deny check bans licenses sources` unchanged green; `dep-vendor-drift` unchanged (2
> vendored forks — `libs/ksni` and the internal-only openMLS fork, both still present in this
> repository — match their recorded divergence; the public-tree-only excision transform landed
> does not touch this repository's own fork, § *Vendored forks*); the release
> credential's scopes unchanged. **`cargo vet --locked` stayed green but its counts
> moved (194/10/978 → 191/9/961)** — not a review regression: the WireGuard-stack
> deletion (2026-08-23) dropped `boringtun`/`smoltcp`/`tun` and their now-unreachable transitives
> from `Cargo.lock` and `[exemptions]`, shrinking the tree the gate counts over. (Per-job CI
> results and run-level `supply-chain.yml` contention stay `merge-gates.md` § CI enforcement's
> remit, not restated here.)
>
> **Update (2026-09-08) — `cargo vet --locked` drifted RED again, unnoticed since at least
> 2026-08-26: 6 unvetted dependencies** (`assert-json-diff:2.0.2`, `deadpool:0.12.3`,
> `deadpool-runtime:0.1.4`, `instant-acme:0.8.5`, `rustls-platform-verifier:0.6.2`,
> `wiremock:0.6.5`, all missing `safe-to-deploy`), reproduced by running the gate locally and
> cross-confirmed via `gh run list --workflow=supply-chain.yml` — the `cargo-vet` job has failed
> on every one of the last 50 runs (2026-09-02 → 2026-09-08). This is exactly the class this
> section already names ("the fleet adds/bumps deps via the merge gate without running vet"),
> recurring: none of the fleet's dep-adding commits in that window ran the burn-down step before
> landing. Fix (mechanical, not yet applied — a docs-only sweep makes no dependency edits):
> `just vet-regen-exemptions`, verify the diff only adds these 6, re-confirm green, land through
> the merge gate. Captured. `cargo audit` (26 warnings, same
> dispositioned 7-vulnerability set), `cargo deny check bans licenses sources`, and
> `dep-vendor-drift` (2 forks) all independently re-verified unaffected and still green.
>
> **Update (2026-09-10) — fixed, `cargo vet --locked` re-green.** `cargo vet regenerate exemptions`
> (`just vet-regen-exemptions`) re-confirmed locally: `Vetting Succeeded (192 fully audited, 9
> partially audited, 960 exempted)`. This corrects the above entry's "not yet applied"/"only adds"
> framing: the regen diff on the 6 named crates was **criteria/version changes on pre-existing exemption
> entries, not new adds** — `assert-json-diff`/`deadpool`/`deadpool-runtime`/`wiremock` each moved
> `safe-to-run` → `safe-to-deploy`; `instant-acme`'s existing entry moved 0.7.2 → 0.8.5;
> `rustls-platform-verifier` gained a second entry at 0.6.2 alongside its existing 0.7.0 (both
> versions coexist in `Cargo.lock`). Alongside those 6, the regen also pruned ~15 exemption entries
> for crates confirmed absent from `Cargo.lock` (`bayespam`, `c2rust-bitfields{,-derive}`,
> `defmt{,-macros,-parser}`, `hash32`, `heapless`, `ip_network{,_table,_table-deps-treebitmap}`,
> `simdutf8`, a stale `winreg` 0.55.0 duplicate — its live 0.50.0 entry survives unchanged —
> `wintun-bindings`), each individually checked against `Cargo.lock` before accepting the prune, per
> the precedent already recorded in this section's 2026-08-21 entry. **The cause was not "the fleet
> forgot" in general — two specific, identifiable commits:** (1) a commit (2026-09-02, IP bridge
> cert) bumped `instant-acme` 0.7→0.8 on the nest's own **shipped** ACME/TLS path
> (`fauna-acme-core`/`fauna-acme-http01`/`fauna-client-dns` → `fauna-nest`, confirmed via
> `cargo tree -e normal -i instant-acme`), pulling in `rustls-platform-verifier` 0.6.2 via
> `hyper-rustls` 0.27.7 — both genuinely `safe-to-deploy`-tier, not the "test/dev-adjacent" crates
> the entry above speculated. (2) a commit (2026-09-03, shared `MockNest`)
> made `wiremock` an optional `test-helpers` feature of `fauna-sync-engine`
> (`libs/fauna-sync-engine/Cargo.toml`); `cargo vet` resolves with all features on, so `wiremock`
> and its transitives (`deadpool`, `deadpool-runtime`, `assert-json-diff`) were newly demanded at
> `safe-to-deploy` even though `cargo tree --workspace -e normal -i wiremock` (default features)
> shows none of the four reach any shipped artifact — a cargo-vet all-features-resolution artifact
> of the deliberate double-build shape § *e2e-automation-surface-gating.md* documents, not an actual
> shipped-closure change. `cargo audit` (unchanged dispositioned 7-vulnerability set), `cargo deny
> check bans licenses sources`, and `dep-vendor-drift` (2 forks) all independently re-verified
> unaffected and still green; `Cargo.toml`/`Cargo.lock` untouched (dependency-metadata-only change).
> Closed.
>
> **Update (2026-09-16) — `cargo audit` found a NEW, undispositioned, production-reachable
> vulnerability: RUSTSEC-2026-0285 (`rustls` 0.23.37, "TLS 1.3 handshake messages incorrectly
> accepted across encryption level boundaries," 5.3 medium, disclosed 2026-09-14 — two days
> before this re-run).** `cargo audit` now finds **8 vulnerabilities (up from the known-7
> dispositioned set above, which is otherwise unchanged verbatim — both `libcrux-aesgcm`,
> `libcrux-chacha20poly1305`, `libcrux-secrets`, both `libcrux-sha3`, `rsa`), 26 warnings (up
> from 24 on 2026-08-26 — no new deps at those versions, same advisory-database-churn pattern
> as the 2026-08-13 entry above, not investigated further per this section's own precedent for
> non-gate-failing warnings)**. Confirmed production-reachable via `cargo tree -i
> rustls@0.23.37 --target all --all-features`: `rustls` ← `aws-smithy-http-client` /
> `fauna-acme-http01` / … ← `fauna-nest` (unconditional, non-dev — the same S3/ACME
> HTTP-client dependency chain the 2026-07-30b and 2026-09-10 entries above name). Not fixed
> here (a docs sweep makes no dependency-version edits): `cargo update -p rustls --dry-run`
> confirms an in-range patch bump is available (`rustls` 0.23.37 → 0.23.45, `rustls-webpki`
> 0.103.13 → 0.103.15, no `Cargo.toml` edit needed) — the same mechanical shape as the
> 2026-08-21 `h2` fix. Cross-confirmed via `gh run list --workflow=supply-chain.yml` +
> `gh run view` on the latest run (35046170346, 2026-09-16): `cargo-audit` is the sole failing
> job (as designed, while the dispositioned set sits unfixed) on every run back through at
> least 2026-09-10; `cargo-vet`, `cargo-deny`, `dep-prescan`, `govulncheck` and
> `vendored-forks` are all green on the same runs. `cargo vet --locked` re-run live and
> unchanged green (`Vetting Succeeded — 192 fully audited, 9 partially audited, 960 exempted`,
> matching the 2026-09-10 entry exactly); `cargo deny check bans licenses sources` unchanged
> green (`bans ok, licenses ok, sources ok`); `dep-vendor-drift` unchanged (`OK — 2 vendored
> fork(s) match their recorded divergence`). Third-party GitHub Actions enumeration
> re-verified exact match (still eleven distinct actions across the three shipped workflows in
> `.github/workflows/`, all 40-hex-SHA-pinned with a trailing
> `# vX.Y.Z` comment); the Dockerfile's four `FROM` images remain digest-pinned. 
>
> **Update (2026-09-18) — RUSTSEC-2026-0285 is FIXED.**
> `cargo update -p rustls` (0.23.37 → 0.23.45, `rustls-webpki` 0.103.13 → 0.103.15, in-range
> patch bump, no `Cargo.toml` edit). `cargo audit` back to the known-7-dispositioned set (8 → 7,
> `rustls` no longer among them; both `libcrux-aesgcm`, `libcrux-chacha20poly1305`,
> `libcrux-secrets`, both `libcrux-sha3`, `rsa`, matching every disposition above verbatim). The
> job itself stays red — 26 warnings (unchanged from the 2026-09-16 entry: same undispositioned
> advisory-database churn that entry already attributes to unrelated causes, not this fix) and
> `supply-chain.yml`'s `cargo-audit` job continues to fail **by design** while those 7
> dispositioned vulnerabilities sit unfixed (release-integrity.md:1288-1290) — this fix closes
> only the newly-disclosed CVE, not the standing baseline. `cargo vet --locked` immediately went
> red as expected: `rustls:0.23.45` / `rustls-webpki:0.103.15` missing `safe-to-deploy` (the
> `[[exemptions.rustls]]` entry was pinned at exactly `0.23.37`, `supply-chain/config.toml:2706`
> pre-fix). `just vet-regen-exemptions` re-green: `Vetting Succeeded (193 fully audited, 10
> partially audited, 958 exempted)` — the regen pulled existing upstream
> `audits.bytecode-alliance` delta audits covering both deltas verbatim (`rustls` 0.23.37 →
> 0.23.45, `rustls-webpki` 0.103.13 → 0.103.15) plus one incidental `rustc_version` 0.4.0 → 0.4.1
> delta audit, and pruned the now-redundant standalone `rustc_version` 0.4.1 exemption — no new
> hand-authored exemption needed, unlike the h2/2026-08-21 precedent. `cargo deny check bans
> licenses sources` unaffected, unchanged green (`bans ok, licenses ok, sources ok`). `cargo test
> -p fauna-nest --lib` re-run per the h2 precedent: **4188 passed, 1 failed** — the failure
> (`rpc_router::tests::router_and_kind_registry_agree_on_every_kind`) is a pre-existing,
> unrelated `KindRegistry` drift (`fauna.account.state.retire` dispatched by the nest since
> 2026-09-16, never registered client-side), confirmed predating this fix and touching none of `Cargo.lock` /
> `supply-chain/*` — not fixed here (out of scope for a dependency-version-edit track), captured
> separately.
>
> **Update (2026-10-02) — the wasmtime component-model runtime landed; the tree is pre-scanned and grandfathered, NOT reviewed, and the pre-scan's two REDs are open.** The nest's WASM plugin sandbox landed `wasmtime` 49.0.1 (the user's 2026-09-26 approval: crates.io under cargo vet, the minimum feature set — `runtime`, `cranelift`, `component-model`, `async`, `std`, `anyhow`, `default-features = false` — at `Cargo.toml`'s `[workspace.dependencies].wasmtime`). **46 registry crate@versions introduced** (`dep-changed --git-base <the landing's parent> --registry-only`; an earlier count of 49 was wrong — the `wit-component`/`wit-parser`/`wasm-metadata` 0.244.0 line predates the landing and stays beside the new 0.258.0 one): the Bytecode Alliance family — `wasmtime`, `wasmtime-environ` and eleven `wasmtime-internal-*` crates at 49.0.1, `pulley-interpreter`/`pulley-macros` 49.0.1, thirteen `cranelift-*` crates at 0.136.1, `regalloc2` 0.15.2, `wasm-encoder`/`wasmparser`/`wasmprinter`/`wasm-metadata`/`wit-component`/`wit-parser` 0.258.0 — all covered by the `bytecode-alliance` import's **wildcard audits bound to trusted publishing** (`trusted-publisher = "github:bytecodealliance/wasmtime"`, `…/wasm-tools`, `…/wit-bindgen` in `supply-chain/imports.lock`: the version must have been published by that repository's release workflow — an organizational-accountability binding, not a maintainer account); `itertools` 0.14.0 by an imported delta audit; `cpp_demangle` 0.5.1 by imported deltas 0.3.5 → 0.4.3 → 0.5.1 over a **new Fauna exemption at 0.3.5**; and **ten more Fauna exemption edits** re-grandfathered by `just vet-regen-exemptions` — new entries for `addr2line` 0.26.1, `anyhow` 1.0.104, `gimli` 0.33.0, `mach2` 0.6.0, `memfd` 0.6.6, `object` 0.40.0, `rustc-demangle` 0.1.28 and `target-lexicon` 0.13.5, bumped entries for `indexmap` 2.13.0 → 2.14.2 and `libc` 0.2.183 → 0.2.189 — eleven exemption edits in all, counting `cpp_demangle`. `cargo vet --locked` re-run this date: `Vetting Succeeded (226 fully audited, 12 partially audited, 968 exempted)` — fully-audited up 33 from the 2026-09-18 entry's 193, the wildcard-covered family; **zero Fauna audits** for any of the 46. **The mechanical pre-scan, re-run this date (`dep-prescan --git-base <the landing's parent>`; all 46 fetched through `cargo vet inspect --mode local`; verdict lines consumed, excerpts unread): 2 RED, 4 FLAGGED, 40 scan-clean, exit 2.** RED `target-lexicon` 0.13.5 — `unicode-hazard`, ZWJ (U+200D) at `src/targets.rs:2119` and ZWNBSP (U+FEFF) at `src/targets.rs:2123`, plus `build-rs-proc` at `build.rs:10` and `:63`, build-time-executing. RED `wasmtime-internal-cranelift` 49.0.1 — `injection`, a reviewer-addressing imperative pattern (`disregard …`) at `src/translate/code_translator.rs:3635`, build-time-executing. FLAGGED `anyhow` 1.0.104 (`build-rs-proc` ×2), `cranelift-codegen` 0.136.1 (`build-log-emit` ×3, `build-rs-proc` ×1), `libc` 0.2.189 (`build-log-emit` ×6, `build-rs-proc` ×5), `wasmtime` 49.0.1 (`injection-soft`, an LLM mention at `build.rs:81`). Every `build-log-emit` line is the § *Dep-authored text in build and CI logs* case: those `cargo:warning` lines are untrusted data wherever they surface. Build-time-executing members, the scan's census (16 of 46 — highest priority within the shipped closure): `anyhow`, `cpp_demangle`, `cranelift-assembler-x64`, `cranelift-codegen`, `cranelift-isle`, `libc`, `pulley-macros`, `target-lexicon`, `wasmtime`, `wasmtime-internal-component-macro`, `wasmtime-internal-core`, `wasmtime-internal-cranelift`, `wasmtime-internal-fiber`, `wasmtime-internal-jit-debug`, `wasmtime-internal-unwinder`, `wasmtime-internal-versioned-export-macros`. A RED is the venue's to clear: by its position the `target-lexicon` finding is probably a test asserting that such a triple is refused, and the cranelift one a code comment — but the scanner cannot tell (§ *Reviewing untrusted source without being subverted*). The venue review that clears the two REDs, writes the dossier and certifies the shipped members is gated on the venue's shakedown. The publisher-trust question `cargo vet suggest` raises alongside is ruled in this section's target state (the *Publisher trust* bullet): exemptions stay, no `[[trusted]]` entries. `cargo deny check bans licenses sources` not re-run here (no dependency-version edit in this entry).

> **Update (2026-10-04) — the venue's first ruling-only read: the site's astro-7 bump's six npm pre-scan REDs are CLEARED, unanimously, by the standing quorum.** The bump report over the site's move to astro 7 (resolved astro 7.3.5; `dependency-bump-report.md`) marked six of 175 new or changed npm packages RED; the owner sent the set to the venue, and it was read under the new ruling (§ Reviewing untrusted source → *Ruling a pre-scan RED outside a review*): staged by `dep-npm-stage` from tarballs sha512-verified against the candidate `deno.lock`, scanned without excerpts (43 rulings over 2827 RED hits), ruled by Sonnet and Opus from one ruling prompt each, aggregated per package by `dep-aggregate --ruling-only` with every quote re-read from the scanned tree. **All six `RED-CLEARED`, 2/2, zero blocking and zero non-blocking findings from either reviewer**, each bound to its `deno.lock` integrity and tree hash: `undici` 8.11.2 (1 `injection`, a `.js` under `lib/web/fetch/`; tree `39201e8a…`), `fast-string-width` 3.0.2 (2 `unicode-hazard`, `readme.md`; `053a0dd7…`), `get-tsconfig` 5.0.0-beta.4 (2 `injection`, a `.d.mts` type declaration; `e547ca7c…`), `jsonc-parser` 3.3.1 (1 `injection`, `SECURITY.md`; `7a0ce0de…`), `vite` 8.3.1 (1 `unicode-hazard`, a byte-order mark mid-file in a `dist/node/chunks/` bundle; `3678cb9c…`), `zod` 4.6.5 (33 `unicode-hazard` on lines of two emoji test files and the Persian and Kannada locale files in source and both built forms, 3 `injection` on one line of its type definitions in three forms; `7060d176…`). Nothing certified, nothing in the review ledger: the six clearances are recorded in the pre-scan ledger (§ Reviewing untrusted source → *The pre-scan ledger*; written 2026-10-05 by `dep-aggregate --ruling-only --record` from the venue's own files), each bound to its `deno.lock` integrity, its tree hash and the lines cleared; this entry and the venue's run history are the narrative. **Consequence:** the astro-7 site path's pre-scan REDs are lifted; `undici` 8.11.2 was also one of the two REDs of the in-range site path, so that path keeps only `micromark-util-edit-map` 1.0.0 open. The FLAG-only trio the bump report named (`am-i-vibing`, `find-proc`, `process-ancestry`) was not read; it stays covered by the report's scanners. Venue: the second Claude account, containment re-checked and passed the same day; both reviewer sessions pushed within minutes of each other, each branch adding exactly its six findings files; webhooks 0 and Actions disabled before and after.

> **Update (2026-10-04, evening) — the wasmtime tree's venue review: eleven Fauna audits, both pre-scan REDs cleared, the 33 Bytecode Alliance crates left on their trusted-publisher audits by owner ruling.** The review the 2026-10-02 entry gated on the venue ran this day, at the versions the security-fix bump moved the tree to (`wasmtime` 49.0.2, `cranelift-*` 0.136.2, `wasm-tools` 0.258.3; the pre-scan re-run over all 46 from checksum-verified fetches: still 2 RED, 4 FLAGGED, 40 scan-clean, 3028 review sites, `wasmtime` alone 1615). **Scope — the owner's ruling, verbatim "let's trust bytecode alliance here combined with the 7-days rule", the smallest of three measured options:** the venue reviewed the twelve crates Fauna itself vouches for — the eleven exemption crates and the two RED crates, overlapping in `target-lexicon` — and the 33 other Bytecode Alliance crates stay on the `bytecode-alliance` import's wildcard audits bound to trusted publishing plus the 7 days of § *Minimum release age*, with no Fauna review. Run shape: `dep-crate-stage` laid the twelve trees sha256-verified against `Cargo.lock`; a dossier without excerpts (711 review sites, 485 of them `libc`'s); one review prompt per model; **two sessions, Sonnet and Opus — the standing quorum — at $1 and $5 (owner-reported), each under a quarter of an hour** (pushed 20:25Z and 20:31Z). **Verdicts (`dep-aggregate`, default minimums, `--record --apply`): CERTIFY 2/2 for all eleven** — `addr2line` 0.26.1, `anyhow` 1.0.104, `cpp_demangle` 0.5.1, `gimli` 0.33.0, `indexmap` 2.14.2, `libc` 0.2.189, `mach2` 0.6.0, `memfd` 0.6.6, `object` 0.40.0, `rustc-demangle` 0.1.28, `target-lexicon` 0.13.5 — every scan-listed site dispositioned with a source-verified quote, no blocking finding: eleven `safe-to-deploy` audits in `supply-chain/audits.toml` bound to the `Cargo.lock` checksums, the eleven exemptions removed (the `cpp_demangle` 0.3.5 base included), eleven `full` entries in the review ledger, and `cargo vet --locked` re-run: `Vetting Succeeded (236 fully audited, 11 partially audited, 949 exempted)`. **Both REDs cleared, unanimously, whole-line quotes matched against the scanned trees:** `target-lexicon` 0.13.5's two `unicode-hazard` lines (`src/targets.rs:2119`, `:2123`; tree `43caf5c8…`) inside its certify, and `wasmtime-internal-cranelift` 49.0.2's `injection` line (`src/translate/code_translator.rs:3635`) by ruling alone — `RED-CLEARED`, checksum `15e96d6c…`, tree `818cab94…`, nothing certified, the crate staying on its publisher's audit as ruled. Concerns recorded, none blocking: `libc`'s build script runs `emcc -dumpversion` from the build machine's `PATH` on every target (a version probe parsed to an integer; no `emcc` on Fauna's build machines), and `libc`'s deprecated `af_alg_iv::as_slice` reads past a zero-length array under the `extra_traits` feature, which Fauna does not enable. Both clearances are recorded in the pre-scan ledger (§ Reviewing untrusted source → *The pre-scan ledger*; written 2026-10-05 by `dep-aggregate --record`), each bound to its `Cargo.lock` checksum, its tree hash and the lines cleared. Containment held: webhooks 0, Actions disabled, private before and after each push; each reviewer branch one commit adding exactly its twelve findings files. **Round note:** under § *A round of bumps* (`dependency-bump-report.md`, ruled the same evening) this pass stood as the 2026-10-04 round's single venue pass by the owner's decision, nothing folded in, the round's other bumps holding no open RED.

> **Update (2026-10-05) — the pre-scan ledger is built and seeded over the whole lockfile: 68 RED crates, 2 of them ruled, 66 open — the `dep-prescan` job is RED on `main` until the venue rules them.** The CI pre-scan's baseline is now the committed scan ledger (§ Reviewing untrusted source → *The pre-scan ledger*), which closes the 2026-10-02 entry's gate-fidelity finding. **Seeded with the venue's eight 2026-10-04 clearances first,** each re-aggregated by `dep-aggregate --record` from the venue's own findings files and bound to its lockfile hash, its tree hash and the lines cleared: `target-lexicon` 0.13.5 and `wasmtime-internal-cranelift` 49.0.2 (the 2026-10-04 evening entry), and the six astro-7 npm packages (the 2026-10-04 entry). **Then the one-time seed scan over every registry crate in `Cargo.lock`** (1180 at the seed, each fetched through `cargo vet inspect --mode local`, checksum-verified; a detached run of 42 minutes; verdict lines consumed, no excerpt read; topped up the same day to the 1215 of a `ratatui` 0.30 lockfile): **1095 scan-clean, 52 FLAGGED, 68 RED, no fetch failure.** Two of the 68 are the wasmtime clearances. **The other 66 carry no disposition, 2,687 RED lines between them (86 `injection`, 2,601 `unicode-hazard`)**, and `unicode-width` 0.2.0 alone holds 2,506 of those lines, almost all of them in its emoji test data: `aho-corasick` 1.1.4, `askama_parser` 0.13.0 and 0.14.0, `atomic` 0.6.1, `aws-sdk-s3` 1.129.0, `aws-smithy-json` 0.62.5, `block-buffer` 0.12.1, `brotli` 7.0.0, `bytemuck` 1.25.0, `c2pa` 0.90.3, `chrono-tz` 0.8.6, `clap_builder` 4.6.0, `compact_str` 0.9.1, `critical-section` 1.2.0, `crossterm` 0.29.0, `curve25519-dalek-derive` 0.1.1, `futures` 0.3.32, `hpke-rs-crypto` 0.6.1, `hpke-rs-libcrux` 0.6.1, `hpke-rs-rust-crypto` 0.6.1, `ipnet` 2.12.0, `iroh` 1.0.0, `jni` 0.21.1, `js-sys` 0.3.103, `libdbus-sys` 0.2.7, `libsqlite3-sys` 0.30.1, `moka` 0.12.14, `nix` 0.29.0 and 0.31.2, `no-std-compat` 0.4.1, `noq-proto` 1.0.0, `notify` 8.2.0, `objc2-foundation` 0.3.2, `objc2-security` 0.3.2, `openmls_memory_storage` 0.5.0, `openmls_rust_crypto` 0.5.1, `openmls_traits` 0.5.0, `palette` 0.7.7, `pem-rfc7468` 0.7.0 and 1.0.0, `proptest` 1.10.0, `r-efi` 5.3.0 and 6.0.0, `rasn` 0.28.13, `ratatui` 0.30.2, `ratatui-core` 0.1.2, `raw-cpuid` 11.6.0, `secp256k1-sys` 0.10.1, `seize` 0.5.1, `signal-hook` 0.3.18, `strsim` 0.11.1, `tantivy` 0.22.1, `target-lexicon` 0.12.16, `termwiz` 0.23.3, `textwrap` 0.16.2, `tiny_http` 0.12.0, `unicode-truncate` 2.0.1, `unicode-width` 0.2.0, `url` 2.5.8, `widestring` 1.2.1, `zbus` 5.14.0, `zbus_macros` 5.14.0, `zbus_names` 4.3.1, `zerocopy` 0.8.47, `zstd-sys` 2.0.16+zstd.1.5.7, `zvariant` 5.10.0. These crates were already in the tree, scanned by no gate until now; each RED is a pattern hit the scanner cannot tell from a hostile line, and none is judged here. **Consequence:** from this landing the `dep-prescan` job fails on every run, the daily one included, until the venue has ruled all 66, which is the gate doing what it should and is not to be silenced. The ruling pass is captured, and so is the open question of how thousands of lines of Unicode test data get a ruling that stays meaningful. npm packages enter the ledger only through a ruling: no CI job pre-scans the tracked `deno.lock` trees against it yet.

> **Update (2026-10-05, afternoon) — the pre-scan ledger's npm leg: both tracked `deno.lock` trees are pre-scanned in CI and seeded; 38 RED npm packages, 6 of them ruled, 32 open.** The `dep-prescan` job now scans every npm package of the web app's and the site's `deno.lock` against the ledger as it does `Cargo.lock`'s crates (§ Reviewing untrusted source → *The pre-scan ledger*), which closes the gap the morning entry named. **The one-time seed over both locks** (395 packages, each tarball fetched from the npm registry and verified against its `deno.lock` integrity before extraction; a detached run of four minutes; verdict lines consumed, no excerpt read): **354 scan-clean, 3 FLAGGED, 38 RED, no fetch failure.** The six astro-7 clearances (the 2026-10-04 entry) re-scanned under rule set `2` and their rulings carried over unchanged. **The other 32 carry no disposition, 72 RED lines between them (52 `injection`, 20 `unicode-hazard`)**, most of them one line in a small `unified`/`micromark` utility package; `rollup` 4.63.5 holds 12 and `@sveltejs/kit` 2.70.3 holds 8. Like the crates of the morning entry, these were already in the tree and are only now scanned. **Consequence:** the `dep-prescan` job fails on them too until the venue has ruled them; the ruling pass is captured.

> **Update (2026-10-05, evening) — prescan-seed: the venue ruled the whole-lockfile seed's 61 open RED crates; all 61 RED-CLEARED.** The ruling-only pass over the 61 crates the morning seed left without a disposition (163 rulings, 82 `injection`, 81 `unicode-hazard`, over 197 RED hits) ran in the venue as one run, `prescan-seed-2026-10-05`. The standing quorum read it: Sonnet and Opus, each fired by a routine from the dev machine. Opus fired only after the owner's new Opus routine passed all three launch-path checks. Each reviewer's branch added exactly its 61 findings files and nothing else, checked by name only. `dep-aggregate --ruling-only --record` then ran once per crate against the run's binding `Cargo.lock`, and **every crate came back `RED-CLEARED`**: both reviewers ruled every RED key `false-positive`, and every quote equalled the line in the staged tree, byte for byte, invisible characters included. The aggregator wrote each disposition into `supply-chain/prescan-ledger.toml`, bound to the crate's `Cargo.lock` checksum and tree hash. No `RED-STANDS`, no warning. **Time is a smell, and it was checked:** the Opus session pushed about three minutes after its fire, the Sonnet session about one, both fast for 163 rulings, so the aggregator's quote verification against the staged bytes is what this clearance rests on. **Consequence:** with the npm entry below, every RED in `Cargo.lock` carries a disposition (63 crates, these 61 plus the wasmtime tree's two), and `dep-prescan` exits 0 on the ledger.

> **Update (2026-10-05, evening) — prescan-npm-seed: the venue ruled the npm leg's 32 open RED packages; all 32 RED-CLEARED.** The ruling-only pass over the 32 npm packages the afternoon npm seed left without a disposition (72 rulings, 52 `injection`, 20 `unicode-hazard`, over 118 RED hits) ran as its own run, `prescan-npm-seed-2026-10-05`. Eight of the packages are pinned by the web app's `deno.lock` and 24 by the site's; each tarball was integrity-verified against its own lock before staging. The context file quoted `build-system.md` § The Deno build sandbox as it stands. The run had the same quorum, launch path and names-only branch check as the crate run above. `dep-aggregate --ruling-only --record` ran once per package against the `deno.lock` that pins it, and **every package came back `RED-CLEARED`**, written into the pre-scan ledger bound to its `deno.lock` integrity and tree hash. No `RED-STANDS`, no warning. The same timing caveat applies: Sonnet pushed about 90 s after its fire, Opus about two minutes, and the clearance rests on the aggregator's byte-exact quote check. **Consequence:** every RED npm package in both tracked locks carries a disposition (38: these 32 plus the six astro-7 rulings of 2026-10-04). the pre-scan check over the ledger and both ecosystems exits 0, with no RED without a disposition across 1215 crates and 395 npm packages, so the CI `dep-prescan` job can go green for the first time since the ledger landed.

> **Update (2026-10-05, night) — the 2026-07-30c lever was pulled upstream: openmls 0.9.0, `openmls_rust_crypto` 0.6 and `hpke-rs` 0.7 (all released by 2026-08-25) are in, with `libcrux-ml-kem` 0.0.10, and the libcrux advisories are fixed rather than excepted.** Both sides of the workspace now agree on `hax-lib` 0.3.7, so the empty intersection recorded above no longer holds; the bump moved the openmls family, the libcrux wave, `fauna-pq-kem`'s exact pin and, forced through `x25519-dalek` 3 → `curve25519-dalek` 5.0.0, `iroh-base` 1.0 → 1.2. The owner approved its consequence table (52 changed, 1 RED: `crabgrind`, lock-only behind `cfg(valgrind_ct_test)`). `cargo audit --deny warnings` is green with RUSTSEC-2026-0212, 0207, 0208, 0124, 0211, 0209 and 0210 deleted from `.cargo/audit.toml` (0173 stays: `hax-lib-macros` 0.3.7 still uses `proc-macro-error2`), and GHSA-hc3c-63hc-2r9f is fixed by `libcrux-chacha20poly1305` 0.0.9. `pinned_vectors_survive_libcrux_bumps` passes unchanged on 0.0.10, so derived X-Wing bytes are stable across the move as this entry's predecessor prepared for. Two openMLS 0.9 shape changes reached our code: an own-leaf message is no longer an error but `ProcessedMessageContent::OwnPrivateMessage` (and an own-leaf commit reaching staging is `StageCommitError::OwnCommitMismatch`), both mapped to the existing `MlsError::OwnLeafCommit` resync signal; and the message-secrets store gained a wall-clock `added_at`, which the cross-device replica merge now ignores when comparing values (`devices.md` § A provider CAS conflict is reported, not repaired).

> **Update (2026-10-05, night) — bump round 1's venue pass: `typetag` 0.2.23 and `console` 0.16.6, both ruling-only, both RED-CLEARED.** The round's single closing venue run, `round-1`, held exactly the two pre-scan REDs the owner's round-1 decision sent there (`supply-chain/rounds/round-1.md`): `typetag` 0.2.23 (new in the workspace `Cargo.lock`, forced by tantivy 0.26.2; build script; `injection` ×2, `README.md:55` and `src/lib.rs:54`) and `console` 0.16.6 (new in the vendored openMLS fork's own lock, compiled only by the fork's regression tests; `unicode-hazard` ×1, `src/utils.rs:1312`). Each `.crate` was sha256-verified against the round's candidate lock that pins it before staging, and each item was aggregated against that lock as its binding: the workspace candidate for `typetag`, the fork's candidate for `console`. The standing quorum read it, Sonnet and Opus, each fired by its routine from the dev machine, one session per model over both items. Each reviewer branch added exactly its two findings files, checked by name only. `dep-aggregate --ruling-only --record` returned **`RED-CLEARED` for both**: every RED key was ruled `false-positive` by both reviewers, every quote byte-equal to the staged line, and no non-blocking finding. The dispositions are in `supply-chain/prescan-ledger.toml`, bound to each crate's checksum and tree hash. The same timing caveat applies: the staging was pushed at 19:12:33Z and both fired within the minute; Sonnet pushed at 19:13:17Z and Opus at 19:14:57Z, for three rulings. **Consequence:** tantivy 0.26.2 (round 1 item A) may be compiled, since `typetag`'s build script is no longer an unruled RED, and the fork's test build (item E) may run with `console` in its graph. The round-1 venue pass is complete.

> **Update (2026-10-06, morning) — bump round 3's venue pass: the nine pre-scan REDs that surfaced after round 1's pass, all ruling-only, all RED-CLEARED.** Round 1 landed without the pre-scan ledger update. Run afterwards, it scanned 137 registry versions and found nine RED crates that round 1's pass had never seen, all `injection`: `tantivy` 0.26.2 (`src/core/mod.rs:20`), `aws-smithy-json` 0.63.1 (`src/deserialize/token.rs:178`), `crabgrind` 0.2.6 (two lines under `doc/valgrind/`), and one `CHANGELOG.md` line each in `hpke-rs-crypto`, `hpke-rs-libcrux` and `hpke-rs-rust-crypto` 0.7.0 and in `openmls_memory_storage`, `openmls_rust_crypto` and `openmls_traits` 0.6.0. The CI `dep-prescan` job failed closed on them. The owner ruled that they wait for the next round's single pass rather than take a second pass for round 1 (`dependency-bump-report.md` § A round of bumps). Round 3 added no RED of its own, so its closing run, `round-3`, held exactly these nine. Each `.crate` was sha256-verified against the workspace `Cargo.lock` before staging, and that lock is the binding for all nine. Sonnet and Opus were routine-fired from the dev machine, one session per model, each branch checked by name only. `dep-aggregate --ruling-only --record` returned **`RED-CLEARED` for all nine**: all ten RED keys were ruled `false-positive` by both reviewers with byte-exact quotes, and there were no non-blocking findings. The dispositions are in `supply-chain/prescan-ledger.toml`, bound to each crate's checksum and tree hash. **Consequence:** the pre-scan check over the ledger exits 0 across 1234 crates and 395 npm packages, so the CI `dep-prescan` job can go green again.
