// Shared snapshot shapes for the page-level `AtprotoSettingsMachine`
// (`libs/fauna-atproto-settings-machine`, WASM twin
// `libs/fauna-wasm-atproto-settings`), consumed by
// `$lib/components/AtprotoSettingsSection.svelte`. See
// docs/goal/behavior/atproto-pds-full.md § App surface.
//
// Mirrors `fauna_atproto_settings_machine::snapshots::{AppCredentialRow,
// AtprotoSessionRow, AtprotoSettingsSnapshot}` serde JSON (snake_case across
// the serde_json boundary).
import type { LocalizedText } from '$lib/i18n/localized';

/** One app credential (`atproto-app-credential-item` row). */
export interface AppCredentialRow {
  credential_id: string;
  label: string;
  dm_allowed: boolean;
  created_at_millis: number;
  last_used_at_millis: number | null;
  /** Whether THIS device holds the secret locally (a sibling-device mint is
   *  listed but not revealable — a normal state, never an error). Gates
   *  `atproto-app-credential-reveal`. */
  revealable: boolean;
}

/** One live session (`atproto-connected-app-item` row). F1 has no OAuth
 *  grant rows — `plane` is `"app_credential"` today; OAuth joins at F4. */
/** One permission set frozen into a grant at the consent ceremony — the same
 *  provenance the consent card showed, composed by the same shared pass. */
export interface ConsentSetRow {
  /** The set's NSID, rendered VERBATIM — its identity, exactly as `client_id`
   *  is the client's. Nothing derives a display name or an authority from it. */
  nsid: string;
  /** The set's declared title, already control-character-stripped by the
   *  machine; `null` renders as the NSID alone, never as an invented label. */
  title: string | null;
  /** Advisory prose from the set's author, stripped in the same pass. */
  details: string | null;
  /** The expansion, one human-readable line per member, through the same
   *  shared `describe_scope` the flat list uses. */
  member_descriptions: string[];
}

/** The OAuth grant behind a session, joined on by the machine (never by a
 *  client): `AtprotoSettingsMachine::refresh` reads `list_grants` and matches
 *  `hex(grant_id) === session_id_hex`, because the nest writes both ids as the
 *  same bytes in one transaction. Nothing here needs `list_grants` or a join. */
export interface AtprotoGrantRow {
  /** The client-id URL, rendered VERBATIM beside the resolved name: it is the
   *  client's self-authenticating identity, while the name is only what a
   *  document at that URL claimed. */
  client_id: string;
  /** The client's name from its resolved metadata document, when it published
   *  one — ATTACKER-CONTROLLED text, already control-character-stripped by the
   *  shared machine. Never add a second sanitizer here. */
  client_name: string | null;
  /** What the user approved, one human-readable line per scope, in the grant's
   *  order — already `describe_scope`'s wording. Never re-word it client-side:
   *  a user comparing what they approved against what this row says must not be
   *  reading two vocabularies. */
  scope_descriptions: string[];
  /** The named permission bundles this grant was made from, frozen at the
   *  ceremony. Empty for a grant made from granular scopes alone. */
  sets: ConsentSetRow[];
  /** Advisory, NOT forensics (`atproto-pds-full.md` D10 § Audit): the nest
   *  observes a grant only at a refresh rotation, so `null` means "not seen
   *  since connecting" and never "unused". */
  last_used_at_millis: number | null;
}

export interface AtprotoSessionRow {
  session_id_hex: string;
  plane: string;
  credential_id: string | null;
  client_note: string | null;
  created_at_millis: number;
  last_refreshed_at_millis: number | null;
  /** `null` exactly when there is no live session to expire — a `suspended`
   *  row. Deliberately NOT backfilled from the grant's own horizon: that is a
   *  different value with a different meaning, and an expiry countdown on a row
   *  that does not work reads as "it works until then". */
  expires_at_millis: number | null;
  /** The OAuth grant behind this session — `null` exactly for an
   *  app-credential row, which is why the field is nullable rather than a
   *  widened flat shape: a row without a grant must not render a scope list. */
  grant: AtprotoGrantRow | null;
  /** The login plane was suspended while this grant was left at rest: the
   *  approval stands and stays revocable, but the app cannot currently act
   *  (`ui/atproto.md`'s downward matrix). Always `false` on the
   *  app-credential plane, which has no grant row at all. */
  suspended: boolean;
}

