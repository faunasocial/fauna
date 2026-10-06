//! The T16 custody facet's **owner side** on Settings → Devices — "who holds
//! my data" (`docs/goal/ui/devices.md` § Custody facet, piece 2).
//!
//! One `custody-holder-card` per cross-account custodian device: the
//! counterpart account, the three-state receipt status, held-bytes against the
//! budget, and the honest-bound revoke. Rendered from the shared fold
//! (`fauna_client_capabilities::view_model::custody_rows`) — this module paints,
//! it derives nothing.
//!
//! **Pieces 1 and 3 and offer initiation live here too** (2026-09-28). The
//! host side — "what I hold for others" — is one `custody-held-card` per
//! accepted custody (owner, scope, metered bytes, the budget input, stop and
//! remove), then one `custody-offer-card` per pending offer (the REQUIRED floor
//! copy, the target select where the nest choice is legal, accept and decline),
//! then the `custody-mint-*` flow the owner asks a friend with. Every control
//! is live: each emits one [`CustodyGesture`] that the page runs through the
//! shared `fauna_client_custody::run_custody_act` and answers on its
//! `error-message` (piece 1's keyless-posture marker rides the roster card —
//! `roster.rs`). The store the budget and stop controls write is the account
//! runtime's (`crate::account_runtime`), and the ceremony sink that lands
//! offers here is registered with the conversations session
//! (`conversations::conv_backend`).

use adw::prelude::*;
use fauna_ui_ids as ids;

use std::rc::Rc;

use fauna_client_capabilities::custody_view::CustodyMintCandidateView;
use fauna_client_capabilities::view_model::{CustodyOfferView, CustodyRowView, HeldCustodyView};

use crate::i18n::strings;
use crate::testid::set_test_id;

/// Build the custodians section — the group plus the list box the render loop
/// repopulates. Mirrors [`super::roster::build_devices_section`]'s shape so the
/// two sections on this page are constructed and refreshed the same way.
///
/// The group starts hidden: an account with no custodians has nothing to say
/// here, and an empty titled group reads as a feature that failed to load.
pub fn build_custody_section() -> (adw::PreferencesGroup, gtk::ListBox) {
    let list = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .css_classes(["boxed-list"])
        .build();

    let group = adw::PreferencesGroup::builder()
        .title(strings::devices::CUSTODY_HOLDER_SECTION)
        .visible(false)
        .build();
    group.add(&list);

    (group, list)
}

/// Repopulate the custodian rows from the shared fold.
///
/// `on_revoke` receives the row's grant id and its accept-bound custodian key —
/// the pair `CustodyAct::Revoke` needs. The gesture carries the GRANT ID, never
/// the row index: a facet refresh re-orders rows, so an index captured at paint
/// time can address a different custody by the time the act runs.
pub fn update_custody_list(
    group: &adw::PreferencesGroup,
    list_box: &gtk::ListBox,
    rows: &[CustodyRowView],
    on_revoke: impl Fn(Vec<u8>, Option<[u8; 32]>) + Clone + 'static,
) {
    while let Some(child) = list_box.first_child() {
        list_box.remove(&child);
    }

    // A nest-anchored custody belongs to the Nests page's
    // `nest-trust-custody-*` family — one custody never renders in both
    // (the nest-custodian identity fact, ruled 2026-08-17).
    let device_rows: Vec<&CustodyRowView> = rows
        .iter()
        .filter(|r| r.custodian_nest_url.is_none())
        .collect();

    group.set_visible(!device_rows.is_empty());

    for row in device_rows {
        list_box.append(&build_custody_card(row, on_revoke.clone()));
    }
}

