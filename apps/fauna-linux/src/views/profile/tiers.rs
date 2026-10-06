//! The profile Tiers-tab **SELF** author-management sections (subscriptions
//! Slice A) — `docs/goal/behavior/monetization.md` § Pillar 1 + `profile.md`
//! § Layout & flow. Three stacked sections render over the shared Rust:
//!
//! - **§1 My tiers** (`subscription-tiers-section`): the author's own tier
//!   definitions (`SubscriptionsClient::tiers_list`) with create/edit
//!   (`SubscriptionsAuthor::create_tier` / `tiers_update`) + delete
//!   (`tiers_delete`).
//! - **§2 Pending requests** (`subscription-requests-section`): the author's
//!   pending subscribe/unsubscribe requests (`requests_list`); approve runs the
//!   transparent mint+upload (`SubscriptionsAuthor::approve_subscriber`, showing
//!   `subscription-request-busy`); reject is the thin `requests_reject`.
//! - **§3 Subscribers** (`subscription-subscribers-section`): the selected
//!   tier's roster (`subscribers_list`); remove rotates+re-mints
//!   (`SubscriptionsAuthor::remove_subscriber`).
//! - **§4 Payment providers** (`subscription-provider-section` —
//!   `monetization.md` § Pillar 3, IDs reserved 2026-07-12): the author's
//!   configured payment providers (`PaymentsClient::providers_list`) with an
//!   add/edit form (kind from the shared `fauna-payments` registry, the
//!   webhook-verification secret — never echoed back by the nest, so never
//!   pre-filled — and the entitled tier from the author's own tiers) over
//!   `providers_set` / `providers_remove`.
//!
//! Encrypted-mode only at the crypto layer; the UI drives the same calls in
//! both modes (plaintext mints nest-side). Observer-free: a manual re-read after
//! each mutation (no client-side caching — `feed.md` § Architectural rules).

use fauna_ui_ids as ids;
use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;

#[cfg(feature = "payments")]
use fauna_client_payments::PaymentsClient;
#[cfg(feature = "payments")]
use fauna_client_payments::payments::{ClaimItem, ProviderItem};
use fauna_client_subscriptions::SubscriptionsClient;
use fauna_client_subscriptions::orchestration::SubscriptionsAuthor;
use fauna_client_subscriptions::subscriptions::{PendingRequest, TierItem};
use fauna_core::identity::{ActorId, ActorKeypair};

use crate::async_helper::spawn_with_snapshot;
use crate::client::FaunaClient;
// Only the §4 provider form's webhook-URL copy button uses this.
#[cfg(feature = "payments")]
use crate::clipboard;
use crate::i18n::strings::subscriptions as s;
use crate::testid::set_test_id;

/// Widget handles the render + event closures need.
struct Widgets {
    // §1 My tiers
    create_button: gtk::Button,
    form: gtk::Box,
    form_name: gtk::Entry,
    form_rank: gtk::Entry,
    form_description: gtk::Entry,
    form_price_hint: gtk::Entry,
    form_asking_price: gtk::Entry,
    form_payment_url: gtk::Entry,
    form_auto_approve: gtk::Switch,
    form_save: gtk::Button,
    form_cancel: gtk::Button,
    tier_list: gtk::Box,
    tier_placeholder: gtk::Label,
    tier_rows: RefCell<Vec<gtk::Box>>,
    // §2 Pending requests
    request_busy: gtk::Label,
    request_list: gtk::Box,
    request_placeholder: gtk::Label,
    request_rows: RefCell<Vec<gtk::Box>>,
    // §3 Subscribers
    tier_select: gtk::DropDown,
    subscriber_list: gtk::Box,
    subscriber_placeholder: gtk::Label,
    subscriber_rows: RefCell<Vec<gtk::Box>>,
    // ── §§4–5, the money plane — every handle `payments`-gated ──────────
    // Gated as the RENDER, not merely as the calls they drive: criterion 1 of
    // `dynamic-features.md` § What "completely compiled away" means is a
    // `strings`-grep for element ids, and a section that merely never loads
    // still ships every id it would have painted. Criterion 5 ("no re-enable
    // path") is the other half: a widget a store-safe build can still click is
    // one.
    // §4 Payment providers
    #[cfg(feature = "payments")]
    provider_add_button: gtk::Button,
    #[cfg(feature = "payments")]
    provider_form: gtk::Box,
    #[cfg(feature = "payments")]
    provider_form_kind: gtk::DropDown,
    #[cfg(feature = "payments")]
    provider_form_secret: gtk::Entry,
    #[cfg(feature = "payments")]
    provider_form_tier: gtk::DropDown,
    #[cfg(feature = "payments")]
    provider_form_webhook_url: gtk::Entry,
    #[cfg(feature = "payments")]
    provider_form_save: gtk::Button,
    #[cfg(feature = "payments")]
    provider_form_cancel: gtk::Button,
    #[cfg(feature = "payments")]
    provider_list: gtk::Box,
    #[cfg(feature = "payments")]
    provider_placeholder: gtk::Label,
    #[cfg(feature = "payments")]
    provider_rows: RefCell<Vec<gtk::Box>>,
    // §5 Manual claim codes
    #[cfg(feature = "payments")]
    claim_tier_select: gtk::DropDown,
    #[cfg(feature = "payments")]
    claim_mint_button: gtk::Button,
    #[cfg(feature = "payments")]
    claim_list: gtk::Box,
    #[cfg(feature = "payments")]
    claim_placeholder: gtk::Label,
    #[cfg(feature = "payments")]
    claim_rows: RefCell<Vec<gtk::Box>>,
    // shared page-level error (owned by the profile view, Rule 2)
    error_label: gtk::Label,
}

/// Everything the handlers + render need.
struct Ctx {
    client: Rc<FaunaClient>,
    rt: tokio::runtime::Handle,
    /// `Some(tier_name)` while the form edits an existing tier; `None` while
    /// creating.
    editing: RefCell<Option<String>>,
    /// The current tier names, index-aligned with the `tier_select` dropdown
    /// model, so a selection maps back to a tier name for the §3 roster read.
    tier_names: RefCell<Vec<String>>,
    /// The current tier names, index-aligned with the §4 provider form's
    /// tier-map dropdown model (refreshed with the same `tiers_list` read).
    #[cfg(feature = "payments")]
    provider_tier_names: RefCell<Vec<String>>,
    /// The current tier names, index-aligned with the §5 claim-mint tier
    /// select's dropdown model (refreshed with the same `tiers_list` read).
    #[cfg(feature = "payments")]
    claim_tier_names: RefCell<Vec<String>>,
    w: Widgets,
}

