# Sessions — target state

Owns: sessions-page
Status: ratified 2026-09-25 (user) — the one ruling on (a) the page's home, (b) the element-ID package (rule A, all three legs) and (c) the confirm shapes, recorded verbatim in § The ruling; designed 2026-09-19. Unbuilt on every app (§ Implementation status today). The behavior it renders was ratified earlier and separately: [`../behavior/devices.md`](../behavior/devices.md) § Session Management + § Emergency lockout.
Authority: ui.yaml (`sessions` page — allocated with the first implementation) owns IDs + per-page element scope; this doc owns the page UX only — layout, row content, the escalation ladder, confirm shapes, the snapshot the page reads. Defers: what a session is, what revoking does and does not do, the client's own-session identity, the lockout's meaning, the locked state and how the two panic buttons relate → [`../behavior/devices.md`](../behavior/devices.md) § Session Management / § Emergency lockout; the stolen-identity ceremony → [`../behavior/identity-succession.md`](../behavior/identity-succession.md); the device roster → [`devices.md`](devices.md); the Settings shell + rail → [`settings.md`](settings.md) § Navigation model; the onboarding steps → [`../behavior/onboarding.md`](../behavior/onboarding.md).

## Goal

Show the member where their account is signed in right now, let them cut one sign-in or every other sign-in, and give them the 24-hour emergency lock — as one escalation ladder whose copy never promises more than the mechanism delivers.

## Implementation status today

**Legs 1 and 2 are built on tui (2026-09-28, 2026-10-04); the other six apps and leg 3 are unbuilt.** The shared half is in: `fauna_client_account::SessionsClient` (the four bearer kinds, the keep-id read from the bearer holder through `fauna_protocol::auth::OwnSessionSource`, answered natively by `fauna_client::NestClient`), the `sessions_view` fold and `fauna_client_recovery::ceremony::LOCKOUT_CONFIRM_WORD`, pinned by their crates' unit tests; ui.yaml carries the `sessions` page, the `session-card` component and the thirteen leg-1 IDs. tui renders the page off the fold (`apps/fauna-tui/src/settings/sessions.rs`), both confirm shapes in its glue, and is witnessed end to end by `tests/e2e-unified/tests/test_sessions_page.py` (list → revoke one → sign out everywhere else). Not yet built: the UniFFI face `libs/fauna-ffi/src/sessions.rs` and its wasm twin (§ Where logic lives) — the six-app trickle-down builds them with its first consumer; tui, a Rust app, calls the crate directly. On a lock that lands, tui leaves the shell and relaunches over the same account, which lands the locked surface. **Leg 2 on tui:** ui.yaml carries the page `launch_account_locked` (the two new IDs) and the step `identity_stolen_entry` (existing elements only); tui paints both as launch surfaces (`apps/fauna-tui/src/locked.rs`), the notice following the launch machine's snapshot and the ceremony riding the shared `fauna_client_recovery::ceremony::succeed_stolen_identity`; witnessed end to end by `tests/e2e-unified/tests/test_locked_surface.py` (lock → the locked surface → the ceremony → signed in as the successor). The nest half is built and conformance-tested. All four gaps [`../behavior/devices.md`](../behavior/devices.md) § Implementation status today measured on 2026-09-19 are built: the two mechanism gaps (own-token-id retention in every bearer holder, 2026-09-20; per-token socket teardown on revoke, 2026-09-20), verify enforcing the lock (2026-10-01, its app half 2026-10-03), and the locked surface on tui (2026-10-04). Build order: shared Rust, then tui — one row per leg — then the six-app trickle-down.

## The ruling (rule A + home — ruled 2026-09-25 by the user, every item as recommended)

