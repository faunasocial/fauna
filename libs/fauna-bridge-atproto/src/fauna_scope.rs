//! The Fauna scope family — `fauna:<plane>:<verb>[:<qualifier>]`.
//!
//! `docs/goal/behavior/authorization-server.md` § Scope grammar → *The Fauna
//! family, exactly* owns every rule this module follows. The family is a
//! **closed table**: a scope string is grantable only when it matches an arm
//! below, and an arm exists only while a kind in the nest's `ThirdParty`
//! ceiling names it — or, for a bridge-doored arm, while the BridgeMda relay
//! kind that admits its token exists ([`FaunaScopeArm::door`]; a nest-side
//! sweep test holds both sets). A plane the goal doc lists and no slice has
//! built yet — `post` … — matches no arm and is `invalid_scope` at PAR; so does a
//! built arm's qualifier the arm does not accept (an `identity:op` class not
//! yet built, or a sovereign operation's name, which never will be).
//!
//! **A shipped arm's reach never widens.** A grant is frozen at its ceremony,
//! so mapping a new kind onto an existing string would enlarge every
//! outstanding grant with no consent event. New reach is a new verb or a new
//! qualifier, with a card row of its own.
//!
//! The two doors every caller already uses stay the only ones:
//! [`crate::authz::scope_grants_something`] answers by arm membership and
//! [`crate::authz::describe_scope`] renders the arm's card row. This module is
//! pure and wasm-clean, like the rest of the always-on core.

/// The prefix every Fauna-family scope carries — the family's membership.
///
/// Membership is not grantability: `fauna:events:subscribe` is a
/// Fauna-family scope (an access token for it names the nest's issuer) and
/// also one no arm grants today. The token's reader is the arm's
/// [`FaunaScopeArm::door`] — the nest's dispatch for most arms, the MDA's
/// WebDAV server for `fauna:folder:read:<id>`.
pub const FAUNA_SCOPE_PREFIX: &str = "fauna:";

/// `fauna:feed:read` — the nest's public timelines.
pub const SCOPE_FEED_READ: &str = "fauna:feed:read";

/// `fauna:feed:read:account` — the account's own reading of the feed plane:
/// its custom feeds, and its like and repost state on the public timelines,
/// public posts only.
pub const SCOPE_FEED_READ_ACCOUNT: &str = "fauna:feed:read:account";

/// The fixed qualifier of [`FaunaScopeArm::FeedReadAccount`] — the server's,
/// never the client's or the user's.
const FEED_ACCOUNT_QUALIFIER: &str = "account";

/// `fauna:feed:read:author` — one author's public posts on this server.
pub const SCOPE_FEED_READ_AUTHOR: &str = "fauna:feed:read:author";

/// The fixed qualifier of [`FaunaScopeArm::FeedReadAuthor`] — the server's,
/// never the client's or the user's.
const FEED_AUTHOR_QUALIFIER: &str = "author";

/// `fauna:identity:op:nostr.sign_event` — ask the nest to sign Nostr events
/// with the user's deposited key (`key-material-hierarchy.md` § *The oracle*).
pub const SCOPE_IDENTITY_OP_NOSTR_SIGN_EVENT: &str = "fauna:identity:op:nostr.sign_event";

/// `fauna:identity:op:nostr.nip44` — ask the nest to NIP-44 encrypt and
/// decrypt with the user's deposited key.
pub const SCOPE_IDENTITY_OP_NOSTR_NIP44: &str = "fauna:identity:op:nostr.nip44";

/// `fauna:identity:op:activitypub.http_sig` — ask the nest to sign an
/// outbound ActivityPub request with the user's actor key, to a host the
/// manifest's `activitypub_peers` member declares (`third-party.md` § The
/// manifest).
pub const SCOPE_IDENTITY_OP_ACTIVITYPUB_HTTP_SIG: &str = "fauna:identity:op:activitypub.http_sig";

/// `fauna:identity:op:post.author` — ask the nest to author public posts as
/// the user, and to retract the ones this client authored, signed with the
/// account's delegated authoring sub-key (`authorization-server.md` § Scope
/// grammar → *The planes ruled 2026-10-02*: the posting door, in place of the
/// retired `fauna:post:write`).
pub const SCOPE_IDENTITY_OP_POST_AUTHOR: &str = "fauna:identity:op:post.author";

/// `fauna:identity:op:profile.author` — ask the nest to edit the display
/// fields of the user's profile as the user, signed with the account's
/// delegated authoring sub-key (`authorization-server.md` § Scope grammar →
/// *The profile arm*: the write is this class, never an arm).
pub const SCOPE_IDENTITY_OP_PROFILE_AUTHOR: &str = "fauna:identity:op:profile.author";

/// `fauna:identity:op:atproto.service_auth` — ask the atproto bridge's
/// custodian for a service-auth token, bounded by the manifest's declared
/// `service_auth` set (`third-party.md` § The manifest); the resolver refuses
/// a document asking for it without declaring the set.
pub const SCOPE_IDENTITY_OP_ATPROTO_SERVICE_AUTH: &str = "fauna:identity:op:atproto.service_auth";

/// `fauna:identity:op:conversation.read` — ask the Fauna app open on the
/// user's device to list, read and search the account's rooms for the
/// principal, through the conversations oracle's local door
/// (`conversations-oracle.md` § The two classes). Refused to the remote form.
pub const SCOPE_IDENTITY_OP_CONVERSATION_READ: &str = "fauna:identity:op:conversation.read";

/// `fauna:identity:op:conversation.send` — ask the Fauna app open on the
/// user's device to send a text message as the account into a room.
/// Refused to the remote form.
pub const SCOPE_IDENTITY_OP_CONVERSATION_SEND: &str = "fauna:identity:op:conversation.send";

/// `fauna:records:rw` — the `records` arm's plane and verb, as discovery
/// advertises it. Never grantable as written: the arm requires its qualifier
/// (`fauna:records:rw:<ext kind | ext.<publisher>.*>`), which is the client's.
pub const SCOPE_RECORDS_RW: &str = "fauna:records:rw";

/// `fauna:conversations:bridge` — carry the account's bridged conversations
/// (`apps/bridges.md` § Bridge-kind catalogue → Phase G).
pub const SCOPE_CONVERSATIONS_BRIDGE: &str = "fauna:conversations:bridge";

/// `fauna:folder:deposit` — the `folder_deposit` arm's plane and verb, as
/// discovery advertises it and a client document declares it. Requestable
/// bare, never grantable as written: the arm requires its qualifier, the
/// folder's row id (`fauna:folder:deposit:<id>`), which the user chooses at
/// consent ([`qualify_bare`]).
pub const SCOPE_FOLDER_DEPOSIT: &str = "fauna:folder:deposit";

/// `fauna:folder:read` — the `folder_read` arm's plane and verb, as discovery
/// advertises it. Never grantable as written: the arm requires its qualifier,
/// the folder's row id (`fauna:folder:read:<id>`).
pub const SCOPE_FOLDER_READ: &str = "fauna:folder:read";

/// `fauna:personalization:rw` — read and train the account's sealed
/// personalization models (`authorization-server.md` § Scope grammar → *The
/// fifth arm*). Device apps and nest-hosted code only: PAR refuses it to a
/// remote-form client.
pub const SCOPE_PERSONALIZATION_RW: &str = "fauna:personalization:rw";

/// `fauna:events:subscribe` — be told which of the principal's own scopes
/// changed (`transport.md` § Push events → *Third-party event doors*).
pub const SCOPE_EVENTS_SUBSCRIBE: &str = "fauna:events:subscribe";

/// `fauna:follow:write` — follow and unfollow as the account, the free
/// `followers` tier only (`authorization-server.md` § Scope grammar → *The
/// sixth arm*).
pub const SCOPE_FOLLOW_WRITE: &str = "fauna:follow:write";

/// `fauna:admin:read` — the admin plane's read verb, bare: the plane and verb
/// discovery names as the family's shape and no grant takes as written
/// (`authorization-server.md` § Scope grammar → *The admin arms*). The
/// qualifier is REQUIRED and is the server's: one built [`AdminPage`].
pub const SCOPE_ADMIN_READ: &str = "fauna:admin:read";

/// `fauna:admin:read:dashboard` — read the server's admin dashboard as its
/// admin: the member, storage and connection counts and the running version
/// (`fauna.admin.stats`, `fauna.admin.status`). The first admin string built.
pub const SCOPE_ADMIN_READ_DASHBOARD: &str = "fauna:admin:read:dashboard";

/// `fauna:admin:read:users` — read the server's members as its admin: the
/// member list, evictions, invitations and invite requests, the tiers, the
/// admin roster and the pending admin actions.
pub const SCOPE_ADMIN_READ_USERS: &str = "fauna:admin:read:users";

/// `fauna:admin:rw` — the admin plane's read-and-write verb, bare: the shape
/// discovery names and no grant takes as written. The qualifier is REQUIRED
/// and is one built [`AdminPage`] that has writes ([`AdminPage::has_writes`]).
pub const SCOPE_ADMIN_RW: &str = "fauna:admin:rw";

/// `fauna:admin:rw:users` — administer the server's members as its admin:
/// the users page's reads and its non-sovereign writes (admitting,
/// suspending and reinstating, tiers, invitations, invite requests and the
/// registration posture). The first `rw` string built.
pub const SCOPE_ADMIN_RW_USERS: &str = "fauna:admin:rw:users";

/// `fauna:admin:read:settings` — read the server's tier settings as its
/// admin: the membership designations linking the admin's subscription tiers
/// to quota tiers (the tier list itself is `read:users`').
pub const SCOPE_ADMIN_READ_SETTINGS: &str = "fauna:admin:read:settings";

/// `fauna:admin:rw:settings` — define the server's tiers as its admin:
/// create, edit and delete a tier, and set or clear a membership designation.
pub const SCOPE_ADMIN_RW_SETTINGS: &str = "fauna:admin:rw:settings";

/// `fauna:admin:read:nest` — read the server's nest-wide settings as its
/// admin: region, web-app origin, update channel, services, transport and
/// feature and content policies, the worker and cluster status, the public
/// sign-in key set, and the reports queue and blocked list.
pub const SCOPE_ADMIN_READ_NEST: &str = "fauna:admin:read:nest";

/// `fauna:admin:rw:nest` — operate the server's nest-wide settings as its
/// admin: those reads and their writes, a host restart, garbage collection,
/// resolving reports and blocking a reported item. Never the factory reset,
/// the deployment seed, a key rotation, a legal takedown or custody hosting.
pub const SCOPE_ADMIN_RW_NEST: &str = "fauna:admin:rw:nest";

/// `fauna:admin:read:logs` — read the server's logs and its admin audit log
/// (and the audit chain's integrity) as its admin. A page of reads only.
pub const SCOPE_ADMIN_READ_LOGS: &str = "fauna:admin:read:logs";

/// `fauna:admin:read:web` — read which member's site the server's apex
/// address serves, as its admin.
pub const SCOPE_ADMIN_READ_WEB: &str = "fauna:admin:read:web";

/// `fauna:admin:rw:web` — designate or clear the apex actor as the server's
/// admin.
pub const SCOPE_ADMIN_RW_WEB: &str = "fauna:admin:rw:web";

/// `fauna:admin:read:dns` — read the server's DNS as its admin: the record
/// matrix, a live verify, a TXT visibility probe, the served TLS
/// certificates' health, the local domains, the DKIM selectors and the
/// primary-domain rename's state.
pub const SCOPE_ADMIN_READ_DNS: &str = "fauna:admin:read:dns";

/// `fauna:admin:rw:dns` — operate the server's domains as its admin: those
/// reads, adding, removing and restoring a local domain and its settings, the
/// catch-all and role addresses and the DKIM rotation interval. Never the host
/// address, the primary-domain rename, a DKIM rotation or revocation, or a
/// certificate write.
pub const SCOPE_ADMIN_RW_DNS: &str = "fauna:admin:rw:dns";

/// `fauna:admin:read:mail` — read the server's mail configuration as its
/// admin: the effective policies, the alias and DMARC policies, the mail
/// health readout, the warm-up and the spam-baseline state.
pub const SCOPE_ADMIN_READ_MAIL: &str = "fauna:admin:read:mail";

/// `fauna:admin:rw:mail` — the mail page's reads and its non-sovereign
/// writes: enabling mail, the new-user default, every policy, the
/// deliverability checks, the warm-up reset and the spam baseline. The
/// secret rotations and the Microsoft registration stay out.
pub const SCOPE_ADMIN_RW_MAIL: &str = "fauna:admin:rw:mail";

/// `fauna:admin:read:mail-reports` — read the reports the server received
/// and ran: DMARC aggregate and forensic, TLS reports, the blocklist and
/// diagnostic histories. Reads only, so no `rw` string.
pub const SCOPE_ADMIN_READ_MAIL_REPORTS: &str = "fauna:admin:read:mail-reports";

