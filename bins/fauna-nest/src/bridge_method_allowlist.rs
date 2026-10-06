//! Per-caller-class allowlist for every WS-RPC kind, not just
//! `fauna.bridges.*` (the name predates the allowlist growing to cover
//! every namespace). The router looks up the calling actor's class —
//! Bridge(MTA), Bridge(MDA), User, or Admin — and gates kind access here.
//! Caller class is computed by handlers via the helper
//! `caller_class_for_actor`, and [`require_permission`] wraps the whole
//! resolve-then-gate control flow shared by every `*_handlers.rs` file.

use crate::db::CacheDb;
use crate::db::bridge_service_users::{BridgeRole, BridgeStatus};
use fauna_protocol::RpcError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallerClass {
    BridgeMta,
    BridgeMda,
    /// A CUSTODIAN device (W8.6 (account-data-plane.md § Workstreams) — `account-data-plane.md` § Replica posture →
    /// *The custody grant + ceremony*): an otherwise-unknown key that a LIVE
    /// custody-class capability row names as holder, authenticated by the
    /// custody handshake's PoP + witness. **Least-privilege by the
    /// allowlist**: its whole WS reach is `fauna.sync.changes.list` (the index
    /// plane) and `fauna.segments.list` (the bulk plane's control half, row 148
    /// slice 2) — both re-derive the verdict from the live row per request, and
    /// every other kind refuses at `require_permission`, so a custody bearer
    /// can never act as anyone. ⚠ The bulk plane's *byte* half — `GET
    /// /api/v1/segments/{kind}/{actor}/{id}[/meta]` — is HTTP and therefore
    /// outside this allowlist entirely; it shares the same
    /// `custody_admission` predicate but is gated by `BearerAuth`, so widening
    /// custody's reach means auditing both surfaces, not just this table.
    /// Custody stays a READ capability on both: `fauna.segments.compact` is
    /// deliberately User-only. Derived per request from the rows themselves —
    /// `fauna.capabilities.revoke` deletes the row and this class evaporates
    /// with it, severing even a live session at its next dispatch.
    Custodian,
    /// A generic content-processing bridge (scorer / FTS-indexer — the dual of
    /// the MDA), the holder class for user-minted capability grants. Its only
    /// gated reach is `fauna.capabilities.fetch` (holder-pubkey-scoped); the
    /// fine-grained boundary is the wrapped-key scope, crypto-self-enforcing
    /// (design § Phase 2 Step 2 § 2.2/§ 2.4). Distinct from `BridgeMda` only in
    /// that it holds no legacy mail-serving reach.
    ContentProcessor,
    /// The out-of-process ATProto PDS bridge (`atproto-pds-bridge.md`, role
    /// `atproto.pds`). **Least-privilege:** in S1 its only reach is the shared
    /// bridge-lifecycle RPCs (`whoami`, `register_service_user`, `fetch_config`)
    /// — it holds **none** of the mail bridge's DKIM/TLS/outbound/IMAP surface.
    /// S2 added its identity roster/keys/mint-report kinds; S3 added the
    /// projection reads (`fetch_public_posts`, `fetch_profile`) plus the
    /// `projection_ready` push, keeping the grant explicit and auditable.
    BridgeAtprotoPds,
    User,
    Admin,
    /// A **third-party principal** on a session of its own
    /// (`apps/bridges.md` § Capability-allowlist enforcement → *`ThirdParty`*;
    /// the session is `transport-connection.md` § Connection lifecycle → *The
    /// principal session*). The arms naming it are the class's **maximum**
    /// reach — the compiled ceiling; what one principal actually reaches is
    /// that ceiling narrowed by [`scope_covers`] over its row's scopes ∩ its
    /// session token's.
    ///
    /// **Never an actor's class.** [`caller_class_for_actor`] has no
    /// `ThirdParty` answer: the dispatch gate resolves this class from a
    /// connection's principal binding (`principal_handlers`), so an account's
    /// own `User`/`Admin` reach is unreachable from a principal session by
    /// construction. And a `ThirdParty` kind is served by a principal handler
    /// of its own, never the actor handler — a matrix arm is necessary, not
    /// sufficient.
    ThirdParty,
}