/// Build the "tiers" inner-stack page — the SELF author-management surface. The
/// page-level `error-message` label is owned by the profile view and shared in.
///
/// Returns the page widget plus a `refresh` entry point the profile view wires
/// to its on-visible hook (`app.rs`): the page is observer-free, so it must
/// re-read §1/§2/§3 when the profile page becomes visible — otherwise a pending
/// subscribe request that arrived while the author was on another page would not
/// appear until a local tier mutation. Mirrors the Peers page's on-visible
/// `DevicesMachine::refresh`.
pub fn build_tiers_tab(
    client: &Rc<FaunaClient>,
    error_label: gtk::Label,
) -> (gtk::Box, Rc<dyn Fn()>) {
    let outer = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(16)
        .margin_top(12)
        .margin_bottom(12)
        .margin_start(12)
        .margin_end(12)
        .build();

    // ── §1 My tiers ─────────────────────────────────────────────────────
    let tiers_section = section_box("subscription-tiers-section", s::MY_TIERS);

    let create_button = gtk::Button::with_label(s::CREATE_TIER);
    create_button.add_css_class("suggested-action");
    create_button.set_halign(gtk::Align::Start);
    set_test_id(&create_button, ids::SUBSCRIPTION_TIER_CREATE_BUTTON);
    tiers_section.append(&create_button);

    let (
        form,
        form_name,
        form_rank,
        form_description,
        form_price_hint,
        form_asking_price,
        form_payment_url,
        form_auto_approve,
        form_save,
        form_cancel,
    ) = build_form();
    tiers_section.append(&form);

    let tier_list = list_box();
    let tier_placeholder = placeholder(s::NO_TIERS);
    tier_list.append(&tier_placeholder);
    tiers_section.append(&tier_list);
    outer.append(&tiers_section);

    // ── §2 Pending requests ─────────────────────────────────────────────
    let requests_section = section_box("subscription-requests-section", s::PENDING_REQUESTS);
    let request_busy = gtk::Label::builder().visible(false).build();
    request_busy.set_halign(gtk::Align::Start);
    request_busy.add_css_class("dim-label");
    request_busy.set_text(s::APPROVING);
    set_test_id(&request_busy, ids::SUBSCRIPTION_REQUEST_BUSY);
    requests_section.append(&request_busy);
    let request_list = list_box();
    let request_placeholder = placeholder(s::NO_REQUESTS);
    request_list.append(&request_placeholder);
    requests_section.append(&request_list);
    outer.append(&requests_section);

    // ── §3 Subscribers roster ───────────────────────────────────────────
    let subscribers_section = section_box("subscription-subscribers-section", s::SUBSCRIBERS);
    let select_row = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .build();
    let select_label = gtk::Label::new(Some(s::TIER_SELECT_LABEL));
    select_row.append(&select_label);
    let tier_select = gtk::DropDown::from_strings(&[]);
    set_test_id(&tier_select, ids::SUBSCRIPTION_SUBSCRIBERS_TIER_SELECT);
    select_row.append(&tier_select);
    subscribers_section.append(&select_row);
    let subscriber_list = list_box();
    let subscriber_placeholder = placeholder(s::NO_SUBSCRIBERS);
    subscriber_list.append(&subscriber_placeholder);
    subscribers_section.append(&subscriber_list);
    outer.append(&subscribers_section);

    // ── §4 Payment providers + §5 Manual claim codes ────────────────────
    // The money plane's two sections. Built inside one `payments`-gated block
    // so a store-safe build emits neither their widgets nor their element ids
    // (`dynamic-features.md` § Platform-family surface excision). §§1–3 above
    // are Pillar-1 subscriptions and stay in both flavors.
    #[cfg(feature = "payments")]
    let (
        provider_add_button,
        provider_form,
        provider_form_kind,
        provider_form_secret,
        provider_form_tier,
        provider_form_webhook_url,
        provider_form_save,
        provider_form_cancel,
        provider_list,
        provider_placeholder,
        claim_tier_select,
        claim_mint_button,
        claim_list,
        claim_placeholder,
    ) = {
        let providers_section = section_box("subscription-provider-section", s::PAYMENT_PROVIDERS);

        let provider_add_button = gtk::Button::with_label(s::ADD_PROVIDER);
        provider_add_button.add_css_class("suggested-action");
        provider_add_button.set_halign(gtk::Align::Start);
        set_test_id(&provider_add_button, ids::SUBSCRIPTION_PROVIDER_ADD_BUTTON);
        providers_section.append(&provider_add_button);

        let (
            provider_form,
            provider_form_kind,
            provider_form_secret,
            provider_form_tier,
            provider_form_webhook_url,
            provider_form_save,
            provider_form_cancel,
        ) = build_provider_form();
        providers_section.append(&provider_form);

        let provider_list = list_box();
        let provider_placeholder = placeholder(s::NO_PROVIDERS);
        provider_list.append(&provider_placeholder);
        providers_section.append(&provider_list);
        outer.append(&providers_section);

        // ── §5 Manual claim codes ───────────────────────────────────────────
        let claims_section = section_box("subscription-claim-section", s::MANUAL_CLAIMS);

        let claim_mint_row = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(8)
            .build();
        claim_mint_row.append(&gtk::Label::new(Some(s::TIER_SELECT_LABEL)));
        let claim_tier_select = gtk::DropDown::from_strings(&[]);
        set_test_id(&claim_tier_select, ids::SUBSCRIPTION_CLAIM_TIER_SELECT);
        claim_mint_row.append(&claim_tier_select);
        let claim_mint_button = gtk::Button::with_label(s::MINT_CLAIM);
        claim_mint_button.add_css_class("suggested-action");
        set_test_id(&claim_mint_button, ids::SUBSCRIPTION_CLAIM_MINT_BUTTON);
        crate::offline_gate::declare_wire_kind(&claim_mint_button, "fauna.payments.claims.mint");
        claim_mint_row.append(&claim_mint_button);
        claims_section.append(&claim_mint_row);

        let claim_list = list_box();
        let claim_placeholder = placeholder(s::NO_CLAIMS);
        claim_list.append(&claim_placeholder);
        claims_section.append(&claim_list);
        outer.append(&claims_section);

        (
            provider_add_button,
            provider_form,
            provider_form_kind,
            provider_form_secret,
            provider_form_tier,
            provider_form_webhook_url,
            provider_form_save,
            provider_form_cancel,
            provider_list,
            provider_placeholder,
            claim_tier_select,
            claim_mint_button,
            claim_list,
            claim_placeholder,
        )
    };

    let widgets = Widgets {
        create_button,
        form,
        form_name,
        form_rank,
        form_description,
        form_price_hint,
        form_asking_price,
        form_payment_url,
        form_auto_approve,
        form_save,
        form_cancel,
        tier_list,
        tier_placeholder,
        tier_rows: RefCell::new(Vec::new()),
        request_busy,
        request_list,
        request_placeholder,
        request_rows: RefCell::new(Vec::new()),
        tier_select,
        subscriber_list,
        subscriber_placeholder,
        subscriber_rows: RefCell::new(Vec::new()),
        #[cfg(feature = "payments")]
        provider_add_button,
        #[cfg(feature = "payments")]
        provider_form,
        #[cfg(feature = "payments")]
        provider_form_kind,
        #[cfg(feature = "payments")]
        provider_form_secret,
        #[cfg(feature = "payments")]
        provider_form_tier,
        #[cfg(feature = "payments")]
        provider_form_webhook_url,
        #[cfg(feature = "payments")]
        provider_form_save,
        #[cfg(feature = "payments")]
        provider_form_cancel,
        #[cfg(feature = "payments")]
        provider_list,
        #[cfg(feature = "payments")]
        provider_placeholder,
        #[cfg(feature = "payments")]
        provider_rows: RefCell::new(Vec::new()),
        #[cfg(feature = "payments")]
        claim_tier_select,
        #[cfg(feature = "payments")]
        claim_mint_button,
        #[cfg(feature = "payments")]
        claim_list,
        #[cfg(feature = "payments")]
        claim_placeholder,
        #[cfg(feature = "payments")]
        claim_rows: RefCell::new(Vec::new()),
        error_label,
    };
    let ctx = wire(client, widgets);
    let refresh: Rc<dyn Fn()> = {
        let ctx = Rc::clone(&ctx);
        Rc::new(move || refresh_all(&ctx))
    };
    (outer, refresh)
}