/// `fauna:admin:read:aliases` — read the server's external forwarders.
pub const SCOPE_ADMIN_READ_ALIASES: &str = "fauna:admin:read:aliases";

/// `fauna:admin:rw:aliases` — list, create and delete external forwarders.
pub const SCOPE_ADMIN_RW_ALIASES: &str = "fauna:admin:rw:aliases";

/// `fauna:admin:read:calendar` — read the CalDAV port.
pub const SCOPE_ADMIN_READ_CALENDAR: &str = "fauna:admin:read:calendar";

/// `fauna:admin:rw:calendar` — turn CalDAV on or off and set its port.
pub const SCOPE_ADMIN_RW_CALENDAR: &str = "fauna:admin:rw:calendar";

/// `fauna:admin:rw:contacts` — turn CardDAV on or off. A page of one write
/// and no read of its own, so no `read` string ([`AdminPage::has_reads`]).
pub const SCOPE_ADMIN_RW_CONTACTS: &str = "fauna:admin:rw:contacts";

/// `fauna:admin:rw:files` — turn WebDAV on or off; the files twin of
/// [`SCOPE_ADMIN_RW_CONTACTS`].
pub const SCOPE_ADMIN_RW_FILES: &str = "fauna:admin:rw:files";

/// `fauna:admin:read:bridges-pending` — read the bridges waiting for
/// approval and the approved service users. Approving, rejecting, revoking
/// and re-keying are the grant plane, sovereign, so no `rw` string.
pub const SCOPE_ADMIN_READ_BRIDGES_PENDING: &str = "fauna:admin:read:bridges-pending";

/// The admin plane's qualifier set — one member per BUILT non-sovereign
/// `admin-*` ui.yaml page, named by the page minus its `admin-` prefix
/// (`authorization-server.md` § Scope grammar → *The admin arms*). A closed
/// set of the server's, like the oracle's classes: a page no slice has built
/// matches no arm and is `invalid_scope` at PAR, and the nest's sweep test
/// holds every member to at least one ceiling kind. The string says what the
/// admin consented to; whether the account may still administer is read per
/// RPC on the nest (`apps/bridges.md` § Capability-allowlist enforcement →
/// rule (e)), never from the string.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AdminPage {
    /// `admin-dashboard` — the counts and the running version; reads only.
    Dashboard,
    /// `admin-users` — the member lifecycle: reads, and the non-sovereign
    /// writes (deletion, eviction, handles and the admin roster stay out).
    Users,
    /// `admin-settings` (nav label "Tiers") — tier definitions and the
    /// membership designations.
    Settings,
    /// `admin-nest` — the nest-wide settings and the moderation queue (the
    /// factory reset, the deployment seed, key rotations and the legal
    /// takedown stay out).
    Nest,
    /// `admin-logs` — the log lines and the admin audit log; reads only.
    Logs,
    /// `admin-web` — the apex-actor designation.
    Web,
    /// `admin-dns` — the record matrix, certificate health and the local
    /// domains (the host address, the primary-domain rename, DKIM rotation
    /// and certificate writes stay out).
    Dns,
    /// `admin-mail` — the deployment's mail switch, policies and health
    /// (the secret rotations and the Microsoft registration stay out).
    Mail,
    /// `admin-mail-reports` — the received and run reports; reads only.
    MailReports,
    /// `admin-aliases` — the external forwarders.
    Aliases,
    /// `admin-calendar` — the CalDAV switch and port.
    Calendar,
    /// `admin-contacts` — the CardDAV switch; one write, no read.
    Contacts,
    /// `admin-files` — the WebDAV switch; one write, no read.
    Files,
    /// `admin-bridges-pending` — the pending and approved bridge rosters'
    /// reads; every write of the page is the grant plane, sovereign.
    BridgesPending,
}

impl AdminPage {
    /// Every built page, in the order discovery advertises their strings.
    pub const ALL: &'static [AdminPage] = &[
        AdminPage::Dashboard,
        AdminPage::Users,
        AdminPage::Settings,
        AdminPage::Nest,
        AdminPage::Logs,
        AdminPage::Web,
        AdminPage::Dns,
        AdminPage::Mail,
        AdminPage::MailReports,
        AdminPage::Aliases,
        AdminPage::Calendar,
        AdminPage::Contacts,
        AdminPage::Files,
        AdminPage::BridgesPending,
    ];

    /// The qualifier: the ui.yaml page minus its `admin-` prefix.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            AdminPage::Dashboard => "dashboard",
            AdminPage::Users => "users",
            AdminPage::Settings => "settings",
            AdminPage::Nest => "nest",
            AdminPage::Logs => "logs",
            AdminPage::Web => "web",
            AdminPage::Dns => "dns",
            AdminPage::Mail => "mail",
            AdminPage::MailReports => "mail-reports",
            AdminPage::Aliases => "aliases",
            AdminPage::Calendar => "calendar",
            AdminPage::Contacts => "contacts",
            AdminPage::Files => "files",
            AdminPage::BridgesPending => "bridges-pending",
        }
    }

    /// The ui.yaml page the qualifier names.
    #[must_use]
    pub const fn ui_page(self) -> &'static str {
        match self {
            AdminPage::Dashboard => "admin-dashboard",
            AdminPage::Users => "admin-users",
            AdminPage::Settings => "admin-settings",
            AdminPage::Nest => "admin-nest",
            AdminPage::Logs => "admin-logs",
            AdminPage::Web => "admin-web",
            AdminPage::Dns => "admin-dns",
            AdminPage::Mail => "admin-mail",
            AdminPage::MailReports => "admin-mail-reports",
            AdminPage::Aliases => "admin-aliases",
            AdminPage::Calendar => "admin-calendar",
            AdminPage::Contacts => "admin-contacts",
            AdminPage::Files => "admin-files",
            AdminPage::BridgesPending => "admin-bridges-pending",
        }
    }

    /// Whether the page has reads of its own — whether
    /// `fauna:admin:read:<page>` is a string at all. A page whose only kinds
    /// are writes (the CardDAV and WebDAV switches) takes no `read` string:
    /// an arm exists only while a ceiling kind names it.
    #[must_use]
    pub const fn has_reads(self) -> bool {
        !matches!(self, AdminPage::Contacts | AdminPage::Files)
    }

    /// Whether the page has non-sovereign writes — whether
    /// `fauna:admin:rw:<page>` is a string at all. A page of reads only takes
    /// no `rw` string: an arm exists only while a ceiling kind names it.
    #[must_use]
    pub const fn has_writes(self) -> bool {
        !matches!(
            self,
            AdminPage::Dashboard
                | AdminPage::Logs
                | AdminPage::MailReports
                | AdminPage::BridgesPending
        )
    }

    /// The page a qualifier names; `None` for a page no slice has built.
    #[must_use]
    pub fn parse(qualifier: &str) -> Option<Self> {
        AdminPage::ALL
            .iter()
            .copied()
            .find(|p| p.name() == qualifier)
    }

    /// The read verb's card row: what an admin consents to when the string
    /// is `fauna:admin:read:<page>` — every row says it acts as this server's
    /// admin and that no keys are shared, because nothing on the admin plane
    /// is sealed. `None` for a page with no reads, whose `read` string
    /// matches no arm.
    #[must_use]
    pub const fn read_card_row(self) -> Option<&'static str> {
        match self {
            AdminPage::Dashboard => Some(
                "As this server's admin, read its dashboard: member, storage and connection \
                 counts and the running version (no keys are shared)",
            ),
            AdminPage::Users => Some(
                "As this server's admin, read its members: the member list, invitations and \
                 invite requests, tiers, the admin roster and pending admin actions (no keys \
                 are shared)",
            ),
            AdminPage::Settings => Some(
                "As this server's admin, read its membership designations: which of your \
                 subscription tiers admits members at which quota tier (no keys are shared)",
            ),
            AdminPage::Nest => Some(
                "As this server's admin, read its nest settings: region, web-app origin, \
                 update channel, services, transport, feature and content policies, worker and \
                 cluster status, the public sign-in keys, and the reports queue and blocked \
                 list (no keys are shared)",
            ),
            AdminPage::Logs => Some(
                "As this server's admin, read its logs and its admin audit log (no keys are \
                 shared)",
            ),
            AdminPage::Web => Some(
                "As this server's admin, read whose site its main address shows (no keys are \
                 shared)",
            ),
            AdminPage::Dns => Some(
                "As this server's admin, read its domains and their DNS records, check them \
                 live and read its certificates' health (no keys are shared)",
            ),
            AdminPage::Mail => Some(
                "As this server's admin, read its mail settings: whether mail is on, its spam, \
                 sender, submission, IMAP, outbound, alias and DMARC policies, and its mail \
                 health (no keys are shared)",
            ),
            AdminPage::MailReports => Some(
                "As this server's admin, read the mail reports it received and ran: DMARC and \
                 TLS reports and the blocklist and deliverability check history (no keys are \
                 shared)",
            ),
            AdminPage::Aliases => Some(
                "As this server's admin, read the addresses it forwards to outside mailboxes \
                 (no keys are shared)",
            ),
            AdminPage::Calendar => Some(
                "As this server's admin, read the port its calendar server listens on (no keys \
                 are shared)",
            ),
            AdminPage::Contacts | AdminPage::Files => None,
            AdminPage::BridgesPending => Some(
                "As this server's admin, read the bridges waiting for approval and the bridges \
                 already approved — never approve, reject or re-key one (no keys are shared)",
            ),
        }
    }

    /// The `rw` verb's card row — `None` for a page of reads only, whose
    /// `rw` string matches no arm. Each row names what the string never
    /// does, the sovereign acts of its page (`apps/cli.md` § The
    /// sovereign-act boundary → *The admin plane's sovereign acts*).
    #[must_use]
    pub const fn rw_card_row(self) -> Option<&'static str> {
        match self {
            AdminPage::Dashboard => None,
            AdminPage::Users => Some(
                "As this server's admin, administer its members: admit, suspend and reinstate \
                 them, their tiers, invitations and invite requests, and who may register — \
                 never delete one, clear a handle or change who is an admin (no keys are \
                 shared)",
            ),
            AdminPage::Settings => Some(
                "As this server's admin, define its tiers: create, edit and delete a tier and \
                 set which subscription tier admits members at which quota tier (no keys are \
                 shared)",
            ),
            AdminPage::Nest => Some(
                "As this server's admin, operate its nest settings: region, web-app origin, \
                 update channel, services, ports and policies, a host restart and cleanup, and \
                 resolving reports and blocking a reported item — never a factory reset, a key \
                 or seed rotation or a legal takedown (no keys are shared)",
            ),
            AdminPage::Web => Some(
                "As this server's admin, choose whose site its main address shows (no keys are \
                 shared)",
            ),
            AdminPage::Dns => Some(
                "As this server's admin, operate its domains: add, remove and restore one, its \
                 catch-all and role addresses and how often its DKIM keys rotate — never rename \
                 the main domain, rotate a key or change a certificate or the server's address \
                 (no keys are shared)",
            ),
            AdminPage::Mail => Some(
                "As this server's admin, run its mail: turn mail on or off, set its policies, \
                 run its deliverability checks, reset the warm-up and publish the spam \
                 baseline — never rotate a mail secret or change the Microsoft sign-in (no \
                 keys are shared)",
            ),
            AdminPage::Aliases => Some(
                "As this server's admin, add and remove the addresses it forwards to outside \
                 mailboxes (no keys are shared)",
            ),
            AdminPage::Calendar => Some(
                "As this server's admin, turn its calendar server on or off and set its port \
                 (no keys are shared)",
            ),
            AdminPage::Contacts => Some(
                "As this server's admin, turn its contacts server on or off (no keys are \
                 shared)",
            ),
            AdminPage::Files => {
                Some("As this server's admin, turn its files server on or off (no keys are shared)")
            }
            AdminPage::Logs | AdminPage::MailReports | AdminPage::BridgesPending => None,
        }
    }
}
/// `fauna:profile:read` — the public profile records this nest holds, the
/// account's own and anyone's (`authorization-server.md` § Scope grammar →
/// *The profile arm*).
pub const SCOPE_PROFILE_READ: &str = "fauna:profile:read";

/// `fauna:notifications:read` — list the account's notifications and read its
/// unread count (`authorization-server.md` § Scope grammar → *The
/// notifications arms*).
pub const SCOPE_NOTIFICATIONS_READ: &str = "fauna:notifications:read";

/// `fauna:notifications:write` — mark the account's notifications read, never
/// dismiss or clear them (`authorization-server.md` § Scope grammar → *The
/// notifications arms*).
pub const SCOPE_NOTIFICATIONS_WRITE: &str = "fauna:notifications:write";

