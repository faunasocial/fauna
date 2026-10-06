//! Admin "Web" page (`admin-web`) — the deployment apex-actor designation
//! (`docs/goal/behavior/web-content-hosting.md` § Admin apex hosting).
//!
//! One control: the **apex-actor picker** (`admin-web-apex-actor-select`) — an
//! Admin-class designation of which actor's `web` content serves at
//! `https://<domain>/`, "none" clearing it to the built-in info page. The direct
//! analogue of the per-domain catch-all mail actor (`admin-dns-domain-catch-all-
//! select`), and built the same way: a `gtk::DropDown` rebuilt each render with
//! the selection set BEFORE the handler is connected (so the initial
//! `set_selected` never spuriously dispatches). The actor list is every account on the
//! nest (`fauna_client_admin::users_list_all`); the current designation + set/clear ride the shared
//! `fauna-client-web` crate. Per-user subdomain hosting is the user `web-settings`
//! page, not here.

use fauna_ui_ids as ids;
use std::rc::Rc;
use std::sync::Arc;

use adw::prelude::*;
use fauna_client::NestClient;
use fauna_client_admin::AdminClient;
use fauna_client_web::WebClient;

use crate::async_helper::spawn_with_snapshot;
use crate::i18n::strings::admin;
use crate::i18n::strings::admin::web_page as WP;
use crate::testid::set_test_id;

type WebRpc = WebClient<Arc<NestClient>>;
type Admin = AdminClient<Arc<NestClient>>;

struct AdminWebCtx {
    web: Arc<WebRpc>,
    admin: Arc<Admin>,
    rt: tokio::runtime::Handle,
    // The container we rebuild the picker into on each render.
    picker_box: gtk::Box,
    info_marker: gtk::Label,
    error_label: gtk::Label,
}

/// One render's data: the current apex designation + the pickable actors.
struct Snapshot {
    current: Option<Vec<u8>>,
    actors: Vec<(Vec<u8>, String)>,
    error: Option<String>,
}

pub fn build_admin_web_page() -> adw::PreferencesPage {
    let page = adw::PreferencesPage::builder()
        .title(WP::TITLE)
        .icon_name("network-server-symbolic")
        .build();

    // The page heading, and a REAL one. ui.yaml scopes `admin-web-heading` as
    // this page's *title text* (registry: "Heading text for the admin Web
    // page"), and ios/windows/tui all render it as the visible title — linux
    // built it as an empty `visible(false)` marker, which is an invisible shim
    // (e2e-conventions.md point 1 bans them) AND unreachable by the driver:
    // `automation::find` prunes non-showing widgets, so no `get_text`/`wait_for`
    // on this id could ever resolve. Found by the convention-17 walk sweep
    // (`walk.rs`), which asserts every reachable surface shows a heading.
    // `adw::PreferencesPage::title` is metadata only inside the shell's plain
    // sub-stack — nothing renders it there — so this label IS the heading a
    // user reads. Shape mirrors the sibling admin pages (`views/admin.rs`'s
    // `admin-nest-heading`), per priority #1.
    let heading_group = adw::PreferencesGroup::new();
    let heading = gtk::Label::new(Some(WP::TITLE));
    heading.add_css_class("title-2");
    heading.set_halign(gtk::Align::Start);
    heading.set_margin_start(12);
    set_test_id(&heading, ids::ADMIN_WEB_HEADING);
    heading_group.add(&heading);
    page.add(&heading_group);

    let group = adw::PreferencesGroup::builder()
        .title(WP::APEX_SELECT_LABEL)
        .description(WP::APEX_SELECT_SUBTITLE)
        .build();
    page.add(&group);

    // The picker lives in a rebuildable container row.
    let picker_box = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    picker_box.set_margin_start(12);
    picker_box.set_margin_end(12);
    group.add(&picker_box);

    // Apex-URL explainer.
    let info_row = adw::ActionRow::builder().activatable(false).build();
    let info_marker = gtk::Label::builder()
        .wrap(true)
        .xalign(0.0)
        .css_classes(["dim-label"])
        .build();
    set_test_id(&info_marker, ids::ADMIN_WEB_APEX_INFO);
    info_row.add_suffix(&info_marker);
    group.add(&info_row);

    // Page-level error (Rule 2).
    let error_label = gtk::Label::builder().visible(false).build();
    set_test_id(&error_label, ids::ERROR_MESSAGE);
    let error_row = adw::ActionRow::builder().activatable(false).build();
    error_row.add_suffix(&error_label);
    group.add(&error_row);

    wire(picker_box, info_marker, error_label);
    page
}

fn wire(picker_box: gtk::Box, info_marker: gtk::Label, error_label: gtk::Label) {
    let client = match crate::settings::get_client() {
        Some(c) => c,
        None => return,
    };
    let ctx = Rc::new(AdminWebCtx {
        web: Arc::new(WebClient::new(client.nest_rpc().clone())),
        admin: Arc::new(AdminClient::new(client.nest_rpc().clone())),
        rt: client.runtime_handle(),
        picker_box,
        info_marker,
        error_label,
    });
    hydrate(&ctx);
}

