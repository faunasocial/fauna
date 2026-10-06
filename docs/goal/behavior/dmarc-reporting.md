# DMARC reporting — target state

Owns: dmarc, dmarc-reporting
Status: ratified
Authority: DMARC policy publication (record shape, per-domain overrides, and the default tag values — the values ruling of the 2026-07-08 mail-cluster review) + the aggregate/forensic report pipelines, both directions; defers inbound DMARC *enforcement* to behavior/smtp-server.md § Inbound policy stack, the knob tier/binding/surface rows to behavior/mail-policy-config.md, DNS publish/reconcile mechanics to behavior/dns-management.md, and TLSRPT (both directions) to behavior/smtp-server.md.

> **Purpose:** we publish a DMARC policy and ingest the aggregate + forensic reports peer MTAs send back about mail claiming to be from our domain — and we generate the symmetric reports for peer domains whose mail we receive. Implements `smtp-server.md` § abuse@ / postmaster@ role-address routing's `dmarc-report@` row.

---

## Goal

A DMARC-conformant deployment, end-to-end:

1. We **publish** a `_dmarc.<our-domain>` TXT record per RFC 7489 §6.3 directing peer MTAs to (a) reject mail claiming to be from our domain that fails DMARC alignment and (b) send daily aggregate reports to a Fauna-owned address so we can see who's trying to forge us.
2. We **receive** those aggregate reports at `dmarc-report@<our-domain>` (envelope-routed at SMTP perimeter to a nest processor, **not** the admin mailbox), validate the gzipped XML against the RFC 7489 §A.1 schema, store, and surface a summary to the admin in their Fauna app. We optionally receive forensic reports — off by default, because forensic reports carry third-party recipient PII and we don't want them as bycatch.
3. We **generate** symmetric aggregate reports for peer domains whose published DMARC policy includes a `rua=` URI — the cooperative-behavior counterpart of our publish step. We optionally generate forensic reports — off by default, because we don't want to amplify third-party-recipient PII outbound either.
4. The admin's Fauna app renders a daily summary card with pass/fail counts, top reporting peers, and a "show me the X senders who claimed our domain yesterday" pivot (proposed — no ratified page home yet, § Admin pane summary surface).

**Bar: full RFC 7489 conformance on receive + publish, plus the cooperative-MTA outbound reporter that responsible mail servers run.** A DMARC-conformant peer reading our TXT record sees a coherent policy; a DMARC-conformant peer that publishes `rua=` receives correctly-shaped reports from us; and the admin running this deployment can read the inbound reports without `xq` on raw XML.

**Default-strict for our own published policy.** Fresh deployments ship `p=reject; sp=reject; pct=100` — strict beats permissive (per `smtp-server.md` § Goal). The "monitor first, then quarantine, then reject" incremental-rollout model is for legacy infrastructure migrating to DMARC; a fresh nest has no legacy infrastructure. Admin can soften it (knob — see `mail-policy-config.md`); we don't.

---

## Implementation status today

- **Record-body assembly — DONE.** The published `_dmarc.<domain>` body is assembled by the shared `fauna_mail::dmarc_publish` builder (`dmarc-policy` feature) from a `DmarcPublishPolicy` whose `Default` matches § Record shape, with the per-domain `mail_domains.dmarc_overrides` partial overlaid. Both producers consume it: the nest DNS surface (`fauna.dns.list_records` / `verify_records`) and the onboarding provisioner (`libs/fauna-provisioning`), so the published record matches what verify expects.
- **Deployment-wide `mail.dmarc.*` catalog — NOT built.** There is no stored catalog for the deployment-wide DMARC knob values (`mail.dmarc.policy_mode`, `adkim_mode`, `rua_destination`, …); the assembler's base is `DmarcPublishPolicy::default()`. An admin editing a deployment-wide DMARC knob is not yet reflected. A per-domain override *is* read from the stored column when the record is assembled, and **its policy keys are written** (built 2026-10-04): `fauna.bridges.update_local_domain_config` carries the optional `dmarc_policy_mode`, which the nest merges into the stored partial by the shared `fauna_mail::dmarc_publish::set_policy_mode_json` (both policy keys set together, the default `reject` clearing both, every other key kept), and the shared local-domains machine exposes each domain's policy (`LocalDomainView::dmarc_policy`) and sends it (`LocalDomainAction::UpdateConfig`). **The control is unbuilt:** no app renders `admin-dns-domain-dmarc-policy-select` yet, so softening one domain's policy, which § Multi-domain deployments allows, is reachable over the wire but from no app today. The other override keys have no writer, by design (§ Multi-domain deployments). When the catalog write-path lands (`DmarcPolicyOverrides` in `mail_policy.rs` + the admin write RPC), it feeds the catalog-derived base into the same assembler — no record-format rework.
- **Publication lifecycle — built, one piece deferred.** The managed-mode auto-publish (a *client-side* create-only reconcile — the nest never writes DNS) **is built**: `_dmarc.<domain>` TXT is one of the records `DnsManagementMachine::publish` zone-publishes via the held provider credential, live on all 7 apps (`dns-management.md` § Fauna-managed → Publish, § Implementation status today). A manual-mode domain (no covering credential — every domain today, including example.com) still gets the onboarding provisioner's initial publish plus the unified DNS surface's expected-vs-live verify for any later admin re-paste. **Deferred:** the 1-rewrite-per-minute rate limit on the managed reconcile is not yet built (§ Architectural rules, below).
- **The entire report receive + outbound pipeline — NOT built.** None of the § Wire shapes RPCs exist yet (`ingest_dmarc_{aggregate,forensic}_report`, `list_dmarc_aggregate_reports`, `get_dmarc_aggregate_report`, `fetch_outbound_dmarc_due`, `dmarc_outbound_completed` — zero code hits, no allowlist entries), and none of the storage tables. Everything from § Aggregate-report receive down is target-state.
- **Inbound `dmarc-report@` falls back to the admin mailbox today.** The MTA/nest resolver classifies the role local-part but the processor dispatch is unwired (`bridge_routing_handlers.rs` role-address fallback; `smtp-server.md` § Implementation status declares the same gap for `tlsrpt@`/`dmarc-report@`) — the never-reject invariant holds, reports just land in the admin's INBOX until the ingest RPC lands.