/// The BridgeMda relay kind that admits a `fauna:folder:read` token at the
/// MDA's WebDAV door (`webdav-server.md` § Key model → *A principal's read*):
/// the read arm's [`Door::Mda`]. Spelled here, beside the arm, because the
/// grammar crate sits below the protocol crate; the nest's allowlist and the
/// Go MDA's method table carry the same string, and the nest's sweep test
/// holds them equal.
pub const WEBDAV_ADMIT_PRINCIPAL_KIND: &str = "fauna.bridges.webdav_admit_principal";

/// Where an arm's token is honoured (`authorization-server.md` § Scope
/// grammar → *A closed table*, amended 2026-10-05): every arm has exactly one
/// door.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Door {
    /// The nest's own dispatch: the arm exists while a kind in the
    /// `ThirdParty` ceiling names it.
    Nest,
    /// A bridge's resource server: the arm exists while the BridgeMda relay
    /// kind that admits its token exists — `relay_kind` names it.
    Mda { relay_kind: &'static str },
}

/// One built arm of the family. Closed: a new arm is a new variant, added in
/// the same change as the nest-side ceiling kind that names it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FaunaScopeArm {
    /// `fauna:feed:read` — no qualifier, no grant twin. The nest's public
    /// timelines as any reader of them would get them: public posts only,
    /// nothing of the account's own state.
    FeedRead,
    /// `fauna:feed:read:account` — the qualifier is REQUIRED and is the fixed
    /// word `account`; no grant twin (nothing it reads is sealed). The
    /// account's own reading of the feed plane (`authorization-server.md`
    /// § Scope grammar → *The fourth arm*): the three custom-feed kinds over
    /// the account's own feeds, and — a *refining* qualifier arm
    /// (`apps/bridges.md` rule (c)) — the account's viewer state on the two
    /// public timelines the bare arm covers, which stays their one arm
    /// ([`reads_feed_as_account`]). Public posts only on every kind: a gated
    /// or room-restricted post never appears.
    FeedReadAccount,
    /// `fauna:feed:read:author` — the qualifier is REQUIRED and is the fixed
    /// word `author`; no grant twin (nothing it reads is sealed), any
    /// execution form. The one kind `fauna.feed.author.posts`: the posts of
    /// the one author the request names, public posts only
    /// (`authorization-server.md` § Scope grammar → *The author arm*). The
    /// account arm refines it as it refines the two public timelines
    /// ([`reads_feed_as_account`]).
    FeedReadAuthor,
    /// `fauna:identity:op:<class>` — the keyless oracle arm
    /// (`key-material-hierarchy.md` § Audience: deployment infrastructure →
    /// *The oracle*). The qualifier names one built
    /// [`fauna_core::identity_op::IdentityOpClass`]; a sovereign or unknown
    /// name matches no arm. The principal asks the key's custodian to perform
    /// the class and never holds the key.
    IdentityOp,
    /// `fauna:records:rw:<qualifier>` — read and write the principal's own
    /// third-party kinds (`third-party-kinds.md` § The record doors). The
    /// qualifier is REQUIRED and is one `ext.*` kind or `ext.<publisher>.*`,
    /// parsed by [`fauna_core::ext_kind::ExtQualifier`] — the wildcard covers
    /// one publisher's kinds structurally, never by string prefix. Whether a
    /// wildcard names the requesting document's own host is PAR's check
    /// ([`records_qualifier`]), not the grammar's.
    Records,
    /// `fauna:conversations:bridge` — no qualifier, no grant twin. The
    /// bridge's six Phase G kinds plus the recipient-key read its deposits are
    /// sealed with (`apps/bridges.md` § Bridge-kind catalogue → Phase G). The
    /// nest is blind both ways: inbound is sealed by the bridge to the user's
    /// recipient key, outbound by the user's app to the bridge's own key — so
    /// no key of the account's is wrapped to the principal.
    ConversationsBridge,
    /// `fauna:folder:deposit:<folder id>` — post files into one of the
    /// account's folders, write-only and blind (`file-sync.md` § Third-party
    /// deposit ingress). The qualifier is REQUIRED and is the folder's row id
    /// in canonical decimal ([`folder_deposit_qualifier`]). The nest seals
    /// what is deposited to the owner's recipient key; the principal reads
    /// nothing and holds no key.
    FolderDeposit,
    /// `fauna:folder:read:<folder id>` — read the files in one of the
    /// account's folders over the MDA's WebDAV door (`webdav-server.md`
    /// § Key model → *A principal's read*). The qualifier is REQUIRED, the
    /// deposit arm's canonical decimal row id ([`folder_read_qualifier`]).
    /// Its twin is keyed — the `content.read{folder, set}` grant, one wrap
    /// per content-key generation — and its door is the MDA, not the nest
    /// ([`Door::Mda`]). Form-gated at PAR: a remote-form (confidential)
    /// client is refused it ([`FaunaScopeArm::refused_to_remote_form`]).
    FolderRead,
    /// `fauna:personalization:rw` — no qualifier, a keyed grant twin. The
    /// sealed personalization plane (`authorization-server.md` § Scope
    /// grammar → *The fifth arm*): the model fetch and put, and the `state`
    /// scope's feed, read only — the trained-factor registry is the sealed
    /// account-plane kind `fauna.state.personalization`, listed as the
    /// delegable cluster's ciphertext. Its twin is two `content.read` tuples,
    /// each the ordinary delegable unit: the registry kind's pair and the
    /// `fauna.personalization.model` pair the blobs are sealed under.
    /// Form-gated at PAR: TP4 reserves personalization models for device
    /// apps and nest-hosted code, so a remote-form client is refused it
    /// ([`FaunaScopeArm::refused_to_remote_form`]).
    PersonalizationRw,
    /// `fauna:events:subscribe` — no qualifier, no grant twin. The events
    /// doors (`transport.md` § Push events → *Third-party event doors*): the
    /// scope-tagged change nudge for exactly the scopes the session's other
    /// arms let it list ([`event_reaches`]), never content. It covers the
    /// `fauna.events.poll` kind; the filtered push on a principal session and
    /// the HTTP long-poll are that kind's other two faces.
    EventsSubscribe,
    /// `fauna:follow:write` — no qualifier, no grant twin. Follow and unfollow
    /// as the account: subscribing to and leaving the free `followers` tier
    /// (`monetization.md` § The unifying model), and the followers bit of the
    /// subscription status — never a paid tier. A nest-side relationship row,
    /// no key of the account's and no signature; the subscriber's ML-KEM
    /// encapsulation key the author wraps to is read from the account's
    /// published registry entry, never held by the principal.
    FollowWrite,
    /// `fauna:admin:read:<page>` — read one admin page as the server's admin
    /// (`authorization-server.md` § Scope grammar → *The admin arms*). The
    /// qualifier is REQUIRED and is the server's: one built [`AdminPage`].
    /// No grant twin (the admin plane is nest-level state in the clear), any
    /// execution form. The string carries the admin's consent; the account's
    /// admin authority is a fourth predicate the nest reads per RPC
    /// (`apps/bridges.md` § Capability-allowlist enforcement → rule (e)) —
    /// a member's principal holding the string is served nothing, and a
    /// demoted admin's grant serves nothing until the role returns.
    AdminRead,
    /// `fauna:admin:rw:<page>` — read and administer one admin page as the
    /// server's admin: its reads and its non-sovereign writes, a superset
    /// string of its own as `records:rw` is. The qualifier is REQUIRED: one
    /// built [`AdminPage`] that [has writes](AdminPage::has_writes). The rest
    /// is [`FaunaScopeArm::AdminRead`]'s — no grant twin, any form, the
    /// nest's door, the account's admin authority read per RPC.
    AdminRw,
    /// `fauna:profile:read` — no qualifier, no grant twin, any execution
    /// form. The one kind `fauna.profile.get`, for any actor this nest holds
    /// a profile of: the public signed record, served verbatim. A profile
    /// carries no viewer state, so no `account` refinement exists; the write
    /// is the oracle's class `profile.author`, never an arm.
    ProfileRead,
    /// `fauna:notifications:read` — no qualifier, no grant twin, any
    /// execution form. `fauna.notifications.list` and `.count` over the
    /// account's inbox, read as its own apps read it: nest-side rows in the
    /// clear, nothing of a sealed plane.
    NotificationsRead,
    /// `fauna:notifications:write` — no qualifier, no grant twin, any
    /// execution form. Exactly `fauna.notifications.mark_read`: dismissal and
    /// clearing are destruction, the owner's affordance, and stay `User`.
    NotificationsWrite,
}

