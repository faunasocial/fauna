//! Observable snapshot + action surface for the `mail-settings` page.
//!
//! `MailSettingsSnapshot` is what the per-app UI renders;
//! `MailSettingsAction` is what user clicks dispatch. The state
//! machine is the only writer of the snapshot; the UI is read-only.

use fauna_core::localized::LocalizedText;
use fauna_core::secret::SecretBytes;
use serde::{Deserialize, Serialize};

/// Record a producer-side error for a reactive `error-message` banner: log it
/// once at `warn` **and** store it for the next render. Every mail-settings
/// sub-machine surfaces failures by reactively painting `snapshot().error` on
/// each observer tick, so the per-app views can't log it (they'd re-log on
/// every repaint — observability.md § Log on the *event*, not the *paint*).
/// Funnelling every `error` write through here logs once, at the transition
/// that sets it, and — being shared Rust running under each app's installed
/// `fauna-log` subscriber — covers the banner on all seven apps at once
/// (priority #2). The message is the same user-facing string the banner shows,
/// so it is redaction-safe (already displayed); mirrors
/// `fauna_onboarding_machine::state::State::set_error`.
pub(crate) fn set_snapshot_error(error: &mut Option<String>, message: String) {
    tracing::warn!(target: "fauna_mail_settings", "{message}");
    *error = Some(message);
}

/// The dispatch tail every `dispatch(action)` method in this crate hand-copied
/// byte for byte: run the action match, and on a failure record it on the
/// shared `error-message` banner (via [`set_snapshot_error`]) and reset status
/// to idle. Each call site still clears the prior error and builds its own
/// action match beforehand — those genuinely vary per page (the client-side-only
/// short-circuit in `export`/`import`, the extra `last_import` clear in
/// `lists::MailListMembersMachine`) (round 201 of the shared-Rust lift
/// sweep).
#[macro_export]
macro_rules! dispatch_capturing_error {
    ($self:ident, $Status:ident, $match_expr:expr) => {{
        let result = $match_expr;
        if let Err(ref e) = result {
            let mut snap = $self.inner.lock().expect("snapshot mutex");
            $crate::state::set_snapshot_error(&mut snap.error, e.to_string());
            snap.status = $Status::Idle;
        }
        result
    }};
}

/// What the per-app UI renders. Mirrors the per-page IDs in
/// `tests/e2e-unified/ui.yaml` § `mail-settings`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct MailSettingsSnapshot {
    /// True iff **email** (SMTP submission + IMAP) is enabled for this actor
    /// (`MailConfig::is_mail_enabled` — keys on the `mail_enabled` flag, **not**
    /// merely on `msek.is_some()`). A CalDAV-only actor holds the shared MSEK but
    /// reads `enabled: false` here, with [`Self::caldav_enabled`] `true`.
    pub enabled: bool,
    /// True iff **CalDAV** (calendar) is enabled for this actor
    /// (`MailConfig::caldav_enabled`). Independent of [`Self::enabled`] — both
    /// ride the one shared MSEK + `default` credential
    /// (`caldav-server.md` § Independent enablement, § Authentication: "no
    /// separate CalDAV credential"). The per-app mail-settings page renders the
    /// shared **credential management** section (add / list / reveal / rotate),
    /// the serve-here toggle, and the connection-detail block whenever the mailbox
    /// is provisioned — i.e. `enabled || caldav_enabled` — so a CalDAV-only
    /// deployment (email off) can still obtain + manage its bridge password (the
    /// independent-enablement UX gap this field closes). The credential is the
    /// same one email AUTHs under, so nothing here is CalDAV-specific except which
    /// connection-detail lines (IMAP/SMTP vs CalDAV URL) the MUA-instructions block
    /// shows.
    pub caldav_enabled: bool,
    /// True iff **CardDAV** (contacts / address book) is enabled for this actor
    /// (`MailConfig::carddav_enabled`) — the contacts sibling of
    /// [`Self::caldav_enabled`], riding the same shared MSEK + `default`
    /// credential (`carddav-server.md` § Independent enablement). Joins the
    /// credential-section render predicate: the page shows credential
    /// management whenever the mailbox is provisioned — i.e.
    /// `enabled || caldav_enabled || carddav_enabled` — so an
    /// address-book-only deployment can still obtain + manage its bridge
    /// password.
    pub carddav_enabled: bool,
    /// True iff this actor serves **≥1 folder over WebDAV** — the per-actor
    /// WebDAV state (`webdav-server.md` § Independent enablement pt 1 + §
    /// Implementation status: "per-actor serve state — a 'serves ≥1 set'
    /// signal"). Nest-sourced, folded from `fauna.folders.list`
    /// (`FolderSummary::webdav_enabled`, which the nest only ever sets on an
    /// owned folder), **not** part of the synced `MailConfig`.
    ///
    /// Deliberately **not** the deployment-wide `webdav_enabled` toggle that
    /// `WebdavPolicyMachine` / `admin-files-webdav-enabled-toggle` read: that
    /// flag defaults **ON** for a real-domain box ("harmless-on — nothing is
    /// served until a set is flagged"), so gating the credential section on it
    /// would render credential management for every actor on every box,
    /// including actors with no mailbox at all — exactly what the CalDAV-only
    /// gating exists to prevent. It is also `Admin`-only to read
    /// (`fauna.bridges.get_mail_config`), and this is a `User`-class page.
    ///
    /// Joins [`Self::credential_management_reachable`] and gates the MUA block's
    /// `mail-settings-mua-webdav-url` row: a WebDAV URL is only mountable once
    /// the actor actually serves a set (an unserved actor's collection root is
    /// empty), so the row appears with the first served set.
    pub serves_webdav_set: bool,
    /// The **credential-management reachability predicate**, computed once in
    /// shared Rust (priority #2/#4) instead of re-derived by each of the 6
    /// clients: `enabled || caldav_enabled || carddav_enabled ||
    /// serves_webdav_set`. Drives the credential-management section (keys
    /// explainer, credentials list, add / reveal / rotate), the serve-here
    /// toggle, and the MUA-instructions block — every surface that exists
    /// because the actor has, or needs, the one shared MSEK + `default` bridge
    /// credential that AUTHs IMAP + SMTP + CalDAV + CardDAV + WebDAV.
    ///
    /// Named for `webdav-server.md` § Independent enablement pt 1
    /// ("Credential-management reachability predicate extends to
    /// `|| webdav_enabled`"); `mail-settings.md` § CalDAV-only mailbox calls the
    /// same predicate "mailbox provisioned". The per-protocol *connection-detail*
    /// rows inside the MUA block stay individually gated (IMAP/SMTP on
    /// [`Self::enabled`], CalDAV on [`Self::caldav_enabled`], WebDAV on
    /// [`Self::serves_webdav_set`]) — only the block itself rides this predicate.
    pub credential_management_reachable: bool,
    /// Whether *this* nest serves the user's mailbox over IMAP/CalDAV to
    /// external MUAs (the per-actor `actor_mail_serving` nest flag; default
    /// **on** / absent ⇒ on). User-set, caller-scoped — see
    /// `docs/goal/architecture/nest/deployment-home-with-public-relay.md`
    /// § MUA reach. Read from the nest on [`hydrate`], flipped by
    /// [`MailSettingsAction::SetServingEnabled`]. Orthogonal to `enabled`
    /// (which is the user's own mailbox/keys) — turning this off never gates
    /// the user's own in-client mail/Events reads, only where the *MDA*
    /// serves external IMAP/CalDAV clients.
    ///
    /// [`hydrate`]: crate::MailSettingsMachine::hydrate
    pub serving_enabled: bool,
    /// One entry per `MailConfig::credentials`. Order matches
    /// `MailConfig::credentials` so the per-app UI can use
    /// `mail-settings-credentials-list[N]` indexing without
    /// re-sorting.
    pub credentials: Vec<MailCredentialSummary>,
    /// Set when a rotation is mid-flight (sentinel persisted in
    /// `MailConfig::pending_rotation`); drives the "resume?"
    /// banner the per-app UI surfaces.
    pub pending_rotation: Option<PendingRotationStatus>,
    /// Hostnames + ports + AUTH guidance the MUA-instructions
    /// component renders. Provided at construction by the per-app
    /// glue layer (the values come from the nest's deployment
    /// config).
    pub mua: MuaInstructions,
    /// Drives the `mail-settings-status-indicator` element.
    pub status: SettingsStatus,
    /// Drives the `error-message` element; cleared on the next
    /// successful dispatch.
    pub error: Option<String>,
}