/// A section container with a title heading, carrying its `view` test id.
/// Shared with the sibling OTHER-profile `offers` tab (`offers.rs`) so the two
/// subscription surfaces render with one idiom (priority #4).
pub(super) fn section_box(test_id: &str, title: &str) -> gtk::Box {
    let b = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(8)
        .accessible_role(gtk::AccessibleRole::Group)
        .build();
    set_test_id(&b, test_id);
    let label = gtk::Label::new(Some(title));
    label.set_halign(gtk::Align::Start);
    label.add_css_class("title-4");
    b.append(&label);
    b
}

/// A vertical `gtk::Box` row container (no `gtk::ListBox` auto-wrap, so rebuilds
/// via `remove` are clean).
pub(super) fn list_box() -> gtk::Box {
    gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(6)
        .accessible_role(gtk::AccessibleRole::Group)
        .build()
}

pub(super) fn placeholder(text: &str) -> gtk::Label {
    let l = gtk::Label::new(Some(text));
    l.set_halign(gtk::Align::Start);
    l.add_css_class("dim-label");
    l
}

/// Build the inline create/edit tier form (`subscription-tier-form`); the same
/// form serves create + edit (the `Ctx::editing` flag distinguishes them).
#[allow(clippy::type_complexity)]
fn build_form() -> (
    gtk::Box,
    gtk::Entry,
    gtk::Entry,
    gtk::Entry,
    gtk::Entry,
    gtk::Entry,
    gtk::Entry,
    gtk::Switch,
    gtk::Button,
    gtk::Button,
) {
    let form = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(6)
        .visible(false)
        .accessible_role(gtk::AccessibleRole::Group)
        .build();
    set_test_id(&form, ids::SUBSCRIPTION_TIER_FORM);

    let name = entry(s::TIER_NAME);
    set_test_id(&name, ids::SUBSCRIPTION_TIER_FORM_NAME);
    form.append(&name);
    let rank = entry(s::RANK);
    set_test_id(&rank, ids::SUBSCRIPTION_TIER_FORM_RANK);
    form.append(&rank);
    let description = entry(s::DESCRIPTION);
    set_test_id(&description, ids::SUBSCRIPTION_TIER_FORM_DESCRIPTION);
    form.append(&description);
    let price_hint = entry(s::PRICE_HINT);
    set_test_id(&price_hint, ids::SUBSCRIPTION_TIER_FORM_PRICE_HINT);
    form.append(&price_hint);
    // The machine-comparable price (`monetization.md` § The asking price) —
    // independent of `price_hint` above; no parsing ever infers one from the
    // other. Empty on create means unpriced; empty on an edit means "keep the
    // current price" (`fauna.subscriptions.tiers.update`'s merge rule).
    let asking_price = entry(s::ASKING_PRICE);
    set_test_id(&asking_price, ids::SUBSCRIPTION_TIER_FORM_ASKING_PRICE);
    form.append(&asking_price);
    let payment_url = entry(s::PAYMENT_URL);
    set_test_id(&payment_url, ids::SUBSCRIPTION_TIER_FORM_PAYMENT_URL);
    form.append(&payment_url);

    let auto_row = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .build();
    let auto_label = gtk::Label::new(Some(s::AUTO_APPROVE));
    auto_row.append(&auto_label);
    let auto_approve = gtk::Switch::new();
    auto_approve.set_halign(gtk::Align::Start);
    set_test_id(&auto_approve, ids::SUBSCRIPTION_TIER_FORM_AUTO_APPROVE);
    auto_row.append(&auto_approve);
    form.append(&auto_row);

    let buttons = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .halign(gtk::Align::End)
        .build();
    let cancel = gtk::Button::with_label(s::CANCEL);
    set_test_id(&cancel, ids::SUBSCRIPTION_TIER_FORM_CANCEL);
    let save = gtk::Button::with_label(s::SAVE);
    save.add_css_class("suggested-action");
    set_test_id(&save, ids::SUBSCRIPTION_TIER_FORM_SAVE);
    buttons.append(&cancel);
    buttons.append(&save);
    form.append(&buttons);

    (
        form,
        name,
        rank,
        description,
        price_hint,
        asking_price,
        payment_url,
        auto_approve,
        save,
        cancel,
    )
}

fn entry(placeholder: &str) -> gtk::Entry {
    gtk::Entry::builder()
        .placeholder_text(placeholder)
        .hexpand(true)
        .build()
}