impl FaunaScopeArm {
    /// Every built arm, in the order discovery advertises them.
    pub const ALL: &'static [FaunaScopeArm] = &[
        FaunaScopeArm::FeedRead,
        FaunaScopeArm::FeedReadAccount,
        FaunaScopeArm::FeedReadAuthor,
        FaunaScopeArm::IdentityOp,
        FaunaScopeArm::Records,
        FaunaScopeArm::ConversationsBridge,
        FaunaScopeArm::FolderDeposit,
        FaunaScopeArm::FolderRead,
        FaunaScopeArm::PersonalizationRw,
        FaunaScopeArm::EventsSubscribe,
        FaunaScopeArm::FollowWrite,
        FaunaScopeArm::AdminRead,
        FaunaScopeArm::AdminRw,
        FaunaScopeArm::ProfileRead,
        FaunaScopeArm::NotificationsRead,
        FaunaScopeArm::NotificationsWrite,
    ];

    /// The arm's plane and verb.
    #[must_use]
    pub const fn plane_verb(self) -> (&'static str, &'static str) {
        match self {
            FaunaScopeArm::FeedRead
            | FaunaScopeArm::FeedReadAccount
            | FaunaScopeArm::FeedReadAuthor => ("feed", "read"),
            FaunaScopeArm::IdentityOp => ("identity", "op"),
            FaunaScopeArm::Records => ("records", "rw"),
            FaunaScopeArm::ConversationsBridge => ("conversations", "bridge"),
            FaunaScopeArm::FolderDeposit => ("folder", "deposit"),
            FaunaScopeArm::FolderRead => ("folder", "read"),
            FaunaScopeArm::PersonalizationRw => ("personalization", "rw"),
            FaunaScopeArm::EventsSubscribe => ("events", "subscribe"),
            FaunaScopeArm::FollowWrite => ("follow", "write"),
            FaunaScopeArm::AdminRead => ("admin", "read"),
            FaunaScopeArm::AdminRw => ("admin", "rw"),
            FaunaScopeArm::ProfileRead => ("profile", "read"),
            FaunaScopeArm::NotificationsRead => ("notifications", "read"),
            FaunaScopeArm::NotificationsWrite => ("notifications", "write"),
        }
    }

    /// Where the arm's token is honoured — exactly one door per arm. Every
    /// arm but `FolderRead` is the nest's; `FolderRead` is the MDA's WebDAV
    /// server, admitted through [`WEBDAV_ADMIT_PRINCIPAL_KIND`].
    #[must_use]
    pub const fn door(self) -> Door {
        match self {
            FaunaScopeArm::FolderRead => Door::Mda {
                relay_kind: WEBDAV_ADMIT_PRINCIPAL_KIND,
            },
            FaunaScopeArm::FeedRead
            | FaunaScopeArm::FeedReadAccount
            | FaunaScopeArm::FeedReadAuthor
            | FaunaScopeArm::IdentityOp
            | FaunaScopeArm::Records
            | FaunaScopeArm::ConversationsBridge
            | FaunaScopeArm::FolderDeposit
            | FaunaScopeArm::PersonalizationRw
            | FaunaScopeArm::EventsSubscribe
            | FaunaScopeArm::FollowWrite
            | FaunaScopeArm::AdminRead
            | FaunaScopeArm::AdminRw
            | FaunaScopeArm::ProfileRead
            | FaunaScopeArm::NotificationsRead
            | FaunaScopeArm::NotificationsWrite => Door::Nest,
        }
    }

    /// Whether PAR refuses the arm to a remote-form principal — a
    /// confidential client, `remote` by rule 3's derivation
    /// ([`crate::oauth_client::ResolvedClient::is_remote_form`]). TP4 grants a
    /// remote principal keys only to a scope it created: the read arm because
    /// no door lets a principal create a folder (`authorization-server.md`
    /// § Scope grammar → *The read arm*), the personalization arm because TP4
    /// reserves personalization models by name for device apps and
    /// nest-hosted code (*The fifth arm*).
    #[must_use]
    pub const fn refused_to_remote_form(self) -> bool {
        matches!(
            self,
            FaunaScopeArm::FolderRead | FaunaScopeArm::PersonalizationRw
        )
    }

    /// Whether the arm takes a qualifier. A string carrying one where the arm
    /// takes none — or none where it requires one — matches no arm.
    #[must_use]
    pub const fn takes_qualifier(self) -> bool {
        match self {
            FaunaScopeArm::FeedRead
            | FaunaScopeArm::ConversationsBridge
            | FaunaScopeArm::PersonalizationRw
            | FaunaScopeArm::EventsSubscribe
            | FaunaScopeArm::FollowWrite
            | FaunaScopeArm::ProfileRead
            | FaunaScopeArm::NotificationsRead
            | FaunaScopeArm::NotificationsWrite => false,
            FaunaScopeArm::FeedReadAccount
            | FaunaScopeArm::FeedReadAuthor
            | FaunaScopeArm::IdentityOp
            | FaunaScopeArm::Records
            | FaunaScopeArm::FolderDeposit
            | FaunaScopeArm::FolderRead
            | FaunaScopeArm::AdminRead
            | FaunaScopeArm::AdminRw => true,
        }
    }

    /// Whether the arm accepts `qualifier` — the arm's own value check on top
    /// of the family's charset. `FeedRead` takes none; `IdentityOp` takes
    /// exactly the name of a built operation class, so a sovereign
    /// operation's name is refused here, at the grammar, before any consent
    /// card renders.
    #[must_use]
    pub fn accepts_qualifier(self, qualifier: Option<&str>) -> bool {
        match (self, qualifier) {
            (
                FaunaScopeArm::FeedRead
                | FaunaScopeArm::ConversationsBridge
                | FaunaScopeArm::PersonalizationRw
                | FaunaScopeArm::EventsSubscribe
                | FaunaScopeArm::FollowWrite
                | FaunaScopeArm::ProfileRead
                | FaunaScopeArm::NotificationsRead
                | FaunaScopeArm::NotificationsWrite,
                q,
            ) => q.is_none(),
            (FaunaScopeArm::FeedReadAccount, q) => q == Some(FEED_ACCOUNT_QUALIFIER),
            (FaunaScopeArm::FeedReadAuthor, q) => q == Some(FEED_AUTHOR_QUALIFIER),
            (FaunaScopeArm::IdentityOp, None) => false,
            (FaunaScopeArm::IdentityOp, Some(q)) => {
                fauna_core::identity_op::IdentityOpClass::parse(q).is_ok()
            }
            (FaunaScopeArm::Records, None) => false,
            (FaunaScopeArm::Records, Some(q)) => {
                q.parse::<fauna_core::ext_kind::ExtQualifier>().is_ok()
            }
            (FaunaScopeArm::FolderDeposit | FaunaScopeArm::FolderRead, None) => false,
            (FaunaScopeArm::FolderDeposit | FaunaScopeArm::FolderRead, Some(q)) => {
                parse_folder_id(q).is_some()
            }
            (FaunaScopeArm::AdminRead, None) => false,
            (FaunaScopeArm::AdminRead, Some(q)) => {
                AdminPage::parse(q).is_some_and(AdminPage::has_reads)
            }
            (FaunaScopeArm::AdminRw, None) => false,
            (FaunaScopeArm::AdminRw, Some(q)) => {
                AdminPage::parse(q).is_some_and(AdminPage::has_writes)
            }
        }
    }

    /// Whether the arm's qualifier is the **user's** — a row of the account's,
    /// different per user and per nest, which no static client document can
    /// declare (`authorization-server.md` § Scope grammar → *The folder
    /// plane's qualifier is the user's*). The folder plane's two arms; every
    /// other qualifier is the client's (`records`) or the server's
    /// (`identity:op`, `feed:read:account`, `feed:read:author`).
    /// Such an arm is declared and may be requested **bare**
    /// ([`user_qualified_bare_arm`]); the user's choice at consent qualifies
    /// it ([`qualify_bare`]).
    #[must_use]
    pub const fn qualifier_is_users(self) -> bool {
        matches!(
            self,
            FaunaScopeArm::FolderDeposit | FaunaScopeArm::FolderRead
        )
    }

    /// Whether a grant under this arm also wraps keys to the principal (the
    /// arm's *grant twin*, `encryption-at-rest.md` § Capability tiering →
    /// *Third-party holders*). The card row says so either way.
    ///
    /// `IdentityOp`'s twin is **keyless**: the user's device mints an
    /// `identity.op` grant naming the class, which wraps nothing — it is the
    /// client-authoritative audit and revocation record the custodian
    /// re-resolves at every operation. `Records`' twin is per covered kind a
    /// `content.read` tuple wrapping the kind's delegable pair and a keyless
    /// `content.write` tuple naming the principal's writer key.
    /// `FolderDeposit`'s twin is keyless too: one `deposit` tuple naming the
    /// folder, the audit + revocation record the deposit door re-resolves.
    /// `FolderRead`'s twin is keyed: the `content.read{folder, set}` tuple
    /// with one wrap per content-key generation. `PersonalizationRw`'s twin
    /// is keyed too: two `content.read` tuples, each a kind's delegable pair.
    #[must_use]
    pub const fn has_grant_twin(self) -> bool {
        match self {
            FaunaScopeArm::FeedRead
            | FaunaScopeArm::FeedReadAccount
            | FaunaScopeArm::FeedReadAuthor
            | FaunaScopeArm::ConversationsBridge
            | FaunaScopeArm::EventsSubscribe
            | FaunaScopeArm::FollowWrite
            | FaunaScopeArm::AdminRead
            | FaunaScopeArm::AdminRw
            | FaunaScopeArm::ProfileRead
            | FaunaScopeArm::NotificationsRead
            | FaunaScopeArm::NotificationsWrite => false,
            FaunaScopeArm::IdentityOp
            | FaunaScopeArm::Records
            | FaunaScopeArm::FolderDeposit
            | FaunaScopeArm::FolderRead
            | FaunaScopeArm::PersonalizationRw => true,
        }
    }

    /// The scope strings discovery advertises for the arm: the arm itself for
    /// one that takes no qualifier, one string per accepted qualifier for an
    /// arm whose qualifiers are a closed set, and the bare plane and verb for
    /// `Records`, whose qualifier is the client's own kind.
    #[must_use]
    pub const fn advertised(self) -> &'static [&'static str] {
        match self {
            FaunaScopeArm::FeedRead => &[SCOPE_FEED_READ],
            FaunaScopeArm::FeedReadAccount => &[SCOPE_FEED_READ_ACCOUNT],
            FaunaScopeArm::FeedReadAuthor => &[SCOPE_FEED_READ_AUTHOR],
            FaunaScopeArm::IdentityOp => &[
                SCOPE_IDENTITY_OP_NOSTR_SIGN_EVENT,
                SCOPE_IDENTITY_OP_NOSTR_NIP44,
                SCOPE_IDENTITY_OP_ACTIVITYPUB_HTTP_SIG,
                SCOPE_IDENTITY_OP_POST_AUTHOR,
                SCOPE_IDENTITY_OP_PROFILE_AUTHOR,
                SCOPE_IDENTITY_OP_ATPROTO_SERVICE_AUTH,
                SCOPE_IDENTITY_OP_CONVERSATION_READ,
                SCOPE_IDENTITY_OP_CONVERSATION_SEND,
            ],
            FaunaScopeArm::Records => &[SCOPE_RECORDS_RW],
            FaunaScopeArm::ConversationsBridge => &[SCOPE_CONVERSATIONS_BRIDGE],
            FaunaScopeArm::FolderDeposit => &[SCOPE_FOLDER_DEPOSIT],
            FaunaScopeArm::FolderRead => &[SCOPE_FOLDER_READ],
            FaunaScopeArm::PersonalizationRw => &[SCOPE_PERSONALIZATION_RW],
            FaunaScopeArm::EventsSubscribe => &[SCOPE_EVENTS_SUBSCRIBE],
            FaunaScopeArm::FollowWrite => &[SCOPE_FOLLOW_WRITE],
            FaunaScopeArm::AdminRead => &[
                SCOPE_ADMIN_READ_DASHBOARD,
                SCOPE_ADMIN_READ_USERS,
                SCOPE_ADMIN_READ_SETTINGS,
                SCOPE_ADMIN_READ_NEST,
                SCOPE_ADMIN_READ_LOGS,
                SCOPE_ADMIN_READ_WEB,
                SCOPE_ADMIN_READ_DNS,
                SCOPE_ADMIN_READ_MAIL,
                SCOPE_ADMIN_READ_MAIL_REPORTS,
                SCOPE_ADMIN_READ_ALIASES,
                SCOPE_ADMIN_READ_CALENDAR,
                SCOPE_ADMIN_READ_BRIDGES_PENDING,
            ],
            FaunaScopeArm::AdminRw => &[
                SCOPE_ADMIN_RW_USERS,
                SCOPE_ADMIN_RW_SETTINGS,
                SCOPE_ADMIN_RW_NEST,
                SCOPE_ADMIN_RW_WEB,
                SCOPE_ADMIN_RW_DNS,
                SCOPE_ADMIN_RW_MAIL,
                SCOPE_ADMIN_RW_ALIASES,
                SCOPE_ADMIN_RW_CALENDAR,
                SCOPE_ADMIN_RW_CONTACTS,
                SCOPE_ADMIN_RW_FILES,
            ],
            FaunaScopeArm::ProfileRead => &[SCOPE_PROFILE_READ],
            FaunaScopeArm::NotificationsRead => &[SCOPE_NOTIFICATIONS_READ],
            FaunaScopeArm::NotificationsWrite => &[SCOPE_NOTIFICATIONS_WRITE],
        }
    }

    /// Grantable strings exemplifying the arm — what a test sweeps when it
    /// needs every arm to match, render and grant. The advertised strings
    /// wherever they are grantable as written; one qualified string for
    /// `Records`, whose advertised form is not.
    #[must_use]
    pub const fn examples(self) -> &'static [&'static str] {
        match self {
            FaunaScopeArm::Records => &[
                "fauna:records:rw:ext.app.example.*",
                "fauna:records:rw:ext.app.example.notes",
            ],
            FaunaScopeArm::FolderDeposit => &["fauna:folder:deposit:42"],
            FaunaScopeArm::FolderRead => &["fauna:folder:read:42"],
            other => other.advertised(),
        }
    }
}

/// The publisher a `records` scope's qualifier names — `None` for any other
/// string. PAR holds a wildcard's publisher to the requesting document's host
/// (`authorization-server.md` § Scope grammar → *Wildcards*); a single kind of
/// another publisher is legal at PAR and is the consent's to admit.
#[must_use]
pub fn records_qualifier(scope: &str) -> Option<fauna_core::ext_kind::ExtQualifier> {
    if arm_of(scope)? != FaunaScopeArm::Records {
        return None;
    }
    parse(scope)?.qualifier?.parse().ok()
}

/// The account plane's delegable scope — the one first-party scope the fifth
/// arm, [`FaunaScopeArm::PersonalizationRw`], lets a session list. The same
/// spelling as the plane's `ACCOUNT_STATE_SCOPE` (`fauna-protocol`, which this
/// crate does not depend on; the nest's events tests pin the two equal).
pub const PERSONALIZATION_LIST_SCOPE: &str = "state";

/// Does a change nudge for `scope_tag` reach a session holding `scopes`? The
/// one filter both events doors apply (`transport.md` § Push events →
/// *Third-party event doors*): the session holds `fauna:events:subscribe`
/// **and** the scope is one its other arms let it list — an `ext:<kind>`
/// scope whose kind a `records` qualifier covers, structurally (the record
/// door's own reach check), or [`PERSONALIZATION_LIST_SCOPE`] when it holds
/// `fauna:personalization:rw`. Every other tag — `state-fleet`, a folder
/// nudge, another publisher's kind — reaches nothing.
#[must_use]
pub fn event_reaches(scopes: &[String], scope_tag: &str) -> bool {
    if !scopes
        .iter()
        .any(|s| arm_of(s) == Some(FaunaScopeArm::EventsSubscribe))
    {
        return false;
    }
    if scope_tag == PERSONALIZATION_LIST_SCOPE {
        return scopes
            .iter()
            .any(|s| arm_of(s) == Some(FaunaScopeArm::PersonalizationRw));
    }
    let Some(kind) = scope_tag.strip_prefix(EXT_SCOPE_PREFIX).and_then(|k| {
        // Strict, never repairing: only the canonical spelling is a scope.
        let kind = k.parse::<fauna_core::ext_kind::ExtKind>().ok()?;
        (kind.to_string() == k).then_some(kind)
    }) else {
        return false;
    };
    scopes
        .iter()
        .any(|s| records_qualifier(s).is_some_and(|q| q.covers(&kind)))
}

/// Does a session holding `scopes` read the feed plane as the account — does
/// it hold [`FaunaScopeArm::FeedReadAccount`]? The refining arm's one
/// question (`apps/bridges.md` rule (c)'s refinement): the two public
/// timelines stay the bare arm's kinds, and their handlers read the account's
/// view off the session's scopes with this.
#[must_use]
pub fn reads_feed_as_account<S: AsRef<str>>(scopes: &[S]) -> bool {
    scopes
        .iter()
        .any(|s| arm_of(s.as_ref()) == Some(FaunaScopeArm::FeedReadAccount))
}