/// The never-enabled shape — what [`MailSettingsMachine::new`] seeds before the
/// first `hydrate`, and the base every fixture should build on with
/// `..Default::default()`.
///
/// Hand-written rather than derived because two fields are **catalog-aligned,
/// not zero-valued**: `serving_enabled` defaults *on* (the nest's
/// `actor_mail_serving` default is on / absent ⇒ on), and [`MuaInstructions`]
/// carries the 993/465/443 implicit-TLS ports. A `derive(Default)` would
/// silently invert the serving default.
///
/// Exists so that a snapshot field added later doesn't collide across every
/// branch that hand-lists the struct: struct-update fixtures grow cleanly where
/// exhaustive ones conflict on the grown axis.
impl Default for MailSettingsSnapshot {
    fn default() -> Self {
        Self {
            enabled: false,
            caldav_enabled: false,
            carddav_enabled: false,
            // Nest-sourced (`fauna.folders.list`); refreshed on the first
            // `hydrate`, like `serving_enabled`.
            serves_webdav_set: false,
            credential_management_reachable: false,
            // Default on / absent ⇒ on (the nest's `actor_mail_serving`
            // default). Refreshed from the nest on the first `hydrate`.
            serving_enabled: true,
            credentials: Vec::new(),
            pending_rotation: None,
            mua: MuaInstructions::placeholder(),
            status: SettingsStatus::Idle,
            error: None,
        }
    }
}

/// One row in the `mail-settings-credentials-list` component. The
/// secret is never surfaced in the snapshot — only the mail custody
/// (sealed under BackupKey) holds it.
///
/// Named `MailCredentialSummary` (not `CredentialSummary`) because the
/// FFI surface already exposes `fauna_client_dns::CredentialSummary`,
/// and UniFFI requires library-wide unique idents.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct MailCredentialSummary {
    pub credential_id: String,
    pub display_name: String,
    pub kind: CredentialKind,
    pub created_at: u64,
    /// The concrete MUA username for this credential with `{credential_id}`,
    /// `{domain}`, and the `default`→bare rule already resolved — only
    /// `{handle}` remains, substituted by the per-app renderer via
    /// [`resolve_mua_username`]. Built in shared Rust
    /// ([`MuaInstructions::username_for`]) so the per-credential row shows the
    /// exact `<handle>+<credential_id>@<domain>` (or bare `<handle>@<domain>`
    /// for `default`) a MUA must use — the user no longer has to infer it from
    /// the generic template (`mail-credentials.md` § Implementation status —
    /// the username-display gap).
    pub mua_username: String,
    /// **The row lost access and was kept on purpose** — the succession burn's
    /// *Compromised — access revoked* state (`mail-credentials.md` § Rotation
    /// and recovery → *Succession*). Every app renders such a row as dead: its
    /// password no longer exists anywhere, and the MUA username beside it now
    /// authenticates nothing. The row is there so the user knows what to set up
    /// again; the existing per-row revoke gesture removes it when they are done.
    pub revoked: bool,
}

