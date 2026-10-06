# Mail deliverability — target state

Owns: deliverability, mail-health
Status: ratified
Authority: fresh-IP warm-up schedule + state model; blocklist self-check pipeline; postmaster-tools out-of-scope rationale; symptom-diagnostic checklist + audit. Defers retry machinery to behavior/smtp-server.md § Outbound delivery, knob tiers/naming to behavior/mail-policy-config.md, the verdict-log rollup (per-receiver success rate) to behavior/mail-observability.md, and DKIM/DNS record rendering to behavior/dns-management.md.

> **Purpose:** the operational outbound deliverability surface — fresh-IP warm-up, periodic blocklist self-check, Gmail/Outlook Postmaster-Tools out-of-scope rationale, and symptom diagnostics. The other mail docs cover the technical outbound stack (DKIM, SPF, DMARC, MTA-STS, DANE, TLSRPT, ARC); this one covers "our IP has no reputation / we're on a blocklist / gmail keeps tempfailing us." The admin UI surface is the **mail health readout** at the top of `admin-mail` (§ Admin-pane Deliverability surface → *The mail health readout*, ratified 2026-09-25; unbuilt on every app at ratification); the nest half behind it is largely implemented — see § Implementation status today.

---

## Goal

The operational outbound deliverability surface: everything beyond "we sign DKIM, we publish SPF, we enforce MTA-STS." The technical correctness work is done in the other mail docs; this doc covers the **operational symptoms** of being a fresh mail server on the internet:

1. **Fresh-IP warm-up.** A freshly-provisioned VPS has a new outbound IP with no reputation. Major receivers (Gmail / Outlook / Yahoo) throttle bursts on unknown IPs — sending 5000 mails on day 1 looks indistinguishable from a snowshoe spammer and trips rate-caps. The deployment ramps outbound volume daily over ~30 days until full volume is acceptable.
2. **Blocklist self-check.** A periodic query of our own outbound IP against a default set of major DNSBLs — Spamhaus zen, Barracuda, Spamcop. If we're listed, the admin sees it on the admin pane + can run the de-listing flow (typically a request submission at the blocklist provider's website).
3. **Out-of-scope-by-design: Postmaster Tools.** Gmail Postmaster Tools and Outlook Smart Network Data Services (SNDS) are receiver-side dashboards that require manual opt-in at Google's / Microsoft's web consoles. There's no API for automated integration. This doc names them as out-of-scope-by-design + links to the help URLs.
4. **Symptom diagnostics.** The admin runs diagnostics (the admin debug RPC surface today; a future `admin-mail` page affordance if one lands — § Admin-pane Deliverability surface) → synchronous check of SPF / DKIM / DMARC / MTA-STS / TLSRPT publication + reverse-DNS for outbound IP + outbound TLS posture check against a known peer (gmail.com). Results render in a check-list. No automated remediation — the diagnostic surfaces problems; the admin fixes them via the existing knobs.

**Bar: parity with the operational comfort that running a mail server in 2026 needs, without the admin having to know which blocklists to check or how to run an SPF lint.** The "works out of the box" invariant extends to operational visibility — a fresh deployment ships ramped-warmup-on + blocklist-self-check-on with sensible defaults.

**Default-on.** Both warmup and blocklist self-check ship enabled. Postmaster Tools is out of scope by design (no auto-integration; help-URL pointer in the admin pane only). The diagnostic is admin-on-demand, plus one run riding the self-check's daily tick for the mail health readout.

---

## Fresh-IP warm-up

### The problem

Gmail throttles outbound from unknown IPs at ~1000 messages/day on day 1, ramping up over weeks. Outlook + Yahoo have similar undocumented per-IP reputation gates. Sending the deployment's full expected volume on day 1 triggers `421 4.7.0 Try again later` tempfails (the deployment's outbound queue blooms) or worse, `550 5.7.1 Suspicious sending behavior` permfails (every recipient at that domain bounces).

### The ramp

The warmup schedule ramps outbound volume per day since first outbound mail:

| Day | Max mails / day |
|---|---|
| 1 | 50 |
| 2 | 100 |
| 3 | 200 |
| 4 | 400 |
| 5 | 750 |
| 6 | 1500 |
| 7 | 3000 |
| 8–13 | ramp from 3000 to 10000 (smooth) |
| 14 | 10000 |
| 15–29 | ramp from 10000 to 50000 (smooth) |
| 30+ | unlimited (subject only to per-actor rate caps per `mail-policy-config.md` § Submission policy) |

The schedule is **admin-tunable** via `mail.outbound.warmup_schedule` (Tier 2; map of `<day_since_first_outbound>` → `<max_mails_per_day>` for explicit days, with smooth ramping between defined days). The default above ships with the deployment image and reflects ~2026 receiver behavior; deployments with prior reputation at the major receivers can disable warmup entirely (`mail.outbound.warmup_enabled = false`).

### State model

`mail_outbound_warmup_state(first_outbound_at TIMESTAMP, current_day INTEGER, mails_sent_today INTEGER, mails_sent_total BIGINT, last_reset_at TIMESTAMP)`:

