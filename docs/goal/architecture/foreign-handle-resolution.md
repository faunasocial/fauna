# Foreign handle resolution — target state

Owns: foreign-handle-resolution
Status: ratified — split verbatim out of [`federation.md`](federation.md) on 2026-09-28. The rule was ratified 2026-08-29; every later ruling and build below carries its own date.
Authority: **what a client concludes when it resolves a typed `localpart@domain` against a domain that is not its home nest's** — the anonymous `by_handle` hop's two answers and one non-answer; the closed disowning set, enumerated by wire code; the dial rule (the canonical handle is built from the dialed domain, never from the reply's echo) and what a peer's answer may and may not claim; the known-Fauna-domain evidence and its three states (known, loaded and absent, not loaded yet); the non-signals rejected on the record; and the build record of all of it. Defers: nest-key discovery, the discovery trust rule (which `nest.info` answer a nest believes) and the rest of the nest↔nest wire layer to [`federation.md`](federation.md); what the recipient picker shows and refuses to [`../ui/conversations.md`](../ui/conversations.md) § Errors & edge cases; the replica restore and durability rules the evidence rests on to [`../behavior/devices.md`](../behavior/devices.md); the nickname paint gate to [`../ui/contacts.md`](../ui/contacts.md) § The private overlay.

Last verified: 2026-09-28 (the split; every ruling date, pin and residual below is its entry's own, carried across unedited) | Sources: `libs/fauna-client-conversations/src/lib.rs` (`remote_by_handle_outcome`, `BY_HANDLE_DISOWNING_CODES`), `libs/fauna-conversations/src/backend.rs` (`classify_foreign_non_answer`, `DomainEvidence`), `libs/fauna-conversations/src/backends/fauna_mls.rs` (`resolve_foreign`), `libs/fauna-client-mls-sync/src/orchestration.rs` (`restore_and_wire`), `libs/fauna-protocol/src/error.rs` (`RpcError::action`)

> **Audience:** the shared-Rust conversation rails every app runs, and anyone touching the recipient picker's resolve, the launch restore, or the wire-error classifier.
> **Purpose:** the rule that stops a failed or hostile foreign lookup from producing a less confidential send than the one the user asked for — a known Fauna peer downgraded to plaintext email — or a participant labelled with a domain nobody vouched for.

*Split verbatim out of [`federation.md`](federation.md) on 2026-09-28. The ruling and its build record had become that doc's fastest-growing part, while they are decided in the shared client rails rather than on the nest↔nest wire the rest of that doc specifies. A stub remains at each original location; prior history: `git log --follow docs/goal/architecture/federation.md`.*

*Reading this doc. Its text was carried verbatim under its original headings, so an unqualified `§ <name>` citation may name a section that is no longer a sibling on the page. In the moved text an unqualified `§ Peer-auth model` (and its lead-ins *Discovery-failure semantics*, *The dial names the peer*, *What a peer's answer may and may not claim*) means this doc's, as does `§ Implementation status today`. The ruling's "this section's first bullet" is [`federation.md`](federation.md) § Peer-auth model → *Nest-key discovery*, the bullet that requires discovery to run over authenticated TLS. Every other unqualified name — § Security and its *TLS is the floor*, and the rest — resolves in [`federation.md`](federation.md).*

## Section map

| Section | What it holds |
|---|---|
| § Peer-auth model → *Discovery-failure semantics* | The ruling, moved verbatim: a nest answered (the closed disowning set, the dial names the peer, what a peer's answer may and may not claim), no nest answered (the known-domain evidence and its three states), the non-signals, and the same-nest sibling. |
| § Implementation status today | The build record, moved verbatim: discovery-failure semantics with its three hardenings and the price of closing the cold-start window, the dial rule, and what a spoofed label could reach. |

---

## Peer-auth model

*Only the bullet below moved here; the section's other bullets — identity, signing, nest-key discovery with the discovery trust rule, and replay protection — stay in [`federation.md`](federation.md) § Peer-auth model.*

- **Discovery-failure semantics (ratified 2026-08-29).** The client's anonymous
  `by_handle` hop against a *foreign* domain — the recipient picker resolving a
  typed `localpart@domain` whose domain is not the home nest's — has two answers
  and one non-answer, and the rail-resolution chain treats them differently:
  1. **A nest answered.** Its reply is authoritative for that domain: found +
     `addressable` → a Fauna recipient; a reply that **disowns** the handle or
     the domain → *not a Fauna recipient at this domain*, and the chain falls
     through to the email rail. The seam carries this structurally
     (`ConversationsRpc::actor_by_handle_remote`: `Ok(Some)` / `Ok(None)` /
     `Err(Rejected)`), never as a transport fault. A version-incompatible nest
     (`NeedsUpdate`) is a Fauna nest the client cannot talk to → an error, never
     email.

     **The disowning set is CLOSED, and enumerated by wire code**
     (`fauna.actor.not_found`, not addressable, `fauna.actor.domain_not_local`,
     `fauna.handle.invalid` — the complete set `discovery_handlers` can refuse a
     `by_handle` with). This is the ruling's load-bearing half, not a detail:
     the client must decide case 1 vs. case 2 from an **allowlist** of codes it
     recognises as a refusal, never from "the classifier did not call it
     transient". `RpcError::action()`'s `Rejected` verdict is its **default**
     arm and therefore an *open* set containing every code nobody has classified
     yet — so reading it as case 1 silently enrolls each future refusal code,
     and every classifier gap, into "not a Fauna recipient". **An open-ended
     error class is never an input to a downgrade decision.** Anything outside
     the allowlist is a *non-answer* and belongs to case 2, where positive
     evidence decides. (This is why the ruling shipped with a hole: a peer
     throttling the probe answers `fauna.protocol.rate_limited`, which was
     unclassified, hence `Rejected`, hence read as a disowning — downgrading a
     known Fauna peer to plaintext SMTP. Both halves are now fixed: the
     `rate_limited` family classifies `Transient`, *and* the seam allowlists.)

     **The dial names the peer (ratified 2026-09-22).** An answer carries two
     identity fields — the `actor_id` the client asked for, and a `domain` the
     answering nest names for **itself** — and only one of them is bound to
     anything. Discovery ran over authenticated TLS to the domain the user
     typed, so the certificate binds *that* domain to the nest that answered
     (this section's first bullet); the echoed domain is the answerer's own
     assertion about itself, vouched for by nobody. **The canonical
     `localpart@domain` the client mints, stores on the thread and renders in
     all 7 apps is therefore always built from the DIALED domain, and the
     reply's domain field is not read for identity.** Honouring it let a nest
     serving `attacker.test` answer with a domain of `trusted.test` and mint a
     participant every app displays as `bob@trusted.test`. The data plane
     derives its route from that same string, so the rule also keys the peer's
     key-package fetch on the TLS-verified domain rather than an
     attacker-chosen one.

     **The rule is "the dial wins", NOT "refuse a reply that disagrees".** The
     hop sends no `domain` qualifier, so a legitimate multi-domain nest reached
     at a secondary domain answers with its **primary** identity domain
     ([`../behavior/mail-multidomain.md`](../behavior/mail-multidomain.md)
     § Resolution and login report the live identity domain) — an honest reply
     that differs from the dial, which a strict equality check would reject,
     breaking `bob@domain2`. Taking the dial needs no wire change and is the
     *better* answer for that peer too: the user gets back the domain they
     typed instead of a silent rewrite to the peer's primary.

     **What a peer's answer may and may not claim.** Its reply is authoritative
     for the dialed domain and nothing else: it asserts "there is an addressable
     actor at `localpart@<dialed>`, and here is the id to address them by". It
     is **not** a claim that the named actor id belongs to, or is vouched for
     by, any other domain — and the client cannot check that at resolve time,
     because a Fauna `ActorId` *is* a self-minted Ed25519 public key with no
     issuing authority to verify it against. The one cryptographic bind on an
     actor id is `credential == leaf signature key`, enforced at every group
     admission ([`../ui/conversations.md`](../ui/conversations.md)
     § Reactions & message delete → *Leaf-credential binding*), and the residual that
     ruling rests on — the delivery service is trusted to serve the honest
     KeyPackage for the requested actor id — stands **ratified and is NOT
     re-opened here**. The dial rule narrows it rather than re-litigating it:
     the delivery service now trusted for a peer's key material is the nest at
     the domain the user actually typed. What the unbound actor id may *reach*
     is bounded on the consumer side: the private contact overlay paints a
     nickname on a participant only once that id is a verified leaf of the
     thread's group, never on the peer's bare claim
     ([`../ui/contacts.md`](../ui/contacts.md) § The private overlay → *The
     paint gate* owns the rule).
  2. **No nest answered** (`Err(Transient)`: DNS failure, connect refused, TLS or
     WS handshake failure, timeout, protocol fault, a transient wire refusal
     such as a rate limit, **or any refusal outside case 1's allowlist**).
     **The transport-error kind is deliberately NOT
     consulted**: the browser reports a failed `WebSocket` opaquely (a dead port,
     a rejected certificate and a non-Fauna web server are one event), so a rule
     reading it would be native-only — forbidden by priority #1. The client
     decides by **positive evidence it holds identically on every arm**: the
     domain is a **known Fauna domain** iff (a) it is the local actor's own
     handle domain, (b) a `TypedAddress::Fauna` participant of any thread in this
     account's conversations carries that domain, or (c) a nest at that domain
     answered a resolve earlier in this session. **Known → the resolve is an
     error** ("Lookup failed — try again"), and the answer is terminal — no
     later rail may claim the address, the picker commits no chip, nothing is
     sent on any rail. **Unknown → the chain falls through to email**, exactly as
     for a domain with no nest at all: first contact with an unreachable,
     unadvertised nest resolves as email by design — the user holds no Fauna
     expectation yet and the chip shows the email rail — and the moment any
     Fauna thread with that domain exists, the domain is known.

     **"Unknown" is a verdict the client must EARN, and there are three states,
     not two.** Clause (b) is scoped to the *account's* conversations, but the
     conversations a client can read are the ones it has **loaded**, and that
     history lives on the home nest — restored over the network at launch. So
     between launch and restore, an account that has conversed with a peer for
     months holds nothing that names it, and reading that empty evidence as
     "unknown" downgrades exactly the peer this rule exists to protect. The
     carve-out is justified by the user holding **no Fauna expectation**, which
     an unread store cannot establish. Therefore: **known → error; loaded and
     absent → email; not loaded yet → error**, the same terminal
     "Lookup failed — try again" as a known peer. The email arm rests on a
     positively *established* absence, never on a merely empty one. The window
     closes, per launch, when the restore actually **carries** that account's
     evidence — not merely when it runs (hardened 2026-08-30; see
     § Implementation status today for the two ways a restore establishes the
     answer and the residuals that remain).
  3. **Non-signals, rejected on the record.** `_fauna._tcp` SRV presence: the
     standard Fauna DNS record set publishes no SRV
     ([`../behavior/mail-multidomain.md`](../behavior/mail-multidomain.md)
     § Per-domain DNS records — the apex `A` and the `:443` fallback cover it), so
     absence says nothing. A health probe (`GET /api/v1/health`): a browser cannot
     read a foreign nest's reply cross-origin (the nest's CORS allow-list is an
     admin choice per origin), so it would be native-only. A home-nest-mediated
     probe kind: not needed for the rule above and a new nest surface; it is the
     named extension should first-contact downgrades ever prove to matter.

  The same-nest sibling (`actor_by_handle` / `keypackage.count` on the home nest)
  already treated a transport fault as an error, because the home domain is
  always known; the two hops now agree. One refinement: a same-nest transport
  fault on a typed *foreign* domain still runs the foreign probe (the peer may be
  up while the home nest hiccups) rather than erroring outright. The security
  framing is § Security's *TLS is the floor*: a hop that fails must never
  produce a *less* confidential send than the one the user asked for. The UX
  arm — what the picker shows and refuses — is owned by
  [`../ui/conversations.md`](../ui/conversations.md) § Errors & edge cases
  (*The picker tells the truth*).

## Implementation status today

*Only the entries below moved here; the rest of the ledger stays in [`federation.md`](federation.md) § Implementation status today.*

- **Discovery-failure semantics (§ Peer-auth model) — BUILT 2026-08-29, shared
  Rust only, zero app edits:** the seam's `remote_by_handle_outcome` (both arms,
  `libs/fauna-client-conversations`) makes the hop's outcome structural;
  `FaunaMlsBackend::resolve_foreign` decides a non-answer by the known-domain
  evidence (`known_domains`: self domain + `RailBackend::observe_participants`
  harvest + answered resolves); `ConversationsManager::probe_address` makes a
  rail's `Error` terminal and shows every rail the thread participants before
  probing; the picker's sync input state is `Resolving` and
  `accept_current_recipient_chip` commits only a probed address. Pinned by
  `fauna_mls_backend_tests.rs` (`resolve_address_foreign_transport_fault_*`,
  `resolve_address_home_nest_down_*`), `manager_integration_tests.rs`
  (`resolve_recipient_rail_error_is_terminal_over_smtp`,
  `recipient_input_is_resolving_until_probed_and_enter_waits_for_the_probe`),
  the seam's `remote_by_handle_outcome_is_structural`, and the tier_3
  `test_fauna_mls_cross_nest_roundtrip.py` pair
  (`test_known_peer_unreachable_never_downgrades_to_email`,
  `test_first_contact_with_unreachable_authority_is_email`).
  **Hardened 2026-08-29 (the closed disowning set, case 1).** As first shipped,
  the client read case 1 off `RpcError::action()`'s `Rejected` verdict — its
  open default arm — so a peer nest throttling the anonymous probe
  (`fauna.protocol.rate_limited`, then unclassified) read as a disowning and
  downgraded a known Fauna peer to plaintext SMTP. Both halves now hold: the
  whole `rate_limited` **family** classifies `Transient` in `RpcError::action()`
  (`fauna.email.rate_limited` carved out — it also carries a per-day submission
  quota — and `localized_family` moved with it, per that function's own rule),
  and `remote_by_handle_outcome` allowlists the disowning codes
  (`BY_HANDLE_DISOWNING_CODES`), handing every other refusal to case 2 so an
  unrecognised *future* code cannot reopen the hole. `resolve_foreign`'s three
  arms are now the one named rule `backend::classify_foreign_non_answer`, which
  is what lets a single test span the classifier: the seam's
  `real_wire_codes_decide_discovery_end_to_end` drives real wire codes from
  `RpcError::action()` through the seam into that rule (the suites on either
  side of the classifier previously joined nowhere, which is why the gap
  survived), red-verified against both halves reverted.
  **Hardened 2026-08-30 (the cold-start window).** The evidence clause (b) is
  account-scoped, but the set the rail consults
  (`FaunaMlsBackend::known_domains`) is per-process and rebuilt from the thread
  store on every probe — and that store is empty until
  `fauna_client_mls_sync::orchestration::restore_and_wire` has fetched this
  account's history from the home nest. Every launch therefore opened a window
  in which a peer of months' standing was "unknown", and a peer nest that did
  not answer inside it fell through to an email chip and a send in the clear
  (the sharp case: home nest reachable, restore not yet complete, peer nest
  unreachable). `classify_foreign_non_answer`'s second input is now the
  three-state `backend::DomainEvidence` — `KnownFauna` /
  `AbsentFromLoadedEvidence` / `Unloaded` — and only an absence the client
  *established* opens the email arm; `restore_and_wire` calls
  `FaunaMlsBackend::mark_conversations_loaded` immediately after restoring the
  history slices, the one funnel all four legs (FFI-native, tui, linux, web)
  reach the plane through. ⚠ **And it calls it only when that restore CARRIED
  the account's evidence (hardened again 2026-08-30, same day).** The first pass
  marked unconditionally, which re-opened the window it had just closed and
  disguised it as shut: `MlsStateSync::load` answers `Ok` with `provider: None`
  for an account that has no replica blob yet, and the history loop lives inside
  that `Some` — so the mark fired over an **empty** `ThreadStore` and every
  foreign domain reported a positively *established* absence. The replica **is**
  the account's channel list (this protocol has no "list my channels" call, and
  `ThreadStore` is in-memory, rebuilt each launch), so without it the
  conversations were never loaded and `Unloaded` is the honest verdict.
  ⚠ **And the channel list is not the evidence either (hardened again
  2026-08-30, third pass).** Nothing downstream reads the provider; what the rail
  spends is `known_domains`, harvested from thread-store **participants**, and
  participants live only in the `history/<ch>` slices — a separate blob behind a
  separate fetch that `load()` tolerates missing on purpose. So `provider: Some`
  carrying no slices marked the account loaded over an empty `ThreadStore` just
  as surely as `provider: None` did. **No fault is required to reach it:** the
  commit gate's `save_provider_snapshot` CAS-puts the provider *alone* by design
  (crash-safety steps 2 and 4), so between a first send on a new channel and the
  debounce that writes its slice the durable state simply **is** `{provider lists
  X, no history/X}` — which a second device launching in that window reads
  verbatim, an ordinary multi-device race with no crash anywhere. The mark now
  additionally requires that **every *chat* channel the provider lists arrived
  with its slice** — the listed channels carrying the durable chat marker in
  the replica's own bytes (`ProviderReplica::is_channel_chat`; the marker is
  `devices.md`'s) against the slices `load()` pushed, one per listed channel
  that had a stored blob. Keyed on the marker, not a bare count (corrected
  2026-09-22): a scheduling or folder channel is an engine group that never
  gets a slice by design, and counted it held the account at `Unloaded` for
  ever — the *price* paragraph below. Not the engine's own members
  instead: `MlsEngine::group_members` answers `ActorId`s and a domain needs a
  handle, which is not on that path today. Two ways
  to establish the answer, and the second is not optional: a replica was read,
  **or** the engine holds no groups at all — an account with nothing to load has
  no replica either, and reading that as "not loaded" would deny the email
  carve-out forever to exactly the user it exists for, one holding no Fauna
  expectation. **Declared residuals**, all narrower than the window they
  replace and all stated rather than papered over: a channel created or
  welcomed since the last replica save is absent from a *sibling* device's
  restore until the targeted import delivers it (`devices.md` § Cross-device
  MLS group-state sync → *A sibling-joined group is adopted mid-session by a
  targeted import* — not "by Welcome", as this line claimed until 2026-09-22:
  an inbox Welcome is acked once drained and is never offered to an
  established member again), and on the *joining* device it is now durable
  before the join returns (the Welcome join's own provider put, and the
  launch swap's carry of the native store's copy — *A group the engine holds
  but the snapshot does not list survives the swap*), leaving a hard crash
  between the join and its awaited put as the crash-shaped remainder, on web
  without a native copy to fall back on; and a
  browser's first launch of an existing account has neither replica nor durable
  engine groups, which reads as established-empty — inherent, because on that
  platform the replica is the *only* evidence there is. A **third** route — a
  replica listing channels whose slices did not arrive — was open until the third
  pass above and is now closed rather than declared.
  **The price of closing it, and how it was paid (2026-09-22).** Requiring
  every listed channel's slice makes a listed-but-slice-less channel hold the
  whole account at `Unloaded`, and it does **not** self-heal: `snapshot_replica`
  gathers slices from **bound** channels only, a channel restored without a
  slice has no thread, and `poll_inbound_conv` early-returns on an unbound
  channel — so no message that lands on it ever binds a thread or writes a
  slice, and the pairing re-writes itself for ever (the slice IS the thread at
  restore: `devices.md` § Durability rules, Rule 3). Worse, the bare count read
  *every* listed channel as owing a slice, while a scheduling or folder channel
  is an engine group that never gets one by design — so an account holding one
  such channel was denied every email recipient permanently (the 2026-09-22
  whole-suite linux sweep's eight recipient-lookup reds: `listed=N
  carried_slices=N-1` on 53 consecutive launches, the one missing channel never
  bound). `Unloaded` is the *safe* direction — a terminal "Lookup failed — try
  again", never a send in the clear, and § Security's *TLS is the floor* ranks
  the two — but a permanent denial is a real harm, the same one the still-open
  permanent-restore-failure finding is about, one door over. Paid in two
  halves, neither of which weakens the predicate. **Reader:** it is keyed on
  the durable chat marker the replica's own bytes carry — a listed channel
  carrying the marker must have arrived with its slice, a thread-less channel
  counts for nothing, and the launch log names the missing chat channels by
  hex. **Producer:** making the chat-side pairing `{chat-marked X, no
  history/X}` unrepresentable is the job of the device that HAS the thread — a
  device restoring it cannot manufacture X's participants, and an empty slice
  from it would re-open exactly the established-absence hole this predicate
  closes — so the bootstrap and the chat Welcome-join both persist the
  still-empty slice before returning (Rule 3), ahead of any provider put that
  could list the channel. Rule 2's ordering is untouched: the debounced save
  still writes history first and the provider after, and the predicate only
  reads; the commit gate's provider-only put remains the one writer that can
  list a channel ahead of its slice, which the join-side persist narrows to a
  transient persist failure inside the debounce window — a declared residual,
  healed by the debounce's retry on the producing device. A listed channel
  with neither marker nor slice (a pre-marker legacy chat channel, or a folder
  group) reads as thread-less: the reader-side declared residual, since such a
  chat channel could never bind again either way. Persisting the evidence
  locally was the rejected alternative: there is no local conversation store to
  put it in, and a plaintext on-disk list of the domains a user converses with
  is at-rest metadata the sealed-at-rest posture does not have today. The two
  halves are pinned in `orchestration.rs` by
  `a_thread_less_listed_channel_does_not_hold_the_account_unloaded` (a
  folder-shaped and a scheduling group beside one chat channel with its slice:
  loaded, the slice's peer domain known) and
  `a_welcome_join_persists_its_slice_before_the_provider_can_list_it`. Red-verified by
  `fauna_mls_backend_tests.rs::resolve_address_foreign_non_answer_before_restore_never_downgrades_to_email`
  (a cold-start dual-rail session: `Error` before the mark, and the ruling's
  first-contact email arm intact after it), with the `Unloaded` verdict pinned
  end-to-end in `real_wire_codes_decide_discovery_end_to_end`. The list-is-not-the-evidence half has its own launch-seam pin,
  `orchestration.rs`'s
  `a_restore_that_carried_the_channel_list_but_no_slices_does_not_establish_absence`
  — a provider published from an engine holding a real group with a peer, no
  slice saved, then a real `restore_and_wire` on that replica; it asserts the
  restore brought a group (or it would pass vacuously as its sibling's case) and
  that the verdict is still `Unloaded`. The carried-vs-ran
  distinction is pinned at the launch seam itself by `orchestration.rs`'s
  `a_restore_that_carried_no_replica_does_not_establish_absence`, which drives
  the real `restore_and_wire` over an empty replica and asserts both halves —
  groups but no replica stays `Unloaded` and terminal, no groups at all
  establishes the absence and opens the arm.
- **The dial names the peer (§ Peer-auth model) — BUILT 2026-09-22, shared Rust
  only, zero app edits:** `FaunaMlsBackend::resolve_foreign` builds the
  canonical handle from the **dialed** domain and never reads the reply's echo,
  which the seam now carries verbatim under its honest name
  `ResolvedHandle::echoed_domain` (the same-nest `actor_by_handle` arm, where
  the answerer is this account's own home nest and the echo is how the client
  learns its own live handle domain, is the one place it is still read). Pinned
  by `fauna_mls_backend_tests.rs`
  (`resolve_address_foreign_echoed_domain_never_overrides_the_dial`,
  `resolve_address_foreign_multidomain_peer_keeps_the_typed_domain`, and
  `bootstrap_fetches_the_key_package_from_the_dialed_domain`, which traces the
  dial end to end through `peer_domain_for` into the seam's `keypackage_fetch`).
- **The dial names the peer on the Contacts Find User result too — BUILT
  2026-09-28, shared Rust, zero app edits** (the second consumer of the same
  anonymous `fauna.actor.by_handle` hop): `fauna_client_core::find_user::
  find_user_by_handle` names a qualified find result by the typed
  `localpart@domain` and reads the reply's `handle`/`domain` only for a bare
  handle resolved on the home nest. The UniFFI `resolve_handle` face (apple,
  windows, android), the wasm `actorByHandle` twin (web) and linux's
  `find_user_message` all route through it; tui never read the echo
  (`resolve_on_peer` keeps only the actor id and URL). Pinned by
  `find_user.rs`'s
  `a_peer_echoing_a_domain_it_was_not_dialed_at_is_named_by_the_dial` and
  linux's `a_foreign_peers_echo_never_names_the_find_result`.
- **What a spoofed label could actually reach — re-measured 2026-09-27, and
  CLOSED the same day by the overlay's paint gate.** At landing, no app keyed
  a display name on `actor_id` and `ContactsCache` was an empty stub, so the
  defect the dial rule closes read as a spoofable *label*, never a
  known-contact impersonation. The private contact overlay
  (`../ui/contacts.md` § The private overlay, built 2026-09-26) ended that:
  `ContactsCache` is a live actor id → overlay projection
  (`libs/fauna-conversations/src/contacts.rs`), and the member chips paint the
  viewer's nickname through `ConversationsManager::project_participant_nicknames`.
  The dial binds the handle, not the actor id (§ Peer-auth model → *What a
  peer's answer may and may not claim*), so a nest at the dialed domain that
  answered with the actor id of a person the viewer had nicknamed got that
  nickname painted on its participant — a defect closed by the ruling `contacts.md` § The private
  overlay → *The paint gate* owns: a nickname paints on a member chip only once
  the participant's actor id is a verified leaf of the thread's bound group
  (`RailBackend::authoritative_roster`), so the peer's bare claim paints the
  public label the viewer typed, and a substituted KeyPackage — the ratified
  residual above, reached — paints as the key it actually is. The device-local
  handle backfill (`ConversationsManager::handle_for_person`) lends a handle
  only from a seat proven the same way, so the forged pair can no longer put
  the dialed handle on that person's nameless seat in another thread. Pinned
  in `libs/fauna-conversations/tests/fauna_mls_backend_tests.rs`
  (`a_nickname_paints_on_no_member_chip_until_the_actor_is_a_verified_leaf`,
  `a_substituted_key_package_never_earns_the_nickname`,
  `an_unproven_seat_lends_no_handle_to_a_nameless_seat_elsewhere`). A message
  bubble's sender was never reachable this way: it is the leaf MLS
  authenticated at decrypt (`conversations.md` MLS-1) or a room message's
  signature-verified author.