/// UI-facing mirror of `fauna_core::data::MailCredentialKind`. Kept
/// separate so the state crate can evolve UI shape without changing
/// the at-rest schema (and vice versa).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum CredentialKind {
    Plain,
    OAuthBearer,
}

impl CredentialKind {
    /// The UI kind of a stored credential, or `None` for a kind this build
    /// does not name ([`fauna_core::data::MailCredentialKind::Other`]) — a
    /// credential this build can neither wrap under nor offer, which is why
    /// this is not a `From`: every caller decides what refusing means for it.
    pub fn of_stored(k: &fauna_core::data::MailCredentialKind) -> Option<Self> {
        match k {
            fauna_core::data::MailCredentialKind::Plain => Some(Self::Plain),
            fauna_core::data::MailCredentialKind::OAuthBearer => Some(Self::OAuthBearer),
            fauna_core::data::MailCredentialKind::Other(_) => None,
        }
    }
}

impl From<CredentialKind> for fauna_core::data::MailCredentialKind {
    fn from(k: CredentialKind) -> Self {
        match k {
            CredentialKind::Plain => Self::Plain,
            CredentialKind::OAuthBearer => Self::OAuthBearer,
        }
    }
}

/// Canonical label for a [`CredentialKind`], returned as [`LocalizedText`] so
/// each app resolves it through its own i18n runtime (mirrors
/// [`alias_kind_badge`](crate::alias_kind_badge) /
/// [`member_status_label`](crate::member_status_label) /
/// [`bridge_display_name`](crate::bridge_display_name)). Lifts the identical
/// two-arm map that linux/web/windows/apple/android each hard-coded for the
/// `mail-settings-credential-item-type` badge — the last un-lifted mail-settings
/// enum→label map, now one source of truth (priority #1/#2/#4). The keys are the
/// pre-existing `settings.mail.kind_*` strings every app already resolved
/// (web and linux had hard-coded the English values, bypassing i18n — adopting
/// this routes them through the shared catalog too).
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn credential_kind_badge(kind: CredentialKind) -> LocalizedText {
    match kind {
        CredentialKind::Plain => LocalizedText::key("settings.mail.kind_password"),
        CredentialKind::OAuthBearer => LocalizedText::key("settings.mail.kind_bearer"),
    }
}

/// Canonical label for the `mail-settings-status-indicator` — the `SettingsStatus`
/// (plus whether mail is `enabled`) mapped to a [`LocalizedText`] each app
/// resolves through its own i18n runtime. Lifts the identical four-arm decision
/// every app hand-rolled (linux was the reference, web mirrored it in
/// `MailSettingsSection.svelte` `statusLabel`, apple in FaunaKit
/// `mailStatusText(enabled:status:)`, android in `MailSettingsScreen.kt`) onto one
/// source of truth (priority #2/#4) — the same shape as [`credential_kind_badge`].
///
/// The `Idle` arm is **gated on `enabled`**: a disabled (or pre-hydrate, `enabled
/// == false`) mailbox reads `status_disabled`, never `status_enabled` — so the
/// indicator is a real cross-app "mail enabled" signal (the exact wording the
/// e2e `wait_for_enabled_status` probe reads). See
/// `docs/goal/ui/mail-settings.md` § the status indicator (`ln`).
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn settings_status_label(status: SettingsStatus, enabled: bool) -> LocalizedText {
    match status {
        SettingsStatus::Syncing => LocalizedText::key("settings.mail.status_syncing"),
        SettingsStatus::RotationInProgress {
            credentials_remaining,
        } => LocalizedText::key_arg(
            "settings.mail.status_rotation",
            "count",
            credentials_remaining.to_string(),
        ),
        SettingsStatus::Idle if enabled => LocalizedText::key("settings.mail.status_enabled"),
        SettingsStatus::Idle => LocalizedText::key("settings.mail.status_disabled"),
    }
}

/// How many credentials a rotation confirmed now re-wraps: the live rows of
/// `snapshot` not in `excluded_credentials` (a revoked row has no secret left to
/// re-wrap). The count `start_rotation` reports first, known at confirm — so an
/// app can paint the rotate form's progress line
/// (`RotationInProgress { credentials_remaining }` through
/// [`settings_status_label`]) while it holds the form open for the rotation,
/// before the machine's own snapshot reaches the page
/// (`docs/goal/ui/mail-settings.md` § Element visibility). Exported over UniFFI
/// so the native apps consume this one rule instead of re-deriving it.
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn rotation_rewrap_count(
    snapshot: &MailSettingsSnapshot,
    excluded_credentials: &[String],
) -> u64 {
    snapshot
        .credentials
        .iter()
        .filter(|c| !c.revoked && !excluded_credentials.contains(&c.credential_id))
        .count() as u64
}