/** The hosted identity summary (`atproto-hosted-handle`). The raw DID string
 *  is never shown — deliberately absent here. */
export interface IdentitySummaryRow {
  /** The derived ATProto handle; empty when it doesn't derive yet. */
  handle: string;
  /** `"plc"` or `"web"` — a fact once minted, displayed not chosen. */
  method: string;
  /** `"pending"` (mint owed) | `"active"` | `"deactivated"`. */
  status: string;
}

/** The consume-side link summary — enough for the Linked panel header and the
 *  transition card's unlink line. */
export interface LinkSummaryRow {
  display: string;
}

/** One pending OAuth consent request (`atproto-consent-card`, F4 rung 2) — an
 *  external ATProto app asking to sign in, blocked in a browser waiting for
 *  the answer. Renders unconditionally, never gated on how close the request
 *  is to expiring — there is deliberately no expiry field to check against
 *  (`atproto-pds-full.md` § F4 detail). */
export interface ConsentCardRow {
  /** Opaque handle `resolveConsent` takes. */
  consent_id_hex: string;
  /** The binding code (`atproto-consent-code`) the user compares against
   *  their browser — minted by the nest, so this value and the one the
   *  browser's `/oauth/authorize` page shows have one origin. */
  code: string;
  /** The requesting `client_id` — a URL, rendered VERBATIM. Never parsed or
   *  used to derive an origin/host/fetch target client-side. */
  client_id: string;
  /** The client's resolved display name, when it published one; `null`
   *  renders as `client_id` alone. */
  client_name: string | null;
  /** One human-readable line per requested scope, from the shared
   *  `authz::describe_scope` the browser's own consent page renders —
   *  render verbatim, never re-author. */
  scope_descriptions: string[];
  /** The named permission bundles this request came from — same shape and
   *  same single composition pass as the grant row's `sets`
   *  (`consent_set_row`; title/details already control-stripped). Rendered
   *  AFTER the flat scope list, never instead of it: every member is already
   *  in `scope_descriptions`, and this section answers what that list cannot
   *  — which bundle produced them, and what its publisher says it is for.
   *  Empty for the overwhelmingly common set-less request. */
  sets: ConsentSetRow[];
  /** What approving this card ends besides granting it — set by the
   *  connected-apps machine when the consent attests a key other than the
   *  roster's for the same client; `null` otherwise. Rendered after the
   *  scope lines. */
  ends?: LocalizedText | null;
}

/** The verified D10 authoring delegation (`atproto-delegation-row`) — what
 *  authorizes an external ATProto app to *post* as this account, as opposed to
 *  merely signing in (the credential and connected-app rows govern that).
 *
 *  `null` on the snapshot covers BOTH "never authorized" and "the stored cert
 *  failed the client-side verify under this account's own identity key" — the
 *  row is withheld either way, never rendered as a grant the user cannot be
 *  shown to have made (`atproto-pds-full.md` § App surface → *The client
 *  verifies the cert it is shown*; the mismatch surfaces on `error-message`). */