/// Every custody widget on the Devices sub-page — the owner-side custodian
/// group, the host-side held and offer groups, and the mint flow — plus the
/// page's gesture sink, which the page installs once it has the client
/// ([`Self::set_sink`]). Built cyclically so the mint flow's controls can
/// forward to the sink before it exists.
pub struct CustodyView {
    pub holder_group: adw::PreferencesGroup,
    pub holder_list: gtk::ListBox,
    pub held_group: adw::PreferencesGroup,
    pub held_list: gtk::ListBox,
    pub offer_group: adw::PreferencesGroup,
    pub offer_list: gtk::ListBox,
    pub mint: MintFlow,
    sink: std::cell::RefCell<Option<OnGesture>>,
    forward: OnGesture,
}

impl CustodyView {
    pub fn build() -> Rc<Self> {
        Rc::new_cyclic(|weak: &std::rc::Weak<Self>| {
            let weak = weak.clone();
            let forward: OnGesture = Rc::new(move |g| {
                if let Some(view) = weak.upgrade() {
                    let sink = view.sink.borrow().clone();
                    if let Some(sink) = sink {
                        sink(g);
                    }
                }
            });
            let (holder_group, holder_list) = build_custody_section();
            let (held_group, held_list) = build_held_section();
            let (offer_group, offer_list) = build_offer_section();
            let mint = MintFlow::build(&forward);
            Self {
                holder_group,
                holder_list,
                held_group,
                held_list,
                offer_group,
                offer_list,
                mint,
                sink: std::cell::RefCell::new(None),
                forward,
            }
        })
    }

    /// Install the page's gesture sink.
    pub fn set_sink(&self, sink: OnGesture) {
        self.sink.replace(Some(sink));
    }

    /// Append every custody widget to the page, in the facet's order: who
    /// holds my data, what I hold for others, the offers waiting on me, then
    /// the ask. Below the roster — none of these are this account's devices.
    pub fn append_to(&self, content: &gtk::Box) {
        content.append(&self.holder_group);
        content.append(&self.held_group);
        content.append(&self.offer_group);
        content.append(&self.mint.container);
    }

    /// Repaint all three families from one fold. `nest_pinned` — this app
    /// holds a pinned identity for its home nest (the offer target select's
    /// second half).
    pub fn paint(
        &self,
        facet: &fauna_client_capabilities::view_model::CustodyFacetSnapshot,
        nest_pinned: bool,
    ) {
        let revoke = Rc::clone(&self.forward);
        update_custody_list(
            &self.holder_group,
            &self.holder_list,
            &facet.rows,
            move |grant_id, holder| revoke(CustodyGesture::Revoke { grant_id, holder }),
        );
        let drafts: Vec<String> = fauna_client_capabilities::view_model::budget_draft_texts(facet)
            .iter()
            .map(|t| t.resolve(strings::lookup))
            .collect();
        update_held_list(
            &self.held_group,
            &self.held_list,
            &facet.held,
            &drafts,
            &self.forward,
        );
        update_offer_list(
            &self.offer_group,
            &self.offer_list,
            &facet.offers,
            nest_pinned,
            &self.forward,
        );
    }
}

/// One custody gesture, raised by a control on this page and run by the page
/// through the shared `run_custody_act` (whose error reaches `error-message`).
///
/// Every gesture carries its GRANT ID, never a row index: a facet refresh
/// re-orders rows, so an index captured at paint time can address a different
/// custody by the time the act runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CustodyGesture {
    Revoke {
        grant_id: Vec<u8>,
        holder: Option<[u8; 32]>,
    },
    Accept {
        grant_id: Vec<u8>,
        on_nest: bool,
    },
    Decline {
        grant_id: Vec<u8>,
    },
    /// The budget input's committed text, parsed by the page with the shared
    /// `fauna_core::format::parse_byte_size` (an unparseable budget makes no
    /// call and says so).
    SetBudget {
        grant_id: Vec<u8>,
        typed: String,
    },
    Stop {
        grant_id: Vec<u8>,
    },
    Remove {
        grant_id: Vec<u8>,
    },
    /// `custody-mint-button` — the page reads the candidates and answers with
    /// [`MintFlow::open`] (or `devices.custody_mint_no_contacts`).
    MintOpen,
    Mint {
        host: Vec<u8>,
        channel_hex: String,
    },
}

