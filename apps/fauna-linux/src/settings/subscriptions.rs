//! The consumer-side **`subscription-settings`** page (subscriptions Slice B) —
//! `docs/goal/behavior/monetization.md` § Pillar 1. Reached from the Settings
//! rail as "Subscriptions" (sibling of mail-settings / web-settings); shows
//! **this user's own subscriptions across every creator** with per-row
//! unsubscribe. Distinct from the profile Tiers-tab SELF author management
//! (`views/profile/tiers.rs`): that is what the user *offers*, this is what the
//! user *consumes*.
//!
//! All data comes from one shared-Rust read — `SubscriptionsClient::mine_list`
//! (`fauna.subscriptions.mine.list`, the caller-scoped enumeration; priority #2)
//! — rendered as `subscription-mine-row`s. Unsubscribe is the thin
//! `SubscriptionsClient::unsubscribe`: in encrypted mode it returns `Queued`
//! (the subscriber stays a member until the author commits the removal), so the
//! row does **not** vanish on click — the re-read reflects the nest's state.
//!
//! Claim redemption (`subscription-claim-redeem-{input,button}` —
//! `monetization.md` § Pillar 3 Q4's universal fallback binding): the buyer
//! pastes a post-payment claim code; the thin `PaymentsClient::claims_redeem`
//! binds it to this actor and the entitlement lands through Pillar 1's grant
//! queue — a success re-reads the mine list, where the queued grant renders
//! exactly like a queued subscribe (a "pending" row). Typed errors
//! (`fauna.payments.claim_{not_found,already_redeemed,voided}`) surface via
//! the page `error-message`.
//!
//! Observer-free, like the profile Tiers tab: a manual re-read on mount and
//! whenever the page becomes visible (the refresh closure the settings shell
//! wires to the stack's visible-child notify), plus a re-read after unsubscribe.
//! No client-side caching (`feed.md` § Architectural rules).
//!
//! The page renders plain `gtk::Box` rows with `set_test_id` visible labels —
//! the same idiom the sibling `views/profile/tiers.rs` uses for the subscriptions
//! lists (priority #3), so the indexed `subscription-mine-*` IDs are directly
//! AT-SPI-discoverable.

use fauna_ui_ids as ids;
use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;

use crate::async_helper::spawn_with_snapshot;

#[cfg(feature = "payments")]
use fauna_client_payments::PaymentsClient;
use fauna_client_subscriptions::SubscriptionsClient;
use fauna_client_subscriptions::subscriptions::MineSubscription;
use fauna_core::identity::ActorId;

use crate::client::FaunaClient;
use crate::i18n::strings::subscriptions as s;
use crate::testid::set_test_id;

/// Widget handles the render + event closures need.
struct Widgets {
    mine_list: gtk::Box,
    mine_placeholder: gtk::Label,
    mine_rows: RefCell<Vec<gtk::Box>>,
    /// The Pillar-3 claim-code input. Excises with the `payments` plane
    /// (`dynamic-features.md` § Platform-family surface excision): a store-safe
    /// build has no claim to redeem, so the field, its section and the
    /// `redeem_claim` handler all compile away together.
    #[cfg(feature = "payments")]
    claim_input: gtk::Entry,
    error_label: gtk::Label,
}

/// Everything the page's handlers + render need: the client (for `nest_rpc`),
/// the tokio handle for async dispatch, and the widget handles.
struct Ctx {
    client: Rc<FaunaClient>,
    rt: tokio::runtime::Handle,
    w: Widgets,
}