export interface DelegationRow {
  /** The authorized sub-key's public half, hex. */
  device_key_hex: string;
  /** Granted capabilities as their WIRE spellings (`"Post"`,
   *  `"UpdateProfile"`) — render them through `delegationCapabilityLabels`,
   *  never a local map (the shared answer lives in Rust). */
  capabilities: string[];
  /** When the user authorized it — epoch **MICROseconds**, `Timestamp`'s unit,
   *  because it comes from the signed cert rather than the wire. The
   *  credential/session rows above use milliseconds; mixing them up dates the
   *  row ~50 000 years out. */
  authorized_at_micros: number;
  /** When it lapses; `null` for a cert carrying no expiry. Microseconds, same
   *  reason. */
  expires_at_micros: number | null;
  /** `"active" | "expiring_soon" | "expired" | "never_expires"` — the wire
   *  spelling, carried verbatim into the leaf's `state` attr so an e2e asserts
   *  the state and never its prose. An unrecognized value from a newer nest
   *  still renders (degrade, never fail to decode). */
  liveness: string;
  /** Epoch-**milliseconds** of the last reported external-app write; `null` =
   *  none reported.
   *
   *  ADVISORY ONLY (`atproto-pds-full.md` D10 § Audit). Every other field here
   *  derives from the signed cert; this one is a bare nest assertion with
   *  nothing signing it, so it is not proof of use and — the direction that
   *  matters — **not proof of non-use**. Present it hedged; the audit surface
   *  that IS trustworthy is the feed's `delegated-origin-badge`. */
  last_used_at_millis: number | null;
}

/** The 72 h recovery-fork contest card (`atproto-contest-card`) — LEADS the
 *  page, above the depth selector, whenever a standing custody alarm names a
 *  box-authored op this client's ring protects
 *  (`docs/goal/behavior/atproto-identity-custody.md` § The 72 h recovery-fork
 *  contest). Rendered off CLIENT-SIDE evidence only. */
export interface ContestCardRow {
  /** `"contestable" | "window-closed" | "not-contestable"` — the three
   *  honest states, assertable rather than inferred from which controls
   *  render. `not-contestable` covers two distinct reasons (a genesis
   *  violation and a log that does not authenticate); `detail` is what
   *  distinguishes them. */
  state: string;
  /** `atproto-contest-detail` — what the box-authored op did, composed by
   *  the machine. Render VERBATIM: it deliberately names what undoing does
   *  NOT restore (the nest keeps the key it publishes with) — never
   *  shorten past that sentence. */
  detail: LocalizedText;
  /** `atproto-contest-deadline` — the advisory countdown to the 72 h
   *  window's close; `null` when the directory published no parseable
   *  timestamp or the state is already terminal. */
  deadline: LocalizedText | null;
  /** Render `atproto-contest`: only in the `contestable` state — a dead
   *  button on a hopeless state is exactly what decision 2 rules out. */
  show_contest: boolean;
}

/** The contest ceremony's confirm card (`atproto-contest-confirm-card`);
 *  present exactly while the user has opened the ceremony and not yet
 *  cancelled it. The open/cancel state lives on the machine, not the page —
 *  the last surface before a signature that rewrites who controls the
 *  identity, and there are seven shells. */
export interface ContestConfirmCardModel {
  /** The card copy, composed machine-side — render verbatim, joined, never
   *  re-authored (names the op being contested, what the fork will sign,
   *  and the deadline). */
  lines: LocalizedText[];
  /** The submit is in flight: `atproto-contest-confirm` /
   *  `atproto-contest-cancel` disable so a second press cannot sign twice. */
  in_progress: boolean;
}

/** The delete ceremony's confirm card (`atproto-delete-confirm-card`);
 *  present exactly while the user has opened the ceremony and not yet
 *  cancelled it — the last surface before the page's one destructive call,
 *  `fauna.bridges.atproto.delete_presence`. Row 7's six-app trickle-down. */
export interface DeleteConfirmCardModel {
  /** The card copy, composed machine-side — render verbatim: what the sweep
   *  destroys, that the identity survives (named), the honest caveat, and
   *  where the level lands. */
  lines: LocalizedText[];
  /** The sweep is in flight: `atproto-delete-confirm` / `atproto-delete-cancel`
   *  disable so a second press cannot send a second sweep. */
  in_progress: boolean;
  /** The `atproto-delete-tombstone` opt-in (S5 slice 5b) — the machine's
   *  `RetireIdentityOptIn`. Not rendered on web yet (the tui-first
   *  trickle-down); mirrored so the type matches what wasm hands over. */
  retire_identity: RetireIdentityOptIn;
}