/// Build the §4 provider add form (`subscription-provider-form`): kind select
/// (the shared `fauna-payments` registry), the webhook-verification secret
/// (masked, and never pre-filled — the nest deliberately omits it from the
/// list reply, so editing means re-entering it), the entitled tier
/// (single-select from the author's own tiers, first cut — one mapping per
/// provider), and a read-only webhook-URL preview (the exact URL to register
/// at the provider's dashboard) with a copy button — recomputed live as the
/// kind selection changes (see `wire`).
#[cfg(feature = "payments")]
#[allow(clippy::type_complexity)]
fn build_provider_form() -> (
    gtk::Box,
    gtk::DropDown,
    gtk::Entry,
    gtk::DropDown,
    gtk::Entry,
    gtk::Button,
    gtk::Button,
) {
    let form = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(6)
        .visible(false)
        .accessible_role(gtk::AccessibleRole::Group)
        .build();
    set_test_id(&form, ids::SUBSCRIPTION_PROVIDER_FORM);

    let kind_row = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .build();
    kind_row.append(&gtk::Label::new(Some(s::PROVIDER_KIND_LABEL)));
    let kinds: Vec<&str> = fauna_client_payments::known_kinds().to_vec();
    let kind = gtk::DropDown::from_strings(&kinds);
    set_test_id(&kind, ids::SUBSCRIPTION_PROVIDER_FORM_KIND);
    kind_row.append(&kind);
    form.append(&kind_row);

    let secret = entry(s::WEBHOOK_SECRET);
    secret.set_visibility(false);
    set_test_id(&secret, ids::SUBSCRIPTION_PROVIDER_FORM_SECRET);
    form.append(&secret);

    let tier_row = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .build();
    tier_row.append(&gtk::Label::new(Some(s::PROVIDER_TIER_LABEL)));
    let tier = gtk::DropDown::from_strings(&[]);
    set_test_id(&tier, ids::SUBSCRIPTION_PROVIDER_FORM_TIER_MAP);
    tier_row.append(&tier);
    form.append(&tier_row);

    let webhook_row = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .build();
    webhook_row.append(&gtk::Label::new(Some(s::WEBHOOK_URL_LABEL)));
    let webhook_url = gtk::Entry::builder().editable(false).hexpand(true).build();
    set_test_id(&webhook_url, ids::SUBSCRIPTION_PROVIDER_FORM_WEBHOOK_URL);
    webhook_row.append(&webhook_url);
    let webhook_copy = clipboard::copy_button_dynamic({
        let webhook_url = webhook_url.clone();
        move || webhook_url.text().to_string()
    });
    set_test_id(
        &webhook_copy,
        ids::SUBSCRIPTION_PROVIDER_FORM_WEBHOOK_URL_COPY_BUTTON,
    );
    webhook_row.append(&webhook_copy);
    form.append(&webhook_row);

    let buttons = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .halign(gtk::Align::End)
        .build();
    let cancel = gtk::Button::with_label(s::CANCEL);
    set_test_id(&cancel, ids::SUBSCRIPTION_PROVIDER_FORM_CANCEL);
    let save = gtk::Button::with_label(s::SAVE);
    save.add_css_class("suggested-action");
    set_test_id(&save, ids::SUBSCRIPTION_PROVIDER_FORM_SAVE);
    crate::offline_gate::declare_wire_kind(&save, "fauna.payments.providers.set");
    buttons.append(&cancel);
    buttons.append(&save);
    form.append(&buttons);

    (form, kind, secret, tier, webhook_url, save, cancel)
}

/// Wire the three sections to the shared calls, load + render on mount, and
/// connect every interaction. Returns the shared `Ctx` so the caller can build
/// the on-visible `refresh` entry point over it.
fn wire(client: &Rc<FaunaClient>, widgets: Widgets) -> Rc<Ctx> {
    let ctx = Rc::new(Ctx {
        client: Rc::clone(client),
        rt: client.runtime_handle(),
        editing: RefCell::new(None),
        tier_names: RefCell::new(Vec::new()),
        #[cfg(feature = "payments")]
        provider_tier_names: RefCell::new(Vec::new()),
        #[cfg(feature = "payments")]
        claim_tier_names: RefCell::new(Vec::new()),
        w: widgets,
    });

    refresh_all(&ctx);

    // §1: Create → open the form in create mode.
    {
        let ctx = Rc::clone(&ctx);
        ctx.w.create_button.clone().connect_clicked(move |_| {
            *ctx.editing.borrow_mut() = None;
            ctx.w.form_name.set_text("");
            ctx.w.form_name.set_sensitive(true);
            ctx.w.form_rank.set_text("");
            ctx.w.form_description.set_text("");
            ctx.w.form_price_hint.set_text("");
            ctx.w.form_asking_price.set_text("");
            ctx.w.form_payment_url.set_text("");
            ctx.w.form_auto_approve.set_active(false);
            clear_error(&ctx.w);
            ctx.w.form.set_visible(true);
        });
    }
    // §1: Cancel → hide the form.
    {
        let ctx = Rc::clone(&ctx);
        ctx.w.form_cancel.clone().connect_clicked(move |_| {
            *ctx.editing.borrow_mut() = None;
            ctx.w.form.set_visible(false);
        });
    }
    // §1: Save → create or update.
    {
        let ctx = Rc::clone(&ctx);
        ctx.w
            .form_save
            .clone()
            .connect_clicked(move |_| submit_form(&ctx));
    }

    // §3: tier selection → re-read that tier's roster.
    {
        let ctx = Rc::clone(&ctx);
        ctx.w
            .tier_select
            .clone()
            .connect_selected_notify(move |dd| {
                let idx = dd.selected() as usize;
                let name = ctx.tier_names.borrow().get(idx).cloned();
                if let Some(name) = name {
                    refresh_roster(&ctx, name);
                }
            });
    }

    // ── §§4–5, the money plane — every handler `payments`-gated ─────────
    // One gated block, mirroring the builder's: a store-safe build wires no
    // interaction into the money plane at all, which is criterion 5 of
    // `dynamic-features.md` § What "completely compiled away" means (no
    // re-enable path — a handler a store-safe build could still reach is one).
    #[cfg(feature = "payments")]
    {
        // §4: Add → open the provider form (the secret is never pre-filled —
        // the nest doesn't echo it back; a re-save re-enters it).
        {
            let ctx = Rc::clone(&ctx);
            ctx.w.provider_add_button.clone().connect_clicked(move |_| {
                ctx.w.provider_form_secret.set_text("");
                update_webhook_url_preview(&ctx);
                clear_error(&ctx.w);
                ctx.w.provider_form.set_visible(true);
            });
        }
        // §4: kind selection changes → recompute the webhook-URL preview (the
        // URL path segment is the selected kind).
        {
            let ctx = Rc::clone(&ctx);
            ctx.w
                .provider_form_kind
                .clone()
                .connect_selected_notify(move |_| {
                    update_webhook_url_preview(&ctx);
                });
        }
        // §4: Cancel → hide the form.
        {
            let ctx = Rc::clone(&ctx);
            ctx.w
                .provider_form_cancel
                .clone()
                .connect_clicked(move |_| {
                    ctx.w.provider_form.set_visible(false);
                });
        }
        // §4: Save → upsert the provider config.
        {
            let ctx = Rc::clone(&ctx);
            ctx.w
                .provider_form_save
                .clone()
                .connect_clicked(move |_| submit_provider_form(&ctx));
        }

        // §5: Mint → a claim code for the selected tier.
        {
            let ctx = Rc::clone(&ctx);
            ctx.w.claim_mint_button.clone().connect_clicked(move |_| {
                let idx = ctx.w.claim_tier_select.selected() as usize;
                let tier = ctx.claim_tier_names.borrow().get(idx).cloned();
                if let Some(tier) = tier {
                    dispatch(&ctx, Action::MintClaim { tier });
                }
            });
        }
    }

    ctx
}

/// Recompute + set the §4 form's webhook-URL preview from the current kind
/// selection — the exact URL the creator registers at their provider's
/// dashboard. The shape is owned by `fauna_payments::webhook_url`, which the
/// nest's ingress route is built from too; don't restate it here.
#[cfg(feature = "payments")]
fn update_webhook_url_preview(ctx: &Rc<Ctx>) {
    let kinds = fauna_client_payments::known_kinds();
    let kind = kinds
        .get(ctx.w.provider_form_kind.selected() as usize)
        .copied()
        .unwrap_or("");
    let url = fauna_client_payments::webhook_url(
        ctx.client.node_url(),
        &ctx.client.actor_id().unwrap_or_default(),
        kind,
    );
    ctx.w.provider_form_webhook_url.set_text(&url);
}