/// Per-kind permission. Returns true iff the caller class is permitted.
///
/// **Admin ⊇ User.** An admin is a user who additionally holds the admin role
/// (the role is grantable/revocable, so it is ADDITIVE — revoking admin leaves a
/// normal user with their data intact). A caller classified `Admin` therefore
/// inherits every `User` permission. This is safe because every user-facing kind
/// is *caller-scoped* (operates on `target == caller`), so an admin only ever
/// reaches its OWN data — never another user's — even on a multi-user nest; and
/// on an encrypted nest a user's data is sealed to their own keys regardless. So
/// the kinds below name only the *minimum* class; admins get the User set for
/// free. See memory `admin-is-a-user-with-extra-role`.
pub fn is_permitted(class: CallerClass, kind: &str) -> bool {
    use CallerClass::*;
    if class == Admin && is_permitted(User, kind) {
        return true;
    }
    match kind {
        // ── ATProto PDS F1 auth core (`atproto-pds-full.md` § WS-RPC kind
        // surface). Bridge class: verifier fetch + the session registry —
        // verifier rows are Argon2id hashes of ≥80-bit generated secrets,
        // safe to hand the attested PDS bridge; still bridge-only. User
        // class (self-scoped, Admin ⊇ User via the blanket rule above):
        // credential mint/list/revoke, session list/revoke, kill-switch.
        "fauna.bridges.atproto.fetch_app_credential_verifiers"
        | "fauna.bridges.atproto.record_session"
        | "fauna.bridges.atproto.refresh_session"
        | "fauna.bridges.atproto.end_session"
        // Preferences (app.bsky.actor.{get,put}Preferences): private per-account
        // state served locally (D4 custody). Bridge-only — the attested PDS
        // host reads/writes them on the authenticated XRPC caller's behalf.
        | "fauna.bridges.atproto.fetch_preferences"
        | "fauna.bridges.atproto.store_preferences"
        // F2 — the external write path (`atproto-pds-full.md` § F2 detail).
        // Bridge-only, and emphatically NOT User-class: this kind authors
        // content on an account's behalf from a batch the attested PDS host
        // assembled after authenticating an XRPC session and applying D8. A
        // Fauna app writes through its own identity-signed post/profile
        // kinds, never through this one.
        | "fauna.bridges.atproto.ingest_external_write"
        // F2.4 — `com.atproto.repo.uploadBlob`'s account-attribution leg. The
        // bytes ride the bulk-binary carve-out (which cannot know the account);
        // this kind names it. Bridge-only for the same reason the write path is:
        // it records media on an account's behalf from an XRPC session the
        // attested PDS host authenticated.
        | "fauna.bridges.atproto.record_blob"
        // The bridge→nest half of the permission-set request call (the nest's
        // `/oauth/par` resolving an `include:` through the bridge's chain).
        // Bridge-only because the delivered bytes feed an authorization
        // decision as VERIFIED: only the attested PDS host runs the chain that
        // verified them.
        | "fauna.bridges.atproto.deliver_permission_set" => matches!(class, BridgeAtprotoPds),
        "fauna.bridges.atproto.provision_app_credential"
        | "fauna.bridges.atproto.list_app_credentials"
        | "fauna.bridges.atproto.revoke_app_credential"
        | "fauna.bridges.atproto.list_sessions"
        | "fauna.bridges.atproto.list_grants"
        | "fauna.bridges.atproto.revoke_session"
        | "fauna.bridges.atproto.set_external_apps_enabled"
        // The depth selector's transition kind and its status read — both
        // self-scoped: they move/report the CALLER's own integration level,
        // never another actor's.
        | "fauna.bridges.atproto.set_integration_level"
        | "fauna.bridges.atproto.get_integration_status"
        // The destructive sibling of the selector's downward transitions
        // (S5 slice 5) — self-scoped for the same reason and, being the
        // stronger action, especially so: a caller may only ever destroy
        // their OWN presence.
        | "fauna.bridges.atproto.delete_presence"
        // The terminal identity retirement's two halves (S5 slice 5b): record
        // the opt-in, and record that the client's own signed PLC tombstone was
        // accepted. USER-class, never bridge-callable — the bridge holds only
        // the JUNIOR rotation key, and the whole point of the custody split is
        // that no box-held key can end the identity.
        | "fauna.bridges.atproto.request_tombstone"
        | "fauna.bridges.atproto.record_tombstone"
        // D10 authoring delegation, all self-scoped on the caller: fetch/mint
        // the account's authoring sub-key, upload its identity-signed
        // delegation cert, destroy it. Deliberately NOT bridge-callable — the
        // delegation is minted by the user's own client from a key only they
        // hold, and the bridge never needs to read or mint one.
        | "fauna.bridges.atproto.fetch_authoring_key"
        | "fauna.bridges.atproto.fetch_authoring_delegation"
        | "fauna.bridges.atproto.provision_authoring_delegation"
        | "fauna.bridges.atproto.revoke_authoring_delegation"
        // F4 consent ceremony, user half: read the live approval cards and
        // answer one. Self-scoped — `list` returns the caller's own rows plus
        // the unassigned pool, and `resolve` matches only a row the caller may
        // answer. This call arriving over the caller's own authed connection IS
        // the ceremony's Ed25519 root (D3 rung 2).
        | "fauna.bridges.atproto.list_pending_consents"
        | "fauna.bridges.atproto.resolve_consent" => matches!(class, User),
        "fauna.bridges.fetch_wrapped_mls_blob" => matches!(class, BridgeMda),
        "fauna.bridges.fetch_mls_snapshot_blob" => matches!(class, BridgeMda),
        // The MDA fetches the actor's WebDAV served-set key blob at AUTH to serve
        // WebDAV (webdav-server.md § MDA↔nest WS-RPC contract). BridgeMda-only —
        // the served-set content keys are the MDA's per-set capability.
        "fauna.bridges.fetch_webdav_keys_blob" => matches!(class, BridgeMda),
        // A bridge mints a short-TTL scoped token for the chunk/manifest byte
        // routes it can't otherwise reach (webdav-server.md § Bulk-byte plane).
        // BOTH bridge classes may call, but the *purpose* decides what they get,
        // and the handler enforces that (`mint_bulk_byte_token_handler`):
        //   Folder  → BridgeMda only, gated on the set being served to the actor.
        //   MailBody → either bridge, gated on the actor being a mail recipient
        //              (smtp-server.md § Message size limits — a sealed body over
        //              the inline budget stages on the byte plane, and the MTA is
        //              the producer).
        // Admitting the MTA here did NOT open the folder path to it: an MTA
        // asking for a Folder token is denied. Pinned by
        // `an_mta_may_not_mint_a_folder_token`.
        "fauna.bridges.mint_bulk_byte_token" => matches!(class, BridgeMda | BridgeMta),
        // The WebDAV data plane the MDA terminator drives over the folder
        // substrate (webdav-server.md § MDA↔nest WS-RPC contract): enumerate the
        // actor's served sets, list a served set's latest-per-path files, and
        // record a WebDAV write as an ordinary change. All BridgeMda-only — each
        // gates on the set being served to the actor (owned + non-reserved +
        // `webdav_enabled`); a Fauna app uses the direct `fauna.sync.*`
        // surface instead, not this served-file bridge channel.
        "fauna.bridges.webdav_list_folders" => matches!(class, BridgeMda),
        "fauna.bridges.webdav_list_files" => matches!(class, BridgeMda),
        "fauna.bridges.webdav_quota" => matches!(class, BridgeMda),
        "fauna.bridges.webdav_record_change" => matches!(class, BridgeMda),
        // The WebDAV bearer door's admission relay (webdav-server.md § Key
        // model → *A principal's read*): the MDA hands over a principal's DPoP
        // token and proof, the nest runs its one principal admission. The read
        // arm's door — `FaunaScopeArm::door` names this kind, and the sweep
        // `live_grammar_arms_equal_the_arms_the_ceiling_names` holds the two.
        "fauna.bridges.webdav_admit_principal" => matches!(class, BridgeMda),
        "fauna.bridges.fetch_wrapped_submission_token" => matches!(class, BridgeMta),
        // The ATProto PDS bridge (`atproto.pds`) terminates its own `pds.<domain>`
        // TLS on the SNI-routed XRPC listener (atproto-pds-full.md § Wire & process
        // topology, F1 packaging resolution), so it fetches the sealed cert blob
        // the same way the mail bridges do — seal-on-read binds it to the caller's
        // attested x25519 and the requested domain, so admitting the class opens no
        // escalation surface (a PDS bridge can only obtain a cert sealed to itself).
        "fauna.bridges.fetch_tls_cert_blob" => {
            matches!(class, BridgeMta | BridgeMda | BridgeAtprotoPds)
        }
        "fauna.bridges.fetch_bridge_pubkey" => {
            matches!(class, BridgeMta | BridgeMda | User | Admin)
        }
        // Sidecar log plane, enrolled-bridge leg (`observability.md` § The
        // sidecar log plane). Every bridge class may report its own catalogued
        // events; nest derives the `<source>:` attribution from *this* class,
        // so admitting all four opens no cross-source impersonation. Emphatically
        // NOT User/Admin: a client has its own local ring and its own Logs page,
        // and a user-writable path into the admin ring would be a spoofing
        // surface with no legitimate caller.
        "fauna.bridges.report_log_events" => {
            matches!(
                class,
                BridgeMta | BridgeMda | ContentProcessor | BridgeAtprotoPds
            )
        }
        // Self-scoped enable-mail provisioning: each handler writes the blob
        // under the *connection's own* `actor_id` (no target param), so there is
        // no escalation surface — the caller can only provision their own mail
        // key material. `User | Admin` because the admin is a Fauna app with
        // their own account and enables their own mail through the same client
        // UI (product invariant: "nest configuration is set from Fauna apps";
        // the admin who claims the nest is its first user). Mirrors the sibling
        // `provision_recipient_mls_pubkey` / `revoke_wrapped_mls_blob` /
        // `revoke_wrapped_submission_token` gates, which were widened the same
        // way; the `User`-only gate here was the matching oversight — it blocked
        // an admin from enabling their own mail (`fauna.bridges.permission_denied`).
        "fauna.bridges.provision_wrapped_mls_blob" => matches!(class, User | Admin),
        "fauna.bridges.provision_mls_snapshot_blob" => matches!(class, User | Admin),
        // Self-scoped WebDAV served-set key blob provisioning: the handler writes
        // the blob under the connection's own actor_id (no target param), so the
        // caller can only provision their own served-set keys. `User | Admin` like
        // the sibling provision gates (the admin is a Fauna app with their own
        // account — memory `admin-is-a-user-with-extra-role`).
        "fauna.bridges.provision_webdav_keys_blob" => matches!(class, User | Admin),
        "fauna.bridges.provision_wrapped_submission_token" => matches!(class, User | Admin),
        "fauna.bridges.provision_tls_cert_blob" => matches!(class, Admin),
        // DKIM read/manage surface for the admin DNS page — public metadata
        // only. No kind hands a DKIM private key to any caller: the nest holds
        // it and signs at the outbound hand-out.
        "fauna.bridges.list_dkim_selectors" => matches!(class, Admin),
        "fauna.bridges.revoke_dkim_blob" => matches!(class, Admin),
        // Unified DNS management (`fauna.dns.*`) —
        // the post-onboarding DNS surface (record matrix, live verify,
        // provider credentials, managed publish). Admin-only: DNS records +
        // provider API tokens are deployment-operational config, never a user
        // or bridge concern. See `docs/goal/behavior/dns-management.md`.
        "fauna.dns.list_records" => matches!(class, Admin),
        "fauna.dns.verify_records" => matches!(class, Admin),
        // The DNS-01 propagation gate's readiness probe, run nest-side for the
        // web app (a browser has no raw DNS). Reads public DNS and mutates
        // nothing, but it belongs to the admin cert surface and stays Admin-only
        // like the rest of the namespace. No bridge has a reason to ask.
        "fauna.dns.probe_txt_visible" => matches!(class, Admin),
        // The onboarding hand-off that persists the deployment's own public
        // address (apex + mail-host A/AAAA/PTR source). Admin-only — host config.
        "fauna.dns.set_host_address" => matches!(class, Admin),
        // Roster read, class-scoped in the HANDLER (2026-07-17, closing the
        // nests.md "non-admin holder discovery" gap): an Admin gets the full
        // roster (approval cards / rotate page); a plain User gets only the
        // mint-relevant holder view — approved + x25519-attested +
        // content-processor-family role + NOT in-process — so the trust facet
        // can enumerate seal targets without exposing enrollment history or
        // deployment topology, and without a second wire kind. Bridges have
        // no reason to read the roster; they stay denied.
        "fauna.bridges.list_service_users" => matches!(class, User | Admin),
        // Bridge approval lifecycle (Stage 1, `mail-bridge-lifecycle.md`
        // § Pending approval / § Default-off): admin-only. Approval is
        // admin-initiated by the security model — a bridge can ask (by
        // connecting with an unknown pubkey) but only an admin completes it.
        "fauna.bridges.list_pending_bridges" => matches!(class, Admin),
        "fauna.bridges.approve_pending_bridge" => matches!(class, Admin),
        "fauna.bridges.reject_pending_bridge" => matches!(class, Admin),
        "fauna.bridges.set_mail_enabled" => matches!(class, Admin),
        "fauna.bridges.set_caldav_enabled" => matches!(class, Admin),
        // Deployment-wide CardDAV-enable toggle — contacts twin of
        // `set_caldav_enabled` (`2026-07-02-carddav-server-design.md` § 5).
        // Admin-only: enabling a contacts surface is a deployment choice.
        "fauna.bridges.set_carddav_enabled" => matches!(class, Admin),
        // Deployment-wide WebDAV-enable toggle — files twin of
        // `set_carddav_enabled` (`webdav-server.md` § Independent enablement).
        // Admin-only: enabling a files surface is a deployment choice.
        "fauna.bridges.set_webdav_enabled" => matches!(class, Admin),
        // Admin-set CalDAV listener port (`caldav-server.md` § Network exposure
        // — admin-settable port). Admin-only: the port a bare-IP / desktop box
        // serves CalDAV on is a deployment-wide admin *choice*, persisted in nest
        // state, never a per-user control.
        "fauna.bridges.set_caldav_port" => matches!(class, Admin),
        // Read twin of `set_caldav_port`, `User | Admin`: a regular user's
        // mail-settings page reads the effective port to display the CalDAV
        // connection detail for a third-party calendar app (the non-sensitive
        // `MuaInstructions.caldav_port` read-path; `caldav-server.md`
        // § Implementation status — Client endpoint display). Setting it stays
        // Admin-only above; reading the single port number is user-safe.
        "fauna.bridges.get_caldav_port" => matches!(class, User | Admin),
        // Deployment-wide "auto-enable mail for new users" policy default
        // (mail-policy-config.md § Tier-2 new-user mail defaults). Admin-only —
        // a deployment default, NOT a per-user control (mail is user-controlled,
        // admin.md § Don't do these). Drives no bridge state; surfaced to clients
        // on `fauna.setup.status`.
        "fauna.bridges.set_auto_enable_mail_for_new_users" => matches!(class, Admin),
        // Per-actor IMAP/CalDAV-serving opt-out (deployment-home-with-public-
        // relay.md § MUA reach). `set` is **User-class + caller-scoped** (the
        // user sets their OWN row; the handler keys on the authenticated actor,
        // so no actor can set another's) — distinct from the Admin-only
        // deployment-wide toggles above. `get` is `User | Admin`: a user reads
        // their own flag; an admin reads any actor's for the read-only audit
        // view (admin may see but not set another user's preference).
        "fauna.bridges.set_mail_serving_enabled" => matches!(class, User),
        "fauna.bridges.get_mail_serving_enabled" => matches!(class, User | Admin),
        // Running-phase re-keying: revoke an already-approved bridge so the
        // admin can rotate its key (`mail-bridge-lifecycle.md` § Service-user
        // re-keying). Admin-only — a bridge cannot revoke itself or a peer.
        "fauna.bridges.revoke_service_user" => matches!(class, Admin),
        // Writer for `actor_mls_pubkeys` — the production counterpart to
        // `fetch_recipient_mls_pubkey` (which the MTA reads on every
        // inbound DATA to seal the body to the recipient). The user
        // self-registers their OWN recipient pubkey at enable-mail (the
        // handler enforces `target == caller` for the User class); Admin
        // may register any actor's (bridge-perimeter / migration). The
        // recipient pubkey is the user's MSEK-derived standing HPKE key
        // (key-material-hierarchy.md § Path B-sibling-2) — public routing
        // metadata = the user's own data, per the "user controls their
        // data / nest config is set from clients" invariant. The earlier
        // Admin-only gate was a
        // placeholder before the user-self-registration path existed.
        "fauna.bridges.provision_recipient_mls_pubkey" => matches!(class, User | Admin),
        "fauna.bridges.revoke_wrapped_mls_blob" => matches!(class, User | Admin),
        "fauna.bridges.revoke_wrapped_submission_token" => matches!(class, User | Admin),
        // Self-attestation of a bridge's own x25519 (the handler enforces
        // `ed25519_pubkey == connected actor`, so no cross-row surface).
        // `ContentProcessor` included (2026-07-17): an approved external
        // content-processor (a future scorer/indexer) must be able to attest
        // its seal target exactly like the mail roles, or it can never become
        // a discoverable capability holder — the enrollment-side twin of the
        // `list_service_users` User relax below (same roster, same threat
        // model). Today's only ContentProcessor (the in-process web-serve
        // holder) writes the DB directly and never dials in, so this arm has
        // no live caller yet — it closes the wire gap for the first external
        // one.
        "fauna.bridges.register_service_user" => {
            matches!(
                class,
                BridgeMta | BridgeMda | ContentProcessor | BridgeAtprotoPds
            )
        }
        "fauna.bridges.report_auth_event" => matches!(class, BridgeMta | BridgeMda),
        // S2 identity surface (`atproto-pds-bridge.md` § State & data shape) —
        // the atproto.pds bridge's own roster/keys/mint-report kinds, no other
        // caller class — plus the S3 projection reads (`atproto-pds-bridge.md`
        // § Where logic lives): the public-post/tombstone stream page and the
        // profile read the bridge's projection loop consumes. All
        // bridge-facing with identical caller semantics, so one arm.
        "fauna.bridges.atproto.fetch_identities"
        | "fauna.bridges.atproto.fetch_identity_key_blob"
        | "fauna.bridges.atproto.fetch_session_secret_blob"
        | "fauna.bridges.atproto.fetch_issuer_jwks"
        | "fauna.bridges.atproto.record_minted_identity"
        | "fauna.bridges.atproto.fetch_public_posts"
        | "fauna.bridges.atproto.fetch_profile" => {
            matches!(class, BridgeAtprotoPds)
        }
        // (The one-shot `enable_identity` kind was retired in S4-B — the depth
        // selector's `set_integration_level`, gated above, is the only
        // enable/level mutation path.)
        // Role-discovery RPC — must be callable regardless of resolved
        // role since this is the call that resolves the role on the
        // bridge side. Listed for both MTA and MDA; subsequent kinds
        // flow through the regular per-role gate.
        "fauna.bridges.whoami" => matches!(class, BridgeMta | BridgeMda | BridgeAtprotoPds),
        // I2b routing/data plane. The MTA calls it at RCPT TO; the MDA calls it
        // at MUA-AUTH to resolve the AUTH username → actor_id before fetching
        // that actor's wrapped-MSEK blob (`internal/mda/imap/auth.go` resolveActor
        // + `caldav/auth.go`). Both bridge roles legitimately resolve a local
        // recipient address, so both are permitted.
        "fauna.bridges.validate_recipient" => matches!(class, BridgeMta | BridgeMda),
        // The fixed-order RCPT-TO resolver:
        // the richer superset of `validate_recipient` (all alias kinds,
        // per-alias controls, header stamping). Runs at the MTA's RCPT TO
        // stage AND in the MDA's server-side auto-schedule classifier
        // (`internal/mda/caldav/autoschedule.go::classifyAutoScheduleRecipients`
        // — a local-domain attendee that Resolves/Forwards rides the email
        // rail; a Reject falls through to the mailbox-less sealed rail), so —
        // exactly like `validate_recipient` above — BOTH bridge roles
        // legitimately resolve a local recipient and both are permitted. (C4
        // wired the MDA classifier but left this MTA-only; the tier_3 e2e
        // `test_caldav_autoschedule_mailbox_less.py` caught the gap — the Go
        // classifier unit tests mock nest, so the real allowlist was never
        // exercised.) See `docs/goal/behavior/mail-aliases.md` § Resolution
        // order + `caldav-server.md` § Server-side auto-schedule.
        "fauna.bridges.resolve_recipient" => matches!(class, BridgeMta | BridgeMda),
        // Nest-side greylist check — MTA at RCPT TO
        // after `validate_recipient`. State lives in `greylist_tuples` so it
        // is uniform across bridge restart (`smtp-server.md` § Greylisting).
        "fauna.bridges.check_greylist" => matches!(class, BridgeMta),
        // Per-account alias user surface.
        // User-class: a logged-in actor manages their *own* aliases (the
        // handler derives the owning actor from the caller and scopes
        // every write by it). The only alias write there is — an admin
        // neither sees nor edits another member's aliases. See
        // `docs/goal/behavior/mail-aliases.md` § Wire shapes (all seven
        // client-facing alias RPCs are "user client" caller).
        "fauna.bridges.list_account_aliases" => matches!(class, User),
        "fauna.bridges.create_account_alias" => matches!(class, User),
        "fauna.bridges.import_account_aliases" => matches!(class, User),
        "fauna.bridges.update_account_alias" => matches!(class, User),
        "fauna.bridges.revoke_account_alias" => matches!(class, User),
        "fauna.bridges.enable_account_alias" => matches!(class, User),
        "fauna.bridges.delete_account_alias" => matches!(class, User),
        // Mailbox-migration import surface (`mailbox-migration.md` § Per-
        // message flow / § Progress lives nest-side). User-class: the importer
        // is the user's own Fauna app submitting into its own mailbox —
        // every handler derives the owning actor from the caller. Deliberately
        // distinct kinds from the BridgeMda-only `append` (same write path,
        // different caller class); the session-lifecycle family mirrors
        // `mail-export.md` § RPC table naming.
        "fauna.bridges.start_import_session" => matches!(class, User),
        "fauna.bridges.import_message" => matches!(class, User),
        "fauna.bridges.import_message_batch" => matches!(class, User),
        "fauna.bridges.list_import_sessions" => matches!(class, User),
        "fauna.bridges.pause_import_session" => matches!(class, User),
        "fauna.bridges.resume_import_session" => matches!(class, User),
        "fauna.bridges.cancel_import_session" => matches!(class, User),
        "fauna.bridges.finalize_import_session" => matches!(class, User),
        "fauna.bridges.fail_import_session" => matches!(class, User),
        // Mailbox-export surface (`mail-export.md` § Wire shapes) — User-class
        // and caller-scoped, the read-out twin of the import block above.
        // § Cross-actor isolation is why none of these is reachable by a
        // bridge class: an export is the user reading their own mail, and no
        // other principal may initiate, drive or download one.
        //
        // `list_own_mailboxes` is a NEW kind rather than a widening of the
        // BridgeMda-only `fauna.bridges.list_mailboxes` below: an MDA calls
        // that one while acting for a credential whose actor it was handed, so
        // it takes a target actor, while this one derives the owner from the
        // authenticated caller and offers no way to name another. Putting both
        // derivation paths behind one name is precisely what this file already
        // refused for `import_message` vs `append`.
        "fauna.bridges.list_own_mailboxes" => matches!(class, User),
        "fauna.bridges.start_export_session" => matches!(class, User),
        "fauna.bridges.list_export_sessions" => matches!(class, User),
        "fauna.bridges.fetch_export_chunk_ciphertext" => matches!(class, User),
        "fauna.bridges.upload_export_chunk" => matches!(class, User),
        "fauna.bridges.pause_export_session" => matches!(class, User),
        "fauna.bridges.resume_export_session" => matches!(class, User),
        "fauna.bridges.restart_export_session" => matches!(class, User),
        "fauna.bridges.cancel_export_session" => matches!(class, User),
        "fauna.bridges.finalize_export_session" => matches!(class, User),
        "fauna.bridges.fail_export_session" => matches!(class, User),
        "fauna.bridges.discard_export_blob" => matches!(class, User),
        // Disposable mint. User-class: the
        // owning actor is the authenticated caller. See `mail-aliases.md`
        // § Kind 5 + § Wire shapes (`generate_disposable_alias` is "user
        // client" caller).
        "fauna.bridges.generate_disposable_alias" => matches!(class, User),
        // Mailing lists (`mail-mass-mailing.md` § Wire shapes, #10a).
        // User-class: a list is per-user —
        // each handler derives the owning actor from the caller and scopes every
        // read/write by `owner_actor_id = caller` (the member sub-surface first
        // verifies ownership via `get_list_for_owner`). The Admin
        // `rotate_list_unsubscribe_secret` (#11) is the lone admin-class list
        // RPC and lands with its consuming item.
        "fauna.bridges.list_account_lists" => matches!(class, User),
        "fauna.bridges.create_account_list" => matches!(class, User),
        "fauna.bridges.update_account_list" => matches!(class, User),
        "fauna.bridges.delete_account_list" => matches!(class, User),
        "fauna.bridges.list_list_members" => matches!(class, User),
        "fauna.bridges.add_list_member" => matches!(class, User),
        "fauna.bridges.batch_import_list_members" => matches!(class, User),
        "fauna.bridges.unsubscribe_list_member" => matches!(class, User),
        "fauna.bridges.resubscribe_list_member" => matches!(class, User),
        // The canonical list-send fan-out + its per-list send audit (#10b).
        // User-class: the handler derives the owner from the caller and scopes
        // to a caller-owned list (else not_found). `send_list_message` is the
        // ONLY list-send path (an external SMTP submission with MAIL FROM = a
        // list address is rejected at enqueue — per-recipient RFC 8058 stamping
        // is structurally impossible for a single-body submission).
        "fauna.bridges.send_list_message" => matches!(class, User),
        "fauna.bridges.list_list_send_history" => matches!(class, User),
        // The lone admin-class list RPC: rotate the deployment-wide
        // one-click-unsubscribe secret + re-tokenize every member (#11). Admin
        // because it is a deployment-wide security action, not a per-list edit.
        "fauna.bridges.rotate_list_unsubscribe_secret" => matches!(class, Admin),
        // Per-alias hit audit list. User-class:
        // the handler verifies the caller owns `alias_id` (else not_found), so
        // a user reads only their own hits. See `mail-aliases.md` § Wire shapes
        // (`list_account_alias_hits` is "user client" caller).
        "fauna.bridges.list_account_alias_hits" => matches!(class, User),
        // Admin external forwarders (mail-aliases.md
        // § Kind 7 / § Wire shapes — all three are "admin client" caller).
        // Admin-class: forwarders are deployment routing config attributed to
        // the managing admin, NOT a user's personal aliases (excluded from
        // `list_account_aliases`).
        "fauna.bridges.create_forwarder" => matches!(class, Admin),
        "fauna.bridges.list_forwarders" => matches!(class, Admin),
        "fauna.bridges.delete_forwarder" => matches!(class, Admin),
        // Per-account forward-all knob.
        // User-class: the handler derives the owning actor from the caller and
        // reads/writes only that actor's `mail_account_settings` row. See
        // `mail-forwarding.md` § Per-account "forward all".
        "fauna.bridges.get_forward_all_to" => matches!(class, User),
        "fauna.bridges.set_forward_all_to" => matches!(class, User),
        "fauna.bridges.get_forward_per_hour" => matches!(class, User),
        "fauna.bridges.set_forward_per_hour" => matches!(class, User),
        "fauna.bridges.get_spam_threshold_override" => matches!(class, User),
        "fauna.bridges.set_spam_threshold_override" => matches!(class, User),
        // Per-rail draft persistence: any
        // authenticated actor persists/reads only their own drafts (the nest
        // derives the owning actor from the connection, neither kind carries an
        // actor_id). See `docs/goal/behavior/file-sync.md` § Drafts Sync.
        "fauna.drafts.get" => matches!(class, User | Admin),
        "fauna.drafts.put" => matches!(class, User | Admin),
        // The `__index` content-index rail, USER plane — same self-scoped
        // posture again (owning actor from the connection; opaque sealed blobs
        // the nest can never read). `User | Admin` and never a bridge: these
        // kinds carry no `actor_id`, so a bridge admitted here could only ever
        // write its OWN rail. The MDA's reach is the bridge plane below. See
        // `docs/goal/behavior/content-index.md` § Ingest triggers, v1.
        "fauna.index.record" => matches!(class, User | Admin),
        "fauna.index.list" => matches!(class, User | Admin),
        // Cross-device MLS state replica — same self-scoped posture as
        // `__drafts` (owning actor from the connection; opaque
        // BackupKey-sealed blobs; CAS on put). See
        // `docs/goal/behavior/file-sync.md` § MLS state replica.
        "fauna.mls.get" => matches!(class, User | Admin),
        "fauna.mls.put" => matches!(class, User | Admin),
        // Task-delegation heartbeat lease — self-scoped (the owning actor is
        // the connection actor; all participants are that user's own devices),
        // advisory in-memory state. `User | Admin` — the human running any of
        // their clients contends for/reads the lease; never a bridge/MDA
        // service user. See `docs/goal/behavior/participants.md`
        // § Coordination primitive.
        "fauna.delegation.heartbeat" => matches!(class, User | Admin),
        "fauna.delegation.observe" => matches!(class, User | Admin),
        // A3 Bucket B — admin write path for the `fetch_config` policy
        // sub-structs: one Admin-only kind per `FetchConfigReply`
        // sub-struct, each a single-row upsert overlaid back onto the
        // catalog default. (Was the single `put_mail_policy` DNS-perimeter
        // slice.) `mail.enabled` +
        // Bucket C (DMARC/TLS/scanning/per-account) remain deferred.
        // Deliverability diagnostics + blocklist self-check are admin-pane-only
        // (mail-deliverability.md § Architectural rules: "Symptom diagnostic is
        // admin-pane-only (no API for users)"; § Wire shapes names the
        // admin-client caller). NOT MTA-class.
        "fauna.bridges.run_deliverability_diagnostics" => matches!(class, Admin),
        "fauna.bridges.blocklist_self_check_run" => matches!(class, Admin),
        // Fresh-IP warm-up status/reset (mail-deliverability.md § Manual reset:
        // "Warmup reset is an admin action"; the deployment-wide outbound IP
        // reputation is an admin concern, not per-user). Admin-only.
        "fauna.bridges.outbound_warmup_status" => matches!(class, Admin),
        "fauna.bridges.outbound_warmup_reset" => matches!(class, Admin),
        // The mail health readout (mail-deliverability.md § The mail health
        // readout): a deployment-wide fold over the self-check, diagnostics,
        // queue, warm-up and bridge-connection facts — admin-pane-only, like
        // the kinds it reads.
        "fauna.bridges.mail_health" => matches!(class, Admin),
        // Deliverability history/observability reads (mail-deliverability.md
        // § Admin-visible audit: diagnostic history is admin-only → 403
        // for non-admin). Admin-pane-only, same as the run/check kinds above.
        "fauna.bridges.list_blocklist_self_check_history" => matches!(class, Admin),
        "fauna.bridges.list_deliverability_diagnostic_runs" => matches!(class, Admin),
        "fauna.bridges.put_spam_policy" => matches!(class, Admin),
        "fauna.bridges.put_auth_policy" => matches!(class, Admin),
        "fauna.bridges.put_submission_policy" => matches!(class, Admin),
        "fauna.bridges.put_imap_policy" => matches!(class, Admin),
        "fauna.bridges.put_outbound_policy" => matches!(class, Admin),
        // Nest-side alias-policy knobs (NOT projected to the bridge — read
        // by the alias resolver + alias CRUD): exact_aliases_max,
        // reserved_local_parts, subaddressing_enabled,
        // wildcard_prefix_enabled.
        "fauna.bridges.put_alias_policy" => matches!(class, Admin),
        // Admin read twin of `put_alias_policy` — the `admin-mail` form reads
        // the effective alias policy to hydrate before edit (these knobs are
        // not in `FetchConfigReply`, so they have their own read kind).
        "fauna.bridges.get_alias_policy" => matches!(class, Admin),
        // Nest-OWN transport/abuse policy (NOT a mail policy, NOT projected to
        // the bridge): the per-source-IP concurrent-connection cap on nest's
        // own client-facing TLS listener, read nest-side by `serve_tls`.
        // Admin-only put/get — a deployment-wide abuse knob (same class as
        // spam thresholds), client-set per the product invariant. See
        // `docs/goal/architecture/transport-connection.md` § Abuse posture item (2).
        "fauna.transport.put_policy" => matches!(class, Admin),
        "fauna.transport.get_policy" => matches!(class, Admin),
        // Local-domain admin (multidomain): the admin client drives the
        // `mail_domains` table over WS-RPC (no HTTP). Admin-only writes;
        // `list` is Admin-only too (the bridge reads its domain list from
        // `fetch_config`, not these). See
        // `docs/goal/behavior/mail-multidomain.md` § Wire shapes.
        "fauna.bridges.add_local_domain" => matches!(class, Admin),
        "fauna.bridges.remove_local_domain" => matches!(class, Admin),
        "fauna.bridges.restore_local_domain" => matches!(class, Admin),
        "fauna.bridges.list_local_domains" => matches!(class, Admin),
        "fauna.bridges.update_local_domain_config" => matches!(class, Admin),
        // Primary-domain rename (mail-primary-domain-rename.md § Wire shapes).
        "fauna.bridges.start_primary_domain_rename" => matches!(class, Admin),
        "fauna.bridges.get_primary_domain_rename_status" => matches!(class, Admin),
        "fauna.bridges.list_primary_domain_renames" => matches!(class, Admin),
        "fauna.bridges.abort_primary_domain_rename" => matches!(class, Admin),
        "fauna.bridges.complete_primary_domain_rename" => matches!(class, Admin),
        "fauna.bridges.extend_primary_domain_rename_grace" => matches!(class, Admin),
        "fauna.bridges.set_catch_all_actor" => matches!(class, Admin),
        // Per-domain role-address override (postmaster/abuse/noc/security target
        // actor); `mail-multidomain.md` § Per-domain role-address routing.
        "fauna.bridges.set_role_address" => matches!(class, Admin),
        // Per-domain DKIM rotation interval — the accelerated-rotation operator
        // lever; `mail-multidomain.md` § Selector.
        "fauna.bridges.set_dkim_rotation_days" => matches!(class, Admin),
        // Emergency DKIM rotation: flip a domain's active selector to its
        // newest-provisioned one; `mail-multidomain.md` § Rotation.
        "fauna.bridges.force_rotate_dkim" => matches!(class, Admin),
        // Admin-synthesized self-signed TLS cert (the WS-RPC twin of the HTTP
        // `POST …/{domain}/self_signed_cert`); seals a TlsCertBlob to approved
        // bridges. `mail-bridge-lifecycle.md` § TLS provisioning.
        "fauna.bridges.provision_self_signed_cert" => matches!(class, Admin),
        // Switch the nest's own TLS listener back to a real (CA-issued) cert
        // after a self-signed override — restores the preserved real cert or
        // triggers an ACME re-issue. `mail-bridge-lifecycle.md` § TLS provisioning.
        "fauna.bridges.restore_real_tls_cert" => matches!(class, Admin),
        // Nest TLS-cert lifecycle (`fauna.tls.*`, Phase 3) — the
        // **producer** seam for client-driven DNS-01 issuance: the admin's client
        // obtains a publicly-trusted cert via a DNS-01 ACME order it runs itself
        // (no nest holds the DNS-provider key), seals it, and delivers the sealed
        // entry here; the nest stores it under the caller's own namespace +
        // `LAN_TLS_CERT_ENTRY_ID` for namespace-sync to a paired private nest
        // (`apply_synced_lan_cert`). Admin-only — the deployment's TLS cert is an
        // admin concern, never a user/bridge one. See
        // `docs/goal/architecture/nest/tls-certificates.md` § B.
        "fauna.tls.publish_cert" => matches!(class, Admin),
        // Nest TLS-cert lifecycle (`fauna.tls.*`, Phase 4) — the
        // **read** surface for the `admin-dns` cert-status row: report the
        // health (`valid-trusted` / `on-floor — renew needed` / `expiring`) of
        // the cert the listener currently serves per domain. Admin-only — same
        // admin-concern scoping as `publish_cert`; read-only (no mutation).
        // `docs/goal/architecture/nest/tls-certificates.md` § C.4.
        "fauna.tls.cert_status" => matches!(class, Admin),
        // The nest-held OAuth issuer key set (`fauna.oauth.*`, TP5 slice S1).
        // Admin-only for the `rotate_srs_secret` / `force_rotate_dkim` reason:
        // this is deployment crypto material. Rotation is the deliberate
        // compromise response `authorization-server.md` § As built rules it is —
        // there is no scheduled rotation — and the status read is its other
        // half, so an admin can see the outgoing key still being served.
        // Notably NOT bridge-callable: the key is the nest's own, and the
        // surface answers on a nest that compiles no bridge at all.
        "fauna.oauth.issuer_key_status" => matches!(class, Admin),
        "fauna.oauth.rotate_issuer_key" => matches!(class, Admin),
        // The forced arm — drop the outgoing key(s) now, horizon skipped — is
        // the actual compromise response (`authorization-server.md` § The
        // issuer → *Two rotation arms*); Admin-only for the same reason as the
        // ordinary arm, and it breaks every live token, so never wider.
        "fauna.oauth.force_rotate_issuer_key" => matches!(class, Admin),
        // The second signer's forced arm — re-mint the refresh-token secret,
        // killing every outstanding OAuth refresh token — completes that
        // compromise response; same class, same reason, never wider.
        "fauna.oauth.force_rotate_session_secret" => matches!(class, Admin),
        // The third-party principal roster (`third-party.md` § The principal
        // model). USER class and self-scoped — the actor is the authenticated
        // connection's, never a request field — because a principal is minted
        // only by the account's OWN consent (rule 1), so only that account
        // lists or ends it. Never bridge-callable, and never callable by a
        // principal itself: a third party cannot enumerate or revoke its
        // siblings.
        "fauna.principals.list" | "fauna.principals.revoke" => matches!(class, User),
        // The events doors' poll (`transport.md` § Push events → *Third-party
        // event doors*). ThirdParty ONLY — the account's own apps hear the
        // unfiltered nudges on their own sockets; the principal handler
        // answers only for scopes `fauna_scope::event_reaches` admits.
        "fauna.events.poll" => matches!(class, ThirdParty),
        // Nest-hosted plugins (`third-party.md` § The principal model →
        // *Hosted principals*). ADMIN-only: an install is the nest's act and no
        // account's, its install row is the nest owner's, and running third-
        // party code on the box is a deployment decision. Never bridge-
        // callable, and never callable by a principal — a plugin cannot
        // install, list or uninstall plugins.
        "fauna.plugins.install" | "fauna.plugins.list" | "fauna.plugins.uninstall" => {
            matches!(class, Admin)
        }
        // The consent starts' user half (`authorization-server.md` § Consent):
        // self-scoped acts on the caller's own consent rows and blocks, User
        // exactly as `resolve_consent` is — the approval arriving over the
        // caller's own authed connection IS the ceremony's root. Never
        // bridge-callable: a bridge that could claim a typed code could approve
        // a device on a user's behalf.
        "fauna.oauth.consent.lookup_code"
        | "fauna.oauth.consent.open_handoff"
        | "fauna.oauth.consent.block_client"
        | "fauna.oauth.consent.list_blocked_clients" => matches!(class, User),
        // I5 Phase D.5: both pubkey-fetch RPCs are public-key reads; the
        // MTA reads recipient keys to seal inbound mail, the MDA reads
        // the AUTH'd actor's own keys for APPEND seal-to-self.  Caller-
        // class scoping is for routing, not privacy — these are public
        // keys, intentionally widely shareable.
        // `ThirdParty` since Phase G (`apps/bridges.md` § Bridge-kind
        // catalogue): a conversation bridge seals each deposit to the user's
        // recipient key — its own account's only (the principal handler).
        "fauna.bridges.fetch_recipient_mls_pubkey" => {
            matches!(class, BridgeMta | BridgeMda | ThirdParty)
        }
        // Phase G — the bridged-conversation family. The bridge's six are
        // `ThirdParty` only; the user's four are `User` and caller-scoped.
        "fauna.bridges.conversation.deposit" => matches!(class, ThirdParty),
        "fauna.bridges.conversation.outbox.fetch" => matches!(class, ThirdParty),
        "fauna.bridges.conversation.outbox.ack" => matches!(class, ThirdParty),
        "fauna.bridges.conversation.room.upsert" => matches!(class, ThirdParty),
        "fauna.bridges.conversation.room.members" => matches!(class, ThirdParty),
        "fauna.bridges.conversation.receipt" => matches!(class, ThirdParty),
        "fauna.bridges.conversation.rooms.list" => matches!(class, User),
        "fauna.bridges.conversation.rooms.open" => matches!(class, User),
        "fauna.bridges.conversation.inbox.fetch" => matches!(class, User),
        "fauna.bridges.conversation.send" => matches!(class, User),
        "fauna.bridges.fetch_recipient_index_key" => matches!(class, BridgeMta | BridgeMda),
        // The PDS bridge fetches config on the same lifecycle cadence (boot +
        // reconnect + `config_changed`); S1 has no atproto-specific knobs yet, so
        // it reads the shared snapshot and ignores the mail fields. A scoped
        // atproto config view is an S3/S4 refinement.
        "fauna.bridges.fetch_config" => {
            matches!(class, BridgeMta | BridgeMda | BridgeAtprotoPds)
        }
        // Admin read twin of `fetch_config`: the admin client reads the same
        // overlaid effective mail config (catalog defaults + `put_<substruct>
        // _policy` overrides) to hydrate the `admin-mail` policy form before
        // edit. Read-only; Admin-only. See
        // `docs/goal/behavior/mail-policy-config.md` § Implementation status today.
        "fauna.bridges.get_mail_config" => matches!(class, Admin),
        "fauna.bridges.report_session_close" => matches!(class, BridgeMta | BridgeMda),
        "fauna.bridges.check_submission_quota" => matches!(class, BridgeMta),
        "fauna.bridges.ingest_inbound_mail" => matches!(class, BridgeMta),
        "fauna.bridges.submit_inbound_mail" => matches!(class, BridgeMta),
        // T1.4 — forensic row for a reject-at-perimeter ClamAV hit. MTA-class:
        // the reject happens at the inbound DATA stage, the same caller that
        // runs `ingest_inbound_mail`. Metadata only (no message bytes). See
        // `docs/goal/behavior/mail-content-scanning.md` § Actions.
        "fauna.bridges.report_rejected_scan" => matches!(class, BridgeMta),
        // I4 Phase D.5 — outbound-queue (MTA worker polls + reports back).
        "fauna.bridges.fetch_outbound_due" => matches!(class, BridgeMta),
        "fauna.bridges.mark_outbound_delivered" => matches!(class, BridgeMta),
        "fauna.bridges.mark_outbound_failed" => matches!(class, BridgeMta),
        "fauna.bridges.mark_outbound_bounced" => matches!(class, BridgeMta),
        // Outbound enqueue: the MTA (submission gateway, sender-unconstrained)
        // and the MDA (server-side `calendar-auto-schedule` gateway, caller-
        // scoped to the AUTH'd organizer in `enqueue_outbound_mail_handler`).
        // The MDA's grant is strictly narrower — it may only enqueue as the one
        // organizer whose encrypted PUT session it is inside (caldav-server.md
        // § Server-side auto-schedule; same `target == actor` pattern as
        // `require_caller_scope` for the calendar storage RPCs).
        "fauna.bridges.enqueue_outbound_mail" => matches!(class, BridgeMta | BridgeMda),
        // The MDA mailbox-less auto-schedule rail (caldav-server.md § Server-side
        // auto-schedule, C3). BridgeMda **only** — never the MTA (no mail leg),
        // never User (this is the server-side gateway, distinct from the
        // User-class `fauna.conversations.welcome.deliver` / `channel.send` it
        // reuses internally). Caller-scoped to the AUTH'd organizer in
        // `deliver_sealed_scheduling_handler` (the same `on_behalf_of_actor` +
        // `original_sender`-resolves-to-it pattern as `enqueue_outbound_mail`).
        "fauna.bridges.deliver_sealed_scheduling" => matches!(class, BridgeMda),
        // N2 forward delivery trigger: the MTA reads a recipient's forward
        // config at the perimeter and enqueues a forward after local delivery
        // commits. Both MTA-class (same caller as `ingest_inbound_mail`). See
        // `docs/goal/behavior/mail-forwarding.md` § Storage-mode interaction.
        "fauna.bridges.fetch_recipient_forward_config" => matches!(class, BridgeMta),
        "fauna.bridges.forward_message" => matches!(class, BridgeMta),
        // AutoReply (Sieve vacation): after the perimeter loop guard passes, the
        // MTA composes + signs the reply and hands it here; nest claims the
        // per-(recipient,sender) rate-limit slot and enqueues it null-sender
        // (`smtp-server.md` § Email filter rules).
        "fauna.bridges.send_auto_reply" => matches!(class, BridgeMta),
        // T3.3 — the MTA reads a recipient's stored filter rules at the
        // perimeter so the pure `fauna_mail::filter::evaluate` engine can run
        // pre-seal on plaintext (`smtp-server.md` § Email filter rules;
        // `mail-forwarding.md` § Where rule eval runs). MTA-class.
        "fauna.bridges.fetch_recipient_filters" => matches!(class, BridgeMta),
        // N3 — decode + verify an inbound SRS0=/SRS1= bounce recipient at
        // RCPT-TO so N4 can route the NDR to the forwarder. MTA-class.
        "fauna.bridges.decode_srs_bounce" => matches!(class, BridgeMta),
        // N5 — admin rotates the per-deployment SRS secret (2-secret overlap).
        // Admin-class: deployment crypto material, admin-triggered; nest
        // mints the bytes and never returns them (mail-forwarding.md:279).
        "fauna.bridges.rotate_srs_secret" => matches!(class, Admin),
        // T2.1a — MTA-STS policy fetch (nest fetches + caches, MTA enforces).
        "fauna.bridges.fetch_mta_sts_policy" => matches!(class, BridgeMta),
        // T2.1b — DANE/TLSA fetch (nest does the DNSSEC lookup, MTA pins).
        "fauna.bridges.fetch_tlsa" => matches!(class, BridgeMta),
        // MX resolution with DNSSEC provenance. Same split, same
        // reason: the Go stdlib can't validate, and DANE may only bind to a
        // name that came out of a validated MX RRset (RFC 7672 §2.2).
        "fauna.bridges.resolve_mx" => matches!(class, BridgeMta),
        // T2.4 — per-attempt TLS outcome report (MTA reports, nest buckets).
        "fauna.bridges.report_tls_attempt" => matches!(class, BridgeMta),
        "fauna.bridges.list_mailboxes" => matches!(class, BridgeMda),
        "fauna.bridges.select_mailbox" => matches!(class, BridgeMda),
        // C.2 — message-metadata fetch.
        "fauna.bridges.list_messages" => matches!(class, BridgeMda),
        "fauna.bridges.fetch_message_metadata" => matches!(class, BridgeMda),
        // C.3 — body-fetch + index-segments.
        "fauna.bridges.fetch_message_ciphertext" => matches!(class, BridgeMda),
        "fauna.bridges.fetch_index_segments_since" => matches!(class, BridgeMda),
        // The `__index` content-index rail, BRIDGE plane (rollout slice S5) —
        // the MDA's reach as a ratified index *builder* during an active
        // MUA-AUTH session (`content-index.md` § Where the index is built).
        // Unlike the user plane these name their target actor explicitly, the
        // shape every MDA-on-behalf-of-user kind uses. `BridgeMda` alone: the
        // MDA is the one ratified non-client builder position, and a `User`
        // must never reach a kind carrying someone else's `actor_id`. The
        // handlers additionally bound the reach to the mail/calendar key class
        // — rule #7's blast-radius invariant at the rail layer.
        "fauna.bridges.index_list" => matches!(class, BridgeMda),
        // C.4 — flag-store + expunge.
        "fauna.bridges.store_flags" => matches!(class, BridgeMda),
        "fauna.bridges.expunge" => matches!(class, BridgeMda),
        // C.5 — copy + move.
        "fauna.bridges.copy" => matches!(class, BridgeMda),
        "fauna.bridges.move" => matches!(class, BridgeMda),
        // C.6 — append.
        "fauna.bridges.append" => matches!(class, BridgeMda),
        // C.7 — search_messages.
        "fauna.bridges.search_messages" => matches!(class, BridgeMda),
        // C.8 — get_quota.
        "fauna.bridges.get_quota" => matches!(class, BridgeMda),
        // I2b Phase-D CalDAV r/w + provisioning. `BridgeMda | User`: the MDA
        // serves these on behalf of an AUTH'd MUA, AND — per events.md Decision B
        // (2026-06-01) — a Fauna app reads/writes its OWN calendars + events
        // directly over the same RPCs (sealing client-side; the legacy plaintext
        // `fauna.{calendars,events}.*` / `content`-table path is retired). Every
        // non-MDA caller is caller-scoped in the handler (`target == actor_id`),
        // the same load-bearing invariant `provision_calendar` already enforces,
        // so a user/admin can only touch their own data and nest never sees
        // plaintext (caldav-server.md § Threat model). Admin ⊇ User via the
        // promotion at the top of this fn.
        "fauna.bridges.provision_calendar" => matches!(class, BridgeMda | User),
        "fauna.bridges.list_calendars" => matches!(class, BridgeMda | User),
        "fauna.bridges.query_events" => matches!(class, BridgeMda | User),
        // D.4 — put_event_ciphertext.
        "fauna.bridges.put_event_ciphertext" => matches!(class, BridgeMda | User),
        // D.5 — delete_event.
        "fauna.bridges.delete_event" => matches!(class, BridgeMda | User),
        // D.6 — sync_calendar_since.
        "fauna.bridges.sync_calendar_since" => matches!(class, BridgeMda | User),
        // The inbound half of the auto-schedule gateway (caldav-server.md §
        // Server-side auto-schedule, "Inbound invite"): the MTA — the only
        // process holding an externally-sent invitation in cleartext — places
        // it, sealed to the recipient, on their calendar. Create-only and
        // encrypt-only, so it grants the MTA no read of anyone's calendar.
        "fauna.bridges.place_inbound_invite" => matches!(class, BridgeMta),
        // Phase-E CardDAV r/w + provisioning — the contacts twin of the Phase-D
        // CalDAV arms above (same CardDAV server design; tracked internally).
        // Same `BridgeMda | User` gate + same handler-layer caller-scoping
        // (`target == actor_id`): the MDA serves generic CardDAV clients, and a
        // Fauna app reads/writes its OWN address books + cards directly over
        // the same RPCs (sealing client-side). nest never sees plaintext vCards.
        "fauna.bridges.provision_addressbook" => matches!(class, BridgeMda | User),
        "fauna.bridges.list_addressbooks" => matches!(class, BridgeMda | User),
        "fauna.bridges.query_cards" => matches!(class, BridgeMda | User),
        // E.4 — put_card_ciphertext.
        "fauna.bridges.put_card_ciphertext" => matches!(class, BridgeMda | User),
        // E.5 — delete_card.
        "fauna.bridges.delete_card" => matches!(class, BridgeMda | User),
        // E.6 — sync_addressbook_since.
        "fauna.bridges.sync_addressbook_since" => matches!(class, BridgeMda | User),
        // E.7 — delete_addressbook (whole-book delete + card cascade). Same
        // `BridgeMda | User` gate + handler-layer caller-scoping as its siblings;
        // a genuinely user-initiated destructive action (WebDAV DELETE on the
        // collection URL).
        "fauna.bridges.delete_addressbook" => matches!(class, BridgeMda | User),
        // Slice 4 item 1 — fetch the actor's per-user spam model sealed to
        // their MSEK-derived key, for on-device scoring at the search-
        // equivalent position (mail-spam.md § Scoring placement). The MDA
        // (server-side, on an AUTH'd session) scores + re-files Junk; the
        // User/Fauna-app leg (the post-decrypt client scorer, e.g. the
        // android per-user posts scorer) fetches its own model to score
        // on-device.
        //
        // `User`/`Admin` are **caller-scoped to their own model** by the
        // handler (`target == caller`, `bridge_imap_handlers.rs`
        // `fetch_spam_model_handler`) — even an admin cannot read another
        // user's model (mail-spam.md § Cross-actor isolation). `BridgeMda`
        // keeps trusted-naming (it serves an actor it didn't authenticate as).
        // The content-reconstruction oracle that previously kept this MDA-only
        // (a User-facing model readback + a no-authz `train(arbitrary
        // content_id)`) is closed: `fauna.moderation.train` no longer trains
        // and gates on the caller being able to read the post, and this fetch is
        // caller-scoped. Both findings are from security reviews
        // tracked internally.
        "fauna.bridges.fetch_spam_model" => matches!(class, BridgeMda | User | Admin),
        // The opaque sealed-write-back twin of `fetch_spam_model` (the tier-1
        // spam-model at-rest sealing end-game — mail-spam.md § Wire shapes /
        // § Encrypted-mode interaction). Two writer paths, ONE RPC (priority
        // #1/#3), matching the read twin's class set:
        //   - `User`/`Admin` — the CLIENT's own-key write: it unwraps its sealed
        //     model, mutates locally, re-seals, and writes the opaque blob back.
        //     Caller-scoped **by construction** (`put_spam_model_handler` ignores
        //     `req.actor_id` for this class — the connection's actor IS the
        //     subject; even an admin writes only their own, § Cross-actor
        //     isolation).
        //   - `BridgeMda` — the MDA agent-side `\Junk`-train re-seal: it opens
        //     the served actor's sealed model under its session MLS capability
        //     (or starts from an empty one), applies the delta, re-seals, and
        //     writes it back naming the served actor in `req.actor_id`
        //     (trusted-naming, the same model as `fetch_spam_model`, bounded by
        //     `require_local_mail_serving`). The one training entry point.
        // `BridgeMta` stays denied (the inbound-MX perimeter never writes a model).
        "fauna.bridges.put_spam_model" => matches!(class, BridgeMda | User | Admin),
        // The User-reachable read of the admin-effective spam-scoring policy
        // (`spam_folder` threshold + Bayesian knobs) for the on-device Fauna-
        // client scorer, so its INBOX→Junk line matches the MDA/nest byte-for-
        // byte (mail-spam.md § Scoring placement). Same class set as
        // `fetch_spam_model`: `User`/`Admin` (the on-device scorer leg) and
        // `BridgeMda` (server-side leg). NOT caller-scoped — the values are
        // deployment-wide, non-secret policy (the Admin-only `fetch_config`
        // twin is unreachable to a client).
        "fauna.bridges.get_spam_scoring_policy" => matches!(class, BridgeMda | User | Admin),
        // Slice 5 — publish_spam_baseline. Admin-only: the admin-opt-in
        // deployment baseline is a deployment-wide action (off by default),
        // reachable only from the flat `admin-mail` admin page
        // (`admin-mail-publish-spam-baseline-button`). Not caller-scoped — it
        // aggregates over all opt-in users (mail-spam.md § Cold start, Path 2).
        "fauna.bridges.publish_spam_baseline" => matches!(class, Admin),
        // The baseline's current state (`mail-spam.md` § Cold start Path 2 →
        // *Standing publish*): the admin page's "published over N contributors
        // on <date>" / "waiting for more contributor activity" text. Admin-only
        // and aggregate-only — no contributor id, no withdrawal time or reason.
        "fauna.bridges.get_spam_baseline_state" => matches!(class, Admin),
        // Slice 5 — set_baseline_contribution. The per-user opt-in toggle
        // (`mail-spam-contribute-baseline-toggle`). User/Admin set their OWN
        // contribution flag — caller-scoped by construction (the handler keys
        // on the connection's actor; there is no `actor_id` field), so even an
        // admin sets only their own (mail-spam.md § Cross-actor isolation). Not
        // BridgeMda: the MDA does not set user preferences.
        "fauna.bridges.set_baseline_contribution" => matches!(class, User | Admin),
        // Slice 5 rest — the user-tier training-management RPCs (mail-spam.md
        // §§ Reset, Training-sample retention). User/Admin manage their OWN
        // model + history — caller-scoped by construction (the handlers key on
        // the connection's actor; no `actor_id` field), so even an admin
        // touches only their own (mail-spam.md § Cross-actor isolation). Not
        // BridgeMda: the MDA trains (through `put_spam_model`) but does not
        // reset/list on a user's behalf — those are the user's own client
        // surfaces. An undo is the client's `put_spam_model` history `Delete`.
        "fauna.bridges.reset_spam_model" => matches!(class, User | Admin),
        "fauna.bridges.list_spam_training_history" => matches!(class, User | Admin),
        // I5 Phase D.6 — mailbox admin (CREATE / DELETE / RENAME). MDA-
        // only: only the IMAP-facing bridge serves the wire commands
        // these wrappers back. The MTA never issues mailbox-admin RPCs
        // (its surface is delivery-only); user/admin clients use the
        // first-party API surface instead, not the bridge channel.
        "fauna.bridges.create_mailbox" => matches!(class, BridgeMda),
        "fauna.bridges.delete_mailbox" => matches!(class, BridgeMda),
        "fauna.bridges.rename_mailbox" => matches!(class, BridgeMda),
        // I5 Phase D.7 — SUBSCRIBE / UNSUBSCRIBE. MDA-only on the same
        // reasoning: subscriptions back the IMAP LSUB / LIST (SUBSCRIBED)
        // commands which the MDA serves.
        "fauna.bridges.subscribe_mailbox" => matches!(class, BridgeMda),
        "fauna.bridges.unsubscribe_mailbox" => matches!(class, BridgeMda),
        // I5 Phase F.1 — IDLE / NOTIFY push subscription. MDA-only on
        // the same reasoning as subscribe_mailbox: the registration
        // backs the IMAP IDLE / NOTIFY wire commands which only the
        // MDA serves.
        "fauna.bridges.subscribe_mailbox_state" => matches!(class, BridgeMda),
        // Layer-3 Bridge Management user-facing surface (the bridges
        // page). Per T1b; arms expand as
        // per-endpoint slices land.
        "fauna.bridges.list" => matches!(class, User),
        // T2 — per-bridge settings overwrite + follow list. The bridges
        // page issues these per-actor; Admin has no per-user bridge
        // surface, MTAs/MDAs never call into the user-facing UI plane.
        "fauna.bridges.set_settings" => matches!(class, User),
        "fauna.bridges.list_follows" => matches!(class, User),
        // T3 — OAuth-flow start + unlink. User-only on the same
        // reasoning as set_settings; MTAs/MDAs never call the
        // user-facing link surface.
        "fauna.bridges.link" => matches!(class, User),
        // `link`'s first half (the proof-of-possession challenge) — the same
        // caller class as the link it precedes.
        "fauna.bridges.link_challenge" => matches!(class, User),
        "fauna.bridges.unlink" => matches!(class, User),
        // T4 — per-bridge follow add/remove. User-only on the same
        // reasoning as link/unlink; MTAs/MDAs never call the
        // user-facing follows surface.
        "fauna.bridges.add_follow" => matches!(class, User),
        "fauna.bridges.remove_follow" => matches!(class, User),
        // Follow requests — the account's own waiting followers, listed and
        // answered from its Bridges card. User-only and self-scoped exactly
        // as `list_follows` is.
        "fauna.bridges.list_follow_requests" => matches!(class, User),
        "fauna.bridges.resolve_follow_request" => matches!(class, User),
        // T5 — cross-bridge feed-subscription CRUD. User-only on the
        // same reasoning as set_settings/list_follows; subscriptions
        // are per-actor and admin/bridge actors never call this surface.
        "fauna.bridges.feeds.list" => matches!(class, User),
        "fauna.bridges.feeds.create" => matches!(class, User),
        "fauna.bridges.feeds.delete" => matches!(class, User),
        // T6 — user-facing email-filter CRUD (the sieve-like per-account
        // rule engine — see `smtp-server.md` § Email filter rules and
        // `mail-content-scanning.md`). Distinct top namespace from
        // `fauna.bridges.*` per the TODO's namespace decision; the
        // allowlist is unified though — caller-class scoping is the
        // privacy mechanism regardless of namespace. User-only because
        // filters are per-actor; admin/bridge actors have no business
        // touching another user's filter rules.
        "fauna.email.filters.list" => matches!(class, User),
        "fauna.email.filters.create" => matches!(class, User),
        "fauna.email.filters.get" => matches!(class, User),
        "fauna.email.filters.update" => matches!(class, User),
        "fauna.email.filters.delete" => matches!(class, User),
        // T7+T8 collapsed — user-facing outbound submission. The Go
        // mail bridge uses `fauna.bridges.enqueue_outbound_mail`
        // (Bridge-class) for post-DKIM submission; user clients use
        // this kind, which fires DKIM-via-MTA and the per-sender rate
        // limit. The (concurrently-deleted) HTTP twin was caller-class
        // unaware — this allowlist arm is the new, sharper gate.
        "fauna.email.send" => matches!(class, User),
        // The inbound twin of `fauna.email.send` — a caller reading its OWN
        // sealed INBOX (the client mail-receive feed). Caller-scoped by
        // construction (no `actor_id` param — always the connection actor),
        // which is exactly what lets an Admin caller inherit it safely
        // (admin ⊇ user, see the fn doc): it can only ever read its own
        // mailbox, never another user's. Bridge actors read mail via the
        // BridgeMda `fauna.bridges.{list_messages,fetch_message_ciphertext}`.
        // NIP-46 bunker control plane (`fauna.nostr.bunker.*`,
        // `docs/goal/ui/nostr.md` § The nest as the user's NIP-46 signer):
        // the user's own connected-apps roster — mint an invite, list,
        // revoke, label. User-only, caller-scoped on the connection actor
        // exactly like the email cluster above; bridge actors and admin have no
        // role on another user's signer roster. The custodial-key gate
        // (linked account + deposited nsec) is in the handler.
        "fauna.nostr.bunker.create_invite" => matches!(class, User),
        "fauna.nostr.bunker.list" => matches!(class, User),
        "fauna.nostr.bunker.revoke" => matches!(class, User),
        "fauna.nostr.bunker.set_label" => matches!(class, User),
        // The oracle's door (TP11 — `key-material-hierarchy.md` § Audience:
        // deployment infrastructure → *The oracle*): a third-party principal
        // binds the NIP-46 client key it will speak to this account's signer
        // with. ThirdParty ONLY — the account's own apps use the invite
        // roster above; the principal handler requires a live `identity.op`
        // grant, and every later request re-resolves it at the signer.
        "fauna.nostr.bunker.bind" => matches!(class, ThirdParty),
        // NIP-57 zap trust root (`fauna.nostr.zap_signers.*`,
        // `docs/goal/behavior/monetization.md` § Zap receipts — the trust
        // model): the payee's own list of signers allowed to speak for their
        // money. User-only and caller-scoped exactly like the bunker roster
        // above — nobody may edit another payee's trust root, and there is
        // deliberately no admin arm: an admin designating a signer on a
        // user's behalf would be an operator configuring somebody else's
        // payments, which is the role § Product invariants says does not
        // exist.
        //
        // Gated with the `zaps` member (a subset of `payments`) for the same
        // reason the `fauna.payments.*` arms below are: a build without it
        // registers no such kind, and the literals must not survive in the
        // artifact.
        #[cfg(feature = "zaps")]
        "fauna.nostr.zap_signers.list" => matches!(class, User),
        #[cfg(feature = "zaps")]
        "fauna.nostr.zap_signers.add" => matches!(class, User),
        #[cfg(feature = "zaps")]
        "fauna.nostr.zap_signers.remove" => matches!(class, User),
        // Protocol-native Nostr content (`nostr.*`, the prefix-less family —
        // the `bluesky.feed.thread` precedent; native-content HTTP→WS-RPC
        // rip 2026-07-22, `docs/goal/ui/nostr.md` § WS-RPC migration
        // contract). User-only like the bunker cluster: zap totals and badge
        // lists are end-user display reads over sync-worker-ingested rows,
        // and `publish_signed` is the NIP-07 leg whose linked-pubkey check
        // lives in the handler (the event pubkey must be the caller's linked
        // account).
        #[cfg(feature = "zaps")]
        "nostr.zaps.total" => matches!(class, User),
        "nostr.badges.list" => matches!(class, User),
        "nostr.events.publish_signed" => matches!(class, User),
        "fauna.email.inbox.fetch" => matches!(class, User),
        // The Sent sibling of `inbox.fetch` — the caller reads its OWN
        // sealed `Sent` mailbox (the outbound copy of mail submitted from an
        // external MUA). Same User-class, caller-scoped contract as
        // `inbox.fetch` (admin ⊇ user inherits it; it can only ever read its
        // own Sent, never another user's). Bridge actors read mail via the
        // BridgeMda `fauna.bridges.{list_messages,fetch_message_ciphertext}`.
        "fauna.email.sent.fetch" => matches!(class, User),
        // The on-device spam scorer's outcome for the caller's OWN INBOX
        // (watermark scored UIDs + move the spam subset INBOX→Junk). User-only
        // + caller-scoped (the handler acts only on the connection's actor,
        // never a target arg — § Cross-actor isolation); admin ⊇ user inherits
        // it. Least-privilege: the RPC can express only this scoring outcome,
        // NOT an arbitrary flag/move (the BridgeMda `fauna.bridges.{store_flags,
        // move}` the MDA uses stay bridge-only). `mail-spam.md` § Wire shapes.
        "fauna.email.apply_spam_disposition" => matches!(class, User),
        // A Fauna app's mail read state — `\Seen` on the caller's OWN INBOX
        // (`mail-app-surface.md` § Read state). `mark_seen` can only ADD
        // `\Seen` (no other flag, no removal, no other mailbox) — the second
        // named-effect flag write beside `apply_spam_disposition`, never a
        // general flag door; `flag_changes` reads the caller's INBOX flag
        // delta. User-only + caller-scoped; admin ⊇ user inherits both.
        "fauna.email.inbox.mark_seen" => matches!(class, User),
        "fauna.email.inbox.flag_changes" => matches!(class, User),
        // Conversations T1b — MLS-channel ciphertext plane (DM + group
        // chat send / fetch / per-actor channel list). User-only —
        // channels are end-user MLS conversations; bridge actors and
        // admin have no role on this surface. The (concurrently-
        // deprecated) HTTP twin `channel_routes` was auth'd by
        // bearer-actor match; the allowlist arm replaces that with the
        // sharper caller-class check.
        "fauna.conversations.channel.send" => matches!(class, User),
        // The foreign-member send relay (step 3b) — same User-only reasoning
        // as `channel.send`; the caller's own nest just adds the federation hop.
        "fauna.conversations.channel.send_remote" => matches!(class, User),
        "fauna.conversations.blob.write_token.get" => matches!(class, User),
        "fauna.conversations.channel.fetch" => matches!(class, User),
        "fauna.conversations.channel.list_for_actor" => matches!(class, User),
        // The roster read (phantom-leaf discriminator) — User-only on the same
        // reasoning as the rest of the channel cluster; its own handler further
        // narrows it to callers already on the channel.
        "fauna.conversations.channel.actors" => matches!(class, User),
        // Its foreign-homed relay twin — same User-only reasoning as
        // `send_remote`; the channel's home nest applies the structural
        // foreign-member gate.
        "fauna.conversations.channel.actors_remote" => matches!(class, User),
        // Conversations T2 — MLS key-package plane (publish own,
        // FIFO-consume a recipient's, count remaining). User-only on
        // the same reasoning as the channel cluster — key packages
        // are end-user MLS material; bridge actors and admin have no
        // role on this surface. The (concurrently-deprecated) HTTP
        // twin path-param-vs-bearer match on upload becomes implicit
        // here (the connection's actor is the upload target); fetch
        // and count carry an explicit target `actor_id` because the
        // sender side of chat initiation queries other users.
        "fauna.conversations.keypackage.upload" => matches!(class, User),
        // Widened to `User | BridgeMda` (caldav-server.md § Server-side
        // auto-schedule, C3): the MDA's mailbox-less auto-schedule gateway
        // fetches a recipient's key package to seal the one-off scheduling
        // delivery against. A public read by design — key packages are *meant*
        // to be fetchable (even anon cross-nest), so widening adds no
        // key-exposure surface; the consume/FIFO + last-resort fallback behaves
        // the same for the MDA caller as for a User. The MDA never uploads or
        // counts, so those two stay User-only.
        "fauna.conversations.keypackage.fetch" => matches!(class, User | BridgeMda),
        "fauna.conversations.keypackage.count" => matches!(class, User),
        // Conversations T3 — same-nest MLS Welcome delivery. The
        // welcomer (caller) is User-class; the HTTP twin's cross-nest
        // path (foreign client → recipient nest, `?nest_url=&...`) stays
        // alive on the HTTP route as federation residue.
        "fauna.conversations.welcome.deliver" => matches!(class, User),
        // The room plane's floor roster (`conversation-rooms.md` § The floor
        // roster). User-only: the report is a member device's account of its
        // own room's membership, and the handler additionally binds the
        // caller into the roster it reports. A bridge principal's membership
        // reaches the roster through the bridge's own capability vector
        // (`bridges.md` § Bridge-kind catalogue → Phase G), never through
        // this door; admin has no membership role in anybody's room.
        "fauna.conversations.room.roster_report" => matches!(class, User),
        // The birth ceremony and the roster read. User-only for the same
        // reason: founding a room and reading its membership are end-user
        // acts, and both handlers bind the caller to the room (the founder
        // must be the birth record's own owner; the reader must be a live
        // member). A bridge principal is seated in a room by that room's
        // members, and never founds one.
        "fauna.conversations.room.create" => matches!(class, User),
        "fauna.conversations.room.list_roster" => matches!(class, User),
        // The roster read's relayed twin, for a member homed on another nest.
        // User-only on the same grounds, plus the relay's own: the request
        // names a peer nest for this one to dial, and a bridge principal is
        // never the member such a leg would be originated for.
        "fauna.conversations.room.list_roster_remote" => matches!(class, User),
        // The report's relayed twin — user-only for the same two reasons: a
        // report comes from a member device, and the kind names a peer nest
        // for this one to dial.
        "fauna.conversations.room.roster_report_remote" => matches!(class, User),
        // The membership doors. User-only for the same reason as the rest of
        // the family: each is an end-user act bound to the caller's own
        // position in the room — the inviter must be the invite's signer and
        // hold a role the join rule admits, the accepter must be the
        // invitee, the remover must be an owner or admin, and the leaver
        // removes only itself.
        "fauna.conversations.room.invite" => matches!(class, User),
        "fauna.conversations.room.accept_invite" => matches!(class, User),
        "fauna.conversations.room.remove" => matches!(class, User),
        "fauna.conversations.room.leave" => matches!(class, User),
        // Who a room has invited, and taking an invitation back: a seated
        // member's, judged by its own seat. No bridge holds a seat's rank.
        "fauna.conversations.room.list_invites" => matches!(class, User),
        "fauna.conversations.room.revoke_invite" => matches!(class, User),
        // The departure's relayed twin — user-only for the same two reasons
        // as the roster relays above: a leave retires the caller's own seat,
        // and the kind names a peer nest for this one to dial.
        "fauna.conversations.room.leave_remote" => matches!(class, User),
        // The acceptance's relayed twin — user-only as the same-nest accept
        // is (a seat is a user principal's own), and the kind names a peer
        // nest for this one to dial.
        "fauna.conversations.room.accept_invite_remote" => matches!(class, User),
        // The invitation's relayed twin, for an inviter homed on another nest
        // — user-only as the same-nest invite is (an invitation is a seated
        // member's signed act), and the kind names a peer nest for this one
        // to dial.
        "fauna.conversations.room.invite_remote" => matches!(class, User),
        // Governance. User-only: a policy change is signed by the
        // caller and gated on its role, and ownership is a user
        // principal's to hold and to hand on.
        "fauna.conversations.room.set_policy" => matches!(class, User),
        // The policy's sibling record, signed by an owner or admin like it.
        "fauna.conversations.room.set_labelers" => matches!(class, User),
        "fauna.conversations.room.transfer_ownership" => matches!(class, User),
        // The sealing plane. User-only for the same reason as the rest of the
        // family, and caller-scoped in the sharpest way: a mint is admitted
        // only from a principal the ROOM's own floor ranks as owner or admin,
        // and `room.generations` serves a caller its own wraps and nobody
        // else's. (The nest-admin class is a *person* with a grantable role —
        // Admin ⊇ User above — not the nest itself; a room's key authority
        // being "never the nest" (`conversation-rooms.md` § Don't do these)
        // is enforced by there being no nest-side mint path at all, not by
        // this table.) A bridge principal seated in a community room reads
        // through its own membership, never this door.
        "fauna.conversations.room.publish_generation" => matches!(class, User),
        "fauna.conversations.room.generations" => matches!(class, User),
        "fauna.conversations.room.backfill_generations" => matches!(class, User),
        // A member's own wrap target for its own seat — a user's act by
        // definition; the nest's read is a grant made at a mint, never a key
        // it sets for itself.
        "fauna.conversations.room.set_reception_key" => matches!(class, User),
        // The relayed read a member homed elsewhere uses. Same class as its
        // same-nest twin: the room's home nest re-derives the recipient from
        // the requesting actor, so this door discloses nothing the local one
        // does not (the `channel.actors_remote` precedent).
        "fauna.conversations.room.generations_remote" => matches!(class, User),
        // Searching a room is a member's read of a member's room. User-only
        // for the same reason the wrap doors are: a bridge principal seated
        // in a community room reads through its own membership, and no
        // non-member class has a floor row to be gated on.
        "fauna.conversations.room.search" => matches!(class, User),
        // Spam slice — the user's local spam-classifier preferences. User-
        // only: these are per-actor end-user settings; bridge actors and
        // admin have no role. The HTTP twins were bearer-authed; the
        // allowlist arm adds the caller-class gate.
        "fauna.spam.get_preferences" => matches!(class, User),
        "fauna.spam.set_preferences" => matches!(class, User),
        // Moderation slice (Track B9) — the end-user-facing moderation
        // surface: aggregate stats, the caller's own obligation-action
        // records, appeals, client scan reports, Bayesian training, and
        // model sync. `User | Admin`: the human running a client (User) or
        // the box admin (Admin) may reach it; bridge actors have no role.
        // The HTTP twins were bearer-authed (or unauthenticated GETs that now
        // ride the bearer WS); the allowlist arm adds the caller-class gate.
        "fauna.moderation.stats" => matches!(class, User | Admin),
        "fauna.moderation.actions" => matches!(class, User | Admin),
        "fauna.moderation.appeal" => matches!(class, User | Admin),
        "fauna.moderation.train" => matches!(class, User | Admin),
        // The narrow legal-compulsion social-takedown carve-out (moderation.md
        // § Categories & enforcement item 1). **Admin-only** — the admin is the
        // deployment's legal-compliance responder (there is no operator; a User
        // must NOT be able to take down another user's post). This is NOT a
        // "remove for policy" lever: the handler additionally requires a legal
        // reference, writes a visible tombstone + an appealable obligation row +
        // a permanent audit row (moderation.md § Do NOT — the sole exception).
        "fauna.moderation.legal_takedown" => matches!(class, Admin),
        // Report-sharing opt-in + transparency (report-sharing.md § Client
        // wire). Both caller-scoped (the subject is always the connection
        // actor); a bridge actor has no user preferences and no reason to read
        // the transparency view, so User|Admin only.
        "fauna.moderation.report_share.set" => matches!(class, User | Admin),
        "fauna.moderation.report_share.status" => matches!(class, User | Admin),
        // Layer-B engagement-signal sharing (engagement-cues.md § Layer B nest
        // legs) — the opt-in sibling of report sharing. Caller-scoped opt-in +
        // transparency, and the per-item cue-verdict write; a bridge actor has
        // no feed to derive cues from, so User|Admin only.
        "fauna.moderation.signal_share.set" => matches!(class, User | Admin),
        "fauna.moderation.signal_share.status" => matches!(class, User | Admin),
        "fauna.moderation.signal_contribute" => matches!(class, User | Admin),
        // User-initiated reporting (moderation.md § User-initiated reporting).
        // Filing, reading and withdrawing one's own reports is every user's;
        // the queue and its resolution are the admin's (the report lands on
        // the admin's queue — there is no operator). A bridge actor neither
        // reports nor adjudicates.
        "fauna.moderation.abuse_report.submit" => matches!(class, User | Admin),
        "fauna.moderation.abuse_report.mine" => matches!(class, User | Admin),
        "fauna.moderation.abuse_report.withdraw" => matches!(class, User | Admin),
        "fauna.moderation.abuse_report.queue" => matches!(class, Admin),
        "fauna.moderation.abuse_report.resolve" => matches!(class, Admin),
        // Family-safety slice (family-safety.md § Wire & data shape). All four
        // are User|Admin at the class level — the real authorization is
        // per-target against the guardianships link table, in the handler:
        // `status` is caller-scoped; `policy.update` requires the caller to BE
        // the ward's guardian (deliberately NOT an admin power — no oversight
        // on the admin role); `graduate`/`transfer` accept guardian OR admin
        // (they only remove/re-point oversight, mirroring admission-time
        // designation being an admin act). Bridge actors have no role.
        "fauna.family.status" => matches!(class, User | Admin),
        "fauna.family.policy.update" => matches!(class, User | Admin),
        "fauna.family.graduate" => matches!(class, User | Admin),
        "fauna.family.transfer" => matches!(class, User | Admin),
        // The transfer consent handshake (family-safety.md § Graduation &
        // transfer): accept/decline authorize per-target against the ward's
        // pending proposal (caller must BE the proposed guardian); cancel is
        // guardian-or-admin, like transfer itself.
        "fauna.family.transfer.accept" => matches!(class, User | Admin),
        "fauna.family.transfer.decline" => matches!(class, User | Admin),
        "fauna.family.transfer.cancel" => matches!(class, User | Admin),
        "fauna.family.approvals.list" => matches!(class, User | Admin),
        "fauna.family.approvals.decide" => matches!(class, User | Admin),
        "fauna.family.contact.add" => matches!(class, User | Admin),
        // The ward's own in-app contact ask (v1.x, family-safety.md § Child-
        // initiated contact requests) — the supervised-caller check is the
        // handler's, per-target against the link table.
        "fauna.family.contact.request" => matches!(class, User | Admin),
        // The ward's own in-app feed-source ask (v1.x, family-safety.md
        // § Feed-source approvals) — like the contact ask, the supervised-caller
        // check is the handler's, per-target against the link table.
        "fauna.family.feed_source.request" => matches!(class, User | Admin),
        // Guardian Notify (family-safety.md § Guardian Notify): the supervised
        // account's conforming client reports coarse per-category enforcement
        // counts. User-called; the handler no-ops unless the caller is a
        // supervised account with the guardian's `content_notify` on.
        "fauna.family.notify_report" => matches!(class, User | Admin),
        // Screen-time budget heartbeat (family-safety.md § Screen time): the
        // supervised account's own client reports its foreground use — the
        // same caller shape as notify_report (User; a non-supervised caller is
        // a silent zero-reply no-op in the handler).
        "fauna.family.usage_report" => matches!(class, User | Admin),
        // The guardian-enrolled-device marker (family-safety.md § Full
        // visibility). Guardian-only in the handler (per-target, via the link)
        // — like `policy.update`, deliberately NOT an admin power, and never
        // the ward's: a child who could unmark could remove their guardian's
        // device, which is the promise this flag exists to keep.
        "fauna.family.device.mark" => matches!(class, User | Admin),
        // Search slice — full-text search scoped to the calling actor.
        // User-only; the HTTP twin was bearer-authed and scoped on the
        // bearer actor.
        "fauna.search.query" => matches!(class, User),
        // Posts slice (T1) — the
        // create / get / interact plane. User-only on the same reasoning
        // as the conversations/search clusters: posts are end-user content;
        // bridge actors and admin have no role on this surface. The
        // (concurrently-deprecated) HTTP twins were bearer-authed (create +
        // interact on the bearer actor; get with an optional bearer for the
        // quarantine gate); the allowlist arm adds the sharper caller-class
        // gate (the quarantine/author/admin checks stay inside the handler).
        "fauna.posts.create" => matches!(class, User),
        "fauna.posts.get" => matches!(class, User),
        // A community room's verdicts for room-restricted posts: User-only,
        // and gated inside on the caller's own floor membership — the
        // `room.search` posture (no non-member class has a floor row).
        "fauna.posts.room_labels" => matches!(class, User),
        // Its relayed twin, for a room homed on another nest: the same
        // User-only class, and the room home re-runs the floor gate itself
        // rather than trusting the relaying nest (the
        // `room.list_roster_remote` posture).
        "fauna.posts.room_labels_remote" => matches!(class, User),
        // The self-scoped own-post enumeration: User-only, and self-scoping is
        // structural (no `actor_id` on the wire), so no bridge or admin class
        // could name a target here even if admitted.
        "fauna.posts.list" => matches!(class, User),
        "fauna.posts.delete" => matches!(class, User),
        "fauna.posts.interact" => matches!(class, User),
        // D4 link previews (render-model.md § D4) — the per-app render
        // manager resolves a bare-url post's OpenGraph metadata through this
        // authenticated kind. User-only: it is an end-user render surface, and
        // authenticating it keeps the nest's SSRF-guarded outbound fetcher off
        // any anonymous request-amplifier path (Admin inherits via the User
        // check at the top of this fn).
        "fauna.linkpreview.resolve" => matches!(class, User),
        // Feed slice (T2) — the feed-CRUD +
        // feed-query + discovery-contributor plane. User-only on the same
        // reasoning as the posts cluster: feeds are an end-user content
        // surface; bridge actors and admin have no role here. The
        // (concurrently-deprecated) HTTP twins were bearer-authed for the
        // mutations + contributor reads; `list`/`get`/`posts`/`local.posts`
        // were unauthenticated on HTTP and become User-authed on WS-RPC,
        // matching T1's `fauna.posts.get` treatment. The per-feed ownership
        // checks stay inside the handler cores.
        "fauna.feed.list" => matches!(class, User),
        "fauna.feed.create" => matches!(class, User),
        "fauna.feed.get" => matches!(class, User),
        "fauna.feed.update" => matches!(class, User),
        "fauna.feed.delete" => matches!(class, User),
        "fauna.feed.posts" => matches!(class, User),
        // The two public timelines are also the first `ThirdParty` reach, under
        // `fauna:feed:read` (`principal_reach`): public posts only, nothing of
        // the account's own state — the principal handler's read, not this one.
        "fauna.feed.local.posts" => matches!(class, User | ThirdParty),
        "fauna.feed.trending.posts" => matches!(class, User | ThirdParty),
        "fauna.feed.contributors.list" => matches!(class, User),
        "fauna.feed.contributors.grant" => matches!(class, User),
        "fauna.feed.contributors.revoke" => matches!(class, User),
        "fauna.feed.factors.get" => matches!(class, User),
        "fauna.feed.factors.set" => matches!(class, User),
        // Sealed personalization-model plane (topic-factors.md § Wire &
        // registry). User-only on the `fauna.feed.factors.*` reasoning: a
        // per-actor personal-preference surface scoped to the calling actor
        // (owner-scoping is by construction in the handlers); bridge actors
        // and admin have no role here — the blob is nest-opaque, sealed
        // under the user's BackupKey.
        "fauna.personalization.model.fetch" => matches!(class, User),
        "fauna.personalization.model.put" => matches!(class, User),
        "fauna.personalization.model.delete" => matches!(class, User),
        // Notifications slice (T1) — the
        // list / mark-read / count plane. User-only on the same reasoning
        // as the posts/feed clusters: notifications are an end-user inbox
        // surface scoped to the calling actor; bridge actors and admin have
        // no role here. The (concurrently-deprecated) HTTP twins were
        // bearer-authed (every handler did `bearer.0.0 != actor_id → 403`);
        // the allowlist arm adds the sharper caller-class gate, and the
        // actor scoping moves to the connection `actor_id`.
        "fauna.notifications.list" => matches!(class, User),
        "fauna.notifications.mark_read" => matches!(class, User),
        "fauna.notifications.count" => matches!(class, User),
        // The user's own deletes (`behavior/notifications.md` § Retention
        // rule 2) — the same inbox surface, the same User-only scoping.
        "fauna.notifications.dismiss" => matches!(class, User),
        "fauna.notifications.clear" => matches!(class, User),
        // Contacts cluster (T2) — the
        // connection-management plane: knocks (inbound contact requests),
        // the contact roster, and the inbox-acceptance policy. User-only on
        // the same reasoning as the posts/feed/notifications clusters: these
        // are end-user surfaces scoped to the calling actor; bridge actors
        // and admin have no role here. The (concurrently-deprecated) HTTP
        // twins were all bearer-authed (every handler did
        // `bearer.0.0 != actor_id → 403`); the allowlist arm adds the
        // sharper caller-class gate, and the actor scoping moves to the
        // connection `actor_id`.
        "fauna.knocks.list" => matches!(class, User),
        "fauna.knocks.accept" => matches!(class, User),
        "fauna.knocks.block" => matches!(class, User),
        "fauna.knocks.unblock" => matches!(class, User),
        "fauna.knocks.dismiss" => matches!(class, User),
        "fauna.contacts.list" => matches!(class, User),
        "fauna.contacts.status" => matches!(class, User),
        "fauna.contacts.confirm" => matches!(class, User),
        "fauna.inbox.mode.get" => matches!(class, User),
        "fauna.inbox.mode.set" => matches!(class, User),
        // fauna-native inbox delivery-queue drain.
        // Caller-scoped by construction (the handler reads the connection's own
        // actor_id, no target param), so `User` (Admin inherits): the actor drains
        // its own store-and-forward queue. Replaces the HTTP `GET /api/v1/inbox`.
        "fauna.inbox.fetch" => matches!(class, User),
        "fauna.inbox.ack" => matches!(class, User),
        // The outbound counterpart — client→home-nest social inbox send.
        // `User` (Admin inherits): an end
        // user sends a signed `(ContactRequest, Post)` to a contact; the handler
        // additionally binds `cr.sender == caller`, so an authed actor can only
        // ever send *as itself*. Bridges have no role (they relay mail/protocol
        // payloads over their own kinds, not fauna-native social inbox sends).
        // Replaces the unauthenticated HTTP `POST /api/v1/inbox/{actor}`.
        "fauna.inbox.send" => matches!(class, User),
        // Bluesky-native thread view. The one
        // protocol-unique consume-side Bluesky kind kept on a `bluesky.*` name
        // (auth/settings/follows fold into `fauna.bridges.*`, interactions into
        // `fauna.posts.*`). User-only on the same reasoning as the posts/feed
        // clusters: a Bluesky-linked end user reads a thread for a crossposted
        // post; bridge actors and admin have no role here. The deprecated HTTP
        // twins (`feed/thread/{uri}` + `thread?post_id=`) were bearer-authed and
        // scoped on the bearer actor; the handler restores that actor's OAuth
        // agent. The arm is unconditional (harmless without the `bluesky`
        // feature — the handler simply isn't registered, so the kind is never
        // dispatched).
        "bluesky.feed.thread" => matches!(class, User),
        // Account cluster — the personal
        // account-management surface (account state, quota, admin-UI gate,
        // handle change, tier upgrade, account deletion). `User | Admin`
        // (NOT User-only like notifications/contacts): this is the surface the
        // human running the client manages, and an admin actor — which
        // resolves to `CallerClass::Admin`, never `User` — has their own
        // account too. `am_i_admin` in particular MUST be reachable by an
        // admin (it IS the admin check). Same reasoning + shape as
        // the other self-scoped account surfaces above. Bridge actors have no personal
        // account → denied. The (concurrently-deleted) HTTP twins were all
        // bearer-authed; the actor scoping moves to the connection `actor_id`.
        "fauna.account.get" => matches!(class, User | Admin),
        "fauna.quota.get" => matches!(class, User | Admin),
        "fauna.account.am_i_admin" => matches!(class, User | Admin),
        "fauna.profile.handle.change" => matches!(class, User | Admin),
        // The domain-expiry watch's record. `User | Admin` deliberately: § Domain
        // loss → *Detection* puts **every authenticated user** in this feeder's
        // audience (a resident's `@domain` addresses and recovery locator die
        // with the name too), and the reply differentiates the *lines* by role
        // rather than the *access* by role. Bridges have no stake in it.
        "fauna.domain.expiry.get" => matches!(class, User | Admin),
        // Per-user profile *detail* fetch (own or anyone else's) — same
        // `User | Admin` read gate as `fauna.account.get`. `docs/goal/ui/
        // profile.md` § Where logic lives.
        "fauna.profile.get" => matches!(class, User | Admin),
        // Own-profile publish/edit (the write half) — same `User | Admin`
        // personal-account gate as its sibling `fauna.profile.handle.change`
        // (the human running the client edits their own profile; an admin actor
        // has one too). The handler additionally asserts the signed
        // `Profile.actor_id` is the caller, so the class cannot widen the write
        // beyond the caller's own profile. `profile.md` § Where logic lives →
        // *Profile publish/edit*.
        "fauna.profile.set" => matches!(class, User | Admin),
        "fauna.account.upgrade" => matches!(class, User | Admin),
        "fauna.account.delete" => matches!(class, User | Admin),
        // Pending-actions surface — the
        // read/manage complement to the account cluster's pending-action
        // *creators*. list/get/cancel are `User | Admin` on the same reasoning
        // as the account cluster: the human running the client manages their own
        // queued destructive operations, and an admin (`CallerClass::Admin`, never
        // `User`) has their own too — plus the cancel authz matrix (creator /
        // target / admin) is enforced inside `CacheDb::cancel_pending_action`, so
        // an admin legitimately cancels admin-scoped actions through this surface.
        // `approve` is **Admin-only**: the (concurrently-deprecated) HTTP twin
        // used `AdminBearerAuth` (quorum approval is an admin action). Bridge
        // actors have no pending actions → denied.
        "fauna.pending_actions.list" => matches!(class, User | Admin),
        "fauna.pending_actions.get" => matches!(class, User | Admin),
        "fauna.pending_actions.cancel" => matches!(class, User | Admin),
        "fauna.pending_actions.approve" => matches!(class, Admin),

        // Stats (`fauna.stats.get`, Track B17) — repository storage stats. The
        // (concurrently-deprecated) HTTP twin used plain `BearerAuth` (any
        // authenticated actor), and the stats are global / per-folder, never
        // actor-scoped, so `User | Admin` preserves that reach. Bridge actors
        // have no business reading storage stats → denied.
        "fauna.stats.get" => matches!(class, User | Admin),

        // File versions (`fauna.files.versions.{list,get}`, Track B16) — version
        // history of a synced file (JSON metadata reads). The twins used plain
        // `BearerAuth`; versions are content-addressed by `path_hash`, never
        // actor-scoped, so `User | Admin` preserves that reach. Bridge actors
        // do not read file versions → denied.
        "fauna.files.versions.list" => matches!(class, User | Admin),
        "fauna.files.versions.get" => matches!(class, User | Admin),
        // The retention pipeline's recovery verb (file-versions.md § Retention
        // (3)) — same reach as the reads it undoes a prune for; the handler
        // additionally owner-gates on the row's set. Bridge actors → denied.
        "fauna.files.versions.undelete" => matches!(class, User | Admin),

        // Web content publishing (`fauna.web.{publish,domain}.*`, Track B18) —
        // a personal user surface (publish/unpublish posts as web pages, register
        // custom domains), actor-scoped on the connection actor. The twins used
        // plain `BearerAuth`, so `User | Admin`. Bridge actors do not publish web
        // content → denied.
        "fauna.web.publish.set" => matches!(class, User | Admin),
        "fauna.web.publish.unset" => matches!(class, User | Admin),
        "fauna.web.publish.list" => matches!(class, User | Admin),
        // Web-paywall capability-URL mint — caller-scoped to the connection
        // actor's OWN paywalled pages (monetization.md § Pillar 2).
        "fauna.web.paywall.mint_token" => matches!(class, User | Admin),
        // The owner client's sealed-`web_files` prune declaration
        // (web-content-hosting.md § Content model) — a personal user surface,
        // owner-scoped inside the handler on the connection actor's own folder.
        // Bridge actors run no sync engine and publish no web content → denied.
        "fauna.web.files.prune_sealed" => matches!(class, User | Admin),
        "fauna.web.domain.set" => matches!(class, User | Admin),
        "fauna.web.domain.get" => matches!(class, User | Admin),
        "fauna.web.domain.delete" => matches!(class, User | Admin),
        // Apex web-content actor designation — a deployment-wide admin setting
        // (the web analogue of `fauna.bridges.set_catch_all_actor`), so
        // `Admin`-only, unlike the per-user publish/domain kinds above.
        // `web-content-hosting.md` § Admin apex hosting.
        "fauna.web.set_apex_actor" => matches!(class, Admin),
        "fauna.web.get_apex_actor" => matches!(class, Admin),
        // Per-user subdomain-hosting opt-in (`<handle>.<domain>`) — a personal
        // user surface, caller-scoped on the connection actor (no `actor_id`
        // param), exactly like `fauna.web.publish.*`. `User | Admin` (the admin
        // is a Fauna user with their own actor + handle, and can host their own
        // subdomain). Bridge actors do not host web content → denied.
        // `web-content-hosting.md` § Routing + Architectural rule 8.
        "fauna.web.set_subdomain_enabled" => matches!(class, User | Admin),
        "fauna.web.get_subdomain_enabled" => matches!(class, User | Admin),

        // (The legacy plaintext `fauna.calendars.*` / `fauna.events.*` calendar
        // surface was retired in the Decision-B § 4c cleanup — calendars/events
        // now ride the encrypted `fauna.bridges.*` CalDAV store.)

        // Labels (hub Track B8) — the moderation-label attach/read plane.
        //
        // `attach` admits exactly the two producer positions `moderation.md`
        // § Per-row badge data path sanctions for a feed post: the post's own
        // author (a `User`) and a holder of that author's `content.label-write`
        // grant. The holder classes are the classes that can FETCH a grant at
        // all — `fauna.capabilities.fetch`'s `BridgeMda | ContentProcessor`
        // (the content-processing positions `content-scoring.md` § The
        // scoring-metadata bus names: the AUTH'd MDA drain and an in-process
        // content processor; `encryption-at-rest.md` § Capability tiering →
        // *Third-party holders* is where that class widens, not here). This
        // arm is the COARSE gate only: which post the caller may label is the
        // handler's ownership-or-grant check (`label_handlers::authorize_attach`),
        // so a holder class here still writes nothing without the author's
        // grant. The MTA and the PDS bridge hold no grants and stay out. Until
        // this arm admitted the holder classes the ratified grant path was
        // unreachable: the handler
        // consulted the grant resolver, which answers only for an enrolled
        // service user, whose class this arm refused first.
        //
        // `list` stays User-only on the posts/feed reasoning: an end-user read
        // surface; the write twin was bearer-authed, the read twin
        // unauthenticated on HTTP and `User`-authed on WS-RPC.
        "fauna.labels.attach" => matches!(class, User | BridgeMda | ContentProcessor),
        "fauna.labels.list" => matches!(class, User),

        // Admin user-management (`fauna.admin.users.*` + `fauna.admin.evictions.list`,
        // Track C / C1) — the admin surface migrating `/admin/api/users*` +
        // `/admin/api/evictions` onto WS-RPC (admin is a Fauna app; product
        // invariant: nest configuration is set from clients). **Admin-only**,
        // matching the HTTP twins' `AdminBearerAuth`. `suspend`'s twin additionally
        // gated on a role-tier extractor, but the roster is single-role today
        // (`admin_actor_ids.role` defaults `'superadmin'` and nothing writes another
        // value — `admin.md` § Admin continuity) and that dead tier machinery was
        // removed, so `is_admin` was always the whole check — the `Admin`
        // gate is behavior-preserving.
        "fauna.admin.users.list" => matches!(class, Admin),
        "fauna.admin.users.get" => matches!(class, Admin),
        "fauna.admin.users.create" => matches!(class, Admin),
        "fauna.admin.users.update" => matches!(class, Admin),
        "fauna.admin.users.delete" => matches!(class, Admin),
        "fauna.admin.users.clear_handle" => matches!(class, Admin),
        "fauna.admin.users.evict" => matches!(class, Admin),
        "fauna.admin.users.cancel_eviction" => matches!(class, Admin),
        "fauna.admin.users.suspend" => matches!(class, Admin),
        "fauna.admin.evictions.list" => matches!(class, Admin),

        // Admin-management (`fauna.admin.{tiers,invite_codes,invite_requests,
        // admins}.*`, Track C / C2) — the admin surface migrating
        // `/admin/api/{tiers,invite-codes,invite-requests,admins}` onto WS-RPC.
        // **Admin-only**, matching the HTTP twins' `AdminBearerAuth`. Distinct
        // from the pre-identity public `fauna.account.invite_{request,code}.*`
        // (Track A5, anonymous connection).
        "fauna.admin.tiers.list" => matches!(class, Admin),
        "fauna.admin.tiers.create" => matches!(class, Admin),
        "fauna.admin.tiers.update" => matches!(class, Admin),
        // Membership designation (monetization.md § Pillar 4) — Admin-only like
        // the rest of C2. WS-RPC-native, no HTTP twin.
        "fauna.admin.membership_tiers.list" => matches!(class, Admin),
        "fauna.admin.membership_tiers.set" => matches!(class, Admin),
        "fauna.admin.membership_tiers.clear" => matches!(class, Admin),
        "fauna.admin.invite_codes.list" => matches!(class, Admin),
        "fauna.admin.invite_codes.create" => matches!(class, Admin),
        "fauna.admin.invite_codes.delete" => matches!(class, Admin),
        "fauna.admin.invite_requests.list" => matches!(class, Admin),
        "fauna.admin.invite_requests.approve" => matches!(class, Admin),
        "fauna.admin.invite_requests.deny" => matches!(class, Admin),
        "fauna.admin.admins.list" => matches!(class, Admin),
        "fauna.admin.admins.add" => matches!(class, Admin),
        "fauna.admin.admins.remove" => matches!(class, Admin),
        // The co-admin seed hand-off (`nest/box-recovery.md` § Mechanism, the
        // co-admin bullet) — Admin-class, same trust argument as rotation
        // below: a roster holder already has full nest-admin authority, so
        // handing them the seed they'd otherwise custody via a stale claim
        // hand-off crosses no new boundary.
        "fauna.admin.deployment_seed.get" => matches!(class, Admin),
        // The deployment-seed rotation ceremony (`nest/box-recovery.md`
        // § Deployment-seed rotation). Admin-class, and deliberately not
        // narrower: a roster holder already has full nest-admin authority and
        // already receives the successor seed through the self-healing capture,
        // so gating rotation below the roster would protect nothing while
        // leaving the box unable to evict a hostile ex-admin.
        "fauna.admin.deployment_seed.rotate" => matches!(class, Admin),

        // Stats / audit / ops (`fauna.admin.{stats,status,audit.*,cluster.status,
        // gc,worker.status,pending_actions.list}`, Track C / C3) — the admin
        // observability + GC + cross-actor pending-actions surface migrating
        // `/admin/api/{stats,status,audit,audit/integrity,cluster/status,gc,
        // worker/status,pending-actions}` onto WS-RPC. **Admin-only**, matching
        // the HTTP twins' `AdminBearerAuth`. The cross-actor
        // `fauna.admin.pending_actions.list` is distinct from B20's user-scoped
        // `fauna.pending_actions.list` (gated `User | Admin`).
        "fauna.admin.stats" => matches!(class, Admin),
        "fauna.admin.status" => matches!(class, Admin),
        "fauna.admin.audit.list" => matches!(class, Admin),
        "fauna.admin.audit.integrity" => matches!(class, Admin),
        "fauna.admin.cluster.status" => matches!(class, Admin),
        "fauna.admin.gc" => matches!(class, Admin),
        "fauna.admin.worker.status" => matches!(class, Admin),
        "fauna.admin.pending_actions.list" => matches!(class, Admin),

        // The deployment's declared region (`region-blocking.md` § Region
        // determination). Admin-only in both directions, including the **read**:
        // the situs is a fact about the deployment that belongs on the admin
        // screen, and the surface a *user* is owed about the region tier is the
        // one that tells them what binds them — `fauna.features.status`, which
        // carries the active document's identity and version and is User-class.
        //
        // Unreachable by every bridge class, like the rest of this plane. A
        // bridge is a service actor, never a rule-setter, and never the party
        // that decides where a deployment legally sits.
        "fauna.admin.region.get" => matches!(class, Admin),
        "fauna.admin.region.set" => matches!(class, Admin),
        // The admin's web-app origin choice (`web-content-hosting.md` § The
        // nest-served `/app/` and the central origin). Admin-only both ways; what
        // a user is owed — the address `/app/` sends them to — rides the
        // anonymous `fauna.setup.status` projection, and is public anyway (any
        // browser that opens `/app/` sees the redirect).
        "fauna.admin.web_app_origin.get" => matches!(class, Admin),
        "fauna.admin.web_app_origin.set" => matches!(class, Admin),
        // The app-facing relay: any user may fetch its own region's
        // published artifact (region-blocking.md § The content plane). User —
        // and so an admin's app too (Admin ⊇ User, above) — never a bridge or
        // a custodian, which run no app and declare no region. Nothing here
        // submits: the ingress is the compiled-in log URL.
        "fauna.region.artifact.get" => matches!(class, User),

        // Folders / services (`fauna.admin.{folders,services}.*`, Track C / C5)
        // — the admin-level folder / sidecar-service surface migrating
        // `/admin/api/{folders*,services*}` onto WS-RPC. **Admin-only**, matching
        // the HTTP twins' `AdminBearerAuth`. The admin `fauna.admin.folders.*`
        // are distinct from the user-class `fauna.folders.*` (B14,
        // `User | Admin`) cluster. (The `wireguard.*` arms died with the
        // WireGuard stack, 2026-08-23.)
        "fauna.admin.folders.create" => matches!(class, Admin),
        "fauna.admin.folders.get" => matches!(class, Admin),
        "fauna.admin.folders.add_member" => matches!(class, Admin),
        "fauna.admin.services.list" => matches!(class, Admin),
        "fauna.admin.services.update" => matches!(class, Admin),

        // Observability (`fauna.admin.logs`, Track C / C6 — observability.md
        // § Surfaces) — the admin Logs view's read of the nest's in-memory
        // `fauna-log` ring. **Admin-only**: the ring can carry deployment-
        // operational targets/metadata (never message plaintext — the redaction
        // rule binds the call sites), an admin concern, not a user/bridge one.
        "fauna.admin.logs" => matches!(class, Admin),

        // Factory reset — destructive return-to-fresh/unclaimed. `Admin`-gated
        // like every other `fauna.admin.*`; the single-admin dogfood box's admin
        // IS effectively the superadmin. Finer superadmin-role gating + a
        // cooldown (client-compromise protection) is the deferred follow-up
        // (see `factory_reset.rs` + goal doc § Factory reset).
        "fauna.admin.factory_reset" => matches!(class, Admin),

        // Client-set `[nest]`-policy toggles (`fauna.admin.set_{require_registration,
        // subhandles}`) — deployment-wide auth/discovery policy an admin chooses
        // from a client instead of CLI/config (a product invariant). The
        // authed Admin-class twin of `fauna.bridges.set_mail_enabled`; the nest
        // upserts a DB singleton + swaps the live `AppState` RwLock. See
        // `node_policy_handlers`.
        "fauna.admin.set_registration_mode" => matches!(class, Admin),
        "fauna.admin.set_subhandles" => matches!(class, Admin),
        "fauna.admin.set_age_verification_required" => matches!(class, Admin),
        "fauna.admin.set_max_storage_bytes" => matches!(class, Admin),
        "fauna.admin.set_cors_origins" => matches!(class, Admin),
        // The admin's chosen client-facing API serving port (the nest's own HTTPS
        // listener) — apply-on-restart, surfaced on `fauna.setup.status`. Unlike
        // its siblings it writes a `/data/serving-port` value flag + no live swap.
        // See `node_policy_handlers` + `nest/common.md` § Serving ports.
        "fauna.admin.set_serving_port" => matches!(class, Admin),
        // Host-OS "restart now": writes a flag the host reboot-coordinator picks
        // up (onboarded VPS only; rejected on a nest with no maintenance mount).
        // See `host_maintenance.rs` + `installers/vps.md` § Host OS Maintenance.
        "fauna.admin.request_host_restart" => matches!(class, Admin),

        // Pairings: admin `fauna.admin.pairings.*` RETIRED (per-user-pairing
        // design) — pairing is the user's own action (`fauna.pair.{add,revoke}`,
        // below); the admin's only control is the `pairing` service knob.

        // Push subscription management (`fauna.push.*`, Track B22) — per-actor
        // device subscriptions + the public VAPID key. `User | Admin` (the human
        // running the client manages their own devices, and an admin actor —
        // `CallerClass::Admin`, never `User` — has devices too + must reach the
        // VAPID key to subscribe). The (concurrently-deprecated) HTTP twins:
        // `subscribe`/`unsubscribe` were `BearerAuth`, `vapid-key` was no-auth
        // (migrated onto the authenticated connection — push subscription is
        // post-login). Bridge actors have no push devices → denied.
        "fauna.push.vapid_key" => matches!(class, User | Admin),
        "fauna.push.subscribe" => matches!(class, User | Admin),
        "fauna.push.unsubscribe" => matches!(class, User | Admin),
        // Tags the caller's own connection with the push device it serves —
        // it can only suppress or receive the actor's own push (ruled
        // 2026-09-26, `apps/common.md` § Registration).
        "fauna.push.presence" => matches!(class, User | Admin),

        // Session management (`fauna.sessions.*`, Track B2) — per-actor active
        // tokens: list, revoke one, revoke-all-except-current, authed emergency
        // lockout. `User | Admin` (the human running the client manages their
        // own sessions, and an admin actor — `CallerClass::Admin`, never `User`
        // — has sessions too). The (concurrently-deprecated) HTTP twins were
        // `BearerAuth` / `BearerAuthWithToken`. Bridge actors don't manage human
        // sessions → denied. The no-token Ed25519 `POST /account/lockout`
        // recovery channel stays HTTP (not a kind).
        "fauna.sessions.list" => matches!(class, User | Admin),
        "fauna.sessions.revoke" => matches!(class, User | Admin),
        "fauna.sessions.revoke_all" => matches!(class, User | Admin),
        "fauna.sessions.lockout" => matches!(class, User | Admin),

        // Folder management (`fauna.folders.*` + `fauna.sync.conflicts.*`,
        // Track B14) — personal file-sync data (CRUD + members + devices +
        // schedule + leases + conflicts), actor-scoped on the connection actor.
        // The (concurrently-deprecated) HTTP twins (`user_folder_routes` +
        // `lease_routes`) used plain `BearerAuth`, so `User | Admin` (an admin
        // actor owns folders too — the web/files.versions/stats/calendars
        // precedent). `sync.conflicts.report` is the fauna-sync daemon's call;
        // the daemon authenticates as the user's actor → `User`. Bridge actors
        // have no file-sync surface → denied.
        "fauna.folders.create" => matches!(class, User | Admin),
        "fauna.folders.list" => matches!(class, User | Admin),
        "fauna.folders.update" => matches!(class, User | Admin),
        "fauna.folders.set_web_paywall" => matches!(class, User | Admin),
        "fauna.folders.delete" => matches!(class, User | Admin),
        "fauna.folders.devices" => matches!(class, User | Admin),
        "fauna.folders.members.list" => matches!(class, User | Admin),
        // The *actor* (user) roster of a shared set — the owner-side "Shared with"
        // list (shared folders Slice 3). Member-aware read via `folder_authz`,
        // same owner/User scoping + bridges-denied as the rest of the surface.
        "fauna.folders.members.list_actors" => matches!(class, User | Admin),
        // Its cross-nest relay twin (the reader's roster source for a foreign
        // set): the same scoping; the home nest applies the membership gate.
        "fauna.folders.members.list_actors_remote" => matches!(class, User | Admin),
        // Member access grant (multi-writer Phase 1): the owner sets a member's
        // reader/writer role + byte cap. Owner+claimant-gated in the handler
        // (mirrors content_key.put); same User scoping + bridges-denied.
        "fauna.folders.members.set_access" => matches!(class, User | Admin),
        "fauna.folders.members.remove" => matches!(class, User | Admin),
        // Device-place flags (folders re-model phase 2 § Places): the one
        // add/edit door onto a device place. Owner-scoped + bridges denied,
        // same as the rest of the surface.
        "fauna.folders.places.set" => matches!(class, User | Admin),
        // Cross-user roster eviction (shared folders Slice 3, F1/OBS-1): the
        // owner drops a removed member's `actor_channels` entry. Owner-scoped +
        // bridges denied, same as the rest of the folder surface.
        "fauna.folders.members.evict" => matches!(class, User | Admin),
        // Member self-remove (shared folders Slice 3): a *recipient* voluntarily
        // leaves a set shared with them, self-dropping their own `actor_channels`
        // row (addressed by the raw group id they hold, not the owner-only name).
        // Self-scoped + bridges denied, same User/Admin scoping as the rest.
        "fauna.folders.leave" => matches!(class, User | Admin),
        // Cross-user share/bind (shared folders Slice 2): an owner binds their
        // own set to a client-created MLS group. Owner-scoped + bridges denied,
        // same as the rest of the folder surface.
        "fauna.folders.share" => matches!(class, User | Admin),
        // M2 content-key envelope (shared folders Slice 3): the owner publishes
        // the sealed generation bundle (`put`, owner-scoped) and roster members
        // fetch it (`get`, member-aware via `folder_authz`). Opaque ciphertext
        // nest-side; same owner/User scoping + bridges-denied as the rest.
        "fauna.folders.content_key.put" => matches!(class, User | Admin),
        "fauna.folders.content_key.get" => matches!(class, User | Admin),
        // Cross-nest writer byte-plane token (Phase 3): the member's own nest
        // relays the mint from the set's home nest. Same User scoping +
        // bridges-denied as the rest of the folder surface.
        "fauna.folders.write_token.get" => matches!(class, User | Admin),
        // Its read-scoped twin, for any member of a set homed elsewhere.
        "fauna.folders.read_token.get" => matches!(class, User | Admin),
        // The served-era adoption (`writer-signed-change-records.md` ruling
        // (7)(b)) — the owner's own sets only, resolved from the connection.
        "fauna.folders.served_rows.adopt" => matches!(class, User | Admin),
        "fauna.folders.lease.acquire" => matches!(class, User | Admin),
        // A third-party principal's write-only ingress (`file-sync.md`
        // § Third-party deposit ingress). ThirdParty ONLY — the account's
        // own apps write through the sync engine; the principal handler
        // requires a scope naming the folder and a live keyless `deposit`
        // grant, re-resolved at every deposit.
        "fauna.folders.deposit" => matches!(class, ThirdParty),
        // The owner's half of that inbox — adoption's list and retire, the
        // account's own folders only (resolved from the connection).
        "fauna.folders.deposits.list" | "fauna.folders.deposits.retire" => {
            matches!(class, User | Admin)
        }
        "fauna.folders.lease.release" => matches!(class, User | Admin),
        // The publicly-synced follow (phase 4 slice 4f-i): any account may read
        // a public folder — the plane's gate is the folder's own audience, not
        // the caller (`federation.md` § The public folder read plane).
        "fauna.folders.public.fetch" => matches!(class, User | Admin),
        "fauna.sync.conflicts.list" => matches!(class, User | Admin),
        "fauna.sync.conflicts.report" => matches!(class, User | Admin),
        "fauna.sync.conflicts.resolve" => matches!(class, User | Admin),

        // Share-link control plane (`fauna.share.*`, Track E1) — a personal
        // surface registering/listing/revoking the actor's own client-minted
        // share tokens, scoped to the connection actor. `User | Admin` (an admin
        // shares files too — the folders/calendars precedent). It is net-new
        // (no HTTP twin); the public `GET /share/{token}` is browser-facing
        // residue, not a kind. Bridge actors have no share surface → denied.
        "fauna.share.create" => matches!(class, User | Admin),
        "fauna.share.list" => matches!(class, User | Admin),
        "fauna.share.revoke" => matches!(class, User | Admin),

        // Device-sync control plane (`fauna.sync.{register,changes.*,status,
        // files,backup_status,devices.*}`, Track B13) — device registration +
        // change recording/polling + per-set status, actor-scoped on the
        // connection actor. Same reasoning as the folder arms above: the
        // (concurrently-deprecated) HTTP twins (`sync_routes`) used plain
        // `BearerAuth`, so `User | Admin`. The fauna-sync daemon registers /
        // records as the user's actor → `User`. Bridge actors have no
        // device-sync surface → denied.
        "fauna.sync.register" => matches!(class, User | Admin),
        // `ThirdParty`: the `records` arm's list of the principal's own
        // `ext:<kind>` scopes — the principal handler serves only a scope its
        // grant covers (`third-party-kinds.md` § The record doors).
        "fauna.sync.changes.list" => matches!(class, User | Admin | Custodian | ThirdParty),
        "fauna.sync.changes.record" => matches!(class, User | Admin),
        "fauna.sync.changes.supersede" => matches!(class, User | Admin),
        "fauna.sync.status" => matches!(class, User | Admin),
        "fauna.sync.files" => matches!(class, User | Admin),
        "fauna.sync.backup_status" => matches!(class, User | Admin),
        "fauna.sync.devices.list" => matches!(class, User | Admin),
        "fauna.sync.devices.delete" => matches!(class, User | Admin),
        // W2.3, the account-data plane's class-2 write leg. **User-class, and
        // the scope is derived from the connection actor** — the request carries
        // no actor id at all (`account-sync-plane.md` § Feeds and cursors →
        // *Scope partition — three scope families, one feed contract*: the
        // account-state scope is exactly one per account), so a caller can
        // only ever write their own,
        // exactly like `fauna.drafts.put`. Bridge actors
        // have no account replica → denied with the rest of this surface.
        // `ThirdParty`: the `records` arm's put into one of the principal's own
        // `ext:<kind>` scopes, as its own attested writer only — the principal
        // handler's three checks (`third-party-kinds.md` § The record doors).
        "fauna.account.state.put" => matches!(class, User | Admin | ThirdParty),
        // The feed's own compaction (fleet-scope reclamation, 2026-09-16):
        // same actor-derived scope, same class as the put it un-does.
        "fauna.account.state.retire" => matches!(class, User | Admin),
        // The generation escrow doors (R14 (account-data-plane.md § The ratified decisions) build step 4 — nest-side
        // requirement 4): account-derived like `fauna.account.state.put`
        // above — the wraps served/deleted are the connection actor's own,
        // and a recovery-ceremony session authenticates as the account too.
        // Bridge actors hold no generation key material → denied.
        "fauna.generation.escrow.put" => matches!(class, User | Admin),
        "fauna.generation.escrow.get" => matches!(class, User | Admin),
        "fauna.generation.escrow.delete" => matches!(class, User | Admin),
        // Renewal-grant registration (sync-agent.md § Credential model): the
        // identity-holding client stores the RenewBearer-scoped grant on its
        // own device row. Same class as the surrounding devices surface.
        "fauna.sync.device_grant.register" => matches!(class, User | Admin),
        // Renewal-grant retirement — the register's inverse, and strictly
        // narrower than the `fauna.sync.devices.delete` any bearer of the
        // account may already call: it retires ONE grant by its device key and
        // leaves the device row, its folder memberships and its label intact.
        // Bridge actors hold no grants → denied.
        "fauna.sync.device_grant.revoke" => matches!(class, User | Admin),
        // Per-device p2p participation (p2p.md § Per-device participation):
        // the owner arm brakes a device of the caller's own account, the self
        // arm is a device reporting on its own row. Bridge actors hold no
        // devices → denied.
        "fauna.sync.devices.p2p_participation.set" => matches!(class, User | Admin),
        // Relay serving's announce (`file-sync.md` § Relay serving): tags the
        // caller's own connection with folders of a device of its own account
        // that it owns or is a member of. Bridge actors run no engines → denied.
        "fauna.sync.serve.announce" => matches!(class, User | Admin),

        // Cross-set media aggregation (`fauna.media.list`) — the Media page's
        // all-media view over the caller's readable folders; a bearer-authed
        // read, gated like the sibling `fauna.sync.*` reads.
        "fauna.media.list" => matches!(class, User | Admin),

        // The media proxy's playback ticket (`fauna.media.playback_ticket`,
        // render-model.md § D6c → *Inline playback*, answer 4) — minted for a
        // signed-in user over the one authenticated channel, like the
        // `fauna.linkpreview.resolve` fetch it sits beside.
        "fauna.media.playback_ticket" => matches!(class, User),

        // Folder snapshot control (`fauna.filesync.snapshot.*`, Track B15) —
        // create / get / queued-delete / undelete / prune / check / diff of the
        // synced folder backup snapshots, plus the `list` fold-in (the kind
        // already existed for owner-implicit message-kind snapshots; the folder
        // branch shares it). `User | Admin` is only the caller-CLASS layer —
        // file_set-name / snapshot-id scoped, so each handler ALSO owner-scopes
        // (review N1: `authorize_snapshot` / `get_folder_for_actor`), the
        // equivalent of the HTTP twins' `authorize_snapshot_owner` over
        // `BearerAuth`. Bridge actors don't manage folder backups → denied.
        // The snapshot byte downloads (ZIP restore / single-file) stay HTTP
        // residue.
        "fauna.filesync.snapshot.create_folder" => matches!(class, User | Admin),
        "fauna.filesync.snapshot.get" => matches!(class, User | Admin),
        // The S8 D3 seal stamp: the handler further narrows to the label
        // audience (a Q5 admin is refused there — the allowlist class is the
        // outer capability gate, not the audience gate).
        "fauna.filesync.snapshot.stamp_labels" => matches!(class, User | Admin),
        "fauna.filesync.snapshot.delete" => matches!(class, User | Admin),
        "fauna.filesync.snapshot.undelete" => matches!(class, User | Admin),
        "fauna.filesync.snapshot.prune" => matches!(class, User | Admin),
        // Same class gate as `prune` — it is the same operation, differing only
        // in where the policy comes from (the set's resting column, not the
        // wire). It carries no policy a caller could smuggle, so if anything it
        // is the narrower of the two.
        "fauna.filesync.snapshot.prune_set_policy" => matches!(class, User | Admin),
        "fauna.filesync.snapshot.check" => matches!(class, User | Admin),
        "fauna.filesync.snapshot.diff" => matches!(class, User | Admin),
        "fauna.filesync.snapshot.list" => matches!(class, User | Admin),
        // Owner-implicit message-kind snapshots (`create_message_kind` /
        // `restore_message_kind` / `delete_immediate`) + the restore-history /
        // divergence reads. These self-enforce ownership inside the handler (no
        // folder / snapshot-id param — the owning actor is the connection
        // actor), so they historically took no allowlist arm. The central
        // capability gate (`routes::dispatch_request` gate 1d) now consults
        // `is_permitted` for *every* non-pre-identity registered kind, so they
        // need an explicit `User | Admin` arm exactly like their folder
        // siblings above — same posture, same reasoning (bridge actors don't
        // manage personal backups → denied).
        "fauna.filesync.snapshot.create_message_kind" => matches!(class, User | Admin),
        "fauna.filesync.snapshot.restore_message_kind" => matches!(class, User | Admin),
        "fauna.filesync.snapshot.delete_immediate" => matches!(class, User | Admin),
        "fauna.filesync.snapshot.list_restore_history" => matches!(class, User | Admin),
        "fauna.filesync.snapshot.list_restore_divergence" => matches!(class, User | Admin),

        // Pairing (per-user multi-homing, "Linked nests") — owner-scoped:
        // `list` reads, `add`/`revoke` authorize/unlink the bearer actor's own
        // nest pairings. `User | Admin` (an admin links their own nests too;
        // `add` is additionally gated by the admin `pairing` service knob in
        // the handler). Bridge actors don't manage user pairings → denied. The
        // retired peer handshake (`POST /pair`, `/pair/revoke`) was never a kind.
        "fauna.pair.list" => matches!(class, User | Admin),
        "fauna.pair.add" => matches!(class, User | Admin),
        "fauna.pair.revoke" => matches!(class, User | Admin),
        // The forward-queue actions act on the caller's own outbox rows only.
        "fauna.pair.forward_retry" => matches!(class, User | Admin),
        "fauna.pair.forward_discard" => matches!(class, User | Admin),

        // Subscriptions / monetization (`fauna.subscriptions.*`) — the author's
        // tier CRUD + offers/requests/subscribers management and the subscriber's
        // subscribe/unsubscribe + status/key-material reads. `User` (Admin
        // inherits via the promotion at the top of this fn): end-user content,
        // every handler caller-scoped on the connection actor (`author_id` /
        // subscriber actor — none derives its target from a request field that
        // could widen reach), exactly like the posts / feed / conversations
        // clusters. Bridge actors have no role on a user's subscriptions →
        // denied (the `_ => false` default before the central gate landed; now an
        // explicit arm so the central gate permits the User/Admin callers it must
        // and the coverage tripwire `every_registered_kind_is_gated` passes). See
        // `docs/goal/behavior/monetization.md` + `docs/goal/ui/profile.md`.
        "fauna.subscriptions.tiers.list" => matches!(class, User),
        "fauna.subscriptions.tiers.create" => matches!(class, User),
        "fauna.subscriptions.tiers.update" => matches!(class, User),
        "fauna.subscriptions.tiers.clear_field" => matches!(class, User),
        "fauna.subscriptions.tiers.delete" => matches!(class, User),
        "fauna.subscriptions.offers.list" => matches!(class, User),
        "fauna.subscriptions.post_unlock.get" => matches!(class, User),
        "fauna.subscriptions.mine.list" => matches!(class, User),
        "fauna.subscriptions.status.get" => matches!(class, User),
        "fauna.subscriptions.subscribe" => matches!(class, User),
        "fauna.subscriptions.unsubscribe" => matches!(class, User),
        "fauna.subscriptions.subscribers.list" => matches!(class, User),
        "fauna.subscriptions.subscribers.remove" => matches!(class, User),
        "fauna.subscriptions.requests.list" => matches!(class, User),
        "fauna.subscriptions.requests.approve" => matches!(class, User),
        "fauna.subscriptions.requests.reject" => matches!(class, User),
        "fauna.subscriptions.key_blob.get" => matches!(class, User),
        "fauna.subscriptions.key_blob.rotate" => matches!(class, User),
        "fauna.subscriptions.delegate.upload" => matches!(class, User),

        // Payment providers (`fauna.payments.*`, monetization.md § Pillar 3) —
        // the author's provider-config CRUD (kind + webhook-verification
        // secret + tier mapping, all keyed on the bearer) and the buyer's
        // claim-code redemption (binds the bearer actor). `User` (Admin
        // inherits via the promotion at the top): end-user monetization
        // config/consumption, every handler caller-scoped on the connection
        // actor exactly like the subscriptions cluster above. Bridge actors
        // have no role on a user's payment providers → denied. The webhook
        // ingress is NOT a kind — providers can't speak WS-RPC; it is the
        // public HTTP route `payment_routes::payment_webhook`.
        //
        // Gated with the plane itself: a store-safe nest registers none of these
        // kinds, so an ungated arm would be dead code — and, more to the point,
        // would leave the `fauna.payments.*` literals in the excised binary,
        // where `just nest-store-safe-check`'s `strings` witness would (rightly)
        // find them (`dynamic-features.md` § What "completely compiled away"
        // means, item 2: no wire senders).
        #[cfg(feature = "payments")]
        "fauna.payments.providers.set" => matches!(class, User),
        #[cfg(feature = "payments")]
        "fauna.payments.providers.list" => matches!(class, User),
        #[cfg(feature = "payments")]
        "fauna.payments.providers.remove" => matches!(class, User),
        #[cfg(feature = "payments")]
        "fauna.payments.claims.redeem" => matches!(class, User),
        #[cfg(feature = "payments")]
        "fauna.payments.claims.mint" => matches!(class, User),
        #[cfg(feature = "payments")]
        "fauna.payments.claims.list" => matches!(class, User),

        // The feature plane's transparency read (`dynamic-features.md`
        // § Transparency & auditability, boundary 4: *"there is no restriction
        // you cannot see"*) — answers for the bearer about their own effective
        // policy. USER-class, and deliberately **not** `payments`-gated (see the
        // registration comment in `lib.rs`): every build ships this kind, and the
        // plane gates three members, so the read answers for whichever ones a
        // build ships — a store-safe nest still tells its users what binds them.
        // Strictly caller-scoped (the handler takes no parameters and answers
        // about the bearer), so a bridge actor has nothing to read here → denied.
        "fauna.features.status" => matches!(class, User),

        // The post-addressed tip attribution read (`monetization.md` § Tips).
        // USER-class like every other post-scoped read: authenticated, and
        // anti-enumeration rests on `post_id = blake3(body)` being unguessable
        // without having seen the post — the same property
        // `fauna.subscriptions.post_unlock.get` above relies on. A bridge actor
        // has no role on a user's tips → denied.
        #[cfg(feature = "payments")]
        "fauna.tips.list" => matches!(class, User),

        // Per-actor segment-store maintenance (`fauna.segments.{list,compact}`) —
        // a client lists / compacts its OWN backup segment store. `User` (Admin
        // inherits): each handler decodes the request `actor_id` and rejects
        // `actor_id != caller` (`not_owner`), so it is strictly caller-scoped.
        // Bridge actors have no segment store → denied. Explicit arm added with
        // the central capability gate (previously `_ => false`).
        //
        // `list` additionally admits `Custodian`: it is the
        // control plane of the BULK half, where a custodian turns the record-cid
        // walk's coordinates into the segments holding those bytes. The class
        // does NOT relax the owner scoping — the handler still refuses
        // `actor_id != caller` unless `custody_admission` finds a live row
        // covering `content:<kind>:<actor_id>`, and the refusal is the same
        // `not_owner` a stranger gets. `compact` stays `User`-only on purpose:
        // it MUTATES the owner's store, and custody is a read capability
        // (`account-data-plane.md` § The custody grant + ceremony).
        "fauna.segments.list" => matches!(class, User | Custodian),
        "fauna.segments.compact" => matches!(class, User),
        // The counter floor MUTATES the owner's store too, so it is `User`
        // only for compact's reason — custody is a read capability.
        "fauna.segments.counter_floor" => matches!(class, User),

        // Protocol-level round-trip echo (`fauna.protocol.echo`) — the WS test
        // kind (e2e `test_api_helpers.py`, integration `echo_round_trip.rs`).
        // "No auth restrictions beyond the WS handshake's actor-bound bearer", so
        // any authenticated class may call it — including bridge actors (a bridge
        // could ping the channel). It carries no data surface (returns the
        // request bytes verbatim), so the wide grant exposes nothing. Listed
        // explicitly so the central gate doesn't reject it and the coverage
        // tripwire passes.
        "fauna.protocol.echo" => matches!(class, BridgeMta | BridgeMda | User | Admin),

        // ── Capability grants (fauna.capabilities.*) ──────────────────────
        //
        // The decisive gating difference from DKIM/TLS (which are *deployment
        // infrastructure* → Admin-minted): a content capability is *the owner's
        // own data access* → **OWNER-minted, holder-fetched** (design § Phase 2
        // Step 2 § 2.3). So mint/renew/revoke are `User` (the content-owning
        // actor; Admin inherits via the User⊇ shortcut above, and mints only
        // over its OWN content — the handler enforces `ix.owner == caller`,
        // caller-scoped like every other User kind). NOT Admin-as-a-special-
        // case, NOT a bridge. `fetch` is the holder side: any approved
        // content-processing bridge (`BridgeMda`, the serve instance, or the
        // generic `ContentProcessor`) pulls the grants sealed to it —
        // self-pubkey-scoped in the handler (`holder == caller's enrolled
        // x25519`), so the coarse role gate here plus the crypto-self-enforcing
        // wrapped-key scope (§ 2.2) are the two layers. An MTA never processes
        // content, so it is excluded.
        // Nest-side segment-backup grant plane (nest-side segment backup,
        // slice 2): a user grants / revokes its OWN `NestBackupKey` to its
        // source nest and reads its own backup status. Owner-scoped in the
        // handler (the store is keyed on the authenticated actor), so the
        // coarse User gate here plus that keying are the two layers. Never a
        // bridge — a co-resident bridge has no reason to touch a user's
        // backup key.
        "fauna.backup.nest_key.grant" => matches!(class, User),
        "fauna.backup.nest_key.revoke" => matches!(class, User),
        "fauna.backup.status" => matches!(class, User),
        // Destination-side nest-writer grants (slice 3): the owner authorizes a
        // source nest to write its backup custody HERE, and revokes it. Same
        // owner-scoped keying and the same never-a-bridge reasoning — and
        // deliberately reachable by an ordinary User, since the whole point is
        // that the freeze-the-backup affordance works from the owner's client
        // with the source nest fully hostile.
        "fauna.backup.writer_grant.register" => matches!(class, User),
        "fauna.backup.writer_grant.revoke" => matches!(class, User),
        "fauna.backup.writer_grant.list" => matches!(class, User),
        // Source-side destination registry (slice 3): the owner tells its own
        // source nest where to back up. Same owner-scoped keying and the same
        // never-a-bridge reasoning as the two grant families above.
        "fauna.backup.destination.register" => matches!(class, User),
        "fauna.backup.destination.remove" => matches!(class, User),
        "fauna.backup.destination.list" => matches!(class, User),
        // Ordinary-folder coverage (`backup-destinations.md` § Ordinary-folder
        // coverage): the owner attaches/detaches its own folders on its own
        // registry rows. Same owner-scoped keying as the registry trio.
        "fauna.backup.destination.attach_folder" => matches!(class, User),
        "fauna.backup.destination.detach_folder" => matches!(class, User),
        // The client-device custodian's progress ack (the third destination
        // kind's nest half). `User` for the same owner-scoped reason as the
        // registry above — it is one of the owner's own devices reporting on
        // itself over its own authed connection, never a bridge and never a
        // foreign writer, which is why none of the adversarial-writer machinery
        // applies to it (`message-segment-store.md` § Client-device custodian
        // (pull) → *Check-in*).
        "fauna.backup.custodian.checkin" => matches!(class, User),
        // Custody-hosting registration (the custodian-nest runtime, stage b):
        // the HOST user deposits/rewrites and reads back its own nest's
        // hosting rows over its own authed connection. Owner-scoped keying
        // (`(caller, grant_id)`), never a bridge, never the custodian class —
        // the custodian-class doors serve a FOREIGN custodian pulling from an
        // owner's nest, while these serve the host user configuring its OWN
        // nest's hold for someone else.
        "fauna.custody.hosting.register" => matches!(class, User),
        "fauna.custody.hosting.list" => matches!(class, User),
        // The reclaim door — host-scoped like its two siblings (the
        // handler keys on the authenticated caller, so a foreign grant id
        // answers `removed: false` rather than touching another host's row).
        "fauna.custody.hosting.remove" => matches!(class, User),
        // The hosting registry's ADMIN surface: the
        // nest-wide list + remove the host-scoped doors above deliberately
        // are not. Admin-only — the *no client-causable unrecoverable nest
        // state* invariant's recoverability half, since any account holder
        // can plant rows over the User-class register door.
        "fauna.admin.custody_hosting.list" => matches!(class, Admin),
        "fauna.admin.custody_hosting.remove" => matches!(class, Admin),
        // The receipt deposit arm (stage c, item 6 of the device-or-nest
        // bullet): the custodian class's ONE write door, and deliberately the
        // narrowest write shape in the file — the handler verifies the
        // receipt's signature against the live capability row's holder key
        // BEFORE staging, staging is latest-per-grant and monotone, and the
        // only thing written is the owner's receipt-staging buffer (never the
        // owner's store). Ratified by the goal doc's REJECTED alternatives:
        // app-relay carriage and owner-side pulls both lose to this arm.
        "fauna.custody.receipt.deposit" => matches!(class, Custodian),
        // The owner's own fleet fetching its staged receipts at sync.
        "fauna.custody.receipt.list" => matches!(class, User),
        // Destination-side custody grace window (slice 3): the owner lists and
        // restores its own retained generations. Same owner-scoped keying; and
        // like `writer_grant.*` these must work from the owner's client with the
        // SOURCE nest fully hostile — recovering from a rogue source is exactly
        // what they are for, so routing them through any other class would
        // defeat the mitigation.
        "fauna.backup.generation.list" => matches!(class, User),
        "fauna.backup.generation.restore" => matches!(class, User),
        // The audit loop's destination-side read of live custody (leg (b)). Same
        // owner-scoped keying and the same hostile-source requirement: its whole
        // purpose is telling the owner what the *destination* holds without
        // asking the source nest, which is the party that wrote it.
        "fauna.backup.custody.list" => matches!(class, User),
        // Phase 3 of the re-seed ceremony. Same owner-scoped keying as its
        // custody siblings: the handler derives the set's owner from the
        // connection, so a caller can only ever materialize their own corpus.
        // Never a bridge — a bridge holds no NestBackupKey grant and has no
        // account to seed, so granting it the kind would only widen the surface.
        "fauna.backup.custody.materialize" => matches!(class, User),
        // The lived-in recovery, materialize's sibling: the same owner-scoped
        // keying (the set's owner is the connection's actor), and never a
        // bridge for the same reason — no NestBackupKey grant, no corpus.
        "fauna.backup.custody.recover" => matches!(class, User),
        // RecoveryKey registration (identity-succession slice 2): the owner
        // registers its own RecoveryKey over its own authenticated connection,
        // and the handler binds the record's `actor_id` to that connection's
        // actor. Never a bridge — a bridge holds no identity seed and no
        // RecoveryKey, so it could not produce a record that verifies; granting
        // it the kind would only widen the surface. (The *read* half,
        // `registration.chain`, is pre-identity and lives in
        // `pre_identity_allowlist`, not in this class matrix.)
        "fauna.recovery.registration.submit" => matches!(class, User),
        // The seed-escrow put (identity-succession slice 2): the owner stores
        // its own opaque blob over its own authenticated connection. Same
        // reasoning as the registration above, plus one of its own — the blob's
        // plaintext is the identity seed, so no non-owner class has any business
        // writing the row a recovery kit later opens. (The read half,
        // `escrow.{challenge,fetch}`, is pre-identity: a user who lost every
        // device has no session, so it lives in `pre_identity_allowlist` behind
        // the RecoveryKey signature, not in this class matrix.)
        "fauna.recovery.escrow.put" => matches!(class, User),
        // The escrow *presence* read, the one escrow kind that is neither
        // pre-identity nor a blob path. It exists because the surface that owns
        // the "registered, no escrow" state is signed-in (`ui/settings.md`
        // § Recovery kit) and cannot reach `escrow.fetch`, which is gated on the
        // offline-only RecoveryKey. Restricting it to `User` — the account
        // itself, taken from the connection — is what stops it being the
        // actor-id→"holds an escrow blob" oracle a pre-identity presence answer
        // would be. Presence only; the bytes stay behind the RecoveryKey.
        "fauna.recovery.escrow.status" => matches!(class, User),
        // The seed-initiated replacement window (identity-succession slice 2):
        // `request` parks a seed-alone replacement of the caller's OWN
        // RecoveryKey (the requester holds the seed, so a session exists);
        // `status` reads the caller's own pending state — the standing
        // "veto this if it wasn't you" banner every app renders. The veto
        // half (`replacement.{challenge,veto}`) is pre-identity: the owner may
        // be locked out by the thief, so it lives in `pre_identity_allowlist`
        // behind the RecoveryKey signature, not in this class matrix.
        "fauna.recovery.replacement.request" => matches!(class, User),
        "fauna.recovery.replacement.status" => matches!(class, User),
        // The succession commit stamp (identity-succession slice 3's authenticated
        // half). `submit`/`lookup` are pre-identity — the seed thief can revoke
        // every session, so the ceremony and the peer-facing statement read must
        // work with none — but this one serves a *server-observed* timestamp,
        // which is not part of any signed artifact and would be an anonymous
        // oracle for "when was this account compromised and recovered" if it sat
        // beside them. `User` — the account itself, taken from the connection —
        // is what makes it self-scoped; the request carries no actor id to widen.
        "fauna.recovery.succession.status" => matches!(class, User),
        // Clearing one of the caller's own owed nests (the nests its
        // predecessors were paired with when the succession burned the
        // pairings). `User`, like the status read that serves the list: the
        // handler takes the caller from the connection and refuses an entry
        // whose retired identity the caller did not succeed.
        "fauna.recovery.succession.owed_settle" => matches!(class, User),
        "fauna.capabilities.mint" => matches!(class, User),
        "fauna.capabilities.renew" => matches!(class, User),
        "fauna.capabilities.revoke" => matches!(class, User),
        // The reconcile sweep (`ui/nests.md` § Trust facet — grants →
        // *Reconcile*, ratified 2026-08-15): owner-scoped like mint/renew/
        // revoke (the handler filters `WHERE owner_actor_id = caller`), never
        // a bridge/holder — a holder has no business enumerating an owner's
        // OWN grant ids, only fetching the ones sealed to it.
        "fauna.capabilities.reconcile" => matches!(class, User),
        // `ThirdParty` reaches it as a SESSION kind (`principal_reach`): its
        // principal handler serves only grants the account wrapped to the
        // principal row's own `holder_x25519`.
        "fauna.capabilities.fetch" => matches!(class, BridgeMda | ContentProcessor | ThirdParty),
        // The re-score drain plane (design § 2.5 step 4) — the holder side, like
        // `fetch`: a content-processing bridge asks its obligation worklist and
        // writes back re-computed scores. Both are holder-scoped in the handler
        // (worklist to the holder's `content.read` grants; write-back to its
        // `content.label-write` grants), so the coarse role gate here plus the
        // per-grant scope check are the two layers. Never the MTA (no content).
        "fauna.capabilities.rescore_worklist" => matches!(class, BridgeMda | ContentProcessor),
        "fauna.capabilities.submit_scores" => matches!(class, BridgeMda | ContentProcessor),
        // The spam-baseline publish drain (`mail-spam.md` § Encrypted-mode
        // interaction, ratified 2026-07-13) — the third drain-plane instance,
        // holder-side like the two above: the poked holder pulls the pending
        // run's sealed-copy worklist (gated per-copy on its standing keyless
        // `content.read{spam-model}` grants in the handler) and writes back
        // its merged half. Never the MTA (no content leg), never a User/Admin
        // (the admin drives the publish via `fauna.bridges.publish_spam_baseline`;
        // the holder legs are service-user-only).
        "fauna.capabilities.spam_baseline_worklist" => {
            matches!(class, BridgeMda | ContentProcessor)
        }
        "fauna.capabilities.submit_spam_baseline" => {
            matches!(class, BridgeMda | ContentProcessor)
        }

        // ── Community-labeler registry (fauna.labelers.*) ─────────────────
        //
        // The publish/subscribe registry (labeler-registry design § 2).
        // publish/list/subscribe/unsubscribe are `User`: publishing is an
        // authenticated-actor operation (the publisher's identity signs the
        // artifact — no net-new human UI), and browse/subscribe/unsubscribe are
        // the content-owner's own catalog + subscription actions (owner ==
        // caller, enforced in the handler). Trust is gated at
        // inspect-before-subscribe + the user's revocable scoped grant + the
        // sandbox, not at this coarse role gate.
        //
        // `inspect` ALSO admits the capability-holder roles (BridgeMda |
        // ContentProcessor): a holder draining a `labeler:<id>` re-score
        // obligation must fetch the exact module + signed metadata to run
        // `label()` (Slice 3b, `rescore_drain.go`; mirrors the rescore plane's
        // `fetch`/`rescore_worklist`/`submit_scores` gate above). The reply is
        // the published, transparent artifact — public by design (the whole
        // point of inspect-before-subscribe) — so it carries no owner-scoped
        // secret; the holder re-verifies sig+hash before instantiation (B1).
        // The controversial-class feature plane's write half
        // (`dynamic-features.md` § Wire & data shape). The tier split IS this
        // pair of arms: `policy.update` writes the **admin** tier, so it is
        // Admin-only and must never fall to `User` (the blanket `Admin ⊇ User`
        // rule at the top of this function would otherwise be the only thing
        // separating them, and it runs the wrong way); `self_limits.update`
        // writes the caller's **own** tier and is User-class, with an admin
        // reaching it through that same blanket rule to set their own limits.
        //
        // Neither is reachable by any bridge class. A bridge is a service actor,
        // never a rule-setter — § The rule-setter model admits exactly
        // governments, app stores, guardians, admins and the user themselves,
        // and every one of those speaks through an app.
        "fauna.features.policy.update" => matches!(class, Admin),
        "fauna.features.self_limits.update" => matches!(class, User),
        // The authored-document reads split the same way and for the same
        // reason: `policy.get` reads the nest-wide admin document the admin
        // editor owns, `self_limits.get` the bearer's own tier.
        "fauna.features.policy.get" => matches!(class, Admin),
        "fauna.features.self_limits.get" => matches!(class, User),
        "fauna.labelers.publish" => matches!(class, User),
        "fauna.labelers.list" => matches!(class, User),
        "fauna.labelers.inspect" => matches!(class, User | BridgeMda | ContentProcessor),
        "fauna.labelers.subscribe" => matches!(class, User),
        "fauna.labelers.unsubscribe" => matches!(class, User),
        _ => false,
    }
}

