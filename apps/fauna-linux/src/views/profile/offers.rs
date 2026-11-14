//! The profile Tiers-tab **OTHER** (subscriber-browse) section — subscriptions
//! Slice B item 7 (`docs/goal/behavior/monetization.md` § Pillar 1, surface 2;
//! `docs/goal/ui/profile.md` § Layout & flow → *Another's profile (subscriber
//! browse)*). When viewing someone else's profile, the Tiers tab shows the
//! creator's offered tiers (`subscription-offers-section` / `subscription-offer-list`)
//! — per-row name / price / description / external payment link / Subscribe +
//! the viewer's status (none / pending / active).
//!
//! Reads over shared Rust (priority #2):
//! `SubscriptionsClient::offers_list_across_nests(author, home)` for the offered
//! tiers — read from the author's home through this nest when the page's
//! actor-home slot names another nest (`subscriptions.md` § Pillar 1 → *The
//! offers browse across nests*) — + `status_get(author)` on this nest for the
//! viewer's current status; `subscribe_across_nests(author, tier, home)` on the
//! per-row button. Observer-free: a manual
//! re-read after each subscribe (no client-side caching — `feed.md`
//! § Architectural rules). The free "followers" tier is followed via the header
//! `profile-follow-button` (`mod.rs`), so it is excluded from the per-row offers.
//!
//! After the offer rows sits the redeem pair (`subscription-claim-redeem-{input,
//! button}` — `profile.md` § Layout & flow → *Another's profile*;
//! `monetization.md` § Pillar 3 → *Which nest an in-app redeem binds on* →
//! *The doors*): the `subscription-settings` pair, here naming THIS creator's
//! home — the slot the follow and the per-row Subscribe read — so a code minted
//! on another nest redeems from the creator's profile over
//! `PaymentsClient::claims_redeem_across_nests`. It paints with no offer rows
//! too, and the page is rebuilt per open, so a code typed for one creator never
//! rides to the next.

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
    /// The redeem pair's code buffer — a local buffer, cleared only when the
    /// nest takes the code (a refused paste stays to correct). Excises with the
    /// `payments` plane, like the settings pair.
    #[cfg(feature = "payments")]
    redeem_input: gtk::Entry,
    /// Shared page-level `error-message`, owned by the profile view (Rule 2).
    error_label: gtk::Label,
}

struct Ctx {
    client: Rc<FaunaClient>,
    author: ActorId,
    /// The page's actor-home slot (`mod.rs`'s `HomeSlot`), read at each press
    /// as the header follow button reads it: `None` = this nest, else the
    /// author's home, where the subscribe goes across nests
    /// (`federated-follow.md` § Decision 2).
    home: Rc<RefCell<Option<String>>>,
    /// The viewer's currently-held tier name for this author (the highest-rank
    /// confirmed subscription `status.get` returns), used to render each row's
    /// status. Refreshed alongside the offer list.
    status_tier: RefCell<Option<String>>,
    /// The home the last offers load read under (`None` until the first load).
    /// The header read may replace the slot after that load ran; [`reload_if_moved`]
    /// then reads again, and an answer read under a home the slot has left is
    /// dropped — tui's `offers_home` / `offers_reload_op`.
    offers_home: RefCell<Option<Option<String>>>,
    /// A transient post-click `Queued` flag for one tier (an encrypted-mode
    /// subscribe, or a redeemed code whose grant queued for the creator's
    /// client), shown as pending until `status.get` names the tier.
    pending_tier: RefCell<Option<String>>,
    w: Widgets,
}

/// The offers tab: its page widget, its `refresh` and its `home_moved` entry points.
type OffersTab = (gtk::Box, Rc<dyn Fn()>, Rc<dyn Fn()>);