/// Read the §4 provider form and dispatch the upsert. No client-side
/// validation — the nest rejects unknown kinds / dangling tiers / empty
/// secrets with typed `fauna.payments.*` errors that surface via the shared
/// error label.
#[cfg(feature = "payments")]
fn submit_provider_form(ctx: &Rc<Ctx>) {
    let kinds = fauna_client_payments::known_kinds();
    let Some(kind) = kinds.get(ctx.w.provider_form_kind.selected() as usize) else {
        return;
    };
    let tier = {
        let idx = ctx.w.provider_form_tier.selected() as usize;
        ctx.provider_tier_names.borrow().get(idx).cloned()
    };
    let Some(tier) = tier else {
        return; // no tiers yet — §1 creates one first
    };
    let secret = ctx.w.provider_form_secret.text().to_string();
    dispatch(
        ctx,
        Action::SetProvider {
            kind: kind.to_string(),
            webhook_secret: secret,
            tier,
        },
    );
}

/// Read the form and dispatch create or update, depending on `Ctx::editing`.
fn submit_form(ctx: &Rc<Ctx>) {
    let name = ctx.w.form_name.text().trim().to_string();
    if name.is_empty() {
        return;
    }
    // Shared non-negative parse (value-formatting.md § Tier rank);
    // blank/garbage falls to rank 0, matching every app's form.
    let rank: u32 = fauna_core::format::parse_count(&ctx.w.form_rank.text()).unwrap_or(0);
    let description = opt(ctx.w.form_description.text().trim());
    let price_hint = opt(ctx.w.form_price_hint.text().trim());
    // Same shape as the fields above (and their shared, separately-tracked
    // "no clear verb yet" gap — monetization.md § The asking price →
    // Editability): a parsed sats value on the wire, `None` for empty OR
    // unparseable. `TierAskingPrice::from_sats` owns the sats→msat
    // arithmetic; no app writes the multiply itself.
    let asking_price = opt(ctx.w.form_asking_price.text().trim())
        .and_then(|s| s.parse::<u64>().ok())
        .and_then(fauna_protocol::subscriptions::TierAskingPrice::from_sats);
    let payment_url = opt(ctx.w.form_payment_url.text().trim());
    let auto_approve = ctx.w.form_auto_approve.is_active();
    let editing = ctx.editing.borrow().clone();
    let action = match editing {
        Some(_) => Action::UpdateTier {
            name,
            rank,
            description,
            price_hint,
            asking_price,
            payment_url,
            auto_approve,
        },
        None => Action::CreateTier {
            name,
            rank,
            description,
            price_hint,
            asking_price,
            payment_url,
            auto_approve,
        },
    };
    dispatch(ctx, action);
}

fn opt(s: &str) -> Option<String> {
    if s.is_empty() {
        None
    } else {
        Some(s.to_string())
    }
}

/// The mutating actions the Tiers tab dispatches.
enum Action {
    CreateTier {
        name: String,
        rank: u32,
        description: Option<String>,
        price_hint: Option<String>,
        asking_price: Option<fauna_protocol::subscriptions::TierAskingPrice>,
        payment_url: Option<String>,
        auto_approve: bool,
    },
    UpdateTier {
        name: String,
        rank: u32,
        description: Option<String>,
        price_hint: Option<String>,
        asking_price: Option<fauna_protocol::subscriptions::TierAskingPrice>,
        payment_url: Option<String>,
        auto_approve: bool,
    },
    DeleteTier {
        name: String,
    },
    Approve {
        request: PendingRequest,
    },
    Reject {
        request_id: i64,
    },
    RemoveSubscriber {
        tier_name: String,
        subscriber_id: ActorId,
    },
    #[cfg(feature = "payments")]
    SetProvider {
        kind: String,
        webhook_secret: String,
        tier: String,
    },
    #[cfg(feature = "payments")]
    RemoveProvider {
        kind: String,
    },
    #[cfg(feature = "payments")]
    MintClaim {
        tier: String,
    },
}

/// Run a mutation on the tokio runtime, then re-render on the GTK thread. The
/// approve path mints+uploads a KeyBlob (`SubscriptionsAuthor`), so it shows the
/// `subscription-request-busy` indicator for its duration.
fn dispatch(ctx: &Rc<Ctx>, action: Action) {
    let nest = ctx.client.nest_rpc().clone();
    let secret = ctx.client.secret_bytes();
    let is_approve = matches!(action, Action::Approve { .. });
    if is_approve {
        ctx.w.request_busy.set_visible(true);
    }
    let ctx_render = Rc::clone(ctx);
    spawn_with_snapshot(
        &ctx.rt,
        move || async move {
            let subs = SubscriptionsClient::new(nest.clone());
            match action {
                Action::CreateTier {
                    name,
                    rank,
                    description,
                    price_hint,
                    asking_price,
                    payment_url,
                    auto_approve,
                } => {
                    let author = SubscriptionsAuthor::over(
                        nest.clone(),
                        ActorKeypair::from_secret(secret),
                        crate::account_runtime::period_key_store(),
                    );
                    author
                        .create_tier(
                            &name,
                            rank,
                            description,
                            price_hint,
                            payment_url,
                            auto_approve,
                            // Ordinary tier-management form — never a per-post
                            // pay-to-unlock tier: that designation is set only
                            // by the "sell this post" orchestration
                            // (monetization.md gap 2).
                            None,
                            asking_price,
                            // The tier-management form mints OFFERED tiers; the
                            // reserved hidden tier is provisioned by the
                            // archive-import machine, not by hand.
                            false,
                        )
                        .await
                        .map(|_| ())
                        .map_err(|e| e.to_string())
                }
                Action::UpdateTier {
                    name,
                    rank,
                    description,
                    price_hint,
                    asking_price,
                    payment_url,
                    auto_approve,
                } => subs
                    .tiers_update(
                        name,
                        Some(rank),
                        description,
                        price_hint,
                        payment_url,
                        Some(auto_approve),
                        // `None` keeps the tier's current asking price
                        // (monetization.md § The asking price) — an empty or
                        // unparseable field means "leave it alone", never
                        // "clear it" (no clear verb wired here yet, matching
                        // `price_hint`/`description`/`payment_url` above).
                        asking_price,
                    )
                    .await
                    .map(|_| ())
                    .map_err(|e| e.to_string()),
                Action::DeleteTier { name } => subs
                    .tiers_delete(name)
                    .await
                    .map(|_| ())
                    .map_err(|e| e.to_string()),
                Action::Approve { request } => {
                    let author = SubscriptionsAuthor::over(
                        nest.clone(),
                        ActorKeypair::from_secret(secret),
                        crate::account_runtime::period_key_store(),
                    );
                    author
                        .approve_subscriber(&request)
                        .await
                        .map(|_| ())
                        .map_err(|e| e.to_string())
                }
                Action::Reject { request_id } => subs
                    .requests_reject(request_id)
                    .await
                    .map(|_| ())
                    .map_err(|e| e.to_string()),
                Action::RemoveSubscriber {
                    tier_name,
                    subscriber_id,
                } => {
                    let author = SubscriptionsAuthor::over(
                        nest.clone(),
                        ActorKeypair::from_secret(secret),
                        crate::account_runtime::period_key_store(),
                    );
                    author
                        .remove_subscriber(&tier_name, subscriber_id)
                        .await
                        .map_err(|e| e.to_string())
                }
                #[cfg(feature = "payments")]
                Action::SetProvider {
                    kind,
                    webhook_secret,
                    tier,
                } => PaymentsClient::new(nest.clone())
                    .providers_set(kind, webhook_secret, tier)
                    .await
                    .map(|_| ())
                    .map_err(|e| e.to_string()),
                #[cfg(feature = "payments")]
                Action::RemoveProvider { kind } => PaymentsClient::new(nest.clone())
                    .providers_remove(kind)
                    .await
                    .map(|_| ())
                    .map_err(|e| e.to_string()),
                #[cfg(feature = "payments")]
                Action::MintClaim { tier } => PaymentsClient::new(nest.clone())
                    .claims_mint(tier, None)
                    .await
                    .map(|_| ())
                    .map_err(|e| e.to_string()),
            }
        },
        move |res| {
            ctx_render.w.request_busy.set_visible(false);
            match res {
                Ok(()) => {
                    clear_error(&ctx_render.w);
                    ctx_render.w.form.set_visible(false);
                    #[cfg(feature = "payments")]
                    {
                        ctx_render.w.provider_form.set_visible(false);
                        // The secret is a credential — don't leave it in the
                        // widget after the nest has it.
                        ctx_render.w.provider_form_secret.set_text("");
                    }
                    *ctx_render.editing.borrow_mut() = None;
                    refresh_all(&ctx_render);
                }
                Err(msg) => show_error(&ctx_render.w, &msg),
            }
        },
    );
}