/// The `ext` scope family's tag (`third-party-kinds.md` § The `ext`
/// sub-scope): `ext:<kind>`, the kind verbatim after it.
const EXT_SCOPE_PREFIX: &str = "ext:";

/// A folder row id in canonical decimal — positive, no sign, no leading
/// zero — so one folder has exactly one scope string.
fn parse_folder_id(q: &str) -> Option<i64> {
    let id = q.parse::<i64>().ok().filter(|id| *id > 0)?;
    (id.to_string() == q).then_some(id)
}

/// The folder a `folder_deposit` scope names — `None` for any other string.
/// What the deposit door matches the request's folder against: the arm-wide
/// gate admits the kind, this is the per-folder half.
#[must_use]
pub fn folder_deposit_qualifier(scope: &str) -> Option<i64> {
    if arm_of(scope)? != FaunaScopeArm::FolderDeposit {
        return None;
    }
    parse_folder_id(parse(scope)?.qualifier?)
}

/// The folder a `folder_read` scope names — `None` for any other string.
/// What the MDA's admission matches a request's folder against.
#[must_use]
pub fn folder_read_qualifier(scope: &str) -> Option<i64> {
    if arm_of(scope)? != FaunaScopeArm::FolderRead {
        return None;
    }
    parse_folder_id(parse(scope)?.qualifier?)
}

/// The admin page a `fauna:admin:read:<page>` string names; `None` for any
/// other string, a bare or unbuilt-page one and a page with no reads
/// included.
#[must_use]
pub fn admin_read_qualifier(scope: &str) -> Option<AdminPage> {
    if arm_of(scope)? != FaunaScopeArm::AdminRead {
        return None;
    }
    AdminPage::parse(parse(scope)?.qualifier?)
}

/// The admin page a `fauna:admin:rw:<page>` string names; `None` for any
/// other string, a bare one, an unbuilt page and a page of reads only
/// included.
#[must_use]
pub fn admin_rw_qualifier(scope: &str) -> Option<AdminPage> {
    if arm_of(scope)? != FaunaScopeArm::AdminRw {
        return None;
    }
    AdminPage::parse(parse(scope)?.qualifier?)
}

/// The operation class a `fauna:identity:op:<class>` scope names — `None`
/// for any other string, a sovereign or unknown name included (the arm
/// accepts only a built class). What the approving app's consent mint reads
/// to declare the keyless `identity.op` tuple.
#[must_use]
pub fn identity_op_qualifier(scope: &str) -> Option<fauna_core::identity_op::IdentityOpClass> {
    if arm_of(scope)? != FaunaScopeArm::IdentityOp {
        return None;
    }
    fauna_core::identity_op::IdentityOpClass::parse(parse(scope)?.qualifier?).ok()
}

/// The `fauna:identity:op:<class>` scope naming `class` — the inverse of
/// [`identity_op_qualifier`]. What a surface describing one operation the
/// oracle ran under `class` words through the describe path, so the record
/// reads as the consent card did. Exhaustive: a new class cannot be built
/// without its scope.
#[must_use]
pub const fn identity_op_scope(class: fauna_core::identity_op::IdentityOpClass) -> &'static str {
    use fauna_core::identity_op::IdentityOpClass;
    match class {
        IdentityOpClass::NostrSignEvent => SCOPE_IDENTITY_OP_NOSTR_SIGN_EVENT,
        IdentityOpClass::NostrNip44 => SCOPE_IDENTITY_OP_NOSTR_NIP44,
        IdentityOpClass::ActivityPubHttpSig => SCOPE_IDENTITY_OP_ACTIVITYPUB_HTTP_SIG,
        IdentityOpClass::PostAuthor => SCOPE_IDENTITY_OP_POST_AUTHOR,
        IdentityOpClass::ProfileAuthor => SCOPE_IDENTITY_OP_PROFILE_AUTHOR,
        IdentityOpClass::AtprotoServiceAuth => SCOPE_IDENTITY_OP_ATPROTO_SERVICE_AUTH,
        IdentityOpClass::ConversationRead => SCOPE_IDENTITY_OP_CONVERSATION_READ,
        IdentityOpClass::ConversationSend => SCOPE_IDENTITY_OP_CONVERSATION_SEND,
    }
}

/// Whether PAR must refuse `scope` to a remote-form (confidential) client:
/// a string of an arm [`FaunaScopeArm::refused_to_remote_form`] names —
/// qualified, or the bare plane and verb a card-qualified request carries
/// (`authorization-server.md` § Scope grammar → *The folder plane's
/// qualifier is the user's*: the form gate fires on a bare request as on the
/// qualified string) — or a `fauna:identity:op:<class>` string whose class a
/// device custodian performs. That gate is per class, not per arm: the second
/// arm stays admitted to the remote form for the nest's and the bridges'
/// classes, while a remote process has no device door to reach and TP4
/// reserves conversations for device apps and nest-hosted code
/// (`conversations-oracle.md` § The two classes).
#[must_use]
pub fn refused_to_remote_form(scope: &str) -> bool {
    FaunaScopeArm::ALL.iter().any(|arm| {
        arm.refused_to_remote_form()
            && (arm_of(scope) == Some(*arm) || arm.advertised().contains(&scope))
    }) || identity_op_qualifier(scope).is_some_and(|class| class.custodian().is_device())
}

/// The user-qualified arm `scope` names **bare** — its plane and verb with no
/// qualifier, the string the client document declares and discovery
/// advertises. `None` for any other string, a qualified one included. No arm
/// grants the bare string as written ([`arm_of`] answers `None`): it is a
/// *card-qualified request*, admitted at PAR and qualified by the user's
/// choice at the ceremony (`authorization-server.md` § Scope grammar → *The
/// folder plane's qualifier is the user's*).
#[must_use]
pub fn user_qualified_bare_arm(scope: &str) -> Option<FaunaScopeArm> {
    let parsed = parse(scope)?;
    if parsed.qualifier.is_some() {
        return None;
    }
    FaunaScopeArm::ALL
        .iter()
        .copied()
        .find(|arm| arm.qualifier_is_users() && arm.plane_verb() == (parsed.plane, parsed.verb))
}

/// The bare form of a user-qualified arm's string — `scope` itself when it is
/// bare, its plane and verb when it is qualified. What PAR holds such a
/// request to: the document declares the bare string and the request narrows
/// it. `None` for every other string, so every other arm keeps exact
/// membership.
#[must_use]
pub fn bare_form(scope: &str) -> Option<&str> {
    if user_qualified_bare_arm(scope).is_some() {
        return Some(scope);
    }
    if !arm_of(scope)?.qualifier_is_users() {
        return None;
    }
    let parsed = parse(scope)?;
    let len = FAUNA_SCOPE_PREFIX.len() + parsed.plane.len() + 1 + parsed.verb.len();
    scope.get(..len)
}

/// The arm's string for a bare user-qualified `scope` qualified by the folder
/// the user chose — what the nest records at the ceremony in place of the
/// bare string. `None` when `scope` is not bare user-qualified or `folder_id`
/// is not a qualifier the arm accepts.
#[must_use]
pub fn qualify_bare(scope: &str, folder_id: i64) -> Option<String> {
    let arm = user_qualified_bare_arm(scope)?;
    let qualified = format!("{scope}:{folder_id}");
    (arm_of(&qualified) == Some(arm)).then_some(qualified)
}

/// A consent row's scope set with every bare user-qualified scope qualified by
/// the one folder the card chose — what the nest records at the ceremony
/// (`authorization-server.md` § Scope grammar → *The folder plane's
/// qualifier is the user's*: one choice qualifies every bare folder verb on
/// the card). Every other scope passes untouched, in order. `None` when the
/// set carries no bare user-qualified scope (a choice is refused there) or
/// `folder_id` is not a qualifier its arm accepts.
#[must_use]
pub fn qualify_bare_scopes<S: AsRef<str>>(scopes: &[S], folder_id: i64) -> Option<Vec<String>> {
    let mut qualified_any = false;
    let mut out = Vec::with_capacity(scopes.len());
    for scope in scopes {
        let scope = scope.as_ref();
        if user_qualified_bare_arm(scope).is_some() {
            out.push(qualify_bare(scope, folder_id)?);
            qualified_any = true;
        } else {
            out.push(scope.to_string());
        }
    }
    qualified_any.then_some(out)
}

/// The consent card's row for `scope`: what the token permits, and — in
/// words — what keys are or are not shared. `None` for a string matching no
/// arm; an arm taking a qualifier renders the row its qualifier names, and a
/// bare user-qualified string renders its arm's row (the card's picker names
/// the folder).
#[must_use]
pub fn card_row(scope: &str) -> Option<&'static str> {
    match arm_of(scope).or_else(|| user_qualified_bare_arm(scope))? {
        FaunaScopeArm::FeedRead => {
            Some("Read this server's public timelines (public posts only; no keys are shared)")
        }
        FaunaScopeArm::FeedReadAccount => Some(
            "Read your feeds and which posts you liked and reposted (public posts only; no keys \
             are shared)",
        ),
        FaunaScopeArm::FeedReadAuthor => Some(
            "Read any one author's public posts on this server (public posts only; no keys are \
             shared)",
        ),
        FaunaScopeArm::IdentityOp => {
            fauna_core::identity_op::IdentityOpClass::parse(parse(scope)?.qualifier?)
                .ok()
                .map(fauna_core::identity_op::IdentityOpClass::card_row)
        }
        FaunaScopeArm::Records => Some(
            "Read and write this app's own records in your account (keys to exactly those \
             records are shared with it)",
        ),
        FaunaScopeArm::ConversationsBridge => Some(
            "Carry your conversations on another network to and from this account (this app \
             reads the messages it carries; no keys of yours are shared with it)",
        ),
        FaunaScopeArm::FolderDeposit => Some(
            "Put files into one of your folders, sealed to you on arrival (it cannot see the \
             folder or anything in it; no keys are shared)",
        ),
        FaunaScopeArm::FolderRead => Some(
            "Read the files in one of your folders, everything already in it included (keys to \
             exactly that folder are shared with it)",
        ),
        FaunaScopeArm::PersonalizationRw => Some(
            "Read and train your personalization — trained topics and viewing cues (keys to \
             exactly those models are shared with this app; nothing else)",
        ),
        FaunaScopeArm::EventsSubscribe => Some(
            "Be told when this app's own records in your account change (it learns only that \
             something changed, never what; no keys are shared)",
        ),
        FaunaScopeArm::FollowWrite => {
            Some("Follow and unfollow accounts as you (no keys are shared)")
        }
        FaunaScopeArm::AdminRead => AdminPage::parse(parse(scope)?.qualifier?)?.read_card_row(),
        FaunaScopeArm::AdminRw => AdminPage::parse(parse(scope)?.qualifier?)?.rw_card_row(),
        FaunaScopeArm::ProfileRead => Some(
            "Read your profile and the public profile of anyone this server knows (no keys are \
             shared)",
        ),
        FaunaScopeArm::NotificationsRead => Some(
            "Read your notifications — who liked, replied to, followed or messaged you, and your \
             security notices — and your unread count (no keys are shared)",
        ),
        FaunaScopeArm::NotificationsWrite => Some(
            "Mark your notifications as read on every device (never dismisses or clears them; no \
             keys are shared)",
        ),
    }
}

/// The consent card's row for a `folder_read` scope, naming the folder when
/// the approving app can resolve its id (`authorization-server.md` § Scope
/// grammar → *The read arm*). `folder_name` is the owner's own name for the
/// folder — `None` when the app cannot resolve it, and the row then reads as
/// [`card_row`]'s. `None` for any scope that is not a `folder_read` scope.
#[must_use]
pub fn folder_read_card_row(scope: &str, folder_name: Option<&str>) -> Option<String> {
    folder_read_qualifier(scope)?;
    Some(match folder_name {
        Some(name) => format!(
            "Read the files in your folder \"{name}\", everything already in it included (keys \
             to exactly that folder are shared with it)"
        ),
        None => card_row(scope)?.to_string(),
    })
}

/// The consent card's row for a `records` scope, worded from what the
/// approving device knows about the request (`authorization-server.md`
/// § Scope grammar → *The third arm*: the publisher, how many kinds, that keys
/// to exactly those records are shared — and, for an app that attested no
/// writer key, that it can only read). `kinds` is how many kinds the scope
/// covers once expanded against the app's verified manifest (`None` when it is
/// not known: a wildcard whose manifest did not verify). `None` for any scope
/// that is not a `records` scope — [`card_row`] words it.
#[must_use]
pub fn records_card_row(scope: &str, kinds: Option<usize>, writable: bool) -> Option<String> {
    use fauna_core::ext_kind::ExtQualifier;
    let what = match records_qualifier(scope)? {
        ExtQualifier::Kind(kind) => format!(
            "records of the kind \"{}\" published by {}",
            kind.name(),
            kind.publisher()
        ),
        ExtQualifier::Publisher(publisher) => match kinds {
            Some(1) => format!("1 kind of records published by {publisher}"),
            Some(n) => format!("{n} kinds of records published by {publisher}"),
            None => format!("the records published by {publisher}"),
        },
    };
    Some(if writable {
        format!(
            "Read and write {what} in your account (keys to exactly those records are shared \
             with it)"
        )
    } else {
        format!(
            "Read {what} in your account — read only: it cannot write them (keys to exactly \
             those records are shared with it)"
        )
    })
}

