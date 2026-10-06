//! The flat **`admin-files`** page (linux; the files-enable seed's lead
//! app, contacts sibling of `admin_contacts.rs`).
//!
//! Where a nest admin flips the deployment-wide **WebDAV-enable** toggle —
//! the files sibling of `admin-contacts`'s CardDAV-enable toggle. Email,
//! calendar, contacts, and files are four independently enableable features
//! of one MDA bridge (`webdav-server.md` § Independent enablement): the MDA
//! runs iff `mail_enabled || caldav_enabled || carddav_enabled ||
//! webdav_enabled`, so this toggle (or its three siblings) is what brings the
//! one bridge up/down — "enabled if any". No port row: WebDAV rides the
//! shared DAV listener `admin-calendar`'s port input governs
//! (`webdav-server.md` § Process topology). Target behavior + the page §:
//! `docs/goal/behavior/admin.md` § Files. UX/IDs:
//! `tests/e2e-unified/ui.yaml` `admin-files`.
//!
//! Like `admin_contacts.rs` this layer holds **no** business logic — it is a
//! dumb renderer of [`WebdavPolicySnapshot`] and dispatcher of
//! [`WebdavPolicyAction`]; the hydrate (`get_mail_config` → `webdav_enabled`)
//! and the `set_webdav_enabled` write live in the shared
//! `fauna_client_mail_settings::webdav_policy` machine (priority #2/#4), the
//! prior art the other five apps lift over the `build_webdav_policy_machine`
//! UniFFI/wasm export. The GTK skeleton itself is
//! [`super::admin_bool_toggle_page`]'s macro — byte-for-byte identical to
//! `admin_contacts.rs` modulo CardDAV/WebDAV naming before round 181 of the
//! shared-Rust lift sweep lifted the shape out.
//!
//! **No nest work** — `set_webdav_enabled` already exists + gates the MDA
//! (slice 1). Read + write are both LIVE; a nest rejection surfaces
//! via `WebdavPolicySnapshot::error`, never faked green. The page is registered
//! as an admin-shell `gtk::Stack` sub-page (child `admin-files`), reached
//! through the state protocol.

use fauna_client_mail_settings::{WebdavPolicyAction, WebdavPolicyMachine, WebdavPolicySnapshot};

use super::admin_bool_toggle_page::admin_bool_toggle_page;

admin_bool_toggle_page! {
    page_fn: build_admin_files_page,
    machine: WebdavPolicyMachine,
    action: WebdavPolicyAction,
    snapshot: WebdavPolicySnapshot,
    build_machine: crate::mail_glue::build_webdav_policy_machine,
    strings: crate::i18n::strings::admin::files_page,
    icon: "folder-symbolic",
    heading_id: "admin-files-heading",
    toggle_id: "admin-files-webdav-enabled-toggle",
    wire_kind: "fauna.bridges.set_webdav_enabled",
    set_variant: SetWebdavEnabled,
    field: webdav_enabled,
}