## DMARC policy publication

The deployment's primary domain (and every additional `local_domains` entry — see `mail-policy-config.md` § Tier 2 — Whether mail runs at all) gets a `_dmarc.<domain>` TXT record published. The record is **assembled and served as expected state by nest** from the policy values (per the product invariant — DMARC policy is set from a Fauna app, not from a hand-edited zonefile): nest exposes it via `fauna.dns.list_records` / `verify_records` on the unified DNS surface. **Publication is never nest's** — the nest holds no DNS-provider credential (`dns-management.md` § The two modes): the record reaches the provider via the onboarding provisioner's initial client-side publish, the client's managed-mode reconcile, or the admin's manual copy-paste from the `admin-dns` page.

### Record shape

**This table owns the default values** (ownership ruling, 2026-07-08 mail-cluster review — the code's `DmarcPublishPolicy::default()` anchors here): `mail-policy-config.md`'s `mail.dmarc.*` catalog rows carry tier/binding/surface only and point here for the values, so there is exactly one value copy per knob.

Per RFC 7489 §6.3, our published record carries the tags:

| Tag | Default | Notes |
|---|---|---|
| `v` | `DMARC1` | mandatory; always first |
| `p` | `reject` | policy mode for the registered domain. `mail.dmarc.policy_mode` (`none`/`quarantine`/`reject`). Default `reject` per § Goal. |
| `sp` | `reject` | policy mode for subdomains. `mail.dmarc.subdomain_policy_mode`. Default `reject`. |
| `pct` | `100` | percentage of mail this policy applies to. `mail.dmarc.pct`. Default `100` — fresh deployments don't need partial rollout. |
| `adkim` | `s` (strict) | DKIM identifier alignment. `mail.dmarc.adkim_mode`. Default `s` — we sign with our own key, so strict alignment is correct. |
| `aspf` | `s` (strict) | SPF identifier alignment. `mail.dmarc.aspf_mode`. Default `s` — same reasoning. |
| `rua` | `mailto:dmarc-report@<primary-domain>` | aggregate-report destination. `mail.dmarc.rua_destination`. The `dmarc-report@` recipient is reserved by `smtp-server.md` § abuse@ / postmaster@ role-address routing and routes envelope-time to the nest processor (NOT the admin mailbox). |
| `ruf` | (unpublished) | forensic-report destination. `mail.dmarc.ruf_destination` is unset by default → no `ruf=` tag in the published TXT → peer MTAs don't generate forensic reports about our domain at all. Admin can opt in; PII implications surface in the UI when they do. |
| `fo` | `1` (any failure) | forensic-report options. `mail.dmarc.fo_mode`. Has no effect when `ruf=` is unpublished — included only when the admin opted into forensic. |
| `ri` | `86400` (24 h) | aggregate-report interval seconds. `mail.dmarc.ri_seconds`. RFC 7489 §6.3 says senders MAY honor shorter intervals; we publish the default. |

The assembled record for a default deployment with primary domain `example.org`:

```
_dmarc.example.org. 3600 IN TXT "v=DMARC1; p=reject; sp=reject; pct=100; adkim=s; aspf=s; rua=mailto:dmarc-report@example.org; ri=86400"
```

### Multi-domain deployments

A nest serving multiple `local_domains` publishes one `_dmarc.<domain>` per domain. Per-domain policy customization is allowed (the admin can run `p=none` on one rolling-out domain and `p=reject` on the others); the catalog binding is `mail.dmarc.per_domain_overrides` (a map of `<domain>` → partial-record). The default for each entry is "inherit from the deployment-wide defaults above." **App surface (designed 2026-10-01; the id approved by the user 2026-10-03, as the one stated exception to the 2026-05-24 directive that DMARC-record publishing has no manual admin UI — an admin whose domain also sends through another service must be able to soften that one domain, and no automatic rule can know it; the write is built, the control is not — § Implementation status today):** one indexed select on each `admin-dns-domain` row, `admin-dns-domain-dmarc-policy-select`, offering `reject` (the default), `quarantine` and `none`. It sets the domain's `policy_mode` and `subdomain_policy_mode` together — an admin softening a domain means the whole domain — and choosing the default clears both keys rather than storing them. The other override keys below get no control: nobody has a reason to choose them per domain, and until someone does they stay at the deployment default (`../principles.md` § One configuration surface — a value no human chooses is a constant). The write is an additive, typed, optional field on `fauna.bridges.update_local_domain_config`, absent meaning unchanged.

The per-domain DMARC pattern this section describes is the canonical "deployment-wide default + per-domain override map" shape generalized across DKIM, MTA-STS, catch-all, and role-address routing in `docs/goal/behavior/mail-multidomain.md`. The `mail_domains.dmarc_overrides` JSONB column there is the storage of `mail.dmarc.per_domain_overrides` (the per-domain row carries this domain's entry); the inbound and outbound aggregate-report processor inboxes are deployment-wide (`dmarc-report@<primary-domain>`) and partition on the report payload's `policy_published.domain` field, not on a per-domain inbox.

The `mail_domains.dmarc_overrides` partial-record is a JSON object whose keys are the catalog-binding names of the per-tag knobs above; every key is optional and an absent key inherits the deployment-wide default. Its typed shape is `fauna_protocol::bridge_routing::DmarcOverrides` (re-exported by `fauna_mail::dmarc_publish`), which is also what the admin-mail wire carries (`MailDomainRow.dmarc_overrides`, [`mail-multidomain.md`](mail-multidomain.md) § Wire shape + storage); a stored value that does not decode reads as no override, both on the wire and when the record is assembled:

| Key | Type | Overrides tag |
|---|---|---|
| `policy_mode` | `"none"`/`"quarantine"`/`"reject"` | `p` |
| `subdomain_policy_mode` | `"none"`/`"quarantine"`/`"reject"` | `sp` |
| `pct` | `0`–`100` | `pct` |
| `adkim_mode` | `"s"`/`"r"` | `adkim` |
| `aspf_mode` | `"s"`/`"r"` | `aspf` |
| `rua_destination` | string (full `mailto:` URI) | `rua` |
| `ruf_publish` | bool | whether `ruf`/`fo` are emitted |
| `ruf_destination` | string (full `mailto:` URI) | `ruf` |
| `fo_mode` | `"0"`/`"1"`/`"d"`/`"s"` | `fo` |
| `ri_seconds` | integer | `ri` |

So `{"policy_mode":"none","pct":50}` on a rolling-out domain renders `v=DMARC1; p=none; sp=reject; pct=50; adkim=s; aspf=s; rua=mailto:dmarc-report@<primary>; ri=86400` — the `rua` still points at the deployment-wide processor. Malformed override JSON falls back to the deployment-wide default body (the write path, once built, is typed, so the read surface stays robust). The body assembler is shared Rust (`fauna_mail::dmarc_publish`, `dmarc-policy` feature) so the nest DNS surface (`fauna.dns.list_records` / `verify_records`) and the onboarding provisioner (`fauna-provisioning`) emit the identical record.

### Publication lifecycle

- **Initial publication** — published at admin-claim time, immediately after `mail.enabled` flips on (per `mail-bridge-lifecycle.md` § Default-off on first claim): the **client-side** onboarding provisioner writes `_dmarc.` in the same DNS step that publishes MX, SPF, and MTA-STS records (manual-mode deployments copy the record from `admin-dns` instead).
- **Policy change** — admin edits the policy in their Fauna app → nest re-assembles the expected record → the **client's** managed-mode reconcile (when a covering DNS credential exists — `dns-management.md` § Reconcile) writes the update, else the `admin-dns` page shows expected-vs-live drift for a manual re-publish. Lifetime in caches is bounded by the published TTL (3600 s default).
- **Disabling DMARC** — admin sets `mail.dmarc.policy_mode = none` (the RFC-7489 "monitor only" mode), which leaves the record published but tells peer MTAs not to act on alignment failures. Removing the record entirely (`mail.dmarc.publish_enabled = false`) is allowed but strongly discouraged — the surface is one toggle in the UI, with a warning that absent-DMARC makes the domain easier to spoof.

### Compile-time decisions (NOT configurable)

- The DMARC version tag (`v=DMARC1`) — there is no DMARC2.
- The record assembly logic itself (which tags appear, the syntactic format) — this is RFC 7489 §6.3 conformance, not policy.
- The rewrite rate limit on managed-mode reconciles (see § Architectural rules — a client-side constraint, one statement there).

---

## Aggregate-report receive (RUA)

The receive pipeline mirrors `smtp-server.md` § TLSRPT inbound report listener — DMARC aggregate reports arrive as `application/gzip` attachments on inbound mail to `dmarc-report@<our-domain>`.

### Envelope-time routing

Per `smtp-server.md` § abuse@ / postmaster@ role-address routing, `dmarc-report@<our-domain>` is a reserved role address. At RCPT TO time, the MTA recognizes the local-part and dispatches the message to the nest processor instead of the admin's mailbox:

1. **RCPT TO** = `dmarc-report@<local-domain>` → flagged as DMARC-report-bound.
2. **DATA** body is received intact (the MTA does not validate XML; that's the processor's job).
3. **After DATA accept**, the MTA calls `fauna.bridges.ingest_dmarc_aggregate_report(payload_bytes, source_envelope)` instead of the usual `VerifyAndDeliver` path.
4. The MTA's normal verdict / metric / log shape still fires — the dispatch is a routing decision, not a bypass of the policy stack. SPF / DKIM / DMARC verification on the **report-carrying** message still runs (a forged DMARC report from an attacker is treated like any other suspicious mail), and the verdict ride to the processor as `source_envelope.auth_results`.

The MTA does **not** require the report-carrying mail to itself align under DMARC — a peer's MTA legitimately sends DMARC reports from a domain that publishes no DMARC policy of its own, or that fails alignment because of a quirk in their own outbound path. Failing to ingest those reports would silence the network's voice about us. The verdict labels carry through; the processor decides whether to surface "this report's wrapper failed alignment" in the admin pane.

### XML validation

The payload (gzip-decompressed) is a UTF-8 XML document conforming to the schema in RFC 7489 §A.1. Validation pipeline:

1. **Decompress** — `application/gzip` (sometimes `application/zip` from older senders — we accept both). Reject payloads > 50 MiB pre-decompression / 500 MiB post-decompression as oversize (counter `dmarc_inbound_aggregate_oversize_total`); store the rejected envelope minus the body for ops triage.
2. **Parse** — strict XML parse against the RFC 7489 §A.1 schema. The schema covers `<feedback>` root, `<report_metadata>`, `<policy_published>`, `<record>` (1..N — each record describes one (source-IP, count, disposition) bucket).
3. **Required fields** — `report_metadata.report_id`, `report_metadata.date_range.begin`, `report_metadata.date_range.end`, `policy_published.domain`, every `<record>` element having `row.source_ip`, `row.count`, `row.policy_evaluated.disposition`. Missing any → reject as malformed, counter `dmarc_inbound_aggregate_malformed_total`, **accept the message on the wire (SMTP 250) + log + count** rather than 4xx/5xx-reject (peer MTAs have no retry obligation for malformed reports; mirroring TLSRPT inbound's policy from `smtp-server.md` § TLSRPT inbound report listener).
4. **Sanity** — `policy_published.domain` must be one of our `local_domains`. A report about a domain we don't serve is dropped with `dmarc_inbound_aggregate_wrong_domain_total`.

### Storage

`dmarc_inbound_aggregate_reports(submitter, report_id, begin_at, end_at, received_at, payload_xml_gz, source_envelope_blob)`:

| Column | Type | Notes |
|---|---|---|
| `submitter` | TEXT | `report_metadata.org_name` + `report_metadata.email` joined |
| `report_id` | TEXT | unique within (submitter, our-domain); per RFC 7489 §A.1 |
| `our_domain` | TEXT | `policy_published.domain` — which of our domains this is about |
| `begin_at` | INTEGER | unix seconds |
| `end_at` | INTEGER | unix seconds |
| `received_at` | INTEGER | when we accepted the report |
| `payload_xml_gz` | BLOB | the gzipped XML as received, for re-parse if our parser improves |
| `parsed_summary_cbor` | BLOB | DAG-CBOR-encoded summary (counts, top source-IPs, top header.from values, disposition breakdown) computed once at ingest time and read by the admin pane query path; this is the index, the gz blob is the archive |
| `source_envelope_blob` | BLOB | DAG-CBOR envelope context (client_ip, helo, mail_from, auth_results) — for forensic correlation if a report looks suspicious |

### Retention

**Default 90 days**, admin-tunable via `mail.dmarc.aggregate_retention_days`. Reports older than retention are pruned by the same nest GC pass that handles TLSRPT inbound reports (per `smtp-server.md` § TLSRPT inbound report listener — 90 d default).

### Push to admin pane

Per-report inserts emit a low-rate `BridgeDmarcReportIngested { our_domain, submitter, report_id }` push to the admin's WS connection if they have the (proposed, § Admin pane summary surface) admin mail-telemetry surface open; the admin pane re-queries the summary on each push (debounced to once per 30 s).

---

## Forensic-report receive (RUF)

DMARC forensic reports are per-message failure reports — each one carries the headers (and optionally the body — RFC 7489 §7.3 leaves this implementation-defined) of a specific inbound mail that failed DMARC alignment against our domain. They're PII-heavy: the reported message is a third party's outbound, and the original recipient's address is exposed in the report.

### Default off

We do **not** publish `ruf=` by default. A deployment that has not opted in receives zero forensic reports, ever — peer MTAs don't generate them without a `ruf=` line in our TXT record. This is the conservative default that respects third-party PII.

The admin opts in via the Tier-2 knob `mail.dmarc.ruf_publish = true`, which causes nest to add `ruf=mailto:dmarc-report@<primary-domain>; fo=<mode>` to the published TXT record. The UI surfaces the PII tradeoff at the toggle point: "Forensic reports carry recipient email addresses from third-party senders. Enable only if your security team needs forensic-level signal."

### Envelope-time routing (when enabled)

Same as RUA: `dmarc-report@<our-domain>` is the role-address destination; the MTA dispatches to `fauna.bridges.ingest_dmarc_forensic_report(payload_bytes, source_envelope)` instead of `VerifyAndDeliver`.

### Format leniency

RFC 7489 §7.3 is loose about format — the wrapper is `message/feedback-report` per RFC 5965, the embedded report is `message/rfc822` (full message) or `message/rfc822-headers` (headers only). Real-world DMARC forensic-report senders are vendor-divergent (Gmail's shape differs from Microsoft's differs from Yahoo's). Our parser is **lenient on the inner shape** (best-effort header extraction, no schema-strict reject) and **strict on the wrapper** (must be valid RFC 5322 + RFC 5965 multipart). Failures of the lenient parse increment `dmarc_inbound_forensic_parse_partial_total` and store the raw payload anyway for admin triage.

### Storage

`dmarc_inbound_forensic_reports(submitter, our_domain, received_at, reported_message_id, reported_arrival_date, payload_raw_gz, source_envelope_blob, parsed_summary_cbor)`. Similar shape to RUA; the `parsed_summary_cbor` carries best-effort-extracted fields.

### Retention

**Default 30 days** (admin-tunable `mail.dmarc.forensic_retention_days`). Shorter than RUA because the PII payload is heavier. The admin can extend to 90 d but is warned in the UI; longer than 90 d requires editing the catalog binding, which is a deliberate admin action.

---

## Outbound aggregate-report sender

The mirror of TLSRPT outbound (per `smtp-server.md` § TLSRPT outbound reporter). For every peer domain whose published DMARC policy includes a `rua=` URI:

1. **Daily aggregation window** — bucket every inbound message claiming to be from a peer domain by (`peer_domain`, `source_ip`, `header_from`, `disposition`, `dkim_aligned`, `spf_aligned`, `count`). Wall-clock window: 00:00 UTC ± per-domain jitter (avoid stampede; reuse the TLSRPT outbound jitter helper).
2. **Fetch peer policy** — at the start of each day, look up `_dmarc.<peer-domain>` TXT. Parse the `rua=` URIs (RFC 7489 §6.2 allows multiple, comma-separated, each a mailto: URI; we honor the first 2). If no `rua=` published, we generate no report for that peer that day.
3. **Serialize** — XML conforming to RFC 7489 §A.1. gzip-compress. Build the report-carrying message: `Subject: Report Domain: <peer> Submitter: <our-domain> Report-ID: <uuid>`; `From: dmarc-report@<our-primary-domain>`; attachment `application/gzip; name="<our-domain>!<peer>!<begin>!<end>.xml.gz"` (RFC 7489 §7.2.2 filename convention).
4. **Submit** — to the peer's `rua=` mailto address via our own MTA, normal outbound delivery path. The MTA queues + retries per the standard outbound schedule (`smtp-server.md` § Retry schedule).
5. **Logging** — retain the report JSON-summary blob in `dmarc_outbound_aggregate_reports(peer_domain, report_id, sent_at, payload_xml_gz)` for **7 days** for ad-hoc admin inspection. The full XML payload is kept for the 7 d window; older rows prune.

**Reports are mailto-only.** RFC 7489 §6.2 only specifies mailto: URIs for `rua=` (TLSRPT-style https: ingestion is a TLSRPT extension and not a DMARC one). Peer policies with `rua=https://...` are ignored with a counter `dmarc_outbound_https_uri_skipped_total` — the URI shape is non-conformant for DMARC, we don't extend.

### Size cap

A single peer's daily report capped at **10 MiB** gzip-compressed (RFC 7489 §6.2 allows a `rua=mailto:...!10MB` "ruri-size" suffix to cap reports; we honor the cap on receive and emit our own at 10 MiB without prompting). Reports exceeding the cap fall back to one of:

- Splitting by `header_from` bucket — the report's records are partitioned across multiple sub-reports each below the cap.
- Truncating with a `<extension>` tag annotating the omission (RFC 7489 §A.1 extension mechanism).

### Generation is on by default

Per the same logic as TLSRPT outbound — non-coercive but cooperative. Admins can disable globally via `mail.dmarc.outbound_aggregate_enabled = false` (default `true`); per-peer-domain disable lives behind the same catalog row (`mail.dmarc.outbound_aggregate_per_domain_overrides`).

---

## Outbound forensic-report sender

**Off by default**, by the same PII logic as inbound forensic. Admin can opt in per the catalog (`mail.dmarc.outbound_forensic_enabled = false`); enabling means: for inbound mail that fails DMARC alignment against a peer domain whose `ruf=` is published, we generate one forensic report per failure and send to the peer's `ruf=` URI.

Format: `multipart/report; report-type=feedback-report` per RFC 5965; one part is the AFRF (Abuse Reporting Format per RFC 5965) summarizing the failure, another part is `message/rfc822-headers` (the failed message's headers only — never the body; we don't amplify body PII outbound). Sub-rate-limit: at most 50 forensic reports per peer per day (counter, capped — peer with a forensic-amplification attack against us gets rate-clamped). Storage / retention: `dmarc_outbound_forensic_reports`, 7 d retention.

---

## Admin pane summary surface

**No ratified page home yet** — there is no `bridges-detail` page (`bridges.md` § Scope keeps the mail bridge off the user-facing `bridges` page; `mail-policy-config.md` § Policy catalog banner). A proposed "DMARC reports" card would render a summary sourced from `fauna.bridges.list_dmarc_aggregate_reports(actor=admin, window_days=N)` — on the flat `admin-mail` page if a consuming track builds it, or deferred like the scanner/deliverability health badges (no current home, `mail-policy-config.md` § Policy catalog banner). Default window 7 days, switchable to 30 / 90 via a dropdown. **This card stands alone, not as an instance of a shared component family:** the `bridges-detail` admin dashboard + the generalized `<concern>-summary-card` pattern this card was originally modeled on was **ratified OUT 2026-06-01** (`mail-observability.md` § ⚠ Superseded UI premise — the six-card observability catalog + "plus DMARC + Deliverability cards" framing is superseded, and its ids are dropped). Per that same 2026-07-08 review (D4), this card's own ids stay **proposed, pending its own ui.yaml ratification** (§ ui.yaml surface, below) on whatever page a future consuming track picks — never a resurrection of `bridges-detail` or the retired shared-card family.

### Card content

| Surface | Source |
|---|---|
| **Senders claiming our domain (last 7 d)** | Sum of `<record>.row.count` from reports where `policy_published.domain` ∈ `local_domains`, partitioned by `<record>.row.source_ip` PTR-resolved hostname (or raw IP if no PTR). Top 20 sources, sortable by count. |
| **Alignment breakdown** | Sum of `<record>.row.count` partitioned by `<row.policy_evaluated.dkim>` ∈ {pass, fail} × `<row.policy_evaluated.spf>` ∈ {pass, fail}. 2×2 grid. Helps the admin see "is our DKIM signing or our SPF record the source of failures?" |
| **Disposition breakdown** | Sum of `<record>.row.count` partitioned by `<row.policy_evaluated.disposition>` ∈ {none, quarantine, reject, ...}. Tells the admin how many of those mails the peer MTAs rejected on our behalf. |
| **Reporting peers** | Distinct `submitter` values across the window. |
| **Forensic reports (last 30 d)** | Count of `dmarc_inbound_forensic_reports` rows, or "Forensic reports disabled" with a "Enable" link if `ruf=` is unpublished. |
| **Empty state** | "No DMARC reports received yet. Peer MTAs usually take 1–7 days to send the first report after we published the policy on <date>." |

### ui.yaml surface

**Proposed, pending ui.yaml ratification (2026-07-08 review ruling):** none of the IDs below exist in `tests/e2e-unified/ui.yaml` yet — they are this doc's *proposal* for the eventual entry (per `mail-policy-config.md` § Policy catalog architectural rules, every Tier-2 surface gets one), to be added with explicit user approval when the card is built. Do not implement against these IDs before they land in ui.yaml.

**Placement in the admin-telemetry split (design pass 2026-09-19; user ruling 2026-09-25):** this summary is a **tier-2** surface, not part of the mail health readout the user ratified on 2026-09-25 (`mail-deliverability.md` § Admin-pane Deliverability surface → *The mail health readout* — its approved id set contains no `dmarc-summary-*` id) — the nest holds only the record-*publication* check today, while the whole report-ingest pipeline the summary reads is unbuilt (§ Implementation status today). Tier 2 was ruled demand-driven, so the summary **stays proposed with no build row**, gated on that pipeline and on its own rule-A pass; the tier split and the triggers that re-open it are owned by `mail-observability.md` § Design pass 2026-09-19.

The card is one component (`dmarc-summary-card`); its proposed elements:

- `dmarc-summary-card-window-selector` (dropdown)
- `dmarc-summary-card-senders-list` (component, indexed)
- `dmarc-summary-card-alignment-grid` (component)
- `dmarc-summary-card-disposition-grid` (component)
- `dmarc-summary-card-peers-list` (component, indexed)
- `dmarc-summary-card-forensic-status` (text + optional `dmarc-summary-card-enable-forensic-button`)
- `dmarc-summary-card-empty` (text, visible only on the empty path)

### Drill-down

Each top-line metric is a hyperlink that opens a detail page (`dmarc-detail-page` — proposed ID, same pending-ratification status as the card elements above) — the same summary card schema, scoped to one filter (one source IP, one alignment-failure cell, one peer). The detail page renders the underlying `<record>` rows as a flat list with raw counts, dates, and a "view raw XML" toggle per row that decodes `payload_xml_gz` server-side and renders the relevant element.

---

## Wire shapes (named, not redefined here)

| RPC | Caller | Purpose | Notes |
|---|---|---|---|
| `fauna.bridges.ingest_dmarc_aggregate_report` | MTA | accept inbound DMARC aggregate report payload | DAG-CBOR `(payload_bytes, source_envelope)`; nest validates + stores |
| `fauna.bridges.ingest_dmarc_forensic_report` | MTA | accept inbound DMARC forensic report payload | same shape |
| `fauna.bridges.list_dmarc_aggregate_reports` | admin client | summary query for the admin pane card | `(window_days, our_domain?)` → list of `parsed_summary_cbor` entries + counts |
| `fauna.bridges.get_dmarc_aggregate_report` | admin client | full-payload re-parse for the detail-page raw-XML view | `(report_id)` → `payload_xml_gz` + decoded summary |
| `fauna.bridges.fetch_outbound_dmarc_due` | MTA | poll for outbound aggregate-reports to send today | mirrors `fauna.bridges.fetch_outbound_due` for the SMTP queue |
| `fauna.bridges.dmarc_outbound_completed` | MTA | confirm an outbound report was delivered | nest updates `dmarc_outbound_aggregate_reports.sent_at` |

Wire-level shape lives in the nest implementation track and the bridge-kind catalogue (`docs/goal/architecture/apps/bridges.md` § Bridge-kind catalogue — its home after the 2026-07-08 move out of storage-modes.md); this doc owns which RPCs exist and what each carries, not the byte-level encoding. All six are **target-state, unbuilt** (§ Implementation status today).

---

## Architectural rules

- **DMARC policy publication is from a Fauna app, never from a zonefile.** Per the product invariant (`principles.md` § One configuration surface), every DMARC tag is a nest-state value set in the admin's UI and assembled into a TXT record by nest. Hand-editing the TXT record at the DNS provider is **not** a supported configuration channel — a managed-mode **client** reconcile overwrites it back to the expected record, and a manual-mode deployment sees it as red drift on the `admin-dns` verify. (The nest itself never writes DNS — `dns-management.md`.)
- **Default-strict.** `p=reject; sp=reject; pct=100; adkim=s; aspf=s` is the shipped policy. The "monitor first" rollout pattern is for legacy infrastructure; fresh deployments have none.
- **`dmarc-report@` is reserved.** Per `smtp-server.md` § abuse@ / postmaster@ role-address routing, the local-part is reserved deployment-wide; it routes envelope-time to the DMARC processor, never to the admin mailbox, never to a user. The same recipient on a peer domain is *their* DMARC processor — we don't get to assume our shape, we just respect the peer's `rua=` URI.
- **Receive-side: accept-on-malformed.** Peer MTAs have no retry obligation for malformed reports (mirroring TLSRPT inbound). We accept on the wire (SMTP 250), log + count, and drop the payload. Rejecting malformed DMARC reports would make us a poor cooperative-MTA citizen and reduce our signal.
- **Forensic reports are PII; default-off both directions.** Inbound: no `ruf=` published → we get zero. Outbound: `outbound_forensic_enabled = false` → we send zero. Admin opt-in is one toggle, with the PII consequence stated at the toggle.
- **mailto: only for DMARC reports.** RFC 7489 §6.2 specifies mailto:; we don't extend to https: (TLSRPT does; DMARC doesn't). Peer policies with `https:` rua are skipped with a counter.
- **Size cap honored.** RFC 7489 §6.2's `ruri-size` suffix is honored on outbound; our own outbound is capped at 10 MiB; inbound oversize is dropped with a counter (accepted on the wire — SMTP 250 — like malformed).
- **Aggregate vs. enforcement.** This doc owns reporting (RUA + RUF). DMARC **enforcement** at inbound (the policy-stack stage that quarantines/rejects per a sender's published DMARC policy) is in `smtp-server.md` § Inbound policy stack. The two share zero code paths — verification produces an alignment verdict; the verdict feeds both the enforcement decision and the aggregate-report bucket.
- **Per-domain customization preserved.** A multi-domain deployment can run `p=none` on one rolling-out domain while running `p=reject` on others. The catalog binding (`mail.dmarc.per_domain_overrides`) is a map; the UX renders one row per `local_domains` entry.
- **TXT record rewrites are rate-limited (target; not yet built).** At most 1 rewrite per `local_domains` entry per minute — a constraint on the **client's** managed-mode reconcile (the client is what writes to the DNS provider; the nest never publishes DNS — `dns-management.md` § The two modes). The reconcile itself is built and live (§ Implementation status today); this specific per-minute throttle on top of it is not yet implemented. Once built, it defends against admin-UI-loop / DDoS-of-provider scenarios.
- **The XML payload is archived gzipped.** `payload_xml_gz` in `dmarc_inbound_aggregate_reports` is the source of truth; the `parsed_summary_cbor` index is computed at ingest time. If our parser improves, we re-parse from the archive — never trust the index as the only source. Same for outbound.

## Don't do these

- Don't route `dmarc-report@` to the admin mailbox. The admin pane consumes the parsed summary card, not the raw reports; landing 50 KB XML attachments in the admin's INBOX every day is the wrong default and the wrong UX.
- Don't default-publish `ruf=`. Peer MTAs receiving a `ruf=` will start sending forensic reports about every mail that fails DMARC alignment, and those reports carry third-party recipient PII. The default-off collects no third-party PII we have no need for; the default-on does.
- Don't generate forensic reports by default outbound. Same PII logic in the opposite direction — we don't want to amplify a third-party sender's PII to a peer domain that may not handle it carefully.
- Don't extend DMARC reporting URIs to `https:`. RFC 7489 §6.2 is mailto:-only; honoring https URIs is non-conformant and confuses peer MTAs that observe our behavior.
- Don't reject inbound DMARC reports on alignment failure. A peer MTA's outbound DMARC mail may itself fail alignment for a quirk in their setup; ingesting the report is still the cooperative behavior. The verdict-on-the-wrapper rides to the admin pane as a metadata signal, not a reject decision.
- Don't tie DMARC reporting to a box's grant posture. The reports are deployment-data (the admin's monitoring signal), not user-data; they sit in nest's plaintext floor (per `docs/goal/architecture/encryption-at-rest.md` § Plaintext floor) the same way TLSRPT reports do, regardless of which capability grants exist on the box (no-modes — `storage-modes.md` § One binary, one posture).
- Don't surface a "delete this DMARC report" button in the admin UI. The reports are a forensic trail; retention is policy-driven (90 d default), individual-row delete is not part of the admin's surface. (An admin asking "I want this report gone right now" is asking for a code change to the retention floor, which is a different conversation.)
- Don't store the `_dmarc.<domain>` private TXT record content anywhere except as derived state. The record is **assembled** from nest-state on every read; storing the assembled record creates a divergence risk between the policy catalog and the published reality. The DNS provider's view is the ground truth for "what peers see" and the catalog's view is the ground truth for "what the admin chose"; they reconcile on each rewrite, in one direction (catalog → provider).
- Don't bundle DMARC enforcement (the inbound policy-stack decision) into this doc. `smtp-server.md` owns it; cross-link, don't duplicate. Same for TLSRPT (`smtp-server.md` § TLSRPT inbound + outbound).

## Reading list

1. `principles.md` § One configuration surface — the rule that puts DMARC policy in the Fauna app UI, not in a zonefile.
2. `docs/goal/behavior/smtp-server.md` § abuse@ / postmaster@ role-address routing — the `dmarc-report@` row this doc resolves; § TLSRPT inbound report listener — the receive-pipeline shape mirrored here; § TLSRPT outbound reporter — the outbound shape mirrored here; § Inbound policy stack — DMARC enforcement (separate concern).
3. `docs/goal/behavior/mail-policy-config.md` § Tier 2 — DMARC — the policy catalog entries this doc plugs into.
4. `docs/goal/behavior/mail-observability.md` § ⚠ Superseded UI premise — the retired `bridges-detail` / `<concern>-summary-card` shared-dashboard pattern this doc's card proposal is independent of, not an instance of (§ Admin pane summary surface, above).
5. `docs/goal/behavior/onboarding.md` § 4. DNS configuration (`dns_config`) — the DNS-provisioning path that writes `_dmarc.<domain>` TXT.
6. `docs/goal/architecture/encryption-at-rest.md` § Plaintext floor — the rationale for keeping DMARC reports in the plaintext floor regardless of a box's grant posture.
7. (design ratified 2026-05-07; tracked internally) § MDA role, § Bridge↔nest WS-RPC API — the wire-level shape the new RPCs align with.
8. RFC 7489 (DMARC — §6.3 record format, §7.2 + Appendix A.1 aggregate-report schema, §7.3 forensic-report format, §6.2 reporting URIs + ruri-size, §3 PII considerations); RFC 5965 (AFRF, the wrapping multipart for forensic reports); RFC 8460 (TLSRPT — the shape DMARC aggregate-report receive mirrors).
