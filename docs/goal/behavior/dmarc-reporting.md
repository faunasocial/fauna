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
4. The admin's Fauna app shows the reports: messages reported, alignment and disposition totals, top sources and senders, and each report (proposed — page home and elements designed 2026-10-09, pending the user's rule-A sign-off, § Admin pane summary surface).

**Bar: full RFC 7489 conformance on receive + publish, plus the cooperative-MTA outbound reporter that responsible mail servers run.** A DMARC-conformant peer reading our TXT record sees a coherent policy; a DMARC-conformant peer that publishes `rua=` receives correctly-shaped reports from us; and the admin running this deployment can read the inbound reports without `xq` on raw XML.

**Default-strict for our own published policy.** Fresh deployments ship `p=reject; sp=reject; pct=100` — strict beats permissive (per `smtp-server.md` § Goal). The "monitor first, then quarantine, then reject" incremental-rollout model is for legacy infrastructure migrating to DMARC; a fresh nest has no legacy infrastructure. Admin can soften it (knob — see `mail-policy-config.md`); we don't.

---

## Implementation status today

- **Record-body assembly — DONE.** The published `_dmarc.<domain>` body is assembled by the shared `fauna_mail::dmarc_publish` builder (`dmarc-policy` feature) from a `DmarcPublishPolicy` whose `Default` matches § Record shape, with the per-domain `mail_domains.dmarc_overrides` partial overlaid. Both producers consume it: the nest DNS surface (`fauna.dns.list_records` / `verify_records`) and the onboarding provisioner (`libs/fauna-provisioning`), so the published record matches what verify expects.
- **Deployment-wide `mail.dmarc.*` catalog — write path built 2026-10-11; the app control is not.** The catalog rests in the single-row `mail_dmarc_policy` table (`DmarcPolicyOverrides` in `db/mail_policy.rs`: the ten record tags as a deployment-wide `DmarcOverrides` partial, plus the two retentions of § Retention), written whole by the Admin kind `fauna.bridges.put_dmarc_policy` (one optional field per knob, absent ⇒ the shipped default; the nest refuses `pct` over 100, a destination that is not a `mailto:` URI, and a retention outside its bounds) and read back by its admin twin `get_dmarc_policy` as the effective `DmarcReportingPolicy`. Like the alias knobs it is consumed nest-side, never projected to the bridge: the DNS surface's assembler takes `DmarcPolicyOverrides::publish_base` (the catalog overlaid onto the shipped default) as the base every domain's own partial is overlaid onto, and the two daily prunes read the effective retentions. The shared `MailPolicyMachine` carries the catalog as its `dmarc` group (`DmarcPolicyView`, `SaveDmarc`; the `fo=` picker catalog `dmarc_forensic_mode_options`, UniFFI- and wasm-exported) — hydrated and saved on every app that renders the machine, **but no app renders a DMARC control yet**: the ids the control needs (the forensic opt-in toggle with its PII text, the `fo=` select, the two retention fields, a save button, all under `admin-mail-dmarc-*`) await the user's rule-A approval, tui to lead. Until then the catalog is reachable over the wire only. Not in the catalog yet: `mail.dmarc.publish_enabled` (the DNS builder would have to drop the row), the `outbound_*` knobs (the aggregate sender, built 2026-10-11, runs at its defaults; its catalog contract is a captured pass), and `per_domain_overrides`, which is the per-domain column, next sentence. A per-domain override *is* read from the stored column when the record is assembled, and **its policy keys are written** (built 2026-10-04): `fauna.bridges.update_local_domain_config` carries the optional `dmarc_policy_mode`, which the nest merges into the stored partial by the shared `fauna_mail::dmarc_publish::set_policy_mode_json` (both policy keys set together, the default `reject` clearing both, every other key kept), and the shared local-domains machine exposes each domain's policy (`LocalDomainView::dmarc_policy`) and sends it (`LocalDomainAction::UpdateConfig`). **The control is built on tui, windows, linux, android and web:** `admin-dns-domain-dmarc-policy-select` renders on each `admin-dns-domain` row from `LocalDomainView::dmarc_policy`, its option catalog shared as `fauna_client_mail_settings::local_domains::dmarc_policy_options` (UniFFI-exported and wasm-exported as `dmarcPolicyOptions`, each option carrying its RFC 7489 tag and the policy it names); the other two apps (macos, ios) have not lifted it yet, so on them softening one domain's policy, which § Multi-domain deployments allows, is reachable over the wire only. The other override keys have no writer, by design (§ Multi-domain deployments).
- **Publication lifecycle — built, one piece deferred.** The managed-mode auto-publish (a *client-side* create-only reconcile — the nest never writes DNS) **is built**: `_dmarc.<domain>` TXT is one of the records `DnsManagementMachine::publish` zone-publishes via the held provider credential, live on all 7 apps (`dns-management.md` § Fauna-managed → Publish, § Implementation status today). A manual-mode domain (no covering credential — every domain today) still gets the onboarding provisioner's initial publish plus the unified DNS surface's expected-vs-live verify for any later admin re-paste. **Deferred:** the 1-rewrite-per-minute rate limit on the managed reconcile is not yet built (§ Architectural rules, below).
- **Aggregate-report receive (RUA) — built 2026-10-09, nest side; no app shows it yet.** `dmarc-report@` on any local domain resolves at RCPT TO to `ResolveRecipientReply::Processor { DmarcReport }` (`resolve_local_recipient` in `bridge_routing_handlers.rs` — no admin needed, so a report arriving before the claim is still ingested), and both MTA doors (`bins/fauna-bridges/internal/mta/processor.go::dispatchProcessor`, from the inbound MX path and the submission path; the app send door relays it through our own MX) hand the whole message to `fauna.bridges.ingest_dmarc_report` ahead of any local commit, never to a mailbox. The nest classifies the message (a forensic report takes the forensic leg, next bullet), finds the report attachment, applies the § XML validation bounds and parses it with the shared `fauna_mail::dmarc_report` (`dmarc-report` feature: a strict reader of its own that refuses any markup declaration — no XML-parser dependency), drops a malformed, oversize or foreign-domain report with its `nest_metrics` counter, and stores the rest in `dmarc_inbound_aggregate_reports`; `list_dmarc_aggregate_reports` and `get_dmarc_aggregate_report` read them back (Admin). Witnessed by the nest unit tests and `tests/e2e-unified/tests/test_mail_dmarc_reports.py`; feature page [`dmarc-reporting`](../../features/dmarc-reporting.md). **Retention is the admin-tunable `mail.dmarc.aggregate_retention_days` of § Retention** (built 2026-10-11 with the catalog; default `DMARC_AGGREGATE_RETENTION_DAYS`), read by the daily mail sweep in `main.rs` — the app control for it is the catalog's, above. **An oversize report's envelope is kept** (built 2026-10-13): the drop writes the envelope minus the body to `dmarc_inbound_aggregate_oversize` (capped at 1,000 rows, pruned by the same daily sweep at the same retention), and `list_dmarc_aggregate_reports` returns it as `oversize`; the retention prune is witnessed on the api tier by `test_oversize_report_keeps_an_envelope_for_triage_and_retention_prunes_it`, through a `test-hooks` route that runs the production prune at a scripted instant. **Not yet built on the receive side:** the admin summary surface (§ Admin pane summary surface — designed 2026-10-09 as the `admin-mail-reports` page, its ids proposed and awaiting the user's rule-A sign-off; no push, by design — § *Live refresh*).
- **Forensic-report receive (RUF) — built 2026-10-11, nest side; the opt-in is reachable over the wire, not from an app, so none arrive yet.** The same `fauna.bridges.ingest_dmarc_report` classifies each `dmarc-report@` message by its top-level header block (the shared `fauna_mail::dmarc_forensic::is_forensic_report`), and a forensic one takes the forensic leg: the § Format leniency parse (`fauna_mail::dmarc_forensic`), the domain check, storage in `dmarc_inbound_forensic_reports` with the § Storage PII rule, and the Admin reads `list_dmarc_forensic_reports` / `get_dmarc_forensic_report`. Witnessed by the fauna-mail and nest unit tests and `tests/e2e-unified/tests/test_mail_dmarc_reports.py`. **Retention is the admin-tunable `mail.dmarc.forensic_retention_days`** (built 2026-10-11 with the catalog; default `DMARC_FORENSIC_RETENTION_DAYS`, ceiling 90 d), read by the same daily mail sweep. **The opt-in's nest half is built:** `mail.dmarc.ruf_publish` (and `fo_mode`) in the catalog make every domain's expected record carry `ruf=mailto:dmarc-report@<primary>; fo=<mode>` — witnessed by the nest unit tests and `test_mail_dmarc_reports.py::test_forensic_opt_in_asks_peers_for_reports_in_the_expected_record`. **Not built:** the app control for the opt-in (§ Default off's toggle and its PII text — the catalog bullet above has the ids awaiting rule-A approval), so no deployment publishes `ruf=` yet and the leg receives only what a peer sends unasked; any app surface for the reports (§ Admin pane summary surface leaves the forensic section to a later design pass).
- **Outbound aggregate-report sender — built 2026-10-11, nest side; no app shows the sent reports.** The MTA verifies every inbound message with the shared `fauna_mail::auth::verify_inbound_observed` and, before any enforcement gate (so mail the perimeter refuses is counted), reports its `DmarcObservation` on `fauna.bridges.report_dmarc_observation`; the nest skips mail claiming one of our own domains, states the disposition its inbound enforcement switches gave the message (`fauna_mail::outbound::dmarc::applied_disposition`) and counts it into an in-memory aggregator (`AppState.email.dmarc_aggregator`, at most 10,000 From domains and 10,000 rows each a window; a restart loses the open day, as for TLSRPT). Once a UTC day, at 00:00 plus up to an hour's jitter, `bins/fauna-nest/src/outbound_dmarc.rs` resolves each From domain's policy, keeps the destinations § Outbound aggregate-report sender allows (`dmarc_outbound_https_uri_skipped_total` and `dmarc_outbound_destination_unauthorized_total` count the ones it drops), builds the RFC 7489 §A.1 XML in shared Rust (`fauna_mail::outbound::dmarc`), splits it under the size cap and enqueues each part on the standard outbound queue from `dmarc-report@<primary>`, recording it in `dmarc_outbound_aggregate_reports` (7 days, pruned by the daily mail sweep). Witnessed by the fauna-mail and nest unit tests, the Go MTA tests and `tests/e2e-unified/tests/test_mail_dmarc_reports.py`, where the report reaches the peer's MX. **Not built:** the `mail.dmarc.outbound_aggregate_enabled` switch and its per-domain overrides (§ Generation is on by default) wait on the unbuilt `mail.dmarc.*` catalog, so generation is unconditionally on; no admin surface lists the sent reports.
- **The outbound forensic sender — NOT built.** Nothing generates a forensic report; § Outbound forensic-report sender is target-state. (`tlsrpt@`, the other report role address, has its own processor since 2026-10-11 — `smtp-server.md` § TLSRPT inbound report listener.)

## DMARC policy publication

The deployment's primary domain (and every additional `local_domains` entry — see `mail-policy-config.md` § Tier 2 — Whether mail runs at all) gets a `_dmarc.<domain>` TXT record published. The record is **assembled and served as expected state by nest** from the policy values (per the product invariant — DMARC policy is set from a Fauna app, not from a hand-edited zonefile): nest exposes it via `fauna.dns.list_records` / `verify_records` on the unified DNS surface. **Publication is never nest's** — the nest holds no DNS-provider credential (`dns-management.md` § The two modes): the record reaches the provider via the onboarding provisioner's initial client-side publish, the client's managed-mode reconcile, or the admin's manual copy-paste from the `admin-dns` page.

### Record shape

**This table owns the default values** (ownership ruling, 2026-07-08 mail-cluster review — the code's one copy, `fauna_protocol::bridge_routing::DmarcReportingPolicy::default()`, anchors here, and the assembler's `DmarcPublishPolicy::default()` derives from it): `mail-policy-config.md`'s `mail.dmarc.*` catalog rows carry tier/binding/surface only and point here for the values, so there is exactly one value copy per knob.

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

A nest serving multiple `local_domains` publishes one `_dmarc.<domain>` per domain. Per-domain policy customization is allowed (the admin can run `p=none` on one rolling-out domain and `p=reject` on the others); the catalog binding is `mail.dmarc.per_domain_overrides` (a map of `<domain>` → partial-record). The default for each entry is "inherit from the deployment-wide defaults above." **App surface (designed 2026-10-01; the id approved by the user 2026-10-03, as the one stated exception to the 2026-05-24 directive that DMARC-record publishing has no manual admin UI — an admin whose domain also sends through another service must be able to soften that one domain, and no automatic rule can know it; the write is built, the control is built on tui, windows, linux, android and web — § Implementation status today):** one indexed select on each `admin-dns-domain` row, `admin-dns-domain-dmarc-policy-select`, offering `reject` (the default), `quarantine` and `none`. It sets the domain's `policy_mode` and `subdomain_policy_mode` together — an admin softening a domain means the whole domain — and choosing the default clears both keys rather than storing them. The other override keys below get no control: nobody has a reason to choose them per domain, and until someone does they stay at the deployment default (`../principles.md` § One configuration surface — a value no human chooses is a constant). The write is an additive, typed, optional field on `fauna.bridges.update_local_domain_config`, absent meaning unchanged.

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

So `{"policy_mode":"none","pct":50}` on a rolling-out domain renders `v=DMARC1; p=none; sp=reject; pct=50; adkim=s; aspf=s; rua=mailto:dmarc-report@<primary>; ri=86400` — the `rua` still points at the deployment-wide processor. Malformed override JSON falls back to the deployment-wide default body (the policy-key write path is typed, so the read surface stays robust). The body assembler is shared Rust (`fauna_mail::dmarc_publish`, `dmarc-policy` feature) so the nest DNS surface (`fauna.dns.list_records` / `verify_records`) and the onboarding provisioner (`fauna-provisioning`) emit the identical record.

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
3. **After DATA accept**, the MTA calls `fauna.bridges.ingest_dmarc_report(raw_message, source_envelope, received_at)` instead of the usual mailbox ingest — the whole message, so telling an aggregate report from a forensic one and finding the report attachment are the nest's shared-Rust parse, not Go. A recipient processor is handed off before any local recipient of the same envelope commits; a hand-off the nest cannot take is a `451`, never a fallback to a mailbox.
4. The MTA's normal verdict / metric / log shape still fires — the dispatch is a routing decision, not a bypass of the policy stack. SPF / DKIM / DMARC verification on the **report-carrying** message still runs (a forged DMARC report from an attacker is treated like any other suspicious mail), and the verdict ride to the processor as `source_envelope.auth_results`.

The MTA does **not** require the report-carrying mail to itself align under DMARC — a peer's MTA legitimately sends DMARC reports from a domain that publishes no DMARC policy of its own, or that fails alignment because of a quirk in their own outbound path. Failing to ingest those reports would silence the network's voice about us. The verdict labels carry through; the processor decides whether to surface "this report's wrapper failed alignment" in the admin pane.

### XML validation

The payload (gzip-decompressed) is a UTF-8 XML document conforming to the schema in RFC 7489 §A.1. Validation pipeline:

1. **Decompress** — `application/gzip` (sometimes `application/zip` from older senders — we accept both). Reject payloads > 50 MiB pre-decompression / 500 MiB post-decompression as oversize (counter `dmarc_inbound_aggregate_oversize_total`); store the rejected envelope minus the body for ops triage.
2. **Parse** — strict XML parse against the RFC 7489 §A.1 schema. The schema covers `<feedback>` root, `<report_metadata>`, `<policy_published>`, `<record>` (1..N — each record describes one (source-IP, count, disposition) bucket). The payload is attacker-supplied, so the parser accepts only well-formed elements, attributes, character data, comments, processing instructions, CDATA and the predefined and numeric character references: **any markup declaration (`<!DOCTYPE`, `<!ENTITY`, …) is refused outright** — no entity is ever expanded or fetched — and element nesting is bounded (32 deep). Element names match by local part, so a namespaced report (RFC 7489bis) parses like an unqualified one.
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
| `payload_xml_gz` | BLOB | the report's XML as received, gzipped (a zip or bare-XML container is re-packed as gzip), for re-parse if our parser improves |
| `parsed_summary_cbor` | BLOB | DAG-CBOR-encoded summary (counts, top source-IPs, top header.from values, disposition breakdown) computed once at ingest time and read by the admin pane query path; this is the index, the gz blob is the archive |
| `source_envelope_blob` | BLOB | DAG-CBOR envelope context (client_ip, helo, mail_from, auth_results) — for forensic correlation if a report looks suspicious |

### Retention

**Default 90 days**, admin-tunable via `mail.dmarc.aggregate_retention_days` (1 to 365 days — the nest refuses more; a report older than a year answers no question the admin pane asks). Reports older than retention are pruned by the nest's daily mail sweep, the pass TLSRPT inbound reports also ride (`smtp-server.md` § TLSRPT inbound report listener — 90 d default).

### Push to admin pane

None (decided 2026-10-09): a per-report insert emits no push; the admin page re-reads on open and on its refresh button — § Admin pane summary surface → *Live refresh* owns the decision and the additive kind that would carry one if live refresh ever became owed.

---

## Forensic-report receive (RUF)

DMARC forensic reports are per-message failure reports — each one carries the headers (and optionally the body — RFC 7489 §7.3 leaves this implementation-defined) of a specific inbound mail that failed DMARC alignment against our domain. They're PII-heavy: the reported message is a third party's outbound, and the original recipient's address is exposed in the report.

### Default off

We do **not** publish `ruf=` by default. A deployment that has not opted in receives zero forensic reports, ever — peer MTAs don't generate them without a `ruf=` line in our TXT record. This is the conservative default that respects third-party PII.

The admin opts in via the Tier-2 knob `mail.dmarc.ruf_publish = true`, which causes nest to add `ruf=mailto:dmarc-report@<primary-domain>; fo=<mode>` to the published TXT record. The UI surfaces the PII tradeoff at the toggle point: "Forensic reports carry recipient email addresses from third-party senders. Enable only if your security team needs forensic-level signal."

### Envelope-time routing (when enabled)

Same as RUA, and through the same kind: `dmarc-report@<our-domain>` is the role-address destination, and the MTA hands every message for it to `fauna.bridges.ingest_dmarc_report` — the MTA cannot tell the two report kinds apart at RCPT TO, since they share the address. **The nest classifies** (decided 2026-10-11): a message whose top-level `Content-Type` is `multipart/report; report-type=feedback-report` (RFC 5965 §2) is a forensic report, anything else an aggregate one. The classification reads the header block only, lives in shared Rust beside the parse, and adds no `RecipientProcessor` value; the reply names the kind for the MTA's log line.

### Format leniency

RFC 7489 §7.3 is loose about format — the wrapper is `message/feedback-report` per RFC 5965, the embedded report is `message/rfc822` (full message) or `message/rfc822-headers` (headers only). Real-world DMARC forensic-report senders are vendor-divergent (Gmail's shape differs from Microsoft's differs from Yahoo's). Our parser is **lenient on the inner shape** (best-effort header extraction, no schema-strict reject) and **strict on the wrapper** (must be valid RFC 5322 + RFC 5965 multipart). Failures of the lenient parse increment `dmarc_inbound_forensic_parse_partial_total` and store the raw payload anyway for admin triage. Concretely, the wrapper must be a `multipart/report` of report type `feedback-report` carrying a `message/feedback-report` part whose `Feedback-Type` is `auth-failure` (RFC 6591) and which has RFC 5965 §3.1's required `User-Agent` and `Version` fields; anything less is dropped as malformed (`dmarc_inbound_forensic_malformed_total`, still a `250`). The reported message (`message/rfc822`, `message/global` or `text/rfc822-headers`) is read for its `From:` and `Message-ID` only; a report without one, or whose reported message has no `Message-ID`, is the partial case. A report mail over 10 MiB is dropped as oversize (`dmarc_inbound_forensic_oversize_total`). The report's domain is its `Reported-Domain` field, else the reported message's `From:` domain; one we do not serve is dropped (`dmarc_inbound_forensic_wrong_domain_total`), like RUA's step 4.

### Storage

`dmarc_inbound_forensic_reports(submitter, our_domain, received_at, reported_message_id, reported_arrival_date, payload_raw_gz, source_envelope_blob, parsed_summary_cbor)`. Similar shape to RUA; the `parsed_summary_cbor` carries best-effort-extracted fields. A forensic report has no report id, so a row is keyed by a nest-minted `id` (what `get_dmarc_forensic_report` takes) and a re-sent copy is caught by `payload_sha256`, the digest of the message as received. `submitter` is the report mail's `From:`, else its envelope sender; `payload_raw_gz` is the whole report mail, gzipped.

**PII rule.** The index — `parsed_summary_cbor` and the listed columns, everything `list_dmarc_forensic_reports` returns — names no recipient of the reported message (`Original-Rcpt-To`, `To`, `Cc`) and carries none of its subject or body: it holds the failure mechanism, source IP, the reported message's envelope and header senders, arrival date, delivery result, identity alignment, DKIM domain and the reporter's user agent. The third party's recipients and content rest only in `payload_raw_gz`, which only the per-report `get_dmarc_forensic_report` returns, and which the retention below deletes. The nest's log lines name the domain and submitter, never the reported message's addresses.

### Retention

**Default 30 days** (admin-tunable `mail.dmarc.forensic_retention_days`). Shorter than RUA because the PII payload is heavier. The admin can extend to 90 d but is warned in the UI; the nest refuses more than 90 d — there is no file or binding an admin edits to go further (`../principles.md` § One configuration surface), so a longer period is a code change.

---

## Outbound aggregate-report sender

The mirror of TLSRPT outbound (per `smtp-server.md` § TLSRPT outbound reporter). For every peer domain whose published DMARC policy includes a `rua=` URI:

1. **Daily aggregation window** — every inbound message is counted at the SMTP perimeter, after verification and before any enforcement gate (a message the perimeter refuses is counted with disposition `reject`), toward the domain its From claims; mail claiming one of our own domains, or a name beneath one, is not counted. Messages that agree on (`header_from`, `source_ip`, `envelope_from`, `disposition`, `dkim_aligned`, `spf_aligned`, the DKIM and SPF `auth_results`) share one `<record>` with their `count`; the `peer_domain` a record is reported to is resolved in step 2. `disposition` is what our inbound enforcement switches did with the message (`mail-policy-config.md` § Inbound authentication enforcement). Wall-clock window: 00:00 UTC plus a jitter of up to an hour (avoid stampede; the TLSRPT outbound jitter helper).
2. **Fetch peer policy** — at the start of each day, look up `_dmarc.<from-domain>` TXT, then the same under each parent domain while at least two labels remain (the organizational-domain fallback, without a public-suffix list); the domain the record is found at is the `peer_domain`, and From domains sharing one are reported together. Parse the `rua=` URIs (RFC 7489 §6.2 allows multiple, comma-separated, each a mailto: URI; we honor the first 2). If no `rua=` published, we generate no report for that peer that day. A mailbox outside the peer domain (neither the same domain nor a name above or beneath it) receives the report only when its domain publishes `<peer-domain>._report._dmarc.<its-domain>` with `v=DMARC1` (RFC 7489 §7.1) — so a record cannot point our reports at a third party that never asked for them.
3. **Serialize** — XML conforming to RFC 7489 §A.1. gzip-compress. Build the report-carrying message: `Subject: Report Domain: <peer> Submitter: <our-domain> Report-ID: <uuid>`; `From: dmarc-report@<our-primary-domain>`; attachment `application/gzip; name="<our-domain>!<peer>!<begin>!<end>.xml.gz"` (RFC 7489 §7.2.2 filename convention).
4. **Submit** — to the peer's `rua=` mailto address via our own MTA, normal outbound delivery path: the nest enqueues the report on the standard outbound queue, which the MTA drains with every other outbound message and retries per the standard outbound schedule (`smtp-server.md` § Retry schedule).
5. **Logging** — retain each queued report in `dmarc_outbound_aggregate_reports(report_id, peer_domain, destination, begin_at, end_at, sent_at, payload_xml_gz)` for **7 days** for ad-hoc admin inspection. The full gzipped XML is kept for the 7 d window; older rows prune.

**Reports are mailto-only.** RFC 7489 §6.2 only specifies mailto: URIs for `rua=` (TLSRPT-style https: ingestion is a TLSRPT extension and not a DMARC one). Peer policies with `rua=https://...` are ignored with a counter `dmarc_outbound_https_uri_skipped_total` — the URI shape is non-conformant for DMARC, we don't extend.

### Size cap

A single peer's daily report capped at **10 MiB** gzip-compressed (RFC 7489 §6.2 allows a `rua=mailto:...!10m` "ruri-size" suffix to cap reports; we honor the cap on receive and emit our own at 10 MiB without prompting — a destination's smaller `ruri-size` lowers it for that destination, a larger one never raises it). A report exceeding the cap is split: its records, ordered by `header_from`, are partitioned across sub-reports each below the cap, the split falling between `header_from` values wherever it can, the parts numbered `<report-id>.1`, `.2`, …. Nothing is truncated; a single record larger than a destination's cap (only a tiny `ruri-size` meets that) is left out of that destination's report.

### Generation is on by default

Per the same logic as TLSRPT outbound — non-coercive but cooperative. Admins can disable globally via `mail.dmarc.outbound_aggregate_enabled = false` (default `true`); per-peer-domain disable lives behind the same catalog row (`mail.dmarc.outbound_aggregate_per_domain_overrides`).

---

## Outbound forensic-report sender

**Off by default**, by the same PII logic as inbound forensic. Admin can opt in per the catalog (`mail.dmarc.outbound_forensic_enabled = false`); enabling means: for inbound mail that fails DMARC alignment against a peer domain whose `ruf=` is published, we generate one forensic report per failure and send to the peer's `ruf=` URI.

Format: `multipart/report; report-type=feedback-report` per RFC 5965; one part is the AFRF (Abuse Reporting Format per RFC 5965) summarizing the failure, another part is `message/rfc822-headers` (the failed message's headers only — never the body; we don't amplify body PII outbound). Sub-rate-limit: at most 50 forensic reports per peer per day (counter, capped — peer with a forensic-amplification attack against us gets rate-clamped). Storage / retention: `dmarc_outbound_forensic_reports`, 7 d retention.

---

## Admin pane summary surface

**Design pass 2026-10-09 — page home and element set PROPOSED, rule-A sign-off asked the same day; unbuilt on every app.** The receive pipeline the summary reads is built (§ Implementation status today), which was the one thing besides its own rule-A pass that stood in front of this surface: `mail-observability.md` § Design pass 2026-09-19 placed the summary in the demand-driven tier 2 *because* that pipeline was unbuilt, so whether the summary now leaves that hold is part of the same ask. Until the user signs off nothing below is ratified — no id below exists in `tests/e2e-unified/ui.yaml`, and no app implements against one (rule A — a new ui.yaml id, page or layout needs the user's explicit approval, `../ui/README.md`).

### Page home

**A new admin sub-page, `admin-mail-reports` ("Mail reports"), registered in `navigation.admin_pages` after `admin-mail`** — the one home for the reports peer mail servers send this nest about its own domains: DMARC aggregate reports now; TLSRPT inbound reports (`smtp-server.md` § TLSRPT inbound report listener) and DMARC forensic reports (§ Forensic-report receive) join it when their receive legs build, each as a section with its own rule-A pass. Why a page and not a section of `admin-mail`: that page is the flat policy *form* (`admin.md` § 6 Mail), whose one read-only exception — the mail health readout — earned its place by sitting under the enable toggle so the admin who turns mail on sees whether it works; a report browser (a window and a domain filter, two top lists, an indexed report list and a raw-report view) is a different kind of surface, and three report kinds would otherwise be three more sections on a seventy-element form. `admin-dns` was considered too — a domain's reports beside its DMARC policy select — and declined: its rows are per-domain DNS state, and the summary is a window over every domain. **The alternative put to the user** is the same element set as a section at the bottom of `admin-mail`, under the Aliases group. Either way the page re-reads on open and on its refresh button like every other admin page, adds no nav entry beyond the shared admin rail, and carries `admin-nav-back` and `error-message` (admin.md § Navigation model).

### What the page shows

Everything comes from `fauna.bridges.list_dmarc_aggregate_reports(window_days, our_domain?)` — the per-report `DmarcAggregateSummary` index computed at ingest (§ Storage) — plus `get_dmarc_aggregate_report` for one opened report. The window totals are folded client-side in shared Rust over the listed entries (§ Where logic lives):

| Surface | Source |
|---|---|
| **Window and domain filter** | 7 (default) / 30 / 90 days; every domain (default) or one `local_domains` entry |
| **Messages reported** | Σ `message_count`, the number of reports, the distinct `submitter` count (the reporting peers) |
| **Alignment** | Σ `dmarc_pass`, `dkim_pass`, `spf_pass`, each as a count and a share of messages reported — "is our DKIM signing or our SPF record the source of failures?" |
| **Dispositions** | Σ `disposition_none` / `disposition_quarantine` / `disposition_reject` — how many of those mails the peers rejected on our behalf |
| **Top sources** | per-value sum over the reports' `top_source_ips`, ten largest, raw IPs (no PTR lookup — a render never queries DNS); approximate at the margin because each report carries only its own top ten (`SUMMARY_TOP_N`), so the page labels the list "by reported messages" |
| **Top senders** | the same over `top_header_from` |
| **Reports** | one row per report, newest received first: submitter, our domain, the report period, messages, DMARC pass; a *view* button opens that report's XML (gunzipped in shared Rust, shown as text) |
| **Truncated** | the nest's page cap (1,000 reports a window, `DMARC_LIST_PAGE_CAP`) was hit: the totals cover the newest 1,000 and the page says so |
| **Empty state** | "No DMARC reports yet. Peer mail servers usually send the first report one to seven days after the policy was published." |

Not on the page: a forensic-report count or an "enable forensic" affordance (the receive leg is built nest-side but the `ruf` knob is not — the forensic section and its ids are a later design pass); a per-report delete (§ Don't do these); a separate detail page — the 2026-07-08 proposal's `dmarc-detail-page` and its per-source pivot are withdrawn, the top lists and the report rows being the drill-down.

### ui.yaml surface

**PROPOSED 2026-10-09, pending rule-A sign-off — do not implement against these ids before they land in ui.yaml.** Page `admin-mail-reports` (heading `S.admin.mail_reports_page.title`); elements `page-heading`, `admin-nav-back`, `error-message`, then:

- `admin-mail-reports-refresh-button` (button — re-read)
- `admin-mail-reports-window-select` (select — 7 / 30 / 90 days)
- `admin-mail-reports-domain-select` (select — every domain / one local domain)
- `admin-mail-reports-summary` (view — the totals container)
- `admin-mail-reports-messages` (text — messages reported · reports · reporting peers)
- `admin-mail-reports-alignment` (text — DMARC pass · DKIM aligned · SPF aligned)
- `admin-mail-reports-dispositions` (text — none · quarantine · reject)
- `admin-mail-reports-truncated` (text — only while the page cap was hit)
- `admin-mail-reports-empty` (text — the empty state, only on the empty path)
- `admin-mail-reports-raw` (text — the opened report's XML; present after a row's view button)

Components, indexed: `admin-mail-reports-source` (leaves `-source-ip`, `-source-count`); `admin-mail-reports-sender` (`-sender-domain`, `-sender-count`); `admin-mail-reports-report` (`-report-submitter`, `-report-domain`, `-report-period`, `-report-messages`, `-report-pass`, `-report-view-button`). Twenty-four new ids and one new page. The 2026-07-08 proposal's `dmarc-summary-card-*` ids and `dmarc-detail-page` are withdrawn.

### Where logic lives

Shared Rust; the apps render. `fauna_mail::dmarc_report::WindowSummary` — a pure fold over `&[DmarcAggregateReportEntry]`: the sums, the distinct-submitter count, the merged top lists — and a `DmarcReportsMachine` in `fauna-client-mail-settings` (the `MailPolicyMachine` / `ForwarderMachine` shape: a snapshot of window, domain, totals, report rows, the opened report and the error; actions `Refresh`, `SetWindow`, `SetDomain`, `OpenReport`), with the UniFFI and wasm twins the other six apps need; the raw view gunzips in shared Rust. Build order: rust → tui (the lead app) → the other six in one batched trickle-down; the feature page [`dmarc-reporting`](../../features/dmarc-reporting.md) gains an `[app]` outcome when the first app shows the reports.

### Live refresh — no push (decided 2026-10-09)

The page re-reads on open, on a filter change and on its refresh button, like every other admin page (`mail-observability.md` § Design pass 2026-09-19: live refresh stays a tier-2 proposal). The `BridgeDmarcReportIngested` push once proposed for the ingest path is **not built, by design**: reports arrive about once a day per peer, so a push would save the admin one refresh a day at the price of a new app-routed kind on every app. If a trigger ever makes live refresh owed, the kind is an additive `fauna.bridges.dmarc_report_ingested { our_domain, submitter, report_id }` to every admin actor's connections (the `cert_nudge` shape), marking the page stale — nothing in the storage or list shape changes.

---

## Wire shapes (named, not redefined here)

| RPC | Caller | Purpose | Notes |
|---|---|---|---|
| `fauna.bridges.ingest_dmarc_report` | MTA | accept a message mailed to `dmarc-report@` — an aggregate or a forensic report | DAG-CBOR `(raw_message, source_envelope, received_at)`; nest classifies (§ Forensic-report receive → *Envelope-time routing*), finds the report, validates + stores; replies the outcome (`stored`/`duplicate`/`malformed`/`oversize`/`wrong_domain`), every one a `250` on the wire, and the `kind` (`aggregate`/`forensic`) |
| `fauna.bridges.list_dmarc_forensic_reports` | admin client | list the forensic reports | `(window_days, our_domain?)` → entries (id, domain, submitter, received and arrival times, reported message id, the `parsed_summary_cbor` index) — never a recipient (§ Storage PII rule) |
| `fauna.bridges.get_dmarc_forensic_report` | admin client | one forensic report with its archive | `(id)` → the entry + `payload_raw_gz` |
| `fauna.bridges.list_dmarc_aggregate_reports` | admin client | summary query for the admin pane card | `(window_days, our_domain?)` → list of `parsed_summary_cbor` entries + counts, plus `oversize`: the dropped oversize reports' envelopes (client IP, HELO, MAIL FROM, auth verdicts, size, received-at; no body) |
| `fauna.bridges.get_dmarc_aggregate_report` | admin client | full-payload re-parse for the detail-page raw-XML view | `(our_domain, submitter, report_id)` — the storage key, as a list entry names it — → `payload_xml_gz` + decoded summary |
| `fauna.bridges.report_dmarc_observation` | MTA | count one verified inbound message toward the outbound aggregate report (§ Outbound aggregate-report sender, step 1) | DAG-CBOR `(client_ip, dmarc, observation)` — the message's DMARC verdict and the shared `DmarcObservation` (From and envelope domains, aligned DKIM/SPF, per-signature DKIM and SPF results); replies `{counted}`. Sent before any enforcement gate; best-effort on both ends |

Wire-level shape lives in the nest implementation track and the bridge-kind catalogue (`docs/goal/architecture/apps/bridges.md` § Bridge-kind catalogue — its home after the 2026-07-08 move out of storage-modes.md); this doc owns which RPCs exist and what each carries, not the byte-level encoding. The outbound reports themselves ride the standard outbound queue (`fetch_outbound_due` and its `mark_outbound_*` replies), so no report-sending kind exists — the `fetch_outbound_dmarc_due` / `dmarc_outbound_completed` pair an earlier draft listed was never built and is withdrawn. The ingest kind, the four admin reads and `report_dmarc_observation` are built (§ Implementation status today).

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
