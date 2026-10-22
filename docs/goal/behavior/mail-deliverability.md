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
3. **Out-of-scope-by-design: Postmaster Tools.** Gmail Postmaster Tools and Outlook Smart Network Data Services (SNDS) are receiver-side dashboards that require manual opt-in at Google's / Microsoft's web consoles. Each publishes an API, but one that serves only the registered owner of the domain or IP range, through credentials that owner sets up at the provider; Fauna integrates with neither. This doc names them as out-of-scope-by-design + links to the help URLs.
4. **Symptom diagnostics.** The admin runs diagnostics (the admin debug RPC surface, or the mail health readout's re-check button on `admin-mail`, which runs the diagnostics and the blocklist self-check together — § Admin-pane Deliverability surface) → synchronous check of SPF / DKIM / DMARC / MTA-STS / TLSRPT publication + reverse-DNS for outbound IP + outbound TLS posture check against a known peer (gmail.com). Results render in a check-list. No automated remediation — the diagnostic surfaces problems; the admin fixes them via the existing knobs.

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

**The schedule and the warm-up itself are constants, not choices (ruled 2026-10-09).** The table above is a Rust constant (`fauna_mail::warmup::max_for_day`), and the warm-up is always on: no human tunes or disables it, in a file or in an app. This is the user's 2026-05-24 directive applied as written — deliverability and warm-up are automatic concerns with no manual admin UI (`mail-policy-config.md` § Policy catalog, the UX-surface reconciliation banner) — and [`../principles.md`](../principles.md) § One configuration surface's test gives the same answer: the table encodes what ~2026 receivers accept from an unknown IP, which no admin knows better than the shipped value, and the one warm-up act an admin does own — starting the ramp over after an IP change — is the mail health readout's confirm-gated reset (§ Admin-pane Deliverability surface). The earlier prose promised an admin-tunable `mail.outbound.warmup_schedule` and a `mail.outbound.warmup_enabled = false` for deployments with prior reputation; both are withdrawn. A fresh Fauna deployment's outbound IP has no reputation of its own, and the rare one that does pays only the ramp's first weeks, its over-cap mail queued for the next day, never bounced (§ Why a queue-tomorrow default).

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

The nest, at **every door a user's own act sends mail out through** — an authenticated submission at port 465/587, a send from the Fauna app (`fauna.email.send`), a mailing-list send (`mail-mass-mailing.md`), and a forward leaving for an external address (`mail-forwarding.md`) — checks the deployment-wide warmup state (RULED 2026-10-09: the cap is the IP's, and the IP does not know which door a message came through, so a door the cap skips is a hole in the ramp, not an exemption). **Mail the nest composes on its own — a bounce, an auto-reply, a calendar invitation — is never counted and never held.** The check:

1. Compute `today_used = mails_sent_today + (this message's recipient count)`.
2. Look up `max_for_today` from `warmup_schedule[current_day]`.
3. If `today_used > max_for_today`:
   - The message is **queued for tomorrow** (deferred, not bounced). Verdict `deferred_warmup`. The door still accepts — the MTA returns `250 OK`, `fauna.email.send` returns success with its queued count — (the user-side UX is "your message was accepted; delivery will start tomorrow"). Counter `mail_outbound_warmup_deferred_total` increments.
   - The user's app surfaces the deferred state where the user reads their sent mail — the sent message's own bubble in its conversation, since received and sent mail render in the DM thread and there is no separate mail message-detail page: `dm-message-warmup-deferred` (an optional, indexed leaf of `components.dm-message-bubble`; ID user-approved 2026-10-16) under the body says "Delivery deferred until <date> per IP warm-up schedule." followed by the queue position ("Next in the queue." / "1 message ahead in the queue." / "<count> messages ahead in the queue."). The nest's `Sent` read carries the hold as `InboxMessage.warmup_hold` (`release_at`, `messages_ahead`); the shared conversations ingest keeps the latest read's hold per message and projects it as `MessageSnapshot.warmup_hold`, and the wording — the release time in the viewer's local clock and the count — is the shared `fauna_conversations::message::warmup_deferred_text`, which says nothing once the release time has passed; no app formats the count, and only web formats the date — wasm has no timezone database, so it hands the shared clock-free half `warmup_deferred_text_at` a release time in the same `YYYY-MM-DD HH:MM` shape (`value-formatting.md` § Absolute local timestamp display). The count is the one the `Sent` read reported; it is not re-counted while the thread stays open. Built on tui 2026-10-17 and on web, linux and android 2026-10-18; windows, macOS and iOS follow.
4. Otherwise the submission proceeds normally.

### Manual reset

Warmup reset is an admin action via the debug RPC surface (`fauna.bridges.outbound_warmup_status` + a reset call), used only when the deployment's outbound IP has changed (e.g., after a VPS migration) → on confirm: `mail_outbound_warmup_state.first_outbound_at = NOW()`; `current_day = 1`; `mails_sent_today = 0`; `last_reset_at = NOW()`. The lifetime counter (`mails_sent_total`) is preserved. (The reset affordance is the mail health readout's `admin-mail-health-warmup-reset-button` on `admin-mail` — § Admin-pane Deliverability surface → *The mail health readout*; it is not a `bridges-detail` card button.)