/// Build the "tiers" inner-stack page for **another** actor's profile — the
/// subscriber-browse offers section. The page-level `error-message` label is
/// owned by the profile view and shared in (so subscribe failures surface
/// there). Returns the page widget, a `refresh` entry point the profile view
/// wires to its on-visible hook (`app.rs`), mirroring the SELF Tiers tab, and a
/// `home_moved` entry point the header read calls after it writes the slot.
pub fn build_offers_tab(
    client: &Rc<FaunaClient>,
    error_label: gtk::Label,
    author: ActorId,
    home: Rc<RefCell<Option<String>>>,
) -> OffersTab {
    let body = build_offers_body();

    let ctx = Rc::new(Ctx {
        client: Rc::clone(client),
        author,
        home,
        status_tier: RefCell::new(None),
        offers_home: RefCell::new(None),
        pending_tier: RefCell::new(None),
        w: Widgets {
            offer_list: body.offer_list,
            offer_placeholder: body.offer_placeholder,
            offer_rows: RefCell::new(Vec::new()),
            #[cfg(feature = "payments")]
            redeem_input: body.redeem.input,
            error_label,
        },
    });

    #[cfg(feature = "payments")]
    {
        let ctx = Rc::clone(&ctx);
        body.redeem
            .button
            .connect_clicked(move |btn| redeem_claim(&ctx, btn));
    }

    let outer = body.outer;
    let refresh: Rc<dyn Fn()> = {
        let ctx = Rc::clone(&ctx);
        Rc::new(move || refresh_offers(&ctx))
    };
    let home_moved: Rc<dyn Fn()> = {
        let ctx = Rc::clone(&ctx);
        Rc::new(move || reload_if_moved(&ctx))
    };
    refresh();
    (outer, refresh, home_moved)
}

/// The OTHER Tiers tab's widget tree, built without a client (so a unit test
/// can read which ids it paints): the offers section, then the redeem pair.
struct OffersBody {
    outer: gtk::Box,
    offer_list: gtk::Box,
    offer_placeholder: gtk::Label,
    #[cfg(feature = "payments")]
    redeem: RedeemPair,
}

fn build_offers_body() -> OffersBody {
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

    // Gated on the RENDER, as the settings pair is: criterion 1 of
    // `dynamic-features.md` § What "completely compiled away" means greps the
    // artifact for ELEMENT IDS.
    #[cfg(feature = "payments")]
    let redeem = {
        let pair = build_redeem_pair();
        outer.append(&pair.section);
        pair
    };

    OffersBody {
        outer,
        offer_list,
        offer_placeholder,
        #[cfg(feature = "payments")]
        redeem,
    }
}

/// The redeem pair — the same ids, strings and wire-kind declaration as the
/// `subscription-settings` page's (`settings/subscriptions.rs`).
#[cfg(feature = "payments")]
struct RedeemPair {
    section: gtk::Box,
    input: gtk::Entry,
    button: gtk::Button,
}

#[cfg(feature = "payments")]
fn build_redeem_pair() -> RedeemPair {
    let section = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(8)
        .accessible_role(gtk::AccessibleRole::Group)
        .build();
    let title = gtk::Label::new(Some(s::REDEEM_CLAIM_TITLE));
    title.set_halign(gtk::Align::Start);
    title.add_css_class("title-4");
    section.append(&title);
    let row = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .build();
    let input = gtk::Entry::builder()
        .placeholder_text(s::CLAIM_CODE)
        .hexpand(true)
        .build();
    set_test_id(&input, ids::SUBSCRIPTION_CLAIM_REDEEM_INPUT);
    row.append(&input);
    let button = gtk::Button::with_label(s::REDEEM);
    button.add_css_class("suggested-action");
    set_test_id(&button, ids::SUBSCRIPTION_CLAIM_REDEEM_BUTTON);
    crate::offline_gate::declare_wire_kind(&button, "fauna.payments.claims.redeem");
    row.append(&button);
    section.append(&row);
    RedeemPair {
        section,
        input,
        button,
    }
}

/// One press of the redeem button, resolved from the page: the trimmed code,
/// THIS creator's id and the home the follow would carry.
#[cfg(feature = "payments")]
#[derive(Debug, Clone, PartialEq, Eq)]
struct RedeemRequest {
    code: String,
    author: ActorId,
    home_nest_url: Option<String>,
}

