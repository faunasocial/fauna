# Mail observability — target state

Owns: mail-observability
Status: draft — deliberately NOT ratified. **User ruling 2026-09-25 (Scope A of the admin-telemetry design pass, § Design pass 2026-09-19):** the cheap half of the admin's four questions — the **mail health readout**, tier 1 — is RATIFIED and owned by `mail-deliverability.md` § Admin-pane Deliverability surface → *The mail health readout*, not by this doc; the verdict-telemetry pipeline this doc owns — tier 2, § Data source onward — is **demand-driven**: unbuilt, not owed for alpha, re-opened only by the named triggers in § Design pass. So the data-model / retention / isolation prose below stays design input, not ratified target state, and consumers still hard-stop here (the earlier 2026-07-08 ruling's reasons stand: the card-catalog UI premise was superseded 2026-06-01 — tombstone below — and the pipeline is unbuilt). This doc is promoted to ratified — resolved by the nest track that builds the verdict pipeline once a § Design pass trigger fires, together with a rule-A pass on whatever richer admin surface it then needs.
Authority: the admin-facing mail-telemetry surface — the verdict-store + rollup + retention data model (target-state), the aggregate-only admin/user split, and the Prometheus-vs-admin-pane separation; defers the SmtpVerdict record schema to behavior/smtp-server.md § Log shape, per-user spam-model isolation to behavior/mail-spam.md, knob tiers to behavior/mail-policy-config.md, and any future ratified telemetry UI to its own ui.yaml ratification pass.

> **Audience:** the future nest track implementing the `smtp_verdicts` table + rollup workers + query RPCs, and the design pass that ratifies a real admin-telemetry surface.
> **Purpose:** admin-facing mail traffic telemetry, sourced from the `SmtpVerdict` records `smtp-server.md` § Log shape specifies. This doc owns the target data model (verdict store, rollups, retention), the cross-actor isolation rules between admin (system-wide aggregate) and user (per-account own data only), and the Prometheus-vs-admin-pane separation.

## ⚠ Superseded UI premise — tombstone (the former card catalog)