/// How a kind in the `ThirdParty` ceiling is reached by a principal
/// (`apps/bridges.md` § Capability-allowlist enforcement → *How the class
/// meets the connection*, rule (c)).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PrincipalReach {
    /// Covered by holding a principal session at all — itself opened only
    /// with at least one Fauna-family scope (`authorization-server.md` § Scope
    /// grammar → *Session kinds need no scope*).
    Session,
    /// Covered by exactly one arm of the Fauna scope family.
    Scope(fauna_bridge_atproto::fauna_scope::FaunaScopeArm),
    /// Covered by an `identity.op` scope (the
    /// [`fauna_bridge_atproto::fauna_scope::FaunaScopeArm::IdentityOp`] arm)
    /// whose class is performed by this custodian — class-aware, not
    /// arm-wide (`authorization-server.md` § The Fauna family → *The second
    /// arm*): a future class held by another custodian must not open this
    /// custodian's door.
    IdentityOp(fauna_core::identity_op::Custodian),
}

/// The compiled kind → reach mapping beside the matrix: `Some` for exactly
/// the kinds [`is_permitted`] admits for [`CallerClass::ThirdParty`] (a test
/// holds the two equal), `None` for every other kind.
///
/// **A shipped arm's reach never widens** (`authorization-server.md` § Scope
/// grammar → *The Fauna family, exactly*): mapping one more kind onto an
/// existing arm enlarges every outstanding grant under it with no consent
/// event. New reach is a new arm.
pub fn principal_reach(kind: &str) -> Option<PrincipalReach> {
    use fauna_bridge_atproto::fauna_scope::FaunaScopeArm;
    match kind {
        "fauna.capabilities.fetch" => Some(PrincipalReach::Session),
        "fauna.feed.trending.posts" | "fauna.feed.local.posts" => {
            Some(PrincipalReach::Scope(FaunaScopeArm::FeedRead))
        }
        "fauna.nostr.bunker.bind" => Some(PrincipalReach::IdentityOp(
            fauna_core::identity_op::Custodian::NostrDepositedKey,
        )),
        // The `records` arm (`third-party-kinds.md` § The record doors). The
        // arm-wide check here is the gate's half; the per-kind half — the
        // request's `ext:<kind>` covered by one of the session's `records`
        // qualifiers — is each principal handler's, before anything else.
        "fauna.account.state.put" | "fauna.sync.changes.list" => {
            Some(PrincipalReach::Scope(FaunaScopeArm::Records))
        }
        // The `conversations:bridge` arm (`apps/bridges.md` § Bridge-kind
        // catalogue → Phase G).
        "fauna.bridges.conversation.deposit"
        | "fauna.bridges.conversation.outbox.fetch"
        | "fauna.bridges.conversation.outbox.ack"
        | "fauna.bridges.conversation.room.upsert"
        | "fauna.bridges.conversation.room.members"
        | "fauna.bridges.conversation.receipt"
        | "fauna.bridges.fetch_recipient_mls_pubkey" => {
            Some(PrincipalReach::Scope(FaunaScopeArm::ConversationsBridge))
        }
        // The `folder_deposit` arm (`file-sync.md` § Third-party deposit
        // ingress). The arm-wide check here is the gate's half; the
        // per-folder half — the request's folder named by one of the
        // session's `folder:deposit` qualifiers — is the handler's.
        "fauna.folders.deposit" => Some(PrincipalReach::Scope(FaunaScopeArm::FolderDeposit)),
        // The `events` arm (`transport.md` § Push events → *Third-party event
        // doors*): the poll is the arm's ceiling kind; the filtered push and
        // the HTTP long-poll are that kind's other faces, under the same
        // per-scope filter (`fauna_scope::event_reaches`), the handler's half.
        "fauna.events.poll" => Some(PrincipalReach::Scope(FaunaScopeArm::EventsSubscribe)),
        _ => None,
    }
}

