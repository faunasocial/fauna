//! The profile Tiers-tab **OTHER** (subscriber-browse) section — subscriptions
//! Slice B item 7 (`docs/goal/behavior/monetization.md` § Pillar 1, surface 2;
//! `docs/goal/ui/profile.md` § Layout & flow → *Another's profile (subscriber
//! browse)*). When viewing someone else's profile, the Tiers tab shows the
//! creator's offered tiers (`subscription-offers-section` / `subscription-offer-list`)
//! — per-row name / price / description / external payment link / Subscribe +
//! the viewer's status (none / pending / active).
//!
//! Reads over shared Rust (priority #2): `SubscriptionsClient::offers_list(author)`
//! for the offered tiers + `status_get(author)` for the viewer's current status;
//! `subscribe(author, tier)` on the per-row button. Observer-free: a manual
//! re-read after each subscribe (no client-side caching — `feed.md`
//! § Architectural rules). The free "followers" tier is followed via the header
//! `profile-follow-button` (`mod.rs`), so it is excluded from the per-row offers.

use fauna_ui_ids as ids;
use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;

use fauna_client_subscriptions::SubscriptionsClient;
use fauna_client_subscriptions::subscriptions::{SubscribeReply, TierItem};
use fauna_core::format::{OfferStatus, offer_status, offer_status_label};
use fauna_core::identity::ActorId;

use super::tiers::{list_box, placeholder, section_box};
use crate::async_helper::spawn_with_snapshot;
use crate::client::FaunaClient;
use crate::i18n::strings::subscriptions as s;
use crate::testid::set_test_id;

/// The free "followers" tier — followed via the header `profile-follow-button`,
/// so it is not shown as a per-row paid offer (`profile.md` § Layout & flow).
///
/// Re-exported from `fauna-core`, not re-typed. The 2026-07-08 ruling that
/// keeps the per-app literals (`fauna_core::subscription::FOLLOWERS_TIER`'s
/// own doc) rests entirely on FFI/WASM call overhead — a lift would trade a
/// compile-time constant for a runtime boundary call. That argument does not
/// reach a **Rust-native** app: linux already compiles `fauna-core`, so the
/// reserved wire name costs exactly the same `&'static str` imported as it
/// did re-typed. tui reaches for the shared constant for this reason
/// (`profile/mod.rs`); linux was the last Rust shell still holding a copy.
pub(super) use fauna_core::subscription::FOLLOWERS_TIER;

struct Widgets {
    offer_list: gtk::Box,
    offer_placeholder: gtk::Label,
    offer_rows: RefCell<Vec<gtk::Box>>,
    /// Shared page-level `error-message`, owned by the profile view (Rule 2).
    error_label: gtk::Label,
}

struct Ctx {
    client: Rc<FaunaClient>,
    author: ActorId,
    /// The viewer's currently-held tier name for this author (the highest-rank
    /// confirmed subscription `status.get` returns), used to render each row's
    /// status. Refreshed alongside the offer list.
    status_tier: RefCell<Option<String>>,
    w: Widgets,
}

/// Build the "tiers" inner-stack page for **another** actor's profile — the
/// subscriber-browse offers section. The page-level `error-message` label is
/// owned by the profile view and shared in (so subscribe failures surface
/// there). Returns the page widget plus a `refresh` entry point the profile view
/// wires to its on-visible hook (`app.rs`), mirroring the SELF Tiers tab.
pub fn build_offers_tab(
    client: &Rc<FaunaClient>,
    error_label: gtk::Label,
    author: ActorId,
) -> (gtk::Box, Rc<dyn Fn()>) {
    let outer = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(16)
        .margin_top(12)
        .margin_bottom(12)
        .margin_start(12)
        .margin_end(12)
        .build();

    let section = section_box("subscription-offers-section", s::OFFERS);
    let offer_list = list_box();
    set_test_id(&offer_list, ids::SUBSCRIPTION_OFFER_LIST);
    let offer_placeholder = placeholder(s::NO_OFFERS);
    offer_list.append(&offer_placeholder);
    section.append(&offer_list);
    outer.append(&section);

    let ctx = Rc::new(Ctx {
        client: Rc::clone(client),
        author,
        status_tier: RefCell::new(None),
        w: Widgets {
            offer_list,
            offer_placeholder,
            offer_rows: RefCell::new(Vec::new()),
            error_label,
        },
    });

    let refresh: Rc<dyn Fn()> = {
        let ctx = Rc::clone(&ctx);
        Rc::new(move || refresh_offers(&ctx))
    };
    refresh();
    (outer, refresh)
}

