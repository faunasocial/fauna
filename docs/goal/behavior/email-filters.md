# Email filter rules — target state

Owns: email-filters
Status: ratified
Authority: the user-defined mail filter engine — the v1 condition set, the action set and its multi-action composition precedence, which mailboxes a `FileInto` may target, `continue`/first-match-wins ordering, the `fauna.email.filters.*` CRUD wire surface, the per-account auto-respond setting as one match-all `AutoReply` rule, and the pre-seal perimeter evaluation point; defers the `Forward` action's protocol mechanics to `behavior/mail-forwarding.md`, the spam score the `SpamScoreAtLeast` condition reads to `behavior/mail-spam.md`, the perimeter that evaluates the rules to `behavior/smtp-server.md` § Inbound pipeline, the per-app filter dialog rollout to `ui/settings.md` § Where logic lives, and which surviving rules an identity succession carries across (kept/removed disposition + the un-adjudicated-until-reviewed marking) to `behavior/succession-aftermath.md` § Re-key scope + § Adjudicating what the aftermath carries across. On conflict in element IDs or per-page element scope, `tests/e2e-unified/ui.yaml` wins.

> **Audience:** the nest + Go MTA work wiring rule evaluation, and the app work building the filter dialogs.
> **Split provenance:** carved out of `behavior/smtp-server.md` on 2026-08-03 (that doc was
> 226K, past comfortable cold-reading size). Content moved verbatim; the only edit was
> promoting the section heading one level. Pre-split history lives in `git log` on
> `behavior/smtp-server.md`.

---

## Email filter rules

Users create server-side filter rules that the **mail-bridge evaluates
at the MTA perimeter, pre-seal**, against the plaintext-floor metadata it
holds during the inbound DATA stage — the same timing and unconditional
posture as forwarding (`mail-forwarding.md` § Storage-mode interaction:
"one posture, unconditional"). The rules themselves are *stored* in nest
(`email_filters`, owned per-actor) and *fetched* per-recipient by the
bridge at delivery time (`fauna.bridges.fetch_recipient_filters`); nest
does not evaluate them — it never sees the subject/headers.

Rules specify match conditions and one action. The **v1 condition set**
is the envelope sender, the `Subject`, arbitrary headers, the **spam
score** — the "a per-user filter rule acts on the score" path of
`smtp-server.md` § Inbound pipeline step 5, carried as a scaled milli-int
(`SpamScoreAtLeast { milli }`; no float crosses the dag-cbor wire) — and
the decoded **body** (`BodyContains`). Body matching is a
**case-insensitive substring** test over the message's decoded
`text/plain` + `text/html` parts, which the MTA holds at the plaintext
floor pre-seal, unconditionally (`mail-forwarding.md` § Storage-mode
interaction); the perimeter size-caps the body it evaluates (256 KiB) and
matches HTML as raw decoded markup (no tag-stripping) in v1. What stays
deferred is the full RFC 5804 **ManageSieve** protocol + the Sieve `body`
extension (§ Known IMAP-server gaps) — the v1 engine is this simple-
substring subset, not a Sieve engine. Actions: move to folder
(`FileInto`), add label (`AddLabel`), discard, reject, forward, auto-reply.
The shared evaluator is `fauna_mail::filter::evaluate` (pure, ordered
multi-action — first-match-wins unless `continue`, see below; uniffi-exported,
called from the Go MTA — the perimeter-scorer split, like
`fauna_mail::scan`/`spam`). On the **client** side, the create-filter dialog's
`(kind, value)` dropdown → typed-variant encoder and its reverse for the
**edit** dialog are the shared `fauna_protocol::email` form seam
(`encode_filter_rule` / `encode_filter_action` / `describe_filter_*` /
`filter_is_editable`; PascalCase variant-name tags), so every app emits the
same wire shape and never opens a stored filter in a form that would silently
narrow it on save — `ui/settings.md` § Where logic lives owns the seam's shape
and the per-app rollout status. The full form covers every v1 condition and
action, the rule's order and its `continue` flag; a filter with no condition or
more than one is one only a raw API call (or the auto-respond setting below)
could have produced, and is listed but not editable. A form that does not yet
collect some inputs gates its edit affordance on the kinds it does collect.

| WS-RPC kind                     | Purpose               |
|---------------------------------|-----------------------|
| `fauna.email.filters.create`    | Create a filter rule  |
| `fauna.email.filters.list`      | List filter rules     |
| `fauna.email.filters.get`       | Get a filter rule     |
| `fauna.email.filters.update`    | Update a filter rule  |
| `fauna.email.filters.delete`    | Delete a filter rule  |