/// The page's gesture sink.
pub type OnGesture = Rc<dyn Fn(CustodyGesture)>;

/// Build the host-side "held for others" section — hidden until this device
/// holds something.
pub fn build_held_section() -> (adw::PreferencesGroup, gtk::ListBox) {
    titled_hidden_group(strings::devices::CUSTODY_HELD_SECTION)
}

/// Build the incoming-offers section — hidden until an offer is pending.
pub fn build_offer_section() -> (adw::PreferencesGroup, gtk::ListBox) {
    // The cards carry their own titles ("‹owner› asks this device…"), so the
    // group needs none.
    titled_hidden_group("")
}

fn titled_hidden_group(title: &str) -> (adw::PreferencesGroup, gtk::ListBox) {
    let list = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .css_classes(["boxed-list"])
        .build();
    let group = adw::PreferencesGroup::builder()
        .title(title)
        .visible(false)
        .build();
    group.add(&list);
    (group, list)
}

/// Repopulate the held-for-others cards. `drafts` are the shared budget seed
/// texts (`view_model::budget_draft_texts`, resolved), one per `held` row.
pub fn update_held_list(
    group: &adw::PreferencesGroup,
    list_box: &gtk::ListBox,
    held: &[HeldCustodyView],
    drafts: &[String],
    on: &OnGesture,
) {
    while let Some(child) = list_box.first_child() {
        list_box.remove(&child);
    }
    group.set_visible(!held.is_empty());
    for (i, row) in held.iter().enumerate() {
        let draft = drafts.get(i).cloned().unwrap_or_default();
        list_box.append(&build_held_card(row, &draft, on));
    }
}

/// Repopulate the offer consent cards. `nest_pinned` is whether this app holds
/// a pinned identity for its home nest — with an offer a nest can hold, the
/// other half of the target select's condition.
pub fn update_offer_list(
    group: &adw::PreferencesGroup,
    list_box: &gtk::ListBox,
    offers: &[CustodyOfferView],
    nest_pinned: bool,
    on: &OnGesture,
) {
    while let Some(child) = list_box.first_child() {
        list_box.remove(&child);
    }
    group.set_visible(!offers.is_empty());
    for offer in offers {
        list_box.append(&build_offer_card(offer, nest_pinned, on));
    }
}