/// Build the **`subscription-settings`** page. Returns the page widget plus a
/// `refresh` entry point the settings shell wires to its on-visible hook (the
/// page is observer-free, so it must re-read when it becomes visible — e.g. an
/// author approved a pending request, or the user subscribed elsewhere). When no
/// client is registered (the unit test) the page still builds with every
/// ui.yaml ID present, at its empty placeholder.
pub fn build_subscriptions_page() -> (gtk::Box, Rc<dyn Fn()>) {
    let outer = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(16)
        .margin_top(12)
        .margin_bottom(12)
        .margin_start(12)
        .margin_end(12)
        .build();

    // page-heading — the global per-page heading element.
    let heading = gtk::Label::new(Some(s::TITLE));
    heading.set_halign(gtk::Align::Start);
    heading.add_css_class("title-2");
    set_test_id(&heading, ids::PAGE_HEADING);
    outer.append(&heading);

    // error-message — page-level error label (Rule 2), hidden until set.
    let error_label = gtk::Label::builder().visible(false).build();
    error_label.set_halign(gtk::Align::Start);
    error_label.add_css_class("error");
    set_test_id(&error_label, ids::ERROR_MESSAGE);
    outer.append(&error_label);

    // subscription-mine-section — the consumer list of subscriptions.
    let section = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(8)
        .accessible_role(gtk::AccessibleRole::Group)
        .build();
    set_test_id(&section, ids::SUBSCRIPTION_MINE_SECTION);
    let section_title = gtk::Label::new(Some(s::MY_SUBSCRIPTIONS));
    section_title.set_halign(gtk::Align::Start);
    section_title.add_css_class("title-4");
    section.append(&section_title);

    let mine_list = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(6)
        .accessible_role(gtk::AccessibleRole::Group)
        .build();
    let mine_placeholder = gtk::Label::new(Some(s::NO_SUBSCRIPTIONS));
    mine_placeholder.set_halign(gtk::Align::Start);
    mine_placeholder.add_css_class("dim-label");
    mine_list.append(&mine_placeholder);
    section.append(&mine_list);
    outer.append(&section);

    // Claim redemption (monetization.md § Pillar 3 Q4 — the universal
    // fallback binding): paste a post-payment claim code → the entitlement
    // binds to this actor and lands as a queued grant in the list above.
    //
    // Gated on the RENDER, not only on the client call below: criterion 1 of
    // `dynamic-features.md` § What "completely compiled away" means is a
    // `strings`-grep for ELEMENT IDS, so a section that merely never fires
    // would still ship `subscription-claim-redeem-input`/`-button`.
    #[cfg(feature = "payments")]
    let (claim_input, claim_button) = {
        let claim_section = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(8)
            .accessible_role(gtk::AccessibleRole::Group)
            .build();
        let claim_title = gtk::Label::new(Some(s::REDEEM_CLAIM_TITLE));
        claim_title.set_halign(gtk::Align::Start);
        claim_title.add_css_class("title-4");
        claim_section.append(&claim_title);
        let claim_row = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(8)
            .build();
        let claim_input = gtk::Entry::builder()
            .placeholder_text(s::CLAIM_CODE)
            .hexpand(true)
            .build();
        set_test_id(&claim_input, ids::SUBSCRIPTION_CLAIM_REDEEM_INPUT);
        claim_row.append(&claim_input);
        let claim_button = gtk::Button::with_label(s::REDEEM);
        claim_button.add_css_class("suggested-action");
        set_test_id(&claim_button, ids::SUBSCRIPTION_CLAIM_REDEEM_BUTTON);
        crate::offline_gate::declare_wire_kind(&claim_button, "fauna.payments.claims.redeem");
        claim_row.append(&claim_button);
        claim_section.append(&claim_row);
        outer.append(&claim_section);
        (claim_input, claim_button)
    };

    let widgets = Widgets {
        mine_list,
        mine_placeholder,
        mine_rows: RefCell::new(Vec::new()),
        #[cfg(feature = "payments")]
        claim_input,
        error_label,
    };

    let refresh: Rc<dyn Fn()> = match crate::settings::get_client() {
        Some(client) => {
            let ctx = Rc::new(Ctx {
                client: Rc::clone(&client),
                rt: client.runtime_handle(),
                w: widgets,
            });
            refresh_mine(&ctx);
            #[cfg(feature = "payments")]
            {
                let ctx = Rc::clone(&ctx);
                claim_button.connect_clicked(move |_| redeem_claim(&ctx));
            }
            let ctx = Rc::clone(&ctx);
            Rc::new(move || refresh_mine(&ctx))
        }
        // No client (unit test / pre-auth): the page stays at its placeholder.
        None => Rc::new(|| {}),
    };

    (outer, refresh)
}

/// Read the caller's subscriptions (`mine.list`) on the tokio runtime, then
/// re-render on the GTK main thread. A single NestClient RPC — the transport
/// already parks it while the post-login socket comes up (transport.md §
/// Request lifecycle step 3), so no app-side retry is needed.
fn refresh_mine(ctx: &Rc<Ctx>) {
    let nest = ctx.client.nest_rpc().clone();
    let ctx_render = Rc::clone(ctx);
    spawn_with_snapshot(
        &ctx.rt,
        move || async move {
            let subs = SubscriptionsClient::new(nest);
            subs.mine_list().await.map_err(|e| e.to_string())
        },
        move |res| match res {
            Ok(subs) => {
                clear_error(&ctx_render.w);
                render_rows(&ctx_render, &subs);
            }
            Err(msg) => show_error(&ctx_render.w, &msg),
        },
    );
}