Wire: the typed `fauna.email.filters.{list,create,get,update,delete}`
WS-RPC kinds; the HTTP twins were deleted in the T9+T10 sweep (apps
reach these via `fauna-client-email::EmailClient` / `fauna-ffi`'s
`FfiEmailClient`). All five are replay-safe at 5 s — `list` / `get`
are pure reads, `update` is an idempotent overwrite, `delete` is
idempotent (already-deleted maps to `fauna.email.not_found`), and
`create` is replay-safe through the idempotency cache (a retried
request replays the prior reply, returning the same server-assigned
id). `get` / `update` / `delete` collapse the unknown-id and
wrong-actor paths to a single `fauna.email.not_found` (same shape the
`fauna.bridges.*` kinds use), keeping per-actor isolation enforced
identically through the DB filter.

Rules are evaluated in order (`priority ASC, id ASC`). The first matching
rule wins **unless** it is marked `continue` (the per-rule
`EmailFilter.continue_on_match` flag, default `false` = first-match-wins):
a `continue` match records its action and evaluation **falls through** to
later rules, so several rules' actions can apply in order. The evaluator
(`fauna_mail::filter::evaluate`) returns the **ordered** matched actions,
ending at the first non-`continue` match (or the last rule).

**Multi-action composition (applied at the MTA perimeter, in match order):**

- `FileInto` / `Allow` set the placement target — **the last one wins**
  (`Allow` = INBOX, overriding spam disposition). A message is filed into a
  single mailbox in v1 (no Sieve multi-`fileinto` fan-out copies).
- **`Allow` overrides the spam disposition at every scoring position, not only
  at delivery** (ruled 2026-09-28). The per-user scorer runs again after
  delivery and re-files spam to Junk ([`mail-spam.md`](mail-spam.md) § Scoring
  placement), so when an `Allow` wins placement the MTA — where `Allow` is
  decided, after DATA — replaces the recipient's `X-Fauna-Spam-Threshold`
  delivery stamp (folded by nest at RCPT, [`mail-aliases.md`](mail-aliases.md)
  § Spam-threshold override) with the disabled tier `0`, as the first and only
  threshold line, before sealing. Every post-delivery scorer reads the first
  stamp, so the message stays in INBOX. One shared-Rust writer beside the
  reader: `fauna_mail::aliases::stamp_filter_allow`. A later `FileInto` that
  wins placement keeps nest's stamp.
- **A `FileInto` targets only a mailbox inbound mail can belong in** (ruled
  2026-09-15): `INBOX`, `Archive`, `Junk`, `Trash`, or a custom folder
  (auto-created at first placement). It never targets a mailbox whose
  contents claim something an inbound delivery would forge:
  - `Sent`, which holds only this account's authenticated sends — the rule
    that makes a `Sent` record proof of authorship is owned by
    [`../ui/conversations.md`](../ui/conversations.md) § Receiving into the
    conversations view;
  - `Drafts` (RFC 6154 `\Drafts`), the account's own unsent compositions — a
    MUA reopens a draft and sends it under the account's identity;
  - the guardian's held mailbox, where a message *is* a hold and lands only
    with its sidecar ([`family-safety.md`](family-safety.md) § The mail gate).

  `Trash` stays a target: nothing reads its contents as authored or pending,
  so filing there is a discard the user can undo. The target is checked the
  way an `AddLabel` label is: at create and update (`fauna.email.filters.create`
  / `update` → `invalid_params`, together with the name rules IMAP `CREATE`
  enforces, [`imap-server.md`](imap-server.md)), and again at placement, where
  a refused or malformed target falls back to the spam-disposition placement —
  so a rule stored before the check, or carried across a succession, never
  files there. Names match exactly, as the mailbox store does: `sent` is an
  ordinary custom folder. The shared check is
  `fauna_protocol::email::validate_file_into_mailbox`.