- One row deployment-wide (no `actor_id` column — the warmup is the deployment's IP, shared across all users). The implemented table guards this with `id INTEGER PRIMARY KEY CHECK (id = 1)`.
- `first_outbound_at` — when the deployment sent its first ever outbound mail; the day-since-first-outbound is computed from this timestamp.
- `current_day` — derived from `first_outbound_at` and `NOW()`; cached for fast lookup (always recomputed on read so it never goes stale).
- `mails_sent_today` — running counter, reset at 00:00 UTC daily.
- `mails_sent_total` — lifetime counter (admin interest; not enforcement; preserved across a manual reset).
- `last_reset_at` — when the admin last manually reset the warmup (e.g., after a deployment IP change).

The implemented table carries two additional **implementation-only** columns the abstract model above folds in:

- `counter_epoch_day` — the UTC epoch-day (`NOW()/86400`) that `mails_sent_today`/`current_day` were last computed for; the marker that drives the **lazy** 00:00-UTC daily reset (a rollover zeroes `mails_sent_today` on the next read/consume — no separate timer, so there is no timer-vs-deferral race).
- `deferred_total` — the lifetime `mail_outbound_warmup_deferred_total` metric (count of recipients deferred by the cap); preserved across a reset.

### Enforcement at submission time

The MTA, on receiving an authenticated submission at port 465/587, checks the deployment-wide warmup state:

1. Compute `today_used = mails_sent_today + (this message's recipient count)`.
2. Look up `max_for_today` from `warmup_schedule[current_day]`.
3. If `today_used > max_for_today`:
   - The message is **queued for tomorrow** (deferred, not bounced). Verdict `deferred_warmup`. The MTA returns `250 OK` to the submitting user (the user-side UX is "your message was accepted; delivery will start tomorrow"). Counter `mail_outbound_warmup_deferred_total` increments.
   - The user's app surfaces the deferred state in the sent-folder message view: a `mail-message-detail-warmup-deferred-row` shows "Delivery deferred until <date> per IP warm-up schedule. <count> messages ahead in the queue." (This is a row on the general mail message-detail surface — part of the mail client, not a `bridges-detail`/`account-detail` page; ID allocated on that surface with the consuming app track per the ui.yaml-owns-IDs split.)
4. Otherwise the submission proceeds normally.

### Manual reset

Warmup reset is an admin action via the debug RPC surface (`fauna.bridges.outbound_warmup_status` + a reset call), used only when the deployment's outbound IP has changed (e.g., after a VPS migration) → on confirm: `mail_outbound_warmup_state.first_outbound_at = NOW()`; `current_day = 1`; `mails_sent_today = 0`; `last_reset_at = NOW()`. The lifetime counter (`mails_sent_total`) is preserved. (If a future `admin-mail` page lands per the mail-UX design pass (tracked internally), the reset affordance + its ui.yaml ID land there; it is not a `bridges-detail` card button.)

### Upgrade seeding

**Retired with the genesis (2026-09-24).** A one-time seed once kept a deployment that *adopted* the warm-up feature with mail already enabled — the `mail_outbound_warmup_state` table created for the first time on an existing `/data` — from being treated as a fresh IP: it stamped `first_outbound_at` from `mail_enabled.set_at`. It served only databases older than the feature. Since the nest schema's genesis ([`../architecture/nest/common.md`](../architecture/nest/common.md) § Database) every nest database carries the table from its first boot, before mail can be enabled, so no such adoption can happen and the seed went with the history (`../architecture/version-compatibility.md` § Dimension 2, the fourth ratified exception). The rule it protected still binds: an established ramp never moves backwards (§ Don't do these).

### Why per-deployment, not per-actor

The IP reputation is the **deployment's IP** — every user shares it. A single high-volume user could trip the warmup on day 2 (which is correct behavior: the deployment's reputation is what receivers throttle on). The per-user UX for hitting the warmup cap is the friendly "your messages will start delivering tomorrow" tempfail, not a per-user error.

### Why a queue-tomorrow default

Bouncing at warmup would surface a confusing error to the user ("your mail was rejected because IP warmup"). Queuing-tomorrow is the long-term-defensible choice — the user's message lands within 24 hours; no error message; counter visible in admin pane.

---

## Blocklist self-check

### The check

Every 24 hours (timer-triggered; default starts at 03:00 UTC ± random offset to avoid synchronized check-storms across deployments), **nest** queries our own outbound IP against the DNSBLs in `mail.outbound.blocklist_self_check_servers` (Tier 2; default `["zen.spamhaus.org", "b.barracudacentral.org", "bl.spamcop.net"]` — this doc's own default, deliberately broader than the inbound `mail.inbound.dnsbl_servers` default of `["zen.spamhaus.org"]` alone: a self-check should sweep the lists major receivers consult even if we don't gate inbound on them. SORBS was dropped from the default 2026-07-08 — the service was decommissioned in 2024, so querying it yields a permanently-misleading NXDOMAIN "not listed" ✓). The check is a nest-internal timer (`run_scheduled_blocklist_self_check`), not an MTA-side or separate-daemon concern.

The query is a standard DNSBL lookup: `<reversed-outbound-IP>.<dnsbl-suffix>` resolves to an A record (typically `127.0.0.2` for "listed") if the IP is listed, or NXDOMAIN if not listed. Counter `mail_outbound_self_blocklist_total{server="..."}` increments on each check.

### Storage

`mail_outbound_self_blocklist_check(checked_at TIMESTAMP, results_json JSONB)`:

- One row per check (so the admin can see history).
- `results_json` carries per-server outcome: `{"zen.spamhaus.org": {"listed": false, "txt_record": null}, "b.barracudacentral.org": {"listed": true, "txt_record": "Listed for spam pattern X"}, ...}`.
- Retention 90 days (admin-tunable `mail.outbound.self_blocklist_retention_days`).

### Admin-pane rendering

