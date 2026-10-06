//! The flat **`admin-contacts`** page (linux; the contacts-enable seed's lead
//! app).
//!
//! Where a nest admin flips the deployment-wide **CardDAV-enable** toggle — the
//! contacts sibling of `admin-calendar`'s CalDAV-enable toggle. Email, calendar,
//! and contacts are three independently enableable features of one MDA bridge
//! (`carddav-server.md` § Independent enablement): the MDA runs iff
//! `mail_enabled || caldav_enabled || carddav_enabled`, so this toggle (or its
//! two siblings) is what brings the one bridge up/down — "enabled if any". No
//! port row: CardDAV rides the shared DAV listener `admin-calendar`'s port input
//! governs (`carddav-server.md` § Process topology). Target behavior + the page
//! §: `docs/goal/behavior/admin.md` § Contacts. UX/IDs:
//! `tests/e2e-unified/ui.yaml` `admin-contacts`.
//!
//! Like `admin_files.rs` this layer holds **no** business logic — it is a
//! dumb renderer of [`CarddavPolicySnapshot`] and dispatcher of
//! [`CarddavPolicyAction`]; the hydrate (`get_mail_config` → `carddav_enabled`)
//! and the `set_carddav_enabled` write live in the shared
//! `fauna_client_mail_settings::carddav_policy` machine (priority #2/#4), the
//! prior art the other five apps lift over the `build_carddav_policy_machine`
//! UniFFI/wasm export. The GTK skeleton itself is
//! [`super::admin_bool_toggle_page`]'s macro — byte-for-byte identical to
//! `admin_files.rs` modulo CardDAV/WebDAV naming before round 181 of the
//! shared-Rust lift sweep lifted the shape out.
//!
//! **No nest work** — `set_carddav_enabled` already exists + gates the MDA
//! (slice 1). Read + write are both LIVE; a nest rejection surfaces
//! via `CarddavPolicySnapshot::error`, never faked green. The page is registered
//! as an admin-shell `gtk::Stack` sub-page (child `admin-contacts`), reached
//! through the state protocol.

use fauna_client_mail_settings::{
    CarddavPolicyAction, CarddavPolicyMachine, CarddavPolicySnapshot,
};

use super::admin_bool_toggle_page::admin_bool_toggle_page;

admin_bool_toggle_page! {
    page_fn: build_admin_contacts_page,
    machine: CarddavPolicyMachine,
    action: CarddavPolicyAction,
    snapshot: CarddavPolicySnapshot,
    build_machine: crate::mail_glue::build_carddav_policy_machine,
    strings: crate::i18n::strings::admin::contacts_page,
    icon: "x-office-address-book-symbolic",
    heading_id: "admin-contacts-heading",
    toggle_id: "admin-contacts-carddav-enabled-toggle",
    wire_kind: "fauna.bridges.set_carddav_enabled",
    set_variant: SetCarddavEnabled,
    field: carddav_enabled,
}