### Upgrade seeding

**Retired with the genesis (2026-09-24).** A one-time seed once kept a deployment that *adopted* the warm-up feature with mail already enabled — the `mail_outbound_warmup_state` table created for the first time on an existing `/data` — from being treated as a fresh IP: it stamped `first_outbound_at` from `mail_enabled.set_at`. It served only databases older than the feature. Since the nest schema's genesis ([`../architecture/nest/common.md`](../architecture/nest/common.md) § Database) every nest database carries the table from its first boot, before mail can be enabled, so no such adoption can happen and the seed went with the history (`../architecture/version-compatibility.md` § Dimension 2, the fourth ratified exception). The rule it protected still binds: an established ramp never moves backwards (§ Don't do these).

### Why per-deployment, not per-actor

The IP reputation is the **deployment's IP** — every user shares it. A single high-volume user could trip the warmup on day 2 (which is correct behavior: the deployment's reputation is what receivers throttle on). The per-user UX for hitting the warmup cap is the friendly "your messages will start delivering tomorrow" tempfail, not a per-user error.

### Why a queue-tomorrow default

Bouncing at warmup would surface a confusing error to the user ("your mail was rejected because IP warmup"). Queuing-tomorrow is the long-term-defensible choice — the user's message lands within 24 hours; no error message; counter visible in admin pane.

---

## Blocklist self-check

### The check

Every 24 hours (timer-triggered, at 03:00 UTC plus a per-deployment offset inside the following hour — a fixed rule over the deployment's own key, so every boot of one deployment keeps the same moment while the fleet's deployments do not query the DNSBLs in one synchronized storm), **nest** queries our own outbound IP against the DNSBLs in `mail.outbound.blocklist_self_check_servers` (Tier 2; default `["zen.spamhaus.org", "b.barracudacentral.org", "bl.spamcop.net"]` — this doc's own default, deliberately broader than the inbound `mail.inbound.dnsbl_servers` default of `["zen.spamhaus.org"]` alone: a self-check should sweep the lists major receivers consult even if we don't gate inbound on them. SORBS was dropped from the default 2026-07-08 — the service was decommissioned in 2024, so querying it yields a permanently-misleading NXDOMAIN "not listed" ✓). The check is a nest-internal timer (`run_scheduled_blocklist_self_check`), not an MTA-side or separate-daemon concern.

The timer reads the mail clock and is restart-safe: the check is due once a daily moment has passed that no pass of the nest's own has covered, and the cover survives a restart because it is the newest diagnostics run the nest made itself (§ The mail health readout). A restart after the day's pass does not run it again, a nest that was down across its moment catches up when it comes back, and a fresh nest makes its first pass at the next moment, not at boot. A cover dated in the future (a clock that ran ahead and came back, or a store carried over from a test run with the mail clock moved) counts as now, so it never holds the pass, or the retention prunes riding it, past the next moment.

The query is a standard DNSBL lookup: `<reversed-outbound-IP>.<dnsbl-suffix>` resolves to an A record (typically `127.0.0.2` for "listed") if the IP is listed, or NXDOMAIN if not listed. Counter `mail_outbound_self_blocklist_total{server="..."}` increments on each check.

### Storage

`mail_outbound_self_blocklist_check(checked_at TIMESTAMP, results_json JSONB)`:

- One row per check (so the admin can see history).
- `results_json` carries per-server outcome: `{"zen.spamhaus.org": {"listed": false, "txt_record": null}, "b.barracudacentral.org": {"listed": true, "txt_record": "Listed for spam pattern X"}, ...}`.
- Retention 90 days, a Rust constant held by the daily tick's prune, which holds the diagnostics history (§ Admin-visible audit) to the same window. The once-promised admin-tunable `mail.outbound.self_blocklist_retention_days` is withdrawn (ruled 2026-10-09 with § The ramp's constants): how many days of a few-hundred-byte history to keep is no admin's choice.

### Admin-pane rendering