/// Read the tier list + pending requests, then re-render §1 + §2 and repopulate
/// the §3 tier picker. Runs on the tokio runtime; result applied on the GTK
/// thread. Used on mount and after every mutation.
fn refresh_all(ctx: &Rc<Ctx>) {
    let nest = ctx.client.nest_rpc().clone();
    let ctx_render = Rc::clone(ctx);
    spawn_with_snapshot(
        &ctx.rt,
        move || async move {
            let subs = SubscriptionsClient::new(nest.clone());
            let tiers = subs.tiers_list().await.map_err(|e| e.to_string());
            let requests = subs.requests_list().await.map_err(|e| e.to_string());
            #[cfg(feature = "payments")]
            let (providers, claims) = {
                let payments = PaymentsClient::new(nest);
                (
                    payments.providers_list().await.map_err(|e| e.to_string()),
                    payments.claims_list().await.map_err(|e| e.to_string()),
                )
            };
            Snapshot {
                tiers,
                requests,
                #[cfg(feature = "payments")]
                providers,
                #[cfg(feature = "payments")]
                claims,
            }
        },
        move |snap| apply_snapshot(&ctx_render, snap),
    );
}

struct Snapshot {
    tiers: Result<Vec<TierItem>, String>,
    requests: Result<Vec<PendingRequest>, String>,
    #[cfg(feature = "payments")]
    providers: Result<Vec<ProviderItem>, String>,
    #[cfg(feature = "payments")]
    claims: Result<Vec<ClaimItem>, String>,
}

fn apply_snapshot(ctx: &Rc<Ctx>, snap: Snapshot) {
    match snap.tiers {
        Ok(tiers) => {
            render_tier_rows(ctx, &tiers);
            // Repopulate the §3 picker, preserving the prior selection by name.
            let prev = {
                let idx = ctx.w.tier_select.selected() as usize;
                ctx.tier_names.borrow().get(idx).cloned()
            };
            let names: Vec<String> = tiers.iter().map(|t| t.name.clone()).collect();
            let refs: Vec<&str> = names.iter().map(|s| s.as_str()).collect();
            ctx.w
                .tier_select
                .set_model(Some(&gtk::StringList::new(&refs)));
            let restore = prev
                .as_ref()
                .and_then(|p| names.iter().position(|n| n == p))
                .unwrap_or(0);
            *ctx.tier_names.borrow_mut() = names.clone();
            if !names.is_empty() {
                ctx.w.tier_select.set_selected(restore as u32);
                refresh_roster(ctx, names[restore].clone());
            } else {
                render_subscriber_rows(ctx, &[]);
            }
            #[cfg(feature = "payments")]
            {
                // Repopulate the §4 provider form's tier-map picker from the same
                // read (preserving the prior selection by name, like §3).
                let prev = {
                    let idx = ctx.w.provider_form_tier.selected() as usize;
                    ctx.provider_tier_names.borrow().get(idx).cloned()
                };
                ctx.w
                    .provider_form_tier
                    .set_model(Some(&gtk::StringList::new(&refs)));
                let restore = prev
                    .as_ref()
                    .and_then(|p| names.iter().position(|n| n == p))
                    .unwrap_or(0);
                *ctx.provider_tier_names.borrow_mut() = names.clone();
                if !names.is_empty() {
                    ctx.w.provider_form_tier.set_selected(restore as u32);
                }
                // Repopulate the §5 claim-mint tier picker from the same read
                // (preserving the prior selection by name, like §3/§4).
                let prev = {
                    let idx = ctx.w.claim_tier_select.selected() as usize;
                    ctx.claim_tier_names.borrow().get(idx).cloned()
                };
                ctx.w
                    .claim_tier_select
                    .set_model(Some(&gtk::StringList::new(&refs)));
                let restore = prev
                    .as_ref()
                    .and_then(|p| names.iter().position(|n| n == p))
                    .unwrap_or(0);
                *ctx.claim_tier_names.borrow_mut() = names.clone();
                if !names.is_empty() {
                    ctx.w.claim_tier_select.set_selected(restore as u32);
                }
            }
        }
        Err(msg) => show_error(&ctx.w, &msg),
    }
    match snap.requests {
        Ok(requests) => render_request_rows(ctx, &requests),
        Err(msg) => show_error(&ctx.w, &msg),
    }
    #[cfg(feature = "payments")]
    {
        match snap.providers {
            Ok(providers) => render_provider_rows(ctx, &providers),
            Err(msg) => show_error(&ctx.w, &msg),
        }
        match snap.claims {
            Ok(claims) => render_claim_rows(ctx, &claims),
            Err(msg) => show_error(&ctx.w, &msg),
        }
    }
}

