<script lang="ts">
  // Admin unified-DNS page (`admin-dns`): the single domain-management surface.
  // Two shared machines render here, merged by domain name (priority #1/#2 —
  // lifts the linux shape, `apps/fauna-linux/src/views/admin.rs::render_admin_dns`):
  //   • `LocalDomainMachine` (`localDomainMachine`, WASM twin of the UniFFI
  //     mail-admin machine) — authoritative for the CRUD list: which domains are
  //     active (+ `is_primary`) and which are soft-deleted (30-day restore).
  //   • `DnsManagementMachine` (`dnsManagementMachineWithCredentials`) — the
  //     per-domain required-record matrix (each record's exact expected value + a
  //     live red/green public-DNS verdict) AND the managed-mode surface: the
  //     client-held DNS-provider credential store + per-domain Fauna-managed /
  //     manual mode. The credential is sealed into the admin's `fauna.state.dns`
  //     (BackupKey-sealed, nest-opaque); the browser publishes/verifies via the
  //     shared `fauna-provisioning` provider seam, routing cross-origin provider
  //     APIs through `fauna_provisioning::proxy`'s CORS proxy (the same path the
  //     onboarding wizard uses) — the nest never sees the provider key
  //     (dns-management.md § Where the credential lives).
  // Both are dumb-rendered: build over the singleton WS-RPC client → `hydrate()`
  // → render `snapshot()` → `dispatch(action)` → re-render. No DNS logic in the
  // SPA (priority #2). Behavior: docs/goal/behavior/dns-management.md
  // § The two modes / § Manual + live verification / § App surface. IDs:
  // tests/e2e-unified/ui.yaml § admin-dns. Prior art: the linux page above + the
  // onboarding DNS-credential form (`routes/onboarding/+page.svelte`).
  import { identity } from '$lib/store';
  import {
    dnsManagementMachineWithCredentials,
    localDomainMachine,
    adminUsersListAll,
    linkedNestsMachine,
  } from '$lib/rpc';
  import { toBytes, actorHex } from '$lib/hex';
  import {
    ensureWasm,
    dnsVerdictLabel,
    certStatusView,
    hexFull,
    roleAddressOptions,
    adminPickerOption,
    type RoleAddressOption,
  } from '$lib/wasm';
  import { graceCountdown } from '$lib/value-format';
  import { onMount } from 'svelte';
  import MessageBanner from '$lib/components/MessageBanner.svelte';
  import { t } from '$lib/i18n/strings';
  import { resolveKey, resolveLocalized } from '$lib/i18n/localized';
  import { PROVIDERS, type ProviderMeta } from '$lib/generated/providers';
  import { IDS } from '$lib/generated/uiIds';

  // Shapes mirror the shared machines' serde JSON (snake_case fields across the
  // serde_wasm_bindgen boundary). Typed locally over the `any` snapshots.
  interface RecordVerdict { observed: string[]; status: string; }
  interface DnsRecordRow {
    name: string;
    record_type: string;
    expected: string;
    ttl_seconds: number;
    verdict: RecordVerdict | null;
  }
  // `auto_renew` is the machine's projected effective state (managed||delegated ∧
  // not opted-out); the `admin-dns-domain-auto-renew` control renders only for
  // managed/delegated rows and reflects it.
  interface DomainView { domain: string; mode: string; auto_renew: boolean; records: DnsRecordRow[]; }
  interface CredentialSummary { provider_id: string; zones: string[]; label: string; }
  // The served-TLS-cert health badge per domain (`admin-dns-cert-status`,
  // tls-certificates.md § C.4). `state` is the serde enum string. A pure read —
  // populates on web too.
  interface CertStatusRow { domain: string; state: string; not_after_unix: number; is_floor: boolean; }
  // A one-time `_acme-challenge` CNAME renewal-delegation for a manual-mode domain
  // (§ B tier 3, S6b). `cname` is the same DnsRecordRow shape every record uses.
  interface DelegationView { domain: string; cname: DnsRecordRow; }
  // A suspended manual-mode DNS-01 order awaiting the admin's paste. Always null on
  // web (the order core is native-only); kept for the uniform shape with Linux.
  interface PendingCertIssue { domain: string; challenges: DnsRecordRow[]; }
  interface DnsSnapshot {
    domains: DomainView[];
    credentials: CredentialSummary[];
    status: string;
    error: string | null;
    cert_statuses: CertStatusRow[];
    delegations: DelegationView[];
    pending_cert: PendingCertIssue | null;
  }
  // `catch_all_actor_id` is the per-domain catch-all designation (`Option<Vec<u8>>`
  // on the shared `LocalDomainView`; serializes to `null | number[]` across the
  // wasm boundary). The `admin-dns-domain-catch-all-select` picker reads/writes it.
  // `role_address_overrides` carries one entry per overridable role that has an
  // override (`role` is the PascalCase `RoleAddressKind`; absent roles fall back to
  // the deployment admin); the four `admin-dns-domain-role-address-<role>-select`
  // pickers read/write it via `SetRoleAddress`.
  interface RoleAddressOverrideView {
    role: 'Postmaster' | 'Abuse' | 'Noc' | 'Security';
    actor_id: number[] | Uint8Array;
  }
  interface LocalDomainView {
    // 16-byte opaque domain_id; the rename picker sends the target's id as
    // `StartPrimaryRename.new_primary_domain_id` (the RPC is id-keyed).
    domain_id: number[] | Uint8Array;
    domain: string;
    is_primary: boolean;
    catch_all_actor_id: number[] | Uint8Array | null;
    // Epoch-millis a SUCCESSION (not an admin) last cleared `catch_all_actor_id`
    // because it named a retired identity; `null` when the catch-all
    // was never set, or was last set/cleared by an admin.
    catch_all_cleared_by_succession_at: number | null;
    role_address_overrides: RoleAddressOverrideView[];
  }
  // One deployment actor offered in the catch-all picker, sourced from
  // `fauna.admin.users.list` (`admin.md` § Users). `hex` keys the current-
  // designation match; `id` is dispatched as `SetCatchAllActor.actor_id`.
  interface ActorOption { id: Uint8Array; label: string; hex: string; }
  // The single in-flight primary-domain rename, projected by the shared
  // `PrimaryDomainRenameView` (mail-primary-domain-rename.md § UX surface). Domain
  // names are pre-resolved; epochs are millis; the `can_*` flags gate the banner's
  // action buttons (the nest owns validation — the client only shows/hides).
  interface PrimaryDomainRenameView {
    rename_id: number[] | Uint8Array;
    state: string;
    old_primary_domain: string;
    new_primary_domain: string;
    started_at: number;
    grace_days: number;
    grace_ends_at: number | null;
    ready_to_complete_at: number | null;
    is_post_flip_active: boolean;
    is_pre_flip: boolean;
    can_complete: boolean;
    can_force_complete: boolean;
    can_extend: boolean;
    can_abort: boolean;
  }
  interface LocalDomainsSnapshot {
    active: LocalDomainView[];
    soft_deleted: LocalDomainView[];
    status: string;
    error: string | null;
    // The in-flight rename (null when none), + the "Rename primary" enable hint.
    active_rename: PrimaryDomainRenameView | null;
    rename_available: boolean;
    // `true` when `active` is empty — the next AddDomain would be the first,
    // becoming the primary and unremovable (`deployment-home-with-public-relay.md`
    // § MUA reach). A UX hint only, mirroring the nest's own `is_primary`
    // derivation; the nest stays authority on whether an add is actually first.
    adding_first_domain: boolean;
  }

  // The AddDomain wizard pick defaults to the shared `DEFAULT_CERT_MODE`
  // (libs/fauna-client-mail-settings/src/local_domains.rs). The MTA-STS policy
  // mode is not sent: the nest sets and advances it.
  const DEFAULT_CERT_MODE = 'expand_primary';

  // DNS-capable providers for the add-credential picker (same source the
  // onboarding DNS step uses; linux filters `PROVIDERS` by `Capability::Dns`).
  const dnsProviders: ProviderMeta[] = PROVIDERS.filter((p) => p.capabilities.includes('dns'));

  // Machine handles, bound once in onMount and reused for every dispatch.
  // `$state` so the `allManaged` derived (which reads `dnsMachine`) recomputes
  // once the handle is bound, and so svelte sees the reassignment as reactive.
  let dnsMachine = $state<Awaited<ReturnType<typeof dnsManagementMachineWithCredentials>> | null>(null);
  let ldMachine = $state<Awaited<ReturnType<typeof localDomainMachine>> | null>(null);

  // `active`/`removed` come from the LocalDomainMachine (authoritative CRUD list);
  // `dnsByName` overlays the DNS record matrix per domain; `credentials` is the
  // held DNS-provider credential list (from the DnsManagementMachine snapshot).
  let active = $state<LocalDomainView[]>([]);
  let removed = $state<LocalDomainView[]>([]);
  // Deployment actors offered by the per-domain catch-all picker. Fetched once on
  // mount (first page) — the same `fauna.admin.users.list` the admin-users hub
  // reads. Empty until it returns; the picker still renders ("None" + any current
  // designation kept visible), mirroring linux's render-then-AdminUsersLoaded flow.
  let actors = $state<ActorOption[]>([]);
  let dnsByName = $state<Record<string, DomainView>>({});
  let credentials = $state<CredentialSummary[]>([]);
  // Cert lifecycle overlays, keyed by domain: the served-cert health badge
  // (`RefreshCertStatus` → `cert_statuses`) and the CNAME renewal-delegations
  // (`delegations`). `pendingCert` is the suspended manual-mode order — it now
  // populates on web too (the wasm-safe `acme_pure` order core landed — tls-certificates.md § Implementation status today), so a manual domain's
  // BeginManualIssueCert surfaces its `_acme-challenge` paste rows here. Per-domain
  // delegate-form open state + the selected target zone (first held-credential zone).
  let certByName = $state<Record<string, CertStatusRow>>({});
  let delegByName = $state<Record<string, DelegationView>>({});
  let pendingCert = $state<PendingCertIssue | null>(null);
  // The 32-byte id the connection is bound to — the `target_nest_id` the cert-issuance
  // dispatch seals to (resolved once on mount via `linkedNestsMachine.thisNestId()`,
  // the wasm twin of linux `resolve_this_nest_id`). Null until resolved → issuance
  // buttons stay inert (mirrors not having a paired nest to seal to).
  let targetNestId = $state<number[] | null>(null);
  let delegateOpen = $state<Record<string, boolean>>({});
  let delegateZone = $state<Record<string, string>>({});
  let error = $state('');
  let loading = $state(true);
  let addFormOpen = $state(false);
  let addInput = $state('');
  // Primary-domain rename (mail-primary-domain-rename.md § UX surface). The state
  // rides the LocalDomainMachine snapshot: `renameActive` is the in-flight rename
  // (drives the banner + per-row state), `renameAvailable` gates the "Rename
  // primary" button (two-step rule). The sheet + reveal-confirm flags are local UI.
  let renameActive = $state<PrimaryDomainRenameView | null>(null);
  let renameAvailable = $state(false);
  // The add-domain form's irreversibility warning (deployment-home-with-public-
  // relay.md § MUA reach) — true only while a domainless nest's next add would
  // become the primary.
  let addingFirstDomain = $state(false);
  let renameSheetOpen = $state(false);
  let renameTarget = $state(''); // the picked new-primary domain name
  let renameGraceDays = $state(''); // optional override; '' → nest default (7)
  let confirmingComplete = $state(false);
  let confirmingAbort = $state(false);
  let extendDays = $state('7');
  // Add-credential form state: revealed on demand; a provider is picked, its
  // DNS fields rendered, then verify+store on submit (write-only — the stored
  // secret is never read back, only the provider + zones + label render).
  let addCredOpen = $state(false);
  let credProviderId = $state<string | null>(null);
  let credValues = $state<Record<string, string>>({});

  const selectedCredProvider = $derived(
    dnsProviders.find((p) => p.id === credProviderId) ?? null,
  );
  // The selected provider's DNS-credential fields (the entries the form renders).
  const credFields = $derived(
    selectedCredProvider?.fields.filter((f) => f.kinds.includes('dns')) ?? [],
  );
  // The deployment master switch reflects "every active domain is Fauna-managed"
  // (dns-management.md § The two modes). The projection itself lives in shared
  // Rust — `DnsSnapshot::all_domains_managed`, exposed as
  // `WasmDnsManagementMachine.allDomainsManaged` (lifted) — so the
  // `mode == "managed"` fold isn't re-coded per client. We just thread in the
  // active-domain names; `active` is reassigned on every `applySnapshots`, so
  // this re-derives whenever either snapshot changes.
  const allManaged = $derived(
    dnsMachine?.allDomainsManaged(active.map((d) => d.domain)) ?? false,
  );

  // The held credentials' covered zones — the `admin-dns-cert-delegate-zone-select`
  // options (a manual domain's `_acme-challenge` renewals re-home into one of these).
  const credZones = $derived(Array.from(new Set(credentials.flatMap((c) => c.zones))));

  // Resolve a dot-notation i18n key (provider display names + field labels) —
  // the shared `$lib/i18n/localized` resolver (was duplicated per-page).
  const L = resolveKey;

  // A missing verdict reads as "checking" (neutral) — never a false red/green.
  // The verdict→key decision is shared Rust (`fauna_core::format::dns_verdict_label`
  // over wasm) the native apps consume via UniFFI; only the CSS class below stays
  // web-local. See value-formatting.md § DNS verdict label.
  function statusLabel(verdict: RecordVerdict | null | undefined): string {
    return resolveLocalized(dnsVerdictLabel(verdict?.status ?? '', verdict?.observed ?? []));
  }
  function statusClass(status: string | undefined): string {
    switch (status) {
      case 'Ok': return 'ok';
      case 'Missing':
      case 'Mismatch': return 'bad';
      default: return 'checking';
    }
  }

  // Re-read both snapshots into render state. Error prefers the local-domains
  // CRUD feedback (e.g. cannot_remove_primary_domain), then the DNS list/verify/
  // managed-mode error — mirrors linux `render_admin_dns`.
  function applySnapshots(): void {
    const dns = (dnsMachine?.snapshot() ?? null) as DnsSnapshot | null;
    const ld = (ldMachine?.snapshot() ?? null) as LocalDomainsSnapshot | null;
    active = ld?.active ?? [];
    removed = ld?.soft_deleted ?? [];
    renameActive = ld?.active_rename ?? null;
    renameAvailable = ld?.rename_available ?? false;
    addingFirstDomain = ld?.adding_first_domain ?? false;
    dnsByName = Object.fromEntries((dns?.domains ?? []).map((d) => [d.domain, d]));
    credentials = dns?.credentials ?? [];
    certByName = Object.fromEntries((dns?.cert_statuses ?? []).map((c) => [c.domain, c]));
    delegByName = Object.fromEntries((dns?.delegations ?? []).map((g) => [g.domain, g]));
    pendingCert = dns?.pending_cert ?? null;
    error = ld?.error ?? dns?.error ?? '';
  }

  // Re-fetch the DNS record matrix + held credentials + overlay live verdicts
  // (`admin-dns-refresh-button`; also run after a domain/credential change so the
  // new state appears). Refresh reloads the credential store too (so the list +
  // effective-mode projection are current).
  async function refreshDnsMatrix(): Promise<void> {
    if (!dnsMachine) return;
    await dnsMachine.dispatch('Refresh');
    applySnapshots();
    await dnsMachine.dispatch({ VerifyRecords: { domain: null } });
    applySnapshots();
    // Overlay the nest's served-cert health (RefreshCertStatus reads the domain
    // names from the just-refreshed matrix). A pure Admin read — runs on web.
    await dnsMachine.dispatch('RefreshCertStatus');
    applySnapshots();
  }

  onMount(async () => {
    const id = $identity;
    if (!id?.secretHex) { loading = false; return; }
    try {
      await ensureWasm();
      ROLE_ADDRESS_ROLES = roleAddressOptions();
      dnsMachine = await dnsManagementMachineWithCredentials(id.secretHex);
      ldMachine = await localDomainMachine(id.secretHex);
      // Both fetches fire on load; the CRUD list + record matrix are independent.
      await Promise.all([ldMachine.hydrate(), dnsMachine.hydrate()]);
      applySnapshots();
      // Overlay live public-DNS verdicts onto the rendered matrix.
      await dnsMachine.dispatch({ VerifyRecords: { domain: null } });
      applySnapshots();
      // Overlay the served-cert health badge (pure read; renders on web too).
      await dnsMachine.dispatch('RefreshCertStatus');
      applySnapshots();
      // The deployment actors offered by the per-domain catch-all and
      // role-address pickers: every account on the nest, never one page
      // (admin.md § 2 → *Which accounts a picker offers*). A failure here must
      // not break the rest of the page — the picker degrades to "None" + any
      // current designation kept visible.
      try {
        const users = await adminUsersListAll(id.secretHex);
        // Option text is `adminPickerOption` (the two-halves rule's display
        // half): the handle, else full hex, never the raw editable label —
        // shared by both the catch-all and role-address pickers below, which
        // both read this one `actors` array.
        actors = users.map((u) => ({
          id: toBytes(u.actor_id),
          label: adminPickerOption(u),
          hex: actorHex(u.actor_id),
        }));
      } catch {
        // Non-fatal: the catch-all picker just can't offer new actors yet.
      }
      // Resolve the connected nest's own id (the cert-issuance seal target). A
      // failure here must not break the page — issuance buttons just stay inert
      // until it resolves (the rest of admin-dns is read/config-only).
      try {
        const ln = await linkedNestsMachine(id.secretHex);
        targetNestId = (await ln.thisNestId()) as number[];
      } catch {
        // Non-fatal: issuance unavailable until the self-nest id resolves.
      }
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    } finally {
      loading = false;
    }
  });

  function openAddForm(): void {
    addFormOpen = true;
    addInput = '';
  }
  function cancelAdd(): void {
    addFormOpen = false;
    addInput = '';
  }
  async function submitAdd(): Promise<void> {
    const domain = addInput.trim().toLowerCase();
    if (!domain || !ldMachine) return;
    addFormOpen = false;
    addInput = '';
    try {
      await ldMachine.dispatch({
        AddDomain: {
          domain,
          mta_sts_cert_mode: DEFAULT_CERT_MODE,
        },
      });
      applySnapshots();
      await refreshDnsMatrix();
    } catch (e) {
      // The machine also records the error on its snapshot; prefer that.
      applySnapshots();
      if (!error) error = e instanceof Error ? e.message : String(e);
    }
  }
  async function removeDomain(domain: string): Promise<void> {
    if (!ldMachine) return;
    try {
      await ldMachine.dispatch({ RemoveDomain: { domain } });
      applySnapshots();
    } catch (e) {
      applySnapshots();
      if (!error) error = e instanceof Error ? e.message : String(e);
    }
  }
  async function restoreDomain(domain: string): Promise<void> {
    if (!ldMachine) return;
    try {
      await ldMachine.dispatch({ RestoreDomain: { domain } });
      applySnapshots();
      await refreshDnsMatrix();
    } catch (e) {
      applySnapshots();
      if (!error) error = e instanceof Error ? e.message : String(e);
    }
  }

  // ── Primary-domain rename (mail-primary-domain-rename.md § UX surface) ──
  // The nest owns all validation (single-active-rename / new-primary-is-additional
  // / cert-mode / TLS-posture / SAN-cap / grace-not-expired); every dispatch
  // surfaces a refusal via the machine's snapshot error. The client only picks the
  // target + the grace override and reads back the projected state.
  function openRenameSheet(target: string): void {
    // `target` is '' when opened from the primary row's "Rename primary" (the
    // admin picks in the select); a domain name when opened via a row's "Promote".
    renameTarget = target || (active.find((d) => !d.is_primary)?.domain ?? '');
    renameGraceDays = '';
    renameSheetOpen = true;
  }
  function cancelRename(): void {
    renameSheetOpen = false;
    renameTarget = '';
    renameGraceDays = '';
  }
  async function submitRename(): Promise<void> {
    if (!ldMachine) return;
    const target = active.find((d) => d.domain === renameTarget && !d.is_primary);
    if (!target) return;
    const days = renameGraceDays.trim() === '' ? null : Number(renameGraceDays);
    renameSheetOpen = false;
    try {
      await ldMachine.dispatch({
        StartPrimaryRename: {
          new_primary_domain_id: Array.from(toBytes(target.domain_id)),
          grace_days: days,
        },
      });
      applySnapshots();
      await refreshDnsMatrix();
    } catch (e) {
      applySnapshots();
      if (!error) error = e instanceof Error ? e.message : String(e);
    }
  }
  async function completeRename(): Promise<void> {
    if (!ldMachine || !renameActive) return;
    // `force` iff the grace window has NOT yet elapsed (can_force_complete);
    // otherwise it's a plain complete (can_complete).
    const force = renameActive.can_force_complete;
    const rename_id = Array.from(toBytes(renameActive.rename_id));
    confirmingComplete = false;
    try {
      await ldMachine.dispatch({ CompletePrimaryRename: { rename_id, force } });
      applySnapshots();
      await refreshDnsMatrix();
    } catch (e) {
      applySnapshots();
      if (!error) error = e instanceof Error ? e.message : String(e);
    }
  }
  async function extendRename(): Promise<void> {
    if (!ldMachine || !renameActive) return;
    const n = Number(extendDays);
    if (!Number.isFinite(n) || n < 1) return;
    const rename_id = Array.from(toBytes(renameActive.rename_id));
    try {
      await ldMachine.dispatch({ ExtendPrimaryRenameGrace: { rename_id, additional_days: n } });
      applySnapshots();
    } catch (e) {
      applySnapshots();
      if (!error) error = e instanceof Error ? e.message : String(e);
    }
  }
  async function abortRename(): Promise<void> {
    if (!ldMachine || !renameActive) return;
    const rename_id = Array.from(toBytes(renameActive.rename_id));
    confirmingAbort = false;
    try {
      await ldMachine.dispatch({ AbortPrimaryRename: { rename_id, reason: null } });
      applySnapshots();
      await refreshDnsMatrix();
    } catch (e) {
      applySnapshots();
      if (!error) error = e instanceof Error ? e.message : String(e);
    }
  }
  // Client-rendered countdown to `grace_ends_at` (mail-primary-domain-rename.md —
  // the state is nest-authoritative; only the *display* is client-side). Routes
  // through the shared `fauna_core::format::grace_countdown` (value-formatting.md
  // § Grace countdown); `null` means past the deadline, render the elapsed label.
  function graceRemaining(endsAtMs: number): string {
    return graceCountdown(endsAtMs) ?? t.admin.dns.rename.grace_elapsed;
  }

  // ── Per-domain catch-all designation (`admin-dns-domain-catch-all-select`) ──
  // mail-aliases.md § Kind 4 / mail-multidomain.md § Per-domain catch-all: a
  // 1-per-domain admin-tier setting — designate one actor as the domain's
  // catch-all (unmatched inbound routes to it) or "None" to clear. Lifts the
  // linux `build_domain_section` picker (apps/fauna-linux/src/views/admin.rs).

  // Build the per-domain picker model: option 0 = "None" (clears), option i = an
  // actor, plus a trailing "actor xxxx…" entry if the current designation isn't
  // among the loaded actors (e.g. paginated out) so it stays visible + selected
  // rather than silently clearing. `ids[i]` is the parallel actor-id map (null at
  // 0) the onchange handler dispatches by `selectedIndex` — index-based like linux.
  // `selected` is the option `value` (`a.label`, which holds `adminPickerOption(u)`
  // — admin.md § 2's two-halves rule) the `<select>` shows as current.
  function buildCatchAll(d: LocalDomainView): {
    options: string[];
    ids: (Uint8Array | null)[];
    selected: string;
  } {
    const none = t.admin.dns.catch_all_none;
    const options: string[] = [none];
    const ids: (Uint8Array | null)[] = [null];
    for (const a of actors) {
      options.push(a.label);
      ids.push(a.id);
    }
    let selected: string = none;
    const cur = d.catch_all_actor_id ? toBytes(d.catch_all_actor_id) : null;
    if (cur && cur.length > 0) {
      const curHex = actorHex(cur);
      const match = actors.find((a) => a.hex === curHex);
      if (match) {
        selected = match.label;
      } else {
        const fallback = t.admin.actor_id_fallback_label({ short: hexFull(cur) });
        options.push(fallback);
        ids.push(cur);
        selected = fallback;
      }
    }
    return { options, ids, selected };
  }

  // Designate (or clear, when `actorId` is null) a domain's catch-all actor.
  // Config-only Admin write → runs on web. The machine re-reads the row so the
  // snapshot's `catch_all_actor_id` reflects the change; we re-render from it.
  async function setCatchAll(domain: string, actorId: Uint8Array | null): Promise<void> {
    if (!ldMachine) return;
    try {
      await ldMachine.dispatch({
        SetCatchAllActor: { domain, actor_id: actorId ? Array.from(actorId) : null },
      });
      applySnapshots();
    } catch (e) {
      applySnapshots();
      if (!error) error = e instanceof Error ? e.message : String(e);
    }
  }

  // ── Per-domain role-address overrides (`admin-dns-domain-role-address-*`) ──
  // mail-multidomain.md § Per-domain role-address routing: the admin can redirect a
  // domain's overridable RFC 2142 role addresses — postmaster@/abuse@/noc@/security@
  // — to a chosen actor (e.g. delegate abuse@ to a community moderator), else they
  // fall back to the deployment admin. The same per-row picker as catch-all, ×4 (one
  // per role); the nest atomic-merges so each role is independent. Lifts the linux
  // `build_domain_section` picker (apps/fauna-linux/src/views/admin.rs).
  //
  // `key` is the storage key / testid suffix / `<key>@` caption; `kind` is the
  // `RoleAddressKind` tag the view's `role_address_overrides[].role` carries and
  // the `SetRoleAddress` action takes. Both halves come from the shared
  // `role_address_options()` (over `RoleAddressKind::ALL` + `as_storage_key`) —
  // this page used to hand-write the four-arm table, which is the copy the
  // cross-language sweep found alongside apple's, android's and windows'.
  //
  // Filled in `onMount` after `ensureWasm()` rather than at script time,
  // because reading it needs the module loaded. Empty until then is not a gap:
  // the pickers render inside the per-domain block, which itself has nothing to
  // draw until `ldMachine.hydrate()` — strictly later.
  let ROLE_ADDRESS_ROLES = $state<RoleAddressOption[]>([]);

  // Build a per-(domain, role) picker model: option 0 = "Admin (default)" (clears
  // the override → the role falls back to the deployment admin), option i = an actor,
  // plus a trailing "actor xxxx…" entry if the current override isn't among the
  // loaded actors (paginated out) so it stays visible + selected. `ids[i]` is the
  // parallel actor-id map (null at 0) the onchange handler dispatches by
  // `selectedIndex` — index-based like linux. Mirrors `buildCatchAll`; the only
  // difference is option 0's meaning ("Admin (default)" not "None") and the current
  // selection reading off `role_address_overrides` (the role's PascalCase `kind`).
  function buildRoleAddress(d: LocalDomainView, kind: string): {
    options: string[];
    ids: (Uint8Array | null)[];
    selected: string;
  } {
    const dflt = t.admin.dns.role_address_admin_default;
    const options: string[] = [dflt];
    const ids: (Uint8Array | null)[] = [null];
    for (const a of actors) {
      options.push(a.label);
      ids.push(a.id);
    }
    let selected: string = dflt;
    const ovr = (d.role_address_overrides ?? []).find((o) => o.role === kind);
    const cur = ovr ? toBytes(ovr.actor_id) : null;
    if (cur && cur.length > 0) {
      const curHex = actorHex(cur);
      const match = actors.find((a) => a.hex === curHex);
      if (match) {
        selected = match.label;
      } else {
        const fallback = t.admin.actor_id_fallback_label({ short: hexFull(cur) });
        options.push(fallback);
        ids.push(cur);
        selected = fallback;
      }
    }
    return { options, ids, selected };
  }

  // Designate (or clear, when `actorId` is null → "Admin (default)") the override
  // actor for one (domain, role). Config-only Admin write → runs on web. The nest
  // atomic-merges so setting one role preserves the others; the machine re-reads the
  // row so the snapshot's `role_address_overrides` reflects the change.
  async function setRoleAddress(
    domain: string,
    kind: string,
    actorId: Uint8Array | null,
  ): Promise<void> {
    if (!ldMachine) return;
    try {
      await ldMachine.dispatch({
        SetRoleAddress: { domain, role: kind, actor_id: actorId ? Array.from(actorId) : null },
      });
      applySnapshots();
    } catch (e) {
      applySnapshots();
      if (!error) error = e instanceof Error ? e.message : String(e);
    }
  }

  // ── Managed mode: credentials + per-domain mode ──────────────────────

  function openAddCred(): void {
    addCredOpen = true;
    credProviderId = null;
    credValues = {};
  }
  function cancelAddCred(): void {
    addCredOpen = false;
    credProviderId = null;
    credValues = {};
  }
  function selectCredProvider(id: string): void {
    credProviderId = id;
    credValues = {};
  }
  // Verify + store a DNS-provider credential (write-only): dispatch PutCredentials
  // → the machine verifies against the provider API (via the CORS proxy on web)
  // and, on success, seals it into DnsConfig. A verify failure surfaces in
  // error-message and stores nothing (dns-management.md § The two modes).
  async function submitAddCred(): Promise<void> {
    if (!dnsMachine || !selectedCredProvider) return;
    const provider_id = selectedCredProvider.id;
    const fields = credFields.map((f) => ({ id: f.id, value: credValues[f.id] ?? '' }));
    const label = L(selectedCredProvider.displayNameKey);
    try {
      await dnsMachine.dispatch({ PutCredentials: { provider_id, fields, label } });
      addCredOpen = false;
      credProviderId = null;
      credValues = {};
      await refreshDnsMatrix();
    } catch (e) {
      // The machine records the provider-verify rejection on its snapshot.error;
      // prefer that (it carries the provider's own message).
      applySnapshots();
      if (!error) error = e instanceof Error ? e.message : String(e);
    }
  }
  async function clearCredential(index: number): Promise<void> {
    if (!dnsMachine) return;
    try {
      await dnsMachine.dispatch({ ClearCredentials: { index } });
      // Domains that lose coverage re-render manual — re-fetch the matrix.
      await refreshDnsMatrix();
    } catch (e) {
      applySnapshots();
      if (!error) error = e instanceof Error ? e.message : String(e);
    }
  }
  // Opt a domain in/out of Fauna-managed DNS. Opting in requires a held
  // credential whose zones cover the domain; the machine rejects otherwise
  // (InvalidState → error-message) and the next render snaps the mode back. A
  // successful opt-in publishes the records inside the same SetMode.
  async function setMode(domain: string, managed: boolean): Promise<void> {
    if (!dnsMachine) return;
    try {
      await dnsMachine.dispatch({ SetMode: { domain, managed } });
      await refreshDnsMatrix();
    } catch (e) {
      applySnapshots();
      if (!error) error = e instanceof Error ? e.message : String(e);
    }
  }
  // Deployment master switch: set every active domain's mode at once. Opting in
  // without covering credentials rejects per-domain (surfaced via error-message);
  // we stop on the first error so the admin sees why.
  async function toggleManageAll(): Promise<void> {
    if (!dnsMachine) return;
    const target = !allManaged;
    try {
      for (const d of active) {
        await dnsMachine.dispatch({ SetMode: { domain: d.domain, managed: target } });
      }
      await refreshDnsMatrix();
    } catch (e) {
      await refreshDnsMatrix();
      if (!error) error = e instanceof Error ? e.message : String(e);
    }
  }

  async function copyValue(value: string): Promise<void> {
    try {
      await navigator.clipboard.writeText(value);
    } catch {
      // Clipboard may be unavailable (no permission / headless) — non-fatal.
    }
  }

  // ── Cert lifecycle (tls-certificates.md § C.4 / § B tier 3) ──────────
  // Lifts the linux cert badge + delegation + auto-renew shapes
  // (`apps/fauna-linux/src/views/admin.rs`). All read/config — runs on web.

  // The served-cert health badge text (mirrors linux `cert_status_text`): the
  // nest-reported state of the cert the listener actually serves, with the
  // self-signed sub-label on the floor and the expiry date for a trusted cert.
  // `undefined` until RefreshCertStatus returns → "checking".
  function certStatusText(cert: CertStatusRow | undefined): string {
    const label = t.admin.dns.cert.label;
    if (!cert) return `${label} ${t.admin.dns.status_checking}`;
    // The badge decision is shared Rust (`fauna_core::format::cert_status_view` over
    // wasm) the native apps consume via UniFFI: the state word AND which of the two
    // mutually-exclusive sub-labels follows it — a floor cert's own far-future expiry
    // is withheld. Only the assembly below is web-local. value-formatting.md
    // § Cert status badge.
    const view = certStatusView(cert.state, cert.is_floor, cert.not_after_unix);
    let s = `${label} ${resolveLocalized(view.state)}`;
    if (view.show_self_signed) {
      s += ` (${t.admin.dns.cert.self_signed})`;
    } else if (view.expires_at_unix != null) {
      s += ` — ${t.admin.dns.cert.expires({ date: formatCertExpiry(view.expires_at_unix) })}`;
    }
    return s;
  }
  // success (trusted) / warning (expiring / on-floor — non-fatal) / dim (loading).
  function certStatusClass(cert: CertStatusRow | undefined): string {
    switch (cert?.state) {
      case 'ValidTrusted': return 'ok';
      case 'Expiring':
      case 'OnFloorRenewNeeded': return 'warn';
      default: return 'checking';
    }
  }
  // A cert `notAfter` (unix seconds) as a local YYYY-MM-DD (mirrors linux
  // `format_cert_expiry`); falls back to the raw seconds on an invalid date.
  function formatCertExpiry(unixSecs: number): string {
    const d = new Date(unixSecs * 1000);
    if (Number.isNaN(d.getTime())) return String(unixSecs);
    const y = d.getFullYear();
    const m = String(d.getMonth() + 1).padStart(2, '0');
    const day = String(d.getDate()).padStart(2, '0');
    return `${y}-${m}-${day}`;
  }

  // Get/renew a domain's TLS cert. The order core runs on web now (the wasm-safe
  // `acme_pure` driver — tls-certificates.md § Implementation status today), so this
  // mirrors linux `build_cert_issuance`: a **managed or CNAME-delegated** domain
  // issues in a single `IssueCert` (the held credential / delegation auto-publishes
  // the `_acme-challenge` TXT); a **manual** domain opens the two-phase
  // `BeginManualIssueCert` → surfaces the TXT on `pending_cert` for the admin to
  // paste, then `CompleteManualIssueCert`. `target_nest_id` is the seal target. The
  // issued cert is sealed to the nest over `fauna.tls.publish_cert`; RefreshCertStatus
  // re-reads the served-cert badge afterwards.
  async function issueCert(domain: string, single: boolean): Promise<void> {
    if (!dnsMachine || !targetNestId) return;
    try {
      await dnsMachine.dispatch(
        single
          ? { IssueCert: { domain, target_nest_id: targetNestId } }
          : { BeginManualIssueCert: { domain, target_nest_id: targetNestId } },
      );
      applySnapshots();
      await dnsMachine.dispatch('RefreshCertStatus');
      applySnapshots();
    } catch (e) {
      applySnapshots();
      if (!error) error = e instanceof Error ? e.message : String(e);
    }
  }
  // Finalize a pending manual order once the admin has pasted the `_acme-challenge`
  // TXT and the record verified green (CompleteManualIssueCert resumes the held
  // order, finalizes, seals + delivers). Clears `pending_cert` on success.
  async function completeManualIssue(): Promise<void> {
    if (!dnsMachine) return;
    try {
      await dnsMachine.dispatch('CompleteManualIssueCert');
      applySnapshots();
      await dnsMachine.dispatch('RefreshCertStatus');
      applySnapshots();
    } catch (e) {
      applySnapshots();
      if (!error) error = e instanceof Error ? e.message : String(e);
    }
  }
  // Drop a suspended manual order (no CA work) and clear the paste surface.
  async function cancelManualIssue(): Promise<void> {
    if (!dnsMachine) return;
    try {
      await dnsMachine.dispatch('CancelManualIssueCert');
      applySnapshots();
    } catch (e) {
      applySnapshots();
      if (!error) error = e instanceof Error ? e.message : String(e);
    }
  }

  // Turn automatic certificate renewal on/off for a domain (config-only → runs on
  // web). The machine persists the opt-OUT and re-projects DomainView.auto_renew.
  async function setAutoRenew(domain: string, enabled: boolean): Promise<void> {
    if (!dnsMachine) return;
    try {
      await dnsMachine.dispatch({ SetAutoRenew: { domain, enabled } });
      applySnapshots();
    } catch (e) {
      applySnapshots();
      if (!error) error = e instanceof Error ? e.message : String(e);
    }
  }
  function openDelegateForm(domain: string): void {
    delegateOpen = { ...delegateOpen, [domain]: true };
    // Default the target zone to the first held-credential zone (the e2e submits
    // without picking one — DelegateRenewal needs a held credential covering it).
    delegateZone = { ...delegateZone, [domain]: credZones[0] ?? '' };
  }
  function cancelDelegateForm(domain: string): void {
    delegateOpen = { ...delegateOpen, [domain]: false };
  }
  // Delegate a manual domain's `_acme-challenge` renewals into a controlled zone
  // (config-only → runs on web). A held credential must cover the chosen zone; the
  // machine rejects otherwise (→ error-message) and the next render reverts.
  async function delegateRenewal(domain: string): Promise<void> {
    if (!dnsMachine) return;
    const target_zone = delegateZone[domain] ?? credZones[0] ?? '';
    try {
      await dnsMachine.dispatch({ DelegateRenewal: { domain, target_zone } });
      delegateOpen = { ...delegateOpen, [domain]: false };
      applySnapshots();
    } catch (e) {
      applySnapshots();
      if (!error) error = e instanceof Error ? e.message : String(e);
    }
  }
  async function removeDelegation(domain: string): Promise<void> {
    if (!dnsMachine) return;
    try {
      await dnsMachine.dispatch({ RemoveDelegation: { domain } });
      applySnapshots();
    } catch (e) {
      applySnapshots();
      if (!error) error = e instanceof Error ? e.message : String(e);
    }
  }