/// A blank (trimmed) buffer is a no-op, never a nest round-trip (the settings
/// pair's rule). The home is read at press time through the follow's own read.
#[cfg(feature = "payments")]
fn redeem_request(
    typed: &str,
    author: ActorId,
    home: &RefCell<Option<String>>,
) -> Option<RedeemRequest> {
    let code = typed.trim();
    if code.is_empty() {
        return None;
    }
    Some(RedeemRequest {
        code: code.to_string(),
        author,
        home_nest_url: super::press_home(home),
    })
}

/// `subscription-claim-redeem-button` — redeem the typed code of THIS creator
/// at the page's actor home (`PaymentsClient::claims_redeem_across_nests`),
/// then re-read offers + status so the tier reads the nest's answer.
#[cfg(feature = "payments")]
fn redeem_claim(ctx: &Rc<Ctx>, btn: &gtk::Button) {
    let Some(req) = redeem_request(&ctx.w.redeem_input.text(), ctx.author, &ctx.home) else {
        return;
    };
    btn.set_sensitive(false);
    let nest = ctx.client.nest_rpc().clone();
    let redeemer_secret = ctx.client.secret_bytes();
    let ctx_render = Rc::clone(ctx);
    let btn = btn.clone();
    spawn_with_snapshot(
        &ctx.client.runtime_handle(),
        move || async move {
            let redeemer = fauna_core::identity::ActorKeypair::from_secret(redeemer_secret);
            // The typed refusals (`claim_not_found`, `claim_voided`,
            // `claim_admits_elsewhere`, `peer_nest_outdated`, …) render as the
            // error's localized sentence, as the settings pair's do.
            fauna_client_payments::PaymentsClient::new(nest)
                .claims_redeem_across_nests(req.code, &redeemer, req.author, req.home_nest_url)
                .await
                .map(|reply| (reply.tier, reply.queued))
                .map_err(|e| e.to_string())
        },
        move |res| {
            btn.set_sensitive(true);
            if apply_redeem_outcome(
                &ctx_render.w.redeem_input,
                &ctx_render.pending_tier,
                &ctx_render.w.error_label,
                res,
            ) {
                refresh_offers(&ctx_render);
            }
        },
    );
}

/// Fold a redeem's answer: a taken code leaves the input, the tier reads
/// pending while the grant is queued for the creator's client, and the error
/// clears; a refusal keeps the code to correct and surfaces on
/// `error-message`. Returns whether to re-read offers + status.
#[cfg(feature = "payments")]
fn apply_redeem_outcome(
    input: &gtk::Entry,
    pending_tier: &RefCell<Option<String>>,
    error_label: &gtk::Label,
    res: Result<(String, bool), String>,
) -> bool {
    match res {
        Ok((tier, queued)) => {
            input.set_text("");
            *pending_tier.borrow_mut() = queued.then_some(tier);
            crate::settings::render_error_label(error_label, None);
            true
        }
        Err(msg) => {
            error_label.add_css_class("error");
            crate::settings::render_error_label(error_label, Some(&msg));
            false
        }
    }
}

/// The offers re-read a moved actor-home slot owes: the last load read under
/// one home and the header read has since replaced it, so the tiers listed are
/// the wrong nest's (or none). A no-op when nothing moved.
fn reload_if_moved(ctx: &Rc<Ctx>) {
    let moved = match &*ctx.offers_home.borrow() {
        Some(loaded) => *loaded != *ctx.home.borrow(),
        None => false,
    };
    if moved {
        refresh_offers(ctx);
    }
}

struct Snapshot {
    /// The home this load read under.
    home: Option<String>,
    offers: Result<Vec<TierItem>, String>,
    status: Result<Option<String>, String>,
}