/// Fetch the current apex + the actor list (retrying while the WS comes up), then
/// rebuild the picker.
fn hydrate(ctx: &Rc<AdminWebCtx>) {
    let web = Arc::clone(&ctx.web);
    let admin = Arc::clone(&ctx.admin);
    let ctx_render = Rc::clone(ctx);
    spawn_with_snapshot(
        &ctx.rt,
        move || async move {
            let current = web.get_apex_actor().await;
            match current {
                Ok(current) => {
                    // Every account on the nest (admin.md § 2 → *Which accounts a
                    // picker offers*), mapped by the shared
                    // `fauna_client_admin::actor_picker_options` (the two-halves
                    // rule's display half).
                    let actors = fauna_client_admin::users_list_all(admin.as_ref())
                        .await
                        .map(|users| fauna_client_admin::actor_picker_options(&users))
                        .unwrap_or_default();
                    Snapshot {
                        current,
                        actors,
                        error: None,
                    }
                }
                Err(e) => Snapshot {
                    current: None,
                    actors: Vec::new(),
                    error: Some(e.to_string()),
                },
            }
        },
        move |snap| render(&ctx_render, snap),
    );
}

/// The apex picker's trailing "not loaded" option for a designation that
/// isn't among the fetched actors — full un-truncated hex, not a short prefix
/// (admin.md § 2's two-halves rule; row 603: widened from a 4-byte-truncated
/// prefix to match apple's `hexFull`; mirrors `views::admin`'s
/// `actor_not_loaded_fallback_label`).
fn actor_not_loaded_fallback_label(id: &[u8]) -> String {
    admin::actor_id_fallback_label(&fauna_core::format::hex_full(id))
}

fn render(ctx: &Rc<AdminWebCtx>, snap: Snapshot) {
    // Rebuild the picker from scratch (mirrors the catch-all picker).
    while let Some(child) = ctx.picker_box.first_child() {
        ctx.picker_box.remove(&child);
    }

    // Model index 0 = "None" (clear); index i>0 = actors[i-1]. `ids` is the
    // parallel actor-id map (None at 0). A current designation not among the
    // loaded actors gets a trailing entry so it stays visible + selected rather
    // than silently clearing.
    let mut labels: Vec<String> = Vec::with_capacity(snap.actors.len() + 1);
    let mut ids: Vec<Option<Vec<u8>>> = Vec::with_capacity(snap.actors.len() + 1);
    labels.push(WP::APEX_NONE.to_string());
    ids.push(None);
    for (id, label) in &snap.actors {
        labels.push(label.clone());
        ids.push(Some(id.clone()));
    }
    let mut selected: u32 = 0;
    if let Some(current) = &snap.current {
        match snap.actors.iter().position(|(id, _)| id == current) {
            Some(i) => selected = (i + 1) as u32,
            None => {
                labels.push(actor_not_loaded_fallback_label(current));
                ids.push(Some(current.clone()));
                selected = (labels.len() - 1) as u32;
            }
        }
    }

    let label_refs: Vec<&str> = labels.iter().map(String::as_str).collect();
    let picker = gtk::DropDown::from_strings(&label_refs);
    picker.add_css_class("flat");
    picker.set_selected(selected);
    set_test_id(&picker, ids::ADMIN_WEB_APEX_ACTOR_SELECT);
    // tui's `admin::Action::SelectApexActor`.
    crate::offline_gate::declare_wire_kind(&picker, "fauna.web.set_apex_actor");
    {
        let ctx = Rc::clone(ctx);
        let ids = ids.clone();
        picker.connect_selected_notify(move |d| {
            let idx = d.selected() as usize;
            if let Some(actor_id) = ids.get(idx).cloned() {
                set_apex(&ctx, actor_id);
            }
        });
    }
    ctx.picker_box.append(&picker);

    // Apex-URL explainer (uses the admin's own domain = the node domain).
    let (_handle, domain, _) = crate::client::load_account_cache();
    let domain = domain.unwrap_or_default();
    let apex_url = fauna_core::web::apex_url(&domain);
    ctx.info_marker.set_text(&WP::apex_info(&apex_url));
    ctx.info_marker.set_visible(true);

    super::render_error_label(&ctx.error_label, snap.error.as_deref());
}

/// Designate (`Some`) or clear (`None`) the apex actor, then re-render.
fn set_apex(ctx: &Rc<AdminWebCtx>, actor_id: Option<Vec<u8>>) {
    let web = Arc::clone(&ctx.web);
    let ctx_render = Rc::clone(ctx);
    spawn_with_snapshot(
        &ctx.rt,
        move || async move {
            web.set_apex_actor(actor_id)
                .await
                .map(|_| ())
                .map_err(|e| e.to_string())
        },
        move |res| match res {
            Ok(()) => hydrate(&ctx_render),
            Err(msg) => super::render_error_label(&ctx_render.error_label, Some(&msg)),
        },
    );
}

#[cfg(test)]
mod tests {
    use super::actor_not_loaded_fallback_label;

    /// The apex picker's not-loaded fallback shows the **full** actor hex,
    /// not a 4-byte-truncated prefix (admin.md § 2's two-halves rule; row
    /// 603, mirroring apple's `actorLabelFallsBackToFullHexWhenActorNotLoaded`
    /// and `views::admin`'s equivalent pin for the DNS pickers). A 32-byte id
    /// renders 64 hex characters through the shared `"actor {short}…"`
    /// template — the trailing "…" stays even though `{short}` no longer is.
    #[test]
    fn apex_picker_fallback_label_shows_full_hex_not_truncated_prefix() {
        let id = [0xabu8; 32];
        let label = actor_not_loaded_fallback_label(&id);
        assert_eq!(label, format!("actor {}…", "ab".repeat(32)));
        assert_ne!(
            label,
            format!("actor {}…", "ab".repeat(4)),
            "must not regress to the old 4-byte-truncated prefix"
        );
    }
}