/// Does `scope` name an `identity.op` class that `custodian` performs? The
/// arm match first ([`fauna_bridge_atproto::fauna_scope::arm_of`] refuses
/// every sovereign and unknown name), then the class's custodian.
fn identity_op_scope_reaches(scope: &str, custodian: fauna_core::identity_op::Custodian) -> bool {
    use fauna_bridge_atproto::fauna_scope::{FaunaScopeArm, arm_of, parse};
    if arm_of(scope) != Some(FaunaScopeArm::IdentityOp) {
        return false;
    }
    parse(scope)
        .and_then(|p| p.qualifier)
        .and_then(|q| fauna_core::identity_op::IdentityOpClass::parse(q).ok())
        .is_some_and(|class| class.custodian() == custodian)
}

/// Does `scopes` — a principal's row scopes intersected with its session
/// token's — cover `kind`? The scope half of the `ThirdParty` gate, checked at
/// the dispatch chokepoint beside [`is_permitted`]. False for any kind outside
/// the ceiling.
pub fn scope_covers(scopes: &[String], kind: &str) -> bool {
    match principal_reach(kind) {
        None => false,
        Some(PrincipalReach::Session) => true,
        Some(PrincipalReach::Scope(arm)) => scopes
            .iter()
            .any(|s| fauna_bridge_atproto::fauna_scope::arm_of(s) == Some(arm)),
        Some(PrincipalReach::IdentityOp(custodian)) => scopes
            .iter()
            .any(|s| identity_op_scope_reaches(s, custodian)),
    }
}