/// Read the author's offered tiers + the viewer's status, then re-render. Runs
/// on the tokio runtime; result applied on the GTK thread (mirrors the SELF
/// Tiers tab `refresh_all`).
fn refresh_offers(ctx: &Rc<Ctx>) {
    let nest = ctx.client.nest_rpc().clone();
    let author = ctx.author;
    let home = ctx.home.borrow().clone();
    *ctx.offers_home.borrow_mut() = Some(home.clone());
    let ctx_render = Rc::clone(ctx);
    spawn_with_snapshot(
        &ctx.client.runtime_handle(),
        move || async move {
            let subs = SubscriptionsClient::new(nest);
            let offers = subs
                .offers_list_across_nests(author, home.clone())
                .await
                .map_err(|e| e.to_string());
            let status = subs
                .status_get(author)
                .await
                .map(|r| r.tier)
                .map_err(|e| e.to_string());
            Snapshot {
                home,
                offers,
                status,
            }
        },
        move |snap| apply_snapshot(&ctx_render, snap),
    );
}

fn apply_snapshot(ctx: &Rc<Ctx>, snap: Snapshot) {
    // Read under a home the slot has since left: the re-read the move started
    // answers instead.
    if snap.home != *ctx.home.borrow() {
        return;
    }
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
/// Active>Pending>None). `status.get` only reports the confirmed tier, so the
/// pending half is the page's transient [`Ctx::pending_tier`] — set by a queued
/// subscribe or a queued claim redeem — until the confirmed tier outranks it.
/// Shared with the other apps so the status→label map can't drift
/// (priority #2/#4).
fn status_text(ctx: &Rc<Ctx>, tier_name: &str) -> String {
    let held = ctx.status_tier.borrow();
    let pending = ctx.pending_tier.borrow().as_deref() == Some(tier_name);
    let status = offer_status(tier_name, held.as_deref(), pending);
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

    // The price display and the payment link below are the money plane's
    // (`dynamic-features.md` § Platform-family surface excision → *The
    // price-and-route class*); the subscribe button is not.
    #[cfg(feature = "payments")]
    {
        let price = gtk::Label::new(tier.price_hint.as_deref());
        price.set_halign(gtk::Align::Start);
        price.add_css_class("dim-label");
        set_test_id(&price, ids::SUBSCRIPTION_OFFER_PRICE);
        info.append(&price);
    }

    let description = gtk::Label::new(tier.description.as_deref());
    description.set_halign(gtk::Align::Start);
    description.add_css_class("dim-label");
    description.set_wrap(true);
    set_test_id(&description, ids::SUBSCRIPTION_OFFER_DESCRIPTION);
    info.append(&description);

    row.append(&info);

    // The viewer's status, built here so the payment link below can show its
    // subscribe's outcome on the row too; appended after the link.
    let status_str = status_text(ctx, &tier.name);
    let status = gtk::Label::new(Some(status_str.as_str()));
    status.set_valign(gtk::Align::Center);
    status.add_css_class("dim-label");
    set_test_id(&status, ids::SUBSCRIPTION_OFFER_STATUS);

    // External payment link (only when the tier carries a checkout URL).
    #[cfg(feature = "payments")]
    if let Some(url) = tier.payment_url.clone() {
        let pay = gtk::Button::with_label(s::PAYMENT_URL);
        pay.set_valign(gtk::Align::Center);
        set_test_id(&pay, ids::SUBSCRIPTION_OFFER_PAYMENT_LINK);
        crate::offline_gate::declare_wire_kind(&pay, "fauna.subscriptions.subscribe");
        let ctx_click = Rc::clone(ctx);
        let tier_name = tier.name.clone();
        let status_label = status.clone();
        pay.connect_clicked(move |btn| {
            // The payment link is nest/author-supplied; refuse to open a
            // non-https scheme via the shared `fauna_core::subscription::
            // is_safe_payment_url` guard (F-CL2 anti-phishing-redirect class — the same check `views/feed/post_list.rs`'s
            // `gated-post-payment-link` applies). Nothing is sent then.
            if !fauna_core::subscription::is_safe_payment_url(&url) {
                show_error(&ctx_click, s::UNSAFE_PAYMENT_URL);
                return;
            }
            pay_to(&ctx_click, &tier_name, &url, &status_label, btn);
        });
        row.append(&pay);
    }

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

/// Subscribe the viewer to `tier_name` (the per-row Subscribe button), across
/// nests at the page's home as the header follow button does
/// (`SubscriptionsClient::subscribe_across_nests`; the plain local call when
/// the author is homed here). Plaintext / auto-approve tiers resolve `Approved`
/// inline; encrypted mode resolves `Queued` → the row flips to "pending"
/// (`monetization.md` § Pillar 1 — Flows).
fn subscribe_to(ctx: &Rc<Ctx>, tier_name: &str, status_label: &gtk::Label, btn: &gtk::Button) {
    btn.set_sensitive(false);
    let nest = ctx.client.nest_rpc().clone();
    let author = ctx.author;
    let tier = tier_name.to_string();
    let tier_reply = tier.clone();
    // The viewer's own identity seed → the subscriber keypair whose ML-KEM ek
    // gets published unconditionally (surface B, S4b).
    let subscriber_secret = ctx.client.secret_bytes();
    let home = ctx.home.borrow().clone();
    let ctx_render = Rc::clone(ctx);
    let status_label = status_label.clone();
    let btn = btn.clone();
    spawn_with_snapshot(
        &ctx.client.runtime_handle(),
        move || async move {
            let subs = SubscriptionsClient::new(nest);
            let subscriber = fauna_core::identity::ActorKeypair::from_secret(subscriber_secret);
            subs.subscribe_across_nests(author, tier, &subscriber, home)
                .await
                .map_err(|e| e.to_string())
        },
        move |res| {
            btn.set_sensitive(true);
            match res {
                Ok(reply) => render_reply(&ctx_render, &tier_reply, &status_label, &reply),
                Err(msg) => show_error(&ctx_render, &msg),
            }
        },
    );
}

/// The payment link (`subscription-offer-payment-link`): the subscribe to the
/// tier the row sells goes first, across nests at the page's home, then the
/// checkout URL the shared reference rule composes is opened
/// (`SubscriptionsClient::subscribe_and_checkout`; `monetization.md` § Pillar 3
/// → *Buyer↔actor binding (Q4)*, the Q13 rules (1) and (2); tui's
/// `Action::OpenPaymentLink`). The row shows the subscribe's outcome as the
/// Subscribe button would; a refused subscribe is the page error and opens
/// nothing; `None` (a URL the shared guard refuses) opens nothing.
#[cfg(feature = "payments")]
fn pay_to(ctx: &Rc<Ctx>, tier_name: &str, url: &str, status_label: &gtk::Label, btn: &gtk::Button) {
    btn.set_sensitive(false);
    let nest = ctx.client.nest_rpc().clone();
    let author = ctx.author;
    let tier = tier_name.to_string();
    let tier_reply = tier.clone();
    let url = url.to_string();
    let subscriber_secret = ctx.client.secret_bytes();
    let home = ctx.home.borrow().clone();
    let ctx_render = Rc::clone(ctx);
    let status_label = status_label.clone();
    let btn = btn.clone();
    spawn_with_snapshot(
        &ctx.client.runtime_handle(),
        move || async move {
            let subs = SubscriptionsClient::new(nest);
            let subscriber = fauna_core::identity::ActorKeypair::from_secret(subscriber_secret);
            subs.subscribe_and_checkout(author, tier, &url, &subscriber, home)
                .await
                .map_err(|e| e.to_string())
        },
        move |res| {
            btn.set_sensitive(true);
            match res {
                Ok(Some(open)) => {
                    render_reply(&ctx_render, &tier_reply, &status_label, &open.reply);
                    crate::url_opener::open(&open.url);
                }
                Ok(None) => {}
                Err(msg) => show_error(&ctx_render, &msg),
            }
        },
    );
}

/// A subscribe's reply on its row: `Approved` → active (and a re-read),
/// `Queued` → the optimistic pending transient, an outcome a newer nest added
/// → a re-read rather than a guess.
fn render_reply(ctx: &Rc<Ctx>, tier_name: &str, status_label: &gtk::Label, reply: &SubscribeReply) {
    match reply {
        SubscribeReply::Approved { .. } => {
            status_label.set_text(
                &offer_status_label(OfferStatus::Active).resolve(crate::i18n::strings::lookup),
            );
            refresh_offers(ctx);
        }
        SubscribeReply::Queued { .. } => {
            *ctx.pending_tier.borrow_mut() = Some(tier_name.to_string());
            status_label.set_text(
                &offer_status_label(OfferStatus::Pending).resolve(crate::i18n::strings::lookup),
            );
        }
        SubscribeReply::Unknown => refresh_offers(ctx),
    }
}

fn show_error(ctx: &Rc<Ctx>, msg: &str) {
    ctx.w.error_label.add_css_class("error");
    crate::settings::render_error_label(&ctx.w.error_label, Some(msg));
}

#[cfg(all(test, feature = "payments"))]
mod tests {
    use super::*;

    fn count(root: &gtk::Box, id: &str) -> usize {
        crate::automation::find::count_in(root.upcast_ref(), id)
    }

    /// The OTHER Tiers tab carries the redeem pair even with no offer rows;
    /// the SELF Tiers tab is
    /// `tiers::build_tiers_tab`, which never builds it (tui's
    /// `the_redeem_row_paints_on_the_other_tiers_tab_only`).
    #[test]
    fn the_redeem_row_paints_on_the_other_tiers_tab_only() {
        crate::testid::run_on_gtk_thread(|| {
            let body = build_offers_body();
            assert!(body.offer_placeholder.is_visible(), "no offer rows");
            assert_eq!(count(&body.outer, ids::SUBSCRIPTION_CLAIM_REDEEM_INPUT), 1);
            assert_eq!(count(&body.outer, ids::SUBSCRIPTION_CLAIM_REDEEM_BUTTON), 1);
        });
        // The SELF tab's builder has no redeem pair to paint.
        let self_tab = include_str!("tiers.rs");
        assert!(!self_tab.contains("SUBSCRIPTION_CLAIM_REDEEM_"));
    }

    /// The redeem names the page's home exactly as the follow does, with the
    /// trimmed code, under this creator's id; a blank buffer sends nothing.
    #[test]
    fn the_redeem_goes_to_the_viewed_creators_home() {
        let author = ActorId([7u8; 32]);
        let home = RefCell::new(Some("https://author-home.test".to_string()));
        assert_eq!(redeem_request("   ", author, &home), None);
        let req = redeem_request("  CODE42 ", author, &home).expect("a typed code redeems");
        assert_eq!(req.author, author);
        assert_eq!(req.code, "CODE42");
        assert_eq!(req.home_nest_url, super::super::press_home(&home));
        assert_eq!(
            req.home_nest_url.as_deref(),
            Some("https://author-home.test")
        );
        // A creator homed here: the plain local redeem.
        let here = RefCell::new(None);
        assert_eq!(
            redeem_request("CODE42", author, &here)
                .unwrap()
                .home_nest_url,
            None
        );
    }

    /// A taken code leaves the input and the tier reads pending until the
    /// creator's client commits the grant; a refusal keeps the code to
    /// correct; a fresh open (the page is rebuilt per open) starts empty.
    #[test]
    fn a_redeemed_code_clears_the_input_and_marks_the_tier_pending() {
        crate::testid::run_on_gtk_thread(|| {
            let body = build_offers_body();
            let input = &body.redeem.input;
            let pending = RefCell::new(None);
            let error = gtk::Label::new(None);
            input.set_text("CODE42");

            let refresh = apply_redeem_outcome(
                input,
                &pending,
                &error,
                Err("This code was not found.".into()),
            );
            assert!(!refresh);
            assert_eq!(input.text(), "CODE42");
            assert!(error.is_visible());
            assert_eq!(error.text(), "This code was not found.");

            let refresh = apply_redeem_outcome(input, &pending, &error, Ok(("gold".into(), true)));
            assert!(refresh);
            assert_eq!(input.text(), "");
            assert_eq!(pending.borrow().as_deref(), Some("gold"));
            assert!(!error.is_visible());

            input.set_text("OTHER");
            let next_open = build_offers_body();
            assert_eq!(next_open.redeem.input.text(), "");
        });
    }
}