/// Values for the `mail-settings-mua-instructions` component. Ports
/// default to 993 / 465 (implicit TLS) per
/// `docs/goal/behavior/mail-credentials.md` § MUA setup conventions.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct MuaInstructions {
    pub imap_host: String,
    pub imap_port: u16,
    pub smtp_host: String,
    pub smtp_port: u16,
    /// CalDAV server host + port the user's **calendar** app connects to:
    /// `mail.<domain>` / `443` (`caldav-server.md` § Network exposure — CalDAV is
    /// served at `mail.<domain>:443` via SNI passthrough, the same host as IMAP).
    /// CalDAV rides the one MDA bridge and AUTHs under the **same** `(actor,
    /// default)` credential as IMAP/SMTP (`caldav-server.md` § Authentication —
    /// "no separate CalDAV credential"), so these are connection-detail lines, not
    /// a second credential. Rendered as the
    /// `mail-settings-mua-caldav-host`/`-port` rows of `mail-settings-mua-instructions`
    /// whenever `caldav_enabled` — independent of email being on, so a CalDAV-only
    /// deployment still shows its calendar server URL (the IMAP/SMTP lines stay
    /// gated on `enabled`, as they describe the *email* protocol).
    pub caldav_host: String,
    pub caldav_port: u16,
    /// The WebDAV **collection-root URL** a file manager mounts —
    /// `https://mail.<domain>/webdav/` on a registrable-domain box, or
    /// `https://<locator>:<caldav_port>/webdav/` on a local-target box (the same
    /// any-locator carve-out the CalDAV host/port take; WebDAV rides the same MDA
    /// DAV listener). Rendered as the `mail-settings-mua-webdav-url` row of
    /// `mail-settings-mua-instructions` whenever
    /// [`MailSettingsSnapshot::serves_webdav_set`].
    ///
    /// A **full URL**, not a host/port pair, because — unlike CalDAV/CardDAV —
    /// no SRV autodiscovery exists for WebDAV, so this line is the primary setup
    /// surface the user copies (`mail-settings.md` § WebDAV files; the apex
    /// `/.well-known/webdav` 301s here, `webdav-server.md` slice 5). Built in
    /// shared Rust so all 7 apps copy an identical, correct URL (priority #2).
    pub webdav_url: String,
    /// The deployment's mail domain (the bare host of the node URL), used to
    /// build each credential's concrete MUA username. Empty until
    /// [`MuaInstructions::for_node_url`] derives it.
    pub domain: String,
    /// Format string with `{handle}`, `{credential_id}`, `{domain}`
    /// placeholders. Retained as the generic template the
    /// `mail-settings-mua-instructions` block can show; the **concrete**
    /// per-credential username (with `{credential_id}`/`{domain}` resolved and
    /// the `default`→bare rule applied) is [`MailCredentialSummary::mua_username`]
    /// — only `{handle}` is left for the per-app renderer to substitute via
    /// [`resolve_mua_username`]. For the credential_id `"default"`, the
    /// `+<credential_id>` segment is omitted per the spec § MUA-username
    /// convention.
    pub username_format: String,
    pub auth_mechanism: String,
}

impl MuaInstructions {
    /// Default placeholder text. Real values come from
    /// [`MuaInstructions::for_node_url`] at machine construction (the per-app
    /// glue passes the deployment's node URL).
    pub fn placeholder() -> Self {
        Self {
            imap_host: String::new(),
            imap_port: 993,
            smtp_host: String::new(),
            smtp_port: 465,
            caldav_host: String::new(),
            caldav_port: 443,
            webdav_url: String::new(),
            domain: String::new(),
            username_format: "{handle}+{credential_id}@{domain}".into(),
            auth_mechanism: "OAUTHBEARER (recommended) or PLAIN over TLS".into(),
        }
    }

    /// The `/webdav/` collection-root URL for a deployment reached at `host`,
    /// whose MDA DAV listener is on `dav_port`. Registrable domain → the
    /// router-fronted canonical `https://mail.<host>/webdav/` (443 implicit).
    /// Local target (bare IP / `localhost` / `.local`) → the bare locator with
    /// the direct-bound port, matching the CalDAV any-locator carve-out
    /// (`caldav-server.md` § Network exposure — Any-locator serving).
    fn webdav_url_for(host: &str, dav_port: u16) -> String {
        if !fauna_core::resolve::is_public_dns_name(host) {
            format!("https://{host}:{dav_port}/webdav/")
        } else {
            format!("https://mail.{host}/webdav/")
        }
    }