/// The error-code family a caller-class refusal of `kind` answers with at the
/// central capability gate (`routes.rs` gate 1d): `Some(family)` — the kind's
/// LEADING segment, with the `fauna.` prefix stripped first when present — iff
/// the kind is LISTED (an allowlist arm permits at least one class), `None` for
/// an unlisted kind.
///
/// The prefix is optional because four listed kinds are named outside the
/// `fauna.<ns>.…` shape — `bluesky.feed.thread` and the three `nostr.…` kinds
/// (the bridge surfaces keep their upstream protocol's own namespace). Deriving
/// only after a mandatory `fauna.` strip sent exactly those four to the central
/// bridges code, which is reserved for unlisted kinds and revoked actors — the
/// silent fallback `every_registered_kind_has_a_refusal_family` exists to catch,
/// and did (2026-08-17).
///
/// Refusal-code contract (ruled 2026-08-17; owner `api-layers.md` § Caller-class
/// authorization): a LISTED kind refused on caller class answers with its own
/// family's `fauna.<ns>.permission_denied` — the same code the handler's
/// `rpc_errors::permission_denied_ns` twin emits — because that refusal is a
/// statement about the kind's family contract, and apps + `tests/api/` branch
/// per-family. The central `fauna.bridges.permission_denied` stays for the two
/// refusals that are NOT statements about a kind's family: an UNLISTED kind
/// (no arm at all — normally impossible for a served kind, since
/// `every_registered_kind_is_gated` fails first) and the unknown/revoked-actor
/// arm, which denies EVERY kind — the every-kind shape the Go bridges'
/// revocation probe keys on (`wsrpc/reconnect.go`).
pub fn class_refusal_namespace(kind: &str) -> Option<&str> {
    use CallerClass::*;
    const ALL: [CallerClass; 8] = [
        BridgeMta,
        BridgeMda,
        Custodian,
        ContentProcessor,
        BridgeAtprotoPds,
        User,
        Admin,
        ThirdParty,
    ];
    if !ALL.iter().any(|&c| is_permitted(c, kind)) {
        return None;
    }
    kind.strip_prefix("fauna.")
        .unwrap_or(kind)
        .split('.')
        .next()
        .filter(|ns| !ns.is_empty())
}

/// Resolve a calling actor to a [`CallerClass`]. Returns `None` when the actor is
/// unknown or revoked — handlers map this to a permission error.
///
/// "Unknown or revoked" is now the literal truth, not an aspiration: the zero
/// actor, a `pending`/`revoked` bridge, an actor with no `users` row (never
/// registered, or deleted), a suspended actor, and a locked-out actor all resolve
/// to `None`. Because the central capability gate calls this per message, every
/// one of those bites at **dispatch**, on a connection the actor already holds.
///
/// The one deliberate exception is an **admin**, who resolves before the row
/// lookup and so is not subject to the lockout check — see the comment there.
pub async fn caller_class_for_actor(
    db: &CacheDb,
    actor_id: &[u8; 32],
) -> anyhow::Result<Option<CallerClass>> {
    // Zero-actor guard. The all-zero actor id
    // is the anonymous pre-identity placeholder; it is never a real enrolled
    // actor. Signed-payload anonymous writes already reject it (the signature is
    // over the zero key, which no caller holds), but this is the defense in
    // depth against a *future* allowlist mistake letting `[0u8;32]` fall through
    // to the `Some(User)` default below — refuse it explicitly, before any DB
    // lookup, so the fallthrough can never grant it a class.
    if actor_id == &[0u8; 32] {
        return Ok(None);
    }
    if let Some(su) = db.lookup_bridge_service_user(actor_id).await? {
        if su.status == BridgeStatus::Approved {
            return Ok(Some(match su.role {
                BridgeRole::Mta => CallerClass::BridgeMta,
                BridgeRole::Mda => CallerClass::BridgeMda,
                BridgeRole::ContentProcessor => CallerClass::ContentProcessor,
                BridgeRole::AtprotoPds => CallerClass::BridgeAtprotoPds,
            }));
        }
        return Ok(None);
    }
    if db.is_admin(&actor_id[..]).await? {
        return Ok(Some(CallerClass::Admin));
    }
    // Authority gate. One row read answers the whole question — "may this actor
    // act right now?" — across all three ways the answer is no: the row is gone,
    // the actor is suspended, or the actor is locked out.
    //
    // It runs on *every* authenticated RPC (the central capability gate in
    // `routes.rs` calls this per message, and each handler family calls it again
    // at its own top), which is what makes revocation bite at **dispatch** rather
    // than only at token mint:
    //
    //  * A **suspended** actor cannot dispatch on the WebSocket it already holds,
    //    and an open-registration nest — whose auth handshake never runs
    //    `check_actor_active` — is covered too. Before this existed, a suspended
    //    user kept full `User`-class dispatch until their token's TTL, and
    //    indefinitely on an open nest (`2026-07-09-family-safety-reach-enforcement-…-review.md`).
    //  * A **locked-out** actor is denied here, not merely at mint. Lockout is the
    //    *emergency* control, so mint-only enforcement left the widest hole: a
    //    bearer validated microseconds before `revoke_actor` runs yields a
    //    connection that registers in `WsState.subs` *after* `disconnect_actor`
    //    swept it, and nothing then denied it. Teardown closes live sockets; this
    //    closes the upgrade-time TOCTOU that teardown alone cannot — **for the
    //    per-actor paths only**, whose revoked thing is a property of this row.
    //    A per-TOKEN revoke (`fauna.sessions.{revoke,revoke_all}`) leaves the
    //    actor healthy, so this read returns `User` for its straggler for ever;
    //    that window is closed at registration instead, by
    //    `AppState::register_upgraded_connection`'s one re-read of the session
    //    (`transport-connection.md` § *The per-token twin* → *The upgrade
    //    window*). The next door that revokes a credential rather than an actor
    //    needs that shape, not this gate.
    //  * An actor with **no `users` row** — never registered, or dropped by
    //    `pending_actions::finalize_user_deletion` — resolves to `None` instead of
    //    falling through to `User`. That fallthrough made this function's contract
    //    ("None when the actor is unknown or revoked") a lie for deleted actors.
    //
    // Placed *after* the bridge and admin resolutions on purpose:
    //
    //  * An **admin** is un-suspendable and un-deletable (`require_not_admin` at
    //    both delete doors, plus `finalize_user_deletion`'s refusal), so a
    //    suspended sole admin — whom nobody could restore — stays unrepresentable.
    //    A locked-out admin consequently still dispatches: lockout is deliberately
    //    NOT extended to admins here, because a locked-out sole admin is that same
    //    off-box brick (`nest/common.md` § Client-state recoverability). Extending
    //    it needs the "cannot lock the last admin" guard suspension already has.
    //  * A **bridge** resolves above from `bridge_service_users` and never reaches
    //    this lookup. It does have a `users` row — `approve_bridge_service_user`
    //    runs `INSERT OR IGNORE INTO users` — and only `pending`/`revoked` bridges
    //    lack one, which return `None` above rather than reaching here.
    let Some(authority) = db.actor_authority(&actor_id[..]).await? else {
        // The custodian fallback (W8.6): an actor with no `users` row may
        // still be a custody-grant HOLDER — the key a live custody-class
        // capability row names. Checked here, after every registered
        // resolution, so no real user/bridge/admin can ever be downgraded;
        // re-derived from the rows on every dispatch, so a revoke (row
        // delete) ends the class immediately.
        if is_live_custody_holder(db, actor_id).await? {
            return Ok(Some(CallerClass::Custodian));
        }
        return Ok(None);
    };
    if authority.is_revoked_at(crate::db::now_epoch_secs()) {
        return Ok(None);
    }
    // Any other authenticated actor on the WS connection is a regular user: the
    // row exists, is not suspended, and carries no live lockout.
    Ok(Some(CallerClass::User))
}

/// Eight-hex-char prefix of an actor pubkey for log spans. Full pubkeys are
/// public-by-design but verbose; the prefix is enough to correlate events
/// across spans.
pub(crate) fn actor_prefix_hex(actor_id: &[u8; 32]) -> String {
    hex::encode(&actor_id[..4])
}