/// A scope string split into the family's three positions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ParsedFaunaScope<'a> {
    pub plane: &'a str,
    pub verb: &'a str,
    pub qualifier: Option<&'a str>,
}

/// Is `s` a non-empty run of lowercase ASCII letters — a plane or a verb?
fn is_word(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_lowercase())
}

/// Is `s` a well-formed qualifier for `plane`: `a–z 0–9 . _ -`, plus — for
/// `records` alone — one trailing `.*`.
fn is_qualifier(plane: &str, s: &str) -> bool {
    let body = match s.strip_suffix(".*") {
        Some(body) if plane == "records" => body,
        _ => s,
    };
    !body.is_empty()
        && body.bytes().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'-')
        })
}

/// Parse a scope string under the family's grammar. `None` for a string
/// outside the family or one that breaks its charset — never a guess.
#[must_use]
pub fn parse(scope: &str) -> Option<ParsedFaunaScope<'_>> {
    let rest = scope.strip_prefix(FAUNA_SCOPE_PREFIX)?;
    let mut parts = rest.splitn(3, ':');
    let plane = parts.next()?;
    let verb = parts.next()?;
    let qualifier = parts.next();
    if !is_word(plane) || !is_word(verb) {
        return None;
    }
    if let Some(q) = qualifier
        && !is_qualifier(plane, q)
    {
        return None;
    }
    Some(ParsedFaunaScope {
        plane,
        verb,
        qualifier,
    })
}