1. **Home.** A new Settings rail sub-page **Sessions** (ui.yaml page `sessions`, heading "Sessions"), placed directly **after Devices** — "machines enrolled" beside "sign-ins live now"; the rail slot is [`settings.md`](settings.md) § Navigation model's. The alternative put beside it — widening [`devices.md`](devices.md)'s ratified roster-only scope and rendering this doc's regions below the roster — was refused with it, for the reasons that carried the recommendation: the roster-only ruling and `DevicesMachine` stay untouched; the Account sub-page stays build-once (`settings.md` § Live-data placement) — a live indexed list does not belong there; and a session is not a device (a seed-holding app has a session and may have no roster row at all), so one list under one heading would invite exactly that confusion.
2. **The ID package — all three legs approved as proposed** (§ Element IDs): leg 1 the page, leg 2 the locked surface, leg 3 the signed-out door.
3. **Confirm shapes.** Revoke one: none (recoverable by re-auth). Sign out everywhere else: the two-press inline confirm (the `sign-out-confirm-button` idiom, inline on every app — no native-dialog variant). Lock: type-to-confirm, the literal `LOCK`, never localized (the `identity-stolen-confirm-field` / `settings-delete-confirm-field` idiom), re-checked in the action arm and not only in the render.
4. **The launch mint refuses a locked account too** — `fauna.auth.verify` enforces the lock exactly as the handshake does, so every launch path receives `fauna.auth.account_locked` and the locked surface (leg 2) is reachable from every app. Ruled with this page because leg 2 is dead without it; owned by [`../behavior/login.md`](../behavior/login.md) § Silent Challenge (the enforcement) and [`../behavior/devices.md`](../behavior/devices.md) § The locked state (the surface).

## Layout & flow

Reached via the Settings shell (`{"view":"settings","id":"sessions"}`). Three regions, top to bottom — the ladder:

1. **The list.** One `session-card` per live session, this app's own first, then this device's other sessions, then the rest by last activity, newest first. A card carries: what kind of sign-in it is (`session-kind`), its times and address (`session-detail`), a "This app" / "This device" mark where it applies (`session-this-mark-badge`), and `session-revoke-button` — absent on this app's own card (leaving this app is Sign out, on the Account sub-page).
2. **Sign out everywhere else.** `sessions-revoke-others-button` → `sessions-revoke-others-confirm-button` / `sessions-revoke-others-cancel-button`. The required note beside it (`sessions-revoke-note`) states the honest bound — owner [`../behavior/devices.md`](../behavior/devices.md) § What revoking a session does.
3. **Lock this account for 24 hours.** `sessions-lockout-warning` (REQUIRED floor copy, always visible — never behind the confirm), `sessions-lockout-confirm-field`, `sessions-lockout-button`. The warning must say all five things [`../behavior/devices.md`](../behavior/devices.md) § The two panic buttons lists, the last being where the *other* panic button is and that it still works while locked.

`error-message` is the page-level error surface; `settings-nav-back` as on every sub-page.

**The locked surface (leg 2).** A device that meets `fauna.auth.account_locked` paints a standing `launch-account-locked-notice` on its launch surface — the unlock time, and the sentence that a lock the owner did not set means somebody holds the secret key — with `launch-account-locked-stolen-button` routing to the onboarding step `identity_stolen_entry`: the existing `recovery-entry-phrase-field`, `identity-stolen-confirm-field`, `identity-stolen-button`, `recovery-entry-back-button` and `error-message`, no new element. Behavior owner: [`../behavior/devices.md`](../behavior/devices.md) § The locked state.

**The signed-out door (leg 3).** `identity_choice` gains the optional `emergency-lockout-entry-button` → a new onboarding step `emergency_lockout_entry`: `paste-secret-field` + `recovery-entry-account-field` (both existing), `sessions-lockout-warning`, `sessions-lockout-confirm-field`, `sessions-lockout-button`, `emergency-lockout-back-button`, `error-message`. Nothing is persisted on the device — that is the door's whole reason to exist (owner: [`../behavior/devices.md`](../behavior/devices.md) § The signed-out door).

## Element IDs

APPROVED 2026-09-25 (rule A, all three legs, as listed). Legs 1 and 2 are allocated in ui.yaml, under exactly these names; leg 3 lands with its first implementation (tui).