    /// Build the MUA connection-detail block from the deployment's node URL.
    /// Shared across every app (priority #2/#4) so the spec constants live
    /// once — the per-app glue calls this at `MailSettingsMachine`
    /// construction instead of hand-rolling the values.
    ///
    /// Conventions per `docs/goal/behavior/mail-credentials.md` § MUA setup +
    /// `docs/goal/behavior/caldav-server.md` § Network exposure:
    /// IMAP/SMTP host `mail.<domain>`, ports 993 / 465 (implicit TLS); CalDAV host
    /// `mail.<domain>`, port 443 (the same bridge host, fronted by the SNI router);
    /// username `<handle>+<credential_id>@<domain>` (the `+<id>` is dropped for the
    /// `default` credential — the per-app renderer applies that), AUTH
    /// OAUTHBEARER or PLAIN over TLS.
    ///
    /// **Any-locator host (Change B′ host-half).** On a **local-target** nest —
    /// reached by a bare IP / `localhost` / `.local` name, classified by the
    /// shared `fauna_core::resolve::is_public_dns_name` returning false (the
    /// `domains-and-tls-bootstrap.md` § local-target carve-out) — there is no
    /// registrable DNS name, so `mail.<host>` is nonsense that breaks mail +
    /// CalDAV host routing. The MDA serves IMAP/CalDAV on the **bare locator
    /// itself** (the self-signed floor cert, TOFU-accepted by the MUA;
    /// `caldav-server.md` § Network exposure — Any-locator serving), so the
    /// reported host is the bare locator with **no `mail.` prefix**. A
    /// registrable domain keeps the canonical `mail.<domain>`.
    ///
    /// **Change B′ port-half — the CalDAV port (`caldav-server.md` § Network
    /// exposure — admin-settable CalDAV port).** The reachable CalDAV port
    /// depends on the deployment shape, the same `is_public_dns_name` split as
    /// the host above:
    /// - a **registrable-domain** box is fronted by the SNI router at the public
    ///   `443` (the admin `caldav_port` singleton governs only the no-router
    ///   direct listener, so it is *not* the reachable port) → `443`;
    /// - a **local-target** box (bare IP / `localhost` / `.local`, incl. a
    ///   desktop-native nest) has no router and the MDA binds the admin-set port
    ///   directly → the admin `caldav_port` (default
    ///   [`DEFAULT_CALDAV_PORT`](fauna_protocol::bridge_routing::DEFAULT_CALDAV_PORT)
    ///   = 8443).
    ///
    /// This builds the **pre-hydrate default** from the node URL alone; the real
    /// admin-set port is threaded in once the [`MailSettingsMachine`] hydrates and
    /// calls [`MuaInstructions::apply_admin_caldav_port`] with the synced
    /// `FetchConfigReply.caldav_port` (read via `fauna.bridges.get_caldav_port`),
    /// which overrides the local-target port and leaves the registrable `443`
    /// untouched. IMAP/SMTP keep their direct `993`/`465` (not admin-settable).
    /// The mail `<domain>` is the node-URL host as a stand-in for the real claimed
    /// `add_local_domain` value.
    pub fn for_node_url(node_url: &str) -> Self {
        let (host, _port) = fauna_core::resolve::parse_node_address(node_url);
        let is_public_name = fauna_core::resolve::is_public_dns_name(&host);
        let mail_host = if is_public_name {
            format!("mail.{host}")
        } else {
            host.clone()
        };
        // Pre-hydrate default — overridden for a local-target box by
        // `apply_admin_caldav_port` on the first hydrate. A registrable
        // domain stays on the router-fronted public 443.
        let caldav_port = if is_public_name {
            443
        } else {
            fauna_protocol::bridge_routing::DEFAULT_CALDAV_PORT
        };
        Self {
            imap_host: mail_host.clone(),
            imap_port: 993,
            smtp_host: mail_host.clone(),
            smtp_port: 465,
            caldav_host: mail_host,
            caldav_port,
            // WebDAV shares the MDA DAV listener with CalDAV, so it takes the
            // same port; recomputed by `apply_admin_caldav_port` on hydrate.
            webdav_url: Self::webdav_url_for(&host, caldav_port),
            domain: host,
            username_format: "{handle}+{credential_id}@{domain}".into(),
            auth_mechanism: "OAUTHBEARER (recommended) or PLAIN over TLS".into(),
        }
    }

    /// Apply the nest-reported admin CalDAV port (the `caldav_port` singleton
    /// projected on `FetchConfigReply.caldav_port`, read by the
    /// [`MailSettingsMachine`] via `fauna.bridges.get_caldav_port` on hydrate).
    /// Only a **local-target** box (bare IP / `localhost` / `.local`) binds the
    /// admin port directly with no SNI router in front, so the synced port is the
    /// reachable one there and overrides the pre-hydrate default. On a
    /// **registrable-domain** box the CalDAV surface is router-fronted at the
    /// public `443` (the admin port governs only the no-router direct listener —
    /// `caldav-server.md` § Network exposure), so the synced value is *not* the
    /// reachable port and is ignored, keeping the canonical `443`. A `0` port is
    /// never reachable, so it is ignored too (the nest never reports 0 — its read
    /// returns the persisted port or `DEFAULT_CALDAV_PORT` — but a future/buggy
    /// peer must not blank the display).
    /// The WebDAV URL rides the same DAV listener, so it is rebuilt here too —
    /// keeping [`Self::webdav_url`] from going stale against the reachable port
    /// on a local-target box.
    pub fn apply_admin_caldav_port(&mut self, port: u16) {
        if port != 0 && !fauna_core::resolve::is_public_dns_name(&self.domain) {
            self.caldav_port = port;
            self.webdav_url = Self::webdav_url_for(&self.domain, self.caldav_port);
        }
    }

    /// The concrete MUA username for a credential, with `{credential_id}` and
    /// `{domain}` resolved and the `default`→bare rule applied — only `{handle}`
    /// remains, which the per-app renderer substitutes via
    /// [`resolve_mua_username`] with the logged-in handle. For the `default`
    /// credential the `+<credential_id>` segment is dropped, yielding
    /// `{handle}@<domain>`; any other credential yields
    /// `{handle}+<credential_id>@<domain>` (RFC 5233 sub-addressing, per
    /// `docs/goal/behavior/mail-credentials.md` § MUA setup conventions). The
    /// bug-prone parts (the suffix, the default-bare rule, the domain) live here
    /// in shared Rust so every app renders an identical, correct username
    /// (priority #2) — closing the gap where each app inferred it from the
    /// generic template and a user could pick the wrong (bare) form.
    pub fn username_for(&self, credential_id: &str) -> String {
        if credential_id == "default" {
            format!("{{handle}}@{}", self.domain)
        } else {
            format!("{{handle}}+{credential_id}@{}", self.domain)
        }
    }
}

/// Substitute the logged-in `handle` into a [`MailCredentialSummary::mua_username`]
/// template (which carries `{handle}` as its only remaining placeholder),
/// yielding the exact `local@domain` a third-party MUA configures. Shared so
/// every app performs the identical substitution rather than hand-rolling a
/// `replace` that could diverge (priority #2): linux calls it directly; native
/// apps via the `resolve_mua_username` UniFFI export
/// (`fauna-ffi/src/mail_admin.rs`); web via the `resolveMuaUsername` wasm twin
/// (`fauna-wasm/src/mail_admin.rs`).
pub fn resolve_mua_username(mua_username: &str, handle: &str) -> String {
    mua_username.replace("{handle}", handle)
}