/// One `custody-held-card`: owner, scope, the metered bytes, the budget input
/// (commit on Enter), stop and remove.
fn build_held_card(held: &HeldCustodyView, draft: &str, on: &OnGesture) -> gtk::ListBoxRow {
    let card = card_box(gtk::Orientation::Vertical);
    set_test_id(&card, ids::CUSTODY_HELD_CARD);

    let owner = gtk::Label::builder()
        .label(strings::devices::custody_held_owner(&short_actor(
            &held.owner,
        )))
        .halign(gtk::Align::Start)
        .css_classes(["heading"])
        .build();
    set_test_id(&owner, ids::CUSTODY_HELD_OWNER);

    let scope_text = match &held.scopes {
        Some(fauna_core::custody_grant::CustodyScopeSet::Scopes(list)) => list.join(", "),
        _ => strings::devices::CUSTODY_HELD_SCOPE_ACCOUNT.to_string(),
    };
    let scope = gtk::Label::builder()
        .label(scope_text)
        .halign(gtk::Align::Start)
        .wrap(true)
        .css_classes(["caption", "dim-label"])
        .build();
    set_test_id(&scope, ids::CUSTODY_HELD_SCOPE);

    let bytes = gtk::Label::builder()
        .label(crate::i18n::custody_held_bytes(held.receipt.as_ref()))
        .halign(gtk::Align::Start)
        .css_classes(["caption"])
        .build();
    set_test_id(&bytes, ids::CUSTODY_HELD_BYTES);

    let budget_label = gtk::Label::builder()
        .label(strings::devices::CUSTODY_BUDGET_LABEL)
        .halign(gtk::Align::Start)
        .build();
    let budget = gtk::Entry::builder().text(draft).hexpand(true).build();
    set_test_id(&budget, ids::CUSTODY_HELD_BUDGET_INPUT);
    budget.update_property(&[gtk::accessible::Property::Label(
        strings::devices::CUSTODY_BUDGET_LABEL,
    )]);
    {
        let on = Rc::clone(on);
        let grant_id = held.grant_id.clone();
        budget.connect_activate(move |entry| {
            on(CustodyGesture::SetBudget {
                grant_id: grant_id.clone(),
                typed: entry.text().to_string(),
            })
        });
    }
    let budget_row = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .build();
    budget_row.append(&budget_label);
    budget_row.append(&budget);

    // Stopping pauses the hold; it does not free the space. Once stopped the
    // control says so and disables — the bytes stay until the custody is
    // removed.
    let stop = gtk::Button::with_label(if held.stopped {
        strings::devices::CUSTODY_STOPPED_BYTES_REMAIN
    } else {
        strings::devices::CUSTODY_STOP
    });
    stop.set_sensitive(!held.stopped);
    set_test_id(&stop, ids::CUSTODY_HELD_STOP_BUTTON);
    {
        let on = Rc::clone(on);
        let grant_id = held.grant_id.clone();
        stop.connect_clicked(move |_| {
            on(CustodyGesture::Stop {
                grant_id: grant_id.clone(),
            })
        });
    }

    // Always available, stopped or not: stop is the pause and this is the
    // reclaim.
    let remove = gtk::Button::with_label(strings::devices::CUSTODY_REMOVE);
    remove.add_css_class("destructive-action");
    set_test_id(&remove, ids::CUSTODY_HELD_REMOVE_BUTTON);
    {
        let on = Rc::clone(on);
        let grant_id = held.grant_id.clone();
        remove.connect_clicked(move |_| {
            on(CustodyGesture::Remove {
                grant_id: grant_id.clone(),
            })
        });
    }

    let buttons = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .build();
    buttons.append(&stop);
    buttons.append(&remove);

    card.append(&owner);
    card.append(&scope);
    card.append(&bytes);
    card.append(&budget_row);
    card.append(&buttons);
    list_row(&card)
}