/** The delete ceremony's terminal "also permanently retire this identity"
 *  opt-in. Greyed with `unavailable_reason` where the identity cannot be
 *  retired (did:web, or a did:plc not yet published); never pre-ticked. */
export interface RetireIdentityOptIn {
  available: boolean;
  selected: boolean;
  unavailable_reason: LocalizedText | null;
}

/** The staged transition card (`atproto-depth-confirm-card`); present exactly
 *  while a level change awaits confirm — the level never changes on
 *  selection alone. */
export interface TransitionCardModel {
  /** The wire spelling of the level the confirm would move to. */
  target_level: string;
  /** The card copy, one localized line per effect, composed from the shared
   *  `TransitionPlan` — render verbatim, never re-author. */
  lines: LocalizedText[];
  /** Render the `atproto-history-backfill` checkbox (only a minting move
   *  offers it). */
  show_history_backfill: boolean;
  /** A confirm is in flight (a PLC mint round-trips) — the confirm/cancel
   *  controls disable. */
  in_progress: boolean;
}

/** The whole renderable ATProto settings surface in one record — the depth
 *  selector, the hosted-identity panel, and the F1 login-plane rows it gates.
 *  Never carries a secret — mint/reveal return theirs directly, once. */
export interface AtprotoSettingsSnapshot {
  // ── The 72 h recovery-fork contest (`atproto-contest-*`) — LEADS the
  //    page, above everything below. `null` unless a standing custody
  //    violation names an op this client's ring protects. ────────────
  contest: ContestCardRow | null;
  /** Present exactly while the ceremony is open. */
  contest_confirm: ContestConfirmCardModel | null;

  // ── The depth selector (`ui/atproto.md` § Layout & flow) ────────────
  /** `"off" | "linked" | "hosted_visible" | "hosted_full"`. */
  level: string;
  /** The real-domain gate verdict: `false` greys the two hosted rungs
   *  (never hides them — `hosted_gate_reason` says why). */
  hosted_allowed: boolean;
  hosted_gate_reason: LocalizedText | null;
  /** The would-be ATProto handle for the "your handle is @you.yourdomain
   *  either way" line; empty when unknown. */
  handle_preview: string;
  /** Present at any status — a deactivated identity stays visible so the
   *  user sees what re-enabling restores. */
  identity: IdentitySummaryRow | null;
  link: LinkSummaryRow | null;
  /** The staged transition card; `null` when no level change is pending. */
  pending_transition: TransitionCardModel | null;
  /** The DID-method radio state (`"plc"` default, `"web"` opt-in); renders
   *  only while `show_did_method_radio`. */
  did_method: string;
  /** Only before any identity exists — after mint the method is a fact. */
  show_did_method_radio: boolean;
  history_backfill: boolean;
  /** Whenever a hosted identity exists, active or deactivated, so the
   *  stronger action stays reachable after a step-down — and NOT once the
   *  presence is already destroyed, where the gesture's only outcome is a
   *  no-op. */
  show_delete_presence: boolean;
  /** Present exactly while the delete ceremony is open. */
  delete_confirm: DeleteConfirmCardModel | null;

  // ── The F1 login-plane surface (gated on level = hosted_full) ───────
  /** Pending OAuth consent requests — leads the full-PDS panel (an app
   *  somewhere is blocked on the answer, everything else can wait). */
  consents: ConsentCardRow[];
  credentials: AppCredentialRow[];
  sessions: AtprotoSessionRow[];
  external_apps_enabled: boolean;
  /** The D10 authoring delegation, or `null` when none is provisioned (or the
   *  stored cert failed the client-side verify — see `DelegationRow`). */
  delegation: DelegationRow | null;
  /** Page-level `error-message` (last gesture/refresh failure); `null` when clear. */
  error: LocalizedText | null;
}