/// The built arm `scope` matches, if any — the family's whole grantability
/// question. Whole-string: a qualifier where the arm takes none, none where
/// it requires one, or one the arm does not accept matches nothing.
#[must_use]
pub fn arm_of(scope: &str) -> Option<FaunaScopeArm> {
    let parsed = parse(scope)?;
    FaunaScopeArm::ALL.iter().copied().find(|arm| {
        arm.plane_verb() == (parsed.plane, parsed.verb) && arm.accepts_qualifier(parsed.qualifier)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_parse_table() {
        let p = parse("fauna:feed:read").expect("parses");
        assert_eq!((p.plane, p.verb, p.qualifier), ("feed", "read", None));
        let p = parse("fauna:folder:read:abc-123_x.y").expect("parses");
        assert_eq!(p.qualifier, Some("abc-123_x.y"));
        let p = parse("fauna:records:rw:ext.dating.example.*").expect("records takes .*");
        assert_eq!(p.qualifier, Some("ext.dating.example.*"));

        for bad in [
            "fauna:",
            "fauna:feed",
            "fauna:feed:",
            "fauna::read",
            "fauna:Feed:read",
            "fauna:feed:READ",
            "fauna:feed1:read",
            "fauna:feed:read:",
            "fauna:feed:read:Abc",
            "fauna:feed:read:a:b",
            "fauna:feed:read:a b",
            "fauna:folder:read:abc.*",
            "fauna:records:rw:.*",
            "fauna:records:rw:a.*.*",
            "fauna:records:rw:a*",
            "faunax:feed:read",
            "feed:read",
        ] {
            assert!(parse(bad).is_none(), "{bad:?} must not parse");
        }
    }

    #[test]
    fn the_first_arm_matches_exactly_its_own_string() {
        assert_eq!(arm_of(SCOPE_FEED_READ), Some(FaunaScopeArm::FeedRead));
        for near in [
            "fauna:feed:read:home",
            "fauna:feed:readx",
            "fauna:feed:write",
            "fauna:feeds:read",
            "fauna:feed:read ",
        ] {
            assert_eq!(arm_of(near), None, "{near:?} is not the arm");
        }
    }

    /// The fourth arm (`authorization-server.md` § Scope grammar → *The
    /// fourth arm*): the qualifier is required and is the fixed word
    /// `account` — no other word, no bare string — with no grant twin and a
    /// card row of its own; the bare string stays the bare arm's.
    #[test]
    fn the_fourth_arm_takes_exactly_the_word_account() {
        assert_eq!(
            arm_of(SCOPE_FEED_READ_ACCOUNT),
            Some(FaunaScopeArm::FeedReadAccount)
        );
        assert_eq!(arm_of(SCOPE_FEED_READ), Some(FaunaScopeArm::FeedRead));
        for near in [
            "fauna:feed:read:accounts",
            "fauna:feed:read:home",
            "fauna:feed:read:account.x",
            "fauna:feed:write:account",
        ] {
            assert_eq!(arm_of(near), None, "{near:?} is not the arm");
        }
        let arm = FaunaScopeArm::FeedReadAccount;
        assert!(arm.takes_qualifier());
        assert!(!arm.has_grant_twin());
        assert!(!arm.qualifier_is_users());
        assert!(!arm.refused_to_remote_form());
        assert_eq!(arm.door(), Door::Nest);
        assert_eq!(arm.advertised(), &[SCOPE_FEED_READ_ACCOUNT]);
        assert_eq!(user_qualified_bare_arm(SCOPE_FEED_READ), None);
        assert_ne!(card_row(SCOPE_FEED_READ_ACCOUNT), card_row(SCOPE_FEED_READ));
        assert!(
            card_row(SCOPE_FEED_READ_ACCOUNT)
                .is_some_and(|r| r.contains("public posts only") && r.contains("no keys"))
        );

        assert!(reads_feed_as_account(&[
            SCOPE_FEED_READ,
            SCOPE_FEED_READ_ACCOUNT
        ]));
        assert!(!reads_feed_as_account(&[SCOPE_FEED_READ]));
        assert!(!reads_feed_as_account(&["fauna:feed:read:home"]));
    }

    /// The author arm (`authorization-server.md` § Scope grammar → *The
    /// author arm*): the qualifier is required and is the fixed word
    /// `author` — the server's, as `account` is — with no grant twin, any
    /// execution form and a card row of its own; holding it never reads as
    /// the account.
    #[test]
    fn the_author_arm_takes_exactly_the_word_author() {
        assert_eq!(
            arm_of(SCOPE_FEED_READ_AUTHOR),
            Some(FaunaScopeArm::FeedReadAuthor)
        );
        for near in [
            "fauna:feed:read:authorx",
            "fauna:feed:read:Author",
            "fauna:feed:read:authors",
            "fauna:feed:read:author.x",
            "fauna:feed:write:author",
        ] {
            assert_eq!(arm_of(near), None, "{near:?} is not the arm");
        }
        assert_eq!(arm_of(SCOPE_FEED_READ), Some(FaunaScopeArm::FeedRead));
        let arm = FaunaScopeArm::FeedReadAuthor;
        assert!(arm.takes_qualifier());
        assert!(!arm.accepts_qualifier(None));
        assert!(!arm.has_grant_twin());
        assert!(!arm.qualifier_is_users());
        assert!(!arm.refused_to_remote_form());
        assert_eq!(arm.door(), Door::Nest);
        assert_eq!(arm.plane_verb(), ("feed", "read"));
        assert_eq!(arm.advertised(), &[SCOPE_FEED_READ_AUTHOR]);
        let row = card_row(SCOPE_FEED_READ_AUTHOR);
        assert_ne!(row, card_row(SCOPE_FEED_READ));
        assert_ne!(row, card_row(SCOPE_FEED_READ_ACCOUNT));
        assert!(row.is_some_and(|r| r.contains("public posts only") && r.contains("no keys")));

        assert!(!reads_feed_as_account(&[SCOPE_FEED_READ_AUTHOR]));
        assert!(reads_feed_as_account(&[
            SCOPE_FEED_READ_AUTHOR,
            SCOPE_FEED_READ_ACCOUNT
        ]));
    }

    /// Every plane the goal doc's table lists but no slice has built matches
    /// no arm — `invalid_scope` at PAR until its own slice adds it.
    #[test]
    fn the_listed_but_unbuilt_planes_match_no_arm() {
        for unbuilt in ["fauna:post:write", "fauna:identity:op:rotate"] {
            assert!(parse(unbuilt).is_some(), "{unbuilt:?} is well-formed");
            assert_eq!(arm_of(unbuilt), None, "{unbuilt:?} is not built");
        }
    }

    #[test]
    fn every_arm_round_trips_through_its_examples() {
        for arm in FaunaScopeArm::ALL {
            assert!(!arm.advertised().is_empty());
            assert!(!arm.examples().is_empty());
            for s in arm.examples() {
                assert_eq!(arm_of(s), Some(*arm), "{s}");
                assert!(card_row(s).is_some(), "{s} renders a card row");
            }
        }
    }

    /// The `records` arm (`third-party-kinds.md` § The record doors): its
    /// qualifier is required and is one `ext.*` kind or one publisher's
    /// wildcard, parsed by the one grammar — a dotted name, a `fauna.` kind,
    /// a bare plane-and-verb or a prefix wildcard matches no arm.
    #[test]
    fn the_records_arm_takes_exactly_one_ext_kind_or_one_publishers_wildcard() {
        use fauna_core::ext_kind::ExtQualifier;
        for good in [
            "fauna:records:rw:ext.app.example.notes",
            "fauna:records:rw:ext.app.example.*",
            "fauna:records:rw:ext.example.co.uk.notes",
        ] {
            assert_eq!(arm_of(good), Some(FaunaScopeArm::Records), "{good}");
            assert!(card_row(good).is_some());
        }
        for bad in [
            SCOPE_RECORDS_RW,
            "fauna:records:rw:notes",
            "fauna:records:rw:fauna.state.notes",
            "fauna:records:rw:ext.notes",
            "fauna:records:rw:ext.-app.example.notes",
            "fauna:records:rw:ext..notes",
            "fauna:records:rw:ext.app.example.*.*",
            "fauna:records:read:ext.app.example.notes",
        ] {
            assert_eq!(arm_of(bad), None, "{bad}");
            assert_eq!(records_qualifier(bad), None, "{bad}");
        }
        assert_eq!(
            records_qualifier("fauna:records:rw:ext.example.co.*"),
            Some(ExtQualifier::Publisher("example.co".into()))
        );
        let one = records_qualifier("fauna:records:rw:ext.example.co.uk.notes").unwrap();
        assert_eq!(one.publisher(), "example.co.uk");
        assert_eq!(records_qualifier(SCOPE_FEED_READ), None);
        assert!(FaunaScopeArm::Records.has_grant_twin());
    }

    /// The records card row names the publisher and the kind count, says keys
    /// to exactly those records are shared, and says "read only" when the app
    /// attested no writer key; a single foreign kind names its own publisher.
    #[test]
    fn the_records_card_row_names_publisher_count_and_read_only() {
        let wild = "fauna:records:rw:ext.app.example.*";
        let row = records_card_row(wild, Some(2), true).unwrap();
        assert!(row.starts_with("Read and write 2 kinds of records published by app.example"));
        assert!(row.contains("keys to exactly those records are shared"));
        assert!(!row.contains("read only"));

        let ro = records_card_row(wild, Some(1), false).unwrap();
        assert!(
            ro.contains("1 kind of records published by app.example"),
            "{ro}"
        );
        assert!(ro.contains("read only"), "{ro}");

        let unknown = records_card_row(wild, None, true).unwrap();
        assert!(
            unknown.contains("the records published by app.example"),
            "{unknown}"
        );

        let foreign =
            records_card_row("fauna:records:rw:ext.other.org.thing", Some(1), true).unwrap();
        assert!(
            foreign.contains("records of the kind \"thing\" published by other.org"),
            "{foreign}"
        );

        assert_eq!(records_card_row(SCOPE_FEED_READ, Some(1), true), None);
        assert_eq!(records_card_row("atproto", None, true), None);
    }

    /// The `folder_deposit` arm (`file-sync.md` § Third-party deposit
    /// ingress): one folder by its canonical decimal row id, nothing else.
    #[test]
    fn the_folder_deposit_arm_takes_exactly_one_canonical_folder_id() {
        for (good, id) in [
            ("fauna:folder:deposit:42", 42),
            ("fauna:folder:deposit:1", 1),
            ("fauna:folder:deposit:9223372036854775807", i64::MAX),
        ] {
            assert_eq!(arm_of(good), Some(FaunaScopeArm::FolderDeposit), "{good}");
            assert_eq!(folder_deposit_qualifier(good), Some(id), "{good}");
            assert!(card_row(good).is_some_and(|r| r.contains("no keys are shared")));
        }
        for bad in [
            SCOPE_FOLDER_DEPOSIT,
            "fauna:folder:deposit:abc",
            "fauna:folder:deposit:042",
            "fauna:folder:deposit:0",
            "fauna:folder:deposit:-4",
            "fauna:folder:deposit:9223372036854775808",
            "fauna:folder:write:42",
            "fauna:folders:deposit:42",
        ] {
            assert_eq!(arm_of(bad), None, "{bad}");
            assert_eq!(folder_deposit_qualifier(bad), None, "{bad}");
        }
        // The read arm shares the qualifier, never the reach.
        assert_eq!(folder_deposit_qualifier("fauna:folder:read:42"), None);
        assert_eq!(folder_deposit_qualifier("fauna:records:rw:ext.a.b"), None);
        assert!(FaunaScopeArm::FolderDeposit.has_grant_twin());
    }

    /// The `folder_read` arm (`webdav-server.md` § Key model → *A principal's
    /// read*): one folder by its canonical decimal row id, a keyed twin, the
    /// MDA's door, and refused to a remote-form client — bare or qualified.
    #[test]
    fn the_folder_read_arm_takes_exactly_one_canonical_folder_id() {
        for (good, id) in [
            ("fauna:folder:read:42", 42),
            ("fauna:folder:read:1", 1),
            ("fauna:folder:read:9223372036854775807", i64::MAX),
        ] {
            assert_eq!(arm_of(good), Some(FaunaScopeArm::FolderRead), "{good}");
            assert_eq!(folder_read_qualifier(good), Some(id), "{good}");
            assert_eq!(folder_deposit_qualifier(good), None, "{good}");
            assert!(card_row(good).is_some_and(|r| r.contains("keys to exactly that folder")));
            assert!(refused_to_remote_form(good), "{good}");
        }
        for bad in [
            SCOPE_FOLDER_READ,
            "fauna:folder:read:abc",
            "fauna:folder:read:042",
            "fauna:folder:read:0",
            "fauna:folder:read:-4",
            "fauna:folder:read:9223372036854775808",
            "fauna:folder:list:42",
            "fauna:folders:read:42",
        ] {
            assert_eq!(arm_of(bad), None, "{bad}");
            assert_eq!(folder_read_qualifier(bad), None, "{bad}");
        }
        assert!(FaunaScopeArm::FolderRead.has_grant_twin());
        assert_eq!(
            FaunaScopeArm::FolderRead.door(),
            Door::Mda {
                relay_kind: WEBDAV_ADMIT_PRINCIPAL_KIND
            }
        );
        // The bare form is refused to a remote client too (a card-qualified
        // request); nothing else is form-gated.
        assert!(refused_to_remote_form(SCOPE_FOLDER_READ));
        for open in [
            SCOPE_FEED_READ,
            "fauna:folder:deposit:42",
            SCOPE_FOLDER_DEPOSIT,
            "fauna:records:rw:ext.app.example.*",
            "fauna:folder:read:abc",
            "atproto",
        ] {
            assert!(!refused_to_remote_form(open), "{open}");
        }
    }

    /// The admin arm (`authorization-server.md` § Scope grammar → *The admin
    /// arms*): the qualifier is required and is one BUILT page of the
    /// server's closed set; a bare string, an unbuilt page and a verb no
    /// slice has built match no arm, so each is `invalid_scope` at PAR. No
    /// grant twin, not form-gated, the nest's door.
    #[test]
    fn the_admin_read_arm_takes_exactly_one_built_page() {
        assert_eq!(
            arm_of(SCOPE_ADMIN_READ_DASHBOARD),
            Some(FaunaScopeArm::AdminRead)
        );
        assert_eq!(
            admin_read_qualifier(SCOPE_ADMIN_READ_DASHBOARD),
            Some(AdminPage::Dashboard)
        );
        assert_eq!(AdminPage::Dashboard.ui_page(), "admin-dashboard");
        let row = card_row(SCOPE_ADMIN_READ_DASHBOARD).expect("a card row");
        assert!(row.starts_with("As this server's admin"), "{row}");
        assert!(row.contains("no keys are shared"), "{row}");
        for bad in [
            SCOPE_ADMIN_READ,
            "fauna:admin:read:custody-hosting",
            "fauna:admin:read:admin-dashboard",
            "fauna:admin:read:dashboard.stats",
        ] {
            assert!(parse(bad).is_some(), "{bad:?} is well-formed");
            assert_eq!(arm_of(bad), None, "{bad:?} matches no arm");
            assert_eq!(admin_read_qualifier(bad), None, "{bad:?}");
        }
        assert!(!FaunaScopeArm::AdminRead.has_grant_twin());
        assert!(!FaunaScopeArm::AdminRead.qualifier_is_users());
        assert!(!refused_to_remote_form(SCOPE_ADMIN_READ_DASHBOARD));
        for page in AdminPage::ALL {
            assert_eq!(AdminPage::parse(page.name()), Some(*page));
            assert_eq!(page.ui_page(), format!("admin-{}", page.name()));
            let advertised = format!("{SCOPE_ADMIN_READ}:{}", page.name());
            assert_eq!(
                FaunaScopeArm::AdminRead
                    .advertised()
                    .contains(&advertised.as_str()),
                page.has_reads(),
                "{page:?}: a read string is advertised iff the page has reads"
            );
            assert_eq!(page.read_card_row().is_some(), page.has_reads(), "{page:?}");
            assert_eq!(
                arm_of(&advertised).is_some(),
                page.has_reads(),
                "{page:?}: a read string matches the arm iff the page has reads"
            );
            // Every built page reaches at least one kind under some verb.
            assert!(page.has_reads() || page.has_writes(), "{page:?}");
        }
    }

    /// The mail-side pages (`authorization-server.md` § Scope grammar → *The
    /// admin arms*): the reports and the pending-bridge roster are reads
    /// only, the CardDAV and WebDAV switches writes only, and the
    /// pending-bridge roster's card row says it never approves.
    #[test]
    fn the_mail_side_pages_take_the_verbs_their_kinds_have() {
        for (scope, page) in [
            (SCOPE_ADMIN_READ_MAIL, AdminPage::Mail),
            (SCOPE_ADMIN_READ_MAIL_REPORTS, AdminPage::MailReports),
            (SCOPE_ADMIN_READ_ALIASES, AdminPage::Aliases),
            (SCOPE_ADMIN_READ_CALENDAR, AdminPage::Calendar),
            (SCOPE_ADMIN_READ_BRIDGES_PENDING, AdminPage::BridgesPending),
        ] {
            assert_eq!(admin_read_qualifier(scope), Some(page), "{scope}");
        }
        for (scope, page) in [
            (SCOPE_ADMIN_RW_MAIL, AdminPage::Mail),
            (SCOPE_ADMIN_RW_ALIASES, AdminPage::Aliases),
            (SCOPE_ADMIN_RW_CALENDAR, AdminPage::Calendar),
            (SCOPE_ADMIN_RW_CONTACTS, AdminPage::Contacts),
            (SCOPE_ADMIN_RW_FILES, AdminPage::Files),
        ] {
            assert_eq!(admin_rw_qualifier(scope), Some(page), "{scope}");
        }
        for bad in [
            "fauna:admin:rw:mail-reports",
            "fauna:admin:rw:bridges-pending",
            "fauna:admin:read:contacts",
            "fauna:admin:read:files",
            "fauna:admin:read:bridges",
        ] {
            assert_eq!(arm_of(bad), None, "{bad:?} matches no arm");
            assert_eq!(card_row(bad), None, "{bad:?}");
            assert_eq!(admin_read_qualifier(bad), None, "{bad:?}");
            assert_eq!(admin_rw_qualifier(bad), None, "{bad:?}");
        }
        let roster = card_row(SCOPE_ADMIN_READ_BRIDGES_PENDING).expect("a card row");
        assert!(roster.contains("never approve"), "{roster}");
        let mail = card_row(SCOPE_ADMIN_RW_MAIL).expect("a card row");
        assert!(mail.contains("never rotate a mail secret"), "{mail}");
    }

    /// The admin plane's `rw` verb: one string per built page WITH writes —
    /// a page of reads only (the dashboard) takes no `rw` string, and a bare
    /// or unbuilt one matches no arm. Its card row names the sovereign acts
    /// it never performs; the rest is the read verb's shape.
    #[test]
    fn the_admin_rw_arm_takes_exactly_one_built_page_with_writes() {
        assert_eq!(arm_of(SCOPE_ADMIN_RW_USERS), Some(FaunaScopeArm::AdminRw));
        assert_eq!(
            admin_rw_qualifier(SCOPE_ADMIN_RW_USERS),
            Some(AdminPage::Users)
        );
        assert_eq!(admin_read_qualifier(SCOPE_ADMIN_RW_USERS), None);
        assert_eq!(admin_rw_qualifier(SCOPE_ADMIN_READ_USERS), None);
        assert_eq!(
            arm_of(SCOPE_ADMIN_READ_USERS),
            Some(FaunaScopeArm::AdminRead)
        );
        let row = card_row(SCOPE_ADMIN_RW_USERS).expect("a card row");
        assert!(row.starts_with("As this server's admin"), "{row}");
        assert!(row.contains("no keys are shared"), "{row}");
        assert!(row.contains("never delete"), "{row}");
        for bad in [
            SCOPE_ADMIN_RW,
            "fauna:admin:rw:dashboard",
            "fauna:admin:rw:custody-hosting",
            "fauna:admin:rw:logs",
            "fauna:admin:rw:users.pending",
        ] {
            assert!(parse(bad).is_some(), "{bad:?} is well-formed");
            assert_eq!(arm_of(bad), None, "{bad:?} matches no arm");
            assert_eq!(admin_rw_qualifier(bad), None, "{bad:?}");
            assert_eq!(card_row(bad), None, "{bad:?}");
        }
        assert!(!FaunaScopeArm::AdminRw.has_grant_twin());
        assert!(!FaunaScopeArm::AdminRw.qualifier_is_users());
        assert_eq!(FaunaScopeArm::AdminRw.door(), Door::Nest);
        assert!(!refused_to_remote_form(SCOPE_ADMIN_RW_USERS));
        for page in AdminPage::ALL {
            let advertised = format!("{SCOPE_ADMIN_RW}:{}", page.name());
            assert_eq!(
                FaunaScopeArm::AdminRw
                    .advertised()
                    .contains(&advertised.as_str()),
                page.has_writes(),
                "{page:?}: an rw string is advertised iff the page has writes"
            );
            assert_eq!(page.rw_card_row().is_some(), page.has_writes(), "{page:?}");
        }
    }

    /// Only the read arm is bridge-doored; every other arm is the nest's.
    #[test]
    fn exactly_one_door_per_arm() {
        for arm in FaunaScopeArm::ALL {
            let mda = matches!(arm.door(), Door::Mda { .. });
            assert_eq!(mda, *arm == FaunaScopeArm::FolderRead, "{arm:?}");
        }
    }

    /// The read card row names the folder when the app knows it, and falls
    /// back to the generic row when it does not.
    #[test]
    fn the_folder_read_card_row_names_the_folder_when_known() {
        let s = "fauna:folder:read:42";
        let named = folder_read_card_row(s, Some("Photos")).unwrap();
        assert!(named.contains("your folder \"Photos\""), "{named}");
        assert!(named.contains("keys to exactly that folder are shared"));
        assert_eq!(folder_read_card_row(s, None).as_deref(), card_row(s));
        assert_eq!(
            folder_read_card_row("fauna:folder:deposit:42", Some("x")),
            None
        );
        assert_eq!(folder_read_card_row(SCOPE_FOLDER_READ, None), None);
    }

    /// The folder plane's qualifier is the user's (`authorization-server.md`
    /// § Scope grammar → *The folder plane's qualifier is the user's*): the
    /// bare string is a card-qualified request — no arm grants it as written,
    /// its bare form is itself, and the user's choice qualifies it into the
    /// arm's string. Every other arm's qualifier is the client's or the
    /// server's, and has no bare form.
    #[test]
    fn the_folder_plane_qualifier_is_the_users() {
        for arm in FaunaScopeArm::ALL {
            let folder = matches!(
                arm,
                FaunaScopeArm::FolderDeposit | FaunaScopeArm::FolderRead
            );
            assert_eq!(arm.qualifier_is_users(), folder, "{arm:?}");
        }
        assert_eq!(
            user_qualified_bare_arm(SCOPE_FOLDER_READ),
            Some(FaunaScopeArm::FolderRead)
        );
        assert_eq!(bare_form("fauna:folder:read:42"), Some(SCOPE_FOLDER_READ));
        assert_eq!(
            qualify_bare_scopes(&[SCOPE_FOLDER_DEPOSIT, SCOPE_FOLDER_READ], 42),
            Some(vec![
                "fauna:folder:deposit:42".to_string(),
                "fauna:folder:read:42".to_string()
            ]),
            "one choice qualifies every bare folder verb"
        );

        assert_eq!(arm_of(SCOPE_FOLDER_DEPOSIT), None);
        assert_eq!(
            user_qualified_bare_arm(SCOPE_FOLDER_DEPOSIT),
            Some(FaunaScopeArm::FolderDeposit)
        );
        assert_eq!(bare_form(SCOPE_FOLDER_DEPOSIT), Some(SCOPE_FOLDER_DEPOSIT));
        assert_eq!(
            bare_form("fauna:folder:deposit:42"),
            Some(SCOPE_FOLDER_DEPOSIT)
        );
        assert_eq!(
            card_row(SCOPE_FOLDER_DEPOSIT),
            card_row("fauna:folder:deposit:42")
        );
        assert_eq!(
            qualify_bare(SCOPE_FOLDER_DEPOSIT, 42).as_deref(),
            Some("fauna:folder:deposit:42")
        );
        assert_eq!(qualify_bare(SCOPE_FOLDER_DEPOSIT, 0), None);
        assert_eq!(qualify_bare("fauna:folder:deposit:42", 7), None);
        assert_eq!(
            qualify_bare_scopes(&["fauna:feed:read", SCOPE_FOLDER_DEPOSIT], 42),
            Some(vec![
                "fauna:feed:read".to_string(),
                "fauna:folder:deposit:42".to_string()
            ])
        );
        assert_eq!(
            qualify_bare_scopes(&["fauna:feed:read", "fauna:folder:deposit:7"], 42),
            None,
            "a choice qualifies only a bare scope"
        );
        assert_eq!(qualify_bare_scopes(&[SCOPE_FOLDER_DEPOSIT], -1), None);

        for not_users in [
            SCOPE_RECORDS_RW,
            "fauna:records:rw:ext.app.example.*",
            SCOPE_FEED_READ,
            SCOPE_IDENTITY_OP_NOSTR_SIGN_EVENT,
            "fauna:identity:op",
            "fauna:folder:deposit:abc",
            "fauna:folders:read",
            "atproto",
        ] {
            assert_eq!(user_qualified_bare_arm(not_users), None, "{not_users}");
            assert_eq!(bare_form(not_users), None, "{not_users}");
            assert_eq!(qualify_bare(not_users, 42), None, "{not_users}");
        }
    }

    /// The oracle arm advertises exactly one string per built class, each
    /// rendering that class's own card row.
    #[test]
    fn the_identity_op_arm_advertises_every_built_class_and_nothing_else() {
        use fauna_core::identity_op::IdentityOpClass;
        let mut want: Vec<String> = IdentityOpClass::ALL
            .iter()
            .map(|c| format!("fauna:identity:op:{}", c.name()))
            .collect();
        let mut got: Vec<String> = FaunaScopeArm::IdentityOp
            .advertised()
            .iter()
            .map(|s| s.to_string())
            .collect();
        want.sort();
        got.sort();
        assert_eq!(got, want);
        for class in IdentityOpClass::ALL {
            let s = format!("fauna:identity:op:{}", class.name());
            assert_eq!(card_row(&s), Some(class.card_row()));
            assert_eq!(identity_op_scope(*class), s);
            assert_eq!(
                identity_op_qualifier(identity_op_scope(*class)),
                Some(*class)
            );
        }
        assert!(FaunaScopeArm::IdentityOp.has_grant_twin());
    }

    /// The sovereign deny list binds at the grammar: no sovereign operation's
    /// name matches the arm, nor an unknown class, nor the bare arm.
    #[test]
    fn no_sovereign_or_unknown_class_matches_the_identity_op_arm() {
        use fauna_core::identity_op::SovereignOp;
        for op in SovereignOp::ALL {
            let s = format!("fauna:identity:op:{}", op.name());
            assert!(parse(&s).is_some(), "{s} is well-formed");
            assert_eq!(arm_of(&s), None, "{s} is sovereign");
            assert_eq!(card_row(&s), None, "{s} renders no card row");
        }
        for bad in [
            "fauna:identity:op",
            "fauna:identity:op:nostr",
            "fauna:identity:op:nostr.nip04",
            "fauna:identity:op:atproto.commit",
            "fauna:identity:read:nostr.sign_event",
        ] {
            assert_eq!(arm_of(bad), None, "{bad}");
        }
    }

    /// The conversations oracle's two classes (`conversations-oracle.md`
    /// § The two classes) are refused to the remote form PER CLASS: the arm
    /// itself stays admitted to it for every class the nest or a bridge
    /// performs, since only a device custodian has no door a remote process
    /// reaches.
    #[test]
    fn the_device_classes_are_refused_to_the_remote_form_and_the_arm_is_not() {
        use fauna_core::identity_op::IdentityOpClass;
        assert!(!FaunaScopeArm::IdentityOp.refused_to_remote_form());
        for class in IdentityOpClass::ALL {
            let scope = identity_op_scope(*class);
            assert_eq!(arm_of(scope), Some(FaunaScopeArm::IdentityOp), "{scope}");
            assert_eq!(
                refused_to_remote_form(scope),
                class.custodian().is_device(),
                "{scope}"
            );
        }
        assert!(refused_to_remote_form(SCOPE_IDENTITY_OP_CONVERSATION_READ));
        assert!(refused_to_remote_form(SCOPE_IDENTITY_OP_CONVERSATION_SEND));
        assert!(!refused_to_remote_form(SCOPE_IDENTITY_OP_POST_AUTHOR));
        // A near miss is no class at all, so it is invalid_scope at PAR on
        // its own grounds rather than form-gated.
        assert!(!refused_to_remote_form(
            "fauna:identity:op:conversation.write"
        ));
        assert_eq!(arm_of("fauna:identity:op:conversation.write"), None);
    }

    /// The fifth arm (`authorization-server.md` § Scope grammar → *The fifth
    /// arm*): exactly its one string, a keyed twin, the nest's door, and
    /// refused to a remote-form client.
    #[test]
    fn the_personalization_arm_is_one_string_keyed_and_form_gated() {
        let arm = FaunaScopeArm::PersonalizationRw;
        assert_eq!(arm_of(SCOPE_PERSONALIZATION_RW), Some(arm));
        assert_eq!(arm.advertised(), &[SCOPE_PERSONALIZATION_RW]);
        assert!(arm.has_grant_twin());
        assert!(!arm.takes_qualifier());
        assert!(!arm.qualifier_is_users());
        assert_eq!(arm.door(), Door::Nest);
        assert!(refused_to_remote_form(SCOPE_PERSONALIZATION_RW));
        assert!(
            card_row(SCOPE_PERSONALIZATION_RW)
                .is_some_and(|r| r.contains("keys to exactly those models"))
        );
        for near in [
            "fauna:personalization:rw:topic",
            "fauna:personalization:read",
            "fauna:personalization:write",
            "fauna:personalisation:rw",
        ] {
            assert_eq!(arm_of(near), None, "{near:?} is not the arm");
            assert!(!refused_to_remote_form(near), "{near:?}");
        }
        assert_eq!(user_qualified_bare_arm(SCOPE_PERSONALIZATION_RW), None);
        assert_eq!(bare_form(SCOPE_PERSONALIZATION_RW), None);
    }

    #[test]
    fn the_events_arm_takes_no_qualifier_and_shares_no_keys() {
        assert_eq!(
            arm_of(SCOPE_EVENTS_SUBSCRIBE),
            Some(FaunaScopeArm::EventsSubscribe)
        );
        assert!(!FaunaScopeArm::EventsSubscribe.has_grant_twin());
        for near in [
            "fauna:events:subscribe:ext.a.example.notes",
            "fauna:events:read",
            "fauna:event:subscribe",
        ] {
            assert_eq!(arm_of(near), None, "{near:?} is not the arm");
        }
        assert!(card_row(SCOPE_EVENTS_SUBSCRIBE).is_some());
    }

    #[test]
    fn the_follow_arm_takes_no_qualifier_and_shares_no_keys() {
        assert_eq!(arm_of(SCOPE_FOLLOW_WRITE), Some(FaunaScopeArm::FollowWrite));
        assert!(!FaunaScopeArm::FollowWrite.has_grant_twin());
        assert!(!FaunaScopeArm::FollowWrite.refused_to_remote_form());
        assert_eq!(FaunaScopeArm::FollowWrite.door(), Door::Nest);
        for near in [
            "fauna:follow:write:followers",
            "fauna:follow:read",
            "fauna:follows:write",
        ] {
            assert_eq!(arm_of(near), None, "{near:?} is not the arm");
        }
        assert!(card_row(SCOPE_FOLLOW_WRITE).is_some());
    }

    /// The profile arm and the two notifications arms
    /// (`authorization-server.md` § Scope grammar → *The profile arm*, *The
    /// notifications arms*): no qualifier, no grant twin, any execution form,
    /// the nest's door — each advertised as written, with its own card row.
    #[test]
    fn the_profile_and_notifications_arms_take_no_qualifier_and_share_no_keys() {
        for (scope, arm) in [
            (SCOPE_PROFILE_READ, FaunaScopeArm::ProfileRead),
            (SCOPE_NOTIFICATIONS_READ, FaunaScopeArm::NotificationsRead),
            (SCOPE_NOTIFICATIONS_WRITE, FaunaScopeArm::NotificationsWrite),
        ] {
            assert_eq!(arm_of(scope), Some(arm), "{scope}");
            assert!(!arm.takes_qualifier(), "{scope}");
            assert!(!arm.has_grant_twin(), "{scope}");
            assert!(!arm.refused_to_remote_form(), "{scope}");
            assert_eq!(arm.door(), Door::Nest, "{scope}");
            assert_eq!(arm.advertised(), &[scope], "{scope}");
            assert!(card_row(scope).is_some(), "{scope}");
        }
        for near in [
            "fauna:profile:read:account",
            "fauna:profile:write",
            "fauna:profile:rw",
            "fauna:profiles:read",
            "fauna:notifications:read:account",
            "fauna:notifications:rw",
            "fauna:notifications:dismiss",
            "fauna:notification:read",
        ] {
            assert_eq!(arm_of(near), None, "{near:?} is not an arm");
        }
        // The write arm's row says what it never does.
        assert!(
            card_row(SCOPE_NOTIFICATIONS_WRITE)
                .unwrap()
                .contains("never dismisses or clears")
        );
    }

    /// The filter both events doors apply: the events arm plus a `records`
    /// qualifier covering the tag's kind — structurally, by publisher.
    #[test]
    fn an_event_reaches_only_a_scope_the_session_can_list() {
        let both = vec![
            SCOPE_EVENTS_SUBSCRIBE.to_string(),
            "fauna:records:rw:ext.a.example.*".to_string(),
        ];
        assert!(event_reaches(&both, "ext:ext.a.example.notes"));
        // Another publisher's kind, a first-party scope, a folder name and a
        // non-canonical spelling reach nothing.
        for other in [
            "ext:ext.b.example.notes",
            "state",
            "state-fleet",
            "__ext:ext.a.example.notes",
            "ext:EXT.a.example.notes",
            "",
        ] {
            assert!(!event_reaches(&both, other), "{other:?} must not reach");
        }
        // The records arm alone subscribes to nothing; the events arm alone
        // covers no scope.
        let records_only = vec!["fauna:records:rw:ext.a.example.*".to_string()];
        assert!(!event_reaches(&records_only, "ext:ext.a.example.notes"));
        let events_only = vec![SCOPE_EVENTS_SUBSCRIBE.to_string()];
        assert!(!event_reaches(&events_only, "ext:ext.a.example.notes"));
        // An exact-kind qualifier covers exactly its kind.
        let exact = vec![
            SCOPE_EVENTS_SUBSCRIBE.to_string(),
            "fauna:records:rw:ext.a.example.notes".to_string(),
        ];
        assert!(event_reaches(&exact, "ext:ext.a.example.notes"));
        assert!(!event_reaches(&exact, "ext:ext.a.example.todo"));
    }

    /// The fifth arm lists `state`, so `state` reaches a session holding it
    /// beside the events arm — and only `state`: the fleet scope and every
    /// other first-party tag still reach nothing.
    #[test]
    fn a_state_event_reaches_only_a_personalization_session() {
        let both = vec![
            SCOPE_EVENTS_SUBSCRIBE.to_string(),
            SCOPE_PERSONALIZATION_RW.to_string(),
        ];
        assert!(event_reaches(&both, "state"));
        for other in ["state-fleet", "__state", "State", "ext:ext.a.example.notes"] {
            assert!(!event_reaches(&both, other), "{other:?} must not reach");
        }
        let personalization_only = vec![SCOPE_PERSONALIZATION_RW.to_string()];
        assert!(!event_reaches(&personalization_only, "state"));
        let records_and_events = vec![
            SCOPE_EVENTS_SUBSCRIBE.to_string(),
            "fauna:records:rw:ext.a.example.*".to_string(),
        ];
        assert!(!event_reaches(&records_and_events, "state"));
    }
}