/// One `custody-offer-card` — the consent surface. The floor copy is REQUIRED
/// before accept (`devices.md` § Custody facet piece 3): what this device would
/// see — the shape, never the content.
fn build_offer_card(
    offer: &CustodyOfferView,
    nest_pinned: bool,
    on: &OnGesture,
) -> gtk::ListBoxRow {
    let card = card_box(gtk::Orientation::Vertical);
    set_test_id(&card, ids::CUSTODY_OFFER_CARD);

    let title = gtk::Label::builder()
        .label(strings::devices::custody_offer_title(&short_actor(
            &offer.owner,
        )))
        .halign(gtk::Align::Start)
        .wrap(true)
        .css_classes(["heading"])
        .build();
    card.append(&title);

    let floor = gtk::Label::builder()
        .label(strings::devices::CUSTODY_OFFER_FLOOR)
        .halign(gtk::Align::Start)
        .wrap(true)
        .css_classes(["caption"])
        .build();
    set_test_id(&floor, ids::CUSTODY_OFFER_FLOOR_NOTE);
    card.append(&floor);

    // The host-side choice (the nest-custodian identity fact): present ONLY
    // for an offer a nest can hold, with a pinned nest identity in hand — absent
    // otherwise, never disabled. Index 0 = this device, 1 = my nest.
    let target = (offer.nest_can_hold && nest_pinned).then(|| {
        let label = gtk::Label::builder()
            .label(strings::devices::CUSTODY_OFFER_TARGET_LABEL)
            .halign(gtk::Align::Start)
            .build();
        let select = gtk::DropDown::from_strings(&[
            strings::devices::CUSTODY_OFFER_TARGET_DEVICE,
            strings::devices::CUSTODY_OFFER_TARGET_NEST,
        ]);
        select.set_selected(0);
        set_test_id(&select, ids::CUSTODY_OFFER_TARGET_SELECT);
        select.update_property(&[gtk::accessible::Property::Label(
            strings::devices::CUSTODY_OFFER_TARGET_LABEL,
        )]);
        let row = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(8)
            .build();
        row.append(&label);
        row.append(&select);
        card.append(&row);
        select
    });

    let accept = gtk::Button::with_label(strings::devices::CUSTODY_OFFER_ACCEPT);
    accept.add_css_class("suggested-action");
    set_test_id(&accept, ids::CUSTODY_OFFER_ACCEPT_BUTTON);
    {
        let on = Rc::clone(on);
        let grant_id = offer.grant_id.clone();
        accept.connect_clicked(move |_| {
            let on_nest = target.as_ref().is_some_and(|t| t.selected() == 1);
            on(CustodyGesture::Accept {
                grant_id: grant_id.clone(),
                on_nest,
            })
        });
    }
    let decline = gtk::Button::with_label(strings::devices::CUSTODY_OFFER_DECLINE);
    set_test_id(&decline, ids::CUSTODY_OFFER_DECLINE_BUTTON);
    {
        let on = Rc::clone(on);
        let grant_id = offer.grant_id.clone();
        decline.connect_clicked(move |_| {
            on(CustodyGesture::Decline {
                grant_id: grant_id.clone(),
            })
        });
    }
    let buttons = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .build();
    buttons.append(&accept);
    buttons.append(&decline);
    card.append(&buttons);
    list_row(&card)
}

/// The offer-initiation flow (`custody-mint-*`): the button, and — once the
/// page has read the candidates — the host select, the REQUIRED floor copy,
/// confirm and cancel. v1 offers the Account scope with the default window over
/// an EXISTING 1:1 conversation, so the options are those conversations.
pub struct MintFlow {
    pub container: gtk::Box,
    flow: gtk::Box,
    host_select: gtk::DropDown,
    confirm: gtk::Button,
    candidates: Rc<std::cell::RefCell<Vec<CustodyMintCandidateView>>>,
}