/// Read the roster of `tier_name` and re-render §3.
fn refresh_roster(ctx: &Rc<Ctx>, tier_name: String) {
    let nest = ctx.client.nest_rpc().clone();
    let read_tier = tier_name.clone();
    let ctx_render = Rc::clone(ctx);
    spawn_with_snapshot(
        &ctx.rt,
        move || async move {
            let subs = SubscriptionsClient::new(nest);
            subs.subscribers_list(read_tier)
                .await
                .map_err(|e| e.to_string())
        },
        move |res| match res {
            // Pair each subscriber with the tier it was read for, so the
            // per-row remove rotates the correct tier.
            Ok(roster) => {
                let rows: Vec<(ActorId, String)> = roster
                    .iter()
                    .map(|e| (e.subscriber_id, tier_name.clone()))
                    .collect();
                render_subscriber_rows(&ctx_render, &rows);
            }
            Err(msg) => show_error(&ctx_render.w, &msg),
        },
    );
}

fn render_tier_rows(ctx: &Rc<Ctx>, tiers: &[TierItem]) {
    let mut rows = ctx.w.tier_rows.borrow_mut();
    for row in rows.drain(..) {
        ctx.w.tier_list.remove(&row);
    }
    // A per-post pay-to-unlock designated tier never appears in §1 My tiers
    // (monetization.md:128) — the unlock affordance renders on the post, not
    // in a tier-management list; the author's `tiers.list` read still carries
    // it (they need it to gate the post and audit sales via §5), so the
    // exclusion is client-side off `unlocks_post`, same shape as `offers.rs`'s
    // `FOLLOWERS_TIER` filter.
    let shown: Vec<&TierItem> = tiers.iter().filter(|t| t.unlocks_post.is_none()).collect();
    for tier in &shown {
        let row = build_tier_row(ctx, tier);
        ctx.w.tier_list.append(&row);
        rows.push(row);
    }
    ctx.w.tier_placeholder.set_visible(shown.is_empty());
}

fn build_tier_row(ctx: &Rc<Ctx>, tier: &TierItem) -> gtk::Box {
    let row = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .build();
    set_test_id(&row, ids::SUBSCRIPTION_TIER_ROW);

    let name = gtk::Label::new(Some(&tier.name));
    name.set_halign(gtk::Align::Start);
    name.set_hexpand(true);
    set_test_id(&name, ids::SUBSCRIPTION_TIER_NAME);
    row.append(&name);

    let rank = gtk::Label::new(Some(&tier.rank.to_string()));
    set_test_id(&rank, ids::SUBSCRIPTION_TIER_RANK);
    row.append(&rank);

    let price = gtk::Label::new(Some(tier.price_hint.as_deref().unwrap_or("")));
    set_test_id(&price, ids::SUBSCRIPTION_TIER_PRICE);
    row.append(&price);

    let edit = gtk::Button::with_label(s::EDIT);
    set_test_id(&edit, ids::SUBSCRIPTION_TIER_EDIT_BUTTON);
    {
        let ctx = Rc::clone(ctx);
        let tier = tier.clone();
        edit.connect_clicked(move |_| {
            *ctx.editing.borrow_mut() = Some(tier.name.clone());
            ctx.w.form_name.set_text(&tier.name);
            // The tier name is the server key — not editable on update.
            ctx.w.form_name.set_sensitive(false);
            ctx.w.form_rank.set_text(&tier.rank.to_string());
            ctx.w
                .form_description
                .set_text(tier.description.as_deref().unwrap_or(""));
            ctx.w
                .form_price_hint
                .set_text(tier.price_hint.as_deref().unwrap_or(""));
            // The reverse of `TierAskingPrice::from_sats` — pre-fill with the
            // tier's current price in sats, or empty for an unpriced tier /
            // a unit this build cannot interpret (fail-closed). An edit that
            // saves without touching this field must keep the current price,
            // never silently clear it (see `submit_form`'s `None` handling).
            ctx.w.form_asking_price.set_text(
                &tier
                    .asking_price
                    .as_ref()
                    .and_then(|p| p.to_sats())
                    .map(|s| s.to_string())
                    .unwrap_or_default(),
            );
            ctx.w
                .form_payment_url
                .set_text(tier.payment_url.as_deref().unwrap_or(""));
            ctx.w.form_auto_approve.set_active(tier.auto_approve);
            clear_error(&ctx.w);
            ctx.w.form.set_visible(true);
        });
    }
    row.append(&edit);

    let delete = gtk::Button::with_label(s::DELETE);
    delete.add_css_class("destructive-action");
    set_test_id(&delete, ids::SUBSCRIPTION_TIER_DELETE_BUTTON);
    crate::offline_gate::declare_wire_kind(&delete, "fauna.subscriptions.tiers.delete");
    {
        let ctx = Rc::clone(ctx);
        let name = tier.name.clone();
        delete.connect_clicked(move |_| {
            dispatch(&ctx, Action::DeleteTier { name: name.clone() });
        });
    }
    row.append(&delete);

    row
}

fn render_request_rows(ctx: &Rc<Ctx>, requests: &[PendingRequest]) {
    let mut rows = ctx.w.request_rows.borrow_mut();
    for row in rows.drain(..) {
        ctx.w.request_list.remove(&row);
    }
    for req in requests {
        let row = build_request_row(ctx, req);
        ctx.w.request_list.append(&row);
        rows.push(row);
    }
    ctx.w.request_placeholder.set_visible(requests.is_empty());
}

fn build_request_row(ctx: &Rc<Ctx>, req: &PendingRequest) -> gtk::Box {
    let row = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .build();
    set_test_id(&row, ids::SUBSCRIPTION_REQUEST_ROW);

    let who = gtk::Label::new(Some(&fauna_core::format::hex_full(&req.subscriber_id.0)));
    who.set_halign(gtk::Align::Start);
    who.set_hexpand(true);
    who.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
    set_test_id(&who, ids::SUBSCRIPTION_REQUEST_SUBSCRIBER);
    row.append(&who);

    let tier = gtk::Label::new(Some(&req.tier_name));
    set_test_id(&tier, ids::SUBSCRIPTION_REQUEST_TIER);
    row.append(&tier);

    let kind = gtk::Label::new(Some(&req.kind));
    set_test_id(&kind, ids::SUBSCRIPTION_REQUEST_KIND);
    row.append(&kind);

    let paid_badge = gtk::Label::new(Some(s::PAID));
    paid_badge.add_css_class("badge");
    paid_badge.set_visible(req.payment_entitled);
    set_test_id(&paid_badge, ids::SUBSCRIPTION_REQUEST_PAID_BADGE);
    row.append(&paid_badge);

    let approve = gtk::Button::with_label(s::APPROVE);
    approve.add_css_class("suggested-action");
    set_test_id(&approve, ids::SUBSCRIPTION_REQUEST_APPROVE_BUTTON);
    crate::offline_gate::declare_wire_kind(&approve, "fauna.subscriptions.requests.approve");
    {
        let ctx = Rc::clone(ctx);
        let req = req.clone();
        approve.connect_clicked(move |_| {
            dispatch(
                &ctx,
                Action::Approve {
                    request: req.clone(),
                },
            );
        });
    }
    row.append(&approve);

    let reject = gtk::Button::with_label(s::REJECT);
    set_test_id(&reject, ids::SUBSCRIPTION_REQUEST_REJECT_BUTTON);
    crate::offline_gate::declare_wire_kind(&reject, "fauna.subscriptions.requests.reject");
    {
        let ctx = Rc::clone(ctx);
        let request_id = req.request_id;
        reject.connect_clicked(move |_| {
            dispatch(&ctx, Action::Reject { request_id });
        });
    }
    row.append(&reject);

    row
}

