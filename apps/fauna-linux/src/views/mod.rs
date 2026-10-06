pub mod admin;
pub mod backups;
pub mod bridges;
pub mod contacts;
pub mod conversations;
/// Settings → Devices (roster) + Settings → Folders (control plane) — two
/// sub-pages over one shared `DevicesMachine` (2026-06-28 sync/folder UI
/// unification; the former top-level `peers` page split + folded into Settings).
pub mod devices_folders;
/// The shared GTK `RenderDocument` walker (`fauna_core::render`) — paints the
/// semantic body tree for **both** the Conversations and Feed pages
/// (render-model.md § D1/D6). Lifted out of `conversations/` once the feed also
/// adopted it, so it reads as the shared render layer it is, not a
/// conversations-internal helper.
pub mod document;
pub mod events;
/// The `family` page (`docs/goal/behavior/family-safety.md` § App surface) —
/// the guardian section (wards + the shared reach-policy editor + the approvals
/// queue + contact pre-approval + graduate) and the supervised section (guardian
/// handle + read-only policy summary). Reached via the **gated** `family-tab`
/// sidebar row (shown only when `fauna.family.status` returns a relationship).
pub mod family;
pub mod feed;
pub mod launch;
pub mod launch_instance_chooser;
pub mod layout;
pub mod lightbox;
/// Lazy GTK paint of a text block the shared line-run projection split
/// (`fauna_core::render::inline_line_runs`) — the document walker's helper for
/// a multi-megabyte body (render-model.md § Implementation status today).
pub mod line_runs;
pub mod media;
pub mod moderation;
pub mod nav_rail;
pub mod notifications;
pub mod onboarding;
pub mod personalization;
pub mod profile;
pub mod search;
pub mod settings_shell;
pub mod sidebar;
pub mod status;