impl MintFlow {
    pub fn build(on: &OnGesture) -> Self {
        let container = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(8)
            .build();
        let open = gtk::Button::with_label(strings::devices::CUSTODY_MINT_BUTTON);
        open.set_halign(gtk::Align::Start);
        set_test_id(&open, ids::CUSTODY_MINT_BUTTON);
        {
            let on = Rc::clone(on);
            open.connect_clicked(move |_| on(CustodyGesture::MintOpen));
        }

        let flow = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(8)
            .visible(false)
            .build();
        let host_label = gtk::Label::builder()
            .label(strings::devices::CUSTODY_MINT_HOST_LABEL)
            .halign(gtk::Align::Start)
            .build();
        let host_select = gtk::DropDown::from_strings(&[]);
        set_test_id(&host_select, ids::CUSTODY_MINT_HOST_SELECT);
        host_select.update_property(&[gtk::accessible::Property::Label(
            strings::devices::CUSTODY_MINT_HOST_LABEL,
        )]);
        let floor = gtk::Label::builder()
            .label(strings::devices::CUSTODY_MINT_FLOOR)
            .halign(gtk::Align::Start)
            .wrap(true)
            .css_classes(["caption"])
            .build();
        set_test_id(&floor, ids::CUSTODY_MINT_FLOOR_NOTE);
        let confirm = gtk::Button::with_label(strings::devices::CUSTODY_MINT_CONFIRM);
        confirm.add_css_class("suggested-action");
        confirm.set_sensitive(false);
        set_test_id(&confirm, ids::CUSTODY_MINT_CONFIRM_BUTTON);
        let cancel = gtk::Button::with_label(strings::common::CANCEL);
        set_test_id(&cancel, ids::CUSTODY_MINT_CANCEL_BUTTON);

        let candidates: Rc<std::cell::RefCell<Vec<CustodyMintCandidateView>>> = Rc::default();
        // Index 0 is the placeholder: confirm is live only once a real host is
        // chosen (a control that cannot succeed is not offered).
        {
            let confirm = confirm.clone();
            host_select.connect_selected_notify(move |d| {
                confirm.set_sensitive(d.selected() >= 1);
            });
        }
        {
            let on = Rc::clone(on);
            let candidates = Rc::clone(&candidates);
            let host_select = host_select.clone();
            confirm.connect_clicked(move |_| {
                let chosen = (host_select.selected() as usize)
                    .checked_sub(1)
                    .and_then(|i| candidates.borrow().get(i).cloned());
                if let Some(c) = chosen {
                    // Passed back unchanged — the page picks a row, it never
                    // assembles a channel.
                    on(CustodyGesture::Mint {
                        host: c.host,
                        channel_hex: c.channel_hex,
                    });
                }
            });
        }
        {
            let flow = flow.clone();
            cancel.connect_clicked(move |_| flow.set_visible(false));
        }
        let buttons = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(8)
            .build();
        buttons.append(&confirm);
        buttons.append(&cancel);
        flow.append(&host_label);
        flow.append(&host_select);
        flow.append(&floor);
        flow.append(&buttons);
        container.append(&open);
        container.append(&flow);
        Self {
            container,
            flow,
            host_select,
            confirm,
            candidates,
        }
    }

    /// Reveal the flow over `candidates` (non-empty — the page answers an
    /// empty list with `devices.custody_mint_no_contacts` instead).
    pub fn open(&self, candidates: Vec<CustodyMintCandidateView>) {
        let mut labels = vec![strings::devices::CUSTODY_MINT_HOST_PLACEHOLDER.to_string()];
        labels.extend(candidates.iter().map(|c| c.label.clone()));
        let refs: Vec<&str> = labels.iter().map(String::as_str).collect();
        self.host_select
            .set_model(Some(&gtk::StringList::new(&refs)));
        self.host_select.set_selected(0);
        self.confirm.set_sensitive(false);
        self.candidates.replace(candidates);
        self.flow.set_visible(true);
    }

    /// Hide the flow (after a sent request, or cancel).
    pub fn close(&self) {
        self.flow.set_visible(false);
    }
}

fn card_box(orientation: gtk::Orientation) -> gtk::Box {
    // `accessible_role(Group)` for the AT-SPI discoverability reason
    // `build_custody_card` documents.
    gtk::Box::builder()
        .orientation(orientation)
        .spacing(6)
        .margin_top(12)
        .margin_bottom(12)
        .margin_start(12)
        .margin_end(12)
        .accessible_role(gtk::AccessibleRole::Group)
        .build()
}

fn list_row(card: &gtk::Box) -> gtk::ListBoxRow {
    let row = gtk::ListBoxRow::new();
    row.set_child(Some(card));
    row
}