The blocklist self-check surfaces per-DNSBL status (rolled into the mail health readout at the top of `admin-mail` and the admin debug surface — see § Admin-pane Deliverability surface; the badge's former `admin-services` home was dropped with that page in the 2026-06-04 redesign — admin.md § Admin IA redesign — and the readout was re-homed to `admin-mail` 2026-09-25) as a colored status:

- Green ✓ — not listed.
- Red ✗ — listed (with the TXT-record reason inlined if the DNSBL returned one).
- Yellow ? — query returned a resolver error (timeout, SERVFAIL) — counter `mail_outbound_self_blocklist_error_total{server="..."}` increments; the prior result is preserved until the next successful query.

On a listed result, the mail health readout (§ Admin-pane Deliverability surface) offers a one-click "Open de-listing URL". The URL is a compile-time constant per known DNSBL — Spamhaus de-listing, Barracuda removal request, Spamcop removal — and **the readout carries one link, not one per list** (the approved id set has a single `admin-mail-health-delist-link`): it opens the removal page of the first list the address is on that has one, while the self-check row's detail names every list it is on, so an admin listed on several works through them — clear one, check again, and the link moves to the next. (Corrected 2026-10-01: this paragraph had read as one link per list.) The actual de-listing is the admin's responsibility (most DNSBLs require human review).

### Force-refresh

An admin-initiated re-check (via the debug RPC `fauna.bridges.blocklist_self_check_run`, or the mail health readout's `admin-mail-health-recheck-button`) fires the query immediately (bypassing the 24h timer for that one DNSBL); counter `mail_outbound_self_blocklist_force_refresh_total`. Rate-limited to once per minute per DNSBL (defends against an admin-loop hammering the DNSBL). A re-check inside the limit asks that list nothing and records nothing for it — the reply reports it rate-limited — so the standing result stays what the last real query found, a listing included.

### No off switch

**The daily self-check always runs (ruled 2026-10-09 with § The ramp's constants).** The once-promised `mail.outbound.blocklist_self_check_enabled = false` is withdrawn under the same directive and the same test. The deployment it was for — outbound behind a corporate proxy, its IP never reaching the public internet — loses nothing by the check running: its queries fail and the readout shows the resolver-error state of § Admin-pane rendering, which is the true answer, where a switched-off check would read as silence. And the same tick carries the daily diagnostics run that the mail health readout's `records_failing` row reads (§ The mail health readout) and the history prune (§ Storage), neither of which may stop.

---

## Out-of-scope: Gmail Postmaster Tools + Outlook SNDS

Gmail Postmaster Tools (postmaster.google.com) and Outlook Smart Network Data Services (SNDS — sendersupport.olc.protection.outlook.com/snds, the target `SENDER_DASHBOARD_LINKS` hard-codes) are **receiver-side dashboards** that admins of mail-sending domains can opt into to see per-receiver-side metrics (Google's view of our spam rate; Microsoft's view). **Each has a programmatic API — Google's Postmaster Tools API and SNDS's OAuth data API (corrected 2026-10-20; this doc said "neither has a programmatic API" until then) — but neither is something a deployment can use without its owner's own enrolment:** both require:

1. Manual admin login at the receiver's web console.
2. Manual domain verification (DNS TXT record or upload a file at a well-known URL).
3. Manual review of dashboards on the receiver's website.

Fauna **does not automate** the setup or read the postmaster-tools data back — not through either provider's API either, since each API serves only the enrolled owner's credentials (an OAuth client the admin would register at Google or Microsoft), and the DMARC aggregate reports below already carry most of the same signal.

### The defensible alternative

The receiver-side feedback we **do** see comes from:

- **Peer-published DMARC reports** (per `dmarc-reporting.md` § Aggregate-report receive) — receivers that publish `rua=` in their DMARC policy send us aggregate reports about how their MTAs handled mail from our domains. Gmail does this; we get daily reports. The reports include source IP, message count, disposition (none/quarantine/reject), DKIM/SPF alignment. This gives us most of the operational signal Postmaster Tools renders.
- **TLSRPT** (per `smtp-server.md` § TLSRPT inbound report listener) — receivers report TLS failures they observed delivering to us.
- **Bounce verdicts** (per `smtp-server.md` § Outbound delivery → bounce classification) — when our outbound mail tempfails or permfails, we see the receiver's SMTP response text directly.

These three give the admin enough signal to diagnose deliverability without setting up Postmaster Tools / SNDS manually.

### The admin-pane surface

Postmaster Tools is documented as a manual, out-of-scope concern (static help text + URLs, no integration), surfaced wherever the deliverability health detail lands — which since 2026-09-25 is the mail health readout. **Its ids are user-approved (2026-10-16) and in `ui.yaml` on page `admin-mail`; it is built on tui (2026-10-17), the other six apps following in their trickle-down:** `admin-mail-health-sender-dashboards-note` (text — the help text below) and `admin-mail-health-sender-dashboard-link`, indexed `[gmail]` / `[outlook]` (one link per receiver, opened — or, on a terminal, copied — like the readout's delist link). They sit beside the readout's nine ids, not among them (§ Admin-pane Deliverability surface → *The mail health readout*, the *ui.yaml scope* paragraph). When it lands it:

- Renders a help text: "Gmail Postmaster Tools and Outlook SNDS are externally managed dashboards on the receiver's website. Sign up at postmaster.google.com and sendersms.outlook.com if you want their per-receiver-side view; Fauna doesn't integrate with these APIs (none exist)."
- Provides links to the help URLs (target=_blank, hard-coded).
- Doesn't query them, doesn't display their data, doesn't track whether the admin has signed up.

This is intentionally a passive surface — naming the option so the admin knows it exists, without pretending to integrate.

---

## Symptom diagnostics

An admin runs diagnostics (via the debug surface, or the mail health readout's re-check button on `admin-mail`) → triggers `fauna.bridges.run_deliverability_diagnostics()` synchronously → returns a check-list. The diagnostic runs each check sequentially (some have network calls; the whole run takes 5–30 seconds depending on DNS responsiveness).

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

Each check produces one result row (check name, pass/fail/warn, plain-language description of failure). Diagnostic results are returned by `fauna.bridges.run_deliverability_diagnostics` and surfaced through the admin debug surface / logs; the mail health readout on `admin-mail` renders the latest run's outcome as its check rows (§ Admin-pane Deliverability surface → *The mail health readout*). Because DKIM/TLS/MTA-STS are auto-provisioned, most "Fix" actions are automatic; the few admin-actionable ones point at their flat home (a failing SPF/DMARC/MX record shows red on `admin-dns`; reverse-DNS mismatch is VPS-provider-side — the description explains, no fix affordance).

### No automated remediation

The diagnostic **surfaces problems**, the admin fixes them via the existing knobs / VPS-provider settings. Auto-remediation (e.g., "I'll fix the SPF for you") would create silent reconfiguration; the admin needs to see + approve each change.

### Admin-visible audit

Diagnostic runs are logged in `mail_outbound_diagnostic_runs(ran_at, results_json, ran_by_actor_id)` — admin-only access. Retention 90 days, the same constant and the same prune as § Storage (the once-promised `mail.outbound.diagnostic_retention_days` is withdrawn with it, ruled 2026-10-09). The audit lets the admin see "I ran diagnostics on 2026-05-01, DKIM was failing, I fixed it on 2026-05-02, diagnostics passed."

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

**ui.yaml scope (ids user-approved 2026-09-25; page `admin-mail`, landed the same day):** elements `admin-mail-health-section` (view), `admin-mail-health-status` (text — the categorical line plus the two heartbeat facts); component `admin-mail-health-check` (indexed rows — `admin-mail-health-check`, `-label`, `-state`, `-detail`: bridge connection · blocklist self-check · outbound queue · DNS/auth records · warm-up ramp · last delivered · last received, in that order); `admin-mail-health-delist-link` (present only while a self-check row lists the outbound IP — the per-DNSBL de-listing URL of § Blocklist self-check), `admin-mail-health-recheck-button` (runs `blocklist_self_check_run` + `run_deliverability_diagnostics`, then re-reads) and `admin-mail-health-warmup-reset-button` (confirm-gated `outbound_warmup_reset`, the after-an-IP-change act). **The confirm is the same-button two-click on every app** — ui.yaml scopes no confirm id to it (nine ids in all: the section, the status line, the check row and its three leaves, the delist link, the two buttons), so the first press only relabels the button with the confirm sentence (`admin.mail_page.health_warmup_reset_confirm`) and the second dispatches `ResetWarmup` and disarms: the shape `mail-spam-reset-model-button` already uses, which the cross-app driver drives as two clicks. **The arm is held by the shared `MailPolicyMachine`, not by each app (2026-10-17):** every press of an `admin-mail` confirm button — this one and the two secret-rotation buttons (`mail-mass-mailing.md` § Secret rotation) — is `MailPolicyAction::Press { button }`, which arms `MailPolicySnapshot::armed` on a first press, disarms and acts on a second press of the same button, moves the arm on a press of another, and is cleared by every other action and re-read; the renderer only relabels the armed button. tui renders all three that way, and macOS and iOS (one FaunaKit view) render the warm-up reset that way; web, linux, android and windows still arm the warm-up reset in their own renderer and dispatch the direct `ResetWarmup`, which goes once their trickle-down moves them to `Press`. The two buttons exist because both acts were raw-RPC-only, which the one-configuration-surface invariant (`principles.md` § One configuration surface) makes a gap rather than a posture; neither edits policy, so the viewer-not-editor rule of `mail-observability.md` § Architectural rules is untouched. This supersedes the "detail lives in logs, not a UI card" half of the 2026-06-01 framing for these seven checks only — the reason being that the mail bridge's per-transaction verdict lines never reach `admin-logs`: `fauna.admin.logs` merges the nest's own ring with the sidecar log plane's remote ring (`architecture/apps/observability.md` § The sidecar log plane), and the Go mail bridge reports into that plane only its fixed lifecycle catalogue (ready, TLS-cert fetch failed, listener bind failed, nest reconnected, forced shutdown, confinement degraded — `bins/fauna-bridges/internal/logplane/catalogue.go`), deliberately never a tee of its SMTP log stream (verified 2026-09-27 by the nest build). **Not in this set, by the same ruling:** the sender-dashboard note — its own two ids, `admin-mail-health-sender-dashboards-note` and the indexed `admin-mail-health-sender-dashboard-link[gmail|outlook]`, were approved separately on 2026-10-16 (§ Out-of-scope: Gmail Postmaster Tools + Outlook SNDS → *The admin-pane surface*) — any tier-2 telemetry id, the DMARC report summary (`dmarc-reporting.md` § ui.yaml surface stays proposed), the scanner health signal and aggregate scan view (`mail-content-scanning.md`), and inbound TLS-report inspection (`smtp-server.md` § TLSRPT inbound report listener) — each stays where its owner doc leaves it and needs its own ask.

**Build order:** shared fold + wire types (`fauna_mail::health`, `fauna-protocol`) → nest (`mail_health` handler, the two stamps, the bridge-connection projection, the timer-driven diagnostics run) → tui (lead app) → the other six in their batched trickle-down. The first implementation mints `docs/features/<slug>.md` and updates `docs/guides/`.

---

## Implementation status today

**The mail health readout (§ Admin-pane Deliverability surface → *The mail health readout*) is built through shared Rust + nest + the client seam, and renders on tui (the lead app, 2026-09-28), web, linux and android (2026-10-11, the section only) windows (2026-10-13, the section only), and macOS and iOS (2026-10-21, the section only, one FaunaKit view).** Landed: the pure fold `fauna_mail::health` (`evaluate` — the worst-wins state, the seven check rows, the `queue_row_stalled` predicate, the compile-time per-DNSBL de-listing URLs), the open-enum labels `fauna_core::format::mail_health_state_label` / `mail_health_check_state_label` (+ their UniFFI and wasm twins), the Admin-class `fauna.bridges.mail_health` → `MailHealthReply` (`fauna-protocol`), the two heartbeat stamps on the one-row `mail_heartbeat_state` table (stamped in `mark_outbound_delivered` / `ingest_inbound_mail`), the bridge-connection projection (`WsState::has_connections` over the approved out-of-process MTA/MDA `bridge_service_users`), the diagnostics run on the 24 h self-check tick (recorded with an all-zero `ran_by_actor_id` — the nest itself), and the client seam on `MailPolicyMachine` (`MailPolicySnapshot::health`, `MailPolicyAction::{RecheckHealth, ResetWarmup, Press}`, a refusal degrading to `None`). The blocklist row folds each list over the retained self-check history (`fauna_mail::health::standing_blocklist`): a list's newest answered query decides listed and its reason, so an unanswered query or a rate-limited re-check never clears a listing (§ Blocklist self-check → *Admin-pane rendering*), and the row's detail inlines each listing's reason. **tui renders it** (`apps/fauna-tui/src/admin/mail.rs` `health_elements`, the dashboard's Mail card in `admin/dashboard.rs`): all nine `admin-mail-health-*` ids under the enable toggle, the status line and heartbeat rows through the shared `mail_health_status_text` / `mail_health_heartbeat_text` (their pure half, `mail_health_heartbeat_label`, is the FFI/wasm-shaped door), the warm-up reset as the same-button two-click, and the sixth `admin-stat-card` (Mail); proven by `tests/e2e-unified/tests/test_admin_mail_health.py`. The UniFFI and wasm twins of `mail_health_heartbeat_label` landed with the web/linux/android legs. The `admin-mail` section is built on web, linux, android, windows, macOS and iOS (macOS and iOS paint it from `AdminMailView`'s `MailHealthSection`, with the warm-up reset on `MailPolicyAction.press` and the label read off `snapshot.armed`; android compile-verified, its emulator e2e not yet run; windows paints it from `AdminMailViewModel.Health`, which composes the status line and the heartbeat rows over the same UniFFI twins). The dashboard's Mail card is not this trickle-down's — it rides the shared `dashboard_snapshot` fold (`admin.md` § 1 Dashboard), which all 7 apps paint.

Landed (tracked internally, nest-side):

- **`fauna.bridges.run_deliverability_diagnostics`** (Admin) — the full § Symptom diagnostics check set: SPF present/valid, DKIM present + pubkey-match per active selector, DMARC present + enforcing, MTA-STS record present + policy-file fetchable, TLSRPT present, reverse-DNS present + matches HELO, outbound STARTTLS posture to gmail. Persists a `mail_outbound_diagnostic_runs` audit row.
- **`fauna.bridges.blocklist_self_check_run`** (Admin force-refresh + nest-internal 24h timer, default-on, ~03:00 UTC ± offset) — the DNSBL self-check; persists `mail_outbound_self_blocklist_check`; per-DNSBL force-refresh rate-limit (1/min); 90-day retention prune.
- The reusable pass/warn/fail verdicts live in `fauna_mail::deliverability` (SPF lint, DMARC mode, MTA-STS/TLSRPT/DKIM presence + DKIM pubkey-match, DNSBL query/interpret); the async orchestration + the `StarttlsProber` seam are nest-side (`bins/fauna-nest/src/mail_deliverability.rs`).

Landed (tracked internally, nest-side — the fresh-IP warm-up half):

- **Ramp curve** — `fauna_mail::warmup::max_for_day(day) -> Option<u64>` (shared, pure, WASM-safe; `None` = day-30+ unlimited), piecewise-linear between the § The ramp anchors. The day-counter `mail_outbound_warmup_state` (one deployment-wide row) + the atomic check-and-consume (`db::mail_warmup::try_consume_warmup`) are nest-side.
- **Submission-time enforcement** — one gate, `enqueue_outbound_warmup_capped` (`bridge_routing_handlers.rs`), at every user door: the authenticated 465/587 path (`class == BridgeMta`) and the mailing-list send fan-out (`fauna.bridges.send_list_message`, unconditionally — per `mail-mass-mailing.md` § Reading list item 7) reach it through `submit_outbound`; the app send (`fauna.email.send`) hands it the external recipients its own exact-match in-domain partition leaves; both forward dispatches (`promote_due_forwards` and the forward handler's under-cap direct dispatch) hand it each forward's single destination — an over-cap forward is held on the queue, never parked, evicted or dropped, and the forward queue's own per-actor cap and ceiling are untouched. The MDA calendar-auto-schedule gateway, auto-reply, and NDR/bounce paths route through `submit_outbound` with warm-up enforcement **off** and are exempt — never counted, never deferred. The cap is over the EXTERNAL recipients that actually leave via the outbound MX (post in-domain partition). Over today's cap → the whole message is queued for tomorrow (`next_attempt_at = next 00:00 UTC` via `enqueue_outbound_warmup_held`, which also stamps the hold — the bullet on the Sent read below; `created_at` kept honest), the MTA still returns `250 OK`, never bounced; `deferred_total` increments.
- **`fauna.bridges.outbound_warmup_status` / `outbound_warmup_reset`** (Admin) — read the deployment-wide state / reset the ramp to day 1 after an outbound-IP change (preserving `mails_sent_total`). On the same `outbound_now()` clock seam as the enforcement.
- **Upgrade seeding (§ Upgrade seeding)** — retired with the genesis (2026-09-24): every nest database has the warm-up table from its first boot, so the one-time seed for a box adopting the feature has no input.

Landed (tracked internally, nest-side — the history/observability **read** half):

- **`fauna.bridges.list_blocklist_self_check_history`** `(window_days)` (Admin) — newest-first blocklist-self-check history within the window (`≤ 0` ⇒ the full 90-day retention), reading the persisted `mail_outbound_self_blocklist_check` rows. Each row's stored JSON is parsed back into the same per-DNSBL `results` shape the live run returns (apps render typed rows; no per-app JSON parse).
- **`fauna.bridges.list_deliverability_diagnostic_runs`** `(limit)` (Admin) — newest-first diagnostic-run audit (`≤ 0` ⇒ 100, max 1000), reading `mail_outbound_diagnostic_runs`; `checks` is the same checklist the live run returns, plus the `ran_by_actor_id`. Admin-only (§ Admin-visible audit); a malformed/legacy stored row degrades to an empty verdict list rather than failing the read (the audit timestamp is the load-bearing part).

Caller-class resolution (was an internal contradiction in this doc): the old § Status labeled these "MTA-class", but § Wire shapes names the *admin-client* caller and § Architectural rules says "Symptom diagnostic is admin-pane-only". **Resolved toward Admin-class** (the implemented state); the gate is in `bins/fauna-nest/src/bridge_method_allowlist.rs`.

- **The daily pass's schedule (§ The check) is built (2026-10-13).** The pure `DailyPassSchedule` in `bins/fauna-nest/src/mail_deliverability.rs` (with `daily_pass_offset_secs`, the offset taken from the first eight bytes of the nest's public key modulo an hour, and `latest_daily_pass_slot`) decides due-ness, tier_1-tested under a plain-seconds clock; the loop in `bins/fauna-nest/src/main.rs` asks it every five minutes against `AppState::outbound_now`, seeds it at boot from the newest all-zero-actor `mail_outbound_diagnostic_runs` row, and runs `run_scheduled_blocklist_self_check` plus the retention prunes on the same pass. Both the self-check row and the diagnostics row are stamped on the mail clock. The e2e witness of the unasked pass is the `test-hooks` poke `POST /api/v1/test/mail/blocklist-self-check/run`, which calls the same function; the cadence itself is proven only by the tier_1 tests.

Documented assumptions / deferred gaps (target-state prose above that code does **not** yet implement):

- **Outbound IP** is derived as the A/AAAA of `mail.<primary-domain>` (the advertised mail host / HELO). Correct for the single-homed VPS deployment; a split outbound IP (multi-homed host) is a follow-on (no STUN/echo discovery today).
- The **gmail STARTTLS probe** is real (tokio + tokio-rustls + webpki roots) but its network path is only exercisable on a deployment with outbound `:25` reach; `for_test` installs a `NullStarttlsProber` (a Warn row).
- **Deferred-mail counting is submission-time, not delivery-time.** Per § Enforcement at submission time, the counter is consumed at submission and an over-cap message is stamped `next_attempt_at = tomorrow 00:00 UTC` — so it "lands within 24 hours" exactly as § Why a queue-tomorrow default promises, but a single very large burst on day N can exceed day N+1's intended cap on the wire (the deferred batch is not re-checked against the next day's cap at delivery). This matches the doc's literal submission-time algorithm; a queue-runner-side re-check (delivery-time counting) is a possible future refinement, not a current gap in the spec.
- **`list_per_receiver_success_rate` — DEFERRED, blocked on the verdict-log rollup, which is unimplemented (no queryable source today).** Per § Architectural rules the source is the `smtp_verdicts` rollup partitioned by `destination_domain`, **owned by `mail-observability.md`** — *not* the Prometheus counter `mail-deliverability.md` previously named (that conflation is now corrected in § Architectural rules; Prometheus is a separate box-owner-metrics concern). Verified against current code: the `smtp_verdicts` SQLite table + `smtp_verdicts_rollup_hourly`/`_daily` rollups **do not exist** (no `CREATE TABLE`; mail-observability.md is an UNRATIFIED draft and its ingest `fauna.bridges.report_smtp_verdict` + rollup workers + query RPC `list_smtp_verdicts_rollup` are explicitly "a later track, tracked internally"). The `outbound_mail_queue` table is transient (rows deleted on delivery; its only `verdict` columns are *inbound* SPF/DMARC for forwarding), so it is not a substitute source. Per § Don't do these ("Don't store the per-receiver-domain delivery-success rate as a separate table"), a new aggregation table here is **forbidden**. → This RPC stays unbuilt until the `mail-observability.md` verdict-log + rollup track lands a nest-queryable, `destination_domain`-partitioned source; it should then read that rollup (no new table). Decided 2026-06-12 (tracked internally).
- **Not yet implemented:** the Tier-2 **config knob** `blocklist_self_check_servers` — the DNSBL set is the hard-coded default and no app offers it (the four other knobs this bullet once listed — `warmup_enabled`, `warmup_schedule`, `blocklist_self_check_enabled` and the two `*_retention_days` — are constants since 2026-10-09: § The ramp, § Storage, § No off switch; the code already behaves so, and nothing is owed for them); the sender-dashboard note of § Out-of-scope: Gmail Postmaster Tools + Outlook SNDS → *The admin-pane surface* on every app but tui (its ids are in `ui.yaml` since 2026-10-16; tui paints it since 2026-10-17); the mail health readout on macOS and iOS (the paragraph above); and the user-side `dm-message-warmup-deferred` on windows, macOS and iOS — built on tui 2026-10-17 and on web, linux and android 2026-10-18 (the shared ingest, `MessageSnapshot.warmup_hold` and `fauna_conversations::message::warmup_deferred_text`, UniFFI-exported under `fauna-ffi`'s `conversations-session`; web calls its clock-free half `warmup_deferred_text_at` over wasm with the release time `localTimestamp` formatted, its Sent drain handing the read's hold to `ingestSealedInbound`), so each of those three apps owes only its paint over the existing UniFFI door; witness `tests/e2e-unified/tests/test_mail_warmup_deferred.py`.
- **The warm-up's hold is a typed fact on the sender's Sent read (built 2026-10-09).** A message `submit_outbound` defers past the day's ramp is queued through `enqueue_outbound_warmup_held` (`bins/fauna-nest/src/db/outbound.rs`): each of its `outbound_mail_queue` rows carries `warmup_held_until` (the next 00:00 UTC) and the message's dedup pair over its raw bytes (`warmup_dedup_key` + `warmup_envelope_key`, `fauna_mail::mail_dedup_keys`). The status stays `pending` (the hold is a column, not a seventh `OutboundStatus`) and the row is still released by `next_attempt_at`. `fauna.email.sent.fetch` returns it as `InboxMessage.warmup_hold` (`{release_at, messages_ahead}`, `libs/fauna-protocol/src/email.rs`; absent when nothing holds the message): `CacheDb::warmup_holds_for_actor` reads the still-pending, not-yet-released rows, folds them per message with the shared `fauna_mail::warmup::held_positions` (the count ahead is over the deployment-wide held queue, in submission order), and ties each held message to the caller's Sent copy through `actor_message_dedup` — both halves of the pair must agree, so a Message-ID another sender reuses never marks the caller's mail as held. Two limits follow from that tie: a Sent copy that did not record the pair first (another copy carrying the same Message-ID landed in the caller's mailbox before it) reads as not held; and the hold is recorded wherever the warm-up is counted, since every counted door ends in the one `enqueue_outbound_warmup_capped` (§ the submission-time bullet above) — the app send (`fauna.email.send`) included, which files its Sent copy over the same raw bytes after the enqueue (witnessed end to end by `tests/e2e-unified/tests/test_mail_warmup_deferred.py`). Witnesses: `email_handlers.rs` tests `a_message_the_warm_up_holds_reads_back_held_on_the_senders_sent_read` and `a_borrowed_message_id_does_not_mark_another_senders_mail_held`; the api-tier wire test `tests/e2e-unified/tests/api/test_mail_deliverability.py::test_warmup_held_list_send_reads_back_held_on_sent`; and, at the 465/587 door, where the Sent copy (`fauna.bridges.submit_inbound_mail`) and the queued message (`fauna.bridges.enqueue_outbound_mail`) reach the nest in two separate bridge calls whose pairs must agree, the tier_3 test `tests/e2e-unified/tests/test_mail_bridge_mua_submission.py::test_mail_app_send_wider_than_the_warmup_allowance_reads_back_held_on_sent` over the real mail bridge (green as built: the bridge ships the same bytes, its own headers already stripped and Message-ID stamped, to both calls).
- **The `mail_outbound_*_total` counter names this doc uses throughout (§ Fresh-IP warm-up → § State model, § Enforcement at submission time, § Blocklist self-check, § Admin-pane rendering, § Force-refresh) are not backed by any metrics-emission today.** `bins/fauna-nest` has no Prometheus/metrics infrastructure at all (no `prometheus` crate dependency, no metrics module) — Prometheus is scraped only from the Go mail bridge's loopback `/metrics` (`bins/fauna-bridges/internal/metrics/metrics.go`, per `smtp-server.md` § Metrics surface, all SMTP-transaction-level), and none of `mail_outbound_warmup_deferred_total`, `mail_outbound_self_blocklist_total{server}`, `mail_outbound_self_blocklist_error_total{server}`, or `mail_outbound_self_blocklist_force_refresh_total` appear in either the nest or the bridge. Only the warm-up one has any backing state at all — the `mail_outbound_warmup_state.deferred_total` column (bumped in `try_consume_warmup`) — but it is never read back by any RPC, including `outbound_warmup_status` (whose wire shape in § Wire shapes correctly omits it). The blocklist counters have no backing state whatsoever (`mail_outbound_self_blocklist_check` is a row-per-check history table with no per-server cumulative column). Treat "Counter `mail_outbound_*` increments" throughout this doc as target-state naming for a future metrics pass, not current behavior.

## Wire shapes (named, not redefined here)

| RPC | Caller | Purpose | Notes |
|---|---|---|---|
| `fauna.bridges.outbound_warmup_status` | admin client | warmup state read (the `admin-mail` health readout) | `()` → `{ current_day, today_used, today_max, ramp_end_date, lifetime_total, first_outbound_at, last_reset_at }` |
| `fauna.bridges.outbound_warmup_reset` | admin client | warmup reset action | `()` → updated state |
| `fauna.bridges.blocklist_self_check_run` | admin client (force-refresh) OR nest internal (timer) | re-check a DNSBL on demand | `(server?)` → updated `mail_outbound_self_blocklist_check` row |
| `fauna.bridges.list_blocklist_self_check_history` | admin client | blocklist self-check history read | `(window_days)` → list of `mail_outbound_self_blocklist_check` rows |
| `fauna.bridges.run_deliverability_diagnostics` | admin client | on-demand diagnostic run | `()` → list of check results |
| `fauna.bridges.list_deliverability_diagnostic_runs` | admin client | the diagnostic history | `(limit)` → list of `mail_outbound_diagnostic_runs` rows |
| `fauna.bridges.list_per_receiver_success_rate` | admin client | per-receiver success/failure read | `(window_days, receivers?)` → per-receiver-domain success/failure counts. **DEFERRED — blocked on the `smtp_verdicts` rollup (owned by `mail-observability.md`), which is unimplemented today**; § Don't do these forbids a separate table. See § Implementation status today. |

Wire-level shape lives in the nest implementation track and the bridge-kind catalogue (`docs/goal/architecture/apps/bridges.md` § Bridge-kind catalogue — its home after the 2026-07-08 move out of storage-modes.md); this doc owns which RPCs exist + what each carries.

---

## Architectural rules

- **Warmup is per-deployment, schedule-driven, queue-tomorrow on cap.** The deployment's outbound IP reputation is shared across all users; warmup state is one deployment-wide row; over-cap mail from any user door — a submission, an app send, a list send, a forward — is queued (not bounced), while the nest's own bounces, auto-replies and invitations are never counted (§ Enforcement at submission time); manual reset is admin-only.
- **Blocklist self-check runs on a 24h timer + admin-on-demand.** Per-DNSBL queries; rate-limited on the force-refresh path; results stored 90 d.
- **Postmaster Tools is out of scope by design.** No integration, though each provider publishes an owner-credentialed API; the admin manually opts in at Google's / Microsoft's web consoles. The admin-pane surface is help text + URLs only.
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
12. Gmail Postmaster Tools help (postmaster.google.com); Outlook SNDS (sendersupport.olc.protection.outlook.com/snds) — the help URLs we link to in the "Postmaster Tools (manual)" admin-pane row.
13. RFC 7208 (SPF — §4.6.4 DNS-lookup-count limit the diagnostic checks); RFC 6376 (DKIM — pubkey-match check); RFC 7489 (DMARC — `p=` policy mode); RFC 8461 (MTA-STS — policy file format).
