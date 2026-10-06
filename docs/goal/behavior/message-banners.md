# New-message banners — target state

Owns: message-banners
Status: ratified — split verbatim out of [`../ui/conversations.md`](../ui/conversations.md) on 2026-09-28; the when/for-whom decision was ratified there (the launch floor 2026-09-22, the unread-count key 2026-09-27, user) and is built on all 7 apps, android's e2e witness owed (§ Implementation status today).
Authority: the running app's banner for a newly arrived conversation message — the one shared when/for-whom decision (`MessageNotificationTracker`: seed silently, fire on an unread-count rise past the run's launch floor, suppress the open thread), the rule that those three rules are the whole decision and the firing arm is app glue, the fired-banner e2e witness, and the per-app build record of both halves. Defers what counts as unread, the open-thread notion of attention and the launch floor itself to [`../ui/conversations.md`](../ui/conversations.md) § State & data shape → *When a thread is read*; the read-state carriers to [`conversation-read-state.md`](conversation-read-state.md); the notifications page and the knock toast to [`notifications.md`](notifications.md); push transports, and a closed app's notifications, to [`../architecture/apps/common.md`](../architecture/apps/common.md); tui's firing arm to [`../architecture/apps/tui.md`](../architecture/apps/tui.md) § System integration.

> **Audience:** shared-Rust work on `fauna_conversations::notification`, and every app's firing site.
> **Purpose:** when a running app raises a banner for a conversation message, and what each app does with that decision.

*Split verbatim out of [`../ui/conversations.md`](../ui/conversations.md) on 2026-09-28, when that page doc was 56 bytes under the whole-file read ceiling. The banner is a notification behavior the page's Authority line never listed, and its per-app status row alone had grown to 12 KB in a week. A routing stub remains at each original location; prior history: `git log --follow docs/goal/ui/conversations.md`.*

*Reading this doc. Its text was carried verbatim, so an unqualified `§ <name>` citation may name a section that is not a heading here. `§ Implementation status today` and `§ Where logic lives` mean this doc's sections, which hold exactly the text they were carried from. Every other unqualified name — § State & data shape → *When a thread is read* above all — resolves in [`../ui/conversations.md`](../ui/conversations.md).*

## Section map

- **§ Implementation status today** — the per-app build record of the banner, one table row carried from the page's frontier table.
- **§ Where logic lives** — the shared decision (*New-message OS-toast decision*) and the app's half (*OS notifications — the firing only*).

## Implementation status today