struct Snapshot {
    offers: Result<Vec<TierItem>, String>,
    status: Result<Option<String>, String>,
}

/// Read the author's offered tiers + the viewer's status, then re-render. Runs
/// on the tokio runtime; result applied on the GTK thread (mirrors the SELF
/// Tiers tab `refresh_all`).
fn refresh_offers(ctx: &Rc<Ctx>) {
    let nest = ctx.client.nest_rpc().clone();
    let author = ctx.author;
    let ctx_render = Rc::clone(ctx);
    spawn_with_snapshot(
        &ctx.client.runtime_handle(),
        move || async move {
            let subs = SubscriptionsClient::new(nest);
            let offers = subs.offers_list(author).await.map_err(|e| e.to_string());
            let status = subs
                .status_get(author)
                .await
                .map(|r| r.tier)
                .map_err(|e| e.to_string());
            Snapshot { offers, status }
        },
        move |snap| apply_snapshot(&ctx_render, snap),
    );
}

fn apply_snapshot(ctx: &Rc<Ctx>, snap: Snapshot) {
    // A failed status read is non-fatal — every row just renders "not subscribed".
    if let Ok(tier) = snap.status {
        *ctx.status_tier.borrow_mut() = tier;
    }
    match snap.offers {
        Ok(offers) => render_offer_rows(ctx, &offers),
        Err(msg) => show_error(ctx, &msg),
    }
}

fn render_offer_rows(ctx: &Rc<Ctx>, offers: &[TierItem]) {
    let mut rows = ctx.w.offer_rows.borrow_mut();
    for row in rows.drain(..) {
        ctx.w.offer_list.remove(&row);
    }
    // The free "followers" tier is the header follow-button's job, not a paid row.
    let shown: Vec<&TierItem> = offers.iter().filter(|t| t.name != FOLLOWERS_TIER).collect();
    for tier in &shown {
        let row = build_offer_row(ctx, tier);
        ctx.w.offer_list.append(&row);
        rows.push(row);
    }
    ctx.w.offer_placeholder.set_visible(shown.is_empty());
}

/// The viewer's status label for `tier_name`, from the held-tier snapshot, over
/// the shared `fauna_core::format::offer_status` derivation (precedence
/// Active>Pending>None). At render time `pending` is `false` — `status.get` only
/// reports the confirmed tier, so a reload shows active/none; the transient
/// post-click pending overlay is applied by [`subscribe_to`]. Shared with the
/// other apps so the status→label map can't drift (priority #2/#4).
fn status_text(ctx: &Rc<Ctx>, tier_name: &str) -> String {
    let held = ctx.status_tier.borrow();
    let status = offer_status(tier_name, held.as_deref(), false);
    offer_status_label(status).resolve(crate::i18n::strings::lookup)
}