- `AddLabel` **accumulates** — every matched label rides the delivery as an
  IMAP keyword. A label MUST be a single RFC 3501 keyword `atom`: it is validated
  at create time (`fauna.email.filters.create` → `invalid_params`) and re-screened
  at ingest, rejecting a leading `\` (the IMAP system-flag namespace — `\Deleted`
  would make matching mail EXPUNGE-eligible, `\Seen` silently read), internal
  whitespace (which would split into several keywords), and any `atom-special` /
  control / non-ASCII byte. The shared validator is
  `fauna_protocol::email::validate_label` (one rule for nest + a future app
  dialog).
- `Discard` is **terminal**: the recipient's delivery is dropped and no
  later action in the chain is applied (the first `Discard` short-circuits
  the composition). A `continue` rule preceding a `Discard` still has no
  effect, since the message is never stored.
- `Reject` (Sieve `reject`, RFC 5429) is **terminal** like `Discard` — it
  suppresses placement and short-circuits the chain. For a **single-recipient**
  transaction (and a non-null sender) it refuses the whole message with
  `550 5.7.1 <reason>` at end-of-DATA, so the sender's own MX bounces it (no
  backscatter from us); the `reason` rides the response text sanitized to a
  single CRLF-stripped, length-capped line so a user string can't smuggle SMTP
  protocol bytes. For a **multi-recipient** transaction — where we have already
  committed `250` for the others and the RCPT-time wire moment for a
  per-recipient 5xx has passed — and for a
  **null-sender** (`MAIL FROM: <>`) message, `Reject` instead **drops that one
  recipient silently** (placement-identical to `Discard`) and increments
  `smtp_inbound_filter_reject_total{mode}`; we never synthesize an unsolicited
  DSN (backscatter suppression — we never bounce a bounce, `mail-forwarding.md`
  § NDR routing on permanent forward failure).
- `AutoReply` (Sieve `vacation`, RFC 5230) is **non-placement and
  non-terminal**: it composes alongside a `FileInto` / `AddLabel` on the same
  `continue` chain (a message can be both filed *and* vacation-replied), but a
  `Discard` earlier in the chain suppresses it (no delivery → no reply), and it
  fires only **after** the recipient's local delivery commits. The reply is sent
  at most once per `(recipient, envelope-sender)` per `interval_hours` (the
  `auto_reply_log` rate limit), with `MAIL FROM: <>` (null sender — an
  auto-reply must not itself be bounceable into a loop, RFC 3834),
  `Auto-Submitted: auto-replied`, `In-Reply-To` / `References` set to the
  triggering message, `From:` the recipient's address, and DKIM-signed under the
  recipient's domain via the existing outbound signer (it is outbound from us).
  The nest composes it from its own stored copy of the rule; the MTA names the
  rule, never the reply's bytes.
  It is **suppressed** (RFC 5230 §4.4/§4.6, RFC 3834) when: the envelope sender
  is null (it's a bounce); the message is itself automated (`Auto-Submitted` ≠
  `no`); it is list/bulk mail (`List-*` headers or `Precedence: bulk|list|junk`,
  see `mail-mass-mailing.md`); the envelope sender is one of our own
  `local_domains` (don't reply to our own notifications); or the recipient is not
  in the message's `To`/`Cc` (the §4.4 bcc/list guard). The loop-guard predicate
  runs at the perimeter on the plaintext-floor envelope + headers (the same
  reason filters evaluate here, not in nest).
- `Forward` is **non-terminal** like `AutoReply`, and in its default `copy`
  mode **non-placement** too: every fired `Forward` in a `continue` chain
  forwards once, after the recipient's local delivery commits (a
  `Discard`/`Reject` in the chain suppresses it — no delivery, no forward). A
  `redirect`-mode `Forward` is the one filter action that suppresses the
  recipient's local placement; that decision, its ordering and its fallback
  are `mail-forwarding.md` § Per-rule "forward to"'s. Its protocol mechanics
  (SRS, loop detection, NDR routing, rate cap, copy mode) live in
  `mail-forwarding.md` — that doc spells out the wire behavior when the action
  fires.

Which of these rules survive an identity succession is **not** this doc's to
say: an action that makes the nest emit content outward under the account's
identity (`Forward`, `AutoReply`) is standing authority a seed thief could have
armed, and the per-action disposition is ruled in
[`succession-aftermath.md`](succession-aftermath.md) § Re-key scope. The rules
that *do* survive are not carried silently: each is marked un-adjudicated until
the successor keeps or removes it, ruled in the same doc's § Adjudicating what
the aftermath carries across (2026-08-14) — which is also where the surface that
routes them to this list is specified.

If no rule matches (or all matches are non-placement log-only actions), the
message falls through to the spam-disposition placement (§ Spam handling).

The `forward` action's protocol mechanics (SRS envelope-from rewriting,
loop detection, NDR routing to the forwarder, per-account rate cap,
encrypted-mode compatibility) live in `docs/goal/behavior/mail-forwarding.md`;
this section names the action, that doc spells out the wire behavior
when the action fires.

### The auto-respond setting

The per-account auto-respond toggle and message (`mail-policy-config.md` § Tier 3) are not state of their own: they are a shared-Rust facade (`fauna_client_email::EmailClient::autorespond_{get,set,clear}`) over **one `AutoReply` filter rule**, so the reply is sent by the perimeter evaluation above and by nothing else. The nest holds no auto-respond field and no marker; there is no second enforcement path.

**The rule's shape.** Zero conditions under `all` (vacuously true — it matches every message; zero conditions under `any` match nothing and are not the setting), action `AutoReply`, `continue_on_match: true` and priority `i32::MIN`. Non-terminal and sorted first, it never shadows a user's rule and no user rule — terminal or not — can shadow it; the reply composes with whatever placement the user's own rules then decide (a `Discard`/`Reject` later in the chain still suppresses it, per the composition rules above). Its `name` (`Auto-respond`), `subject` (`Automatic reply`) and `interval_hours` (168 — seven days, the Sieve `vacation` default) are hard-coded constants in that crate, not knobs: the setting is one toggle and one message, the message being the action's `body`.

**Recognised by shape, not by name.** Any rule with zero conditions under `all` and an `AutoReply` action IS the setting — it does exactly what the toggle means, whoever built it. Read reports on, with the body of the first such rule in evaluation order (`priority ASC, id ASC`), else off with an empty message. Setting a message creates the rule when none exists; otherwise it rewrites the first one to the canonical shape above (keeping its id) and deletes any further ones, so the account ends with exactly one. Turning it off deletes every such rule — a filter has no disabled state, so the message goes with the rule and an app re-collects it the next time the toggle is turned on. An `AutoReply` rule *with* conditions (a reply only to certain senders, built by a raw `fauna.email.filters.*` call) is the user's own rule: the facade neither reads nor touches it.

**In the filter list.** The rule is listed like any other — hiding a rule that acts on the user's mail would hide what their mail does — with its name and the `Auto-reply` action label (`filter_action_label`), and, like every rule the shared form cannot represent (`filter_is_editable` is false for a rule with no condition), without an edit affordance on any app; the toggle and message on `mail-settings` are where it is edited. Deleting it from the list is turning auto-respond off. A distinct "managed by auto-respond" treatment in the list would need new ui.yaml ids and is not ruled.

**Implementation status today.** The CRUD surface (`fauna.email.filters.*`,
`email_filters` table, `fauna-client-email`) is live and conformance-tested;
the shared evaluator `fauna_mail::filter::evaluate` + the spam-score condition
(`SpamScoreAtLeast`) landed as Slice 1 of the filter track. **Perimeter
execution IS wired** (S3+S4a): nest serves the rules to the MTA
(`fauna.bridges.fetch_recipient_filters`), and the Go MTA fetches them per
recipient at delivery, evaluates (`fauna_mail::filter::evaluate`), and applies
the verdict to placement — `FileInto`/`Allow` ride a new ingest `target_mailbox`
field (overriding the spam-disposition → folder map, leaving the recorded
`spam_disposition` truthful; a custom folder is auto-created with its `Create`
placement record), `AddLabel` rides `extra_flags`, `Discard` skips the
recipient's ingest. **`Reject` is wired**: the Go MTA treats a fired `Reject` as
terminal (like `Discard`) — a single-recipient, non-null-sender transaction gets
a `550 5.7.1 <sanitized reason>` at end-of-DATA (the sender's MX bounces it),
while a multi-recipient or null-sender message drops that recipient silently and
counts `smtp_inbound_filter_reject_total{mode}`, never a DSN. **`Forward` is
wired** (2026-09-24, `redirect` copy mode 2026-09-25): the MTA stashes each fired
`Forward` and dispatches it through the shared forward pipeline — after local
delivery in `copy` mode, in place of it in `redirect` mode (`mail-forwarding.md`
§ Implementation status today; the rule editor offers the action on tui, the
other apps following).
**`AutoReply` is
wired**: the MTA stashes a matched `AutoReply` and, after the recipient's local
delivery commits, applies the loop guard
(`fauna_mail::filter::auto_reply_decision` — null-sender / `Auto-Submitted` ≠ no /
list-bulk / our-own-domain / recipient-in-To-Cc) — and never replies for a
recipient reached through a role address such as `postmaster@` (`smtp-server.md`
§ abuse@ / postmaster@ role-address routing; suppressed as
`reason="role_address"`) — and calls `fauna.bridges.send_auto_reply` naming the
recipient, the delivered-to address, the triggering sender and `Message-ID`, and
the matched rule's id. The nest composes the reply from its own stored copy of
that rule (`fauna_mail::outbound::autoreply::compose_auto_reply`; a rule id that
is not the recipient's `AutoReply` is refused — the door's rule is
`mail-multidomain.md` § From: header ownership → *Per door, nest-side*), claims
the `(recipient, sender)` rate-limit slot (`auto_reply_log`) atomically and — if
free — enqueues the reply into `outbound_mail_queue` with a null envelope-from;
it signs it at the outbound hand-out like every queued message. A `Discard`/`Reject` earlier in the chain suppresses it (no
delivery → no reply). Metrics: `smtp_inbound_autoreply_sent_total`,
`smtp_inbound_autoreply_suppressed_total{reason}`. Each
layer is unit/integration-covered (nest placement, the Go enum decode, the
evaluator), and the full-stack tier_3
(`tests/e2e-unified/tests/test_mail_bridge_filter_delivery.py`) is **green** — it
delivers through the real bridge and asserts placement from the nest DB
(`SenderDomain`→a custom folder, `SpamScoreAtLeast`→Junk, `BodyContains`→a custom
folder, `Discard`→dropped). **Body matching (`BodyContains`) is wired (S4b)** — the
MTA threads the decoded `text/plain` + `text/html` (reusing the body it already
parsed, size-capped at 256 KiB) into `FilterContext.body`, and the shared
evaluator does a case-insensitive substring match (raw HTML markup, no
tag-stripping). **`continue`/multi-action is wired** — `EmailFilter.continue_on_match`
(DB column + the `fauna.bridges.fetch_recipient_filters` projection) lets the
evaluator return an ordered list of matched actions, which the MTA composes per
the precedence above (last-`FileInto`-wins, `AddLabel` accumulates, `Discard`
terminal). **`Allow` holds past delivery** (2026-09-28): the MTA's
`recipientSealedCopy` seals an Allowed recipient's copy through
`stamp_filter_allow`, so the MDA's SELECT-time pass and the shared on-device
INBOX scorer both read threshold `0`; proof: `filter_allow_test.go` and the
tier_3 `test_mail_spam_threshold_override.py::test_allow_rule_keeps_a_senders_mail_in_the_inbox_past_every_scoring_pass`. The typed `fauna-client-email`
`filters_{create,update}` carry the rule's order and its `continue` flag
(2026-10-18), and the tui, linux, web and android rule editors set both
(`filter-priority-input`, `filter-continue-toggle`) beside every v1 condition
and action, gated on the shared `filter_is_editable`; witnessed by
`test_settings.py::test_email_filter_{files_labels_and_replies_from_the_form,matches_a_header_value_or_the_spam_score,order_and_continue_are_set_from_the_form}`.
The FFI face carries every input (`FfiFilterRuleInputs`, the grown
`FfiFilterActionInputs`, `FfiEmailFilter.continue_on_match`, the
`continue_on_match` argument of `filters_{create,update}`) and so does the wasm
face. Apple and Windows still offer the single-value conditions and the
`Allow`/`Discard`/`Reject`/`Forward` actions and save every rule
first-match-wins at order 0, so an edit there resets both.

**`FileInto` targets are enforced** (2026-09-15). `validate_action` refuses a
`Sent`, `Drafts`, held-mailbox or malformed target on `create` and `update`,
and ingest placement falls back to the spam disposition for such a target
already stored, so no rule places inbound mail in any of the three. Proof: the
in-crate placement test beside the custom-folder one, and
`conformance_email_filters.rs`. The tui rule editor collects a `FileInto`
(2026-10-18) and checks the target with the same
`validate_file_into_mailbox` before sending, so the form refuses `Sent`,
`Drafts` and the held mailbox on the page.

**The auto-respond facade is built** (2026-10-12): `fauna_client_email::autorespond` implements § The auto-respond setting, crate-tested for on, off, editing the message, read-back, a hand-built conditional `AutoReply` left alone and hand-built match-all rules adopted and collapsed to one. Since 2026-10-18 it is exposed over UniFFI as `FfiEmailClient::autorespond_{get,set,clear}` and over wasm as `emailAutorespond{Get,Set,Clear}`, beside the draft check `autorespond_message_draft_outcome` (wasm `autorespondMessageDraftOutcome`). tui renders it (2026-10-17): `mail-settings-autorespond-toggle` and `mail-settings-autorespond-message-input`, the blank-message check the shared `fauna_client_mail_settings::parse_autorespond_message_draft`; web follows (2026-10-18) over the wasm facade and `autorespondMessageDraftOutcome`; linux renders them too (2026-10-18); the other four apps follow.