/// Redeem the pasted claim code (`fauna.payments.claims.redeem`) — binds the
/// entitlement to this actor; success clears the input and re-reads the mine
/// list, where the queued grant renders exactly like a queued subscribe (a
/// "pending" row). Typed `fauna.payments.claim_*` errors surface via the
/// page error label.
#[cfg(feature = "payments")]
fn redeem_claim(ctx: &Rc<Ctx>) {
    let code = ctx.w.claim_input.text().trim().to_string();
    if code.is_empty() {
        return;
    }
    let nest = ctx.client.nest_rpc().clone();
    let ctx_render = Rc::clone(ctx);
    spawn_with_snapshot(
        &ctx.rt,
        move || async move {
            PaymentsClient::new(nest)
                .claims_redeem(code)
                .await
                .map(|_| ())
                .map_err(|e| e.to_string())
        },
        move |res| match res {
            Ok(()) => {
                clear_error(&ctx_render.w);
                ctx_render.w.claim_input.set_text("");
                refresh_mine(&ctx_render);
            }
            Err(msg) => show_error(&ctx_render.w, &msg),
        },
    );
}

/// Dispatch an unsubscribe for `author_id` (`fauna.subscriptions.unsubscribe`),
/// then re-read so the list reflects the nest's state. Encrypted mode returns
/// `Queued` (the row stays until the author commits), so a successful click is
/// *not* an immediate removal.
fn unsubscribe(ctx: &Rc<Ctx>, author_id: ActorId) {
    let nest = ctx.client.nest_rpc().clone();
    let ctx_render = Rc::clone(ctx);
    spawn_with_snapshot(
        &ctx.rt,
        move || async move {
            let subs = SubscriptionsClient::new(nest);
            subs.unsubscribe(author_id)
                .await
                .map(|_| ())
                .map_err(|e| e.to_string())
        },
        move |res| match res {
            Ok(()) => {
                clear_error(&ctx_render.w);
                refresh_mine(&ctx_render);
            }
            Err(msg) => show_error(&ctx_render.w, &msg),
        },
    );
}

fn render_rows(ctx: &Rc<Ctx>, subs: &[MineSubscription]) {
    let mut rows = ctx.w.mine_rows.borrow_mut();
    for row in rows.drain(..) {
        ctx.w.mine_list.remove(&row);
    }
    for sub in subs {
        let row = build_row(ctx, sub);
        ctx.w.mine_list.append(&row);
        rows.push(row);
    }
    ctx.w.mine_placeholder.set_visible(subs.is_empty());
}

fn build_row(ctx: &Rc<Ctx>, sub: &MineSubscription) -> gtk::Box {
    let row = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .build();
    set_test_id(&row, ids::SUBSCRIPTION_MINE_ROW);

    // The viewer's own nickname for the creator, else their handle if the
    // nest resolved one, else the hex actor id — the one resolver over the
    // shared chooser (value-formatting.md § Subscription author label, § Peer
    // display label).
    let author_text = crate::conversations::overlays::projection()
        .subscription_author_label(sub.handle.as_deref(), &sub.author_id.0);
    let author = gtk::Label::new(Some(&author_text));
    author.set_halign(gtk::Align::Start);
    author.set_hexpand(true);
    author.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
    set_test_id(&author, ids::SUBSCRIPTION_MINE_AUTHOR);
    row.append(&author);

    let tier = gtk::Label::new(Some(&sub.tier));
    set_test_id(&tier, ids::SUBSCRIPTION_MINE_TIER);
    row.append(&tier);

    // The raw wire status ("active" | "pending"), rendered verbatim (uniform
    // with §2's `subscription-request-kind`).
    let status = gtk::Label::new(Some(&sub.status));
    status.add_css_class("dim-label");
    set_test_id(&status, ids::SUBSCRIPTION_MINE_STATUS);
    row.append(&status);

    let unsubscribe_btn = gtk::Button::with_label(s::UNSUBSCRIBE);
    unsubscribe_btn.add_css_class("destructive-action");
    set_test_id(&unsubscribe_btn, ids::SUBSCRIPTION_MINE_UNSUBSCRIBE_BUTTON);
    crate::offline_gate::declare_wire_kind(&unsubscribe_btn, "fauna.subscriptions.unsubscribe");
    {
        let ctx = Rc::clone(ctx);
        let author_id = sub.author_id;
        unsubscribe_btn.connect_clicked(move |_| unsubscribe(&ctx, author_id));
    }
    row.append(&unsubscribe_btn);

    row
}

fn show_error(w: &Widgets, msg: &str) {
    super::render_error_label(&w.error_label, Some(msg));
}

fn clear_error(w: &Widgets) {
    super::render_error_label(&w.error_label, None);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_builds_without_client() {
        crate::testid::run_on_gtk_thread(|| {
            // No registered client → the page stays at its static placeholder, but
            // it must still build with every ui.yaml ID present.
            let (_page, _refresh) = build_subscriptions_page();
        });
    }
}