- **Leg 1 — the page (`sessions`):** `page-heading`, `error-message`, `settings-nav-back` (existing); new: `session-card` (view, indexed), `session-kind` (text, indexed), `session-detail` (text, indexed), `session-this-mark-badge` (text, indexed, optional — renders only on own rows), `session-revoke-button` (button, indexed, optional — absent on this app's own row), `sessions-empty` (text, optional), `sessions-revoke-others-button`, `sessions-revoke-others-confirm-button`, `sessions-revoke-others-cancel-button`, `sessions-revoke-note` (text), `sessions-lockout-warning` (text), `sessions-lockout-confirm-field` (text_input), `sessions-lockout-button`. Thirteen new IDs.
- **Leg 2 — the locked surface:** new `launch-account-locked-notice` (text), `launch-account-locked-stolen-button` (button); new onboarding step `identity_stolen_entry` composed entirely of existing elements. Two new IDs. In ui.yaml the two IDs sit on their own launch page, `launch_account_locked` — the shape every other terminal launch verdict has (`launch_sign_in_refused`, `launch_identity_changed`) — with no retry and no fallthrough beside them.
- **Leg 3 — the signed-out door:** new `emergency-lockout-entry-button` (optional on `identity_choice`), `emergency-lockout-back-button`; new onboarding step `emergency_lockout_entry` otherwise composed of existing and leg-1 elements. Two new IDs.

## State & data shape

One pure, wasm-clean fold in shared Rust (§ Where logic lives) produces the whole page:

```
SessionsSnapshot { rows: Vec<SessionRowView>, error: Option<LocalizedText> }
SessionRowView {
    token_id: String,               // row identity + the revoke argument
    kind: LocalizedText,            // "App sign-in" | "Device: {name}" | "A device key not in your device list"
    mark: Option<LocalizedText>,    // "This app" | "This device"
    created_at, last_used_at, expires_at: u64,   // epoch seconds — see below
    ip_address: Option<String>,     // None → the detail line says the address was not recorded
    can_revoke: bool,               // false on this app's own row
}
```

Inputs: the `fauna.sessions.list` reply, this process's own live token ids, the device roster (`fauna.sync.devices.list`, for the `minted_by_device` = `principal` join and the device name — handed in as `(principal, name)` pairs already rendered by `fauna_devices_machine`, which owns the sealed-label render, so the fold never touches keys), this machine's enrolled principal, and a caller-supplied `now`. This app's own superseded-but-unexpired tokens **fold into its one row** — the rule and its reason are [`../behavior/devices.md`](../behavior/devices.md) § The client's own session. Times cross as epoch seconds and each native app renders them through the shared `format_unix_local`; web formats them itself (the custody-row precedent, [`devices.md`](devices.md) § Implementation status today). The page is a read-through with no local cache.

## Where logic lives

- **The four bearer kinds** — shared Rust: `fauna_client_account::SessionsClient<R: RpcRequester, S: OwnSessionSource>` (`list`, `revoke`, `revoke_others`, `lockout`; `S` is the bearer holder's own-id seam, `fauna_protocol::auth::OwnSessionSource`), beside `AccountClient` in the crate that is already "the account-management surface clients hit from Settings" and already in every app's graph, wasm included. `revoke_others` takes no argument: it reads the keep-id from the bearer source at call time.
- **The fold** — shared Rust, same crate, module `sessions_view`; pure and wasm-clean.
- **The pre-identity lockout** — shared Rust: `fauna_client_recovery::lockout::lock_account_with_seed(client, &ActorKeypair)`, in the crate that owns the anonymous seed/RecoveryKey-signed emergency plane; `fauna-client-account` scopes itself to bearer kinds by its own crate doc.
- **Own-session identity** — shared Rust, in the bearer holders; owner [`../behavior/devices.md`](../behavior/devices.md) § The client's own session.
- **The confirm word** — shared Rust constant `LOCKOUT_CONFIRM_WORD`, beside `STOLEN_CONFIRM_WORD`.
- **Faces** — UniFFI `libs/fauna-ffi/src/sessions.rs` and a wasm twin: `sessions_load`, `sessions_revoke(token_id)`, `sessions_revoke_others`, `sessions_lockout`, each returning the re-folded snapshot beside the act's error (the custody-face shape), plus `lock_account_with_seed`. No page machine: tui hydrates on the nav edge and re-snapshots after each act (its Devices / Mail / Privacy sub-page shape).
- **App glue** — the render, the two confirm states, and the time formatting on web. Nothing else.

## User actions

| Element | Action | Where it runs |
|---|---|---|
| `session-revoke-button[i]` | Revoke that session; re-fold. | Shared Rust — `SessionsClient::revoke`. |
| `sessions-revoke-others-confirm-button` | Revoke every session but this app's; re-fold. | Shared Rust — `SessionsClient::revoke_others`. |
| `sessions-lockout-button` (gated by `sessions-lockout-confirm-field` = `LOCK`) | Lock the account for 24 h. On success the app leaves the shell for the locked surface. | Shared Rust — `SessionsClient::lockout` (signed in) / `lock_account_with_seed` (the signed-out door). |
| `launch-account-locked-stolen-button` | Open `identity_stolen_entry`. | App glue (navigation); the ceremony is `fauna_client_recovery::succeed_with_held_kit` over an anonymous connection. |

## Persistence

None. Own token ids are in-memory only ([`../behavior/devices.md`](../behavior/devices.md) § The client's own session); the signed-out door persists nothing by design.

## Errors & edge cases

- A failed read or act paints `error-message` and leaves the previously-painted rows in place; the next successful act clears it. An act's error is never dropped (e2e convention 11).
- The list is never legitimately empty for a signed-in app (its own row exists); `sessions-empty` is the fail-safe for a reply that carries none, not a designed state.
- A `minted_by_device` with no roster match is expected (a custodian's key is one), and renders the neutral third `kind`, never an error.
- `ip_address` is `None` for every session today — the WS mint path does not yet record the peer address ([`../behavior/login.md`](../behavior/login.md) § Implementation status today, the dormant new-IP event). The detail line says so plainly rather than hiding the field, so the page gains the address with no app change when the nest starts recording it.
- Driving `sessions-lockout-button` with the wrong word refuses loudly in the action arm.

## Architectural rules

1. One fold, seven renders — no app decides which row is its own, how rows sort, or what a row is called.
2. The ladder's copy is part of the contract: the revoke note and the lockout warning are REQUIRED elements, not decoration, because without them the page promises a remote sign-out the mechanism cannot deliver.
3. Indexed IDs throughout the list (`session-card[i]` and its descendants).

## Don't do these

- Don't offer a lockout duration, anywhere — the window is a Rust constant ([`../behavior/devices.md`](../behavior/devices.md) § Emergency lockout).
- Don't call single-session revoke "sign out this device" — for a device that holds the secret key or a device grant it is not one.
- Don't put a revoke button on this app's own row, and don't render this app's superseded tokens as separate rows.
- Don't build a per-app sessions screen the other six cannot adopt; don't add the list to the Account sub-page.
- Don't join `minted_by_device` to the roster's `device_id` — the join key is `principal`.

## Done definition

- [x] The user's ruling is recorded in § The ruling and `Status:` is ratified; the rail slot landed in [`settings.md`](settings.md) § Navigation model in the same commit (2026-09-25).
- [ ] ui.yaml carries the approved IDs; all seven apps render the page off the shared fold; `ui-actual-<app>.yaml` refreshed. *(ui.yaml's leg-1 IDs and tui's render landed 2026-09-28; the other six apps remain.)*
- [ ] A tier_3 journey drives list → revoke one → sign out everywhere else through the app UI, and a second drives lock → the locked surface → the stolen-identity ceremony → signed in as the successor. *(The first journey is green on tui, 2026-09-28 — `test_sessions_page.py`; the second is `test_locked_surface.py`, tui, 2026-10-04.)*
- [x] `docs/guides/` gains the feature with its first implementation (2026-09-28 — `docs/guides/app-tour.md` § Settings, *Sessions*).

## Reading list

1. `principles.md`.
2. [`../behavior/devices.md`](../behavior/devices.md) § Session Management + § Emergency lockout — read its § Implementation status today first.
3. [`../behavior/identity-succession.md`](../behavior/identity-succession.md) — the other panic button.
4. [`settings.md`](settings.md) § Navigation model; [`devices.md`](devices.md).
5. `libs/fauna-protocol/src/sessions.rs`, `libs/fauna-protocol/src/account.rs` (the wire); `apps/fauna-tui/src/settings/{devices,recovery}.rs` (prior art).