/// One `custody-holder-card`. The card's own line carries the REQUIRED
/// honest-bound revoke copy (`ui/nests.md` § Trust facet — custody rows), so
/// the bound is stated beside the control it bounds and never over-promises.
fn build_custody_card(
    row: &CustodyRowView,
    on_revoke: impl Fn(Vec<u8>, Option<[u8; 32]>) + 'static,
) -> gtk::ListBoxRow {
    // `accessible_role(Group)` at construction, for the same AT-SPI
    // discoverability reason `build_device_card` documents: a plain `gtk::Box`
    // defaults to `Generic`, which some compositors omit from the tree, and
    // then `set_test_id`'s Description never resolves.
    let card = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(12)
        .margin_top(12)
        .margin_bottom(12)
        .margin_start(12)
        .margin_end(12)
        .accessible_role(gtk::AccessibleRole::Group)
        .build();
    set_test_id(&card, ids::CUSTODY_HOLDER_CARD);

    let info = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(4)
        .hexpand(true)
        .build();

    let name = gtk::Label::builder()
        .label(short_actor(&row.host))
        .halign(gtk::Align::Start)
        .css_classes(["heading"])
        .build();
    set_test_id(&name, ids::CUSTODY_HOLDER_NAME);

    // Three states, three strings — never collapsed, never empty (the A7
    // honesty rule). A stale custodian must read as degraded redundancy the
    // owner can see, so it is styled as a warning rather than dimmed away.
    let status_text = crate::i18n::custody_receipt_status(
        row.receipt_state,
        row.receipt.as_ref().map(|r| r.attested_at_micros),
    );
    let status_css: &[&str] = match row.receipt_state {
        fauna_client_capabilities::view_model::ReceiptState::Fresh => &["success"],
        fauna_client_capabilities::view_model::ReceiptState::Stale => &["warning"],
        fauna_client_capabilities::view_model::ReceiptState::NoReceiptYet => &["dim-label"],
    };
    let status = gtk::Label::builder()
        .label(status_text)
        .halign(gtk::Align::Start)
        .css_classes(status_css)
        .build();
    set_test_id(&status, ids::CUSTODY_HOLDER_RECEIPT_STATUS);

    let bytes = gtk::Label::builder()
        .label(crate::i18n::custody_held_bytes(row.receipt.as_ref()))
        .halign(gtk::Align::Start)
        .css_classes(["caption"])
        .build();
    set_test_id(&bytes, ids::CUSTODY_HOLDER_HELD_BYTES);

    info.append(&name);
    info.append(&status);
    info.append(&bytes);

    // The honest bound, stated beside the control: revoke stops future copies
    // and serving on honest devices; copies already held stay held — and stay
    // sealed forever. A pending ceremony has minted nothing to revoke yet, so
    // the bound note would over-promise there and the control is insensitive.
    if !row.pending {
        let bound = gtk::Label::builder()
            .label(strings::devices::CUSTODY_REVOKE_BOUND_NOTE)
            .halign(gtk::Align::Start)
            .wrap(true)
            .css_classes(["caption", "dim-label"])
            .build();
        info.append(&bound);
    }

    let revoke = gtk::Button::with_label(strings::devices::CUSTODY_REVOKE);
    revoke.add_css_class("destructive-action");
    revoke.set_valign(gtk::Align::Center);
    // A control that cannot succeed is not offered — the same rule
    // `build_identity_section` applies to the copy button with no actor id.
    revoke.set_sensitive(!row.pending);
    set_test_id(&revoke, ids::CUSTODY_HOLDER_REVOKE_BUTTON);
    // The capability plane's own kind, not the `fauna.state.custody-ceremony` write that FOLLOWS
    // it: `custody_acts::revoke_custody` calls the nest first and records the
    // signed Revoke event only on success, so the nest call is what a partial
    // failure leaves undone. Declared after `set_sensitive` above, so the
    // pending row's own "cannot succeed yet" intent is what the gate restores
    // on reconnect.
    crate::offline_gate::declare_wire_kind(&revoke, "fauna.capabilities.revoke");
    {
        let grant_id = row.grant_id.clone();
        let holder = row.custodian_key;
        revoke.connect_clicked(move |_| on_revoke(grant_id.clone(), holder));
    }

    let row_widget = gtk::ListBoxRow::new();
    card.append(&info);
    card.append(&revoke);
    row_widget.set_child(Some(&card));
    row_widget
}

/// The counterpart account, abbreviated — the same shared `short_id` every app
/// uses, so an actor reads identically across the seven UIs.
fn short_actor(id: &[u8; 32]) -> String {
    fauna_core::format::short_id(&hex::encode(id))
}