fn build_offer_row(ctx: &Rc<Ctx>, tier: &TierItem) -> gtk::Box {
    let row = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .build();
    set_test_id(&row, ids::SUBSCRIPTION_OFFER_ROW);

    let info = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(2)
        .hexpand(true)
        .build();

    let name = gtk::Label::new(Some(&tier.name));
    name.set_halign(gtk::Align::Start);
    name.add_css_class("heading");
    set_test_id(&name, ids::SUBSCRIPTION_OFFER_NAME);
    info.append(&name);

    let price = gtk::Label::new(tier.price_hint.as_deref());
    price.set_halign(gtk::Align::Start);
    price.add_css_class("dim-label");
    set_test_id(&price, ids::SUBSCRIPTION_OFFER_PRICE);
    info.append(&price);

    let description = gtk::Label::new(tier.description.as_deref());
    description.set_halign(gtk::Align::Start);
    description.add_css_class("dim-label");
    description.set_wrap(true);
    set_test_id(&description, ids::SUBSCRIPTION_OFFER_DESCRIPTION);
    info.append(&description);

    row.append(&info);

    // External payment link (only when the tier carries a checkout URL).
    if let Some(url) = tier.payment_url.clone() {
        let pay = gtk::Button::with_label(s::PAYMENT_URL);
        pay.set_valign(gtk::Align::Center);
        set_test_id(&pay, ids::SUBSCRIPTION_OFFER_PAYMENT_LINK);
        let ctx_click = Rc::clone(ctx);
        pay.connect_clicked(move |btn| {
            // The payment link is nest/author-supplied; refuse to open a
            // non-https scheme via the shared `fauna_core::subscription::
            // is_safe_payment_url` guard (F-CL2 anti-phishing-redirect class — the same check `views/feed/post_list.rs`'s
            // `gated-post-payment-link` applies).
            if !fauna_core::subscription::is_safe_payment_url(&url) {
                show_error(&ctx_click, s::UNSAFE_PAYMENT_URL);
                return;
            }
            gtk::UriLauncher::new(&url).launch(
                btn.root().and_downcast::<gtk::Window>().as_ref(),
                gtk::gio::Cancellable::NONE,
                |_| {},
            );
        });
        row.append(&pay);
    }

    let status_str = status_text(ctx, &tier.name);
    let status = gtk::Label::new(Some(status_str.as_str()));
    status.set_valign(gtk::Align::Center);
    status.add_css_class("dim-label");
    set_test_id(&status, ids::SUBSCRIPTION_OFFER_STATUS);
    row.append(&status);

    let subscribe = gtk::Button::with_label(s::SUBSCRIBE);
    subscribe.add_css_class("suggested-action");
    subscribe.set_valign(gtk::Align::Center);
    set_test_id(&subscribe, ids::SUBSCRIPTION_OFFER_SUBSCRIBE_BUTTON);
    crate::offline_gate::declare_wire_kind(&subscribe, "fauna.subscriptions.subscribe");
    {
        let ctx = Rc::clone(ctx);
        let tier_name = tier.name.clone();
        let status_label = status.clone();
        subscribe.connect_clicked(move |btn| {
            subscribe_to(&ctx, &tier_name, &status_label, btn);
        });
    }
    row.append(&subscribe);

    row
}

/// Subscribe the viewer to `tier_name` (the per-row Subscribe button). Plaintext
/// / auto-approve tiers resolve `Approved` inline; encrypted mode resolves
/// `Queued` → the row flips to "pending" (`monetization.md` § Pillar 1 — Flows).
fn subscribe_to(ctx: &Rc<Ctx>, tier_name: &str, status_label: &gtk::Label, btn: &gtk::Button) {
    btn.set_sensitive(false);
    let nest = ctx.client.nest_rpc().clone();
    let author = ctx.author;
    let tier = tier_name.to_string();
    // The viewer's own identity seed → the subscriber keypair whose ML-KEM ek
    // gets published unconditionally (surface B, S4b).
    let subscriber_secret = ctx.client.secret_bytes();
    let ctx_render = Rc::clone(ctx);
    let status_label = status_label.clone();
    let btn = btn.clone();
    spawn_with_snapshot(
        &ctx.client.runtime_handle(),
        move || async move {
            let subs = SubscriptionsClient::new(nest);
            let subscriber = fauna_core::identity::ActorKeypair::from_secret(subscriber_secret);
            subs.subscribe_publishing_ek(author, tier, &subscriber)
                .await
                .map_err(|e| e.to_string())
        },
        move |res| {
            btn.set_sensitive(true);
            match res {
                Ok(SubscribeReply::Approved { .. }) => {
                    status_label.set_text(
                        &offer_status_label(OfferStatus::Active)
                            .resolve(crate::i18n::strings::lookup),
                    );
                    refresh_offers(&ctx_render);
                }
                Ok(SubscribeReply::Queued { .. }) => {
                    status_label.set_text(
                        &offer_status_label(OfferStatus::Pending)
                            .resolve(crate::i18n::strings::lookup),
                    );
                }
                // An outcome a newer nest added: sent, state unknown — re-read
                // rather than show a guess.
                Ok(SubscribeReply::Unknown) => refresh_offers(&ctx_render),
                Err(msg) => show_error(&ctx_render, &msg),
            }
        },
    );
}

fn show_error(ctx: &Rc<Ctx>, msg: &str) {
    ctx.w.error_label.add_css_class("error");
    crate::settings::render_error_label(&ctx.w.error_label, Some(msg));
}