/// Drives `mail-settings-status-indicator`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum SettingsStatus {
    /// Nothing in flight.
    Idle,
    /// A background re-snapshot or token refresh is in progress.
    Syncing,
    /// A rotation is in flight; the number of credentials still to
    /// re-wrap is shown so the per-app UI can render progress.
    /// `u64` (not `usize`) — UniFFI has no `usize`.
    RotationInProgress { credentials_remaining: u64 },
}

/// Surfaced when `MailConfig::pending_rotation` is `Some(_)` at
/// load time — the per-app UI uses this to render the "Resume?"
/// banner.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct PendingRotationStatus {
    pub credentials_remaining: Vec<String>,
}

/// Every user-driven write the state machine accepts. The per-app
/// UI builds one of these from a button click and calls
/// `MailSettingsMachine::dispatch(action)`.
///
/// Secret-bearing variants carry their bytes in [`SecretBytes`]
/// (zeroized on drop, redacted `Debug`); the action is consumed by the
/// dispatch path, so the per-app glue layer constructs a fresh
/// action per dispatch. `SecretBytes` is also what lets the action be a
/// `uniffi::Enum` (its UniFFI custom-type marshals as `Vec<u8>` on the
/// FFI boundary) while keeping the uniform `dispatch(action)` model on
/// all 7 apps.
///
/// `Deserialize` backs the wasm `from_js` decode of a dispatched action;
/// the secret marshals as a byte array on that path and as a `Vec<u8>`
/// custom-type on the UniFFI path. Not `Serialize` — an action carrying
/// a plaintext secret is never serialized back out.
#[derive(Debug, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum MailSettingsAction {
    /// First credential on a never-enabled actor; mints all three
    /// initial blobs (snapshot, wrapped-MSEK, submission-token).
    EnableMail {
        display_name: String,
        kind: CredentialKind,
        secret: SecretBytes,
    },
    /// New credential on an already-mail-enabled actor; mints the
    /// per-credential wrapped-MSEK + submission-token (no new
    /// snapshot).
    AddCredential {
        display_name: String,
        kind: CredentialKind,
        secret: SecretBytes,
    },
    /// Retire one credential (soft revoke per § Soft revoke); calls
    /// `revoke_wrapped_mls_blob` + `revoke_wrapped_submission_token`.
    /// MSEK is unchanged; other credentials keep working.
    RevokeCredential { credential_id: String },
    /// Disable mail entirely: bulk soft-revoke **every** credential (one
    /// `RevokeCredential` per row) and clear the MSEK +
    /// credentials list, returning the snapshot to `enabled: false`. Per
    /// `docs/goal/ui/mail-settings.md` § Disable mail. Deliberately does **not**
    /// touch the Admin-scoped, deployment-wide `set_mail_enabled` flag: that
    /// boots/tears the box-level s6 mail subsystem shared by every mailbox, so
    /// one user disabling their own mail must not turn mail off for everyone.
    DisableMail,
    /// Hard revoke — generate a fresh MSEK and re-wrap under every
    /// surviving credential. Resumable per § Rotation and recovery.
    /// `excluded_credentials` names credentials the user flagged as
    /// compromised; they are dropped before the rotation runs.
    StartRotation { excluded_credentials: Vec<String> },
    /// Re-enter the rotation flow from the persisted sentinel.
    /// Surfaced as the "Resume?" banner action.
    ResumeRotation,
    /// Flip whether *this* nest serves the user's mailbox over IMAP/CalDAV to
    /// external MUAs (the per-actor `actor_mail_serving` nest flag). User-set,
    /// caller-scoped — fires `fauna.bridges.set_mail_serving_enabled` for the
    /// authenticated actor only (no "set another user's flag" path; the admin
    /// view is read-only by design). Default **on**. See
    /// `docs/goal/architecture/nest/deployment-home-with-public-relay.md`
    /// § MUA reach.
    SetServingEnabled { enabled: bool },
    /// Provision the user's **existing** mailbox onto the nest *this* machine is
    /// connected to, reusing the fleet MSEK (`MailConfig::msek`, and the default
    /// credential) instead of minting a fresh MSEK. The home-with-public-relay
    /// path: after the user links their private home box to the public relay box,
    /// the client provisions the home box's read credential + recipient pubkey +
    /// MLS snapshot from the **same** MSEK the public box already holds, so mail
    /// the relay seals to the recipient key is decryptable by the home box's MDA
    /// (`docs/goal/architecture/nest/deployment-home-with-public-relay.md`
    /// § Inbound mail; `mail-credentials.md` § MSEK lifecycle — one MSEK per actor
    /// across every *box* the user owns, not just every app). **No submission
    /// token** — the home box runs no MTA (§ Outbound mail); it gets only the
    /// read credential. Idempotent atomic-replace, so re-running it (e.g. on the
    /// primary, or a retry) overwrites rather than duplicates. Errors if mail
    /// isn't enabled yet (no MSEK to reuse). The peer-bound machine the client
    /// builds for this carries the **primary's** config store (the MSEK source),
    /// not the peer nest's own (empty) mail custody.
    ProvisionRelayMailbox,
}