/// `(subscriber_id, tier_name)` pairs — the tier is threaded through so the
/// per-row remove knows which tier to rotate.
fn render_subscriber_rows(ctx: &Rc<Ctx>, roster: &[(ActorId, String)]) {
    let mut rows = ctx.w.subscriber_rows.borrow_mut();
    for row in rows.drain(..) {
        ctx.w.subscriber_list.remove(&row);
    }
    for (subscriber_id, tier_name) in roster {
        let row = build_subscriber_row(ctx, *subscriber_id, tier_name.clone());
        ctx.w.subscriber_list.append(&row);
        rows.push(row);
    }
    ctx.w.subscriber_placeholder.set_visible(roster.is_empty());
}

fn build_subscriber_row(ctx: &Rc<Ctx>, subscriber_id: ActorId, tier_name: String) -> gtk::Box {
    let row = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .build();
    set_test_id(&row, ids::SUBSCRIPTION_SUBSCRIBER_ROW);

    let who = gtk::Label::new(Some(&fauna_core::format::hex_full(&subscriber_id.0)));
    who.set_halign(gtk::Align::Start);
    who.set_hexpand(true);
    who.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
    set_test_id(&who, ids::SUBSCRIPTION_SUBSCRIBER_HANDLE);
    row.append(&who);

    let remove = gtk::Button::with_label(s::REMOVE);
    remove.add_css_class("destructive-action");
    set_test_id(&remove, ids::SUBSCRIPTION_SUBSCRIBER_REMOVE_BUTTON);
    crate::offline_gate::declare_wire_kind(&remove, "fauna.subscriptions.subscribers.remove");
    {
        let ctx = Rc::clone(ctx);
        remove.connect_clicked(move |_| {
            dispatch(
                &ctx,
                Action::RemoveSubscriber {
                    tier_name: tier_name.clone(),
                    subscriber_id,
                },
            );
        });
    }
    row.append(&remove);

    row
}

#[cfg(feature = "payments")]
fn render_provider_rows(ctx: &Rc<Ctx>, providers: &[ProviderItem]) {
    let mut rows = ctx.w.provider_rows.borrow_mut();
    for row in rows.drain(..) {
        ctx.w.provider_list.remove(&row);
    }
    for provider in providers {
        let row = build_provider_row(ctx, provider);
        ctx.w.provider_list.append(&row);
        rows.push(row);
    }
    ctx.w.provider_placeholder.set_visible(providers.is_empty());
}

#[cfg(feature = "payments")]
fn build_provider_row(ctx: &Rc<Ctx>, provider: &ProviderItem) -> gtk::Box {
    let row = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .build();
    set_test_id(&row, ids::SUBSCRIPTION_PROVIDER_ROW);

    let kind = gtk::Label::new(Some(&provider.kind));
    kind.set_halign(gtk::Align::Start);
    kind.set_hexpand(true);
    set_test_id(&kind, ids::SUBSCRIPTION_PROVIDER_KIND);
    row.append(&kind);

    let tier = gtk::Label::new(Some(&provider.tier));
    row.append(&tier);

    // Shared evidence-based 3-state decision
    // (`fauna_core::format::provider_status_label`) — never an active probe;
    // see monetization.md § Pillar 3. Same idiom as `claim_status_label`
    // below.
    let status_text = fauna_core::format::provider_status_label(
        provider.last_verified_at,
        provider.last_rejected_at,
    )
    .resolve(crate::i18n::strings::lookup);
    let status = gtk::Label::new(Some(&status_text));
    status.add_css_class("dim-label");
    set_test_id(&status, ids::SUBSCRIPTION_PROVIDER_STATUS);
    row.append(&status);

    let remove = gtk::Button::with_label(s::REMOVE);
    remove.add_css_class("destructive-action");
    set_test_id(&remove, ids::SUBSCRIPTION_PROVIDER_REMOVE_BUTTON);
    crate::offline_gate::declare_wire_kind(&remove, "fauna.payments.providers.remove");
    {
        let ctx = Rc::clone(ctx);
        let kind = provider.kind.clone();
        remove.connect_clicked(move |_| {
            dispatch(&ctx, Action::RemoveProvider { kind: kind.clone() });
        });
    }
    row.append(&remove);

    row
}

#[cfg(feature = "payments")]
fn render_claim_rows(ctx: &Rc<Ctx>, claims: &[ClaimItem]) {
    let mut rows = ctx.w.claim_rows.borrow_mut();
    for row in rows.drain(..) {
        ctx.w.claim_list.remove(&row);
    }
    for claim in claims {
        let row = build_claim_row(claim);
        ctx.w.claim_list.append(&row);
        rows.push(row);
    }
    ctx.w.claim_placeholder.set_visible(claims.is_empty());
}

/// One claim-code row: bare code/tier/status labels, no captions — matches
/// the §4 provider-row idiom. Status is derived client-side from `ClaimItem`
/// (unlike §4's `provider-status`, this has no unresolved verified-signal
/// caveat — redeemed/voided are hard nest-side facts).
#[cfg(feature = "payments")]
fn build_claim_row(claim: &ClaimItem) -> gtk::Box {
    let row = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .build();
    set_test_id(&row, ids::SUBSCRIPTION_CLAIM_ROW);

    let code = gtk::Label::new(Some(&claim.code));
    code.set_halign(gtk::Align::Start);
    code.set_hexpand(true);
    set_test_id(&code, ids::SUBSCRIPTION_CLAIM_CODE);
    row.append(&code);

    let tier = gtk::Label::new(Some(&claim.tier));
    set_test_id(&tier, ids::SUBSCRIPTION_CLAIM_TIER);
    row.append(&tier);

    // Shared 3-state decision (`fauna_core::format::claim_status_label`) —
    // redeemed wins over voided; see monetization.md § Pillar 3.
    let status_text = fauna_core::format::claim_status_label(
        claim.redeemed_by.is_some(),
        claim.voided_at.is_some(),
    )
    .resolve(crate::i18n::strings::lookup);
    let status = gtk::Label::new(Some(&status_text));
    status.add_css_class("dim-label");
    set_test_id(&status, ids::SUBSCRIPTION_CLAIM_STATUS);
    row.append(&status);

    row
}

fn show_error(w: &Widgets, msg: &str) {
    w.error_label.add_css_class("error");
    crate::settings::render_error_label(&w.error_label, Some(msg));
}

fn clear_error(w: &Widgets) {
    crate::settings::render_error_label(&w.error_label, None);
}