| Surface | web | linux | windows | macos | ios | android | tui |
|---|---|---|---|---|---|---|---|
| DM OS-toast firing (shared `MessageNotificationTracker`) | ✅ 2026-09-20 — the running tab raises a real `Notification`. `$lib/message-banner` takes the whole decision from the shared tracker over one new wasm face (`WasmConversationsManager::newMessageBanners`, which owns the tracker so an actor switch rebuilds it and the incoming identity's threads seed silently) and fires from `refreshConversations`, the SPA's post-change chokepoint. The **service worker's `showNotification` covers only the CLOSED app** — the two paths never overlap. Deliberately **not** gated on `Notification.permission`: the browser already decides (the constructor is a silent no-op when permission is `denied` or `default`, and raises no prompt), so reading it first would be a fourth when/for-whom rule in glue and would key the fired log on a platform setting rather than on what the app did. Also **not** the push *subscribed* bit — [`../ui/settings.md`](../ui/settings.md) § Push notifications keeps those apart, and a user who disabled background push while sitting in front of the app still wants the banner. `test_conversations_message_banner.py` green `--app web` | ✅ **e2e-witnessed 2026-09-20** — `test_conversations_message_banner.py` green `--app linux`, the first witness outcome 11 has had on any column. The witness is the shared fired-banner log (`fauna_e2e_agent::MESSAGE_BANNERS_KEY`, recorded in `fauna_conversations::notification` and published by each app's state builder): the app appends every banner it hands to the platform at the FIRING site, so the log means "the user was shown this" rather than "the tracker returned this", and the OS notification centre — the last inch — is never read. Writing it found two bugs the built cell had been hiding. **(1)** The app-lifetime observer that drives both the toasts and the tray unread badge was attached once at startup and dropped by every `manager().clear_observers()` (sign-out, account switch, failed-trust relaunch, agent reset) with nothing re-attaching it, so from the first sign-out onward the process fired no DM toast and froze its badge — the loop now re-attaches on a closed channel, with a fresh tracker so the incoming identity's restored threads seed silently instead of toasting one apiece. **(2)** `notify_message` also gated on `should_show_notifications()` (`!WINDOW_FOCUSED`) — a FOURTH when/for-whom rule, in app glue, that this section does not list and windows does not apply, falsifying the outcome whenever the app was focused on another page. Removed from the message banner only; the knock/sync/unified notifiers keep it, no ratified outcome covering them | ✅ **e2e-witnessed 2026-09-21** — `test_conversations_message_banner.py` green `--app windows`. `MessageToastObserver` holds the shared `MessageNotificationTracker` and applies no rule of its own (it was the reference when linux's window-focus rule was removed). The recorders are `test-helpers` UniFFI exports keyed on the feature alone, so every call site sits behind the shell's `DEBUG`-or-`FAUNA_E2E_AGENT` compile gate — the production FFI flavor has no such symbol. `NotificationService.ShowMessageNotification` now returns whether `AppNotificationManager.Show` actually ran, a fire is recorded only on `true`, and an ungated `[banner] not raised` warn says so otherwise (apple's shape); the log is published as top-level `message_banners` from `SerializeState`. **Writing the witness found the e2e login path never wired the feature at all**: production attaches the observer and calls `NotificationService.Initialize()` in `StartMainAppAsync`, which the `set_state` e2e login never reaches, so the e2e app fired nothing and no banner tick ever ran. `BuildE2eConvSessionAsync` — the seam both e2e logins share — now does both, through the same null-guarded `AttachMessageToastObserver` the production block calls, handing it the window's `DispatcherQueue`: that builder runs off the UI thread, and an observer built with no queue diffs inline on the mutator's thread. `AppNotificationManager.Register()` succeeds in the unpackaged e2e launch, so windows needs no launch-shape carve-out (unlike macOS). Mutation-proved: passing `null` for the selected thread reds the open-thread assertion | ✅ **e2e-witnessed 2026-09-21 — in artifact mode**: `test_conversations_message_banner.py` green `--app macos --macos-artifact`, over the same FaunaKit code the ios cell describes (one shared `MessageBannerObserver`, one `ConversationsVM` tick, one state key). **Naming the mode is the point, not a footnote** — an ordinary `--app macos` run launches the bare `FaunaMacOS` binary on purpose (a fixed-bundle-id `.app` wedges WindowServer; `tests/e2e-unified/drivers/macos.py`), and a non-bundle process has no notification host at all (`NotificationHost.isAvailable`), so the app hands no banner to the platform and honestly records none. The module therefore reads the launch shape back off the driver (`bundle_path()`, `None` exactly for a bare-binary launch), asserting the column under the replay and *declaring* it short on the default path with the replay command in the skip reason — never redding the ordinary run, which is most of them. Recording *around* the host guard was considered and rejected: an entry in the fired-banner log means **a banner was raised**, and blurring that would let a real firing regression hide behind a build-shape quirk — the glue warns in the app's own log instead, which the module's `_why` diagnoser now folds into the failure. No per-module launch shape was invented for this: the replay is the run-level one the harness already sanctions ([`../architecture/apps/apple-e2e-automation.md`](../architecture/apps/apple-e2e-automation.md) § Artifact launch mode), so the default stays bare-binary for every other apple test. Headless cover on the default path: `MessageBannerObserverTests` (`just swift-test`) pins the snapshot projection, the seeding silence and the identity-change tracker reset | ✅ **e2e-witnessed 2026-09-21** — `test_conversations_message_banner.py` green `--app ios`, over the same FaunaKit code macOS runs. `MessageBannerObserver` holds the shared tracker and is ticked from `ConversationsVM.onManagerChanged`: the app-lifetime manager observer, already hopped onto the main actor by `notifyOnMainActor` — which is exactly the marshalling windows arranges by hand through its `DispatcherQueue`, so apple needs no second observer (`observer_count` stays flat) and adds no fourth rule. The tracker is **rebuilt in `deactivate()`**, which `ActorScope.dropAppOwnedState` runs at every switch / sign-out: `clearForIdentityChange()` preserves observers and the tracker never re-seeds itself, so a carried seed would toast once per restored thread — linux's 2026-09-20 fix in apple's idiom. A fire is recorded only when `postMessageNotification` actually reached `UNUserNotificationCenter` (it now returns whether it did), and the log is published as top-level `message_banners` from `AppStateObservables.commonState`. The three recorders + the JSON face were on no UniFFI surface before this (linux links the crate, web goes through wasm), so they are exported from `fauna_conversations::notification` keyed on `feature = "test-helpers"` alone — `../architecture/e2e-automation-surface-gating.md` § The convention | ✅ **2026-09-22 — built, compile- and Robolectric-verified; the e2e run is still owed.** `MessageBannerObserver` (`core/conversations/`, apple's name and shape) holds the shared tracker and is ticked from `ConversationsManagerHost`'s app-lifetime `SnapshotObserver` — the one that already feeds the Compose snapshot — so it sees the bare pre-login manager (the e2e mock target) and every session's manager alike, and adds no observer and no fourth rule. `diff` gets the snapshot's own `selectedThreadId` and `launchFloorMs`. Ticks are serialised under one lock: the observer fires on whichever Rust thread mutated the manager, and an older snapshot diffed second would roll a thread's stamp back and fire it twice (windows takes the same lock for the same reason). The tracker is **rebuilt in `stopConversationsSession`**, which every sign-out and account switch runs. `NotificationHelper.postMessageNotification` now returns whether `NotificationManagerCompat.notify` ran; a fire is recorded only on `true`, and an ungated `[banner] not raised` warn says so otherwise. **Not gated on the POST_NOTIFICATIONS grant**, for web's reason: on API 33+ the platform drops a disallowed notification itself and raises no prompt (the ask stays the shell's launch effect, never a message's arrival), so reading the grant first would be a fourth rule in glue. The three recorders are reached through `TestAgent`'s same-signature twins — real in `src/debug`, no-ops in `src/noAgent` — since the shipping bindings carry no `test-helpers` export; the log is published as top-level `message_banners` from `serializeState`. `MessageBannerObserverTest` (`just android-host-test`) pins the seeding silence, one banner per new thread logged under its label, a refused fire kept out of the log, the open-thread suppression over the snapshot's selection, a real notification reaching the platform through the host, and the sign-out reseed — the suppression and the reseed mutation-proved. `test_conversations_message_banner.py` carries android; **a recorded run is still open** — android has no run venue yet ([`../architecture/testing.md`](../architecture/testing.md) § Default app and nest mode → *Android's run venue*) | ✅ **2026-09-20 — the OSC arm, e2e-witnessed.** `conversations::fire_message_banners` feeds every `ConversationsChanged` tick to the shared tracker and `os_notify::notify_message` writes the banner as an OSC 9 escape (OSC 777 on the terminals that speak it and not 9 — one dialect per banner, never both, since a terminal understanding both would pop two toasts for one message). The escape is forwarded by the terminal emulator, so it reaches the *user's* desktop even when fauna-tui runs on a remote box over SSH — the arm no other app can offer, and it needs no dependency, which is why it shipped first rather than last. **The escape's payload is sanitised as a security boundary, not for tidiness**: a message body is remote-authored text entering the terminal's *control* channel, so `os_notify::sanitize` strips every control character before it enters the sequence — a surviving `\x1b`/`\x07` would close the OSC string early and leave the sender's remaining bytes being parsed as fresh control sequences. Unit-pinned against a hostile body and a hostile thread label. A **fresh tracker per identity** comes free here — `ConversationsState` is replaced at both ends of an identity's life (`session::establish`, `App`'s identity teardown) — so tui has no surviving state to carry a seed across an account switch and no re-attach path to forget, which is the bug linux had to fix the same day. **The desktop arm is built (2026-09-26):** a present desktop session (`fauna_credential_store::keyring_probe`, the probe tui already trusts for credential-backend selection, asked once per process) selects a `notify-rust` notification instead of the escape — **selected, never stacked**, with a failed desktop notification degrading to the escape for that banner; unit-pinned by `os_notify`'s exclusivity tests. The fired-banner log is recorded at the firing site above the arm, so the witness below covers whichever arm fired. Windows' constant-true probe means the desktop arm always wins there, SSH included — a known gap recorded in [`../architecture/apps/tui.md`](../architecture/apps/tui.md) § System integration. **The banner takes no gate from the push opt-in toggle**, for the reason web's cell gives and under the same rule (§ Where logic lives: the three rules are the whole decision, so the opt-in bit would be a fourth): the toggle governs what reaches the device while tui is closed, never what the running app raises — so tui's install-scoped notifications choice is that toggle's intent bit ([`../ui/settings.md`](../ui/settings.md) § Push notifications), and tui grows no separate banner switch. `test_conversations_message_banner.py` green `--app tui` |

## Where logic lives

**Shared Rust (`libs/fauna-conversations`):**

- **New-message OS-toast decision** — `MessageNotificationTracker`
  (`notification.rs`): the stateful *when / for-whom* diff over successive
  snapshots (seed silently on the first **non-empty** snapshot — pre-existing
  threads at login aren't "new"; fire when a thread's `unread_count` **rises
  while its newest activity, `last_activity_ms`, is at or past the run's launch
  floor** — whether the thread was seen before or is first seen now with unread
  messages; suppress the selected/focused thread). Pure +
  deterministic, so it lives once here instead of being re-derived per app
  (priority #2/#4) — only the native toast *firing* is app glue (see below).
  **The floor in rule 2 (ratified 2026-09-22) is the unread rule's** — the same
  `launch_floor_ms` the store stamps and every snapshot now publishes (§ State &
  data shape → *When a thread is read*), passed to `diff` straight off the
  snapshot so the banner and `dm-unread-indicator` can never disagree about
  where news starts. It exists because the rails load asynchronously and the
  mail rail re-drains the whole mailbox from UID 0 at every launch, record by
  record ([`../behavior/mail-app-surface.md`](../behavior/mail-app-surface.md)
  § Inbound client receive): seeding on the first non-empty snapshot seeds on
  whichever rail lands first, and every mail thread the drain reached afterwards
  read as brand-new — a sign-in raised a banner for days-old mail whenever mail
  was the slower rail (found by the 2026-09-22 whole-suite linux sweep, once the
  shared e2e account's seeded mail first opened). A rise that stays below the
  floor is history paging in, not news. So **a message that arrived while the
  app was closed raises no banner, whichever snapshot delivers it** — the banner
  is for what arrives while the app runs; what arrived while it was away is the
  unread indicator's to carry, with that section's declared gaps — and the
  stamp comparison inherits that section's under-reports (a device clock
  running ahead; a sender-backdated stamp on a rail that carries the sender's
  own time — and on the banner that under-report lasts the whole run on every
  app, since only the unread read retires: the floor is compared with the
  thread's newest stamp, and the store's newest is its last-appended message).
  **The key is the unread count, not the stamp (user-ratified 2026-09-27):** the count
  follows carriers the sender does not choose (the nest-assigned `seq`, mail's
  `\Seen` — [`../behavior/conversation-read-state.md`](../behavior/conversation-read-state.md)
  § How the carriers meet the in-memory set) and never counts the user's own
  messages, so a live message stamped below its thread's newest — backdated by
  its sender, or merely from a sender whose clock runs behind the previous
  one's — banners exactly when the indicator takes it, and a message the user
  sent from another device banners nowhere. It carries two declared edges of
  its own, both pinned in `notification.rs`'s docs: a mail client marking an
  old message unread, on a thread whose newest is past the floor, raises the
  count and so a banner; and a read from another device landing in the same
  snapshot tick as an arrival can leave the count where it stood, and so raise
  none. The floor's *unread* read leaves with synced read positions
  ([`../behavior/conversation-read-state.md`](../behavior/conversation-read-state.md));
  the banner's stays — "did this arrive while the app was running" has a
  run-start answer whatever read state knows. Pinned in `notification.rs`
  (a thread first seen after the seed with pre-floor activity, and a rise that
  stays below the floor, both silent; a rise across it, a banner; an arrival
  stamped below its thread's newest, a banner; an own message, none).
  Every app projects a snapshot thread into the tracker's input through the
  one shared `ThreadActivity::from_summary` (over UniFFI,
  `thread_activity_from_summary`), never a per-app constructor.
  Native apps hold the UniFFI object (`#[uniffi::export]` gated behind
  `client-display`, dropped from the Go bridge build); linux holds it as a Rust
  dep. **The three rules above are the WHOLE decision** — an app that adds a
  fourth of its own (linux suppressed on window focus until 2026-09-20) is
  diverging, not refining, and falsifies the promise the catalog records.
  *Status:* all 7 apps consume it (windows and linux
  deleted their client-side twins; web reaches it over the `newMessageBanners`
  wasm face, which owns the tracker so an actor switch reseeds it; macos + ios
  share one FaunaKit `MessageBannerObserver`, which rebuilds the tracker at the
  identity change for that same reason, and android's same-named observer
  rebuilds its own at sign-out; § Implementation status today). Each app records the
  banners it FIRES on its e2e state (`fauna_e2e_agent::MESSAGE_BANNERS_KEY`,
  derived by `fauna_conversations::notification::message_banners_json` — an app
  whose firing site is not Rust reaches the same recorders over UniFFI; web
  keeps the same shape in TS, since its firing is TS) — the only observable the
  decision has, since no test can read an OS notification centre; green on
  linux, tui, web, ios, windows and macOS (in artifact mode — its bare-binary
  launch has no notification host), owed by android, whose leg is built and
  whose e2e run waits on android's run venue (§ Implementation status
  today). **What the firing arm may be is the app's business; whether to fire is
  not** — tui reaches the user with an OSC terminal escape rather than a
  desktop-notification API ([`../architecture/apps/tui.md`](../architecture/apps/tui.md)
  § System integration), and it satisfies this section identically, because the
  three rules are the whole decision and the arm below them is glue.

**App glue:**

- OS notifications — the *firing* only: feed each snapshot diff to the shared
  `MessageNotificationTracker` (Shared-Rust list above) and trigger the platform's
  native toast API for each thread it returns. The *when/for-whom* decision is no
  longer per-app glue.