// [`SecretBytes`]'s UniFFI custom-type marshalling (as `bytes`/`Vec<u8>`) is
// registered once, blanket, in `fauna-core` (`fauna_core::secret`, behind
// `feature = "uniffi"`), so the secret-bearing `MailSettingsAction` variants
// here — and any other crate's `uniffi` types carrying a `SecretBytes` — share
// one registration. This crate's `uniffi` feature forwards `fauna-core/uniffi`.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn username_for_drops_suffix_for_default_else_subaddresses() {
        let mua = MuaInstructions::for_node_url("https://example.com:8443");
        // The `default` credential uses the bare `<handle>@<domain>` form …
        assert_eq!(mua.username_for("default"), "{handle}@example.com");
        // … every other credential carries the RFC 5233 `+<credential_id>`.
        assert_eq!(mua.username_for("iphone"), "{handle}+iphone@example.com");
        assert_eq!(
            mua.username_for("apple-mail"),
            "{handle}+apple-mail@example.com"
        );
    }

    #[test]
    fn webdav_url_is_the_router_fronted_collection_root_for_a_real_domain() {
        // `webdav-server.md` slice 5: the apex `/.well-known/webdav` 301s to the
        // collection root at `mail.<domain>` — 443 implicit, no port in the URL.
        let mua = MuaInstructions::for_node_url("https://example.com:8443");
        assert_eq!(mua.webdav_url, "https://mail.example.com/webdav/");
        // A registrable domain is router-fronted, so the admin's direct-listener
        // port never rewrites the URL (same carve-out as `caldav_port`).
        let mut mua = mua;
        mua.apply_admin_caldav_port(9443);
        assert_eq!(mua.webdav_url, "https://mail.example.com/webdav/");
    }

    #[test]
    fn webdav_url_takes_the_bare_locator_and_dav_port_on_a_local_target() {
        // Any-locator carve-out: no registrable name ⇒ no `mail.` prefix, and the
        // MDA binds the DAV listener directly, so the reachable port is in the URL.
        let mut mua = MuaInstructions::for_node_url("https://127.0.0.1:8443");
        assert_eq!(
            mua.webdav_url,
            format!(
                "https://127.0.0.1:{}/webdav/",
                fauna_protocol::bridge_routing::DEFAULT_CALDAV_PORT
            )
        );
        // WebDAV rides the same DAV listener as CalDAV, so the admin-set port
        // rewrites the URL too — it must not go stale against `caldav_port`.
        mua.apply_admin_caldav_port(9443);
        assert_eq!(mua.caldav_port, 9443);
        assert_eq!(mua.webdav_url, "https://127.0.0.1:9443/webdav/");
        // A `0` port is never reachable and must not blank the display.
        mua.apply_admin_caldav_port(0);
        assert_eq!(mua.webdav_url, "https://127.0.0.1:9443/webdav/");
    }

    #[test]
    fn resolve_mua_username_substitutes_handle() {
        // The renderer turns the snapshot template into the exact MUA login.
        assert_eq!(
            resolve_mua_username("{handle}+iphone@example.com", "test"),
            "test+iphone@example.com"
        );
        assert_eq!(
            resolve_mua_username("{handle}@example.com", "alice"),
            "alice@example.com"
        );
    }

    #[test]
    fn credential_kind_badge_maps_every_variant() {
        // Pins the two-arm CredentialKind → i18n-key map every app renders for
        // the `mail-settings-credential-item-type` badge (lifted from the
        // hand-rolled per-app maps — priority #1/#2/#4). The keys are the same
        // `settings.mail.kind_*` strings linux/windows/apple/android already used.
        for (kind, key) in [
            (CredentialKind::Plain, "settings.mail.kind_password"),
            (CredentialKind::OAuthBearer, "settings.mail.kind_bearer"),
        ] {
            assert_eq!(credential_kind_badge(kind).key, key, "{kind:?}");
        }
    }

    #[test]
    fn settings_status_label_maps_every_state() {
        // Pins the four-arm SettingsStatus(+enabled) → i18n-key decision every
        // app hand-rolled for the `mail-settings-status-indicator` (lifted to
        // one source of truth — priority #2/#4). The `Idle` arm is gated on
        // `enabled` so a disabled mailbox never reads "up to date".
        assert_eq!(
            settings_status_label(SettingsStatus::Syncing, true).key,
            "settings.mail.status_syncing"
        );
        assert_eq!(
            settings_status_label(SettingsStatus::Idle, true).key,
            "settings.mail.status_enabled"
        );
        // Disabled (and pre-hydrate, enabled == false) → "Mail is disabled".
        assert_eq!(
            settings_status_label(SettingsStatus::Idle, false).key,
            "settings.mail.status_disabled"
        );
        let rotating = settings_status_label(
            SettingsStatus::RotationInProgress {
                credentials_remaining: 3,
            },
            true,
        );
        assert_eq!(rotating.key, "settings.mail.status_rotation");
        assert_eq!(rotating.args.get("count").map(String::as_str), Some("3"));
    }

    #[test]
    fn mua_instructions_match_doc_conventions() {
        let mua = MuaInstructions::for_node_url("https://nest.example.com:8443");
        assert_eq!(mua.imap_host, "mail.nest.example.com");
        assert_eq!(mua.smtp_host, "mail.nest.example.com");
        assert_eq!(mua.imap_port, 993);
        assert_eq!(mua.smtp_port, 465);
        // CalDAV shares the bridge host on :443 (caldav-server.md § Network exposure).
        assert_eq!(mua.caldav_host, "mail.nest.example.com");
        assert_eq!(mua.caldav_port, 443);
        assert_eq!(mua.username_format, "{handle}+{credential_id}@{domain}");
        assert!(mua.auth_mechanism.contains("OAUTHBEARER"));
    }

    #[test]
    fn mua_instructions_local_target_uses_bare_host() {
        // A bare-IP / localhost / `.local` nest has no registrable DNS name, so
        // `mail.<locator>` is nonsense that breaks mail + CalDAV host routing
        // (`domains-and-tls-bootstrap.md` § local-target carve-out). The MDA
        // serves IMAP/CalDAV on the bare locator itself (the self-signed floor
        // cert, TOFU-accepted by the MUA). This is the host-half of Change B′
        // (`caldav-server.md` § Network exposure — Any-locator serving).
        for url in [
            "https://192.168.1.50:8443",
            "https://10.1.8.51",
            "http://localhost:3000",
            "https://pi.local",
        ] {
            let mua = MuaInstructions::for_node_url(url);
            let bare = fauna_core::resolve::parse_node_address(url).0;
            assert_eq!(mua.imap_host, bare, "imap host for {url}");
            assert_eq!(mua.smtp_host, bare, "smtp host for {url}");
            assert_eq!(mua.caldav_host, bare, "caldav host for {url}");
            assert!(
                !mua.imap_host.starts_with("mail."),
                "{url} must not get a `mail.` prefix"
            );
            assert_eq!(mua.domain, bare, "domain for {url}");
            // IMAP stays canonical (993, not admin-settable). CalDAV's pre-hydrate
            // default on a local-target box is now `DEFAULT_CALDAV_PORT` (the MDA
            // binds the admin port directly, no router) — `apply_admin_caldav_port`
            // then threads in the real synced port on hydrate (Change B′ port-half).
            assert_eq!(mua.imap_port, 993);
            assert_eq!(
                mua.caldav_port,
                fauna_protocol::bridge_routing::DEFAULT_CALDAV_PORT,
                "local-target pre-hydrate CalDAV port = DEFAULT_CALDAV_PORT for {url}"
            );
        }
    }

    #[test]
    fn mua_instructions_port_hidden_ipv6_loopback_is_a_local_target() {
        // A port-*hidden* bracketed IPv6 node URL used to reach
        // `is_public_dns_name` as the mangled fragment `"[:"`, which classifies
        // as a registrable public name — so the block a user pastes into their
        // MUA advertised host `mail.[:` on the router-fronted `443` instead of
        // the bare locator on the direct MDA port. Both halves of Change B′
        // (host and port) failed together, and every value here is user-visible.
        let mua = MuaInstructions::for_node_url("https://[::1]");
        assert_eq!(mua.imap_host, "[::1]");
        assert_eq!(mua.smtp_host, "[::1]");
        assert_eq!(mua.caldav_host, "[::1]");
        assert_eq!(mua.domain, "[::1]");
        assert!(!mua.imap_host.starts_with("mail."));
        assert_eq!(mua.imap_port, 993);
        assert_eq!(
            mua.caldav_port,
            fauna_protocol::bridge_routing::DEFAULT_CALDAV_PORT
        );
        assert_eq!(
            mua.webdav_url,
            format!(
                "https://[::1]:{}/webdav/",
                fauna_protocol::bridge_routing::DEFAULT_CALDAV_PORT
            )
        );

        // The synced admin port must still override on hydrate — the
        // local-target branch of `apply_admin_caldav_port` is keyed on the same
        // classification, so a mangled domain silently disabled it too.
        let mut mua = mua;
        mua.apply_admin_caldav_port(9443);
        assert_eq!(mua.caldav_port, 9443);
        assert_eq!(mua.webdav_url, "https://[::1]:9443/webdav/");
    }

    #[test]
    fn mua_instructions_use_the_host_not_the_userinfo() {
        // `mail.user` is not a hostname anyone can reach.
        let mua = MuaInstructions::for_node_url("https://user:pass@nest.example.com");
        assert_eq!(mua.imap_host, "mail.nest.example.com");
        assert_eq!(mua.domain, "nest.example.com");
        assert_eq!(mua.caldav_port, 443);
    }

    #[test]
    fn apply_admin_caldav_port_overrides_local_target_only() {
        // Local-target box: the synced admin port IS the reachable port (MDA binds
        // it directly, no SNI router) → override the pre-hydrate default.
        let mut local = MuaInstructions::for_node_url("https://192.168.1.50:8443");
        assert_eq!(
            local.caldav_port,
            fauna_protocol::bridge_routing::DEFAULT_CALDAV_PORT
        );
        local.apply_admin_caldav_port(9000);
        assert_eq!(local.caldav_port, 9000, "local-target takes the admin port");

        // Registrable-domain box: CalDAV is router-fronted at the public 443; the
        // admin singleton governs only the no-router direct listener, so it is NOT
        // the reachable port → keep 443 regardless of what the nest reports.
        let mut domain = MuaInstructions::for_node_url("https://nest.example.com:8443");
        assert_eq!(domain.caldav_port, 443);
        domain.apply_admin_caldav_port(9000);
        assert_eq!(
            domain.caldav_port, 443,
            "registrable domain stays on the router-fronted public 443"
        );

        // A 0 port is never reachable — ignored even on a local-target box.
        let mut local2 = MuaInstructions::for_node_url("https://10.1.8.51");
        let before = local2.caldav_port;
        local2.apply_admin_caldav_port(0);
        assert_eq!(local2.caldav_port, before, "port 0 is ignored");
    }

    #[test]
    fn rotation_rewraps_the_live_credentials_not_excluded() {
        let cred = |id: &str, revoked: bool| MailCredentialSummary {
            credential_id: id.into(),
            display_name: id.into(),
            kind: CredentialKind::Plain,
            created_at: 0,
            mua_username: String::new(),
            revoked,
        };
        let snapshot = MailSettingsSnapshot {
            credentials: vec![
                cred("default", false),
                cred("phone", false),
                cred("old", true),
            ],
            ..Default::default()
        };
        assert_eq!(
            rotation_rewrap_count(&snapshot, &[]),
            2,
            "a revoked row is not re-wrapped"
        );
        assert_eq!(rotation_rewrap_count(&snapshot, &["phone".into()]), 1);
        assert_eq!(
            rotation_rewrap_count(&MailSettingsSnapshot::default(), &[]),
            0
        );
    }
}