/// Is `key` the holder of at least one LIVE custody-class capability row?
/// The whole basis of [`CallerClass::Custodian`] — row-derived, per call,
/// no session registry (W8.6 pin N1). Blob decode failures skip the row
/// (an unreadable blob authorizes nothing).
async fn is_live_custody_holder(db: &CacheDb, key: &[u8; 32]) -> anyhow::Result<bool> {
    let now = crate::db::now_epoch_secs();
    for blob in db.fetch_capability_grants_for_holder(key, now).await? {
        if let Ok(grant) = fauna_mls::wrapped_blob::GrantBlob::from_canonical_bytes(&blob)
            // The start bound too — the storage filter can only express
            // "not expired".
            && fauna_mls::wrapped_blob::grant_window_is_open(&grant, now)
            && grant
                .scope
                .iter()
                .any(|t| t.class == fauna_mls::wrapped_blob::ScopeTuple::CLASS_CUSTODY)
        {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Shared caller-class permission gate: resolve `actor_id`'s [`CallerClass`]
/// via [`caller_class_for_actor`], then check `kind` against [`is_permitted`].
/// Every `*_handlers.rs` file used to hand-roll this exact control flow under
/// a `require_permission`/`require_admin`/`require_class` name — 46
/// near-identical copies, one pair of which (the bridge blob/routing
/// handlers) additionally audit-logged a denial. That audit log is now
/// unconditional here (priority #4: adopt the richest existing pattern, not
/// the simplest) rather than present at 2 of 46 sites.
///
/// The refusal code is derived HERE, not by the caller — byte-identical to
/// the central gate's arms (`api-layers.md` § Caller-class authorization →
/// *Refusal codes at the gate*): a listed kind refused on class gets its
/// family's `fauna.<ns>.permission_denied` ([`class_refusal_namespace`]); an
/// unknown/revoked actor — and the never-in-practice unlisted kind — gets the
/// central `fauna.bridges.permission_denied`. Callers pass only
/// `on_lookup_err` (their namespaced `internal`); per-module
/// `permission_denied` helpers remain for the finer within-class refusals
/// this gate cannot decide (caller scope, authorship). Returns the resolved
/// class so the sites that need it (not just pass/fail) can reuse it without
/// a second lookup.
pub async fn require_permission(
    db: &CacheDb,
    actor_id: &[u8; 32],
    kind: &str,
    on_lookup_err: impl FnOnce(anyhow::Error) -> RpcError,
) -> Result<CallerClass, RpcError> {
    let class = match caller_class_for_actor(db, actor_id)
        .await
        .map_err(on_lookup_err)?
    {
        Some(c) => c,
        None => {
            tracing::warn!(
                target: "permission_gate",
                kind,
                actor_prefix = actor_prefix_hex(actor_id),
                "permission denied: unknown or revoked actor"
            );
            return Err(crate::rpc_errors::central_permission_denied());
        }
    };
    if !is_permitted(class, kind) {
        tracing::warn!(
            target: "permission_gate",
            kind,
            actor_prefix = actor_prefix_hex(actor_id),
            ?class,
            "permission denied: kind not permitted for caller class"
        );
        return Err(match class_refusal_namespace(kind) {
            Some(ns) => crate::rpc_errors::permission_denied_ns(
                ns,
                format!("{kind} not permitted for caller class {class:?}"),
            ),
            None => crate::rpc_errors::central_permission_denied(),
        });
    }
    Ok(class)
}

/// The common case of [`require_permission`]: map lookup failures to
/// `rpc_errors::internal` and discard the resolved `CallerClass`. Replaces an
/// identical private wrapper 32 `*_handlers.rs` modules had each grown on
/// their own — the WS-RPC counterpart of the HTTP `BearerAuth` extractor gate.
pub(crate) async fn require_permission_default(
    state: &crate::routes::AppState,
    actor_id: &[u8; 32],
    kind: &str,
) -> Result<(), RpcError> {
    require_permission(&state.db, actor_id, kind, crate::rpc_errors::internal).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::bridge_service_users::BridgeRole;

    /// **The custody class reaches exactly two kinds, and a widening must be a
    /// deliberate act** (`account-data-plane.md` § Implementation status today
    /// → *Built — W8.6*, which states the pair by name).
    ///
    /// `CallerClass::Custodian` is the only class in this file granted to an
    /// actor with **no `users` row at all** — it is derived from a capability
    /// grant, so its holder is a third party the owner named, never someone the
    /// nest registered. That makes its reach the narrowest and most
    /// consequential set here: everything it touches is another account's data,
    /// admitted by a row rather than by identity.
    ///
    /// The risk this pins is silent widening. The class arrives at each door as
    /// one more name in a `matches!(class, …)` list, and adding it reads like
    /// the smallest edit in the file — which is exactly how a read capability
    /// grows a write door. It nearly happened in the slice that added the
    /// second kind: `fauna.segments.compact` sits on the line below
    /// `fauna.segments.list`, shares its prefix and its handler module, and is
    /// a **mutation** of the owner's store. It stayed `User`-only on purpose.
    ///
    /// So the assertion is over kinds this test **derives from the source
    /// rather than a hand-kept list** — a hand-kept list is the thing that goes
    /// stale, and "exactly three gates while 97 existed" is a mistake this
    /// codebase has already paid for once. Every kind literal in
    /// [`is_permitted`]'s own match is extracted and asked; adding an arm
    /// anywhere in the file therefore lands inside this test's scope
    /// automatically, with no maintenance.
    ///
    /// Widening the set is allowed — it is a design decision, not a defect —
    /// but it costs one deliberate line here, and the goal doc sentence this
    /// test cites has to move with it.
    /// Every `"fauna.…" =>` arm in [`is_permitted`]'s own match, read from this
    /// file's source so a new arm cannot be outside a reach test's scope.
    fn every_kind_arm_in_this_file() -> Vec<&'static str> {
        let src = include_str!("bridge_method_allowlist.rs");
        let mut kinds: Vec<&str> = Vec::new();
        for line in src.lines() {
            let t = line.trim_start();
            if t.starts_with("//") || !t.starts_with('"') {
                continue;
            }
            let Some(rest) = t.strip_prefix('"') else {
                continue;
            };
            let Some((kind, tail)) = rest.split_once('"') else {
                continue;
            };
            if kind.starts_with("fauna.") && tail.trim_start().starts_with("=>") {
                kinds.push(kind);
            }
        }
        kinds.sort_unstable();
        kinds.dedup();
        assert!(
            kinds.len() > 200,
            "the extractor found only {} kind arms",
            kinds.len()
        );
        kinds
    }

    /// **The `ThirdParty` ceiling, by name, and a widening is a deliberate
    /// act** — the custody pin's shape, for the same reason: every kind here is
    /// reach a party outside the nest holds over an account's data, admitted by
    /// a roster row and a token rather than by identity. Widening costs one
    /// line here and the `apps/bridges.md` / `authorization-server.md`
    /// sentences that name the set.
    #[test]
    fn the_third_party_class_reaches_exactly_its_ceiling() {
        const CEILING: [&str; 15] = [
            "fauna.account.state.put",
            "fauna.bridges.conversation.deposit",
            "fauna.bridges.conversation.outbox.ack",
            "fauna.bridges.conversation.outbox.fetch",
            "fauna.bridges.conversation.receipt",
            "fauna.bridges.conversation.room.members",
            "fauna.bridges.conversation.room.upsert",
            "fauna.bridges.fetch_recipient_mls_pubkey",
            "fauna.capabilities.fetch",
            "fauna.events.poll",
            "fauna.feed.local.posts",
            "fauna.feed.trending.posts",
            "fauna.folders.deposit",
            "fauna.nostr.bunker.bind",
            "fauna.sync.changes.list",
        ];
        let kinds = every_kind_arm_in_this_file();
        // Two `|`-joined arms sit on lines of their own, so the extractor
        // must see them — a kind it missed would make absence below vacuous.
        for k in CEILING {
            assert!(kinds.contains(&k), "{k} is not among the extracted arms");
        }
        let reached: Vec<&str> = kinds
            .iter()
            .copied()
            .filter(|k| is_permitted(CallerClass::ThirdParty, k))
            .collect();
        assert_eq!(
            reached, CEILING,
            "the ThirdParty ceiling changed. If deliberate, update this list AND the goal-doc \
             sentences naming the set; if not, a third party just grew a door."
        );
        // The account's own classes never leak in: Admin ⊇ User is a rule
        // about the account, and ThirdParty is not the account.
        assert!(!is_permitted(CallerClass::ThirdParty, "fauna.feed.posts"));
        assert!(!is_permitted(
            CallerClass::ThirdParty,
            "fauna.capabilities.mint"
        ));
    }

    /// The compiled kind → reach mapping is exactly the ceiling: every
    /// `ThirdParty` kind names a reach, no other kind does (rule (c)).
    #[test]
    fn principal_reach_is_defined_exactly_on_the_ceiling() {
        for kind in every_kind_arm_in_this_file() {
            assert_eq!(
                is_permitted(CallerClass::ThirdParty, kind),
                principal_reach(kind).is_some(),
                "{kind}: the matrix and the reach mapping disagree"
            );
        }
    }

    /// The predicate rule made structural (`authorization-server.md` § Scope
    /// grammar → *The Fauna family, exactly*, amended 2026-10-05), read per
    /// door: the grammar's live NEST-doored arms are exactly the arms the
    /// ceiling's kinds name — every such arm reaches at least one kind, and
    /// every scoped kind names a live arm — and every MDA-doored arm's relay
    /// kind is a live BridgeMda kind that no ceiling kind names.
    #[test]
    fn live_grammar_arms_equal_the_arms_the_ceiling_names() {
        use fauna_bridge_atproto::fauna_scope::{Door, FaunaScopeArm};
        for arm in FaunaScopeArm::ALL {
            if let Door::Mda { relay_kind } = arm.door() {
                assert!(
                    every_kind_arm_in_this_file().contains(&relay_kind),
                    "{arm:?}'s relay kind {relay_kind} is not in the matrix"
                );
                assert!(is_permitted(CallerClass::BridgeMda, relay_kind));
                assert!(!is_permitted(CallerClass::ThirdParty, relay_kind));
            }
        }
        let mut named: Vec<FaunaScopeArm> = every_kind_arm_in_this_file()
            .into_iter()
            .filter_map(principal_reach)
            .filter_map(|r| match r {
                PrincipalReach::Scope(arm) => Some(arm),
                PrincipalReach::IdentityOp(_) => Some(FaunaScopeArm::IdentityOp),
                PrincipalReach::Session => None,
            })
            .collect();
        let mut live: Vec<FaunaScopeArm> = FaunaScopeArm::ALL
            .iter()
            .copied()
            .filter(|a| a.door() == Door::Nest)
            .collect();
        named.sort_by_key(|a| a.advertised());
        live.sort_by_key(|a| a.advertised());
        named.dedup();
        assert_eq!(named, live);
        // Every custodian a built class names has a door in the ceiling, so
        // no grantable class is a scope string that reaches nothing.
        for class in fauna_core::identity_op::IdentityOpClass::ALL {
            assert!(
                every_kind_arm_in_this_file().into_iter().any(|k| {
                    principal_reach(k) == Some(PrincipalReach::IdentityOp(class.custodian()))
                }),
                "{} names a custodian with no ceiling kind",
                class.name()
            );
        }
    }

    #[test]
    fn scope_covers_reads_the_arm_a_kind_names() {
        let feed = vec!["fauna:feed:read".to_string()];
        let openid = vec!["openid".to_string()];
        assert!(scope_covers(&feed, "fauna.feed.trending.posts"));
        assert!(scope_covers(&feed, "fauna.feed.local.posts"));
        assert!(!scope_covers(&openid, "fauna.feed.trending.posts"));
        // A near-miss string is not the arm — whole-string, never prefix.
        assert!(!scope_covers(
            &["fauna:feed:read:home".to_string()],
            "fauna.feed.trending.posts"
        ));
        // A session kind needs no scope; a kind outside the ceiling is never
        // covered, whatever the set holds.
        assert!(scope_covers(&openid, "fauna.capabilities.fetch"));
        assert!(scope_covers(&[], "fauna.capabilities.fetch"));
        assert!(!scope_covers(&feed, "fauna.feed.posts"));
        assert!(!scope_covers(&feed, "fauna.capabilities.mint"));

        // The oracle's door is class-aware: either Nostr class covers bind,
        // `fauna:feed:read` does not, and a sovereign-named or unknown
        // `identity:op` string covers nothing.
        for s in [
            "fauna:identity:op:nostr.sign_event",
            "fauna:identity:op:nostr.nip44",
        ] {
            assert!(
                scope_covers(&[s.to_string()], "fauna.nostr.bunker.bind"),
                "{s}"
            );
            assert!(
                !scope_covers(&[s.to_string()], "fauna.feed.trending.posts"),
                "{s}"
            );
        }
        assert!(!scope_covers(&feed, "fauna.nostr.bunker.bind"));
        for s in [
            "fauna:identity:op:atproto.plc_rotate",
            "fauna:identity:op:identity.succession",
            "fauna:identity:op:mls.identity_root",
            "fauna:identity:op:nostr.nip04",
            "fauna:identity:op",
        ] {
            assert!(
                !scope_covers(&[s.to_string()], "fauna.nostr.bunker.bind"),
                "{s}"
            );
        }
    }

    /// `caller_class_for_actor` never answers `ThirdParty`, so a principal's
    /// class cannot be reached through an actor id — here, the shapes an
    /// actor can take that come closest: a plain user and the zero actor.
    #[tokio::test]
    async fn no_actor_resolves_to_the_third_party_class() {
        let db = CacheDb::open_in_memory().unwrap();
        assert_eq!(caller_class_for_actor(&db, &[0u8; 32]).await.unwrap(), None);
        assert_ne!(
            caller_class_for_actor(&db, &[0x11; 32]).await.unwrap(),
            Some(CallerClass::ThirdParty)
        );
    }

    #[test]
    fn the_custodian_class_reaches_exactly_its_declared_kinds() {
        // Two reads (the W8.6 pull pair) + ONE write: the stage-(c) receipt
        // deposit — signature-verified at the door against the live
        // capability row's holder key, latest-per-grant, touching only the
        // owner's receipt-staging buffer. The deliberate widening line this
        // test's doc demands, with the goal-doc sentence moved beside it
        // (`account-data-plane.md` § Implementation status today —
        // the *Ruled — the nest-custodian identity fact* entry, stage (c)).
        const EXPECTED: [&str; 3] = [
            "fauna.sync.changes.list",
            "fauna.segments.list",
            "fauna.custody.receipt.deposit",
        ];

        // Every `"fauna.…" =>` arm in this file's own match, read from source so
        // a new arm cannot be outside this test's reach.
        let src = include_str!("bridge_method_allowlist.rs");
        let mut kinds: Vec<&str> = Vec::new();
        for line in src.lines() {
            let t = line.trim_start();
            if t.starts_with("//") || !t.starts_with('"') {
                continue;
            }
            let Some(rest) = t.strip_prefix('"') else {
                continue;
            };
            let Some((kind, tail)) = rest.split_once('"') else {
                continue;
            };
            // Only match arms — `"kind" => …` — never a string used as a value.
            if kind.starts_with("fauna.") && tail.trim_start().starts_with("=>") {
                kinds.push(kind);
            }
        }
        kinds.sort_unstable();
        kinds.dedup();

        // Beside-control: an extractor that found nothing would pass vacuously
        // forever, which is the failure mode this whole idiom exists to avoid.
        assert!(
            kinds.len() > 200,
            "the extractor found only {} kind arms in this file; it is not reading what it \
             claims to read, and every assertion below is vacuous",
            kinds.len()
        );
        for expected in EXPECTED {
            assert!(
                kinds.contains(&expected),
                "{expected} is not among the extracted arms — the extractor missed a kind it \
                 must see, so absence below would prove nothing"
            );
        }

        let reached: Vec<&str> = kinds
            .iter()
            .copied()
            .filter(|k| is_permitted(CallerClass::Custodian, k))
            .collect();
        let mut want = EXPECTED.to_vec();
        want.sort_unstable();
        assert_eq!(
            reached, want,
            "the custody class's reach changed. A custodian is a third party holding another \
             account's data under a capability row, so every kind here is someone else's data \
             reachable without a `users` row. If the change is deliberate, update this list AND \
             `account-data-plane.md` § Implementation status today → *Built — W8.6*, which names \
             the set. If it is not, a capability just grew a door."
        );

        // The near-miss, stated as its own assertion because prefix-adjacency is
        // what makes it a live hazard rather than a hypothetical one.
        assert!(
            !is_permitted(CallerClass::Custodian, "fauna.segments.compact"),
            "compact MUTATES the owner's segment store; custody is a read capability"
        );
    }

    #[test]
    fn mda_can_fetch_mls_but_mta_cannot() {
        assert!(is_permitted(
            CallerClass::BridgeMda,
            "fauna.bridges.fetch_wrapped_mls_blob"
        ));
        assert!(!is_permitted(
            CallerClass::BridgeMta,
            "fauna.bridges.fetch_wrapped_mls_blob"
        ));
    }

    /// The DKIM key never leaves the nest: the two kinds that once carried it
    /// sealed — out to the MTA, and in from an admin — are permitted to no
    /// class.
    #[test]
    fn no_class_reaches_a_dkim_key_kind() {
        for kind in [
            "fauna.bridges.fetch_dkim_blob",
            "fauna.bridges.provision_dkim_blob",
        ] {
            for class in [
                CallerClass::BridgeMta,
                CallerClass::BridgeMda,
                CallerClass::Admin,
                CallerClass::User,
            ] {
                assert!(!is_permitted(class, kind), "{class:?} must not {kind}");
            }
        }
    }

    #[test]
    fn every_bridge_class_may_report_log_events_but_no_client_may() {
        // The sidecar log plane's enrolled-bridge leg (`observability.md` § The
        // sidecar log plane). All four bridge classes report their own
        // catalogued events — nest derives the `<source>:` attribution from the
        // class, so admitting all four opens no impersonation surface. The
        // User/Admin denial is the load-bearing half: a client has its own local
        // ring and Logs page, and a user-writable path into the *admin* ring
        // would be a spoofing surface with no legitimate caller. Note the
        // blanket `Admin ⊇ User` rule at the top of `is_permitted` — this kind
        // must stay out of the User set or admins inherit it silently.
        let kind = "fauna.bridges.report_log_events";
        for class in [
            CallerClass::BridgeMta,
            CallerClass::BridgeMda,
            CallerClass::ContentProcessor,
            CallerClass::BridgeAtprotoPds,
        ] {
            assert!(is_permitted(class, kind), "{class:?} must reach {kind}");
        }
        assert!(!is_permitted(CallerClass::User, kind), "a user must not");
        assert!(!is_permitted(CallerClass::Admin, kind), "nor an admin");
    }

    #[test]
    fn user_and_admin_can_provision_own_mail_blobs() {
        // The three enable-mail provision kinds are self-scoped (the handler
        // keys the blob on the connection actor), so both User and Admin may
        // provision their OWN — an admin enabling their own mail must not hit
        // permission_denied. Bridges never self-provision these.
        for kind in [
            "fauna.bridges.provision_wrapped_mls_blob",
            "fauna.bridges.provision_mls_snapshot_blob",
            "fauna.bridges.provision_wrapped_submission_token",
        ] {
            assert!(is_permitted(CallerClass::User, kind), "User must {kind}");
            assert!(is_permitted(CallerClass::Admin, kind), "Admin must {kind}");
            assert!(
                !is_permitted(CallerClass::BridgeMta, kind),
                "BridgeMta must not {kind}"
            );
            assert!(
                !is_permitted(CallerClass::BridgeMda, kind),
                "BridgeMda must not {kind}"
            );
        }
    }

    #[test]
    fn admin_can_provision_tls() {
        assert!(is_permitted(
            CallerClass::Admin,
            "fauna.bridges.provision_tls_cert_blob"
        ));
        assert!(!is_permitted(
            CallerClass::Admin,
            "fauna.bridges.fetch_wrapped_mls_blob"
        ));
    }

    #[test]
    fn nostr_bunker_kinds_are_user_class() {
        // The NIP-46 bunker control plane: the user's own connected-apps
        // roster. Same User-class contract as the email cluster; bridge actors
        // have no role on another user's signer roster.
        for kind in [
            "fauna.nostr.bunker.create_invite",
            "fauna.nostr.bunker.list",
            "fauna.nostr.bunker.revoke",
            "fauna.nostr.bunker.set_label",
        ] {
            assert!(is_permitted(CallerClass::User, kind), "User must {kind}");
            assert!(is_permitted(CallerClass::Admin, kind), "Admin must {kind}");
            assert!(
                !is_permitted(CallerClass::BridgeMta, kind),
                "BridgeMta must not {kind}"
            );
            assert!(
                !is_permitted(CallerClass::BridgeMda, kind),
                "BridgeMda must not {kind}"
            );
        }
        // The oracle's door is the principal's alone: no actor class — the
        // account's own apps included — reaches `bind`.
        for class in [
            CallerClass::User,
            CallerClass::Admin,
            CallerClass::BridgeMta,
            CallerClass::BridgeMda,
            CallerClass::ContentProcessor,
        ] {
            assert!(
                !is_permitted(class, "fauna.nostr.bunker.bind"),
                "{class:?} must not bind"
            );
        }
        assert!(is_permitted(
            CallerClass::ThirdParty,
            "fauna.nostr.bunker.bind"
        ));
    }

    #[test]
    fn nostr_zap_signer_kinds_are_user_class() {
        // The NIP-57 trust root: the payee's own list of signers allowed to
        // speak for their money. Same User-class contract as the bunker
        // roster — a bridge actor editing a payee's trust root would be able
        // to make its own forged receipts believable, which is the whole
        // attack the designation exists to stop.
        for kind in [
            "fauna.nostr.zap_signers.list",
            "fauna.nostr.zap_signers.add",
            "fauna.nostr.zap_signers.remove",
        ] {
            assert!(is_permitted(CallerClass::User, kind), "User must {kind}");
            assert!(is_permitted(CallerClass::Admin, kind), "Admin must {kind}");
            assert!(
                !is_permitted(CallerClass::BridgeMta, kind),
                "BridgeMta must not {kind}"
            );
            assert!(
                !is_permitted(CallerClass::BridgeMda, kind),
                "BridgeMda must not {kind}"
            );
        }
    }

    #[test]
    fn admin_only_can_provision_self_signed_cert() {
        let kind = "fauna.bridges.provision_self_signed_cert";
        assert!(is_permitted(CallerClass::Admin, kind));
        for class in [
            CallerClass::User,
            CallerClass::BridgeMta,
            CallerClass::BridgeMda,
        ] {
            assert!(!is_permitted(class, kind), "{class:?} must not {kind}");
        }
    }

    #[test]
    fn admin_only_can_list_and_revoke_dkim() {
        for kind in [
            "fauna.bridges.list_dkim_selectors",
            "fauna.bridges.revoke_dkim_blob",
            "fauna.bridges.force_rotate_dkim",
        ] {
            assert!(is_permitted(CallerClass::Admin, kind), "admin {kind}");
            for class in [
                CallerClass::User,
                CallerClass::BridgeMta,
                CallerClass::BridgeMda,
            ] {
                assert!(!is_permitted(class, kind), "{class:?} must not {kind}");
            }
        }
    }

    /// The roster read is User-callable (the trust facet's holder discovery,
    /// nests.md § Where logic lives) with the HANDLER class-scoping the reply;
    /// bridges never read the roster. Closes the "non-admin holder discovery"
    /// gap without a second wire kind.
    #[test]
    fn any_user_may_list_service_users_bridges_may_not() {
        use CallerClass::*;
        let kind = "fauna.bridges.list_service_users";
        assert!(is_permitted(User, kind), "User lists (holder view)");
        assert!(is_permitted(Admin, kind), "Admin lists (full roster)");
        for class in [BridgeMta, BridgeMda, ContentProcessor] {
            assert!(!is_permitted(class, kind), "{class:?} must not {kind}");
        }
    }

    /// Self-attestation covers every bridge class that can hold a seal target:
    /// an approved external content-processor must be able to attest its
    /// x25519 like the mail roles (the handler enforces `ed25519_pubkey ==
    /// connected actor`, so the widened gate exposes no cross-row surface).
    #[test]
    fn every_bridge_class_may_self_attest_its_key() {
        use CallerClass::*;
        let kind = "fauna.bridges.register_service_user";
        for class in [BridgeMta, BridgeMda, ContentProcessor] {
            assert!(is_permitted(class, kind), "{class:?} attests its own key");
        }
        for class in [User, Admin] {
            assert!(!is_permitted(class, kind), "{class:?} must not {kind}");
        }
    }

    #[test]
    fn admin_only_can_list_dns_records() {
        for kind in [
            "fauna.dns.list_records",
            "fauna.dns.verify_records",
            "fauna.dns.set_host_address",
            "fauna.dns.probe_txt_visible",
        ] {
            assert!(is_permitted(CallerClass::Admin, kind), "Admin must {kind}");
            for class in [
                CallerClass::User,
                CallerClass::BridgeMta,
                CallerClass::BridgeMda,
            ] {
                assert!(!is_permitted(class, kind), "{class:?} must not {kind}");
            }
        }
    }

    #[test]
    fn admin_only_can_designate_apex_actor() {
        // Apex hosting is a deployment-wide admin setting (the web analogue of
        // catch-all mail), so a plain User must NOT be able to designate or read
        // it — unlike the per-user `fauna.web.{publish,domain}.*` kinds.
        for kind in ["fauna.web.set_apex_actor", "fauna.web.get_apex_actor"] {
            assert!(is_permitted(CallerClass::Admin, kind), "Admin must {kind}");
            for class in [
                CallerClass::User,
                CallerClass::BridgeMta,
                CallerClass::BridgeMda,
            ] {
                assert!(!is_permitted(class, kind), "{class:?} must not {kind}");
            }
        }
    }

    #[test]
    fn admin_management_cluster_is_admin_only() {
        // C2: tiers / invite codes / invite requests / admins — Admin-only,
        // matching the HTTP twins' `AdminBearerAuth`.
        for kind in [
            "fauna.admin.tiers.list",
            "fauna.admin.tiers.create",
            "fauna.admin.tiers.update",
            "fauna.admin.membership_tiers.list",
            "fauna.admin.membership_tiers.set",
            "fauna.admin.membership_tiers.clear",
            "fauna.admin.invite_codes.list",
            "fauna.admin.invite_codes.create",
            "fauna.admin.invite_codes.delete",
            "fauna.admin.invite_requests.list",
            "fauna.admin.invite_requests.approve",
            "fauna.admin.invite_requests.deny",
            "fauna.admin.admins.list",
            "fauna.admin.admins.add",
            "fauna.admin.admins.remove",
        ] {
            assert!(is_permitted(CallerClass::Admin, kind), "Admin must {kind}");
            for class in [
                CallerClass::User,
                CallerClass::BridgeMta,
                CallerClass::BridgeMda,
            ] {
                assert!(!is_permitted(class, kind), "{class:?} must not {kind}");
            }
        }
    }

    #[test]
    fn plugins_cluster_is_admin_only() {
        for kind in [
            "fauna.plugins.install",
            "fauna.plugins.list",
            "fauna.plugins.uninstall",
        ] {
            assert!(is_permitted(CallerClass::Admin, kind), "Admin must {kind}");
            for class in [
                CallerClass::User,
                CallerClass::BridgeMta,
                CallerClass::BridgeMda,
                CallerClass::Custodian,
                CallerClass::ContentProcessor,
                CallerClass::BridgeAtprotoPds,
                CallerClass::ThirdParty,
            ] {
                assert!(!is_permitted(class, kind), "{class:?} must not {kind}");
            }
        }
    }

    #[test]
    fn admin_stats_audit_ops_cluster_is_admin_only() {
        // C3: stats / status / audit / cluster / gc / worker / cross-actor
        // pending-actions — Admin-only, matching the HTTP twins' `AdminBearerAuth`.
        for kind in [
            "fauna.admin.stats",
            "fauna.admin.status",
            "fauna.admin.audit.list",
            "fauna.admin.audit.integrity",
            "fauna.admin.cluster.status",
            "fauna.admin.gc",
            "fauna.admin.worker.status",
            "fauna.admin.pending_actions.list",
        ] {
            assert!(is_permitted(CallerClass::Admin, kind), "Admin must {kind}");
            for class in [
                CallerClass::User,
                CallerClass::BridgeMta,
                CallerClass::BridgeMda,
            ] {
                assert!(!is_permitted(class, kind), "{class:?} must not {kind}");
            }
        }
    }

    #[test]
    fn admin_folders_services_is_admin_only() {
        // C5: folders / services — Admin-only, matching the HTTP twins'
        // `AdminBearerAuth`. Distinct from the user-class `fauna.folders.*`
        // (B14) cluster.
        for kind in [
            "fauna.admin.folders.create",
            "fauna.admin.folders.get",
            "fauna.admin.folders.add_member",
            "fauna.admin.services.list",
            "fauna.admin.services.update",
            // C4 pairings RETIRED — see `fauna.pair.{add,revoke,list}` tests.
        ] {
            assert!(is_permitted(CallerClass::Admin, kind), "Admin must {kind}");
            for class in [
                CallerClass::User,
                CallerClass::BridgeMta,
                CallerClass::BridgeMda,
            ] {
                assert!(!is_permitted(class, kind), "{class:?} must not {kind}");
            }
        }
    }

    #[test]
    fn admin_factory_reset_is_admin_only() {
        // Destructive return-to-fresh. Admin-only (the dogfood box's admin is
        // effectively the superadmin); never a user or a bridge.
        let kind = "fauna.admin.factory_reset";
        assert!(is_permitted(CallerClass::Admin, kind), "Admin must {kind}");
        for class in [
            CallerClass::User,
            CallerClass::BridgeMta,
            CallerClass::BridgeMda,
        ] {
            assert!(!is_permitted(class, kind), "{class:?} must not {kind}");
        }
    }

    #[test]
    fn user_and_admin_can_provision_recipient_mls_pubkey() {
        // The recipient pubkey is the user's MSEK-derived standing key
        // (key-material-hierarchy.md § Path B-sibling-2): the user
        // self-registers their own at enable-mail. The allowlist permits
        // both User and Admin; the `target == caller` restriction for the
        // User class lives in the handler, not here.
        assert!(is_permitted(
            CallerClass::User,
            "fauna.bridges.provision_recipient_mls_pubkey"
        ));
        assert!(is_permitted(
            CallerClass::Admin,
            "fauna.bridges.provision_recipient_mls_pubkey"
        ));
    }

    #[test]
    fn bridges_cannot_provision_recipient_mls_pubkey() {
        // A bridge service-user reads the table on every inbound DATA
        // (fetch_recipient_mls_pubkey) but must never write it — writes
        // are user/admin provisioning, not a per-message-class action.
        assert!(!is_permitted(
            CallerClass::BridgeMta,
            "fauna.bridges.provision_recipient_mls_pubkey"
        ));
        assert!(!is_permitted(
            CallerClass::BridgeMda,
            "fauna.bridges.provision_recipient_mls_pubkey"
        ));
    }

    #[test]
    fn validate_recipient_permitted_for_both_bridge_roles() {
        // The MTA resolves RCPT TO; the MDA resolves the MUA-AUTH username to an
        // actor before fetching that actor's wrapped-MSEK blob
        // (mail-mda-7 / `internal/mda/imap/auth.go`). Both bridge roles need it.
        assert!(is_permitted(
            CallerClass::BridgeMta,
            "fauna.bridges.validate_recipient"
        ));
        assert!(is_permitted(
            CallerClass::BridgeMda,
            "fauna.bridges.validate_recipient"
        ));
    }

    #[test]
    fn validate_recipient_denied_for_non_bridge_actors() {
        assert!(!is_permitted(
            CallerClass::User,
            "fauna.bridges.validate_recipient"
        ));
        assert!(!is_permitted(
            CallerClass::Admin,
            "fauna.bridges.validate_recipient"
        ));
    }

    #[test]
    fn whoami_permitted_for_both_bridge_roles() {
        assert!(is_permitted(CallerClass::BridgeMta, "fauna.bridges.whoami"));
        assert!(is_permitted(CallerClass::BridgeMda, "fauna.bridges.whoami"));
    }

    #[test]
    fn whoami_denied_for_non_bridge_actors() {
        assert!(!is_permitted(CallerClass::User, "fauna.bridges.whoami"));
        assert!(!is_permitted(CallerClass::Admin, "fauna.bridges.whoami"));
    }

    #[test]
    fn unknown_kind_denied_for_all() {
        assert!(!is_permitted(
            CallerClass::Admin,
            "fauna.bridges.does_not_exist"
        ));
    }

    #[tokio::test]
    async fn caller_class_resolves_approved_bridge() {
        let db = CacheDb::open_in_memory().unwrap();
        let bridge_pk = [11u8; 32];
        db.create_pending_bridge_service_user(&bridge_pk, BridgeRole::Mda, "mda-1")
            .await
            .unwrap();
        db.upsert_bridge_x25519(&bridge_pk, &[22u8; 32])
            .await
            .unwrap();
        db.approve_bridge_service_user(&bridge_pk, None)
            .await
            .unwrap();

        let class = caller_class_for_actor(&db, &bridge_pk).await.unwrap();
        assert_eq!(class, Some(CallerClass::BridgeMda));
    }

    #[tokio::test]
    async fn caller_class_resolves_content_processor() {
        // An approved 'content-processor' service user classifies as
        // ContentProcessor (design § 2.4 — the generic capability-holder role).
        let db = CacheDb::open_in_memory().unwrap();
        let cp_pk = [55u8; 32];
        db.create_pending_bridge_service_user(&cp_pk, BridgeRole::ContentProcessor, "cp-1")
            .await
            .unwrap();
        db.upsert_bridge_x25519(&cp_pk, &[66u8; 32]).await.unwrap();
        db.approve_bridge_service_user(&cp_pk, None).await.unwrap();

        let class = caller_class_for_actor(&db, &cp_pk).await.unwrap();
        assert_eq!(class, Some(CallerClass::ContentProcessor));
    }

    #[tokio::test]
    async fn caller_class_resolves_atproto_pds() {
        // An approved 'atproto.pds' service user classifies as its own dedicated
        // BridgeAtprotoPds class (the out-of-process ATProto PDS host bridge,
        // atproto-pds-bridge.md) — NOT a mail bridge class.
        let db = CacheDb::open_in_memory().unwrap();
        let pds_pk = [77u8; 32];
        db.create_pending_bridge_service_user(&pds_pk, BridgeRole::AtprotoPds, "atproto-1")
            .await
            .unwrap();
        db.upsert_bridge_x25519(&pds_pk, &[88u8; 32]).await.unwrap();
        db.approve_bridge_service_user(&pds_pk, None).await.unwrap();

        let class = caller_class_for_actor(&db, &pds_pk).await.unwrap();
        assert_eq!(class, Some(CallerClass::BridgeAtprotoPds));
    }

    #[test]
    fn atproto_pds_reach_is_lifecycle_plus_f1_auth_least_privilege() {
        use CallerClass::*;
        // The PDS bridge holds the shared bridge-lifecycle RPCs plus its own
        // F1 auth-core surface (verifier fetch + session registry —
        // `atproto-pds-full.md` § WS-RPC kind surface), and `fetch_tls_cert_blob`
        // for terminating its own `pds.<domain>` TLS (§ Wire & process topology,
        // F1 packaging resolution) — seal-on-read binds the blob to the caller's
        // own x25519, so this is shared TLS infra, not mail key material…
        for kind in [
            "fauna.bridges.whoami",
            "fauna.bridges.register_service_user",
            "fauna.bridges.fetch_config",
            "fauna.bridges.fetch_tls_cert_blob",
            "fauna.bridges.atproto.fetch_app_credential_verifiers",
            "fauna.bridges.atproto.record_session",
            "fauna.bridges.atproto.refresh_session",
            "fauna.bridges.atproto.end_session",
            "fauna.bridges.atproto.fetch_session_secret_blob",
            "fauna.bridges.atproto.fetch_issuer_jwks",
            "fauna.bridges.atproto.fetch_preferences",
            "fauna.bridges.atproto.store_preferences",
        ] {
            assert!(
                is_permitted(BridgeAtprotoPds, kind),
                "{kind} must be permitted for the PDS bridge"
            );
        }
        // …NONE of the mail bridge's DKIM/outbound/IMAP surface (a PDS bridge
        // reaching mail key material or the mail data plane would be a
        // least-privilege breach)…
        for kind in [
            "fauna.bridges.fetch_wrapped_mls_blob",
            "fauna.bridges.enqueue_outbound_mail",
            "fauna.bridges.list_mailboxes",
            "fauna.bridges.validate_recipient",
        ] {
            assert!(
                !is_permitted(BridgeAtprotoPds, kind),
                "{kind} must be DENIED to the PDS bridge (not a mail bridge)"
            );
        }
        // …and NONE of the user-facing atproto kinds (mint/revoke stay
        // client-driven; the bridge cannot mint itself credentials or flip
        // the kill-switch).
        for kind in [
            "fauna.bridges.atproto.provision_app_credential",
            "fauna.bridges.atproto.list_app_credentials",
            "fauna.bridges.atproto.revoke_app_credential",
            "fauna.bridges.atproto.list_sessions",
            "fauna.bridges.atproto.list_grants",
            "fauna.bridges.atproto.revoke_session",
            "fauna.bridges.atproto.set_external_apps_enabled",
        ] {
            assert!(
                !is_permitted(BridgeAtprotoPds, kind),
                "{kind} must be DENIED to the PDS bridge (user-facing)"
            );
        }
    }

    #[test]
    fn atproto_user_kinds_are_user_scoped_and_denied_to_mail_bridges() {
        use CallerClass::*;
        for kind in [
            "fauna.bridges.atproto.provision_app_credential",
            "fauna.bridges.atproto.list_app_credentials",
            "fauna.bridges.atproto.revoke_app_credential",
            "fauna.bridges.atproto.list_sessions",
            "fauna.bridges.atproto.list_grants",
            "fauna.bridges.atproto.revoke_session",
            "fauna.bridges.atproto.set_external_apps_enabled",
        ] {
            assert!(is_permitted(User, kind), "{kind}: User");
            assert!(is_permitted(Admin, kind), "{kind}: Admin inherits User");
            for class in [BridgeMta, BridgeMda, ContentProcessor] {
                assert!(!is_permitted(class, kind), "{kind}: denied to {class:?}");
            }
        }
        // The bridge session-registry kinds are equally out of reach for
        // ordinary users and mail bridges.
        for kind in [
            "fauna.bridges.atproto.fetch_app_credential_verifiers",
            "fauna.bridges.atproto.record_session",
            "fauna.bridges.atproto.refresh_session",
            "fauna.bridges.atproto.end_session",
            "fauna.bridges.atproto.fetch_session_secret_blob",
            "fauna.bridges.atproto.fetch_issuer_jwks",
            "fauna.bridges.atproto.fetch_preferences",
            "fauna.bridges.atproto.store_preferences",
        ] {
            for class in [User, Admin, BridgeMta, BridgeMda, ContentProcessor] {
                assert!(!is_permitted(class, kind), "{kind}: denied to {class:?}");
            }
        }
    }

    #[test]
    fn capability_mint_renew_revoke_are_owner_only() {
        use CallerClass::*;
        for kind in [
            "fauna.capabilities.mint",
            "fauna.capabilities.renew",
            "fauna.capabilities.revoke",
        ] {
            // Owner-minted: the content-owning user (Admin inherits User).
            assert!(is_permitted(User, kind), "{kind} permitted for User");
            assert!(
                is_permitted(Admin, kind),
                "{kind} permitted for Admin (inherits User)"
            );
            // Never a bridge / holder — mint authority is the owner's, off-box.
            assert!(
                !is_permitted(BridgeMta, kind),
                "{kind} denied for BridgeMta"
            );
            assert!(
                !is_permitted(BridgeMda, kind),
                "{kind} denied for BridgeMda"
            );
            assert!(
                !is_permitted(ContentProcessor, kind),
                "{kind} denied for ContentProcessor"
            );
        }
    }

    #[test]
    fn capability_reconcile_is_owner_only() {
        use CallerClass::*;
        let kind = "fauna.capabilities.reconcile";
        // Owner-scoped, like mint/renew/revoke (Admin inherits User).
        assert!(is_permitted(User, kind), "{kind} permitted for User");
        assert!(
            is_permitted(Admin, kind),
            "{kind} permitted for Admin (inherits User)"
        );
        // Never a bridge/holder — a holder fetches its OWN sealed grants
        // (`fetch`), never enumerates an owner's ids.
        assert!(
            !is_permitted(BridgeMta, kind),
            "{kind} denied for BridgeMta"
        );
        assert!(
            !is_permitted(BridgeMda, kind),
            "{kind} denied for BridgeMda"
        );
        assert!(
            !is_permitted(ContentProcessor, kind),
            "{kind} denied for ContentProcessor"
        );
    }

    #[test]
    fn backup_nest_key_grant_plane_is_owner_only() {
        use CallerClass::*;
        for kind in [
            "fauna.backup.nest_key.grant",
            "fauna.backup.nest_key.revoke",
            "fauna.backup.status",
            "fauna.backup.writer_grant.register",
            "fauna.backup.writer_grant.revoke",
            "fauna.backup.writer_grant.list",
            "fauna.backup.destination.register",
            "fauna.backup.destination.remove",
            "fauna.backup.destination.list",
            "fauna.backup.destination.attach_folder",
            "fauna.backup.destination.detach_folder",
            "fauna.backup.generation.list",
            "fauna.backup.generation.restore",
            "fauna.backup.custody.list",
            "fauna.backup.custody.materialize",
            "fauna.backup.custody.recover",
            "fauna.backup.custodian.checkin",
            "fauna.recovery.registration.submit",
            "fauna.recovery.escrow.put",
            "fauna.recovery.escrow.status",
            "fauna.recovery.replacement.request",
            "fauna.recovery.replacement.status",
            "fauna.recovery.succession.status",
            "fauna.recovery.succession.owed_settle",
        ] {
            // The user acts on its OWN NestBackupKey / reads its own status
            // (Admin inherits User).
            assert!(is_permitted(User, kind), "{kind} permitted for User");
            assert!(
                is_permitted(Admin, kind),
                "{kind} permitted for Admin (inherits User)"
            );
            // Never a bridge — a co-resident bridge has no backup role.
            assert!(
                !is_permitted(BridgeMta, kind),
                "{kind} denied for BridgeMta"
            );
            assert!(
                !is_permitted(BridgeMda, kind),
                "{kind} denied for BridgeMda"
            );
            assert!(
                !is_permitted(ContentProcessor, kind),
                "{kind} denied for ContentProcessor"
            );
        }
    }

    /// The hosting doors are the HOST user configuring its own nest's hold —
    /// never the custodian class, whose doors serve a FOREIGN custodian
    /// pulling from an owner's nest. A custodian-class caller reaching the
    /// hosting registry would let any admitted foreign custodian plant pull
    /// instructions on someone else's nest.
    #[test]
    fn custody_hosting_plane_is_host_user_only() {
        use CallerClass::*;
        for kind in [
            "fauna.custody.hosting.register",
            "fauna.custody.hosting.list",
            "fauna.custody.hosting.remove",
        ] {
            assert!(is_permitted(User, kind), "{kind} permitted for User");
            assert!(
                is_permitted(Admin, kind),
                "{kind} permitted for Admin (inherits User)"
            );
            assert!(
                !is_permitted(Custodian, kind),
                "{kind} denied for Custodian"
            );
            assert!(
                !is_permitted(BridgeMta, kind),
                "{kind} denied for BridgeMta"
            );
            assert!(
                !is_permitted(BridgeMda, kind),
                "{kind} denied for BridgeMda"
            );
            assert!(
                !is_permitted(ContentProcessor, kind),
                "{kind} denied for ContentProcessor"
            );
        }
        // The registry's admin surface is ADMIN-only —
        // a plain User must never reach the nest-wide list or drop another
        // host's rows.
        for kind in [
            "fauna.admin.custody_hosting.list",
            "fauna.admin.custody_hosting.remove",
        ] {
            assert!(is_permitted(Admin, kind), "{kind} permitted for Admin");
            for class in [User, Custodian, BridgeMta, BridgeMda, ContentProcessor] {
                assert!(!is_permitted(class, kind), "{kind} denied for {class:?}");
            }
        }
    }

    /// The receipt arm's polarity: deposit is CUSTODIAN-only (the one write
    /// door the class holds — never a user impersonating a custodian without
    /// the bearer, never a bridge), and the fetch is the OWNER's alone.
    #[test]
    fn receipt_deposit_is_custodian_only_and_list_is_owner_only() {
        use CallerClass::*;
        let deposit = "fauna.custody.receipt.deposit";
        assert!(is_permitted(Custodian, deposit), "deposit for Custodian");
        for class in [User, Admin, BridgeMta, BridgeMda, ContentProcessor] {
            assert!(
                !is_permitted(class, deposit),
                "deposit denied for {class:?}"
            );
        }
        let list = "fauna.custody.receipt.list";
        assert!(is_permitted(User, list), "list for User");
        assert!(is_permitted(Admin, list), "list for Admin (inherits User)");
        for class in [Custodian, BridgeMta, BridgeMda, ContentProcessor] {
            assert!(!is_permitted(class, list), "list denied for {class:?}");
        }
    }

    #[test]
    fn capability_fetch_is_holder_only() {
        use CallerClass::*;
        let kind = "fauna.capabilities.fetch";
        // The serve MDA and a generic content-processor are the grant holders.
        assert!(is_permitted(BridgeMda, kind), "fetch for BridgeMda");
        assert!(
            is_permitted(ContentProcessor, kind),
            "fetch for ContentProcessor"
        );
        // Not the owner (that's the mint side), not the MTA (no content leg).
        assert!(!is_permitted(User, kind), "fetch denied for User");
        assert!(!is_permitted(Admin, kind), "fetch denied for Admin");
        assert!(!is_permitted(BridgeMta, kind), "fetch denied for BridgeMta");
    }

    #[test]
    fn capability_drain_plane_is_holder_only() {
        use CallerClass::*;
        // The re-score drain plane (design § 2.5 step 4) is the holder side, like
        // `fetch`: a content-processing bridge reads its worklist + writes back
        // scores. Never the owner (that's mint), never the MTA (no content leg).
        for kind in [
            "fauna.capabilities.rescore_worklist",
            "fauna.capabilities.submit_scores",
            // The spam-baseline publish drain shares the exact same holder
            // gate (`mail-spam.md` § Encrypted-mode interaction).
            "fauna.capabilities.spam_baseline_worklist",
            "fauna.capabilities.submit_spam_baseline",
        ] {
            assert!(is_permitted(BridgeMda, kind), "{kind} for BridgeMda");
            assert!(
                is_permitted(ContentProcessor, kind),
                "{kind} for ContentProcessor"
            );
            assert!(!is_permitted(User, kind), "{kind} denied for User");
            assert!(!is_permitted(Admin, kind), "{kind} denied for Admin");
            assert!(
                !is_permitted(BridgeMta, kind),
                "{kind} denied for BridgeMta"
            );
        }
    }

    #[test]
    fn labeler_registry_gate() {
        use CallerClass::*;
        // publish/list/subscribe/unsubscribe are the content-owner's own actions
        // (owner == caller enforced in the handler); a holder never publishes or
        // subscribes.
        for kind in [
            "fauna.labelers.publish",
            "fauna.labelers.list",
            "fauna.labelers.subscribe",
            "fauna.labelers.unsubscribe",
        ] {
            assert!(is_permitted(User, kind), "{kind} permitted for User");
            assert!(
                !is_permitted(BridgeMda, kind),
                "{kind} denied for BridgeMda"
            );
            assert!(
                !is_permitted(ContentProcessor, kind),
                "{kind} denied for ContentProcessor"
            );
        }
        // inspect ALSO admits the holder roles: the re-score drain fetches the
        // transparent, public module + signed metadata to run `label()` (Slice
        // 3b). No owner-scoped secret crosses; B1 re-verify covers store swaps.
        let inspect = "fauna.labelers.inspect";
        assert!(is_permitted(User, inspect), "inspect for User");
        assert!(is_permitted(BridgeMda, inspect), "inspect for BridgeMda");
        assert!(
            is_permitted(ContentProcessor, inspect),
            "inspect for ContentProcessor"
        );
        // Still never the MTA (no content-processing leg).
        assert!(
            !is_permitted(BridgeMta, inspect),
            "inspect denied for BridgeMta"
        );
    }

    #[tokio::test]
    async fn caller_class_returns_none_for_pending_bridge() {
        let db = CacheDb::open_in_memory().unwrap();
        let bridge_pk = [33u8; 32];
        db.create_pending_bridge_service_user(&bridge_pk, BridgeRole::Mta, "mta-1")
            .await
            .unwrap();
        let class = caller_class_for_actor(&db, &bridge_pk).await.unwrap();
        assert_eq!(class, None);
    }

    /// Note the *absent* `users` row: an admin resolves before the authority
    /// lookup, so it needs none. This pins that ordering. It is no longer a
    /// production-reachable shape — `fauna.admin.admins.add` now requires the
    /// target to be a registered user, at the door and in the executor — but the
    /// ordering itself is load-bearing (it is what keeps a suspended or
    /// locked-out sole admin from bricking the nest).
    #[tokio::test]
    async fn caller_class_resolves_admin() {
        let db = CacheDb::open_in_memory().unwrap();
        let admin_pk = [44u8; 32];
        db.add_admin_actor(&admin_pk[..]).await.unwrap();
        let class = caller_class_for_actor(&db, &admin_pk).await.unwrap();
        assert_eq!(class, Some(CallerClass::Admin));
    }

    /// An actor with **no `users` row** resolves to no class at all. This test
    /// used to be `caller_class_falls_back_to_user_for_unknown`, asserting the
    /// opposite — it pinned the bug: a deleted actor's still-open socket kept
    /// `User` class, and the function's "None when unknown" contract was a lie.
    #[tokio::test]
    async fn caller_class_is_none_for_an_actor_with_no_users_row() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = [55u8; 32];
        assert_eq!(caller_class_for_actor(&db, &actor).await.unwrap(), None);

        // Seeding the row is the only thing that grants the class.
        db.create_user_with_handle(&actor, "personal", "known", None)
            .await
            .unwrap();
        assert_eq!(
            caller_class_for_actor(&db, &actor).await.unwrap(),
            Some(CallerClass::User)
        );
    }

    /// Lockout bites at **dispatch**, not merely at token mint — closing, for
    /// this per-actor revocation, the upgrade-time TOCTOU that teardown alone
    /// cannot: a bearer validated just before `revoke_actor` runs yields a
    /// connection that registers in `WsState.subs` after `disconnect_actor`
    /// swept it. (A per-token session revoke leaves the actor untouched, so
    /// this gate cannot see its straggler; that window is closed at
    /// registration and pinned by `routes.rs::upgrade_revocation_race_tests`.)
    ///
    /// A *lapsed* lock is not a revocation: it expires with nobody clearing it.
    #[tokio::test]
    async fn caller_class_returns_none_for_a_locked_out_user() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = [77u8; 32];
        db.create_user_with_handle(&actor, "personal", "locked", None)
            .await
            .unwrap();
        assert_eq!(
            caller_class_for_actor(&db, &actor).await.unwrap(),
            Some(CallerClass::User)
        );

        let now = crate::db::now_epoch_secs();
        db.set_locked_until(&actor[..], Some(now + 3600))
            .await
            .unwrap();
        assert_eq!(caller_class_for_actor(&db, &actor).await.unwrap(), None);

        // A lock in the past has lapsed — dispatch resumes with no admin action.
        db.set_locked_until(&actor[..], Some(now - 1))
            .await
            .unwrap();
        assert_eq!(
            caller_class_for_actor(&db, &actor).await.unwrap(),
            Some(CallerClass::User)
        );

        // …and an explicitly cleared lock likewise.
        db.set_locked_until(&actor[..], None).await.unwrap();
        assert_eq!(
            caller_class_for_actor(&db, &actor).await.unwrap(),
            Some(CallerClass::User)
        );
    }

    /// A locked-out **admin** keeps its class — deliberately. Admins resolve
    /// before the authority lookup, so lockout is not extended to them: a
    /// locked-out sole admin would be the same off-box brick that
    /// `require_not_admin` keeps unrepresentable for suspension. Changing this
    /// needs a "cannot lock the last admin" guard first.
    #[tokio::test]
    async fn caller_class_keeps_admin_class_for_a_locked_out_admin() {
        let db = CacheDb::open_in_memory().unwrap();
        let admin_pk = [88u8; 32];
        db.create_user_with_handle(&admin_pk, "personal", "root", None)
            .await
            .unwrap();
        db.add_admin_actor(&admin_pk[..]).await.unwrap();
        db.set_locked_until(&admin_pk[..], Some(crate::db::now_epoch_secs() + 3600))
            .await
            .unwrap();
        assert_eq!(
            caller_class_for_actor(&db, &admin_pk).await.unwrap(),
            Some(CallerClass::Admin)
        );
    }

    /// The suspension gate. A suspended actor resolves to no class
    /// at all, so every `User`-class kind is refused — on every RPC, which is
    /// what closes the "already-held token / already-open WebSocket" window and
    /// the open-registration nest where auth never read `users.suspended`.
    #[tokio::test]
    async fn caller_class_returns_none_for_suspended_user() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = [66u8; 32];
        db.create_user_with_handle(&actor, "personal", "mallory", None)
            .await
            .unwrap();
        assert_eq!(
            caller_class_for_actor(&db, &actor).await.unwrap(),
            Some(CallerClass::User)
        );

        db.suspend_user_now(&actor, "abuse", "abuse").await.unwrap();
        assert_eq!(caller_class_for_actor(&db, &actor).await.unwrap(), None);

        // …and restoring the user restores dispatch, with no re-onboarding.
        db.cancel_eviction(&actor).await.unwrap();
        assert_eq!(
            caller_class_for_actor(&db, &actor).await.unwrap(),
            Some(CallerClass::User)
        );
    }

    /// An admin is resolved *before* the suspension gate, so an admin row
    /// carrying `suspended = 1` (reachable: `admin.add` promotes a suspended
    /// user with no suspension check) keeps its class rather than bricking the
    /// nest.
    #[tokio::test]
    async fn caller_class_keeps_admin_class_for_a_suspended_admin() {
        let db = CacheDb::open_in_memory().unwrap();
        let admin_pk = [77u8; 32];
        db.create_user_with_handle(&admin_pk, "personal", "root", None)
            .await
            .unwrap();
        db.add_admin_actor(&admin_pk[..]).await.unwrap();
        db.suspend_user_now(&admin_pk, "suspended before promotion", "other")
            .await
            .unwrap();
        assert_eq!(
            caller_class_for_actor(&db, &admin_pk).await.unwrap(),
            Some(CallerClass::Admin),
            "a suspended admin must stay able to restore themselves"
        );
    }

    #[tokio::test]
    async fn caller_class_returns_none_for_zero_actor() {
        // § L6 zero-actor guard: the anonymous `[0u8;32]` placeholder must never
        // resolve to a class (it would otherwise hit the User fallthrough).
        let db = CacheDb::open_in_memory().unwrap();
        let class = caller_class_for_actor(&db, &[0u8; 32]).await.unwrap();
        assert_eq!(class, None);
    }

    #[test]
    fn provision_calendar_mda_permitted() {
        assert!(is_permitted(
            CallerClass::BridgeMda,
            "fauna.bridges.provision_calendar"
        ));
    }

    #[test]
    fn provision_calendar_user_permitted() {
        assert!(is_permitted(
            CallerClass::User,
            "fauna.bridges.provision_calendar"
        ));
    }

    #[test]
    fn provision_calendar_mta_denied() {
        assert!(!is_permitted(
            CallerClass::BridgeMta,
            "fauna.bridges.provision_calendar"
        ));
    }

    #[test]
    fn provision_calendar_admin_permitted() {
        // admin ⊇ user: provision_calendar is caller-scoped (the caller's own
        // calendar), so an admin (also a user) inherits it for its own account.
        assert!(is_permitted(
            CallerClass::Admin,
            "fauna.bridges.provision_calendar"
        ));
    }

    #[test]
    fn list_calendars_mda_permitted() {
        assert!(is_permitted(
            CallerClass::BridgeMda,
            "fauna.bridges.list_calendars"
        ));
    }

    #[test]
    fn list_calendars_mta_denied() {
        assert!(!is_permitted(
            CallerClass::BridgeMta,
            "fauna.bridges.list_calendars"
        ));
    }

    #[test]
    fn list_calendars_user_permitted() {
        // Decision B (events.md § Persistence, 2026-06-01): a Fauna app
        // reads its own calendars over the bridge RPCs directly. Caller-scoped
        // to the connection actor in the handler (mirrors provision_calendar).
        assert!(is_permitted(
            CallerClass::User,
            "fauna.bridges.list_calendars"
        ));
    }

    #[test]
    fn list_calendars_admin_permitted() {
        // admin ⊇ user: inherits the User grant for its own account.
        assert!(is_permitted(
            CallerClass::Admin,
            "fauna.bridges.list_calendars"
        ));
    }

    #[test]
    fn query_events_mda_permitted() {
        assert!(is_permitted(
            CallerClass::BridgeMda,
            "fauna.bridges.query_events"
        ));
    }

    #[test]
    fn query_events_mta_denied() {
        assert!(!is_permitted(
            CallerClass::BridgeMta,
            "fauna.bridges.query_events"
        ));
    }

    #[test]
    fn query_events_user_permitted() {
        // Decision B: a Fauna app reads its own calendar's events directly.
        // Caller-scoped to the connection actor in the handler.
        assert!(is_permitted(
            CallerClass::User,
            "fauna.bridges.query_events"
        ));
    }

    #[test]
    fn query_events_admin_permitted() {
        assert!(is_permitted(
            CallerClass::Admin,
            "fauna.bridges.query_events"
        ));
    }

    #[test]
    fn put_event_ciphertext_mda_permitted() {
        assert!(is_permitted(
            CallerClass::BridgeMda,
            "fauna.bridges.put_event_ciphertext"
        ));
    }

    #[test]
    fn place_inbound_invite_is_the_mtas_alone() {
        let kind = "fauna.bridges.place_inbound_invite";
        assert!(is_permitted(CallerClass::BridgeMta, kind));
        for other in [
            CallerClass::BridgeMda,
            CallerClass::User,
            CallerClass::Admin,
        ] {
            assert!(
                !is_permitted(other, kind),
                "{other:?} must not place invitations"
            );
        }
    }

    #[test]
    fn put_event_ciphertext_mta_denied() {
        assert!(!is_permitted(
            CallerClass::BridgeMta,
            "fauna.bridges.put_event_ciphertext"
        ));
    }

    #[test]
    fn put_event_ciphertext_user_permitted() {
        // Decision B: a Fauna app seals an event body client-side and writes
        // it to its OWN calendar directly (caller-scoped in the handler — the
        // client can only write under its own actor_id, nest never sees
        // plaintext). The MDA keeps the same grant for served MUAs.
        assert!(is_permitted(
            CallerClass::User,
            "fauna.bridges.put_event_ciphertext"
        ));
    }

    #[test]
    fn put_event_ciphertext_admin_permitted() {
        assert!(is_permitted(
            CallerClass::Admin,
            "fauna.bridges.put_event_ciphertext"
        ));
    }

    #[test]
    fn delete_event_mda_permitted() {
        assert!(is_permitted(
            CallerClass::BridgeMda,
            "fauna.bridges.delete_event"
        ));
    }

    #[test]
    fn delete_event_mta_denied() {
        assert!(!is_permitted(
            CallerClass::BridgeMta,
            "fauna.bridges.delete_event"
        ));
    }

    #[test]
    fn delete_event_user_permitted() {
        // Decision B: a Fauna app deletes events from its OWN calendar.
        assert!(is_permitted(
            CallerClass::User,
            "fauna.bridges.delete_event"
        ));
    }

    #[test]
    fn delete_event_admin_permitted() {
        assert!(is_permitted(
            CallerClass::Admin,
            "fauna.bridges.delete_event"
        ));
    }

    #[test]
    fn sync_calendar_since_mda_permitted() {
        assert!(is_permitted(
            CallerClass::BridgeMda,
            "fauna.bridges.sync_calendar_since"
        ));
    }

    #[test]
    fn sync_calendar_since_mta_denied() {
        assert!(!is_permitted(
            CallerClass::BridgeMta,
            "fauna.bridges.sync_calendar_since"
        ));
    }

    #[test]
    fn sync_calendar_since_user_permitted() {
        // Decision B: a Fauna app RFC-6578 sync-polls its OWN calendar.
        assert!(is_permitted(
            CallerClass::User,
            "fauna.bridges.sync_calendar_since"
        ));
    }

    #[test]
    fn sync_calendar_since_admin_permitted() {
        assert!(is_permitted(
            CallerClass::Admin,
            "fauna.bridges.sync_calendar_since"
        ));
    }

    // ── Phase-E CardDAV allowlist arms (contacts twins of the CalDAV arms) ──

    const CARDDAV_DATA_PLANE_KINDS: &[&str] = &[
        "fauna.bridges.provision_addressbook",
        "fauna.bridges.list_addressbooks",
        "fauna.bridges.query_cards",
        "fauna.bridges.put_card_ciphertext",
        "fauna.bridges.delete_card",
        "fauna.bridges.sync_addressbook_since",
        "fauna.bridges.delete_addressbook",
    ];

    #[test]
    fn carddav_data_plane_mda_and_user_permitted() {
        for kind in CARDDAV_DATA_PLANE_KINDS {
            assert!(
                is_permitted(CallerClass::BridgeMda, kind),
                "MDA must be permitted for {kind}"
            );
            // admin ⊇ user: caller-scoped in the handler (own address book).
            assert!(
                is_permitted(CallerClass::User, kind),
                "User must be permitted for {kind}"
            );
            assert!(
                is_permitted(CallerClass::Admin, kind),
                "Admin (⊇ User) must be permitted for {kind}"
            );
        }
    }

    #[test]
    fn carddav_data_plane_mta_denied() {
        for kind in CARDDAV_DATA_PLANE_KINDS {
            assert!(
                !is_permitted(CallerClass::BridgeMta, kind),
                "MTA must be denied for {kind}"
            );
        }
    }

    #[test]
    fn set_carddav_enabled_admin_only() {
        assert!(is_permitted(
            CallerClass::Admin,
            "fauna.bridges.set_carddav_enabled"
        ));
        assert!(!is_permitted(
            CallerClass::User,
            "fauna.bridges.set_carddav_enabled"
        ));
        assert!(!is_permitted(
            CallerClass::BridgeMda,
            "fauna.bridges.set_carddav_enabled"
        ));
    }

    #[test]
    fn search_messages_mda_permitted() {
        assert!(is_permitted(
            CallerClass::BridgeMda,
            "fauna.bridges.search_messages"
        ));
    }

    #[test]
    fn search_messages_mta_denied() {
        assert!(!is_permitted(
            CallerClass::BridgeMta,
            "fauna.bridges.search_messages"
        ));
    }

    #[test]
    fn search_messages_user_denied() {
        assert!(!is_permitted(
            CallerClass::User,
            "fauna.bridges.search_messages"
        ));
    }

    #[test]
    fn search_messages_admin_denied() {
        assert!(!is_permitted(
            CallerClass::Admin,
            "fauna.bridges.search_messages"
        ));
    }

    #[test]
    fn get_quota_mda_permitted() {
        assert!(is_permitted(
            CallerClass::BridgeMda,
            "fauna.bridges.get_quota"
        ));
    }

    #[test]
    fn get_quota_mta_denied() {
        assert!(!is_permitted(
            CallerClass::BridgeMta,
            "fauna.bridges.get_quota"
        ));
    }

    #[test]
    fn get_quota_user_denied() {
        assert!(!is_permitted(CallerClass::User, "fauna.bridges.get_quota"));
    }

    #[test]
    fn get_quota_admin_denied() {
        assert!(!is_permitted(CallerClass::Admin, "fauna.bridges.get_quota"));
    }

    // I5 Phase D.5 — fetch_recipient_mls_pubkey + fetch_recipient_index_key
    // widened from BridgeMta-only to BridgeMta | BridgeMda.  The MDA needs
    // both pubkeys for APPEND seal-to-self (IMAP write surface).  Both
    // pubkeys are public-key material; the gate exists for caller-class
    // scoping, not privacy.

    #[test]
    fn fetch_recipient_mls_pubkey_mta_permitted() {
        assert!(is_permitted(
            CallerClass::BridgeMta,
            "fauna.bridges.fetch_recipient_mls_pubkey"
        ));
    }

    #[test]
    fn fetch_recipient_mls_pubkey_mda_permitted() {
        assert!(is_permitted(
            CallerClass::BridgeMda,
            "fauna.bridges.fetch_recipient_mls_pubkey"
        ));
    }

    #[test]
    fn fetch_recipient_mls_pubkey_user_denied() {
        assert!(!is_permitted(
            CallerClass::User,
            "fauna.bridges.fetch_recipient_mls_pubkey"
        ));
    }

    #[test]
    fn fetch_recipient_mls_pubkey_admin_denied() {
        assert!(!is_permitted(
            CallerClass::Admin,
            "fauna.bridges.fetch_recipient_mls_pubkey"
        ));
    }

    #[test]
    fn fetch_recipient_index_key_mta_permitted() {
        assert!(is_permitted(
            CallerClass::BridgeMta,
            "fauna.bridges.fetch_recipient_index_key"
        ));
    }

    #[test]
    fn fetch_recipient_index_key_mda_permitted() {
        assert!(is_permitted(
            CallerClass::BridgeMda,
            "fauna.bridges.fetch_recipient_index_key"
        ));
    }

    #[test]
    fn fetch_recipient_index_key_user_denied() {
        assert!(!is_permitted(
            CallerClass::User,
            "fauna.bridges.fetch_recipient_index_key"
        ));
    }

    #[test]
    fn fetch_recipient_index_key_admin_denied() {
        assert!(!is_permitted(
            CallerClass::Admin,
            "fauna.bridges.fetch_recipient_index_key"
        ));
    }

    #[test]
    fn fetch_spam_model_mda_permitted() {
        assert!(is_permitted(
            CallerClass::BridgeMda,
            "fauna.bridges.fetch_spam_model"
        ));
    }

    #[test]
    fn fetch_spam_model_user_admin_permitted() {
        // The User/Fauna-app leg is enabled;
        // User/Admin are caller-scoped to
        // their own model in the handler. The allowlist layer permits them.
        for class in [CallerClass::User, CallerClass::Admin] {
            assert!(
                is_permitted(class, "fauna.bridges.fetch_spam_model"),
                "{class:?} must be permitted (caller-scoped in the handler)"
            );
        }
    }

    #[test]
    fn fetch_spam_model_mta_denied() {
        // The MTA never serves a per-user mailbox session — only the MDA
        // (trusted naming) and User/Admin (caller-scoped) may fetch a model.
        assert!(
            !is_permitted(CallerClass::BridgeMta, "fauna.bridges.fetch_spam_model"),
            "BridgeMta must be denied (it serves no AUTH'd mailbox session)"
        );
    }

    #[test]
    fn put_spam_model_user_admin_permitted() {
        // The opaque sealed-write-back (client's own-key write path). User/Admin
        // are caller-scoped to their own model by construction — the handler
        // ignores `req.actor_id` for this class. The allowlist layer permits them.
        for class in [CallerClass::User, CallerClass::Admin] {
            assert!(
                is_permitted(class, "fauna.bridges.put_spam_model"),
                "{class:?} must be permitted (caller-scoped in the handler)"
            );
        }
    }

    #[test]
    fn put_spam_model_mda_permitted() {
        // Widened for leg 2: the MDA agent-side `\Junk`-train re-seal writes the
        // served actor's re-sealed model back via trusted-naming (the same class
        // set as the read twin `fetch_spam_model`; the handler names the target
        // from `req.actor_id` and bounds it with `require_local_mail_serving`).
        assert!(
            is_permitted(CallerClass::BridgeMda, "fauna.bridges.put_spam_model"),
            "BridgeMda must be permitted for the leg-2 agent-side write-back"
        );
    }

    #[test]
    fn put_spam_model_mta_denied() {
        assert!(
            !is_permitted(CallerClass::BridgeMta, "fauna.bridges.put_spam_model"),
            "BridgeMta must be denied (it serves no AUTH'd mailbox session)"
        );
    }

    #[test]
    fn publish_spam_baseline_admin_only() {
        // The deployment-baseline publish is an admin-only deployment action.
        assert!(is_permitted(
            CallerClass::Admin,
            "fauna.bridges.publish_spam_baseline"
        ));
        for class in [
            CallerClass::User,
            CallerClass::BridgeMda,
            CallerClass::BridgeMta,
        ] {
            assert!(
                !is_permitted(class, "fauna.bridges.publish_spam_baseline"),
                "{class:?} must NOT be able to publish the deployment baseline"
            );
        }
    }

    #[test]
    fn get_spam_baseline_state_admin_only() {
        // The baseline state read is an admin surface; no user or bridge
        // learns when the deployment published, over how many, or that the
        // last run was deferred.
        assert!(is_permitted(
            CallerClass::Admin,
            "fauna.bridges.get_spam_baseline_state"
        ));
        for class in [
            CallerClass::User,
            CallerClass::BridgeMda,
            CallerClass::BridgeMta,
        ] {
            assert!(
                !is_permitted(class, "fauna.bridges.get_spam_baseline_state"),
                "{class:?} must NOT be able to read the deployment baseline's state"
            );
        }
    }

    #[test]
    fn set_baseline_contribution_user_admin_only() {
        // The per-user opt-in toggle: User/Admin set their own flag
        // (caller-scoped in the handler); the bridges never set preferences.
        for class in [CallerClass::User, CallerClass::Admin] {
            assert!(
                is_permitted(class, "fauna.bridges.set_baseline_contribution"),
                "{class:?} must be permitted to set their own contribution"
            );
        }
        for class in [CallerClass::BridgeMda, CallerClass::BridgeMta] {
            assert!(
                !is_permitted(class, "fauna.bridges.set_baseline_contribution"),
                "{class:?} must NOT set a user's contribution flag"
            );
        }
    }

    // ── I4 Phase D.5 outbound-queue gating ────────────────────────

    #[test]
    fn outbound_queue_methods_permitted_for_mta_only() {
        for kind in [
            "fauna.bridges.fetch_outbound_due",
            "fauna.bridges.mark_outbound_delivered",
            "fauna.bridges.mark_outbound_failed",
            "fauna.bridges.mark_outbound_bounced",
            "fauna.bridges.fetch_mta_sts_policy",
            "fauna.bridges.fetch_tlsa",
            "fauna.bridges.resolve_mx",
            "fauna.bridges.fetch_recipient_forward_config",
            "fauna.bridges.forward_message",
            "fauna.bridges.send_auto_reply",
            "fauna.bridges.fetch_recipient_filters",
            "fauna.bridges.decode_srs_bounce",
        ] {
            assert!(
                is_permitted(CallerClass::BridgeMta, kind),
                "{kind} should be permitted for MTA"
            );
            for class in [
                CallerClass::BridgeMda,
                CallerClass::User,
                CallerClass::Admin,
            ] {
                assert!(
                    !is_permitted(class, kind),
                    "{kind} should be denied for {class:?}"
                );
            }
        }
    }

    #[test]
    fn enqueue_outbound_mail_permitted_for_mta_and_mda_only() {
        // The MTA (submission gateway) and the MDA (server-side
        // `calendar-auto-schedule` gateway) both enqueue outbound mail; the
        // MDA path is caller-scoped to the AUTH'd organizer in the handler
        // (caldav-server.md § Server-side auto-schedule). User/Admin never
        // reach it directly — they use `fauna.email.send`.
        let kind = "fauna.bridges.enqueue_outbound_mail";
        assert!(is_permitted(CallerClass::BridgeMta, kind));
        assert!(is_permitted(CallerClass::BridgeMda, kind));
        for class in [CallerClass::User, CallerClass::Admin] {
            assert!(
                !is_permitted(class, kind),
                "{kind} should be denied for {class:?}"
            );
        }
    }

    #[test]
    fn rotate_srs_secret_admin_only() {
        assert!(is_permitted(
            CallerClass::Admin,
            "fauna.bridges.rotate_srs_secret"
        ));
        for class in [
            CallerClass::BridgeMta,
            CallerClass::BridgeMda,
            CallerClass::User,
        ] {
            assert!(
                !is_permitted(class, "fauna.bridges.rotate_srs_secret"),
                "rotate_srs_secret must be Admin-only, denied for {class:?}"
            );
        }
    }

    #[test]
    fn the_principal_roster_kinds_are_the_accounts_own() {
        for kind in ["fauna.principals.list", "fauna.principals.revoke"] {
            assert!(is_permitted(CallerClass::User, kind), "{kind}: User");
            assert!(
                is_permitted(CallerClass::Admin, kind),
                "{kind}: Admin ⊇ User"
            );
            for class in [
                CallerClass::BridgeMta,
                CallerClass::BridgeMda,
                CallerClass::ContentProcessor,
                CallerClass::BridgeAtprotoPds,
            ] {
                assert!(
                    !is_permitted(class, kind),
                    "{kind} must be denied to {class:?}"
                );
            }
        }
    }

    #[test]
    fn the_oauth_issuer_kinds_are_admin_only() {
        // Both halves, and the User class explicitly: `issuer_key_status`
        // returns no secret, so "it is only a read" is exactly the argument
        // that would widen it — but a user learning when the deployment last
        // rotated its issuer key learns about the deployment's incident
        // history, not about themselves.
        for kind in [
            "fauna.oauth.issuer_key_status",
            "fauna.oauth.rotate_issuer_key",
            "fauna.oauth.force_rotate_issuer_key",
            "fauna.oauth.force_rotate_session_secret",
        ] {
            assert!(is_permitted(CallerClass::Admin, kind));
            for class in [
                CallerClass::BridgeMta,
                CallerClass::BridgeMda,
                CallerClass::User,
            ] {
                assert!(
                    !is_permitted(class, kind),
                    "{kind} must be Admin-only, denied for {class:?}"
                );
            }
        }
    }

    #[test]
    fn the_consent_start_kinds_are_user_only() {
        for kind in [
            "fauna.oauth.consent.lookup_code",
            "fauna.oauth.consent.open_handoff",
            "fauna.oauth.consent.block_client",
            "fauna.oauth.consent.list_blocked_clients",
        ] {
            assert!(
                is_permitted(CallerClass::User, kind),
                "{kind} is the user's own act"
            );
            for class in [CallerClass::BridgeMta, CallerClass::BridgeMda] {
                assert!(
                    !is_permitted(class, kind),
                    "{kind} must never be bridge-callable, denied for {class:?}"
                );
            }
        }
    }

    // I5 Phase D.6 — mailbox admin (CREATE / DELETE / RENAME)

    #[test]
    fn create_mailbox_mda_permitted() {
        assert!(is_permitted(
            CallerClass::BridgeMda,
            "fauna.bridges.create_mailbox"
        ));
    }

    #[test]
    fn create_mailbox_mta_denied() {
        assert!(!is_permitted(
            CallerClass::BridgeMta,
            "fauna.bridges.create_mailbox"
        ));
    }

    #[test]
    fn create_mailbox_user_denied() {
        assert!(!is_permitted(
            CallerClass::User,
            "fauna.bridges.create_mailbox"
        ));
    }

    #[test]
    fn create_mailbox_admin_denied() {
        assert!(!is_permitted(
            CallerClass::Admin,
            "fauna.bridges.create_mailbox"
        ));
    }

    #[test]
    fn delete_mailbox_mda_permitted() {
        assert!(is_permitted(
            CallerClass::BridgeMda,
            "fauna.bridges.delete_mailbox"
        ));
    }

    #[test]
    fn delete_mailbox_mta_denied() {
        assert!(!is_permitted(
            CallerClass::BridgeMta,
            "fauna.bridges.delete_mailbox"
        ));
    }

    #[test]
    fn delete_mailbox_user_denied() {
        assert!(!is_permitted(
            CallerClass::User,
            "fauna.bridges.delete_mailbox"
        ));
    }

    #[test]
    fn delete_mailbox_admin_denied() {
        assert!(!is_permitted(
            CallerClass::Admin,
            "fauna.bridges.delete_mailbox"
        ));
    }

    #[test]
    fn rename_mailbox_mda_permitted() {
        assert!(is_permitted(
            CallerClass::BridgeMda,
            "fauna.bridges.rename_mailbox"
        ));
    }

    #[test]
    fn rename_mailbox_mta_denied() {
        assert!(!is_permitted(
            CallerClass::BridgeMta,
            "fauna.bridges.rename_mailbox"
        ));
    }

    #[test]
    fn rename_mailbox_user_denied() {
        assert!(!is_permitted(
            CallerClass::User,
            "fauna.bridges.rename_mailbox"
        ));
    }

    #[test]
    fn rename_mailbox_admin_denied() {
        assert!(!is_permitted(
            CallerClass::Admin,
            "fauna.bridges.rename_mailbox"
        ));
    }

    // I5 Phase D.7 — SUBSCRIBE / UNSUBSCRIBE

    #[test]
    fn subscribe_mailbox_mda_permitted() {
        assert!(is_permitted(
            CallerClass::BridgeMda,
            "fauna.bridges.subscribe_mailbox"
        ));
    }

    #[test]
    fn subscribe_mailbox_mta_denied() {
        assert!(!is_permitted(
            CallerClass::BridgeMta,
            "fauna.bridges.subscribe_mailbox"
        ));
    }

    #[test]
    fn subscribe_mailbox_user_denied() {
        assert!(!is_permitted(
            CallerClass::User,
            "fauna.bridges.subscribe_mailbox"
        ));
    }

    #[test]
    fn subscribe_mailbox_admin_denied() {
        assert!(!is_permitted(
            CallerClass::Admin,
            "fauna.bridges.subscribe_mailbox"
        ));
    }

    #[test]
    fn unsubscribe_mailbox_mda_permitted() {
        assert!(is_permitted(
            CallerClass::BridgeMda,
            "fauna.bridges.unsubscribe_mailbox"
        ));
    }

    #[test]
    fn unsubscribe_mailbox_mta_denied() {
        assert!(!is_permitted(
            CallerClass::BridgeMta,
            "fauna.bridges.unsubscribe_mailbox"
        ));
    }

    #[test]
    fn unsubscribe_mailbox_user_denied() {
        assert!(!is_permitted(
            CallerClass::User,
            "fauna.bridges.unsubscribe_mailbox"
        ));
    }

    #[test]
    fn unsubscribe_mailbox_admin_denied() {
        assert!(!is_permitted(
            CallerClass::Admin,
            "fauna.bridges.unsubscribe_mailbox"
        ));
    }

    #[test]
    fn subscribe_mailbox_state_mda_permitted() {
        assert!(is_permitted(
            CallerClass::BridgeMda,
            "fauna.bridges.subscribe_mailbox_state"
        ));
    }

    #[test]
    fn subscribe_mailbox_state_mta_denied() {
        assert!(!is_permitted(
            CallerClass::BridgeMta,
            "fauna.bridges.subscribe_mailbox_state"
        ));
    }

    #[test]
    fn subscribe_mailbox_state_user_denied() {
        assert!(!is_permitted(
            CallerClass::User,
            "fauna.bridges.subscribe_mailbox_state"
        ));
    }

    #[test]
    fn subscribe_mailbox_state_admin_denied() {
        assert!(!is_permitted(
            CallerClass::Admin,
            "fauna.bridges.subscribe_mailbox_state"
        ));
    }

    // ── Layer-3 Bridge Management (user-facing) ─────────────────

    #[test]
    fn bridges_list_user_permitted() {
        assert!(is_permitted(CallerClass::User, "fauna.bridges.list"));
    }

    #[test]
    fn bridges_list_bridges_denied() {
        assert!(!is_permitted(CallerClass::BridgeMta, "fauna.bridges.list"));
        assert!(!is_permitted(CallerClass::BridgeMda, "fauna.bridges.list"));
    }

    #[test]
    fn bridges_list_admin_permitted() {
        // Admin ⊇ user: this per-actor (caller-scoped) kind routes through
        // `User`, which an admin (also a user) inherits — its own bridges list.
        assert!(is_permitted(CallerClass::Admin, "fauna.bridges.list"));
    }

    // T2 — fauna.bridges.set_settings / fauna.bridges.list_follows.

    #[test]
    fn bridges_set_settings_user_permitted() {
        assert!(is_permitted(
            CallerClass::User,
            "fauna.bridges.set_settings"
        ));
    }

    #[test]
    fn bridges_set_settings_bridges_denied() {
        assert!(!is_permitted(
            CallerClass::BridgeMta,
            "fauna.bridges.set_settings"
        ));
        assert!(!is_permitted(
            CallerClass::BridgeMda,
            "fauna.bridges.set_settings"
        ));
        // admin ⊇ user (caller-scoped: the admin's own settings)
        assert!(is_permitted(
            CallerClass::Admin,
            "fauna.bridges.set_settings"
        ));
    }

    #[test]
    fn bridges_list_follows_user_permitted() {
        assert!(is_permitted(
            CallerClass::User,
            "fauna.bridges.list_follows"
        ));
    }

    #[test]
    fn bridges_list_follows_bridges_denied() {
        assert!(!is_permitted(
            CallerClass::BridgeMta,
            "fauna.bridges.list_follows"
        ));
        assert!(!is_permitted(
            CallerClass::BridgeMda,
            "fauna.bridges.list_follows"
        ));
        // admin ⊇ user (caller-scoped: the admin's own follows)
        assert!(is_permitted(
            CallerClass::Admin,
            "fauna.bridges.list_follows"
        ));
    }

    // T3 — fauna.bridges.link / fauna.bridges.unlink.

    #[test]
    fn bridges_link_user_permitted() {
        assert!(is_permitted(CallerClass::User, "fauna.bridges.link"));
    }

    #[test]
    fn bridges_link_bridges_denied() {
        assert!(!is_permitted(CallerClass::BridgeMta, "fauna.bridges.link"));
        assert!(!is_permitted(CallerClass::BridgeMda, "fauna.bridges.link"));
        // admin ⊇ user (caller-scoped)
        assert!(is_permitted(CallerClass::Admin, "fauna.bridges.link"));
    }

    #[test]
    fn bridges_link_challenge_is_scoped_exactly_like_link() {
        for class in [
            CallerClass::User,
            CallerClass::Admin,
            CallerClass::BridgeMta,
            CallerClass::BridgeMda,
        ] {
            assert_eq!(
                is_permitted(class, "fauna.bridges.link_challenge"),
                is_permitted(class, "fauna.bridges.link"),
                "{class:?}"
            );
        }
        assert!(is_permitted(
            CallerClass::User,
            "fauna.bridges.link_challenge"
        ));
    }

    #[test]
    fn bridges_unlink_user_permitted() {
        assert!(is_permitted(CallerClass::User, "fauna.bridges.unlink"));
    }

    #[test]
    fn bridges_unlink_bridges_denied() {
        assert!(!is_permitted(
            CallerClass::BridgeMta,
            "fauna.bridges.unlink"
        ));
        assert!(!is_permitted(
            CallerClass::BridgeMda,
            "fauna.bridges.unlink"
        ));
        // admin ⊇ user (caller-scoped)
        assert!(is_permitted(CallerClass::Admin, "fauna.bridges.unlink"));
    }

    // T4 — fauna.bridges.add_follow / fauna.bridges.remove_follow.

    #[test]
    fn bridges_add_follow_user_permitted() {
        assert!(is_permitted(CallerClass::User, "fauna.bridges.add_follow"));
    }

    #[test]
    fn bridges_add_follow_bridges_denied() {
        assert!(!is_permitted(
            CallerClass::BridgeMta,
            "fauna.bridges.add_follow"
        ));
        assert!(!is_permitted(
            CallerClass::BridgeMda,
            "fauna.bridges.add_follow"
        ));
        // admin ⊇ user (caller-scoped)
        assert!(is_permitted(CallerClass::Admin, "fauna.bridges.add_follow"));
    }

    #[test]
    fn bridges_remove_follow_user_permitted() {
        assert!(is_permitted(
            CallerClass::User,
            "fauna.bridges.remove_follow"
        ));
    }

    #[test]
    fn bridges_remove_follow_bridges_denied() {
        assert!(!is_permitted(
            CallerClass::BridgeMta,
            "fauna.bridges.remove_follow"
        ));
        assert!(!is_permitted(
            CallerClass::BridgeMda,
            "fauna.bridges.remove_follow"
        ));
        // admin ⊇ user (caller-scoped)
        assert!(is_permitted(
            CallerClass::Admin,
            "fauna.bridges.remove_follow"
        ));
    }

    // Follow requests — fauna.bridges.{list_follow_requests,resolve_follow_request}.

    #[test]
    fn bridges_follow_request_kinds_are_user_only() {
        for kind in [
            "fauna.bridges.list_follow_requests",
            "fauna.bridges.resolve_follow_request",
        ] {
            assert!(is_permitted(CallerClass::User, kind), "{kind}");
            assert!(!is_permitted(CallerClass::BridgeMta, kind), "{kind}");
            assert!(!is_permitted(CallerClass::BridgeMda, kind), "{kind}");
            // admin ⊇ user (caller-scoped)
            assert!(is_permitted(CallerClass::Admin, kind), "{kind}");
        }
    }

    // T5 — fauna.bridges.feeds.{list,create,delete}.

    #[test]
    fn bridges_feeds_user_permitted() {
        for kind in [
            "fauna.bridges.feeds.list",
            "fauna.bridges.feeds.create",
            "fauna.bridges.feeds.delete",
        ] {
            assert!(
                is_permitted(CallerClass::User, kind),
                "{kind} should be permitted for User"
            );
        }
    }

    #[test]
    fn bridges_feeds_bridges_denied() {
        for kind in [
            "fauna.bridges.feeds.list",
            "fauna.bridges.feeds.create",
            "fauna.bridges.feeds.delete",
        ] {
            for class in [CallerClass::BridgeMta, CallerClass::BridgeMda] {
                assert!(
                    !is_permitted(class, kind),
                    "{kind} should be denied for {class:?}"
                );
            }
        }
    }

    // T6 — fauna.email.filters.{list,create,get,update,delete}.

    #[test]
    fn email_filters_user_permitted() {
        for kind in [
            "fauna.email.filters.list",
            "fauna.email.filters.create",
            "fauna.email.filters.get",
            "fauna.email.filters.update",
            "fauna.email.filters.delete",
        ] {
            assert!(
                is_permitted(CallerClass::User, kind),
                "{kind} should be permitted for User"
            );
        }
    }

    #[test]
    fn email_filters_bridges_denied() {
        for kind in [
            "fauna.email.filters.list",
            "fauna.email.filters.create",
            "fauna.email.filters.get",
            "fauna.email.filters.update",
            "fauna.email.filters.delete",
        ] {
            for class in [CallerClass::BridgeMta, CallerClass::BridgeMda] {
                assert!(
                    !is_permitted(class, kind),
                    "{kind} should be denied for {class:?}"
                );
            }
        }
    }

    // T7+T8 — fauna.email.send. Self-scoped: the caller submits outbound mail
    // as ITSELF from a client UI button. Permitted for User AND Admin — on a
    // personal single-user nest the claimer-admin is also the mail user and
    // sends from the same client (the same reasoning that already lets an admin
    // enable their own mail; the kind is caller-scoped, so an admin can only
    // send as themselves, never on another user's behalf). Bridge actors use
    // `fauna.bridges.enqueue_outbound_mail`, never this.

    #[test]
    fn email_send_user_and_admin_permitted() {
        assert!(is_permitted(CallerClass::User, "fauna.email.send"));
        assert!(is_permitted(CallerClass::Admin, "fauna.email.send"));
    }

    #[test]
    fn email_send_bridges_denied() {
        for class in [CallerClass::BridgeMta, CallerClass::BridgeMda] {
            assert!(
                !is_permitted(class, "fauna.email.send"),
                "fauna.email.send should be denied for {class:?}"
            );
        }
    }

    // fauna.email.inbox.fetch. The
    // inbound twin of `fauna.email.send`: the caller reads its OWN sealed
    // INBOX (caller-scoped — no actor_id param). Permitted for User AND
    // Admin: on a personal single-user nest the claimer-admin is also the
    // mail user and reads its mailbox from the same client. Caller-scoping
    // means an admin can only read its own INBOX, never another user's.
    // Bridge actors read mail via the BridgeMda surface, never this.

    #[test]
    fn email_inbox_fetch_user_and_admin_permitted() {
        assert!(is_permitted(CallerClass::User, "fauna.email.inbox.fetch"));
        assert!(is_permitted(CallerClass::Admin, "fauna.email.inbox.fetch"));
    }

    #[test]
    fn inbox_delivery_queue_user_and_admin_permitted() {
        // `send` is the outbound counterpart of the `fetch`/`ack` drain —
        // same User(+Admin)/Bridge-denied gate.
        for kind in ["fauna.inbox.fetch", "fauna.inbox.ack", "fauna.inbox.send"] {
            assert!(is_permitted(CallerClass::User, kind), "{kind} for User");
            assert!(is_permitted(CallerClass::Admin, kind), "{kind} for Admin");
            for class in [CallerClass::BridgeMta, CallerClass::BridgeMda] {
                assert!(
                    !is_permitted(class, kind),
                    "{kind} should be denied for {class:?}"
                );
            }
        }
    }

    #[test]
    fn email_inbox_fetch_bridges_denied() {
        for class in [CallerClass::BridgeMta, CallerClass::BridgeMda] {
            assert!(
                !is_permitted(class, "fauna.email.inbox.fetch"),
                "fauna.email.inbox.fetch should be denied for {class:?}"
            );
        }
    }

    // The Sent sibling of `inbox.fetch` — same User+Admin permitted /
    // bridges-denied contract (the caller reads its OWN sealed `Sent`
    // mailbox, the outbound copy of mail submitted from an external MUA).

    #[test]
    fn email_sent_fetch_user_and_admin_permitted() {
        assert!(is_permitted(CallerClass::User, "fauna.email.sent.fetch"));
        assert!(is_permitted(CallerClass::Admin, "fauna.email.sent.fetch"));
    }

    #[test]
    fn email_sent_fetch_bridges_denied() {
        for class in [CallerClass::BridgeMta, CallerClass::BridgeMda] {
            assert!(
                !is_permitted(class, "fauna.email.sent.fetch"),
                "fauna.email.sent.fetch should be denied for {class:?}"
            );
        }
    }

    // Conversations T1b — channel plane (send/fetch/list_for_actor).
    // User-only, mirroring the email-CRUD + bridges-UI pattern: bridge
    // actors and admin never call into the user-facing MLS plane.

    #[test]
    fn conversations_channel_kinds_user_permitted() {
        for kind in [
            "fauna.conversations.channel.send",
            "fauna.conversations.channel.fetch",
            "fauna.conversations.channel.list_for_actor",
            "fauna.conversations.channel.actors",
            "fauna.conversations.channel.actors_remote",
        ] {
            assert!(
                is_permitted(CallerClass::User, kind),
                "{kind} should be permitted for User"
            );
        }
    }

    #[test]
    fn conversations_channel_kinds_bridges_denied() {
        for kind in [
            "fauna.conversations.channel.send",
            "fauna.conversations.channel.fetch",
            "fauna.conversations.channel.list_for_actor",
            "fauna.conversations.channel.actors",
            "fauna.conversations.channel.actors_remote",
        ] {
            for class in [CallerClass::BridgeMta, CallerClass::BridgeMda] {
                assert!(
                    !is_permitted(class, kind),
                    "{kind} should be denied for {class:?}"
                );
            }
        }
    }

    // ── Conversations T2 — keypackage cluster ──────────────────────

    #[test]
    fn conversations_keypackage_kinds_user_permitted() {
        for kind in [
            "fauna.conversations.keypackage.upload",
            "fauna.conversations.keypackage.fetch",
            "fauna.conversations.keypackage.count",
        ] {
            assert!(
                is_permitted(CallerClass::User, kind),
                "{kind} should be permitted for User"
            );
        }
    }

    #[test]
    fn conversations_keypackage_kinds_bridges_denied() {
        // `upload` + `count` stay bridge-denied. `fetch` is excluded here — it
        // was widened to `User | BridgeMda` for the MDA mailbox-less
        // auto-schedule rail (caldav-server.md § Server-side auto-schedule, C3);
        // its precise allow/deny matrix is pinned below.
        for kind in [
            "fauna.conversations.keypackage.upload",
            "fauna.conversations.keypackage.count",
        ] {
            for class in [CallerClass::BridgeMta, CallerClass::BridgeMda] {
                assert!(
                    !is_permitted(class, kind),
                    "{kind} should be denied for {class:?}"
                );
            }
        }
    }

    #[test]
    fn conversations_keypackage_fetch_widened_for_mda_only() {
        // C3: the MDA gateway fetches a recipient's key package to seal the
        // one-off scheduling delivery against — a public-by-design read.
        assert!(is_permitted(
            CallerClass::BridgeMda,
            "fauna.conversations.keypackage.fetch"
        ));
        assert!(is_permitted(
            CallerClass::User,
            "fauna.conversations.keypackage.fetch"
        ));
        // Still denied to the MTA (no scheduling leg).
        assert!(!is_permitted(
            CallerClass::BridgeMta,
            "fauna.conversations.keypackage.fetch"
        ));
    }

    #[test]
    fn deliver_sealed_scheduling_mda_only() {
        // C3: BridgeMda-only — never MTA, never User.
        assert!(is_permitted(
            CallerClass::BridgeMda,
            "fauna.bridges.deliver_sealed_scheduling"
        ));
        for class in [CallerClass::BridgeMta, CallerClass::User] {
            assert!(
                !is_permitted(class, "fauna.bridges.deliver_sealed_scheduling"),
                "deliver_sealed_scheduling should be denied for {class:?}"
            );
        }
    }

    // ── A2.2 — resolve_recipient (both bridge roles) ──
    // The MTA resolves at RCPT TO; the MDA resolves in its auto-schedule
    // classifier (C5 / `test_caldav_autoschedule_mailbox_less.py`).

    #[test]
    fn resolve_recipient_bridge_roles_permitted() {
        for class in [CallerClass::BridgeMta, CallerClass::BridgeMda] {
            assert!(
                is_permitted(class, "fauna.bridges.resolve_recipient"),
                "resolve_recipient should be permitted for {class:?}"
            );
        }
    }

    #[test]
    fn resolve_recipient_non_bridge_denied() {
        for class in [CallerClass::User, CallerClass::Admin] {
            assert!(
                !is_permitted(class, "fauna.bridges.resolve_recipient"),
                "resolve_recipient should be denied for {class:?}"
            );
        }
    }

    // ── A2.3 — generate_disposable_alias (User-class mint) ──────────

    #[test]
    fn generate_disposable_alias_user_permitted() {
        assert!(is_permitted(
            CallerClass::User,
            "fauna.bridges.generate_disposable_alias"
        ));
    }

    #[test]
    fn generate_disposable_alias_non_user_denied() {
        for class in [CallerClass::BridgeMta, CallerClass::BridgeMda] {
            assert!(
                !is_permitted(class, "fauna.bridges.generate_disposable_alias"),
                "generate_disposable_alias should be denied for {class:?}"
            );
        }
    }

    // ── T1.4 — report_rejected_scan (MTA-class forensic row) ──

    #[test]
    fn report_rejected_scan_mta_permitted() {
        assert!(is_permitted(
            CallerClass::BridgeMta,
            "fauna.bridges.report_rejected_scan"
        ));
    }

    #[test]
    fn report_rejected_scan_non_mta_denied() {
        for class in [
            CallerClass::BridgeMda,
            CallerClass::User,
            CallerClass::Admin,
        ] {
            assert!(
                !is_permitted(class, "fauna.bridges.report_rejected_scan"),
                "report_rejected_scan should be denied for {class:?}"
            );
        }
    }

    // ── A3 Bucket B — put_<substruct>_policy admin write path ──────

    const PUT_POLICY_KINDS: &[&str] = &[
        "fauna.bridges.put_spam_policy",
        "fauna.bridges.put_auth_policy",
        "fauna.bridges.put_submission_policy",
        "fauna.bridges.put_imap_policy",
        "fauna.bridges.put_outbound_policy",
        // The nest-side alias-policy kind shares the Admin-only property.
        "fauna.bridges.put_alias_policy",
    ];

    #[test]
    fn put_policy_kinds_admin_permitted() {
        for kind in PUT_POLICY_KINDS {
            assert!(
                is_permitted(CallerClass::Admin, kind),
                "{kind} must be Admin-permitted"
            );
        }
    }

    #[test]
    fn put_policy_kinds_non_admin_denied() {
        for kind in PUT_POLICY_KINDS {
            for class in [
                CallerClass::BridgeMta,
                CallerClass::BridgeMda,
                CallerClass::User,
            ] {
                assert!(!is_permitted(class, kind), "{kind} must deny {class:?}");
            }
        }
    }

    #[test]
    fn transport_policy_kinds_are_admin_only() {
        // The nest-OWN transport/abuse policy (per-IP TLS connection cap) is a
        // deployment-wide admin knob — both put and get are Admin-only; no
        // bridge role or user may read or set nest's own listener caps.
        for kind in ["fauna.transport.put_policy", "fauna.transport.get_policy"] {
            assert!(
                is_permitted(CallerClass::Admin, kind),
                "{kind} must be Admin-permitted"
            );
            for class in [
                CallerClass::BridgeMta,
                CallerClass::BridgeMda,
                CallerClass::User,
            ] {
                assert!(!is_permitted(class, kind), "{kind} must deny {class:?}");
            }
        }
    }

    #[test]
    fn get_mail_config_is_admin_only() {
        // The admin read twin of `fetch_config` is Admin-only — the bridge
        // roles read their config via `fetch_config`, not this kind.
        assert!(is_permitted(
            CallerClass::Admin,
            "fauna.bridges.get_mail_config"
        ));
        for class in [
            CallerClass::BridgeMta,
            CallerClass::BridgeMda,
            CallerClass::User,
        ] {
            assert!(
                !is_permitted(class, "fauna.bridges.get_mail_config"),
                "get_mail_config must deny {class:?}"
            );
        }
    }

    #[test]
    fn get_alias_policy_is_admin_only() {
        // The admin read twin of `put_alias_policy` is Admin-only, like the
        // write kind — no bridge role reads the nest-side alias policy.
        assert!(is_permitted(
            CallerClass::Admin,
            "fauna.bridges.get_alias_policy"
        ));
        for class in [
            CallerClass::BridgeMta,
            CallerClass::BridgeMda,
            CallerClass::User,
        ] {
            assert!(
                !is_permitted(class, "fauna.bridges.get_alias_policy"),
                "get_alias_policy must deny {class:?}"
            );
        }
    }

    // ── AF — admin external forwarders ──

    const FORWARDER_KINDS: &[&str] = &[
        "fauna.bridges.create_forwarder",
        "fauna.bridges.list_forwarders",
        "fauna.bridges.delete_forwarder",
    ];

    #[test]
    fn forwarder_kinds_admin_permitted() {
        for kind in FORWARDER_KINDS {
            assert!(
                is_permitted(CallerClass::Admin, kind),
                "{kind} must be Admin-permitted"
            );
        }
    }

    #[test]
    fn forwarder_kinds_non_admin_denied() {
        // Forwarders are deployment config — never a User's personal aliases,
        // never a bridge service-user's call.
        for kind in FORWARDER_KINDS {
            for class in [
                CallerClass::BridgeMta,
                CallerClass::BridgeMda,
                CallerClass::User,
            ] {
                assert!(!is_permitted(class, kind), "{kind} must deny {class:?}");
            }
        }
    }

    // ── Conversations T3 — welcome cluster ─────────────────────────

    #[test]
    fn conversations_welcome_deliver_user_permitted() {
        assert!(is_permitted(
            CallerClass::User,
            "fauna.conversations.welcome.deliver"
        ));
    }

    #[test]
    fn conversations_welcome_deliver_bridges_denied() {
        for class in [CallerClass::BridgeMta, CallerClass::BridgeMda] {
            assert!(
                !is_permitted(class, "fauna.conversations.welcome.deliver"),
                "fauna.conversations.welcome.deliver should be denied for {class:?}"
            );
        }
    }

    // ── Posts slice (T1) ─────────────

    #[test]
    fn posts_kinds_user_permitted() {
        for kind in [
            "fauna.posts.create",
            "fauna.posts.get",
            "fauna.posts.list",
            "fauna.posts.delete",
            "fauna.posts.interact",
            "fauna.posts.room_labels",
            "fauna.posts.room_labels_remote",
        ] {
            assert!(
                is_permitted(CallerClass::User, kind),
                "{kind} should be permitted for User"
            );
        }
    }

    #[test]
    fn posts_kinds_bridges_denied() {
        for kind in [
            "fauna.posts.create",
            "fauna.posts.get",
            "fauna.posts.list",
            "fauna.posts.delete",
            "fauna.posts.interact",
            "fauna.posts.room_labels",
            "fauna.posts.room_labels_remote",
        ] {
            for class in [CallerClass::BridgeMta, CallerClass::BridgeMda] {
                assert!(
                    !is_permitted(class, kind),
                    "{kind} should be denied for {class:?}"
                );
            }
        }
    }

    // ── Feed slice (T2) ──────────────

    const FEED_KINDS: [&str; 11] = [
        "fauna.feed.list",
        "fauna.feed.create",
        "fauna.feed.get",
        "fauna.feed.update",
        "fauna.feed.delete",
        "fauna.feed.posts",
        "fauna.feed.local.posts",
        "fauna.feed.trending.posts",
        "fauna.feed.contributors.list",
        "fauna.feed.contributors.grant",
        "fauna.feed.contributors.revoke",
    ];

    #[test]
    fn feed_kinds_user_permitted() {
        for kind in FEED_KINDS {
            assert!(
                is_permitted(CallerClass::User, kind),
                "{kind} should be permitted for User"
            );
        }
    }

    #[test]
    fn feed_kinds_bridges_denied() {
        for kind in FEED_KINDS {
            for class in [CallerClass::BridgeMta, CallerClass::BridgeMda] {
                assert!(
                    !is_permitted(class, kind),
                    "{kind} should be denied for {class:?}"
                );
            }
        }
    }

    // ── Notifications slice (T1) ─────

    const NOTIFICATIONS_KINDS: [&str; 5] = [
        "fauna.notifications.list",
        "fauna.notifications.mark_read",
        "fauna.notifications.count",
        "fauna.notifications.dismiss",
        "fauna.notifications.clear",
    ];

    #[test]
    fn notifications_kinds_user_permitted() {
        for kind in NOTIFICATIONS_KINDS {
            assert!(
                is_permitted(CallerClass::User, kind),
                "{kind} should be permitted for User"
            );
        }
    }

    #[test]
    fn notifications_kinds_bridges_denied() {
        for kind in NOTIFICATIONS_KINDS {
            for class in [CallerClass::BridgeMta, CallerClass::BridgeMda] {
                assert!(
                    !is_permitted(class, kind),
                    "{kind} should be denied for {class:?}"
                );
            }
        }
    }

    // ── Contacts cluster (T2) ────────

    const CONTACTS_CLUSTER_KINDS: [&str; 10] = [
        "fauna.knocks.list",
        "fauna.knocks.accept",
        "fauna.knocks.block",
        "fauna.knocks.unblock",
        "fauna.knocks.dismiss",
        "fauna.contacts.list",
        "fauna.contacts.status",
        "fauna.contacts.confirm",
        "fauna.inbox.mode.get",
        "fauna.inbox.mode.set",
    ];

    #[test]
    fn contacts_cluster_kinds_user_permitted() {
        for kind in CONTACTS_CLUSTER_KINDS {
            assert!(
                is_permitted(CallerClass::User, kind),
                "{kind} should be permitted for User"
            );
        }
    }

    #[test]
    fn contacts_cluster_kinds_bridges_denied() {
        for kind in CONTACTS_CLUSTER_KINDS {
            for class in [CallerClass::BridgeMta, CallerClass::BridgeMda] {
                assert!(
                    !is_permitted(class, kind),
                    "{kind} should be denied for {class:?}"
                );
            }
        }
    }

    // ── Account cluster ────────────

    const ACCOUNT_KINDS: [&str; 6] = [
        "fauna.account.get",
        "fauna.quota.get",
        "fauna.account.am_i_admin",
        "fauna.profile.handle.change",
        "fauna.account.upgrade",
        "fauna.account.delete",
    ];

    #[test]
    fn account_kinds_user_and_admin_permitted() {
        // Unlike notifications/contacts (User-only), the account cluster is
        // `User | Admin` — an admin manages their own account too, and
        // `am_i_admin` must be reachable by an admin.
        for kind in ACCOUNT_KINDS {
            for class in [CallerClass::User, CallerClass::Admin] {
                assert!(
                    is_permitted(class, kind),
                    "{kind} should be permitted for {class:?}"
                );
            }
        }
    }

    #[test]
    fn account_kinds_bridges_denied() {
        for kind in ACCOUNT_KINDS {
            for class in [CallerClass::BridgeMta, CallerClass::BridgeMda] {
                assert!(
                    !is_permitted(class, kind),
                    "{kind} should be denied for {class:?}"
                );
            }
        }
    }

    // ── Profile surface (docs/goal/ui/profile.md) ──────────────────

    #[test]
    fn profile_get_set_user_and_admin_permitted_bridges_denied() {
        // `fauna.profile.{get,set}` — per-user profile *detail* read + the
        // owner's own-write, same `User | Admin` personal-account gate as
        // `fauna.account.get` / `fauna.profile.handle.change`. Bridges have no
        // personal account → denied.
        for kind in ["fauna.profile.get", "fauna.profile.set"] {
            for class in [CallerClass::User, CallerClass::Admin] {
                assert!(
                    is_permitted(class, kind),
                    "{kind} should be permitted for {class:?}"
                );
            }
            for class in [CallerClass::BridgeMta, CallerClass::BridgeMda] {
                assert!(
                    !is_permitted(class, kind),
                    "{kind} should be denied for {class:?}"
                );
            }
        }
    }

    // ── Pending-actions surface (B20) ──

    /// list/get/cancel: `User | Admin` (same arm as the account cluster).
    const PENDING_ACTIONS_USER_KINDS: [&str; 3] = [
        "fauna.pending_actions.list",
        "fauna.pending_actions.get",
        "fauna.pending_actions.cancel",
    ];

    #[test]
    fn pending_actions_user_kinds_user_and_admin_permitted() {
        for kind in PENDING_ACTIONS_USER_KINDS {
            for class in [CallerClass::User, CallerClass::Admin] {
                assert!(
                    is_permitted(class, kind),
                    "{kind} should be permitted for {class:?}"
                );
            }
        }
    }

    #[test]
    fn pending_actions_user_kinds_bridges_denied() {
        for kind in PENDING_ACTIONS_USER_KINDS {
            for class in [CallerClass::BridgeMta, CallerClass::BridgeMda] {
                assert!(
                    !is_permitted(class, kind),
                    "{kind} should be denied for {class:?}"
                );
            }
        }
    }

    #[test]
    fn pending_actions_approve_admin_only() {
        // approve mirrors the twin's `AdminBearerAuth`: Admin yes, everyone else
        // no (User included — quorum approval is an admin action).
        assert!(is_permitted(
            CallerClass::Admin,
            "fauna.pending_actions.approve"
        ));
        for class in [
            CallerClass::User,
            CallerClass::BridgeMta,
            CallerClass::BridgeMda,
        ] {
            assert!(
                !is_permitted(class, "fauna.pending_actions.approve"),
                "approve should be denied for {class:?}"
            );
        }
    }

    // ── Stats surface (B17) ──

    #[test]
    fn stats_get_user_and_admin_permitted_bridges_denied() {
        // `fauna.stats.get` mirrors the twin's plain `BearerAuth`: any
        // authenticated actor (User | Admin), but bridge actors denied.
        for class in [CallerClass::User, CallerClass::Admin] {
            assert!(
                is_permitted(class, "fauna.stats.get"),
                "fauna.stats.get should be permitted for {class:?}"
            );
        }
        for class in [CallerClass::BridgeMta, CallerClass::BridgeMda] {
            assert!(
                !is_permitted(class, "fauna.stats.get"),
                "fauna.stats.get should be denied for {class:?}"
            );
        }
    }

    // ── File-versions surface (B16) ──

    #[test]
    fn files_versions_user_and_admin_permitted_bridges_denied() {
        for kind in [
            "fauna.files.versions.list",
            "fauna.files.versions.get",
            "fauna.files.versions.undelete",
        ] {
            for class in [CallerClass::User, CallerClass::Admin] {
                assert!(
                    is_permitted(class, kind),
                    "{kind} should be permitted for {class:?}"
                );
            }
            for class in [CallerClass::BridgeMta, CallerClass::BridgeMda] {
                assert!(
                    !is_permitted(class, kind),
                    "{kind} should be denied for {class:?}"
                );
            }
        }
    }

    // ── Web-content-publishing surface (B18) ──

    #[test]
    fn web_kinds_user_and_admin_permitted_bridges_denied() {
        for kind in [
            "fauna.web.publish.set",
            "fauna.web.publish.unset",
            "fauna.web.publish.list",
            "fauna.web.domain.set",
            "fauna.web.domain.get",
            "fauna.web.domain.delete",
            // Per-user subdomain-hosting opt-in (Slice 3) — same User|Admin,
            // caller-scoped gate as the publish/domain kinds.
            "fauna.web.set_subdomain_enabled",
            "fauna.web.get_subdomain_enabled",
        ] {
            for class in [CallerClass::User, CallerClass::Admin] {
                assert!(
                    is_permitted(class, kind),
                    "{kind} should be permitted for {class:?}"
                );
            }
            for class in [CallerClass::BridgeMta, CallerClass::BridgeMda] {
                assert!(
                    !is_permitted(class, kind),
                    "{kind} should be denied for {class:?}"
                );
            }
        }
    }

    // ── Pairing surface — per-user multi-homing (Linked nests) ──

    #[test]
    fn pair_kinds_user_and_admin_permitted_bridges_denied() {
        for kind in [
            "fauna.pair.list",
            "fauna.pair.add",
            "fauna.pair.revoke",
            "fauna.pair.forward_retry",
            "fauna.pair.forward_discard",
        ] {
            for class in [CallerClass::User, CallerClass::Admin] {
                assert!(
                    is_permitted(class, kind),
                    "{kind} should be permitted for {class:?}"
                );
            }
            for class in [CallerClass::BridgeMta, CallerClass::BridgeMda] {
                assert!(
                    !is_permitted(class, kind),
                    "{kind} should be denied for {class:?}"
                );
            }
        }
    }

    #[test]
    fn admin_pairings_kinds_retired_denied_for_all() {
        for kind in ["fauna.admin.pairings.list", "fauna.admin.pairings.approve"] {
            for class in [
                CallerClass::Admin,
                CallerClass::User,
                CallerClass::BridgeMta,
                CallerClass::BridgeMda,
            ] {
                assert!(
                    !is_permitted(class, kind),
                    "retired {kind} must be denied for {class:?}"
                );
            }
        }
    }

    // ── Folder snapshot control surface (B15) ──

    #[test]
    fn filesync_snapshot_control_kinds_user_and_admin_permitted_bridges_denied() {
        for kind in [
            "fauna.filesync.snapshot.create_folder",
            "fauna.filesync.snapshot.get",
            "fauna.filesync.snapshot.stamp_labels",
            "fauna.filesync.snapshot.delete",
            "fauna.filesync.snapshot.undelete",
            "fauna.filesync.snapshot.prune",
            "fauna.filesync.snapshot.prune_set_policy",
            "fauna.filesync.snapshot.check",
            "fauna.filesync.snapshot.diff",
            "fauna.filesync.snapshot.list",
        ] {
            for class in [CallerClass::User, CallerClass::Admin] {
                assert!(
                    is_permitted(class, kind),
                    "{kind} should be permitted for {class:?}"
                );
            }
            for class in [CallerClass::BridgeMta, CallerClass::BridgeMda] {
                assert!(
                    !is_permitted(class, kind),
                    "{kind} should be denied for {class:?}"
                );
            }
        }
    }

    // ── Labels + engagement (hub Track B8) ──────────────────────────

    #[test]
    fn labels_engagement_kinds_user_permitted() {
        for kind in ["fauna.labels.attach", "fauna.labels.list"] {
            assert!(
                is_permitted(CallerClass::User, kind),
                "{kind} should be permitted for User"
            );
        }
    }

    /// The attach arm admits the grant-holder classes — the same set
    /// `fauna.capabilities.fetch` admits, since a holder is a class that can
    /// fetch a grant — and refuses the two bridge classes that hold none; the
    /// list arm stays User-only. Re-decided deliberately: this test used to pin `BridgeMda` out of `attach`,
    /// which is what made the ratified grant path unreachable.
    #[test]
    fn labels_attach_admits_grant_holder_classes_and_list_stays_user_only() {
        for class in [CallerClass::BridgeMda, CallerClass::ContentProcessor] {
            assert!(
                is_permitted(class, "fauna.labels.attach"),
                "fauna.labels.attach should admit the grant-holder class {class:?}"
            );
            assert_eq!(
                is_permitted(class, "fauna.labels.attach"),
                is_permitted(class, "fauna.capabilities.fetch"),
                "the attach holder set is the fetch holder set ({class:?})"
            );
            assert!(
                !is_permitted(class, "fauna.labels.list"),
                "fauna.labels.list should be denied for {class:?}"
            );
        }
        for kind in ["fauna.labels.attach", "fauna.labels.list"] {
            for class in [CallerClass::BridgeMta, CallerClass::BridgeAtprotoPds] {
                assert!(
                    !is_permitted(class, kind),
                    "{kind} should be denied for {class:?}"
                );
            }
        }
    }

    // Mailbox-migration import surface (`mailbox-migration.md` § Per-message
    // flow) — User-class: the importer is the user's own Fauna app, caller-
    // scoped. Distinct from the BridgeMda-only `fauna.bridges.append` the
    // handlers share a write path with.
    const IMPORT_KINDS: [&str; 9] = [
        "fauna.bridges.start_import_session",
        "fauna.bridges.import_message",
        "fauna.bridges.import_message_batch",
        "fauna.bridges.list_import_sessions",
        "fauna.bridges.pause_import_session",
        "fauna.bridges.resume_import_session",
        "fauna.bridges.cancel_import_session",
        "fauna.bridges.finalize_import_session",
        "fauna.bridges.fail_import_session",
    ];

    #[test]
    fn bridges_import_kinds_user_permitted() {
        for kind in IMPORT_KINDS {
            assert!(
                is_permitted(CallerClass::User, kind),
                "{kind} should be permitted for User"
            );
            assert!(
                is_permitted(CallerClass::Admin, kind),
                "{kind} should be permitted for Admin (admin ⊇ user)"
            );
        }
    }

    #[test]
    fn bridges_import_kinds_bridges_denied() {
        for kind in IMPORT_KINDS {
            for class in [
                CallerClass::BridgeMta,
                CallerClass::BridgeMda,
                CallerClass::ContentProcessor,
            ] {
                assert!(
                    !is_permitted(class, kind),
                    "{kind} should be denied for {class:?}"
                );
            }
        }
    }

    // Mailbox-export surface (`mail-export.md` § Wire shapes) — User-class,
    // caller-scoped, the read-out twin of IMPORT_KINDS.
    const EXPORT_KINDS: [&str; 11] = [
        "fauna.bridges.list_own_mailboxes",
        "fauna.bridges.start_export_session",
        "fauna.bridges.list_export_sessions",
        "fauna.bridges.fetch_export_chunk_ciphertext",
        "fauna.bridges.upload_export_chunk",
        "fauna.bridges.pause_export_session",
        "fauna.bridges.resume_export_session",
        "fauna.bridges.restart_export_session",
        "fauna.bridges.cancel_export_session",
        "fauna.bridges.finalize_export_session",
        "fauna.bridges.discard_export_blob",
    ];

    #[test]
    fn bridges_export_kinds_user_permitted() {
        for kind in EXPORT_KINDS {
            assert!(
                is_permitted(CallerClass::User, kind),
                "{kind} should be permitted for User"
            );
            assert!(
                is_permitted(CallerClass::Admin, kind),
                "{kind} should be permitted for Admin (admin ⊇ user)"
            );
        }
    }

    /// § Cross-actor isolation: "the admin cannot trigger an export of another
    /// user's mail", and no bridge principal may either — an export reads the
    /// user's own plaintext-bearing records, so the only caller class that
    /// reaches it is the user's own app. This is the gate that keeps a future
    /// widening honest; the shape of the requests (no target actor) is the
    /// other half.
    #[test]
    fn bridges_export_kinds_bridges_denied() {
        for kind in EXPORT_KINDS {
            for class in [
                CallerClass::BridgeMta,
                CallerClass::BridgeMda,
                CallerClass::ContentProcessor,
                CallerClass::BridgeAtprotoPds,
            ] {
                assert!(
                    !is_permitted(class, kind),
                    "{kind} should be denied for {class:?}"
                );
            }
        }
    }

    /// The MDA's `list_mailboxes` and the user's `list_own_mailboxes` are two
    /// kinds on purpose (`mail-export.md` § Wire shapes, ratified 2026-09-20).
    /// If a later edit ever collapses them, this is what says no.
    #[test]
    fn the_two_mailbox_listers_stay_disjoint_in_caller_class() {
        assert!(is_permitted(
            CallerClass::BridgeMda,
            "fauna.bridges.list_mailboxes"
        ));
        assert!(!is_permitted(
            CallerClass::User,
            "fauna.bridges.list_mailboxes"
        ));
        assert!(is_permitted(
            CallerClass::User,
            "fauna.bridges.list_own_mailboxes"
        ));
        assert!(!is_permitted(
            CallerClass::BridgeMda,
            "fauna.bridges.list_own_mailboxes"
        ));
    }

    // Push subscription management (Track B22) — User | Admin; bridges denied.
    #[test]
    fn push_kinds_user_and_admin_permitted_bridges_denied() {
        for kind in [
            "fauna.push.vapid_key",
            "fauna.push.subscribe",
            "fauna.push.unsubscribe",
            "fauna.push.presence",
        ] {
            for class in [CallerClass::User, CallerClass::Admin] {
                assert!(
                    is_permitted(class, kind),
                    "{kind} should be permitted for {class:?}"
                );
            }
            for class in [CallerClass::BridgeMta, CallerClass::BridgeMda] {
                assert!(
                    !is_permitted(class, kind),
                    "{kind} should be denied for {class:?}"
                );
            }
        }
    }

    /// Coverage tripwire for the central capability gate
    /// (`routes::dispatch_request` gate 1d). Every kind the authenticated
    /// per-actor router can dispatch — except the pre-identity bootstrap /
    /// discovery kinds, which an authed connection may also call and which are
    /// gated by `pre_identity_allowlist` — MUST be permitted for at least one
    /// `CallerClass`, i.e. it must appear in `is_permitted` rather than falling
    /// to the `_ => false` arm. Two hazards this catches: a registered kind that
    /// maps to NO class is rejected by the central gate for every caller (a dead
    /// kind); and — the real concern — a *new* handler registered without an
    /// allowlist arm would be silently bridge-reachable if it relied on a
    /// per-handler check it forgot to add. This test fails the moment such a kind
    /// lands, keeping the documented "every method is on the per-role allowlist"
    /// chokepoint (`docs/goal/architecture/apps/bridges.md` § Why this shape)
    /// honest. Builds the exact same router `start_server` does
    /// (`crate::build_rpc_router`), so it can never drift from the live set.
    // ── WebDAV served-set key blob (webdav-server.md § MDA↔nest contract) ──

    #[test]
    fn fetch_webdav_keys_blob_is_bridge_mda_only() {
        assert!(is_permitted(
            CallerClass::BridgeMda,
            "fauna.bridges.fetch_webdav_keys_blob"
        ));
        for class in [
            CallerClass::BridgeMta,
            CallerClass::User,
            CallerClass::Admin,
        ] {
            assert!(
                !is_permitted(class, "fauna.bridges.fetch_webdav_keys_blob"),
                "fetch_webdav_keys_blob should be denied for {class:?}"
            );
        }
    }

    #[test]
    fn mint_bulk_byte_token_is_bridges_only() {
        // Both bridge classes may CALL the mint — the MTA needs it to stage a sealed
        // mail body over the inline budget (smtp-server.md § Message size limits).
        // What each may mint is decided by `purpose` inside the handler, NOT here:
        // an MTA asking for a folder token is denied there. That split is why
        // admitting BridgeMta to this kind did not open the WebDAV byte path to it.
        for class in [CallerClass::BridgeMda, CallerClass::BridgeMta] {
            assert!(
                is_permitted(class, "fauna.bridges.mint_bulk_byte_token"),
                "mint_bulk_byte_token should be permitted for {class:?}"
            );
        }
        // No non-bridge class ever mints: a user/admin holds a full session bearer,
        // which the byte routes already accept.
        for class in [CallerClass::User, CallerClass::Admin] {
            assert!(
                !is_permitted(class, "fauna.bridges.mint_bulk_byte_token"),
                "mint_bulk_byte_token should be denied for {class:?}"
            );
        }
    }

    #[test]
    fn provision_webdav_keys_blob_is_user_and_admin() {
        for class in [CallerClass::User, CallerClass::Admin] {
            assert!(is_permitted(
                class,
                "fauna.bridges.provision_webdav_keys_blob"
            ));
        }
        for class in [CallerClass::BridgeMta, CallerClass::BridgeMda] {
            assert!(
                !is_permitted(class, "fauna.bridges.provision_webdav_keys_blob"),
                "provision_webdav_keys_blob should be denied for {class:?}"
            );
        }
    }

    #[test]
    fn webdav_data_plane_kinds_are_bridge_mda_only() {
        for kind in [
            "fauna.bridges.webdav_list_folders",
            "fauna.bridges.webdav_list_files",
            "fauna.bridges.webdav_quota",
            "fauna.bridges.webdav_record_change",
            "fauna.bridges.webdav_admit_principal",
        ] {
            assert!(
                is_permitted(CallerClass::BridgeMda, kind),
                "{kind} must be permitted for BridgeMda"
            );
            for class in [
                CallerClass::BridgeMta,
                CallerClass::User,
                CallerClass::Admin,
            ] {
                assert!(
                    !is_permitted(class, kind),
                    "{kind} should be denied for {class:?}"
                );
            }
        }
    }

    #[test]
    fn every_registered_kind_is_gated() {
        let router = crate::build_rpc_router();
        // Every caller class: a kind is "gated" if at least one class may
        // call it (the F1 atproto session-registry kinds are
        // BridgeAtprotoPds-only, which is why the bridge classes are all
        // listed here, not just the mail pair).
        let classes = [
            CallerClass::BridgeMta,
            CallerClass::BridgeMda,
            CallerClass::BridgeAtprotoPds,
            CallerClass::ContentProcessor,
            CallerClass::User,
            CallerClass::Admin,
            // The receipt deposit arm (stage c) is the first CUSTODIAN-only
            // kind — reachable by no other class, which is its whole point.
            CallerClass::Custodian,
            CallerClass::ThirdParty,
        ];
        let mut ungated: Vec<&str> = Vec::new();
        for kind in router.iter_kinds() {
            // Pre-identity kinds (auth bootstrap, discovery, claim, invite,
            // storage/nat-mode, bridge self-enrollment) are reachable on the
            // anonymous connection and exempt from the class gate; the central
            // gate (1d) skips them too, so they are NOT expected in is_permitted.
            if crate::pre_identity_allowlist::is_pre_identity_kind(kind) {
                continue;
            }
            if !classes.iter().any(|&c| is_permitted(c, kind)) {
                ungated.push(kind);
            }
        }
        ungated.sort_unstable();
        assert!(
            ungated.is_empty(),
            "registered authenticated kinds with NO allowlist arm (rejected by \
             the central capability gate for every caller / silently \
             bridge-reachable if a handler forgets its own check): {ungated:?}"
        );
    }

    /// Companion to `every_registered_kind_is_gated`: that test skips
    /// pre-identity kinds because they have no caller class to check, so it
    /// can catch a MISSING allowlist arm but not a DEAD one. A kind in both
    /// sets would have gate (1d) skip its class check (pre-identity) while
    /// gate (1b) admits it on the anonymous connection — its `is_permitted`
    /// arm would read as an enforced restriction while enforcing nothing.
    /// Pins the invariant `bridges.md` § Capability-allowlist enforcement and
    /// `transport.md` § Pre-identity both rest on: the two sets are disjoint.
    #[test]
    fn no_kind_is_both_pre_identity_and_class_allowlisted() {
        let router = crate::build_rpc_router();
        let classes = [
            CallerClass::BridgeMta,
            CallerClass::BridgeMda,
            CallerClass::BridgeAtprotoPds,
            CallerClass::ContentProcessor,
            CallerClass::User,
            CallerClass::Admin,
            CallerClass::Custodian,
            CallerClass::ThirdParty,
        ];
        for kind in router.iter_kinds() {
            if crate::pre_identity_allowlist::is_pre_identity_kind(kind) {
                assert!(
                    !classes.iter().any(|&c| is_permitted(c, kind)),
                    "{kind} is BOTH pre-identity and class-allowlisted — gate \
                     (1d) skips it, so its allowlist arm is dead and it is \
                     anonymously reachable"
                );
            }
        }
    }

    #[test]
    fn class_refusal_namespace_derives_the_family_for_listed_kinds() {
        assert_eq!(
            class_refusal_namespace("fauna.admin.invite_requests.list"),
            Some("admin")
        );
        assert_eq!(
            class_refusal_namespace("fauna.labelers.publish"),
            Some("labelers")
        );
        // A listed bridges-family kind keeps the bridges code — same wire
        // bytes as the pre-ruling hard-coded refusal, deliberately.
        assert_eq!(
            class_refusal_namespace("fauna.bridges.atproto.record_session"),
            Some("bridges")
        );
        // A listed kind named OUTSIDE the `fauna.<ns>.…` shape derives from its
        // leading segment all the same: the bridge surfaces that keep their
        // upstream protocol's namespace are still statements about their own
        // family, never the central bridges code (which the revoked-actor arm
        // owns). These four are the whole set today.
        assert_eq!(
            class_refusal_namespace("bluesky.feed.thread"),
            Some("bluesky")
        );
        assert_eq!(class_refusal_namespace("nostr.badges.list"), Some("nostr"));
        assert_eq!(
            class_refusal_namespace("nostr.events.publish_signed"),
            Some("nostr")
        );
        // Mirrors the arm's own `#[cfg]`: excising `zaps` unlists this kind, so
        // it derives no family — the unlisted-kind arm, not a regression.
        #[cfg(feature = "zaps")]
        assert_eq!(class_refusal_namespace("nostr.zaps.total"), Some("nostr"));
    }

    #[test]
    fn class_refusal_namespace_is_none_for_unlisted_kinds() {
        // An unlisted kind keeps the central `fauna.bridges.permission_denied`
        // (`transport.md` add-a-kind recipe: an unlisted kind default-denies).
        assert_eq!(class_refusal_namespace("fauna.nonexistent.kind"), None);
        assert_eq!(class_refusal_namespace("not-even-a-kind"), None);
    }

    /// Companion to `every_registered_kind_is_gated`: every registered,
    /// non-pre-identity kind must also yield a refusal FAMILY, so the central
    /// gate's wrong-class refusal always carries the kind's own
    /// `fauna.<ns>.permission_denied` — a kind named outside the
    /// `fauna.<ns>.…` shape would silently fall back to the central bridges
    /// code, which is reserved for unlisted kinds and revoked actors.
    #[test]
    fn every_registered_kind_has_a_refusal_family() {
        let router = crate::build_rpc_router();
        for kind in router.iter_kinds() {
            if crate::pre_identity_allowlist::is_pre_identity_kind(kind) {
                continue;
            }
            assert!(
                class_refusal_namespace(kind).is_some(),
                "{kind} yields no refusal family for the central gate's \
                 wrong-class denial"
            );
        }
    }
}