</script>

<h1 data-testid={IDS.PAGE_HEADING}>{t.admin.dns.title}</h1>
<p class="subtitle">{t.admin.dns.description}</p>

<MessageBanner bind:error />

<!-- Deployment-wide primary-domain-rename banner (mail-primary-domain-rename.md
     § UX surface). Shown while a rename is in flight; a dumb render of the shared
     snapshot's `active_rename`. The action buttons are gated on the projected
     `can_*` flags (the nest owns state advancement + validation). Complete/Abort
     are reveal-then-confirm; the confirm copy names the risk/cost. -->
{#if renameActive}
  <div class="rename-banner" data-testid={IDS.ADMIN_DNS_RENAME_BANNER}>
    <div class="rename-banner-head">
      <strong>{t.admin.dns.rename.banner_title}</strong>
      <span class="mono">{renameActive.old_primary_domain} → {renameActive.new_primary_domain}</span>
    </div>
    <div class="rename-banner-meta">
      <span>{t.admin.dns.rename.state_label} {renameActive.state}</span>
      {#if renameActive.is_post_flip_active && renameActive.grace_ends_at != null}
        <span>{t.admin.dns.rename.grace_ends} {graceRemaining(renameActive.grace_ends_at)}</span>
      {/if}
    </div>
    <div class="rename-banner-actions">
      {#if renameActive.can_complete || renameActive.can_force_complete}
        {#if confirmingComplete}
          {#if renameActive.can_force_complete}
            <span class="warn">{t.admin.dns.rename.complete_force_warning}</span>
          {/if}
          <button
            class="btn primary"
            data-testid={IDS.ADMIN_DNS_RENAME_COMPLETE_CONFIRM_BUTTON}
            onclick={completeRename}
          >{t.admin.dns.rename.complete_confirm}</button>
        {:else}
          <button
            class="btn"
            data-testid={IDS.ADMIN_DNS_RENAME_COMPLETE_BUTTON}
            onclick={() => (confirmingComplete = true)}
          >{t.admin.dns.rename.complete}</button>
        {/if}
      {/if}
      {#if renameActive.can_extend}
        <input
          class="add-input narrow"
          type="number"
          min="1"
          max="30"
          data-testid={IDS.ADMIN_DNS_RENAME_EXTEND_DAYS_INPUT}
          bind:value={extendDays}
        />
        <button
          class="btn"
          data-testid={IDS.ADMIN_DNS_RENAME_EXTEND_BUTTON}
          onclick={extendRename}
        >{t.admin.dns.rename.extend}</button>
      {/if}
      {#if renameActive.can_abort}
        {#if confirmingAbort}
          {#if !renameActive.is_pre_flip}
            <span class="warn">{t.admin.dns.rename.abort_postflip_warning}</span>
          {/if}
          <button
            class="btn danger"
            data-testid={IDS.ADMIN_DNS_RENAME_ABORT_CONFIRM_BUTTON}
            onclick={abortRename}
          >{t.admin.dns.rename.abort_confirm}</button>
        {:else}
          <button
            class="btn danger"
            data-testid={IDS.ADMIN_DNS_RENAME_ABORT_BUTTON}
            onclick={() => (confirmingAbort = true)}
          >{t.admin.dns.rename.abort}</button>
        {/if}
      {/if}
    </div>
  </div>
{/if}

<!-- Start-a-rename wizard sheet (single instance). Opened from a domain row's
     rename/promote button; picks the new primary from existing active non-primary
     domains (the two-step rule — the wizard never adds a domain) + an optional
     grace override, then dispatches StartPrimaryRename. -->
{#if renameSheetOpen}
  <div class="rename-sheet" data-testid={IDS.ADMIN_DNS_RENAME_SHEET}>
    <h2 class="rename-sheet-title">{t.admin.dns.rename.sheet_title}</h2>
    <label class="caption" for="rename-new-primary">{t.admin.dns.rename.new_primary_label}</label>
    <select
      id="rename-new-primary"
      class="add-input"
      data-testid={IDS.ADMIN_DNS_RENAME_NEW_PRIMARY_SELECT}
      bind:value={renameTarget}
    >
      {#each active.filter((d) => !d.is_primary) as d}
        <option value={d.domain}>{d.domain}</option>
      {/each}
    </select>
    <label class="caption" for="rename-grace">{t.admin.dns.rename.grace_days_label}</label>
    <input
      id="rename-grace"
      class="add-input"
      type="number"
      min="1"
      max="30"
      data-testid={IDS.ADMIN_DNS_RENAME_GRACE_DAYS_INPUT}
      bind:value={renameGraceDays}
    />
    <div class="rename-sheet-actions">
      <button
        class="btn primary"
        data-testid={IDS.ADMIN_DNS_RENAME_SUBMIT_BUTTON}
        disabled={!renameTarget}
        onclick={submitRename}
      >{t.admin.dns.rename.submit}</button>
      <button
        class="btn"
        data-testid={IDS.ADMIN_DNS_RENAME_CANCEL_BUTTON}
        onclick={cancelRename}
      >{t.admin.dns.rename.cancel}</button>
    </div>
  </div>
{/if}

<!-- One DNS-record card (`admin-dns-record`): name / type / value + red/green
     verdict + copy. Reused for every required record, the one-time delegation
     CNAME, and the manual-paste `_acme-challenge` challenges (one record type, one
     path — tls-certificates.md § "The `_acme-challenge` record"). -->
{#snippet recordCard(r: DnsRecordRow)}
  <div class="record" data-testid={IDS.ADMIN_DNS_RECORD}>
    <div class="record-row">
      <span class="caption">{t.admin.dns.field_name}</span>
      <span class="mono" data-testid={IDS.ADMIN_DNS_RECORD_NAME}>{r.name}</span>
    </div>
    <div class="record-row">
      <span class="caption">{t.admin.dns.field_type}</span>
      <span data-testid={IDS.ADMIN_DNS_RECORD_TYPE}>{r.record_type}</span>
    </div>
    <div class="record-row">
      <span class="caption">{t.admin.dns.field_value}</span>
      <span class="mono" data-testid={IDS.ADMIN_DNS_RECORD_VALUE}>{r.expected}</span>
    </div>
    <div class="record-row status-row">
      <span
        class="status {statusClass(r.verdict?.status)}"
        data-testid={IDS.ADMIN_DNS_RECORD_STATUS}
      >{statusLabel(r.verdict)}</span>
      <button
        class="btn"
        data-testid={IDS.ADMIN_DNS_RECORD_COPY_BUTTON}
        onclick={() => copyValue(r.expected)}
      >{t.admin.dns.copy}</button>
    </div>
    {#if r.record_type === 'PTR'}
      <!-- PTR can never be zone-published — reverse DNS is set at the IP owner
           (dns-management.md § Records covered) — so only this row gets the
           advisory instead of the normal paste-and-verify treatment. -->
      <div class="record-row">
        <span class="hint" data-testid={IDS.ADMIN_DNS_RECORD_PROVIDER_NOTE}>{t.admin.dns.ptr_provider_note}</span>
      </div>
    {/if}
  </div>
{/snippet}

<!-- Managed-mode: deployment master switch + client-held DNS-provider
     credential store. The credential never goes to the nest (sealed into
     fauna.state.dns); the browser verifies/publishes via the CORS proxy. -->
<div class="creds-section">
  <div class="creds-header">
    <h2 class="creds-title">{t.admin.dns.credentials_title}</h2>
    <button class="btn" data-testid={IDS.ADMIN_DNS_REFRESH_BUTTON} onclick={refreshDnsMatrix}>
      {t.admin.dns.refresh}
    </button>
  </div>

  <button
    class="btn"
    class:active={allManaged}
    data-testid={IDS.ADMIN_DNS_MANAGE_ALL_TOGGLE}
    onclick={toggleManageAll}
  >{t.admin.dns.manage_all}</button>

  <!-- Held credentials (indexed admin-dns-credential-item). Always present (the
       container carries the test id); empty until a credential is held. -->
  <div class="creds-list" data-testid={IDS.ADMIN_DNS_CREDENTIALS_LIST}>
    {#if credentials.length === 0}
      <p class="muted">{t.admin.dns.credentials_empty}</p>
    {:else}
      {#each credentials as cred, i}
        <div class="cred-item" data-testid={IDS.ADMIN_DNS_CREDENTIAL_ITEM}>
          <span class="cred-provider" data-testid={IDS.ADMIN_DNS_CREDENTIAL_ITEM_PROVIDER}>{cred.provider_id}</span>
          <span class="cred-zones muted" data-testid={IDS.ADMIN_DNS_CREDENTIAL_ITEM_ZONES}>
            {t.admin.dns.credential_zones}{cred.zones.length ? `: ${cred.zones.join(', ')}` : ''}
          </span>
          <button
            class="btn danger"
            data-testid={IDS.ADMIN_DNS_CREDENTIAL_ITEM_CLEAR_BUTTON}
            onclick={() => clearCredential(i)}
          >{t.admin.dns.remove}</button>
        </div>
      {/each}
    {/if}
  </div>

  <!-- Write-only add-credential form: reveal → pick provider → type fields →
       verify+store (PutCredentials). -->
  {#if !addCredOpen}
    <button class="btn" data-testid={IDS.ADMIN_DNS_ADD_CREDENTIAL_BUTTON} onclick={openAddCred}>
      {t.admin.dns.add_credential}
    </button>
  {:else}
    <div class="add-cred-form">
      <div class="provider-row" data-testid={IDS.ADMIN_DNS_ADD_CREDENTIAL_PROVIDER_ROW}>
        {#each dnsProviders as p}
          <button
            class="btn"
            class:selected={credProviderId === p.id}
            data-testid={`admin-dns-add-credential-provider-row[${p.id}]`}
            onclick={() => selectCredProvider(p.id)}
          >{L(p.displayNameKey)}</button>
        {/each}
      </div>

      {#if selectedCredProvider}
        <div class="fields" data-testid={IDS.ADMIN_DNS_ADD_CREDENTIAL_FORM}>
          {#each credFields as field (field.id)}
            <label class="field-group">
              <span class="field-label">{L(field.labelKey)}</span>
              <input
                class="add-input"
                type={field.type === 'secret' || field.type === 'hosted-auth' ? 'password' : 'text'}
                data-testid={field.id}
                value={credValues[field.id] ?? ''}
                oninput={(e) => { credValues = { ...credValues, [field.id]: (e.currentTarget as HTMLInputElement).value }; }}
              />
            </label>
          {/each}
        </div>
      {/if}

      <div class="form-actions">
        <button class="btn primary" data-testid={IDS.ADMIN_DNS_ADD_CREDENTIAL_SUBMIT_BUTTON} onclick={submitAddCred}>
          {t.admin.dns.add_credential_submit}
        </button>
        <button class="btn" data-testid={IDS.ADMIN_DNS_ADD_CREDENTIAL_CANCEL_BUTTON} onclick={cancelAddCred}>
          {t.common.cancel}
        </button>
      </div>
    </div>
  {/if}
</div>

<!-- Add-domain form: the button reveals an inline input + submit/cancel.
     Submit dispatches AddDomain on the shared LocalDomainMachine. -->
<div class="add-domain">
  {#if !addFormOpen}
    <button class="btn primary" data-testid={IDS.ADMIN_DNS_ADD_DOMAIN_BUTTON} onclick={openAddForm}>
      {t.admin.dns.add_domain}
    </button>
  {:else}
    {#if addingFirstDomain}
      <p class="warning">{t.admin.dns.add_domain_primary_warning}</p>
    {/if}
    <div class="add-form">
      <input
        class="add-input"
        data-testid={IDS.ADMIN_DNS_ADD_DOMAIN_INPUT}
        placeholder={t.admin.dns.add_domain_placeholder}
        bind:value={addInput}
        onkeydown={(e) => { if (e.key === 'Enter') submitAdd(); }}
      />
      <button class="btn primary" data-testid={IDS.ADMIN_DNS_ADD_DOMAIN_SUBMIT_BUTTON} onclick={submitAdd}>
        {t.admin.dns.add_domain_submit}
      </button>
      <button class="btn" data-testid={IDS.ADMIN_DNS_ADD_DOMAIN_CANCEL_BUTTON} onclick={cancelAdd}>
        {t.common.cancel}
      </button>
    </div>
  {/if}
</div>

{#if loading}
  <p class="muted">{t.admin.dns.empty}</p>
{:else if active.length === 0 && removed.length === 0}
  <p class="muted" data-testid={IDS.ADMIN_DNS_EMPTY}>{t.admin.dns.empty_desc}</p>
{:else}
  <div class="domain-list">
    {#each active as d}
      {@const view = dnsByName[d.domain]}
      <!-- Per-domain managed verdict via the shared projection (no hand-coded
           `mode == "managed"`): a single-element active set reduces
           `allDomainsManaged` to "is this one domain managed". -->
      {@const isManaged = dnsMachine?.allDomainsManaged([d.domain]) ?? false}
      {@const deleg = delegByName[d.domain]}
      {@const cert = certByName[d.domain]}
      <!-- The auto-renew control shows only where a client can auto-issue —
           managed or CNAME-delegated (tls-certificates.md § C.3). -->
      {@const showAutoRenew = isManaged || !!deleg}
      {@const pendingHere = pendingCert?.domain === d.domain}
      <!-- Per-domain catch-all picker model (options + parallel actor-id map +
           current selection); dispatched by `selectedIndex` like linux. -->
      {@const ca = buildCatchAll(d)}
      <div class="domain-card" data-testid={IDS.ADMIN_DNS_DOMAIN}>
        <div class="domain-header">
          <span class="domain-name" data-testid={IDS.ADMIN_DNS_DOMAIN_NAME}>{d.domain}</span>
          <!-- Per-domain Fauna-managed / manual toggle. Active ⟺ the effective-
               mode projection is "managed"; clicking dispatches SetMode. -->
          <button
            class="btn mode"
            class:active={isManaged}
            data-testid={IDS.ADMIN_DNS_DOMAIN_MODE}
            onclick={() => setMode(d.domain, !isManaged)}
          >{isManaged ? t.admin.dns.mode_managed : t.admin.dns.mode_manual}</button>
          <!-- Default-on auto-renew toggle (managed/delegated rows only). The
               label is the constant "Auto-renew", so the checked state rides the
               `state` attr (the e2e reads it); clicking dispatches SetAutoRenew. -->
          {#if showAutoRenew}
            <button
              class="btn"
              class:active={view?.auto_renew}
              data-testid={IDS.ADMIN_DNS_DOMAIN_AUTO_RENEW}
              data-state={view?.auto_renew ? 'on' : 'off'}
              onclick={() => setAutoRenew(d.domain, !view?.auto_renew)}
            >{t.admin.dns.cert.auto_renew}</button>
          {/if}
          {#if d.is_primary}
            <span class="badge" data-testid={IDS.ADMIN_DNS_DOMAIN_PRIMARY_BADGE}>{t.admin.dns.primary_badge}</span>
          {/if}
          <!-- Present on every active row (index aligns with admin-dns-domain-name
               for scoped e2e clicks); disabled on the primary, which cannot be
               removed (the nest refuses with cannot_remove_primary_domain). -->
          <button
            class="btn danger"
            data-testid={IDS.ADMIN_DNS_DOMAIN_REMOVE_BUTTON}
            disabled={d.is_primary}
            onclick={() => removeDomain(d.domain)}
          >{t.admin.dns.remove}</button>
          <!-- Primary-domain rename affordances (mail-primary-domain-rename.md
               § UX surface). On the primary row: "Rename primary" (disabled until a
               non-primary exists — the two-step rule) + the in-flight state. On a
               non-primary row: "Promote to primary" (hidden while a rename runs). -->
          {#if d.is_primary}
            <button
              class="btn"
              data-testid={IDS.ADMIN_DNS_DOMAIN_RENAME_BUTTON}
              disabled={!renameAvailable || !!renameActive}
              onclick={() => openRenameSheet('')}
            >{t.admin.dns.rename.button}</button>
            {#if renameActive}
              <span class="badge" data-testid={IDS.ADMIN_DNS_DOMAIN_RENAME_STATE}
                >{t.admin.dns.rename.renaming_to} {renameActive.new_primary_domain} ({renameActive.state})</span>
            {/if}
          {:else if !renameActive}
            <button
              class="btn"
              data-testid={IDS.ADMIN_DNS_DOMAIN_PROMOTE_BUTTON}
              onclick={() => openRenameSheet(d.domain)}
            >{t.admin.dns.rename.promote}</button>
          {/if}
        </div>

        <!-- Per-domain catch-all actor picker (admin-dns-domain-catch-all-select;
             mail-aliases.md § Kind 4, mail-multidomain.md § Per-domain catch-all).
             "None" clears; any actor designates. Index-based dispatch (selectedIndex
             → ca.ids[idx]) so a label collision can't mis-route. The option `value`
             equals its label (the e2e selects by value === label, like the tier
             picker). -->
        <div class="catch-all-row">
          <span class="caption">{t.admin.dns.catch_all_label}</span>
          <select
            class="add-input"
            data-testid={IDS.ADMIN_DNS_DOMAIN_CATCH_ALL_SELECT}
            value={ca.selected}
            onchange={(e) => setCatchAll(d.domain, ca.ids[(e.currentTarget as HTMLSelectElement).selectedIndex])}
          >
            {#each ca.options as opt}<option value={opt}>{opt}</option>{/each}
          </select>
        </div>

        <!-- Present only when a SUCCESSION (not an admin) last cleared this
             domain's catch-all — tells the admin why the picker above
             reads "none" and that unmatched mail is now bouncing; re-designating
             via that same picker is the fix. Same read-only-explainer idiom as
             admin-dns-domain-rename-state above. -->
        {#if d.catch_all_cleared_by_succession_at !== null}
          <span class="hint" data-testid={IDS.ADMIN_DNS_DOMAIN_CATCH_ALL_CLEARED_STATE}
            >{t.admin.dns.catch_all_cleared_by_succession}</span>
        {/if}

        <!-- Per-domain role-address override pickers (admin-dns-domain-role-address-
             <role>-select; mail-multidomain.md § Per-domain role-address routing).
             One actor dropdown per overridable RFC 2142 role (postmaster/abuse/noc/
             security): option 0 = "Admin (default)" clears (the role falls back to
             the deployment admin), any actor designates. Same index-based dispatch as
             catch-all (selectedIndex → ram.ids[idx]) ×4; the nest atomic-merges so
             each role is independent. The option `value` equals its label (e2e selects
             by value === label). Lifts linux build_domain_section. -->
        <div class="role-address-block">
          <span class="caption">{t.admin.dns.role_address_label}</span>
          {#each ROLE_ADDRESS_ROLES as ra}
            {@const ram = buildRoleAddress(d, ra.kind)}
            <div class="role-address-row">
              <span class="role-caption">{ra.key}@</span>
              <select
                class="add-input"
                data-testid="admin-dns-domain-role-address-{ra.key}-select"
                value={ram.selected}
                onchange={(e) => setRoleAddress(d.domain, ra.kind, ram.ids[(e.currentTarget as HTMLSelectElement).selectedIndex])}
              >
                {#each ram.options as opt}<option value={opt}>{opt}</option>{/each}
              </select>
            </div>
          {/each}
        </div>

        <!-- Served-TLS-cert health badge (admin-dns-cert-status, § C.4): the
             nest-computed state of the cert the listener actually serves. A pure
             read — renders on every app incl web; "checking" until the read. -->
        <div class="cert-row">
          <span class="cert-badge {certStatusClass(cert)}" data-testid={IDS.ADMIN_DNS_CERT_STATUS}>{certStatusText(cert)}</span>
        </div>

        <!-- Cert issuance (§ B tier 2/3). The wasm-safe `acme_pure` order core
             landed (tls-certificates.md § Implementation status
             today), so web issues natively: a managed/CNAME-delegated domain issues
             in a single IssueCert (the held credential / delegation auto-publishes
             the `_acme-challenge` TXT); a manual domain opens the two-phase
             BeginManualIssueCert → paste → CompleteManualIssueCert. The get/renew
             button is inert while a manual order for this domain is pending (one
             order at a time) or until the self-nest seal target resolves. Mirrors
             linux `build_cert_issuance`. -->
        <div class="cert-issue-row">
          <button
            class="btn"
            data-testid={IDS.ADMIN_DNS_CERT_ISSUE_BUTTON}
            disabled={pendingHere || !targetNestId}
            onclick={() => issueCert(d.domain, isManaged || !!deleg)}
          >{t.admin.dns.cert.issue}</button>
        </div>
        {#if pendingHere && pendingCert}
          <p class="muted paste-instr">{t.admin.dns.cert.paste_instructions}</p>
          {#each pendingCert.challenges as ch}
            {@render recordCard(ch)}
          {/each}
          <div class="cert-actions">
            <button class="btn primary" data-testid={IDS.ADMIN_DNS_CERT_COMPLETE_BUTTON} onclick={completeManualIssue}>{t.admin.dns.cert.issue_complete}</button>
            <button class="btn" data-testid={IDS.ADMIN_DNS_CERT_CANCEL_BUTTON} onclick={cancelManualIssue}>{t.admin.dns.cert.issue_cancel}</button>
          </div>
        {/if}

        <!-- One-time `_acme-challenge` CNAME renewal-delegation (§ B tier 3, S6b).
             Config-only → renders + works on web. Delegated → "renewals automated"
             + remove + the one-time CNAME card; otherwise a reveal-on-demand form
             picking a controlled zone (disabled when no credential covers any). -->
        {#if deleg}
          <div class="cert-deleg-row">
            <span class="cert-badge ok">{t.admin.dns.cert.renewals_automated}</span>
            <button
              class="btn"
              data-testid={IDS.ADMIN_DNS_CERT_REMOVE_DELEGATION_BUTTON}
              onclick={() => removeDelegation(d.domain)}
            >{t.admin.dns.cert.remove_delegation}</button>
          </div>
          {@render recordCard(deleg.cname)}
        {:else if !delegateOpen[d.domain]}
          <div class="cert-deleg-row">
            <button
              class="btn"
              data-testid={IDS.ADMIN_DNS_CERT_DELEGATE_BUTTON}
              disabled={credZones.length === 0}
              title={credZones.length === 0 ? t.admin.dns.cert.delegate_no_zones : undefined}
              onclick={() => openDelegateForm(d.domain)}
            >{t.admin.dns.cert.delegate}</button>
          </div>
        {:else}
          <div class="cert-deleg-row">
            <span class="caption">{t.admin.dns.cert.delegate_zone_label}</span>
            <select
              class="add-input"
              data-testid={IDS.ADMIN_DNS_CERT_DELEGATE_ZONE_SELECT}
              value={delegateZone[d.domain] ?? credZones[0] ?? ''}
              onchange={(e) => { delegateZone = { ...delegateZone, [d.domain]: (e.currentTarget as HTMLSelectElement).value }; }}
            >
              {#each credZones as z}<option value={z}>{z}</option>{/each}
            </select>
            <button class="btn primary" data-testid={IDS.ADMIN_DNS_CERT_DELEGATE_SUBMIT_BUTTON} onclick={() => delegateRenewal(d.domain)}>{t.admin.dns.cert.delegate_submit}</button>
            <button class="btn" data-testid={IDS.ADMIN_DNS_CERT_DELEGATE_CANCEL_BUTTON} onclick={() => cancelDelegateForm(d.domain)}>{t.admin.dns.cert.delegate_cancel}</button>
          </div>
        {/if}

        <div class="record-list">
          {#each view?.records ?? [] as r}
            {@render recordCard(r)}
          {/each}
        </div>
      </div>
    {/each}
  </div>

  {#if removed.length > 0}
    <div class="removed-section">
      <h2 class="removed-heading">{t.admin.dns.removed_title}</h2>
      <p class="muted removed-desc">{t.admin.dns.removed_desc}</p>
      {#each removed as d}
        <div class="removed-row" data-testid={IDS.ADMIN_DNS_REMOVED_DOMAIN}>
          <span class="domain-name" data-testid={IDS.ADMIN_DNS_REMOVED_DOMAIN_NAME}>{d.domain}</span>
          <button
            class="btn"
            data-testid={IDS.ADMIN_DNS_REMOVED_DOMAIN_RESTORE_BUTTON}
            onclick={() => restoreDomain(d.domain)}
          >{t.admin.dns.restore}</button>
        </div>
      {/each}
    </div>
  {/if}
{/if}

<style>
  h1 { margin-bottom: 0.25rem; font-size: 1.5rem; }
  .subtitle { color: var(--text-muted, #8b949e); margin-bottom: 1.5rem; font-size: 0.875rem; }
  .muted { color: var(--text-muted, #8b949e); }
  .warning {
    color: var(--danger);
    font-size: 0.875rem;
    margin-bottom: 0.5rem;
    padding: 0.75rem;
    border: 1px solid var(--danger);
    border-radius: 8px;
  }

  .creds-section {
    border: 1px solid var(--border, #30363d);
    border-radius: 8px;
    background: var(--bg-surface, #161b22);
    padding: 1rem;
    margin-bottom: 1.5rem;
    display: flex;
    flex-direction: column;
    gap: 0.75rem;
    align-items: flex-start;
  }
  .creds-header { display: flex; align-items: center; justify-content: space-between; width: 100%; }
  .creds-title { font-size: 1rem; margin: 0; }
  .creds-list { display: flex; flex-direction: column; gap: 0.5rem; width: 100%; }
  .cred-item {
    display: flex;
    align-items: center;
    gap: 0.75rem;
    padding: 0.5rem 0.75rem;
    background: var(--bg-hover, #1c2128);
    border-radius: 6px;
  }
  .cred-provider { font-weight: 600; }
  .cred-zones { flex: 1 1 auto; font-size: 0.8rem; }
  .add-cred-form { display: flex; flex-direction: column; gap: 0.75rem; width: 100%; }
  .provider-row { display: flex; flex-wrap: wrap; gap: 0.5rem; }
  .fields { display: flex; flex-direction: column; gap: 0.5rem; }
  .field-group { display: flex; flex-direction: column; gap: 0.25rem; }
  .field-label { font-size: 0.75rem; color: var(--text-muted, #8b949e); }
  .form-actions { display: flex; gap: 0.5rem; }

  .add-domain { margin-bottom: 1.5rem; }
  .add-form { display: flex; gap: 0.5rem; align-items: center; }
  .add-input {
    flex: 1 1 auto;
    padding: 0.375rem 0.625rem;
    border-radius: 6px;
    border: 1px solid var(--border, #30363d);
    background: var(--bg-surface, #161b22);
    color: var(--text, #e6edf3);
    font-size: 0.875rem;
  }

  .domain-list { display: flex; flex-direction: column; gap: 1rem; }
  .domain-card {
    border: 1px solid var(--border, #30363d);
    border-radius: 8px;
    background: var(--bg-surface, #161b22);
    padding: 1rem;
  }
  .domain-header {
    display: flex;
    align-items: center;
    gap: 0.75rem;
    margin-bottom: 0.75rem;
  }
  .domain-name { font-weight: 600; font-size: 1.05rem; }
  .badge {
    font-size: 0.7rem;
    font-weight: 600;
    padding: 0.125rem 0.5rem;
    border-radius: 4px;
    background: rgba(88, 166, 255, 0.15);
    color: var(--accent, #58a6ff);
  }

  .record-list { display: flex; flex-direction: column; gap: 0.5rem; }
  .record {
    background: var(--bg-hover, #1c2128);
    border-radius: 6px;
    padding: 0.625rem 0.75rem;
    display: flex;
    flex-direction: column;
    gap: 0.25rem;
  }
  .record-row { display: flex; align-items: baseline; gap: 0.75rem; }
  .caption {
    flex: 0 0 4rem;
    font-size: 0.75rem;
    color: var(--text-muted, #8b949e);
  }
  .mono { font-family: ui-monospace, SFMono-Regular, Menlo, monospace; word-break: break-all; }
  .status-row { justify-content: space-between; margin-top: 0.25rem; }
  .hint { font-size: 0.75rem; color: var(--text-muted, #8b949e); font-style: italic; }

  .status {
    font-size: 0.75rem;
    font-weight: 600;
    padding: 0.125rem 0.5rem;
    border-radius: 4px;
  }
  .status.ok { background: rgba(63, 185, 80, 0.15); color: var(--success, #3fb950); }
  .status.bad { background: rgba(248, 81, 73, 0.15); color: var(--danger, #f85149); }
  .status.checking { background: rgba(139, 148, 158, 0.15); color: var(--text-muted, #8b949e); }

  /* Per-domain catch-all picker row. */
  .catch-all-row {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    margin-bottom: 0.5rem;
  }
  .catch-all-row .caption { flex: 0 0 auto; }
  .catch-all-row select { flex: 0 1 auto; }

  /* Per-domain role-address override pickers (one row per overridable role). */
  .role-address-block { margin-bottom: 0.5rem; }
  .role-address-row {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    margin: 0.25rem 0 0 1rem;
  }
  .role-address-row .role-caption {
    flex: 0 0 6rem;
    font-size: 0.75rem;
    font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
    color: var(--text-muted, #8b949e);
  }
  .role-address-row select { flex: 0 1 auto; }

  /* Per-domain cert lifecycle rows (badge / issuance / delegation). */
  .cert-row, .cert-issue-row, .cert-deleg-row {
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: 0.5rem;
    margin-bottom: 0.5rem;
  }
  .cert-badge {
    font-size: 0.75rem;
    font-weight: 600;
    padding: 0.125rem 0.5rem;
    border-radius: 4px;
  }
  .cert-badge.ok { background: rgba(63, 185, 80, 0.15); color: var(--success, #3fb950); }
  .cert-badge.warn { background: rgba(210, 153, 34, 0.15); color: var(--warning, #d29922); }
  .cert-badge.checking { background: rgba(139, 148, 158, 0.15); color: var(--text-muted, #8b949e); }
  .paste-instr { font-size: 0.8rem; margin: 0.25rem 0; }
  .cert-actions { display: flex; gap: 0.5rem; margin-bottom: 0.5rem; }

  .removed-section { margin-top: 2rem; }
  .removed-heading { font-size: 1rem; margin-bottom: 0.25rem; }
  .removed-desc { font-size: 0.8rem; margin-bottom: 0.75rem; }
  .removed-row {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 0.5rem 0.75rem;
    border: 1px solid var(--border, #30363d);
    border-radius: 6px;
    margin-bottom: 0.5rem;
  }

  .btn {
    font-size: 0.8rem;
    padding: 0.25rem 0.625rem;
    border-radius: 6px;
    border: 1px solid var(--border, #30363d);
    background: transparent;
    color: var(--text, #e6edf3);
    cursor: pointer;
  }
  .btn.primary { background: var(--accent, #1f6feb); border-color: var(--accent, #1f6feb); color: #fff; }
  .btn.danger { color: var(--danger, #f85149); border-color: rgba(248, 81, 73, 0.4); }
  .btn.active { background: rgba(63, 185, 80, 0.15); border-color: var(--success, #3fb950); color: var(--success, #3fb950); }
  .btn.selected { background: var(--accent, #1f6feb); border-color: var(--accent, #1f6feb); color: #fff; }
  .btn:disabled { opacity: 0.4; cursor: not-allowed; }
  /* Primary-domain rename banner + wizard sheet. */
  .rename-banner {
    border: 1px solid var(--accent, #1f6feb);
    border-radius: 8px;
    padding: 0.75rem 1rem;
    margin: 0.5rem 0 1rem;
    display: flex;
    flex-direction: column;
    gap: 0.4rem;
  }
  .rename-banner-head { display: flex; gap: 0.75rem; align-items: baseline; flex-wrap: wrap; }
  .rename-banner-meta { display: flex; gap: 1rem; flex-wrap: wrap; opacity: 0.85; font-size: 0.9em; }
  .rename-banner-actions { display: flex; gap: 0.5rem; align-items: center; flex-wrap: wrap; }
  .rename-sheet {
    border: 1px solid var(--border, #30363d);
    border-radius: 8px;
    padding: 1rem;
    margin: 0.5rem 0 1rem;
    display: flex;
    flex-direction: column;
    gap: 0.4rem;
    max-width: 32rem;
  }
  .rename-sheet-title { margin: 0 0 0.25rem; font-size: 1.05em; }
  .rename-sheet-actions { display: flex; gap: 0.5rem; margin-top: 0.5rem; }
  .add-input.narrow { max-width: 5rem; }
  .warn { color: var(--danger, #f85149); font-size: 0.9em; }
</style>
