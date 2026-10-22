# Gated e2e seeds — the inner runtime switches that carry real suite posture — target state

Owns: e2e-gated-seeds
Status: ratified — split verbatim out of `e2e-automation-surface-gating.md` on 2026-10-14; seven seeds of the one gated-seed shape are ruled (the e2e trust seed, the source-IMAP trust seed, the drafts autosave-window seam, the nest-leg trust seed, the Microsoft authority seed, the ACME directory seed, and the preset-host redirect seed — refused 2026-10-13, reopened and ruled 2026-10-17) and one candidate stays refused, each seed's build state in its own section and in § Implementation status today
Authority: **the gated seeds** — the inner runtime switches that carry real suite posture rather than merely turning the test agent on: each seed's variable, the one door it acts on, why it exists (a witness otherwise impossible), how each app receives it and its opt-out, and the shape they share — plus the seeds considered under that shape and refused (§ Seeds ruled out). Defers convention 15 itself — that every such seam is compiled out of release artifacts, the per-language gate and the artifact-level verification method — and the convention's build-out record to [`e2e-automation-surface-gating.md`](e2e-automation-surface-gating.md); the numbering of the e2e conventions to [`e2e-conventions.md`](e2e-conventions.md); the trust model a trust seed sits beside to [`transport-trust.md`](transport-trust.md).

Last verified: 2026-10-17 | Sources: `tests/e2e-unified/conftest.py`, `libs/fauna-anon-client/src/trust.rs`, `libs/fauna-mail/src/imap_client/native.rs`

Split verbatim out of [`e2e-automation-surface-gating.md`](e2e-automation-surface-gating.md) on
2026-10-14. That doc had reached 203,978 B, about eleven days from the 262,144 B whole-file read
ceiling, and its seed sections were the bulk of its growth in the week before the split (the
nest-leg, Microsoft authority and ACME directory seeds and § Seeds ruled out were all written
that week). Its Authority line already listed them as an addition to convention 15 — the
convention is the gate that compiles every seed out; the seeds are what is gated — so the family
moved whole, with its three status entries and its section-map rows.

**Citations.** Every section keeps its original `##` heading, so an
`e2e-automation-surface-gating.md § <seed>` citation resolves here by swapping the filename alone.
Unqualified § citations in the moved text that name a section not found here — § The convention,
and § Implementation status today's release-profile warning (the convention's record) — resolve
in [`e2e-automation-surface-gating.md`](e2e-automation-surface-gating.md); this doc's own
§ Implementation status today holds only the three seed entries that moved with it.

## Section map

| Section | What it holds |
|---|---|
| § The e2e trust seed | The first worked example of an inner runtime switch that carries real test posture: `FAUNA_E2E_TRUST_NEST_IDENTITY`, why it is default-on and keyed by nest, how each app receives it (no declared absence remains), and its opt-out. |
| § The source-IMAP trust seed | The second, and the one that generalizes the shape: `FAUNA_E2E_IMAP_EXTRA_CA_PEM` on `NativeImapConnector::new`, why it adds an anchor rather than disabling verification, and the artifact-level measurement of its absence from a shipped build. |
| § The drafts autosave-window seam | The third, and the first that makes a *timing* fact provable rather than supplying a fact a rig cannot produce: `FAUNA_E2E_DRAFTS_AUTOSAVE_DEBOUNCE_MS` on the one autosave-debounce accessor, for the iOS leave-flush witness. |
| § The nest-leg trust seed | The fourth, and the one that sits on the verifier guarding the login's pin: `FAUNA_E2E_NEST_EXTRA_CA_PEM` on `webpki_verifier()`, ruled before any code on five terms — compiled out, add-only, one door and one variable per leg, never default-on, a witness that cannot pass on the self-signed branch — with why the nest needs no seam and why pebble is not the alternative. |
| § The Microsoft authority seed | The fifth, and the first that redirects a far end rather than trusting one: `FAUNA_E2E_MICROSOFT_AUTHORITY` on the one door that builds the device-code flow's endpoints, so the Outlook sign-in's witnesses never reach Microsoft. |
| § The ACME directory seed | The sixth, and the first that redirects a far end and trusts it in one seed: `FAUNA_E2E_ACME_*` on the one shared builder of the native admin-dns machine, so a walk of a manual or managed certificate issuance opens a real order against the harness's pebble CA while the CA stays a constant. |
| § The preset-host redirect seed | The seventh, and the first that moves where a provider preset's IMAP connection is dialled: `FAUNA_E2E_IMAP_HOST_MAP` on `NativeImapConnector::new` beside the trust seed, a loopback-only host-and-port map that keeps the TLS name it verifies, so the Outlook import's XOAUTH2 leg reaches a harness IMAP server — refused 2026-10-13 for the provider hint, reopened 2026-10-17 by the witness that refusal had excluded. |
| § Seeds ruled out | The seeds considered under the shape and refused, so the next session that meets the same gap reads the ruling: the preset-host redirect for the mail-import wizard's provider hint (2026-10-13; reopened 2026-10-17 for the Outlook import's IMAP leg, one host only — the grounds stand for the rest) and the seed-selected transport for the propagation probe (2026-10-14). |
| § Implementation status today | The seed entries of the convention's build-out record, moved with them: the e2e trust seed keyed by nest, the nest-leg trust seed built, the ACME directory seed's name-server route built, and the preset-host redirect seed built. |

## The e2e trust seed — `FAUNA_E2E_TRUST_NEST_IDENTITY` (ratified 2026-08-17)

The convention's "inner convenience switch" clause is usually about turning the agent on. This
one switch is different in kind and is written down here because it decides what the whole suite
**exercises**: it is the e2e's stand-in for a trust fact a test rig structurally cannot produce.

**The gap it closes.** A client's R14 (account-data-plane.md § The ratified decisions) escrow-holder trust is its TLS **channel-binding TOFU pin**
for the nest — the identity it pinned, never the nest's own claim about itself
([`account-data-plane.md`](account-data-plane.md) § the generation plane). An e2e nest is
plaintext, so no pin can ever graduate, so `trust::trusted_escrow_holders` returned an **empty
set in every app-e2e run ever made**: no generation tip resolved, every fleet-only sealed write
refused, `device_endpoints: Unmintable` in every pump report. The plane was dormant fleet-wide
and nothing went red, because honest refusal *is* the fail-safe design — the failure mode a
"needs a human" claim would never have surfaced either. Found 2026-08-17 while building the
first app-level consumer (`test_custody_ceremony_journey.py`).

**The seed.** `fauna_anon_client::trust::trusted_escrow_holders(nest_url)` — the ONE door every
escrow-trust consumer reads, so the rule lives in one place — additionally accepts
`FAUNA_E2E_TRUST_NEST_IDENTITY` under
`#[cfg(any(test, debug_assertions, feature = "test-helpers"))]`, and so does its sibling
`trust::pinned_nest_custodian_identity(nest_url)`: the identity a NEST-form custody accept
binds, where the seed stands in only when no pin exists. Textbook convention 15: the
compile-time gate is the boundary, this is the switch inside it. Production semantics are
untouched — a release build has no such arm, and the seed never writes the TOFU pin store, so
the nest-identity-changed / re-pin surfaces behave identically with it set.

