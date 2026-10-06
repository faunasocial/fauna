# Mail content scanning — target state

Owns: content-scanning
Status: ratified
Authority: ClamAV + rspamd scan-pipeline configuration (knobs, actions, sidecar topology), the message_scan_results storage shape + retention, scan-result cross-actor isolation (admin aggregate / user own-row), and the encrypted-mode interaction (deployment-data plaintext); defers scorer placement to `architecture/content-scoring.md`, mail-pipeline ordering to `behavior/smtp-server.md` § Inbound pipeline, the combined-score formula to `behavior/mail-spam.md`, knob tiers to `behavior/mail-policy-config.md`, the at-rest property to `architecture/encryption-at-rest.md`.

> **Audience:** nest work touching `message_scan_results` / the scan-verdict ingest fields; work on the shared scan pipeline in `libs/fauna-mail/src/scan/` + the Go `scan_gate.go`; per-app work implementing the per-message scan-result row on the **general mail message-detail surface** (`mail-message-detail-scan-result-row`); the scanner mail-health badge consumers (the badge's former `admin-services` home was removed 2026-06-04 — admin.md § Admin IA redesign — so the readout is deferred, no current home). (The admin scanning **knobs** are automatic / inert — no manual admin UI — per the mail-UX design pass; see § UX surface.)
> **Purpose:** the canonical doc for the ClamAV malware-scan + rspamd content-pre-classifier configuration / opt-in / per-message result storage / cross-actor isolation / encrypted-mode interaction / admin-vs-user-visible result surface. On conflict in pipeline ordering, `smtp-server.md` wins; in combined-score-formula contract, `mail-spam.md` wins; in encrypted-mode storage shape, `wrapped-blob-crypto-design.md` wins. (The earlier `fauna.bridges.scan_message_content` plaintext-to-nest design was **removed** at the 2026-05-23 perimeter-scorer ratification (tracked internally); see § Where the pipeline runs.)

---

## Goal

Two **deployment-wide pre-classifiers** that score every inbound message before user-specific spam classification and filter-rule evaluation:

1. **ClamAV malware scan** — pattern-matched against the freshclam-updated signature database. Verdict: clean / infected (with signature name) / error. Infected mail rejects at SMTP perimeter (DSN to sender) by default, or — admin opt-in — is filed to the recipient's Junk, or is delivered tagged with a header for the recipient's filter rules.
2. **rspamd content score** — pattern-matched against rspamd's deployment-default rule set (BAYES, RBL, URIBL, MIME structure, etc.). Verdict: a score on our 0–15 scale (carried as a milli-int `scaled_milli: i32` on the wire — the dag-cbor float ban; matching the per-user Bayesian scale per `mail-spam.md` § Two-classifier pipeline for combinator simplicity), plus the set of flagged rule names.

Both run **after** DKIM/SPF/DMARC verification (the bridge daemon's perimeter pipeline per `smtp-server.md` § Inbound policy stack) and **before** the per-user Bayesian classifier (`mail-spam.md` § Two-classifier pipeline). Both produce a row in `message_scan_results` carrying the verdict + signature/rule details — deployment-data shape, plaintext storage regardless of deployment storage mode, with its own retention.

**Bar: parity with how SpamAssassin + ClamAV deploy in modern self-hosted mail stacks (mailcow, Mail-in-a-Box, iRedMail) — deployment-wide pre-classification, per-message result audit, admin-tunable rules, user-immune (a user can't change the deployment-wide rspamd weights).** The Fauna-specific shape on top: results stored per-message in nest state (not in syslog), cross-actor-isolated by the user-controls-their-data invariant (admin sees aggregate, user sees own).

**Default-on.** Both ClamAV and rspamd ship enabled. ClamAV default action is `reject` at SMTP perimeter (DSN). rspamd default score-threshold-to-spam is 5 (cross-referenced from `mail-policy-config.md` § Inbound perimeter's `mail.inbound.max_score_before_spam_folder`). Disabling either requires the admin to flip a Tier-2 toggle (with a warning in the UI that disabling weakens the inbound defense).

---

## Pipeline

Per `smtp-server.md` § Inbound pipeline, the order is:

1. **DKIM / SPF / DMARC / ARC verification** (the bridge daemon's perimeter pipeline; runs at the MTA process before reaching nest).
2. **ClamAV malware scan** (this doc § ClamAV).
3. **rspamd content score** (this doc § rspamd).
4. **Per-user Bayesian classifier** (per `mail-spam.md` § Two-classifier pipeline).
5. **Combined score → routing decision** (per `mail-spam.md` § Combined-score formula).
6. **Filter rules** (per `email-filters.md` § Email filter rules).
7. **Delivery to INBOX / Junk / filtered folder** (per `email-filters.md` § Email filter rules).

Steps 2–3 are this doc's authority. Step 1 is the bridge's; step 4–7 cross-link to the existing docs.

### Where the pipeline runs

The scan is a **perimeter scorer** per `docs/goal/architecture/content-scoring.md` — it runs at the **mail bridge (MTA), on the plaintext the bridge already holds, before sealing**, in both storage modes. It does **not** run on the nest: the nest never receives the message plaintext (in encrypted mode the body reaches the nest already sealed to the recipient), and running ClamAV/rspamd in the nest process would puncture the encrypted-mode trust property (`content-scoring.md` § Why this is a corollary of encryption-at-rest).

Concretely, after DKIM/SPF/DMARC verification and the spam gate, and before `EncryptToRecipient`: the bridge runs ClamAV then rspamd against the plaintext via the shared `libs/fauna-mail` scanner clients (the same cgo/UniFFI path the bridge already uses for spam scoring), decides the action (deliver / reject / junk / tag — § Actions), stamps the verdict-derived headers on the message (e.g. `X-Fauna-Scan-Clamav: clean`, `X-Fauna-Scan-Rspamd-Score: 1.2`, `X-Fauna-Scan-Rspamd-Rules: BAYES_HAM,MIME_GOOD`), then seals and delivers. Only the **verdict metadata** crosses to the nest — it rides the `ingest_inbound_mail` request (the same way `spam_score` / `spam_disposition` already do), and the nest inserts the `message_scan_results` row from it. There is **no `scan_message_content` RPC** and no plaintext-to-nest path. A reject-at-perimeter (which makes no ingest call, since the message is never stored) records its forensic `message_scan_results` row via a metadata-only report path (§ Per-message scan-result storage; the consuming track decides whether that is a small dedicated RPC or a flag on the verdict path).

---

## UX surface

> **Ratified 2026-06-01 by the mail-UX design pass (tracked internally, § DESIGN DECISION).** The knob tables and result-row references below were drafted against a `bridges-detail` mail-admin page and an `account-detail` mail-detail page — **neither exists in ui.yaml**. They are reconciled to the flat homes:

- **Admin scanning knobs → automatic / inert, no manual UI.** The `mail.scanning.*` Tier-2 knobs (clamav enable/action/max-size, rspamd enable/threshold/scaling/rule-overrides, signature status + force-refresh) are **catalogued but inert** today (no projection, no admin write-path — § Implementation status today, `mail-policy-config.md` Bucket C). The `bridges-detail-mail-scanning-*` IDs in the tables below are **dropped**. Scanning ships on with sensible defaults (works-out-of-the-box); ClamAV signature updates are automatic (freshclam, hands-off). If a future admin track exposes any genuinely-admin-choice scanning knob, its ID lands on the flat **`admin-mail`** page (allocated with that consuming nest+app track, not pre-invented), and scanner health surfaces as a status badge — never a `bridges-detail` card (the badge's former `admin-services` home was removed 2026-06-04 — admin.md § Admin IA redesign — so the readout is deferred, no current home). The `bridges-detail-mail-scanning-clamav-signature-status` / `-refresh-button` likewise fold into the (deferred) health badge / `admin-mail` if a consuming track needs the force-refresh affordance.
- **User scan-result row → the general mail message-detail surface.** `account-detail-mail-message-detail-scan-result-row` is renamed `mail-message-detail-scan-result-row` — a row on the general mail message-detail surface (part of the mail client, not the `mail-settings` family or a `bridges-detail`/`account-detail` page), exactly like `mail-deliverability.md`'s `mail-message-detail-warmup-deferred-row`. Its ID is allocated on that surface with the consuming mail-client track per the ui.yaml-owns-IDs split.

The behavior / storage / cross-actor / encrypted-mode sections below stand unchanged; only the `bridges-detail` / `account-detail` UI-rendering premise is reconciled.

---

## ClamAV configuration

The deployment ships clamd as a **compose sidecar container** (`clamav/clamav:latest-debian` — the `-debian` suffix is load-bearing, see `installers/docker.md` § Platform Support; reached over the compose network at `clamd:3310` — the topology hatch `clamd_addr`, defaulted from `FAUNA_CLAMD_ADDR`; `installers/docker.md` § docker-compose.yml owns the topology). Signatures are kept current by freshclam, which the sidecar image bundles and runs continuously (pulls from clamav.net on a timer); the admin doesn't manage signatures.

### Tier 2 knobs

All four knobs are **inert today (no manual UI)** — compile-time defaults, no projection / admin write-path (§ UX surface, § Implementation status today). If a consuming admin track exposes any of them it lands on the flat `admin-mail` page; the `bridges-detail-mail-scanning-*` IDs are dropped.

| Knob | Default | Binding |
|---|---|---|
| Malware scanning enabled | `true` | `mail.scanning.clamav_enabled` |
| Max scanned file size | **derived, not a knob** — the product ceiling `max_message_bytes` itself (ruled 2026-08-26, § Oversize messages); a separately-lower scan cap is never a choice an admin wants | ~~`mail.scanning.clamav_max_filesize_mb`~~ retired 2026-08-26 |
| Action on malware hit | `reject` | `mail.scanning.clamav_action_on_infected` (`reject` / `junk` / `tag`) |

### Actions

- **`reject`** (default) — at SMTP perimeter, after DATA but before delivery, the message is rejected with `554 5.7.1 Message contains malware: <signature-name>`. The sender's MTA sees the DSN; the message is not stored. The scan-result row is **still inserted** (audit trail of "we rejected this") with `delivered = false`.
- **`junk`** — the message is delivered to the original recipient's Junk folder, sealed to the recipient like any other mail, with `X-Fauna-Scan-Clamav: infected` + the signature header stamped. Nothing is copied anywhere else: there is no deployment-wide quarantine actor or folder and no admin review of members' mail (an admin reading members' mail is ruled out — `mail-policy-config.md` § Inbound perimeter).
- **`tag`** — the message is delivered to the original recipient with the scan headers stamped but no routing override; the user's filter rules can act on `X-Fauna-Scan-Clamav: infected`.

The long-term-defensible default is `reject` — most malware in 2026 is from never-going-to-be-legitimate senders, and reject-at-perimeter avoids the storage cost + the user surprise of malware in their mailbox.

### Oversize messages

**Ruled 2026-08-26: the scan cap is not a knob — the gate scans everything the perimeter accepts.** The ClamAV gate's size cap is **derived** from the product ceiling `max_message_bytes` ([`mail-message-size.md`](mail-message-size.md) § Message size limits), read from the same live policy snapshot the perimeter's `552 5.3.4` clamp reads — so by construction no accepted message is above the scan cap at *any* admin setting, and the never-allow-without-scan rule (§ Implementation status today) has no size exception. The scanner sidecar's own limits (clamd's `StreamMaxLength` / `MaxScanSize` / `MaxFileSize`) are bucket-1 artifact-set wiring, never a file anyone edits: the compose bundle ships them inline — exactly as it already ships rspamd's worker config — at a fixed value ≥ `MAX_MESSAGE_BYTES_CEILING`, the product ceiling's upper bound (owner: `mail-message-size.md`), so clamd never refuses an admissible message on size; the scan round-trip timeout is sized for that ceiling, not for 50 MiB. The `bypassed_oversize` verdict and its `X-Fauna-Scan-Clamav: bypassed_oversize` header stay defined as the *defensive* shape for a mis-wired deployment (a gate handed no ceiling), and `mail_scanning_clamav_oversize_total` is emitted as that shape's **tripwire** — a non-zero value is a bug, not a statistic.

*Why the three alternatives were refused.* Clamping `max_message_bytes` to the scanner's cap makes an antivirus detail the ceiling on a product capability (wrong direction). Keeping a separately-chosen scan cap (the former Tier-2 knob above) fails the two-bucket test of `principles.md` § One configuration surface on the *derived* side — a scan cap below the ceiling is only ever a silent downgrade, so no admin would choose it — which is why the knob is retired rather than projected. Warning at the door while leaving the band unscanned keeps a security control off for exactly the largest messages, by design; the door still refuses a ceiling the scanner cannot cover, but that refusal is `mail-message-size.md`'s upper bound, not a scanning knob.

### Signature management

Freshclam updates run automatically (hands-off). Signature freshness surfaces as part of the scanner mail-health badge rather than a dedicated card; if freshclam fails for >24h (network outage, clamav.net down), the badge goes yellow. (The badge's former `admin-services` home was removed 2026-06-04 — admin.md § Admin IA redesign — so the readout is deferred, no current home. **Still deferred after 2026-09-25:** the mail health readout the user ratified that day — `mail-deliverability.md` § Admin-pane Deliverability surface → *The mail health readout* — covers deliverability checks only; its approved id set has no scanner row, so the scanner health signal is **outside that ratified scope** and needs its own ask before any id is minted.) A "Force refresh" affordance is not required for correctness (freshclam self-retries); if a consuming admin track adds an on-demand force-refresh, its ID lands on the flat `admin-mail` page. The draft's `bridges-detail-mail-scanning-clamav-signature-status` / `-refresh-button` IDs are dropped.

### Compile-time decisions

- The clamd address is OS-deployment topology, **set on the mail-bridge, not the nest** — the scan runs Go-side at the bridge perimeter (§ Where the pipeline runs), so the address of the deployment's clamd is a bridge topology hatch value (`clamd_addr`, defaulted from `FAUNA_CLAMD_ADDR`; the compose deployment sets `clamd:3310`), the direct analogue of the bridge's `mta_bind_addr`. (The legacy nest-side `--clamd-host` flag was for the retired in-nest scanner path; it does not configure the perimeter scan.)
- Signature update endpoint (clamav.net) is hard-coded — alternative signature feeds need a code change.
- The `X-Fauna-Scan-Clamav` header name is not a knob.

---

## rspamd configuration

rspamd runs as a **compose sidecar container** (`rspamd/rspamd:latest`, reached over the compose network at `rspamd:11333` — the topology hatch `rspamd_url`, defaulted from `FAUNA_RSPAMD_URL`) with its own rule database (BAYES.GLOBAL, RBL.SPAMHAUS, URIBL.SURBL, MIME.STRUCTURE, etc.). The sidecar image ships a curated rule set; rules update via rspamd's built-in update-on-restart logic.

### Tier 2 knobs

Same as the ClamAV knobs: **inert today (no manual UI)**; `admin-mail` if a consuming admin track exposes them; the `bridges-detail-mail-scanning-*` IDs are dropped (§ UX surface).

| Knob | Default | Binding |
|---|---|---|
| rspamd scanning enabled | `true` | `mail.scanning.rspamd_enabled` |
| Rule overrides (admin's safety valve) | `{}` | `mail.scanning.rspamd_rule_overrides` (map of `<rule_name>` → `{enabled, weight}`; any realized wire shape carries the weight as a scaled integer — the dag-cbor float ban) |
| rspamd score scaling | 500 per-mille (= 0.5 — maps rspamd's nominal 0–30 to our 0–15 scale; the multiplier the bridge applies before feeding mail-spam) | `mail.scanning.rspamd_score_scaling` (wire: `rspamd_score_scaling_per_mille: u16`) |

The spam-folder threshold the scaled score is compared against is **not** a scanning knob — it is `mail.inbound.max_score_before_spam_folder` (`SpamPolicyThresholds`, default 5; `mail-policy-config.md` § Inbound perimeter owns it).

### Rule overrides (the safety valve)

The deployment-default rspamd rule set is sensible for most cases. Specific deployments may find a rule over-tuned (e.g., `BAYES_SPAM` over-fires on a deployment with a particular kind of legitimate-but-bulky email — newsletter-heavy users get false-positives). The admin can:

- **Disable a rule** — set `mail.scanning.rspamd_rule_overrides[<rule_name>].enabled = false`. The rule's score contribution becomes 0 on every message.
- **Adjust a rule's weight** — set `mail.scanning.rspamd_rule_overrides[<rule_name>].weight = <new-weight>`. The rule's contribution is the new weight × the rule's nominal score.

Rule overrides are **admin-tier only** — a user with the ability to change rspamd weights would have a privilege escalation (their override affects every user's pre-classification). The user's safety valve is the per-account spam-threshold override (`mail.account.spam_threshold_override`, Tier 3) which lets them be more or less aggressive without touching the deployment-wide rules.

### Score scaling

rspamd's native score range is around 0–30 (with a default spam-threshold of 5–15 depending on the rule set). The Fauna-side combined-score formula (per `mail-spam.md` § Combined-score formula) uses a 0–15 range for compatibility with the Bayesian classifier. The bridge **scales rspamd's raw score** by `mail.scanning.rspamd_score_scaling` (wire `rspamd_score_scaling_per_mille: u16`, default 500 = ×0.5 — maps rspamd's nominal 30 to our 15; scores ride as milli-ints, `libs/fauna-mail/src/scan/mod.rs`) before feeding into the combined-score formula.

### Compile-time decisions

- The rspamd HTTP endpoint URL is OS-deployment topology, **set on the mail-bridge** (`rspamd_url` in the topology hatch, defaulted from `FAUNA_RSPAMD_URL`; the compose deployment sets `http://rspamd:11333`), not a nest knob — same rationale as the clamd address above (the scan is a Go-side perimeter operation).
- The rspamd checkv2 vs. legacy-check protocol — pinned to checkv2 (HTTP/2 + JSON body).
- The rule database update mechanism (rspamd's built-in HTTP pulls) — not a knob; alternative rule sources require a code change.
- The `X-Fauna-Scan-Rspamd-*` header names are not knobs.

---

## Per-message scan-result storage

Every inbound message that reaches the scan pipeline (i.e., DKIM/SPF/DMARC-verified — not rejected at the connection-time or envelope-time policy stack) gets one row in `message_scan_results`. A message no scanner touched gets **no row**: every submission-door delivery (the colleague twin and the sender's own Sent copy — submission never invokes the scan gate, § Implementation status today's per-door census) and an inbound with both scanners disabled arrive with the wire's `ClamavVerdict::NotScanned` and no rspamd score, and the nest never turns that silence into a verdict (until 2026-09-28 the wire's default was `Clean`, so each of these recorded a scan that never ran). When ClamAV did not run but rspamd did, the row is kept for rspamd's detail record and `clamav_verdict` says `'not_scanned'`:

```
message_scan_results(               -- SQLite DDL: db/migrations.rs MIGRATIONS_MESSAGE_SCAN_RESULTS
    message_id             BLOB PRIMARY KEY, -- the 32-byte ingest message_id (idempotent insert on retry)
    received_at            INTEGER NOT NULL, -- unix seconds
    direction              TEXT NOT NULL DEFAULT 'inbound', -- (outbound is not scanned by ClamAV; rspamd outbound is a separate concern)
    clamav_verdict         TEXT NOT NULL,    -- 'clean' / 'infected' / 'error' / 'bypassed_oversize' / 'not_scanned' (the last only beside an rspamd score — no scanner ran ⇒ no row)
    clamav_signature       TEXT,             -- non-null only when clamav_verdict = 'infected'
    rspamd_score_raw       INTEGER,          -- rspamd's native score as a milli-int (0–30 range ⇒ 0–30000)
    rspamd_score_scaled    INTEGER,          -- milli-int after applying the per-mille scaling; the value fed to mail-spam combined-score
    rspamd_flagged_rules   TEXT,             -- JSON array of rule names that fired
    rspamd_score_breakdown TEXT,             -- JSON per-rule contribution map
    scanned_at             INTEGER NOT NULL,
    action_taken           TEXT NOT NULL,    -- 'delivered' / 'rejected_malware' / 'junked' / 'tagged'
    delivered_to_actor     BLOB              -- the recipient actor (NULL only for reject-at-perimeter forensic rows)
)
```

Indexes:

- `(received_at)` for retention GC.
- `(delivered_to_actor, received_at)` for the per-user "my scan results" query.
- `(clamav_verdict, received_at)` for the admin-pane "ClamAV hits" aggregate query.

> **Relation to the uniform scoring-metadata bus (2026-07-05; contract phase 2026-09-28).** These per-kind columns are the *detail record* for the ClamAV/rspamd factors; the **canonical cross-factor shape** a downstream consumer reads is the uniform `scores: [{factor, score, tier, scorer_version}]` array on the nest's `content_scores` table, minted at the perimeter from the same verdicts and sent beside these fields (`content-scoring.md` § The scoring-metadata bus owns it, including the ruling that this table is a detail record ratified to stay, not a shape awaiting retirement — the columns hold what a row cannot: the signature, the raw score, the rule breakdown, the action).

### What's NOT in the row

- **The message body** — the scan-result row is envelope metadata only. The actual message bytes live in the per-user mailbox, sealed under the actor's MLS-derived key (unconditionally, since Phase 3 — `encryption-at-rest.md` § The sealed posture); the scan-result row holds only the classification outcome.
- **Per-user training context** — the per-user Bayesian classifier's model is held nest-side keyed by actor (the `spam_models` store; sealed under the actor's MLS-derived key at rest, target — today's gap is `mail-spam.md` § Implementation status item 4b) per `mail-spam.md` § Per-user model storage. The scan-result row records the rspamd-only pre-classifier output; the user-specific Bayesian score is computed later at the scoring position (post-delivery at the authenticated agent, uniformly — never at the nest — `mail-spam.md` § Scoring placement), never at this perimeter scan step.
- **The actor's reading status** — whether the user opened / replied / deleted the message is a user-content concern, not an deployment-data concern. The scan-result row is frozen at the time of scan.

### Deployment-data, NOT user content

The `message_scan_results` row is **deployment-data** — a verdict from the deployment's pre-classifiers about an inbound message. It's analogous to `smtp_verdicts` (per `smtp-server.md` § Log shape) — envelope metadata for operational + forensic purposes. The row is stored **plaintext on nest disk regardless of deployment storage mode**, same shape as the `smtp_verdicts` table (per `mail-observability.md` § Storage-mode interaction).

This is a deliberate split from user-content storage:

- In **plaintext-mode** deployments, both the message body and the scan-result row are plaintext; this poses no additional disclosure risk.
- In **encrypted-mode** deployments, the message body is sealed under the actor's MLS-derived key (per `docs/goal/architecture/encryption-at-rest.md`); the scan-result row is **deployment-data plaintext** — the box owner can see "this message had a ClamAV hit" + the rspamd score + the flagged rules, but the box owner cannot see the message body that triggered the verdict.

The defensible-privacy position: the verdict result is classification metadata (a 32-bit float + a rule-name list); the body that produced it is user content. We disclose the metadata; we don't disclose the content.

---

## Retention

Default 30 days. Admin-tunable via `mail.scanning.result_retention_days` (Tier 2). Cross-references `mail.observability.verdict_retention_days` (the `smtp_verdicts` table's retention, target-state default 7 days) — the two retention windows are configured independently and do **not** actually ship with the same default; that pipeline, including this knob's own write path, is unbuilt today regardless.

After retention, the `message_scan_results` row is pruned by a nest periodic GC pass — an independent per-table sweeper, on the same ~daily cadence the other deployment-data sweepers use (`alias_hits`, `bridge_audit`, `tlsrpt_outbound`), for operational symmetry with the eventual `smtp_verdicts` GC (which is itself not yet built). The per-user Bayesian model's n-gram weights are unaffected — they were absorbed into the user's classifier at delivery time; the per-message audit trail is independent of the classifier's persisted state.

### Retention with action_taken = 'rejected_malware'

Rejected-at-perimeter rows have no delivered-to-actor; they're admin-only forensic records. Default retention is the same 30 days. The admin may want longer retention for forensic / compliance reasons — `mail.scanning.rejected_malware_retention_days` (Tier 2, default null = falls back to `mail.scanning.result_retention_days`) is a per-action-category override.

---

## Cross-actor isolation

### Admin sees aggregate (not per-user-identifying)

The aggregate scan counts are admin-class data behind `fauna.bridges.list_scan_results` (**target-state, unbuilt** — § Implementation status today; the only surface for them today is host-side logs) plus a mail-health badge (a yellow/red signal) — **not** a rich dashboard card (the `mail-observability.md` "card catalog" is an unratified draft, not a ratified surface). The badge's former `admin-services` home was removed 2026-06-04 (admin.md § Admin IA redesign), so the badge readout is deferred (no current home; logs remain the live surface). **Outside the scope ratified 2026-09-25:** the mail health readout the user approved that day (`mail-deliverability.md` § Admin-pane Deliverability surface → *The mail health readout*) is deliverability-only — neither this aggregate scan view nor the scanner health badge is in its id set, so both stay deferred until raised as their own ask; nothing here rides in under the `admin-mail-health-*` ids. The aggregates the RPC returns:

- **ClamAV hits** — total infected-flagged messages in the window.
- **rspamd score distribution** — histogram of scaled scores across all inbound messages.
- **Top flagged rules** — the most-frequently-firing rspamd rule names in the window (top-20, with counts).
- **Action breakdown** — % delivered / % rejected_malware / % junked / % tagged.

The admin never sees per-message rows on the admin pane (the data exists in `message_scan_results`, but the admin-class RPC `fauna.bridges.list_scan_results` — target-state, unbuilt — returns aggregate counts + top-N samples, not raw rows tied to recipient identity).

### User sees their own message's row

The per-message detail surface in the user's mail-detail page (`mail-message-detail-scan-result-row`, on the general mail message-detail surface — § UX surface) shows the scan result for **their own** received message:

```
Delivered to: bob-amazon-AbCdEf@<our-domain>
ClamAV: clean
Rspamd score: 1.2 (BAYES_HAM, MIME_GOOD)
Action: delivered to INBOX
```

The user does **not** see scan results for messages they did not receive (the per-user query path scopes by `delivered_to_actor = <my-actor>`). Per-actor authorization is enforced by the WS-RPC handler.

### Per-account opt-out for visible scan details

Some users prefer a quieter UI without scan-result details (e.g., users who don't want a constant reminder of the inbound classification overhead). Tier-3 knob `mail.account.scan_result_visible` (default `true` — the user sees scan details by default; opt-out per-account).

When the toggle is `false`, the `mail-message-detail-scan-result-row` is hidden from the user's mail-detail page; the data still exists server-side (the admin's aggregate still includes the user's messages); only the per-message rendering is suppressed. (The per-account `mail.account.scan_result_visible` toggle itself is a Tier-3 user knob; if surfaced, it is a section on the `mail-spam` page — its ID allocated with that consuming track.)

---

## Encrypted-mode interaction

The scan runs at the **mail-bridge (MTA) perimeter on plaintext** — on the bytes the bridge already holds between **receiving the inbound message and sealing it** to the recipient. (Inbound external mail arrives in plaintext; there is no "unwrap" step — the seal is produced by the bridge at this point, not unwound.) The bridge holds the plaintext transiently; the nest never sees it. This is the perimeter scorer placement of `content-scoring.md` § The two plaintext positions in encrypted mode.

### Why the scan runs on plaintext

- ClamAV needs to match signature patterns against the raw message bytes (headers + MIME parts + attachment bytes). Scanning a sealed ciphertext would surface no signal — sealed bytes look random to any pattern matcher.
- rspamd needs to evaluate MIME structure, URL patterns, Bayesian n-grams over the body — same logic, plaintext-dependent.
- A "scan post-delivery on the user's plaintext" architecture is **structurally wrong** — by the time the message reaches the user's mailbox, it's already classified by their filter rules + the per-user Bayesian; the pre-classifier step needs to run before delivery.

### How the result is sealed

- The scan-result row (`message_scan_results`) is **deployment-data plaintext** — see § Deployment-data, NOT user content above.
- The message body re-sealed for delivery is **unaffected** by the scan — the scan reads, doesn't modify. The MTA stamps the `X-Fauna-Scan-*` headers on the message before re-sealing; the headers carry the verdict into the user's mailbox.
- The per-user Bayesian model is neither read nor trained at the perimeter: no per-user scoring or training runs at the unauthenticated MX (owner [`mail-spam.md`](mail-spam.md) § Scoring placement; the MTA is refused the training call outright). The model is trained when the user acts on a message, at the authenticated agent, and rests sealed under the actor's key (`mail-spam.md` § Encrypted-mode interaction). (This bullet placed the model update "at the same MTA-perimeter unwrap/re-seal point" until 2026-10-01, contradicting that owner and this doc's own status.)

### Operational implication

The bridge (MTA) process must be trusted with plaintext briefly. The deployment's encryption-at-rest contract (per `docs/goal/architecture/encryption-at-rest.md`) carves out the **perimeter's** transient plaintext access — the bridge holds the inbound message in plaintext between receipt and the seal it produces for delivery. Scanning happens inside that window. The carve-out is the perimeter's, **not** the nest's: the persistent nest store never holds the plaintext, which is why the scan cannot be a nest-side operation in encrypted mode (`content-scoring.md` § The two plaintext positions in encrypted mode).

---

## Wire shapes (named, not redefined here)

| RPC | Caller | Purpose | Notes |
|---|---|---|---|
| scan verdict on `fauna.bridges.ingest_inbound_mail` | MTA bridge | carry the scan verdict to the nest for storage | the bridge runs ClamAV + rspamd locally (shared `libs/fauna-mail` scanner clients) and adds the verdict fields (`clamav_verdict`, `clamav_signature?`, `rspamd_score_raw`, `rspamd_score_scaled`, `rspamd_flagged_rules`, `rspamd_score_breakdown`) to the existing `ingest_inbound_mail` request; the nest inserts the `message_scan_results` row. **No plaintext-to-nest RPC** — the scan runs at the perimeter, see `content-scoring.md`. |
| `fauna.bridges.report_rejected_scan` | MTA bridge | record the forensic `message_scan_results` row for a reject-at-perimeter (no `ingest_inbound_mail` call) | `(scan_id, clamav_verdict, clamav_signature?, rspamd_score_*, rspamd_flagged_rules, action_taken=rejected_malware) → ok`; metadata only, never the rejected bytes. The consuming track (tracked internally) may instead fold this into the verdict path if a no-store ingest variant is cleaner. |
| `fauna.bridges.list_scan_results` | admin client | **target-state, unbuilt** — aggregate counts for the admin surface | `(window_days, dimension)` → `{ clamav_hits, rspamd_score_histogram, top_flagged_rules, action_breakdown }`; never per-message-identifying |
| `fauna.bridges.get_message_scan_result` | user client | **target-state, unbuilt** — per-message detail for the user's own message | `(message_id)` → `ScanResult` row, scoped to the calling actor's owned messages |
| `fauna.bridges.refresh_clamav_signatures` | admin client | **target-state, unbuilt** — force-refresh signature DB (override the auto-update timer) | `()` → ok / error |
| `fauna.bridges.set_rspamd_rule_override` | admin client | **target-state, unbuilt** — enable/disable/weight-adjust a rule | `(rule_name, enabled?, weight?)` → ok |

Of this table, the two bridge-caller rows (the `ingest_inbound_mail` verdict fields + `report_rejected_scan`) are **live**; the four client-caller rows are target-state with no code yet (§ Implementation status today). Wire-level shape lives with the bridge-kind catalogue (`architecture/apps/bridges.md` § Bridge-kind catalogue (caller classes)); this doc owns which RPCs exist + what each carries.

---

## Architectural rules

- **ClamAV + rspamd are deployment-wide pre-classifiers.** Admin-configured, not per-user. Users cannot override rspamd rule weights (that's a privilege escalation).
- **Per-message scan results are envelope metadata, not user content.** Stored plaintext on nest regardless of storage mode (the row holds classification metadata; the body that produced the verdict is sealed per the storage-mode contract).
- **The scan runs at the mail-bridge (MTA) perimeter on plaintext, before sealing — never on the nest.** The bridge runs ClamAV + rspamd on the plaintext it holds between receipt and seal, in both storage modes; only the verdict metadata crosses to the nest. The nest never receives the plaintext, so the scan cannot be a nest-side operation in encrypted mode. This is the perimeter scorer placement of `content-scoring.md`; the transient-plaintext carve-out is the perimeter's, not the persistent nest store's.
- **The user sees their own message's scan result** (default `true`); opt-out per-account via `mail.account.scan_result_visible`.
- **The admin sees aggregate, never per-message-identifying.** The admin-class RPCs return counts + top-N samples, not raw rows.
- **rspamd rule overrides are the safety valve, not the default surface.** The deployment-default rule set is sensible; overrides are for the edge case where a rule over-tunes.
- **clamav-infected default action is `reject` at SMTP perimeter.** DSN to the sender; the message never enters the user's mailbox by default.
- **The scan result is one input to the combined-score formula.** rspamd's scaled score feeds into `max(rspamd_score, weighted_bayesian_score)` per `mail-spam.md` § Combined-score formula. ClamAV is a binary pass/reject signal — it doesn't feed the combined score; an infected message is rejected (or filed to Junk / tagged) at the pipeline step before the combined score is computed.
- **Retention is independent of message retention.** The scan result lives in deployment-data with its own retention (default 30 days); the message body lives in user-data with its own retention per the storage-mode contract.
- **Signature updates are hands-off.** Freshclam pulls from clamav.net continuously; the admin sees the status but doesn't manage signatures.

---

## Don't do these

- **Don't store the actual scanned bytes in `message_scan_results`.** Envelope metadata only. The body is stored per the storage-mode contract (sealed in encrypted-mode, plaintext in plaintext-mode); the scan-result row is deployment-data.
- **Don't surface per-user-identifying scan results on the admin pane.** The user's mail-detail page shows their own; the admin pane aggregates only.
- **Don't run the scanner on a message after it's been re-sealed.** The scan runs in the brief plaintext window at the MTA perimeter; after re-seal, the bytes are sealed under the actor's key + the MTA has no key to unwrap them. A scan-after-the-fact (e.g., re-scan with updated signatures) on encrypted-mode messages is structurally impossible without re-deriving the actor's key (which would violate the user-controls-their-data invariant).
- **Don't tie scan-result retention to message retention.** The result lives in deployment-data with its own retention; the message body lives in user-data with its own retention. Truncating one doesn't truncate the other.
- **Don't allow a user to override the deployment-wide rspamd rule weights.** Rule overrides are admin-tier only; a user with privilege to skew rspamd globally is a privilege escalation.
- **Don't deliver malware-flagged mail to the user's INBOX/Junk silently.** Default action is `reject` at SMTP perimeter (DSN to sender, message never stored). `junk` and `tag` are admin opt-ins with explicit consequences spelled out at the toggle.
- **Don't run rspamd against outbound mail by default.** rspamd's outbound use case (a deployment's own users sending spam-like patterns) is a different feature (related to `mail-deliverability.md` and the warmup story); inbound and outbound rspamd are separately configured if both are enabled. Outbound rspamd is **disabled by default** (the deployment doesn't second-guess its own users at submission time).
- **Don't bypass ClamAV for "trusted" senders by default.** A `cleartext_allowlist` allows STARTTLS-less senders (per `mail-policy-config.md` Tier 2) but does NOT bypass ClamAV — the scan runs regardless. An admin wanting to bypass ClamAV for a specific sender needs to disable ClamAV deployment-wide; per-sender ClamAV bypass is not a knob (the long-term concern: a misconfigured per-sender exemption is one of the highest-risk infrastructure mistakes).
- **Don't surface the rspamd raw score on the user's UI.** The user sees the scaled score (0–15) — same range as the per-user Bayesian + the combined score. Showing the raw 0–30 number would confuse: "why is my message's spam-score 8 but the system says it's not spam?" The scaled value is the operationally-meaningful one.
- **Don't tie the freshclam update to a hand-edited signature database.** Custom signatures are a code-change conversation, not a UI control. The signature DB is hands-off by design.
- **Don't store the rspamd rule descriptions in `rspamd_flagged_rules`.** Store rule names only (`BAYES_SPAM`, `URIBL_BLACK`). The description is fetched at render-time via the rspamd checkv2 metadata endpoint; storing the description duplicates state + drifts on rule update.
- **Don't fall back to "allow without scan" on a ClamAV outage.** If clamd is unavailable, the MTA tempfails (`451 4.7.0 Spam scanner unavailable, retry later`) so the sender's MTA retries. Failing-open to "allow without scan" would create a deployment-wide malware-bypass on outage. The same logic applies to rspamd: outage tempfails, retries; doesn't allow-without-score.
- **Don't leave the scan gate's resource use unbounded — but keep every guard fail-closed.** A wedged or compromised clamd/rspamd, or a flood of inbound DATA, must not exhaust the bridge (the gate sits on the unauthenticated inbound perimeter — exposed-ports security review, finding D7). The `scan_gate.go` guards: (1) the clamd/rspamd reply readers are length-bounded (`io.LimitReader`; an over-cap reply is a read error), so a daemon streaming unbounded bytes can't OOM the bridge; (2) a per-scanner **circuit breaker** opens after a run of consecutive failures and fast-tempfails (no dial) for a cooldown, so a daemon outage doesn't make every inbound message block the full scan timeout on a dead socket; (3) a process-wide **in-flight cap** bounds concurrent scans, tempfailing at capacity. Every one of these returns `Tempfail` (→ `451`), never allow-without-scan — they bound *resources*, they never weaken the malware gate (the rule above still holds on every path).

---

## Implementation status today

Goal-vs-code gap as of 2026-07-23 (re-verified this sweep against current code; the consumer reads this first):

- **The scan is live at the Go MTA perimeter (T1.4 complete: shared scorer + wire + nest storage, Go call-site + forensic path (tracked internally), 2026-05-23).** `libs/fauna-mail/src/scan/` holds the **pure** scan functions (`clamd_parse_reply`, `rspamd_parse_reply`, `decide_scan_action`), uniffi-exported to the Go binding. The scan **result types** (`ClamavVerdict`, `RspamdScore`, `RspamdRuleContribution`) live once in `fauna_core::mail_scan` and are re-exported by both `fauna_mail::scan` (the producer) and `fauna_protocol::bridge_routing` (the wire) — they were hand-mirrored in those two crates until 2026-08-18, when they were unified exactly as row 163 had done for the auth verdicts one family over. The **policy/action** types (`ScanPolicy`, `ScanAction`, `ClamavAction`) are still declared in `fauna-mail`; since UniFFI attributes a type to its defining crate, the Go binding now takes the result types from its `fauna_core` package and the policy types from `fauna_mail`. The Go MTA (`bins/fauna-bridges/internal/mta/scan_gate.go`) runs the scan on the plaintext `raw` after the spam gate and before sealing: it dials clamd (unix socket / TCP loopback), POSTs to rspamd `/checkv2`, calls `decide_scan_action`, stamps `X-Fauna-Scan-*` headers, and attaches `clamav_verdict` + `rspamd_score` to the `ingest_inbound_mail` request. `persist_inbound_mail_request` writes the `message_scan_results` row from those fields (deriving `action_taken`).
- **The scan logic is shared, not nest-side; the legacy scanners are dead.** Per `content-scoring.md`'s pure-scorer shape, the I/O (dial clamd / POST rspamd) is **Go-side** at the bridge perimeter (mirroring the spam gate); Rust owns only the parse/scale/decision. The new `scan` module **fixes the legacy `clamd.rs` fail-open bug**: only an explicit clean verdict counts as clean — `decide_scan_action` maps an `Error` verdict to `Tempfail` (→ `451`), and the Go gate also 451s on any clamd/rspamd dial/connect/timeout error (fail-closed, never allow-without-scan). `bins/fauna-nest/src/{clamd,rspamd}.rs` + `inbox_routes.rs` were deleted in the I6 mail-bridge cutover (2026-05-24) and were never the model.
- **Reject-at-perimeter forensic rows are built, and the retention GC now runs (tracked internally).** A `reject` action makes no ingest call, so the Go gate calls `fauna.bridges.report_rejected_scan` (MTA-class), which reuses `insert_scan_result` with `clamav_verdict='infected'`, `action_taken='rejected_malware'`, `delivered_to_actor=NULL`, and a synthetic deterministic `message_id`. Nest startup spawns `spawn_scan_result_retention_sweeper` (`db/bridge_routing.rs`, beside the `alias_hits`/`bridge_audit`/`tlsrpt_outbound` sweepers in `lib.rs`): on a 1/24-of-retention cadence it deletes rows past their window via `prune_scan_results_older_than`, with the `action_taken = 'rejected_malware'` forensic rows on a separate (per-action-category) cutoff. The 30-day default is the shared compile-time `fauna_mail::scan::SCAN_RESULT_RETENTION_DAYS`; the admin knobs (`mail.scanning.result_retention_days` / `rejected_malware_retention_days`) stay deferred with the rest of the inert `mail.scanning.*` Tier-2 catalog (next bullet) — the sweeper takes `None` for the rejected-malware override (falls back to the general window) until that write-path lands.
- **clamd + rspamd now ship as compose sidecars — the "works-out-of-the-box / tempfails-all-inbound" gap is CLOSED for the standard deployment, on BOTH published architectures (Stage 0 packaging; arm64 only since 2026-08-26, witnessed end to end on arm64 2026-09-26 — see the next bullet).** § ClamAV configuration says the deployment ships a co-located clamd + freshclam, and § rspamd a co-located rspamd; the scan is default-on (ScanPolicyDefault, clamav `reject`). These are provisioned as **separate compose sidecar containers** (`docker-compose.yml`: `clamd` = `clamav/clamav:latest-debian` on `:3310` — the tag as fixed 2026-08-26, see the next bullet; bundles freshclam and self-updates signatures on a timer; `rspamd` = `rspamd/rspamd:latest` on `:11333`; both refs digest-pinned since 2026-08-27, owned by [release-integrity.md](../architecture/release-integrity.md) § Third-party container images), reached by the bridge over the compose network via the topology hatch (`clamd_addr` / `rspamd_url`, defaulted from `FAUNA_CLAMD_ADDR` / `FAUNA_RSPAMD_URL`); `docs/goal/architecture/installers/docker.md` § docker-compose.yml documents the topology (both sidecars "required whenever mail is enabled"). So a standard compose deployment **scans** inbound rather than fail-closed-451'ing it. (The shape chosen was sidecar *containers*, **not** bundling the scanners into the nest image — the earlier "installer must add them to the image" framing is superseded.)

- **The bundle named an amd64-only scanner while the nest was published multi-arch — FIXED 2026-08-26; the pin is the durable part.** `clamav/clamav`'s **unsuffixed** tags (`latest`, `stable`, `1.4`, `1.5`) are published for **amd64 only**; the `-debian` / `-debian13-slim` family, from the same official publisher and automation, carries **amd64 + arm64 + ppc64le** (measured 2026-08-26 via `docker manifest inspect`, metadata only). All four shipping copies of the compose bundle named the unsuffixed `clamav/clamav:latest`, with no `platform:` key anywhere, while `build-nest-image.yml` builds, smoke-tests and publishes **both** `linux/amd64` and `linux/arm64`. Because the scan gate is default-on and fail-closes (`ClamavVerdict::Error` → `Tempfail` → 451) and the Tier-2 disable toggle is still inert, **every arm64 mail-enabled deployment 451'd all inbound mail forever, at shipped defaults, with no admin action able to clear it.** The fix is the tag: all four copies now name `clamav/clamav:latest-debian` (tag+channel chosen by the user 2026-08-26; every shipped third-party ref has since been pinned by manifest-index digest — [release-integrity.md](../architecture/release-integrity.md) § Third-party container images owns that ruling). **What stops it recurring is not the tag but the pin:** a dedicated dev-fleet test asserts every shipping copy names a sidecar image drawn from a vetted table of measured architecture coverage, that the copies agree, and that each shipped ref's coverage is a superset of the platforms `build-nest-image.yml` publishes — so adding an architecture to the nest image without re-vetting the sidecars now fails the merge (`installer-test`, cheap tier). A tag's architecture coverage is **not** inferable from its name, which is why the table records measurements rather than guesses. The mechanism is also proven end-to-end against fake daemons (`tests/e2e-unified/tests/test_mail_bridge_mta.py` — EICAR→554, clean→delivered). **The arm64 claim is witnessed, not only reasoned (2026-09-26):** `tests/e2e-unified/tests/platform/docker/test_mail_scan_real_clamd.py` (tier_4, opt-in via `FAUNA_E2E_REAL_CLAMD=1` because it pulls the third-party image) ran the published arm64 nest image against the host-architecture leg of the bundle's own digest-pinned clamd index, pulled by that leg's digest; a clean inbound was delivered and read back over IMAPS carrying `X-Fauna-Scan-Clamav: clean`, and an EICAR inbound was refused 5xx — the anti-vacuity half, since Fauna's fake clamd answers `OK` to EICAR (verified: wiring the fake in reds exactly that assertion). The test removes the clamd container and image when it ends. What genuinely remains is the **admin disable toggle** (the Tier-2 `mail.scanning.*` snapshot projection is still inert — next bullet), so scanning can't yet be turned off from the app; live-box sidecar *health* (reachability, RAM headroom for clamd's ~1.5 GB signature DB) is a deployment-ops concern, not a packaging gap.
- **The Tier-2 knobs are catalogued but inert.** `mail.scanning.*` (this doc's knob tables; mirrored in `mail-policy-config.md` § Content scanning) are not projected to the bridge and have no admin write path — they sit at compile-time defaults, in catalog "Bucket C" — neither projected to the bridge nor admin-writable (`mail-policy-config.md` § Implementation status today; the A3 Bucket B write path covers only the already-projected spam/auth/submission/imap/outbound sub-structs). Wiring them to app UI is out of T1.4 scope (admin UI is tracked internally / per-app tracks).
- **rspamd's routing feed landed with T3.1 (2026-05-24) — this bullet had gone stale describing it as still deferred.** The Go MTA now runs the combined-score disposition on every message reaching this step: `combined_spam_score_milli(rspamd_scaled_milli, weighted_bayesian_milli)` → `decide_spam_disposition` (`libs/fauna-mail/src/spam/mod.rs`, wired in `bins/fauna-bridges/internal/mta/server.go`), with the admin's `spam_folder`/`reject` tiers (`SpamPolicyThresholds`, default `spam_folder=5, reject=0`; `0 = disabled`) deciding routing — see `smtp-server.md` § Inbound pipeline step 4 and `mail-spam.md` § Implementation status today for the full build history. The per-user Bayesian arm still contributes a fixed `weighted_bayesian_milli = 0` at the perimeter (per-user scoring is forbidden at the unauthenticated MX perimeter — it runs post-delivery at the authenticated agent instead, per `mail-spam.md` § Scoring placement), so the perimeter combined score today equals the rspamd score. ClamAV's reject/junk/tag still short-circuits ahead of this step (a binary signal, not part of the combined score).
- **The oversize-bypass branch is UNREACHABLE at defaults — census run 2026-08-24, one verdict per door.** At census time the scan gate's cap sat above the pre-parser reject cap (`libs/fauna-protocol/src/bridge_routing.rs:1921`), so nothing that could trip the bypass survived the cap; since the 2026-08-26 fix (two bullets below) the scan gate no longer carries a fixed default at all — it takes the live ceiling directly — so the same relationship now holds by construction rather than by two constants happening to agree. The gate compares the **whole raw message** (`case len(raw) > maxFilesize`, `scan_gate.go:255`), not a per-file size, so no MIME/base64-inflation argument is involved — both caps measure the identical bytes. Per door: **inbound SMTP** — cap first, `server.go:1278` precedes the only non-test `applyScanGate` call at `:1414` in the same `Data`; **submission 465/587** — enforces the cap, never invokes the scan gate (outbound is not ClamAV-scanned, § above); **IMAP `APPEND`** and **client import** — enforce `effective_max_raw_message_bytes` nest-side (`bridge_imap_handlers.rs:1288`, `bridge_import_handlers.rs:433`) and run no ClamAV; **MDA capability drain** — calls `scan.Clamd` directly with *no* size gate (`rescore_drain.go:471`), so it scans at any size and can never emit `BypassedOversize`. ⚠ **The census's closing residue — a real one-knob crossing — is CLOSED (2026-08-26).** It read: `max_message_bytes` is admin-writable with no upper clamp while the scan cap is never projected, so one ordinary admin edit ("let users send 100 MB attachments") opened a silent ~47 MB band delivered **unscanned**. Both halves are now fixed — the gate takes the live ceiling and the knob has an enforced upper bound (next bullet). The census's *per-door* verdicts above still hold and should not be re-derived; only the compile-time-constant premise under them has moved.
- **`mail_scanning_clamav_oversize_total` is BUILT (2026-08-26) and is a tripwire, not a statistic.** Declared in `bins/fauna-bridges/internal/metrics/metrics.go` and incremented on the gate's `BypassedOversize` arm (`bins/fauna-bridges/internal/mta/scan_gate.go`), so the "did the bypass ever fire" question is now empirically answerable from the bridge's `/metrics`. Since the cap is derived from the ceiling the perimeter door already enforced, production cannot reach that arm at all: **any non-zero value is a bug** (a gate handed no ceiling), which is why it carries no labels and no per-deployment interpretation. Pinned by `TestApplyScanGate_OversizeBypassesWithoutDialingClamd` (fires on an injected tiny ceiling) and `TestApplyScanGate_RaisedCeilingStillScans` (must NOT fire inside the ceiling).
- **§ Oversize messages' ruling (2026-08-26) is BUILT for the gate + the knob's bound; the SIDECAR half is NOT, and is gated on a measurement.** What landed: `fauna_mail::transport_limits::MAX_MESSAGE_BYTES_CEILING` = 250,000,000 (its read-side clamp of a stored larger value was removed 2026-09-27 as a compat remnant); the nest's `put_spam_policy` refusing a larger write with `fauna.protocol.malformed` (`bins/fauna-nest/src/bridge_routing_handlers.rs`, beside the Bayesian-ramp refusal, pinned by `conformance_mail_policy.rs::admin_put_spam_rejects_max_message_bytes_above_the_product_ceiling`); `applyScanGate` taking the session's live `max_message_bytes` — the identical value the `552 5.3.4` door enforced for that message — instead of the deleted compile-time `defaultMaxFilesizeBytes`, with the now-dead `scan.Config.MaxFilesize` removed; the `mail_scanning_clamav_oversize_total` tripwire (bullet above); and `scan.ScanTimeout()` derived from the ceiling rather than the retired 50 MiB assumption. So **a raised ceiling no longer opens an unscanned band** — the gate scans everything the perimeter accepts, at every admin setting. **What is NOT built: the sidecar's own limits.** The compose bundle still ships `clamav/clamav:latest-debian` with stock config in all four copies (`docker-compose.yml`, the public dev-fleet installer script, `libs/fauna-provisioning/src/cloud_init.rs`, and the `docs/guides/nest-internet-setup.md` snippet), so clamd's own `StreamMaxLength`/`MaxScanSize`/`MaxFileSize` are whatever the image defaults to. **The consequence of the fix landing without it is a direction change, not a regression:** above clamd's stock stream limit the gate now fails **closed** (clamd `ERROR` → `Tempfail` → 451, `libs/fauna-mail/src/scan/mod.rs:139-143` → `:210`) where it previously delivered unscanned — correct per the never-allow-without-scan rule, and loud. Measured black-box over clamd's INSTREAM protocol against a shipped ClamAV image (the ALPINE variant originally measured; the bundle has since moved to `clamav/clamav:latest-debian` — bullet above — so this figure needs re-verifying against that image before it is relied on further): `StreamMaxLength` is exactly **104,857,600 bytes (100 MiB)** — 104,857,600 answers `stream: OK`, 104,857,601 answers `INSTREAM size limit exceeded. ERROR`. Nothing is silently skipped below that line: an EICAR attachment placed after 100,000,000 bytes of filler is still `FOUND`, so `MaxScanSize`/`MaxFileSize` do not truncate a message the stream limit accepted. **Two consequences.** (1) 104,857,600 sits *above* the 50,000,000 shipped reject cap, so at shipped defaults every admissible message is scanned and nothing tempfails; the historical 10M/25M figures in published clamd.conf documentation are not what this image ships. (2) The sidecar limits are still owed, and now have a number to beat: `MAX_MESSAGE_BYTES_CEILING` (250,000,000) is **above** 104,857,600, so an admin raising the ceiling past 100 MiB meets a fail-closed 451 wall — the gate hands clamd the larger stream, clamd refuses it, and `ClamavVerdict::Error` → `Tempfail`. Shipping the limits means shipping a whole config file (the ClamAV documentation records only a whole-file `/etc/clamav/clamd.conf` volume-mount override — no env-var and no `conf.d` mechanism), and landing that unwitnessed could stop clamd booting in every mail deployment.
- **A door that never invokes the scan gate records no ClamAV verdict (2026-09-28).** `ClamavVerdict` grew the additive `NotScanned` arm (`fauna_core::mail_scan`; the enum's `Default` and so the `#[serde(default)]` wire fallback, which had been `Clean`). The submission twin and the sender's own Sent copy (`fauna_recipient.go`) send it explicitly and mint their bus rows from it, the gate's disabled-scanner arm produces it instead of `Clean` (`scan_gate.go`), and the `wsrpc` chokepoint normalizes a zero-value verdict to it. Shared Rust treats it like `BypassedOversize`: `perimeter_mail_score_rows` emits no `clamav` row, `decide_scan_action` delivers (the never-allow-without-scan rule governs a scanner that ran), and no `X-Fauna-Scan-Clamav` header is stamped. Nest-side, `persist_inbound_mail_request` writes the `message_scan_results` row only when some scanner ran — `NotScanned` with no rspamd score ⇒ no row; beside an rspamd score ⇒ `'not_scanned'` in the column (§ Per-message scan-result storage). Pinned by the nest's `submit_inbound_mail_not_scanned_writes_no_scan_result_row` / `ingest_inbound_mail_not_scanned_twin_records_no_clamav_verdict` / `ingest_inbound_mail_not_scanned_beside_rspamd_keeps_the_row`, the Go `TestPerimeterMailScoreRowsSubmissionTwinClaimsNoScan`, and the Go wire-variant contract (fixture + `ClamavVerdictToWire` arm).
- **The read surface is unbuilt app-track work.** `fauna.bridges.list_scan_results` (admin aggregate), `fauna.bridges.get_message_scan_result` (user per-message), the `mail-message-detail-scan-result-row` (general mail message-detail surface), and the admin scanner-health signal (a mail-health badge, not a card; its former `admin-services` home was removed 2026-06-04 — admin.md § Admin IA redesign — so deferred, no current home) are all later per-app / admin tracks, not in T1.4. **2026-09-25:** the admin-mail health readout ratified that day (`mail-deliverability.md` § Admin-pane Deliverability surface → *The mail health readout*) does **not** cover the scanner signal or the aggregate scan view — they were outside the user's answer, so they remain unscheduled and need a separate ask (§ Signature management and § Admin sees aggregate carry the same note).

## Reading list

1. `principles.md` — § Product invariants (user always controls their data — the per-message scan result is deployment-data, but the result-row visibility on the user's UI is user-controlled; the rspamd rule overrides are admin-tier per the invariant); iron-clad cross-doc rule.
2. `docs/goal/behavior/smtp-server.md` § Inbound pipeline — the pipeline ordering this doc canonicalizes; § Spam handling — the rspamd score's feed into the per-user Bayesian.
3. `docs/goal/behavior/mail-spam.md` § Two-classifier pipeline — the rspamd → Bayesian feed; § Combined-score formula — the rspamd score's role in `max(rspamd, weighted_bayesian)`; § Encrypted-mode interaction — the at-rest seal shape this doc's `message_scan_results` row deliberately diverges from (this doc is deployment-data plaintext; mail-spam's model is sealed under the actor's key).
4. `docs/goal/behavior/mail-policy-config.md` § Tier 2 — the new knob rows for ClamAV + rspamd configuration; § Tier 3 — the `mail.account.scan_result_visible` per-account opt-out.
5. `docs/goal/behavior/mail-observability.md` § Cross-actor isolation — the admin-aggregate-only rule this doc inherits. (Its "card catalog" is an unratified draft, not a ratified admin surface — scanner health is a status badge per the mail-UX design pass (tracked internally, § DESIGN DECISION); the badge's former `admin-services` home was removed 2026-06-04 — admin.md § Admin IA redesign — so the readout is deferred, no current home.)
6. `docs/goal/architecture/encryption-at-rest.md` — the "MTA holds plaintext briefly between AEAD-unwrap and AEAD-re-seal" carve-out that scan-at-perimeter relies on.
7. **Mail-bridge topology hatch (`bins/fauna-bridges/internal/config`) — `clamd_addr` / `rspamd_url`** are the deployment-topology addresses for the perimeter scan (OS-level, not policy knobs), the analogue of the bridge's `mta_bind_addr`. The legacy nest-side `--clamd-host` / `--rspamd-url` flags in `docs/goal/architecture/nest/common.md` were for the retired in-nest scanner and do **not** drive the perimeter scan.
8. `docs/goal/architecture/apps/bridges.md` § Bridge-kind catalogue (caller classes) — where the scan-verdict fields on `ingest_inbound_mail` (T1.4) and the later `list_scan_results` / `get_message_scan_result` read RPCs land. There is no `scan_message_content` kind — the scan runs at the bridge perimeter, not the nest (`docs/goal/architecture/content-scoring.md`).
9. (design ratified 2026-05-07; tracked internally) § Inbound nest-side processing — the wire-shape this doc aligns with.
10. (design ratified 2026-05-08; tracked internally) — the AEAD shape; the deployment-data-plaintext-vs-user-data-sealed split this doc explicitly applies.
11. ClamAV documentation — clamd Unix socket protocol; freshclam update mechanism.
12. rspamd documentation — checkv2 HTTP/JSON protocol; default rule set; per-rule weight adjustment.