An earlier draft of this doc specified a **`Bridges-detail` admin dashboard** of `<concern>-summary-card` components — six cards (inbound / outbound / auth / TLS / spam / queue) plus DMARC + Deliverability cards, drill-down detail pages, and a 10-second `BridgeDashboardTick` live ticker. **That UI premise was ratified OUT on 2026-06-01** (mail-UX design pass, tracked internally) and the catalog prose was removed from this doc on 2026-07-08 (cluster-#3 wave-2 fix pass) because sibling docs kept mis-citing it as an existing ratified shape:

- No `bridges-detail` mail page and no `*-summary-card` mail id exists in ui.yaml; the mail bridge is deliberately off the `bridges` page (`bridges.md` § Scope).
- Per the design decision, **admin mail telemetry is out of scope for the flat mail-UX**: deployment health is subsumed by **health badges** (e.g. "Mail: delivering / warming up / blocklisted", sourced from the warmup + blocklist-self-check state — `mail-deliverability.md` § Admin-pane Deliverability surface records the card-drop + badge framing) plus logs. The badges' former `admin-services` home was removed 2026-06-04 (admin.md § Admin IA redesign); the readout was homeless until 2026-09-25, when the user ratified its home as the mail health readout at the top of `admin-mail` (`mail-deliverability.md` § Admin-pane Deliverability surface → *The mail health readout* — unbuilt at ratification; until it lands, logs remain the live surface).
- The DMARC summary surface is likewise **proposed, pending ui.yaml ratification** (`dmarc-reporting.md` § ui.yaml surface).
- A future telemetry pass that wants a richer admin view than badges must ratify its **own** ui.yaml ids (likely under the flat `admin-mail` / `admin-nest` / `admin-dns` family), not resurrect `bridges-detail` or the card catalog. The data-model sections below are its design input.

## Goal (target-state)

Give the admin a way to answer, from their Fauna app and without `ssh`: (1) is mail being delivered? (2) is mail being received? (3) is our auth posture working? (4) is anything obviously broken? — sourced from structured `SmtpVerdict` records, aggregated server-side, never per-transaction streaming to the client.

This is **not** a Prometheus / Grafana replacement — a box owner with their own observability stack scrapes the bridge's loopback `/metrics` endpoint (`smtp-server.md` § Metrics surface); the Fauna-app surface is the default story for admins who won't deploy one. The two surfaces are independent (§ Prometheus stays loopback-only).

## Design pass 2026-09-19 — the two-tier split (ruled 2026-09-25: tier 1 ratified, tier 2 demand-driven)

> **User ruling 2026-09-25 — Scope A.** **Tier 1 is owed now and ratified**, with rule-A sign-off on its ids and home — owned by `mail-deliverability.md`, built rust → nest → tui → the other six. **Tier 2 is demand-driven**: not owed for alpha, no build row exists, and this doc stays `draft` with the consumer hard-stop in the header until one of the named triggers below fires. Nothing under § Data source onward is ratified target state.

The four questions above split by cost, and the halves are wildly unequal:

- **Tier 1 — the mail health readout (RATIFIED 2026-09-25 — owed now).** Owned by `mail-deliverability.md` § Admin-pane Deliverability surface → *The mail health readout*: one read-only section on `admin-mail`, one server-side fold over state the nest already holds plus three small additions (a mail-bridge connection projection, two deployment-wide heartbeat stamps, a timer-driven diagnostics run). It answers question (4) in full, (3) at the published-records level, and (1)/(2) at heartbeat granularity ("last delivered / last received"). For the common deployment — one box, one admin, low volume — that is what works-out-of-the-box owes: the admin learns *that* something is wrong and *which* subsystem, from the app, without a shell.
- **Tier 2 — the verdict pipeline (RULED 2026-09-25: demand-driven, not owed for alpha; no build row until a trigger fires).** Everything under § Data source onward: the bridge→nest verdict stream, `smtp_verdicts`, the rollups and their workers, the query kinds, per-receiver success rate (`mail-deliverability.md`'s deferred `list_per_receiver_success_rate`), and the DMARC report summary — which belongs here and **not** in tier 1, because the nest holds only the record-*publication* check while the report-ingest pipeline behind the summary is unbuilt (`dmarc-reporting.md` § Implementation status today). It answers "how much, to whom, rejected why" — questions a low-volume box rarely has and a high-volume box answers with the loopback Prometheus endpoint today. Named triggers that make it owed: an alpha admin asks a volume or rejection-reason question tier 1 cannot answer; a deliverability incident is diagnosed from a shell because the app could not show it; or the per-receiver success rate becomes a build dependency.

**Open items this pass resolved (ruled with the split, 2026-09-25):**

- **The outbound verdict record — resolved toward the nest, no `smtp-server.md` extension.** Every outbound outcome already arrives at the nest through `mark_outbound_{delivered,failed,bounced}`, so when tier 2 builds, the outbound half is counted nest-side at those three handlers (the second alternative the `verdict` row of § Data source names) and `report_smtp_verdict` carries **inbound** records only. `smtp-server.md` § Log shape stays inbound-only and needs no outbound record.
- **The inert `mail.observability.*` retention rows stay inert**, bound to tier 2; tier 1 retains nothing (its facts are latest-row reads and two stamps), so it needs no retention knob.
- **Live refresh:** tier 1 re-reads on page open and after an action, like every other admin page; `BridgeDashboardTick` and any topic-subscription mechanism stay tier-2-only proposals.

## Implementation status today

**The entire telemetry pipeline is unbuilt** (verified 2026-07-08 — zero code hits for every named surface):

- `fauna.bridges.report_smtp_verdict` (the bridge→nest verdict stream) — no allowlist entry, no bridge caller; the bridge emits verdicts to stderr only (`smtp-server.md` § Log shape records the same gap from the owner side).
- `fauna.bridges.list_smtp_verdicts_rollup`, `get_smtp_verdict`, `get_queue_depth`, `list_account_mail_stats` — none exist.
- `BridgeDashboardTick`, `BridgeVerdictRecorded` push events, and any `subscribe_topic` mechanism — none exist (the realized bridge push machinery is **topicless**: `config_changed`-style whole-refresh nudges).
- The `smtp_verdicts` table + hourly/daily rollup tables + rollup workers — none exist.
- The retention knobs landed in the mail-policy-config catalog as **inert Bucket-C rows** (`mail.observability.*` — projected nowhere until this pipeline builds).
- The outbound half needs **no** new verdict record (ruled 2026-09-25, § Design pass → *Open items*): when tier 2 builds, outbound is counted nest-side at `mark_outbound_{delivered,failed,bounced}` and `report_smtp_verdict` carries inbound records only — the `verdict` row of § Data source records the same.
- **What IS built of the admin's four questions is tier 1 — the mail health readout — and it lives in `mail-deliverability.md` § Implementation status today** (ratified 2026-09-25; its status and remaining build rows are recorded there). Nothing in this doc gains a build row until a § Design pass trigger fires.

## Data source (target-state, unbuilt)

`SmtpVerdict` records are the raw source-of-truth. Target: each verdict ships from the bridge to nest over WS-RPC via `fauna.bridges.report_smtp_verdict(verdict)` — one per accepted-or-rejected SMTP transaction, low-rate — and nest persists it into an `smtp_verdicts` table.

**Schema below is conceptual** — the shipped shape follows the codebase's SQLite + integer conventions (BLOB ids, INTEGER unix-seconds timestamps, milli-int scores — the dag-cbor float ban; no UUID/TIMESTAMP/NUMERIC/TEXT[] literals), fixed at build time:

| Column (conceptual) | Notes |
|---|---|
| `verdict_id` | primary key; minted nest-side on insert |
| `received_at` | wall-clock at insert |
| `direction` | `inbound` / `outbound` — **proposed extension**, see below |
| `verdict` | **inbound**: the enum `smtp-server.md` § Log shape owns (`accepted`, `rejected_*`, `tempfail_*`). **Outbound: no verdict record exists in the owner schema, and none will** (ruled 2026-09-25, § Design pass → *Open items*) — the outbound values an earlier draft listed (`delivered`, `tempfail_connect`, `permfail_*`) are Prometheus *label* values from § Outbound metrics, not verdict records; outbound outcomes are counted nest-side at `mark_outbound_{delivered,failed,bounced}` when tier 2 builds, so `direction = outbound` rows are minted by the nest from those three handlers, never shipped by the bridge. |
| `reason` | free-text reason |
| `source_ip` | client IP (inbound) / remote MX IP (outbound) |
| `source_domain`, `helo_domain`, `destination_domain`, `dnsbl_hits`-with-codes, `spam_score` | **proposed extensions** — not in the owner's `SmtpVerdict` table (which logs `rcpt_to_hash` and a `dnsbl` tri-state); adopting them extends the owner schema and needs `smtp-server.md` § Log shape to ratify the additions |
| `spf` / `dkim` / `dmarc` / `arc` | the owner's auth-verdict strings |
| `message_id` | message-id header (cross-referencing a forward's inbound/outbound legs) |
| `full_verdict_dag_cbor` | the complete record as DAG-CBOR, for forensic replay |

Indexes: `(direction, received_at)`, `(source_domain, received_at)`, `(destination_domain, received_at)`.

### Rollup workers (target-state, unbuilt)

A nest-internal worker on a 1-minute timer: **hourly rollup** (previous complete hour, grouped by `(direction, verdict, source_domain_bucket, destination_domain_bucket)` into `smtp_verdicts_rollup_hourly`) and **daily rollup** (previous day's hourly buckets into `smtp_verdicts_rollup_daily`). Pre-computed dimensions: per-hour timeseries, top-N source/destination domain, verdict breakdown, auth-verdict breakdown, TLS-posture summary, spam-handling summary. Queries read rollups; raw verdict rows are the forensic archive.

### Retention tiers (target-state; knobs landed as inert Bucket-C catalog rows)

| Tier | Default | Knob (Tier 2, inert until this pipeline builds) |
|---|---|---|
| Per-message verdict rows | 7 days | `mail.observability.verdict_retention_days` |
| Hourly rollup rows | 30 days | `mail.observability.rollup_retention_hourly_days` |
| Daily rollup rows | 90 days | `mail.observability.rollup_retention_daily_days` |

Windows map to tiers (1/7 d → per-message; 8–30 d → hourly; 31–90 d → daily). Per-message retention is intentionally short — verdicts are forensic; the rollup serves the surface. Long-tail forensic queries are the box owner's export problem; 7 days covers "what happened yesterday?".

### Prometheus stays loopback-only, unchanged

The bridge's `/metrics` endpoint on `127.0.0.1:9090` (`smtp-server.md` § Metrics surface, the owner) is unchanged by this doc. A box owner with their own stack scrapes it; the Fauna admin surface uses the WS-RPC rollup query path and never scrapes Prometheus. The bridge increments Prometheus counters **synchronously** per transaction and (target) ships `SmtpVerdict` records **asynchronously** — the two are independent, may disagree at the margins across disconnects, and no cross-agreement is attempted.

## Cross-actor isolation

The admin sees **system-wide aggregate**; a user sees **per-account own data** (their delivery success rate, their spam-marked rate for messages they sent or received, their alias hit rate per `mail-aliases.md`). **No surface on either pane identifies another user.**

- Spam telemetry on the admin pane is aggregate counts only ("12 messages marked as spam by users this week") — never per-user attribution. Per-user training data is the user's, not the admin's (`mail-spam.md` § Cross-actor isolation owns the per-user model isolation; this doc owns the admin aggregate-only posture — the split is declared on both sides).
- Auth rates aggregate across all recipients; never broken down by recipient (that would leak who-receives-mail-from-whom). Outbound aggregates are per-destination, never per sending actor.
- The admin **cannot drill** into "what did user X send this week" — no RPC has that shape; building one would be a discrete design conversation, not a UI control.
- Target RPC scoping: admin queries (`list_smtp_verdicts_rollup`) return system-wide aggregates with no actor-breakdown columns; user queries (`list_account_mail_stats`) scope to the implicit caller identity and never accept a `target_actor` argument.

## Storage-mode interaction

Verdict rollup data is **operational telemetry**, not user content — plaintext on nest disk regardless of storage mode. Verdict rows contain envelope-level metadata only (source IP, sender domain, recipient domain, message-id header, verdict, auth results, dnsbl hits) — never message bodies, attachments, or per-user training data. The `message_id` column is peer-MTA-derived and already visible to every hop in transit; storing it is not an additional disclosure. `full_verdict_dag_cbor` is likewise envelope metadata only — a `SmtpVerdict` describing accepted body bytes does not contain the bytes.

## Wire shapes (target-state — ALL unbuilt; named, not redefined here)

| RPC | Caller | Purpose | Status |
|---|---|---|---|
| `fauna.bridges.report_smtp_verdict` | bridge → nest | one per SMTP transaction; nest persists into `smtp_verdicts` | **unbuilt** (no allowlist entry, no caller) |
| `fauna.bridges.list_smtp_verdicts_rollup` | admin client | aggregate query (top-N, breakdown, timeseries) | **unbuilt** |
| `fauna.bridges.get_smtp_verdict` | admin client | raw-verdict forensic read | **unbuilt** |
| `fauna.bridges.get_queue_depth` | admin client | live outbound-queue read | **unbuilt** |
| `fauna.bridges.list_account_mail_stats` | user client | caller's own per-account stats | **unbuilt** |
| `BridgeDashboardTick` / `BridgeVerdictRecorded` push events | nest → admin client | live refresh for a future ratified UI | **proposed with that UI** — no topic-subscription mechanism exists today (the realized bridge push machinery is topicless whole-refresh nudges) |

Wire-level shape lives in the nest implementation track (`docs/goal/architecture/apps/bridges.md` § Bridge-kind catalogue (caller classes) gains the rows when built); this doc owns which RPCs exist and their isolation scoping.

## Architectural rules (target-state)

- **Prometheus separate from the admin surface.** `/metrics` stays loopback-only, owned by `smtp-server.md` § Metrics surface; the admin surface uses the WS-RPC rollup path, never Prometheus, and no Prometheus chart embeds in a Fauna app.
- **Verdict logs are append-only.** Rows never update; rollups are computed from inserted rows.
- **Three retention tiers** — per-message, hourly, daily — with monotonically increasing windows.
- **Aggregate-only on the admin pane; own-data-only on the user pane.**
- **The surface is passive** — a viewer, never an editor. No "block this IP" buttons; policy changes go through the mail-policy-config catalog pages.
- **Rollup computation is idempotent, single-writer** (per-rollup-type lease; restart catches up the previous complete hour).
- **Verdict rollup is plaintext-at-rest operational telemetry** — envelope metadata only.

## Don't do these

- Don't store message bodies in `smtp_verdicts.full_verdict_dag_cbor` — envelope metadata only.
- Don't surface per-user identifiable metrics on the admin pane; don't let a user see another user's traffic.
- Don't let the rollup miss verdicts across a nest restart (append-only log + previous-complete-hour catch-up).
- Don't run two rollup workers concurrently (lease; concurrent writers double-count).
- Don't read raw `smtp_verdicts` rows for windows the rollup tiers cover.
- Don't auto-act on rollup data (auto-rate-limit "malicious" peers) — the surface is a viewer; rate-limit changes are the admin's deliberate policy-catalog action.
- Don't ship a "delete this verdict" button — retention is the knob; per-row deletion is the wrong shape.
- Don't resurrect the `bridges-detail` card catalog or its `<concern>-summary-card` ids — a future telemetry UI ratifies its own ui.yaml ids (§ tombstone above).

## Reading list

1. `principles.md` § Works out-of-the-box (the default observability story) + § The user always controls their data (cross-actor isolation).
2. `docs/goal/behavior/smtp-server.md` § Log shape — the inbound `SmtpVerdict` schema this surface consumes (outbound is counted nest-side, no record extension); § Metrics surface — the Prometheus endpoint that stays loopback-only.
3. `docs/goal/behavior/mail-policy-config.md` — the `mail.observability.*` retention knobs, landed as inert Bucket-C rows until this pipeline builds.
4. `docs/goal/behavior/mail-spam.md` — the per-user isolation rule the aggregate posture honors.
5. `docs/goal/behavior/mail-deliverability.md` § Admin-pane Deliverability surface — the card-drop framing this doc's tombstone matches, and the owner of the ratified tier-1 mail health readout (its § *The mail health readout*).
6. `docs/goal/architecture/apps/bridges.md` § Bridge-kind catalogue (caller classes) — where the RPC rows land when built.