The blocklist self-check surfaces per-DNSBL status (rolled into the deliverability mail-health badge / admin debug surface — see § Admin-pane Deliverability surface; the badge's former `admin-services` home was dropped with that page in the 2026-06-04 redesign — admin.md § Admin IA redesign — and the readout is deferred) as a colored status:

- Green ✓ — not listed.
- Red ✗ — listed (with the TXT-record reason inlined if the DNSBL returned one).
- Yellow ? — query returned a resolver error (timeout, SERVFAIL) — counter `mail_outbound_self_blocklist_error_total{server="..."}` increments; the prior result is preserved until the next successful query.

On a listed result, the mail health readout (§ Admin-pane Deliverability surface) offers a one-click "Open de-listing URL". The URL is a compile-time constant per known DNSBL — Spamhaus de-listing, Barracuda removal request, Spamcop removal — and **the readout carries one link, not one per list** (the approved id set has a single `admin-mail-health-delist-link`): it opens the removal page of the first list the address is on that has one, while the self-check row's detail names every list it is on, so an admin listed on several works through them — clear one, check again, and the link moves to the next. (Corrected 2026-10-01: this paragraph had read as one link per list.) The actual de-listing is the admin's responsibility (most DNSBLs require human review).

### Force-refresh

An admin-initiated re-check (via the debug RPC `fauna.bridges.blocklist_self_check_run`, or a future `admin-mail`-page affordance) fires the query immediately (bypassing the 24h timer for that one DNSBL); counter `mail_outbound_self_blocklist_force_refresh_total`. Rate-limited to once per minute per DNSBL (defends against an admin-loop hammering the DNSBL).

### Disable

`mail.outbound.blocklist_self_check_enabled = false` (Tier 2; default `true`) stops the timer. Useful for deployments with policy-strict outbound (e.g., behind a corporate proxy where the deployment's outbound IP doesn't even reach the public internet); turning this off accepts blind operation.

---

## Out-of-scope: Gmail Postmaster Tools + Outlook SNDS

Gmail Postmaster Tools (postmaster.google.com) and Outlook Smart Network Data Services (SNDS — sendersms.outlook.com) are **receiver-side dashboards** that admins of mail-sending domains can opt into to see per-receiver-side metrics (Google's view of our spam rate; Microsoft's view). **Neither has a programmatic API** — both require:

1. Manual admin login at the receiver's web console.
2. Manual domain verification (DNS TXT record or upload a file at a well-known URL).
3. Manual review of dashboards on the receiver's website.

Fauna **does not automate** the setup or read the postmaster-tools data back. There's no API to integrate with.

### The defensible alternative

The receiver-side feedback we **do** see comes from:

- **Peer-published DMARC reports** (per `dmarc-reporting.md` § Aggregate-report receive) — receivers that publish `rua=` in their DMARC policy send us aggregate reports about how their MTAs handled mail from our domains. Gmail does this; we get daily reports. The reports include source IP, message count, disposition (none/quarantine/reject), DKIM/SPF alignment. This gives us most of the operational signal Postmaster Tools renders.
- **TLSRPT** (per `smtp-server.md` § TLSRPT inbound report listener) — receivers report TLS failures they observed delivering to us.
- **Bounce verdicts** (per `smtp-server.md` § Outbound delivery → bounce classification) — when our outbound mail tempfails or permfails, we see the receiver's SMTP response text directly.

These three give the admin enough signal to diagnose deliverability without setting up Postmaster Tools / SNDS manually.

### The admin-pane surface

Postmaster Tools is documented as a manual, out-of-scope concern (static help text + URLs, no integration), surfaced wherever the deliverability health detail lands — which since 2026-09-25 is the mail health readout. **Unbuilt, and it has no element yet:** the nine ids approved for the readout hold none for this note (§ Admin-pane Deliverability surface → *The mail health readout*, the *ui.yaml scope* paragraph), so it waits on an id of its own being approved. When it lands it:

- Renders a help text: "Gmail Postmaster Tools and Outlook SNDS are externally managed dashboards on the receiver's website. Sign up at postmaster.google.com and sendersms.outlook.com if you want their per-receiver-side view; Fauna doesn't integrate with these APIs (none exist)."
- Provides links to the help URLs (target=_blank, hard-coded).
- Doesn't query them, doesn't display their data, doesn't track whether the admin has signed up.

This is intentionally a passive surface — naming the option so the admin knows it exists, without pretending to integrate.

---

## Symptom diagnostics

An admin runs diagnostics (via the debug surface, or a future `admin-mail`-page affordance) → triggers `fauna.bridges.run_deliverability_diagnostics()` synchronously → returns a check-list. The diagnostic runs each check sequentially (some have network calls; the whole run takes 5–30 seconds depending on DNS responsiveness).

### Checks

| Check | What it does | Pass / Fail signal |
|---|---|---|
| **SPF record present** | Query `<primary-domain> TXT`; look for `v=spf1 ...` | Pass: SPF found + parses; Fail: no SPF found OR SPF doesn't include `mx` mechanism (this deployment relies on `mx` for SPF) |
| **SPF record valid** | Parse the SPF; check for the `>10 DNS lookup` RFC 7208 §4.6.4 issue | Pass: SPF parse succeeds + lookup-count ≤ 10; Fail: parse errors / too many lookups |
| **DKIM record present (per active selector)** | Query `<selector>._domainkey.<primary-domain> TXT`; look for `v=DKIM1; ...; p=<pubkey>` | Pass: TXT found + has the expected pubkey (matches the deployment's wrapped private key's public counterpart per `mail-multidomain.md` § Per-domain DKIM); Fail: TXT missing OR pubkey mismatch |
| **DKIM record per algorithm** | Same as above for each algorithm in `mail.dkim.algorithms` (default `ed25519` + `rsa-2048`) | Pass per algorithm: TXT found; Fail per algorithm: TXT missing |
| **DMARC record present** | Query `_dmarc.<primary-domain> TXT`; look for `v=DMARC1; ...` | Pass: DMARC found; Fail: missing |
| **DMARC policy is enforcing** | Parse the DMARC; check `p=` is `reject` or `quarantine` (not `none`) | Pass: `p=reject` or `quarantine`; Warn: `p=none` (monitor-only); Fail: missing |
| **MTA-STS record present** | Query `_mta-sts.<primary-domain> TXT`; look for `v=STSv1; id=...` | Pass: TXT found; Fail: missing |
| **MTA-STS policy file fetchable** | HTTPS GET `https://mta-sts.<primary-domain>/.well-known/mta-sts.txt` | Pass: 200 with valid policy file content; Fail: 4xx/5xx/network error/invalid content |
| **TLSRPT record present** | Query `_smtp._tls.<primary-domain> TXT`; look for `v=TLSRPTv1; rua=...` | Pass: TXT found; Fail: missing (`fauna_mail::deliverability::tlsrpt_record_present`) |
| **Reverse-DNS for outbound IP** | Query `<reversed-outbound-IP>.in-addr.arpa PTR` | Pass: returns a hostname; Fail: NXDOMAIN |
| **Reverse-DNS matches HELO** | Compare the PTR result with `mail.<primary-domain>` (the HELO we send) | Pass: PTR == HELO; Fail: PTR differs (admin-fixable at the VPS provider's networking page) |
| **Outbound TLS to gmail.com** | Connect to `gmail-smtp-in.l.google.com:25` (gmail's MX); STARTTLS; verify cert path | Pass: STARTTLS succeeds + cert chain valid; Fail: connection refused / cert path invalid / STARTTLS rejected |
| **Blocklist self-check (refresh)** | Re-run § Blocklist self-check on demand — via the separate `blocklist_self_check_run` RPC, not merged into the same `run_deliverability_diagnostics()` call (verified against `bins/fauna-nest/src/bridge_routing_handlers.rs`: the diagnostics handler calls only `mail_deliverability::run_diagnostics`, never `run_blocklist_self_check`; § Implementation status today's "Landed" bullet for `run_deliverability_diagnostics` correctly omits this row for the same reason) | Pass: all DNSBLs return not-listed; Fail per DNSBL: listed |

### Output rendering

Each check produces one result row (check name, pass/fail/warn, plain-language description of failure). Diagnostic results are returned by `fauna.bridges.run_deliverability_diagnostics` and surfaced through the admin debug surface / logs (and, if a future `admin-mail` page lands, rendered there with IDs allocated then). Because DKIM/TLS/MTA-STS are auto-provisioned, most "Fix" actions are automatic; the few admin-actionable ones point at their flat home (a failing SPF/DMARC/MX record shows red on `admin-dns`; reverse-DNS mismatch is VPS-provider-side — the description explains, no fix affordance).

### No automated remediation

The diagnostic **surfaces problems**, the admin fixes them via the existing knobs / VPS-provider settings. Auto-remediation (e.g., "I'll fix the SPF for you") would create silent reconfiguration; the admin needs to see + approve each change.

### Admin-visible audit

Diagnostic runs are logged in `mail_outbound_diagnostic_runs(ran_at, results_json, ran_by_actor_id)` — admin-only access. Retention 90 days (admin-tunable `mail.outbound.diagnostic_retention_days`). The audit lets the admin see "I ran diagnostics on 2026-05-01, DKIM was failing, I fixed it on 2026-05-02, diagnostics passed."

---

## Admin-pane Deliverability surface

> **Ratified 2026-06-01 by the mail-UX design pass (tracked internally).** This section previously proposed a rich `bridges-detail-mail-deliverability-card` — the "seventh card" on a `bridges-detail` mail dashboard, alongside `mail-observability.md`'s six-card catalog. That premise is **superseded** and the `bridges-detail-mail-deliverability-*` IDs are **dropped**: there is no `bridges-detail` page or six-card observability dashboard in ui.yaml, and the 2026-05-24 user directive dropped the `admin-mail-deliverability` page entirely (DKIM/TLS automatic; deliverability health → a status badge, **not** a generic error feed or rich card; its former `admin-services` home was in turn dropped with that page in the 2026-06-04 redesign — admin.md § Admin IA redesign — which left the badge readout homeless until the 2026-09-25 ruling below gave it one).

**Where deliverability surfaces now (flat admin pane):**

- **Health at a glance** → the **mail health readout** at the top of `admin-mail` (the subsection below — ratified 2026-09-25): a categorical status line ("Mail: delivering / warming up / blocklisted / …") over one server-side fold of state the nest holds, plus seven check rows and two action buttons. The former `admin-services` badge home (alongside `admin-service-bridge-status`) was removed in the 2026-06-04 redesign (admin.md § Admin IA redesign); `admin-nest`/`admin-dns`, floated in between as homes, were rejected by the design pass (reasons below).
- **Rejected-by-peer, per-receiver success-rate, the diagnostic-run history** → still admin observability via the structured logs + the admin debug RPC surface (`run_deliverability_diagnostics`, `mail_outbound_diagnostic_runs`), not a per-element UI card; the readout shows only the *latest* diagnostics run and the *latest* self-check. Anything richer (volumes, rejection reasons, per-receiver rates) is the demand-driven verdict pipeline, `mail-observability.md` § Design pass 2026-09-19 — this doc does not pre-invent its IDs.

The behavior + data + RPC + wire sections below (warmup state machine, blocklist self-check, diagnostics) stand unchanged; only the `bridges-detail` UI rendering is dropped.

### The mail health readout on `admin-mail` (design pass 2026-09-19; ratified 2026-09-25)

> **Ratified 2026-09-25 by the user:** the readout is **owed now**, with rule-A sign-off on the id set below and on the home (the top of `admin-mail` plus the dashboard's Mail stat card); the verdict-telemetry pipeline stays demand-driven (`mail-observability.md` § Design pass 2026-09-19 owns the two-tier split and its triggers). The ids landed in ui.yaml the same day. **Unbuilt on every app at ratification** — § Implementation status today names the build rows. The readout gives the formerly homeless badge a home and widens it just enough to meet `mail-observability.md` § Goal for the common deployment without the verdict pipeline.

**Home: a read-only health section at the top of `admin-mail`, directly under `admin-mail-enabled-toggle`.** The admin who turns mail on sees whether it works on the same page; precedent is the host-OS status line living on its feature page (`admin.md` § N Nest, `nest-os-maintenance-status`) rather than on a hub. `admin-nest`/`admin-dns` (the homes earlier prose floated) are rejected: `admin-nest` is nest-wide deployment facts, and `admin-dns` would hide a blocklisting or a dead bridge behind a records page. The section adds no nav entry on any of the 7 apps. The same categorical state additionally renders as one more `admin-stat-card` instance ("Mail") on `admin-dashboard` — the existing component, no new id — so a broken state is visible on the shell's landing page.

**One server-side fold, one read.** A new Admin-class `fauna.bridges.mail_health` `()` returns the categorical `state` plus the facts behind it; the state decision is a pure shared fold (`fauna_mail::health`, WASM-safe, the nest calls it; apps dumb-render it through one shared label function, the `os_maintenance_status_label` pattern). The wire `state` is an open string enum — an app that meets a value it does not know renders the generic "needs attention" label, so a newer nest never breaks an older app. Worst state wins, in this order:

| `state` | Meaning | Source (all nest-side) |
|---|---|---|
| `off` | mail is not enabled (neutral, not an alarm) | `mail_enabled` |
| `bridge_down` | mail is enabled but an approved MTA or MDA bridge has no live connection (or no MTA/MDA bridge is approved at all — nothing can carry the mail) | **net-new projection** — `WsState::has_connections` over the approved `bridge_service_users`; the nest holds the raw fact but exposes it nowhere today (the `connected` flag on `fauna.admin.worker.status` is the storage worker, not the mail bridge) |
| `blocklisted` | the latest self-check lists the outbound IP | latest `mail_outbound_self_blocklist_check` row |
| `queue_stalled` | at least one `pending` outbound row has already tripped the existing delayed-delivery warning (a row that has **failed at least once** and is older than the outbound policy's `delay_warning_at_hours`, default 4 h) | `outbound_mail_queue`, the same condition `mark_outbound_failed` already warns on — no new threshold and no new knob. Deliberately **not** a bare age test: a warm-up-deferred row is `pending` with an old `created_at` and no failure (§ Implementation status today), and must never read as a stall |
| `records_failing` | the latest diagnostics run has a failing SPF/DKIM/DMARC/rDNS check | latest `mail_outbound_diagnostic_runs` row — the 24 h self-check tick also persists one diagnostics run, so a fresh row always exists |
| `warming_up` | healthy, inside the fresh-IP ramp | `mail_outbound_warmup_state` |
| `delivering` | healthy | none of the above |

**Two heartbeat stamps — the cheap answer to "delivered?" and "received?".** One deployment-wide row carries `last_outbound_delivered_at` (stamped in `mark_outbound_delivered`) and `last_inbound_accepted_at` (stamped in `ingest_inbound_mail`); no actor, no domain, no message identity — so `mail-observability.md` § Cross-actor isolation holds trivially. They render as facts ("last delivered 3 min ago"), never as a state input: an idle box is not a broken box, and a wall-clock silence threshold would be a guess. **Every app renders the same line and the same two rows through shared Rust:** `fauna_core::format::mail_health_heartbeat_label` maps a stamp (`None` → "Never") onto the shared relative-time display, and `mail_health_status_text` composes the status line from the one `admin.mail_page.health_status_line` template (`{state} — last delivered: {delivered} · last received: {received}`); the two heartbeat rows (indices 5 and 6) carry an empty `detail` from the fold on purpose, and the app paints the same heartbeat text there.

**ui.yaml scope (ids user-approved 2026-09-25; page `admin-mail`, landed the same day):** elements `admin-mail-health-section` (view), `admin-mail-health-status` (text — the categorical line plus the two heartbeat facts); component `admin-mail-health-check` (indexed rows — `admin-mail-health-check`, `-label`, `-state`, `-detail`: bridge connection · blocklist self-check · outbound queue · DNS/auth records · warm-up ramp · last delivered · last received, in that order); `admin-mail-health-delist-link` (present only while a self-check row lists the outbound IP — the per-DNSBL de-listing URL of § Blocklist self-check), `admin-mail-health-recheck-button` (runs `blocklist_self_check_run` + `run_deliverability_diagnostics`, then re-reads) and `admin-mail-health-warmup-reset-button` (confirm-gated `outbound_warmup_reset`, the after-an-IP-change act). **The confirm is the same-button two-click on every app** — ui.yaml scopes no confirm id to it (nine ids in all: the section, the status line, the check row and its three leaves, the delist link, the two buttons), so the first press only relabels the button with the confirm sentence (`admin.mail_page.health_warmup_reset_confirm`) and the second dispatches `ResetWarmup` and disarms: the shape `mail-spam-reset-model-button` already uses, which the cross-app driver drives as two clicks. The two buttons exist because both acts were raw-RPC-only, which the one-configuration-surface invariant (`principles.md` § One configuration surface) makes a gap rather than a posture; neither edits policy, so the viewer-not-editor rule of `mail-observability.md` § Architectural rules is untouched. This supersedes the "detail lives in logs, not a UI card" half of the 2026-06-01 framing for these seven checks only — the reason being that the mail bridge's per-transaction verdict lines never reach `admin-logs`: `fauna.admin.logs` merges the nest's own ring with the sidecar log plane's remote ring (`architecture/apps/observability.md` § The sidecar log plane), and the Go mail bridge reports into that plane only its fixed lifecycle catalogue (ready, TLS-cert fetch failed, listener bind failed, nest reconnected, forced shutdown, confinement degraded — `bins/fauna-bridges/internal/logplane/catalogue.go`), deliberately never a tee of its SMTP log stream (verified 2026-09-27 by the nest build). **Not in this set, by the same ruling:** the sender-dashboard note (§ Out-of-scope: Gmail Postmaster Tools + Outlook SNDS → *The admin-pane surface*), any tier-2 telemetry id, the DMARC report summary (`dmarc-reporting.md` § ui.yaml surface stays proposed), the scanner health signal and aggregate scan view (`mail-content-scanning.md`), and inbound TLS-report inspection (`smtp-server.md` § TLSRPT inbound report listener) — each stays where its owner doc leaves it and needs its own ask.

**Build order:** shared fold + wire types (`fauna_mail::health`, `fauna-protocol`) → nest (`mail_health` handler, the two stamps, the bridge-connection projection, the timer-driven diagnostics run) → tui (lead app) → the other six in their batched trickle-down. The first implementation mints `docs/features/<slug>.md` and updates `docs/guides/`.

---

## Implementation status today

**The mail health readout (§ Admin-pane Deliverability surface → *The mail health readout*) is built through shared Rust + nest + the client seam, and renders on tui (the lead app, 2026-09-28) — the other six apps owe it.** Landed: the pure fold `fauna_mail::health` (`evaluate` — the worst-wins state, the seven check rows, the `queue_row_stalled` predicate, the compile-time per-DNSBL de-listing URLs), the open-enum labels `fauna_core::format::mail_health_state_label` / `mail_health_check_state_label` (+ their UniFFI and wasm twins), the Admin-class `fauna.bridges.mail_health` → `MailHealthReply` (`fauna-protocol`), the two heartbeat stamps on the one-row `mail_heartbeat_state` table (stamped in `mark_outbound_delivered` / `ingest_inbound_mail`), the bridge-connection projection (`WsState::has_connections` over the approved out-of-process MTA/MDA `bridge_service_users`), the diagnostics run on the 24 h self-check tick (recorded with an all-zero `ran_by_actor_id` — the nest itself), and the client seam on `MailPolicyMachine` (`MailPolicySnapshot::health`, `MailPolicyAction::{RecheckHealth, ResetWarmup}`, a refusal degrading to `None`). **tui renders it** (`apps/fauna-tui/src/admin/mail.rs` `health_elements`, the dashboard's Mail card in `admin/dashboard.rs`): all nine `admin-mail-health-*` ids under the enable toggle, the status line and heartbeat rows through the shared `mail_health_status_text` / `mail_health_heartbeat_text` (their pure half, `mail_health_heartbeat_label`, is the FFI/wasm-shaped door), the warm-up reset as the same-button two-click, and the sixth `admin-stat-card` (Mail); proven by `tests/e2e-unified/tests/test_admin_mail_health.py`. Still owed: the other six apps, in one batched trickle-down — and before the non-Rust apps can call them, UniFFI and wasm twins of `mail_health_heartbeat_label` (the two label functions already have theirs).

Landed (tracked internally, nest-side):

- **`fauna.bridges.run_deliverability_diagnostics`** (Admin) — the full § Symptom diagnostics check set: SPF present/valid, DKIM present + pubkey-match per active selector, DMARC present + enforcing, MTA-STS record present + policy-file fetchable, TLSRPT present, reverse-DNS present + matches HELO, outbound STARTTLS posture to gmail. Persists a `mail_outbound_diagnostic_runs` audit row.
- **`fauna.bridges.blocklist_self_check_run`** (Admin force-refresh + nest-internal 24h timer, default-on, ~03:00 UTC ± offset) — the DNSBL self-check; persists `mail_outbound_self_blocklist_check`; per-DNSBL force-refresh rate-limit (1/min); 90-day retention prune.
- The reusable pass/warn/fail verdicts live in `fauna_mail::deliverability` (SPF lint, DMARC mode, MTA-STS/TLSRPT/DKIM presence + DKIM pubkey-match, DNSBL query/interpret); the async orchestration + the `StarttlsProber` seam are nest-side (`bins/fauna-nest/src/mail_deliverability.rs`).

Landed (tracked internally, nest-side — the fresh-IP warm-up half):

- **Ramp curve** — `fauna_mail::warmup::max_for_day(day) -> Option<u64>` (shared, pure, WASM-safe; `None` = day-30+ unlimited), piecewise-linear between the § The ramp anchors. The day-counter `mail_outbound_warmup_state` (one deployment-wide row) + the atomic check-and-consume (`db::mail_warmup::try_consume_warmup`) are nest-side.
- **Submission-time enforcement** — fires inside `submit_outbound` wherever the caller opts in: the authenticated 465/587 path (`class == BridgeMta`) **and** the mailing-list send fan-out (`fauna.bridges.send_list_message`, unconditionally — per `mail-mass-mailing.md` § Reading list item 7, list-mode submissions count against the same deployment-wide quota, since they leave via the same shared outbound IP); the MDA calendar-auto-schedule gateway, auto-reply, and NDR/bounce paths route through the same chokepoint with warm-up enforcement **off** and are exempt — never deferred. The cap is over the EXTERNAL recipients that actually leave via the outbound MX (post in-domain partition). Over today's cap → the whole message is queued for tomorrow (`next_attempt_at = next 00:00 UTC` via `enqueue_outbound_split`, `created_at` kept honest), the MTA still returns `250 OK`, never bounced; `deferred_total` increments.
- **`fauna.bridges.outbound_warmup_status` / `outbound_warmup_reset`** (Admin) — read the deployment-wide state / reset the ramp to day 1 after an outbound-IP change (preserving `mails_sent_total`). On the same `outbound_now()` clock seam as the enforcement.
- **Upgrade seeding (§ Upgrade seeding)** — retired with the genesis (2026-09-24): every nest database has the warm-up table from its first boot, so the one-time seed for a box adopting the feature has no input.

Landed (tracked internally, nest-side — the history/observability **read** half):

- **`fauna.bridges.list_blocklist_self_check_history`** `(window_days)` (Admin) — newest-first blocklist-self-check history within the window (`≤ 0` ⇒ the full 90-day retention), reading the persisted `mail_outbound_self_blocklist_check` rows. Each row's stored JSON is parsed back into the same per-DNSBL `results` shape the live run returns (apps render typed rows; no per-app JSON parse).
- **`fauna.bridges.list_deliverability_diagnostic_runs`** `(limit)` (Admin) — newest-first diagnostic-run audit (`≤ 0` ⇒ 100, max 1000), reading `mail_outbound_diagnostic_runs`; `checks` is the same checklist the live run returns, plus the `ran_by_actor_id`. Admin-only (§ Admin-visible audit); a malformed/legacy stored row degrades to an empty verdict list rather than failing the read (the audit timestamp is the load-bearing part).

Caller-class resolution (was an internal contradiction in this doc): the old § Status labeled these "MTA-class", but § Wire shapes names the *admin-client* caller and § Architectural rules says "Symptom diagnostic is admin-pane-only". **Resolved toward Admin-class** (the implemented state); the gate is in `bins/fauna-nest/src/bridge_method_allowlist.rs`.

Documented assumptions / deferred gaps (target-state prose above that code does **not** yet implement):

- **Outbound IP** is derived as the A/AAAA of `mail.<primary-domain>` (the advertised mail host / HELO). Correct for the single-homed VPS deployment; a split outbound IP (multi-homed host) is a follow-on (no STUN/echo discovery today).
- The **gmail STARTTLS probe** is real (tokio + tokio-rustls + webpki roots) but its network path is only exercisable on a deployment with outbound `:25` reach; `for_test` installs a `NullStarttlsProber` (a Warn row).
- **Deferred-mail counting is submission-time, not delivery-time.** Per § Enforcement at submission time, the counter is consumed at submission and an over-cap message is stamped `next_attempt_at = tomorrow 00:00 UTC` — so it "lands within 24 hours" exactly as § Why a queue-tomorrow default promises, but a single very large burst on day N can exceed day N+1's intended cap on the wire (the deferred batch is not re-checked against the next day's cap at delivery). This matches the doc's literal submission-time algorithm; a queue-runner-side re-check (delivery-time counting) is a possible future refinement, not a current gap in the spec.
- **`list_per_receiver_success_rate` — DEFERRED, blocked on the verdict-log rollup, which is unimplemented (no queryable source today).** Per § Architectural rules the source is the `smtp_verdicts` rollup partitioned by `destination_domain`, **owned by `mail-observability.md`** — *not* the Prometheus counter `mail-deliverability.md` previously named (that conflation is now corrected in § Architectural rules; Prometheus is a separate box-owner-metrics concern). Verified against current code: the `smtp_verdicts` SQLite table + `smtp_verdicts_rollup_hourly`/`_daily` rollups **do not exist** (no `CREATE TABLE`; mail-observability.md is an UNRATIFIED draft and its ingest `fauna.bridges.report_smtp_verdict` + rollup workers + query RPC `list_smtp_verdicts_rollup` are explicitly "a later track, tracked internally"). The `outbound_mail_queue` table is transient (rows deleted on delivery; its only `verdict` columns are *inbound* SPF/DMARC for forwarding), so it is not a substitute source. Per § Don't do these ("Don't store the per-receiver-domain delivery-success rate as a separate table"), a new aggregation table here is **forbidden**. → This RPC stays unbuilt until the `mail-observability.md` verdict-log + rollup track lands a nest-queryable, `destination_domain`-partitioned source; it should then read that rollup (no new table). Decided 2026-06-12 (tracked internally).
- **Known defect (found 2026-10-01):** a list that does not answer, and a force-refresh inside the per-DNSBL rate limit (§ Force-refresh), are each recorded as *not listed, with an error*, and the health read takes only the newest record — so an unanswered check, or pressing Check again twice within a minute, turns a real listing into "did not answer" and clears the blocklisted state, against § Blocklist self-check's *the prior result is preserved*.
- **Not yet implemented:** the Tier-2 **config knobs** (`blocklist_self_check_servers`, `blocklist_self_check_enabled`, `*_retention_days`, `warmup_schedule`, `warmup_enabled`) — the DNSBL set + 90d retention + the default ramp are currently the hard-coded defaults (warm-up and the daily self-check are always-on), and no app offers any of the five; the sender-dashboard note of § Out-of-scope: Gmail Postmaster Tools + Outlook SNDS → *The admin-pane surface* (no element id is approved for it); the mail health readout on the six apps after tui (the paragraph above); and the user-side `mail-message-detail-warmup-deferred-row` — **corrected 2026-07-23: not merely a per-app route-3 follow-on.** The doc previously claimed this row is "gated on the `deferred_warmup` verdict that now lands" — verified against current code, that verdict does **not** land: `outbound_mail_queue.status` (`OutboundStatus`, `bins/fauna-nest/src/db/outbound.rs`) takes `pending`/`sent`/`permfail`/`bounced`/`suppressed_rate`/`suppressed_backscatter` — the latter two are the backscatter/rate-suppression outcomes `smtp-server.md` § Outbound delivery owns, unrelated to warmup — and none of the six is a warmup marker; `enqueue_outbound_split` (`bins/fauna-nest/src/db/outbound.rs:188`) inserts a warm-up-deferred row with `status = 'pending'` — identical to any other not-yet-attempted row. The **only** trace of a warm-up deferral is `next_attempt_at` stamped to the next UTC midnight, which is indistinguishable on the row from an ordinary retry landing on the same boundary; there is no `deferred_warmup` flag/verdict column anywhere (consistent with this doc's own adjacent per-receiver-success-rate bullet, which correctly notes `outbound_mail_queue`'s only `verdict` columns are *inbound* SPF/DMARC). So this row is blocked on a **nest-side** follow-on (a real distinguishing marker on the row or a companion table) before any app can build the affordance — not app UI alone.
- **The `mail_outbound_*_total` counter names this doc uses throughout (§ Fresh-IP warm-up → § State model, § Enforcement at submission time, § Blocklist self-check, § Admin-pane rendering, § Force-refresh) are not backed by any metrics-emission today.** `bins/fauna-nest` has no Prometheus/metrics infrastructure at all (no `prometheus` crate dependency, no metrics module) — Prometheus is scraped only from the Go mail bridge's loopback `/metrics` (`bins/fauna-bridges/internal/metrics/metrics.go`, per `smtp-server.md` § Metrics surface, all SMTP-transaction-level), and none of `mail_outbound_warmup_deferred_total`, `mail_outbound_self_blocklist_total{server}`, `mail_outbound_self_blocklist_error_total{server}`, or `mail_outbound_self_blocklist_force_refresh_total` appear in either the nest or the bridge. Only the warm-up one has any backing state at all — the `mail_outbound_warmup_state.deferred_total` column (bumped in `try_consume_warmup`) — but it is never read back by any RPC, including `outbound_warmup_status` (whose wire shape in § Wire shapes correctly omits it). The blocklist counters have no backing state whatsoever (`mail_outbound_self_blocklist_check` is a row-per-check history table with no per-server cumulative column). Treat "Counter `mail_outbound_*` increments" throughout this doc as target-state naming for a future metrics pass, not current behavior.

## Wire shapes (named, not redefined here)

| RPC | Caller | Purpose | Notes |
|---|---|---|---|
| `fauna.bridges.outbound_warmup_status` | admin client | warmup state read (badge / future `admin-mail` page) | `()` → `{ current_day, today_used, today_max, ramp_end_date, lifetime_total, first_outbound_at, last_reset_at }` |
| `fauna.bridges.outbound_warmup_reset` | admin client | warmup reset action | `()` → updated state |
| `fauna.bridges.blocklist_self_check_run` | admin client (force-refresh) OR nest internal (timer) | re-check a DNSBL on demand | `(server?)` → updated `mail_outbound_self_blocklist_check` row |
| `fauna.bridges.list_blocklist_self_check_history` | admin client | blocklist self-check history read | `(window_days)` → list of `mail_outbound_self_blocklist_check` rows |
| `fauna.bridges.run_deliverability_diagnostics` | admin client | on-demand diagnostic run | `()` → list of check results |
| `fauna.bridges.list_deliverability_diagnostic_runs` | admin client | the diagnostic history | `(limit)` → list of `mail_outbound_diagnostic_runs` rows |
| `fauna.bridges.list_per_receiver_success_rate` | admin client | per-receiver success/failure read | `(window_days, receivers?)` → per-receiver-domain success/failure counts. **DEFERRED — blocked on the `smtp_verdicts` rollup (owned by `mail-observability.md`), which is unimplemented today**; § Don't do these forbids a separate table. See § Implementation status today. |

Wire-level shape lives in the nest implementation track and the bridge-kind catalogue (`docs/goal/architecture/apps/bridges.md` § Bridge-kind catalogue — its home after the 2026-07-08 move out of storage-modes.md); this doc owns which RPCs exist + what each carries.

---

## Architectural rules

- **Warmup is per-deployment, schedule-driven, queue-tomorrow on cap.** The deployment's outbound IP reputation is shared across all users; warmup state is one deployment-wide row; over-cap submission is queued (not bounced); manual reset is admin-only.
- **Blocklist self-check runs on a 24h timer + admin-on-demand.** Per-DNSBL queries; rate-limited on the force-refresh path; results stored 90 d.
- **Postmaster Tools is out of scope by design.** No API; the admin manually opts in at Google's / Microsoft's web consoles. The admin-pane surface is help text + URLs only.
- **Symptom diagnostic is admin-pane-only (no API for users).** Users shouldn't be diagnosing "why didn't my mail to gmail go through" via the deployment's deliverability dashboard.
- **The warmup ramp ships with a sensible default.** Matches the "works out of the box" invariant; admin can tune the schedule but doesn't have to.
- **An established ramp is never restarted by an upgrade.** No upgrade can create the warm-up table under an already-sending box any more — the genesis gives every nest database the table from its first boot (§ Upgrade seeding) — and nothing else resets the ramp but the admin's `warmup_reset`.
- **No automated remediation.** Diagnostic surfaces problems; the admin fixes via existing knobs / VPS-provider settings.
- **Blocklist-listed status is non-blocking.** Being listed on Spamhaus doesn't stop the MTA from trying to deliver — receivers themselves decide whether to honor the DNSBL. The admin sees the listing + decides whether to take action (some listings are false-positives and resolve themselves; some require active de-listing request).
- **Per-receiver-domain success rate is sourced from the verdict-log rollup, not a separate table.** The data is the `smtp_verdicts` rollup partitioned by `destination_domain`, **owned by `mail-observability.md`** (§ Verdict store + § Rollup workers; query RPC `list_smtp_verdicts_rollup`). That SQLite verdict log + hourly/daily rollup is the canonical admin-pane source — Prometheus counters (`smtp_outbound_attempts_total{verdict}`) are a *separate* box-owner-metrics concern per `mail-observability.md`'s Prometheus-vs-admin separation, **not** the source for this read. ⚠ The `smtp_verdicts` table + rollup workers are **unimplemented today** (mail-observability.md is an UNRATIFIED draft; the ingest `report_smtp_verdict` + rollup are a later nest track), so `list_per_receiver_success_rate` is **blocked on that track** (see § Implementation status today).
- **Diagnostic history is admin-only.** Per-actor authorization scopes the read to admin; non-admin users get `403`.

---

## Don't do these

- **Don't tie warmup enforcement to per-user submission quotas.** The warmup is deployment-wide outbound; a single high-volume user could trip it on day 2 (correct behavior — the deployment's reputation is what receivers throttle on). The user-side UX is the friendly tempfail-with-queue-tomorrow message.
- **Don't auto-quarantine the deployment on a blocklist hit.** The admin needs to see + decide; some blocklists return false-positives that resolve themselves. Auto-quarantine would be over-correction.
- **Don't claim to integrate with Postmaster Tools.** We don't. The admin-pane surface is help text + URLs; pretending to integrate would be a lie.
- **Don't surface deliverability symptoms on user-facing UI.** Admin-pane only. Users shouldn't see "your mail to gmail is failing because our IP is on Spamhaus" — that's an admin's job to fix, not a per-user concern.
- **Don't store the per-receiver-domain delivery-success rate as a separate table.** Query over the existing verdict rollup with `destination_domain` partition. New aggregation tables drift from the source-of-truth verdict log.
- **Don't run the warmup-deferred queue at low priority.** Deferred mail is delivered tomorrow at the next day's quota; it's not a "low priority"queue. Receivers don't see a difference; the warmup is a sender-side rate cap, not a receiver-perception adjustment.
- **Don't let the warmup ramp go backwards.** If the admin resets the warmup, `current_day` goes to 1; if the admin un-resets (manually edits `first_outbound_at`), the state is undefined (the table's not directly edittable; only the reset button is supported). Going backwards in `current_day` without a reset would lock the admin out of their actual day-N quota.
- **Don't run a force-refresh blocklist query without rate-limiting.** A button-mashing admin could hammer Spamhaus + get the deployment IP listed for "abusive query patterns." The 1-per-minute rate cap per DNSBL is the floor.
- **Don't run the diagnostic on its own or a more frequent timer.** Beyond the admin's on-demand run, exactly one diagnostics run piggybacks on the existing 24h blocklist self-check tick (so the mail health readout's `records_failing` input stays fresh — § The mail health readout); there is no separate or more frequent timer. Continuous DNS queries against the deployment's own DNS provider + outbound TLS connection-tests to gmail are operationally expensive (and surveillance-y from gmail's perspective). The 24h self-check tick is the only continuous timer in this doc.
- **Don't surface the verdict-log search via the diagnostic.** The diagnostic is a fresh-state check (DNS + TLS); the verdict-log search is the existing `mail-observability.md` surface. Two separate concerns.
- **Don't let users see the warmup-deferred queue's contents in someone else's mailbox.** Per-actor authorization on the deferred-queue read path; the admin surface sees aggregate counts only (not per-user identification).

---

## Reading list

1. `principles.md` — § Product invariants (works out of the box: warmup + blocklist-self-check ship enabled with sensible defaults; user always controls their data: deliverability symptoms are admin-pane only).
2. `docs/goal/behavior/smtp-server.md` § Outbound delivery — the retry machinery this doc builds on; § Retry schedule (the admin-tunable retry cadence); § Log shape (the verdict source for per-receiver-domain success-rate).
3. `docs/goal/behavior/mail-observability.md` — the verdict-log rollup that `list_per_receiver_success_rate` waits on; § Cross-actor isolation (the admin-aggregate-only rule extends to per-receiver-domain rendering). (Deliverability health itself surfaces as a status badge, not an observability card — § Admin-pane Deliverability surface.)
4. `docs/goal/behavior/mail-policy-config.md` § Tier 2 (new warmup + blocklist self-check + per-receiver knobs); § Inbound perimeter (DNSBL servers — same default list reused for outbound self-check).
5. `docs/goal/behavior/dmarc-reporting.md` § Aggregate-report receive — the receiver-side feedback channel that replaces Postmaster-Tools integration; its "Senders claiming our domain" summary surface.
6. `docs/goal/behavior/mail-multidomain.md` § Per-domain DKIM (the diagnostic checks the active selector's pubkey per-domain); § Per-domain DNS records (the diagnostic walks the SPF / DKIM / DMARC / MTA-STS / TLSRPT records per local_domain).
7. `docs/goal/behavior/onboarding.md` § DNS configuration — the initial DNS-record set; the diagnostic verifies these records remain published + valid.
8. `docs/goal/behavior/mail-mass-mailing.md` — the list-send fan-out that also submits through the warm-up chokepoint (§ Fresh-IP warm-up → § Enforcement at submission time); list-mode sends count against the same deployment-wide quota.
9. `docs/goal/architecture/installers/vps.md` — per-VPS-provider reverse-DNS configuration is the admin-side fix path the diagnostic surfaces. (The blocklist self-check is a nest-internal 24h timer, not a separate image daemon.)
10. `docs/goal/architecture/apps/bridges.md` § Bridge-kind catalogue — where the `outbound_warmup_status` / `blocklist_self_check_run` / `run_deliverability_diagnostics` RPC rows live (moved out of storage-modes.md 2026-07-08).
11. Spamhaus de-listing documentation; Barracuda removal request URL; Spamcop removal flow — the compile-time-constant URL map per DNSBL.
12. Gmail Postmaster Tools help (postmaster.google.com); Outlook SNDS (sendersms.outlook.com) — the help URLs we link to in the "Postmaster Tools (manual)" admin-pane row.
13. RFC 7208 (SPF — §4.6.4 DNS-lookup-count limit the diagnostic checks); RFC 6376 (DKIM — pubkey-match check); RFC 7489 (DMARC — `p=` policy mode); RFC 8461 (MTA-STS — policy file format).