**Keyed by nest, like the pin it stands in for (2026-09-15).** A TOFU pin belongs to one nest
authority (`authority_of(nest_url)`), so the seed does too: `<nest url>=<nest_id>` entries
(64-hex, nest.info's `nest_id`) joined by `,`. Each door answers with the entry whose key names
the same authority as the `nest_url` it is asked about — a later entry for one authority
superseding an earlier one — and with nothing for any other nest; a bare identity names no nest
and is honoured for none. Until 2026-09-15 the seed was one bare identity, trusted for EVERY
nest, so an app pointed at a dedicated nest trusted the launch nest's key there: each
generation mint ran to a post-deposit refusal ("the escrow receipt is signed by a holder this
account does not trust"), and a custody accept on that nest would have bound the launch nest's
identity — an account trusting nest A's receipts while talking to nest B, which no production
client can reach. Both sides of the contract are pinned: the doors by
`libs/fauna-anon-client/tests/e2e_trust_seed.rs`, the harness's writer by
`tests/e2e-unified/tests/test_r14_trust_seed_default.py`. A build from before the keyed grammar
reads the variable as one bare identity instead, so the version-skew grid seeds a pinned previous
release that old in the bare form, chosen by the pinned commit's ancestry
(`tests/e2e-unified/helpers/prev_build.py::reads_keyed_trust_seed`); a build of the working tree
is always seeded keyed.

**Default-on, at the launch config.** `conftest._apply_r14_trust_env` puts the seed into
`config["environment"]` for every app launch, not into the harness's own `os.environ`: the
account runtime captures its trust set at **assembly**, so it must precede app start, and the
per-launch dict scopes it to the app (plus the `fauna-sync-agent` it spawns as a child) instead
of leaking into every subprocess the harness makes. Both production assembly sites read the one
door — `apps/fauna-tui/src/session.rs` and `bins/fauna-sync-agent/src/account_host.rs`, the
account-runtime host for the other six apps — so the seed reaches an app whichever way it hosts
its runtime. A launch that will talk to more than one nest carries one entry per nest
(`_apply_r14_trust_env` called once per nest into the same environment). The environment is
fixed at launch, so it can name only the nests that exist by then: an app that will use a nest
its launch did not name is launched — or relaunched — seeded for that nest before it is pointed
there, never re-pointed into a nest it holds no seed for, where its trust set is empty and the
plane is dormant again. "Every app launch" includes the ones a test makes itself — a second seat,
a launch/relaunch harness, a previous release's binary, a bare process — which build their
environment through the same writer (`conftest._seeded_environment`, or `_trust_seeder` handed to
a launcher outside conftest); `tests/e2e-unified/tests/test_r14_trust_seed_self_launch.py` scans
every driver launch under `tests/` and fails on one that carries no seed and declares no reason.

**No declared absence remains. `web` was the last one** — it runs in wasm, where `std::env::var`
has nothing to read, until the SPA began hosting the account runtime in-tab (the web-hosts-the-account-runtime
change): the harness's web launch writes the same keyed entries into the origin's `localStorage`
(`drivers/web.py::launch`), and the wasm account runtime reads them under `test-helpers` in the one
grammar `fauna_client_core::nest_trust::seeded_nest_identity` shares with native.

**android was a declared absence for the same structural reason — Intent launch, not exec, so
only keys `BridgeHttpServer.kt::launchApp()` explicitly forwards ever arrive — until a
2026-08-22 fix closed the gap.** `launchApp()` now forwards this one key
as an intent extra, and `MainActivity.onCreate` re-exports it into the process environment via
`android.system.Os.setenv` (`BuildConfig.DEBUG`-gated, before the launch-persistence chain can
trigger an auto-login) — `Os.setenv` sets the process's real environment, which the in-process
`fauna_client_account_runtime::build_params` call (`libs/fauna-ffi/src/account_runtime.rs`,
android's own account-runtime host since the W3 (account-data-plane.md § Workstreams) row 44 in-process shape) then reads exactly as
any other app's `std::env::var` call would.

**Skipped for the `live` nest mode (fixed 2026-08-27).** A live box
(`testing.md` § Default app and nest mode) graduates a real TLS pin, so the seed is inapplicable
by design — but the guard that was supposed to skip it (`if not port: return`, reading the
pre-nest-mode-axis assumption that a live handle carries no `port`) went stale on 2026-08-02
when `_LiveProvider` started declaring one (`port=443`, a real capability under the
capability-honesty contract), silently pointed `ws_api.nest_info` at `127.0.0.1:443` instead of
the box, and `ConnectionRefusedError`'d every app-fixture test in every live-mode run from
2026-08-17 (this switch's own landing) onward — undetected because no live sweep ran in that
window. `_apply_r14_trust_env` now checks `nest_mode.run_mode().is_live` directly instead of
inferring the mode from `port`'s truthiness.

**Opt-out: the `no_r14_trust` marker**, for a test asserting the fail-safe posture itself
(refusal with no trusted holder, no durable local row). It is session-wide, like
`real_conversations`, because the app drivers are session-scoped (`_driver_cache`) — a per-test
opt-out cannot un-seed an app that is already running — so such tests run in their own
invocation.

## The source-IMAP trust seed — `FAUNA_E2E_IMAP_EXTRA_CA_PEM` (ratified 2026-08-28)

The second switch of the same kind, and it is written down beside the first because the shape
generalizes: **where a test rig structurally cannot produce a trust fact, the seed goes on the
ONE door every consumer reads, under the compile-time gate, and nowhere else.**

**The gap it closes.** [`mailbox-migration.md`](../behavior/mailbox-migration.md) § The two TLS
modes ratifies that a source IMAP session has **no plaintext mode** — the user's foreign-mailbox
password crosses it — so an e2e walk of the mail-import wizard has to point the app at a TLS
server it can verify, and no harness can mint a publicly-chained certificate for `localhost`.
Nor can the harness reach in and configure trust: the app's connector is built by
`NativeImapConnector::new`, several layers below anything the driver touches. Without the seed
the wizard's tier_3 walk is not merely hard, it is unreachable — which is why
`tests/e2e-unified/tests/test_mail_import.py` did not exist while every other layer of the
wizard was built and green.

**The seed.** `fauna_mail::imap_client::native::NativeImapConnector::new` — the ONE door both
native construction sites build their source trust store through (`rpc_glue`'s
`RpcImportSourceNest` for linux/tui, `fauna-ffi`'s `FfiMailImportClient` for
apple/android/windows) — additionally trusts the PEM in `FAUNA_E2E_IMAP_EXTRA_CA_PEM`, under
`#[cfg(any(test, debug_assertions, feature = "test-helpers"))]`, with a same-signature no-op twin
under the negation so the shipped path is explicit rather than implied. Putting it on `new`
rather than at each call site is what stops a consumer from being the one that forgot it
(priority #2); it is also why no app-side plumbing changed to make the seed reach six apps.
The one non-app reader is the e2e harness library: `fauna-ffi`'s `e2e-harness` flavor (built
`--release` by `just e2e-ffi`, shipped in nothing) forwards `fauna-mail/test-helpers`, so the
app-free import its `fauna_harness_mail_import` export runs — the `rpc_glue` machine again, for
a `[nest]` witness of a client claim — reaches the same door with the same CA.

**It ADDS an anchor; it is not an accept-any switch.** The seed routes through the production
`with_extra_root_pem`, so an unverifiable source server still fails the handshake with the seed
set. That asymmetry is the point: `FAUNA_INSECURE_TLS` — the fleet's retired accept-any path —
is why "trust this specific CA" and "trust anything" are kept apart, and a gate on an accept-any
switch would still be an accept-any switch in every debug build a developer runs. A malformed
value **panics** rather than being ignored, because a silently-dropped seed degrades into a
handshake failure indistinguishable from a wizard bug.

**Verified at the artifact, not the source** (the convention's own method): built with
`-C debug-assertions=on` the `fauna-mail` rlib carries 3 `strings` hits for the variable name;
with `-C debug-assertions=off`, **0**. The negative alone would be vacuous, so both halves are
measured. Source-level and manifest-level enforcement is
`tests/e2e-unified/tests/test_mail_import_tls_seam_gating.py` (tier_1, five assertions: both
items gated, the env read confined to the gated reader, the twin present, the additive-only
property, the Rust/Python spellings pinned to each other, and no shipping dep line naming
`test-helpers` on the three crates in the forward chain). The seed's own behaviour is
`libs/fauna-mail/tests/imap_client_trust_seed.rs` — its own test *target*, and therefore its own
process, because a sibling test setting the variable would silently defeat
`imap_client_native.rs`'s "an unverifiable source cert is refused before any password is sent".

**No declared absences, and none possible.** Unlike the R14 seed, this one is native-only by
construction: the wasm transport is a different impl of the same seam and terminates TLS inside
the browser client, so there is nothing here for `web` to be absent from.

## The drafts autosave-window seam — `FAUNA_E2E_DRAFTS_AUTOSAVE_DEBOUNCE_MS` (ratified 2026-09-21)

The third switch of the same kind, and the first whose purpose is to make a *timing* fact provable
rather than to supply a fact a rig cannot produce. The generalized shape holds unchanged: **the seed
goes on the ONE door every consumer reads, under the compile-time gate, and nowhere else.**

**The gap it closes.** [`../behavior/reserved-folders.md`](../behavior/reserved-folders.md) § The
leave-flush promise ratifies that leaving an app never loses what the debounced autosave has not yet
caught. A witness for that promise has to show the draft reached the nest *through the leave door*
and not through the ordinary ~1.5 s debounce landing on its own — and on six of the seven apps it
gets that for free, because the leave door ends the process: the debounce provably cannot have
fired. **iOS is the exception, and it is structural**: its door is the background transition and the
app keeps running afterwards, so there is no process death to assert and a poll of the nest cannot
tell the two paths apart. Measured before the seam existed, a 6 s poll after `enter_background()`
passed, failed, passed and failed across four runs — and *the passes were the dangerous half*, since
each could have been the debounce. That is a false green, which is worse than an honestly short
cell, so the leg was refused rather than landed.

**The seam.** `fauna_client_drafts::autosave_debounce()` — the one accessor every shell's timer now
reads, directly on linux and tui and through `fauna-ffi`'s `autosave_debounce_ms` /
`fauna-wasm`'s `autosaveDebounceMs` elsewhere — additionally honours
`FAUNA_E2E_DRAFTS_AUTOSAVE_DEBOUNCE_MS`, under
`#[cfg(any(test, debug_assertions, feature = "test-helpers"))]`, with a same-signature no-op twin
under the negation. `AUTOSAVE_DEBOUNCE` stays the production answer and the documented window.
Putting it on the accessor rather than at each timer is what makes one marker reach every app with
no app-side plumbing (priority #2) — the same reason the IMAP seed sits on
`NativeImapConnector::new`. A malformed value **panics** rather than being ignored, because a
silently-dropped window degrades into a witness that is racing the debounce again while reporting
that it is not.

**It LENGTHENS; it is not a speed-up knob.** The e2e leg sets the window past the whole test, which
restores exactly the property process death gives the other six: any save at all is necessarily the
flush. Shortening the window to race the debounce is the wall-clock dependence convention 14
forbids — a bound under 1.5 s only trades the false green for a load-dependent false red. The
harness surface is `@pytest.mark.drafts_autosave_window_ms(ms)`, session-wide because the app
drivers are session-scoped; co-selecting it with a `drafts_production_window` test (the
restart-round-trip family, which needs the debounce itself to land a draft) is **refused at
collection** rather than left to turn those tests red, since a red there would be recorded against
the app in the feature ledger as though the product had regressed.

**Its standing witness is `tests/e2e-unified/tests/test_drafts_autosave_window_seam_gating.py`**
(tier_1, no build), which carries one half this family's other witnesses do not need: **every app
timer must read the ACCESSOR, not the constant.** A timer that reverts to `AUTOSAVE_DEBOUNCE` opts
its app out of the seam with nothing to see — no build breaks, no test fails, no `strings` sweep
changes — and the app's own leave-flush witness would then quietly race the debounce again while
its docstring still said it could not. That is convention 11's third lesson one layer down (a flag
that does not reach the app makes a sweep measure nothing while reporting clean), and it is the
half the web leg's 2026-09-02 decay shows a verification *event* never catches.

**One declared absence: `web`.** `wasm32-unknown-unknown` has no process environment, so
`std::env::var` there is always `Err(NotPresent)` and web reads the constant in every build. Web's
own 150 ms `__faunaTestAgent` override is its substitute and it shortens where this lengthens, so
`test_web_leave_flush.py` still races the debounce rather than ruling it out — narrower than iOS's
old gap (the dispatch follows the typing within milliseconds) but the same *kind* of claim, and it
is the one leg of this promise not yet proven the way the other six are.

## The nest-leg trust seed — `FAUNA_E2E_NEST_EXTRA_CA_PEM` (ruled 2026-10-09)

The fourth switch of the same kind, and the one that sits on the verifier guarding the login's pin
— which is why it was RULED before any code existed, as a security boundary, rather than built the
way the source-IMAP seed was and written up after. The generalized shape holds unchanged: **the
seed goes on the ONE door every consumer reads, under the compile-time gate, and nowhere else.**

**The gap it closes.** [`transport-trust.md`](transport-trust.md) § The login's pin, rule (1): a binding already
in hand is never waived by WebPKI — an app that holds a nest's identity pin keeps checking it when
the nest presents a publicly trusted certificate, so another server at the same address cannot pass
as the nest ([`connect-and-sign-in`](../../features/connect-and-sign-in.md) outcome 14). The rule
is built in `fauna_anon_client::trust` (`webpki_waives_binding`, `record_login_identity`) and pinned
by that crate's unit tests, and no journey can reach it: every e2e TLS fixture serves the
self-signed floor, on which `webpki_valid` is false and the binding was required before the rule
existed — a journey on that branch is green against the pre-rule code and witnesses nothing.
Reaching the arm needs a nest the APP judges publicly trusted, and the app's nest legs trust exactly
the baked Mozilla roots (`fauna_tls_bootstrap::webpki_root_store`, consulted through
`fauna_ws_substrate::tls_verify::webpki_verifier`): no harness can mint a publicly-chained
certificate for `127.0.0.1` or a LAN address, and no rig ever will — the production case the rule
guards (a § B-IP box, a public-CA domain) exists only on a box a public CA can validate. So the
trust fact is one the rig structurally cannot produce, which is this shape's precondition.

**The ruling: the seam MAY exist, on five terms** (fable, 2026-10-09).
A seam on this verifier reads as a widening of the one check that stands between a bearer and an
impostor, so each term is written against that reading:

1. **Compiled out of every release artifact — the boundary, as always.** The anchor arm is
   `#[cfg(any(test, debug_assertions, feature = "test-helpers"))]` with the same-signature no-op
   twin under the negation, as the source-IMAP seed is. The forward chain already exists:
   `fauna-ws-substrate/test-helpers` is forwarded by `fauna-ffi`'s test flavor and by nothing that
   ships; linux and tui reach the arm through `debug_assertions`. Measured at the artifact, both
   halves (the variable's name present in a debug `fauna-ws-substrate` rlib, absent from a release
   one), and pinned by a tier_1 witness of the same four halves as
   `test_mail_import_tls_seam_gating.py`.
2. **It ADDS an anchor to the root set and changes no policy.** The seam extends the
   `RootCertStore` the one verifier is built from and touches nothing else: not `NoPinPolicy`, not
   the pin compare, not the waiver. Everything `webpki_valid` unlocks under the seam — the
   `RequireWebPki` bearer dial with no pin, the first-contact waiver with no root, no pin and no
   binding, the stale-SPKI clearing — is exactly what a certificate chaining to any trusted root
   unlocks in production; the seam changes which roots are trusted, never what trust permits. A
   self-signed nest is refused under the seam wherever it is refused without it. A malformed value
   panics (the harness is the only writer; a silently dropped anchor reads as a trust bug).
3. **One door, one variable per leg.** The door is
   `fauna_ws_substrate::tls_verify::webpki_verifier()` — the verifier all three client configs are
   built on (`capturing_client_config`, the pre-identity WS dial; `spki_pinned_client_config`, the
   bearer dial; `dynamic_pinned_client_config`, the reqwest byte plane), so every app nest leg reads
   the seed through one function and no leg can be the one that forgot it (priority #2). Not
   `fauna_tls_bootstrap::webpki_root_store()`: that store also builds the source-IMAP connector,
   which has its own seed and its own refusal witness, and the nest's mail-deliverability STARTTLS
   probe — one variable per leg keeps each leg's posture explicit and each artifact measurement
   attributable to one seam. The variable is `FAUNA_E2E_NEST_EXTRA_CA_PEM`, PEM text like the
   source-IMAP seed (the two client-side seams read the same launch environment and spell their
   value the same way; the nest's own `FAUNA_TEST_*_EXTRA_CA_PEM` hooks take paths and are the
   nest's family, not this one). A declared consequence, not a goal: the nest's federation dial
   shares this verifier, so a debug or `test-hooks` nest honours the variable too — the posture its
   three outbound `FAUNA_TEST_*_EXTRA_CA_PEM` hooks already carry — and the release image, built
   with neither, carries no arm.
4. **Never default-on.** The R14 trust seed is default-on because it stands in for a fact every e2e
   run needs; this seed changes which branch of the trust posture the suite walks. Set for every
   run, every nest would read as publicly trusted and the self-signed-floor branch — outcome 4's
   witnesses, every TOFU journey, the `spki_pinned_nest` pin-branch proof — would stop being
   exercised. So the harness sets it per launch, for the one app pointed at the one nest that serves
   a harness-CA leaf, and nowhere else; the writer is the journey's own, never conftest's session
   start.
5. **The witness cannot pass on the self-signed branch.** A journey built on the seam asserts,
   before the app launches, that the served chain verifies against the harness CA (a TLS handshake
   from the harness with that CA as its only trust anchor), and is run red against the pre-rule
   code first — the `binding.is_none()` clause of `webpki_waives_binding` removed locally, which
   makes a WebPKI login with no prior pin take the waiver and record nothing, as every login did
   before 2026-10-05. The half that goes red is the POSITIVE one: the login leaves the pin. (The
   pin-in-hand clause predates the rule, so a seeded disagreeing pin warned under the old code too
   and alone distinguishes nothing.) A self-signed nest shows no difference between the two builds,
   because its binding was required all along — that control is what proves the journey walks the
   WebPKI branch.

**The nest needs no seam.** The nest serves whatever `fullchain.pem` + `privkey.pem` sit in its
ACME directory: `self_signed_cert::ensure_floor_present` is write-if-absent,
`listener_tls_from_floor` loads the pair on disk, `acme::cert_watcher_task` hot-reloads a pair
written after boot within ~2 s, and the login binding signs the live resolver's SPKI
(`auth_handlers.rs`, `served_cert_spki.current_spki_sha256()`). That is the production path an ACME
finish and a client-issued LAN cert install both take
([`nest/tls-certificates.md`](nest/tls-certificates.md) § B, § Where logic lives), so the harness
mints a leaf for the nest's dial address with `tests/e2e-unified/helpers/ephemeral_ca.py` (it
issues IP SANs) and writes the pair into the nest's `<tmp_dir>/acme` — where the e2e config loader
points `[acme].dir` — exactly as an ACME client would: no test hook, no nest flag.

**The dial is the LAN address, not loopback.** The Rust verifier consults no loopback carve-out,
but the FFI apps' residual-HTTP cert callbacks do (`is_loopback_authority`: trust is
`webPkiValid || isLoopback || spki == pin`), so a loopback dial would pass there on the carve-out
and prove nothing when the journey trickles down; the `spki_pinned_nest` fixture already dials
`common.nest.lan_ipv4()` for the same reason and carries the macOS self-dial preflight the journey
inherits. The leaf's SAN is that IP.

**Why not pebble.** The row that asked for this ruling named a pebble-issued IP certificate at the
docker tier as the alternative. It is not an alternative to the seam: pebble's root is a private
anchor exactly as the harness CA's is, so the app would need the same seed before it could call
pebble's leaf `webpki_valid`; and the docker tier adds an ACME round trip that proves tier_4 facts
outcome 14 does not ask about. Pebble stays the venue for the issuance machinery
([`nest/tls-certificates.md`](nest/tls-certificates.md) § B-IP *Testing*), the seam the venue for
the client's trust rule.

**Declared absence: web, by construction.** The browser terminates TLS and exposes no received
certificate to WASM; the wasm transport never builds this verifier, so there is nothing for web to
be absent from — the same shape as the source-IMAP seed. Web's reading of outcome 14 is the
possession proof over any origin (`transport-trust.md` § The login's pin, *What this is, and is not*),
witnessed on its own terms and never through this seam.

## The Microsoft authority seed — `FAUNA_E2E_MICROSOFT_AUTHORITY` (ruled 2026-10-09)

The fifth switch of the same kind, ruled before any code together with the flow it serves ([`mailbox-migration.md`](../behavior/mailbox-migration.md) § Microsoft sign-in — the device-code grant), and the first that *redirects* a far end rather than trusting one. **The gap it closes.** The Outlook import signs in through the OAuth 2.0 device-code grant against `https://login.microsoftonline.com`, and no suite may reach Microsoft: a real sign-in needs a human at a browser and a real account, while the witnesses of `mailbox-import` outcomes 17 and 18 have to run on every pass against a far end the harness controls — and the authority's address is a Rust constant several layers below anything the driver touches, which is this shape's precondition. **The seed.** `fauna_mail::imap_client::oauth_device` reads `FAUNA_E2E_MICROSOFT_AUTHORITY` — an absolute `https://` base URL that stands in for the authority, with `/devicecode` and `/token` appended to it — on the ONE door that builds the flow's endpoints, under `#[cfg(any(test, debug_assertions, feature = "test-helpers"))]` with a same-signature no-op twin, exactly as the source-IMAP seed sits on `NativeImapConnector::new`; the fake far end's certificate is trusted through `FAUNA_E2E_IMAP_EXTRA_CA_PEM`, so no second trust variable exists. **Terms:** compiled out of every release artifact (the `strings` measurement and `_apple-assert-no-e2e-env` cover it by prefix); never default-on; `https://` only — a plaintext authority is refused even under the gate, so a token never crosses a plaintext hop in a test either; the client ID and the scopes are NOT seeded, the fake accepts the constant; and it redirects the identity far end alone — the IMAP far end keeps its own seed. The fake it points at is `tests/e2e-unified/fakes/fake_microsoft_identity.py` (`mailbox-migration.md` § Microsoft sign-in — the device-code grant, the witness paragraph). **The seed is built** with the flow's shared Rust: `E2E_AUTHORITY_ENV` and its reader `authority_base` sit under the gate in `oauth_device.rs` with the same-signature no-op twin, a non-`https://` value panics, and `test_mail_import_tls_seam_gating.py` pins the gate, the twin and the reader as it pins the source-IMAP seed; the native `NativeOAuthHttp` trusts the fake's certificate through `FAUNA_E2E_IMAP_EXTRA_CA_PEM` under the same gate. **The fake far end and the harness's setting of the variable are built** (2026-10-16): `fake_microsoft_identity.session_microsoft_identity()` is the session's one instance, serving the session chain the source-IMAP anchor already trusts, and the harness seeds its `authority` beside that anchor under the same `mail_import_source` marker, for the same reason — the app reads both out of its launch environment and the drivers are session-scoped.

## The ACME directory seed — `FAUNA_E2E_ACME_*` (2026-10-13)

The sixth switch of the same kind, built for the manual-issuance witnesses and widened 2026-10-14 to the managed order the background auto-renew tick drives. **The gap it closes.** Every DNS-01 issuance — manual (§ B tier 3) or managed (§ B tier 2, which the § C.3 auto-renew tick drives with no tap) — opens its ACME order FROM THE CLIENT ([`nest/tls-certificates.md`](nest/tls-certificates.md)), and the certificate authority is a constant no human chooses (§ ACME settings — constants, not choices): no app, nest or file setting names a CA, and none may. So the walks of `admin-dns-and-certificates` outcomes 53, 54, 55, 59 and 60 had no CA they could reach, and the only redirect — `AcmeOrderTestSeam` (named `ManualOrderTestSeam` until it served both paths), the pebble real-wire proof's machine seam — was installed by no app. **The seed.** `fauna_client_dns::e2e_acme_seam` reads three variables and builds that seam from them: `FAUNA_E2E_ACME_DIRECTORY_URL` (the directory, `https://` only — a plaintext value panics even under the gate), `FAUNA_E2E_ACME_CA_PEM` (PEM text of the directory's CA, required once the directory is set) and the optional `FAUNA_E2E_ACME_NAMESERVER` (`ip:port` the propagation gate polls as the domain's authoritative set — the harness's loopback-published port, the one value `127.0.0.1:<port>` on every app (*The name server's reach*, below); unset skips the gate). Both order paths consult the seam — `open_manual_order` and the managed `obtain_managed_cert` (behind `IssueCert`), each in a gated arm beside its production branch — so the seed moves the directory, the ACME client and the probe for either, and nothing else: the SAN set, the publish through the provider seam, the seal and the delivery stay the production path. **The managed path's challenge is published, not pasted:** the e2e fake DNS provider (`SentinelDnsProvider`, itself compiled out of a release build) mirrors every TXT it publishes or withdraws into the seed's name server as an RFC 2136 dynamic UPDATE over TCP (`e2e_acme_seam::mirror_fake_txt`; the write is ruled under *The name server's reach*), so pebble and the propagation gate see the managed challenge as a real provider's zone would serve it. A delegated domain reaches the same `IssueCert` but needs a CNAME the fake name server does not serve, so its witness is the managed shape's. The CA is trusted by the seeded ACME client alone — its root store holds that CA and nothing else — never added to the app's general trust, so no `SSL_CERT_FILE` or system-store change stands in for it. **One door.** The native `build_dns_management_machine_with_credentials` installs the seam (`maybe_seed_acme_directory`, a same-signature no-op twin in a release build), so every native app that builds its admin-dns machine there — tui, linux and the `fauna-ffi` apps — honours the seed with no per-app logic; android, whose app starts by Intent and inherits no environment, carries the three variables onto its launch intent and re-exports them through `MainActivity`'s `Os.setenv` door beside the trust seed; web runs its order in wasm and has no environment to read (a declared absence). **Terms:** the module, the seam and its order entry points sit under `#[cfg(any(test, debug_assertions, feature = "test-helpers"))]` (native only), and tui's `e2e-agent` forwards `fauna-client-dns/test-helpers` for a release-profile e2e build; never default-on — the harness seeds it only for a session that selects an `acme_pebble`-marked test (`_apply_acme_pebble_env`); the CA constant and the empty account contact are untouched — the seed moves the directory, not the policy. The stack it points at is `tests/e2e-unified/helpers/acme_pebble.py`: pebble on a private docker network, its directory published on a loopback port taken through `android_venue.nest_port` (in an android venue run, a lease on the tunnelled range, so the launch's `adb reverse` reaches it), validating through a `fakes/fake_dns.py` sidecar — published to loopback the same way (*The name server's reach*) — whose records the test writes (`AcmePebble.publish_txt`) or the app's fake provider publishes. **The tick's poke.** The auto-renew tick sleeps six hours before its first pass, so outcome 55's walk drives it through each app's `auto_renew_wake` agent command (`fauna_e2e_agent::AUTO_RENEW_WAKE`, convention 14's `run_now`): the production loop with only its wait raceable, never a compressed cadence knob. tui and linux race it with a `LoopWake` (the critical-alert sweep's shape, its notifier and raced arm under the app's convention-15 gate); android's loop takes its wait from `TestAgent.autoRenewWaiter`, the debug source set's `AutoRenewWake` in a test build and a bare `delay` in the `src/noAgent` twin every shipping flavor compiles. `test_acme_directory_seed_gating.py` pins the gate, the twin, the `https://` refusal, the Rust/harness spelling agreement of the three variables, the managed path's gated seam arm, the mirror's sentinel-only call sites and each built app's wake gate; the shipping-dep-line half is the general `test_test_helpers_dep_line_gating.py` scan. **The one non-app reader** is the e2e harness library, as for the source-IMAP seed: `fauna-ffi`'s `e2e-harness` flavor (shipped in nothing) forwards `fauna-client-dns/test-helpers`, so the app-less managed issue its `fauna_harness_dns_dispatch` export runs — the shared builder's machine again, with the fake provider and this seed read from the test process's own environment — is a `[nest]` witness of what the issuing client orders.

**The name server's reach — ruled 2026-10-14.** The gap: the device of an android venue run reaches a harness service only as its own loopback, through an `adb reverse` and the venue's SSH tunnel, both TCP-only ([`testing.md`](testing.md) § Default app and nest mode → *Android's run venue*, constraints 2 and 3), while the fake name server sat on a docker-bridge address answering UDP alone and the managed path's mirror wrote a host directory the device cannot see — so every witness whose issuance ran the app's own propagation probe, and the whole managed path, was an android skip. **Three decisions, one route, no test-only transport.** **(1) The probe speaks DNS over TCP as production behaviour, not under a seed** — the RFC 7766 shape [`nest/tls-certificates.md`](nest/tls-certificates.md) § Implementation status today → *The browser's arm of the propagation gate* → **Transport** owns: UDP first on a connected socket, one TCP retry of the same query on a truncated answer, a UDP transport error or the UDP timeout. A seed-selected test-only transport (a `tcp://` scheme on `FAUNA_E2E_ACME_NAMESERVER`, or a fifth variable) was the alternative and is refused (§ Seeds ruled out): a real authoritative server must answer TCP, so the retry is a fidelity gain the production probe owed anyway, and a transport knob read by the shared core would be a convention-15 seam inside the one query every native app and the nest run. On the device the UDP leg fails at once — nothing listens on the device's loopback UDP port, and a connected socket reports the port-unreachable as an error rather than waiting out the timeout — and the TCP retry rides the reverse. **(2) The fake name server is reached at the dev VM's loopback, on a leased port, over TCP and UDP alike, by every app.** `fakes/fake_dns.py` serves TCP (two-byte length framing) beside UDP on its port; the sidecar publishes that port to `127.0.0.1:<port>` for both transports, the port taken through `android_venue.nest_port` as the directory's is (`acme_pebble.lease_name_server_port`, beside `lease_directory_port`), so in a venue run it is a lease on the tunnelled range and the android launch adds it to `nest_ports`; `FAUNA_E2E_ACME_NAMESERVER` is `127.0.0.1:<port>` on every app — one seed value, the venue only reversing it — and pebble keeps validating against the sidecar's container address on the docker network. **(3) The managed challenge is written into the name server over the same TCP port, as an RFC 2136 dynamic UPDATE, and the records-directory variable is retired.** `mirror_fake_txt` sends one UPDATE per publish or withdrawal to the seed's name server — zone the challenge name's parent, an add (class IN) per published TXT, a delete-this-RR (class NONE, the withdrawn value) per withdrawal, unsigned: the far end is a fake on loopback — and the fake applies it to an in-memory overlay above its records file, which stays a read-only mount (the sidecar never writes the host; `AcmePebble.publish_txt`, the manual path's "admin pastes the record", keeps writing the file). So the seed is three variables, all device-meaningful, and android forwards the whole of it; `FAUNA_E2E_ACME_RECORDS_DIR` is removed, not kept as an alias (the seed is test-only, and the product is in the 0.1.x compat-free window). A records write over a second HTTP port was the alternative and is refused: a second port, protocol, lease and variable to carry what the name server's own update protocol carries on the port already reversed. **Terms:** nothing here widens production — the TCP leg is standard resolver behaviour, the UPDATE sender sits under the convention-15 gate, and the fake's UPDATE arm is harness code; the probe's unit tests gain a TCP-only responder and a truncating one, `fake_dns.py` gains an in-process tier_1 test of both arms, and `test_acme_directory_seed_gating.py` pins the three-variable seed.

## The preset-host redirect seed — `FAUNA_E2E_IMAP_HOST_MAP` (ruled 2026-10-17)

The seventh switch of the same kind, and the one this doc first REFUSED (2026-10-13, § Seeds ruled out) and then reopened under that refusal's own terms. **The gap it closes.** The Outlook import's sign-in has two far ends, not one: the Microsoft identity authority, which § The Microsoft authority seed redirects, and the IMAP server the resulting bearer is presented to with `AUTHENTICATE XOAUTH2`. An Outlook pick dials the fixed preset `outlook.office365.com:993` — `fauna_client_mail_settings::import::provider_preset`, a constant no human chooses, written on `SelectSourceKind` and offered for change by no action and no app field, since [`mailbox-migration.md`](../behavior/mailbox-migration.md) § Microsoft sign-in — the device-code grant paints no host field for Outlook. So the witnesses of `mailbox-import` outcomes 17 and 18 (that doc's "Witnessing outcomes 17 and 18" paragraph) — the import completing against a fake IMAP source that recorded one XOAUTH2 authentication carrying the fake authority's token, then the nest's files searched for that token after the import — had no server they could reach: approving the code in a test would have sent the app, with the fake's bearer, to Microsoft's real IMAP server, which no suite may reach. The 2026-10-13 refusal excluded exactly this case on the premise that "the Outlook sign-in's far end is the Microsoft authority seed's"; that premise named one far end of two, and the refusal's reopen clause — a second outcome that genuinely needs a preset kind to reach a harness IMAP server — fires. **Why not amend the witnesses instead.** The alternative weighed was to let the app journey witness only the code shown, the client ID asked with and the grant, and leave the XOAUTH2 leg to the shared crate's tests. Refused: the native XOAUTH2 leg — `NativeImapConnector` dialling, verifying and handing a real TLS socket to `ImapSession::authenticate_xoauth2` — is exercised by no test at any tier today (`libs/fauna-mail/tests/imap_client_native.rs` has no XOAUTH2 case, `session.rs`'s two XOAUTH2 tests are sans-io, and the wizard machine's grant test runs against a fake seam), so the amendment would have left the one leg that carries the token across a socket unwitnessed anywhere; and outcome 18's black-box read of the nest's files after an import that never ran would witness nothing, since no import RPC would have carried anything. A mechanism is testable, and this one leaves a human only the last inch. **The seed.** `NativeImapConnector::new` — the ONE door every native consumer builds its source connector through (`rpc_glue`'s `RpcImportSourceNest` for linux and tui, `fauna-ffi`'s `FfiMailImportClient` for apple, android and windows), where the source-IMAP trust seed already sits — additionally reads `FAUNA_E2E_IMAP_HOST_MAP` under `#[cfg(any(test, debug_assertions, feature = "test-helpers"))]` with a same-signature no-op twin: a comma-separated list of `<host>=<ip>:<port>` entries. `connect(host, port)` looks the host up before dialling; a mapped host is dialled at its entry's address and port instead, and **everything else is unchanged** — the `ServerName` the handshake verifies, the SNI it sends, the host every error names and the host the wizard shows all stay the provider's own, so the far end must present a leaf carrying the provider's name that chains to an anchor the connector trusts, which in a test-capable build only the source-IMAP trust seed supplies. **Terms:** compiled out of every release artifact (the `strings` measurement and `_apple-assert-no-e2e-env` cover it by prefix, and the measurement is recorded with the build); never default-on — the harness sets it only beside the trust seed and the authority seed, under the `mail_import_source` marker; **loopback only** — an entry whose address is not a loopback address panics naming the variable even under the gate, as a plaintext authority does, so the seed can point a connection nowhere but into the harness's own box (on an android venue run the device's loopback, reversed by the launch as the ACME seed's ports are); a malformed value panics rather than degrading; the redirect never touches `provider_preset`, which is wasm-shared and has no environment to read; and **the harness maps one host, `outlook.office365.com`, and no other** — the gating witness pins that the harness's key set is exactly that host and that its spelling is the Rust preset's (`test_mail_import_tls_seam_gating.py`, beside the source-IMAP seed's pins). **Ground (2) of the refusal, re-weighed for a bearer.** The refusal's second ground held that a seed moving the far end a user's Google or Apple app password is sent to is a different class of hazard from one adding a trust anchor. For the one witness this seed serves, the payload is a bearer the harness's own identity fake minted, scoped to the IMAP resource and good for an hour; no real credential exists anywhere in the walk. Structurally, a test-capable build carrying the trust seed could already be pointed at a harness host by the box's own name resolution, so the map adds no reach that build lacked; what the ground still buys is scope, and it is kept as the one-host term above — mapping Gmail's or iCloud's host would serve a witness where a user's app password is the payload, and needs its own ruling against a witness that is otherwise impossible, which none is today (outcome 51's hint is pinned per provider at tier 1, and outcome 16 witnesses the picker). **The harness half.** The port is leased at session start — the app reads the map out of its launch environment and the drivers are session-scoped, exactly as the authority's port is — through the `android_venue.nest_port` seam `acme_pebble.lease_directory_port` uses, so a venue run reverses it; a per-test `FakeImapSource` binds that leased port (its listener sets `SO_REUSEADDR`, and its stop shuts the listener down before closing it, so the next test's source rebinds the port at once) and presents a leaf the session CA issues for `outlook.office365.com`, so the handshake verifies under the Outlook name. **Witnesses:** an own-process behaviour test in its own test file beside `imap_client_trust_seed.rs` (that file is one process for one env var by design) — a mapped host is dialled at the loopback listener and verified under its own name (a leaf for `localhost` fails under the map, one for the mapped name passes), an unmapped host is dialled as given, a non-loopback entry and a malformed value panic naming the variable; the gating pins (the constant and the reader gated with a twin, every `env::var` inside a gated reader, the Rust/harness spellings of the variable and of the mapped host, the one-host key set); and the two witnesses the seed exists for, outcome 17's tui journey and outcome 18's app-free import. **Declared absence: web, by construction** — the wasm transport builds no `NativeImapConnector` and has no environment to read, as for the source-IMAP trust seed. **Built 2026-10-17**: the reader is `e2e_host_redirects` with its twin, the parse `parse_host_map`, the dial `connect`'s `redirect_for` lookup; the own-process test is `libs/fauna-mail/tests/imap_client_host_map_seed.rs`, the harness half `fake_imap_source.session_preset_redirect` / `session_cert_for` / `FakeImapSource(..., port=…)` written by `conftest._apply_mail_import_source_trust_env`; the two witnesses are `tests/e2e-unified/tests/test_mail_import.py::test_outlook_signs_in_with_microsoft_and_imports` (outcome 17, on tui) and `tests/e2e-unified/tests/api/test_mail_import_client_api.py::test_the_microsoft_token_never_reaches_the_nest` (outcome 18, the shared machine with no app, through `fauna-ffi`'s `fauna_harness_mail_import_outlook`, which reads the three seeds from the test process's own environment). **Measured** on `fauna-mail`'s release rlib (`cargo rustc -p fauna-mail --release --lib --features imap-client-native -- -C debug-assertions=on|off`, then `strings -a <rlib> | grep -c FAUNA_E2E_IMAP_HOST_MAP`): 3 with debug assertions on, **0** off, the production string `is not a valid source hostname` present (2) in both, so the zero is the gate's.

## Seeds ruled out

A seed is a permanent surface and a standing hazard (§ Implementation status today's release-profile warning), so the shape is taken only where a witness is otherwise impossible. This section records the seeds considered and refused, with the chain that makes each one unneeded, so the next session that meets the same gap reads the ruling instead of re-deriving it — and the terms under which a ruling would be reopened.

**A seed-selected transport for the propagation probe — REFUSED (2026-10-14).** The android venue reaches the harness's fake name server over TCP only (§ The ACME directory seed → *The name server's reach*), and the candidate was a transport the seed picks — a `tcp://` scheme on `FAUNA_E2E_ACME_NAMESERVER`, or a fifth variable — threaded into `fauna_core::authoritative_dns`, the one query every native app and the nest's `fauna.dns.probe_txt_visible` run. Refused because the production probe owes the TCP leg anyway: RFC 7766 makes TCP mandatory for every DNS server, so a UDP-only probe is the less faithful one, and a UDP-first, TCP-on-failure probe reaches the device's reversed port with no knob at all. Reopen only if a production reason ever forbids the probe's TCP leg — then the seed would be the only route, and this ruling would be re-weighed.

**The preset-host redirect — REFUSED (2026-10-13); REOPENED 2026-10-17 for one host, § The preset-host redirect seed.** [`mailbox-migration.md`](../behavior/mailbox-migration.md) § Wizard steps step 2 says a failed source connection is told in a plain sentence plus the provider's hint, and `mailbox-import` outcome 51's witness (`test_mail_import.py::test_a_failed_connection_is_explained_in_plain_words_never_the_servers_reply`) walks it with the Generic IMAP kind only: a preset (Gmail, Outlook, iCloud) dials its provider's own host — `fauna_client_mail_settings::import::provider_preset`, a constant no human chooses — so no harness server can stand behind it, and the hint half has no app witness. The candidate was a seventh seed redirecting a preset's host and port to a harness address, on `provider_preset` or on `NativeImapConnector::new` beside the source-IMAP trust seed. **Refused, on two grounds.** (1) It would witness nothing the existing chain leaves open. The hint is appended by `connect_failure_sentence` from the machine's own kind at the one connect site, and that function and that path are unit-tested per provider — `a_refused_sign_in_says_so_in_plain_words_with_the_providers_hint` drives Gmail and iCloud through `dispatch(Connect)` against a refusing source and asserts sentence + hint exactly, `a_refused_xoauth2_connect_forgets_the_token` does the same for Outlook; the string they pin lands in the same `error` field the Generic arm proves tui paints as `error-message`; and tui's picker writing the machine's kind — the one app-side link — is witnessed by outcome 16's test, which selects Gmail and iCloud and reads the app-password help the Source step paints from the snapshot's `source_kind`. Every link is covered, and a redirect arm would only re-walk the last two end to end. (2) It would be the one seed whose payload is where a user's Google or Apple app password is sent: the single promise a preset makes over Generic is that the password goes to the provider, and a variable that silently moves that far end is a different class of hazard from a seed that adds a trust anchor or moves an identity endpoint over `https://` only. The cost — the Rust door and its twin, a gating witness, a loopback port reserved at session start for the fake to bind (the app is launched before any per-test fake exists), a leaf certificate carrying the provider's name — would buy a re-walk of a hint the unit tests already pin. **Terms of the ruling:** outcome 51 keeps the Generic witness as its honest app proof, its docstring points here, and the catalog is untouched (nothing moved — [`feature-catalog.md`](feature-catalog.md) § Closing a gap). **Reopened only by** a second outcome that genuinely needs a preset kind to reach a harness IMAP server; none exists today — outcome 52's Gmail-mailbox roles are what the server advertises, kind-independent, and the Outlook sign-in's far end is the Microsoft authority seed's. Should one appear, the seed goes on `NativeImapConnector::new` next to the trust seed, as a host-and-port map that keeps the TLS name it verifies, never on `provider_preset`, which is wasm-shared and has no environment to read. **Reopened 2026-10-17:** that second outcome had existed all along — the premise above named one far end of the Outlook sign-in where there are two, and its IMAP leg, `outlook.office365.com:993` with the bearer, had no seed — so the seed is ruled on exactly this paragraph's fallback shape, scoped to Outlook's host and to loopback targets, in § The preset-host redirect seed; grounds (1) and (2) stand as written for outcome 51, whose witness stays Generic, and for the Gmail and iCloud presets, which stay unmapped.

## Implementation status today

- **The e2e trust seed is keyed by nest, and the three conftest login seams and the crash-recovery journeys relaunch a session app seeded for a dedicated nest (2026-09-15); the other seams that point an app at a nest its launch did not name do not yet.** The two doors and the harness's writer carry the keyed grammar (§ The e2e trust seed). `conftest._relaunch_trusting_nest` is the relaunch: it reads the environment the driver's next `recover()` starts with (`HttpBridgeDriver.relaunch_environment` — `_launch_config` on linux, tui, macOS and iOS; the bridge session body on windows; none on web, which reads no environment, or on android, which cannot cold-relaunch), rebuilds the seed as the launch entries plus the nest's own, and relaunches; it is a no-op for the launch nest and for an unseeded session. `_login_app_as`, `_login_admin_as` and `handled_logged_in_app` call it before their `set_state`, an order `tests/e2e-unified/tests/test_r14_trust_seed_default.py` pins. **Witnessed on tui, linux, macos, ios and windows:** `tests/e2e-unified/tests/test_r14_trust_seed_dedicated_nest.py` asserts a tip-sealed row reaches a dedicated nest — red on tui before the seams were wired (two fleet-only rows, none tip-sealed; 50 `device_endpoints: Unmintable` pump reports against the launch-nest control's `Published`), green after on tui and on linux, with `test_delete_account.py` green across the new relaunch. **The macos/ios run needed one more fix first, not a relaunch defect****: `FaunaKit`'s `AppStateObservables` omitted the `account_pump_cycles` state key entirely pre-login (`liveClient == nil`), instead of the shared Rust contract's own pre-session fallback shape (`{started:0,completed:0,runtime:false,holder:false}`) that windows' `AccountPumpCyclesForSerialization` already publishes — so the witness's own pre-flight capability probe misread a built leg as absent and skipped, on both apple platforms, every run. Fixed by publishing the same fallback shape apple-wide (one shared file covers macOS + iOS); green on both after. **The windows run needed no fix:** the dedicated-nest witness's two cases (launch-nest control, dedicated-nest) and both consumers, `test_delete_account.py` and `test_admin_registration_posture.py` (3 tests), went green first try — 6/6, 2026-09-15. **The measurement trap, recorded because the witness's first draft fell into it:** a non-empty `state-fleet` page is no evidence of a tip — the generation machinery itself (device set, escrow target, mint, wraps, receipt) is `SealingEpoch::Gen0`, envelope form v1, and fills that plane on every first pass; only a form-v2 envelope (`tests/common/auth.py::is_generation_sealed`) is. **The crash-recovery journeys seed before their choreography, because their seed cannot ride the pointing call the way a login seam's does.** Each journey (`tests/e2e-unified/tests/test_crash_recovery_journeys.py`) points the session app at a per-test crash nest and then kills the app or the nest, usually re-pointing it after the crash — and a fresh launch there would erase the on-disk state the journey tests. So each journey calls `_relaunch_trusting_nest` before it first points the app at its crash nest; every relaunch after the kill (`hard_reload()`, `recover()`) re-reads the environment that call seeded, and the one seed holds through a factory reset or a nest restart because the nest identity it names does ([`nest/common.md`](nest/common.md), `nest_deployment.key`). The same tier_1 file pins that order per journey, anchored on each journey's own first `inject_admin_session`/`navigate_to_status` call and its first kill — the `set_state` sits inside the helper, where a source-order check of the journey cannot see it. **Witnessed on tui and linux:** journey 1 asserts a tip-sealed row on its crash nest through `tests/e2e-unified/helpers/trust_seed_witness.py`, the same reader the dedicated-nest witness uses — red on tui before the journeys seeded (2 fleet-only rows, none tip-sealed, beside a pump holder cycling passes), green after, with all 11 journeys green on tui and on linux. **Apps a test launches itself are seeded too (2026-09-15).** Second seats, the launch harness (its writer handed in as `seed_trust`, each launch's nest named as `trust` and its identity read afresh on every relaunch), the version-skew grid's two builds, the sync seats (whose nest handle had carried no port), the box-recovery launches that use two nests and a proxy, and the two bare process launches (the desktop binary; `flatpak run --env=`) all seed the nests they point at. `tests/e2e-unified/tests/test_r14_trust_seed_self_launch.py` scans every driver launch under `tests/` and fails on one that neither reaches the writer nor is a browser launch, a relaunch of the config already launched, or a declared exception with its reason. `test_r14_trust_seed_dedicated_nest.py`'s self-launch arm is the witness, measured on tui: with the harness's seed withheld, two gen-0 fleet-only rows, none tip-sealed, and 25 `device_endpoints: Unmintable` pump reports; with it, a tip-sealed row on the first poll. The windows, macOS and iOS self-launch sites are wired and scanned but have not been run. **Not yet:** the login helpers outside conftest (`helpers/mail_dedicated_nest.py`, `helpers/e2e_session.py`, the UI claim in `helpers/mail_client_ui.py`, module-local and inline sites).
- **The nest-leg trust seed is BUILT (ruled 2026-10-09, built 2026-10-13; § The nest-leg trust seed), tui first.** **The seam:** `fauna_ws_substrate::tls_verify::webpki_verifier()` builds its `RootCertStore` from `fauna_tls_bootstrap::webpki_root_store()` and passes it through `e2e_extra_nest_roots`, which under the convention-15 gate adds every CERTIFICATE block in `FAUNA_E2E_NEST_EXTRA_CA_PEM` (constant `E2E_NEST_EXTRA_CA_ENV`, gated the same way) and panics naming the variable on a malformed or block-less value; the production twin takes the same `&mut RootCertStore` and does nothing. Its own-process behaviour test `libs/fauna-ws-substrate/tests/e2e_nest_trust_seed.rs` runs a real in-memory rustls handshake from `capturing_client_config` dialled as `ServerName::IpAddress(127.0.0.1)`: a harness-CA IP-SAN leaf is not `webpki_valid` unseeded and is once seeded (so rustls accepts an IP SAN, which the LAN dial needs), a self-signed leaf stays not-valid under the seed, two malformed values panic naming the variable, and a blank value is "unset". **The tier_1 witness** `test_nest_leg_tls_seam_gating.py` pins the gate on both items (derived), every `env::var` in `tls_verify.rs` inside the gated reader's body, the same-signature twin, add-only (the reader adds to the store and names no `NoPinPolicy`, `ServerCertVerified`, `dangerous`, pin resolver or `webpki_valid`; `webpki_root_store()` is called once, by the one door; `config_with` builds on it) and the Rust/seed-test/journey spellings; the shipping-dep-line half is the general `test_test_helpers_dep_line_gating.py` scan, as for the ACME directory seed. **Artifact measurement, both ways, varying only the gate** (`cargo rustc -p fauna-ws-substrate --release --lib -- -C debug-assertions=on|off`, `strings -a … | grep -c FAUNA_E2E_NEST_EXTRA_CA_PEM`): **on 3, off 0**, with the verifier's own `pinned nest identity` error string present in both (1 and 1), so the zero is the gate and not an empty artifact; the off build compiles with zero warnings, so the twin is exercised. **The fixture** `webpki_lan_nest` (`conftest.py`, routed through `_start_dedicated_nest` with `{dial_host, serve_tls}`, registered in `nest_surface.FIXTURE_START_OPTIONS`) dials `lan_ipv4()`, writes `EphemeralCA().issue(<lan ip>)` as `<tmp_dir>/acme/privkey.pem` + `fullchain.pem`, and returns only once a Python `ssl` handshake with that CA as its ONLY anchor, hostname checked, verifies the served chain (term 5's harness half; the watcher reload is polled, never slept on); its indirect `self_signed_control` param installs nothing and asserts the opposite handshake fails. **The journey** `test_nest_identity_pin_webpki.py` (tui; the seed set through the launch harness's per-launch `environment`, this test only — term 4): half A logs in and reads `nest_identity_pin_for_test` = `nest_id_from_data_dir`; half B stops nest A through its own handle, starts nest B on A's port and address from a fresh data dir with a same-CA leaf and the same user registered, relaunches → `nest-identity-changed-warning`, no `launch-retry-button`, and re-trust re-pins B. The impostor's start takes `nest_binary` (no provider start option takes a port), recorded OPEN in `test_nest_mode_axis.py`'s `_NEST_BINARY_DISPOSITIONS`. **Red-first on Linux, both arms each build:** current code — webpki PASSED, control PASSED; `binding.is_none() &&` removed from `webpki_waives_binding` — **webpki FAILED at half A** (`the store holds None`: the WebPKI login took the waiver and recorded no pin, as before 2026-10-05), control PASSED. The control's indifference is what shows the webpki arm walks the WebPKI branch. `connect-and-sign-in` outcome 14 cites the journey. **Trickle-down:** linux joined 2026-10-14 (test app list only, no app code; both arms green); macos, ios and windows join by a launch-harness leg each (the seed is read in shared Rust; the FFI flavors forward `fauna-ws-substrate/test-helpers`); web is the declared absence by construction.
- **The ACME directory seed's name-server route is BUILT (ruled and built 2026-10-14; § The ACME directory seed → *The name server's reach*).** The probe's TCP leg is the production one ([`nest/tls-certificates.md`](nest/tls-certificates.md) owns its measurement). `fakes/fake_dns.py` serves TCP on its UDP port and applies UPDATEs to an overlay above its read-only file (`test_fake_dns.py`, tier_1). The sidecar is published to `127.0.0.1:<lease>` for both transports (`lease_name_server_port`, `name_server_port(env)` in the android `nest_ports`). `mirror_fake_txt` sends one UPDATE per zone through the gated `fauna_core::authoritative_dns::send_txt_update`; a release-profile e2e build reaches that function through `fauna-core/test-helpers`, which tui's `e2e-agent` and fauna-ffi's `e2e-harness` forward. The records-directory variable is gone, pinned by the seed gating test. Android has one skip branch, the first venue run. **Walks on Linux:** tui, all 5 manual-issue outcomes and the auto-renew tick, 6/6 green; linux, the manual file 5/5 green; `just e2e-pebble-dns01` green.
- **The preset-host redirect seed is BUILT (ruled and built 2026-10-17; § The preset-host redirect seed), its measurement recorded there.** The gated reader and its twin, the dial lookup, the own-process test, the gating pins, the session lease and the Outlook-named leaf are in; the witnesses it exists for — `mailbox-import` outcome 17's tui journey and outcome 18's app-free import — are written (§ The preset-host redirect seed names them). Android forwards neither this seed nor the source-IMAP trust seed yet (the android mail-import lift carries both).
