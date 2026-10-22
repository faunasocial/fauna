//! The admin's hosted plugins on the Bridges page (`bridges.md` § Hosted
//! plugins on this page): one `bridge-plugin-item` per installed plugin — its
//! name, publisher and runner status, each the shared row's text
//! (`fauna_client_bridges::installed_plugin_row`) — Uninstall behind an inline
//! confirm, then the install entry, the document URL an admin gives
//! (`third-party.md` § The runner contract → *The install-approval leg*).
//! tui leads (`apps/fauna-tui/src/bridges.rs::plugin_elements`); this is the
//! same composition painted in GTK. Shown only while the admin gate passes.

use adw::prelude::*;
use fauna_client_bridges::{
    InstalledPluginRow, PluginCard, PluginCardAction, plugin_publisher_text, plugin_status_lines,
};
use fauna_ui_ids as ids;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

use crate::client::FaunaClient;
use crate::i18n::strings::bridges as t;
use crate::testid::{set_test_attr, set_test_attr_text, set_test_id};

thread_local! {
    /// Whether the signed-in account is the nest's admin (`set_admin`), the
    /// one gate on reading the plugin list.
    static ADMIN: Cell<bool> = const { Cell::new(false) };
}

/// Re-read the plugin cards, and the admin's plugin list, when the Bridges
/// page is entered (the real sidebar edge and the test agent's navigate both
/// call this). The cards are every account's; only an admin may list.
pub fn refresh(client: &FaunaClient) {
    client.fetch_plugin_cards();
    if ADMIN.get() {
        client.fetch_plugins();
    }
}

/// `value` as a CSS-identifier-safe token (`testid::set_test_attr`'s contract).
fn css_token(value: &str) -> String {
    value
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect()
}

/// The plugins section's live handles. Built once: every account's settings
/// cards (`update_cards`, from `fauna.plugins.available`) sit above the
/// admin's list and install entry, which `set_admin` reveals and `update`
/// repaints from a `fauna.plugins.list` reply.
pub struct PluginsSection {
    root: gtk::Box,
    /// The admin's list and install entry.
    admin_box: gtk::Box,
    /// One settings card per installed plugin (`bridge-card`).
    cards_box: gtk::Box,
    /// "Review the request in Connected apps → Requests" after a Connect.
    bind_hint: gtk::Label,
    cards: RefCell<Vec<PluginCard>>,
    items: gtk::Box,
    url_entry: gtk::Entry,
    submit: gtk::Button,
    opened_hint: gtk::Label,
    rows: RefCell<Vec<InstalledPluginRow>>,
    /// The plugin whose inline uninstall confirm is open, by install row id.
    armed: RefCell<Option<Vec<u8>>>,
    client: Rc<FaunaClient>,
}

impl PluginsSection {
    pub fn build(client: &Rc<FaunaClient>) -> Rc<Self> {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 6);
        root.set_margin_start(12);
        root.set_margin_end(12);
        root.set_margin_bottom(12);

        // Every account's settings cards, one `bridge-card` per plugin
        // (`bridges.md` § Hosted plugins on this page).
        let cards_box = gtk::Box::new(gtk::Orientation::Vertical, 6);
        root.append(&cards_box);
        let bind_hint = gtk::Label::new(Some(t::PLUGIN_BIND_OPENED));
        bind_hint.set_halign(gtk::Align::Start);
        bind_hint.set_xalign(0.0);
        bind_hint.set_wrap(true);
        bind_hint.add_css_class("dim-label");
        bind_hint.set_visible(false);
        root.append(&bind_hint);

        // The admin's half: hidden until the admin gate passes (`set_admin`).
        let admin_box = gtk::Box::new(gtk::Orientation::Vertical, 6);
        admin_box.set_visible(false);
        root.append(&admin_box);

        let heading = gtk::Label::new(Some(t::PLUGINS_HEADING));
        heading.add_css_class("heading");
        heading.set_halign(gtk::Align::Start);
        admin_box.append(&heading);

        let items = gtk::Box::new(gtk::Orientation::Vertical, 6);
        admin_box.append(&items);

        let url_entry = gtk::Entry::builder()
            .placeholder_text(t::PLUGIN_INSTALL_LABEL)
            .hexpand(true)
            .build();
        set_test_id(&url_entry, ids::BRIDGE_PLUGIN_INSTALL_URL);
        let submit = gtk::Button::builder()
            .label(t::PLUGIN_INSTALL_SUBMIT)
            .css_classes(["suggested-action"])
            .sensitive(false)
            .build();
        set_test_id(&submit, ids::BRIDGE_PLUGIN_INSTALL_SUBMIT);
        crate::offline_gate::declare_wire_kind(&submit, "fauna.plugins.install");
        let entry_row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        entry_row.append(&url_entry);
        entry_row.append(&submit);
        admin_box.append(&entry_row);

        let opened_hint = gtk::Label::new(Some(t::PLUGIN_INSTALL_OPENED));
        opened_hint.set_halign(gtk::Align::Start);
        opened_hint.set_xalign(0.0);
        opened_hint.set_wrap(true);
        opened_hint.add_css_class("dim-label");
        opened_hint.set_visible(false);
        admin_box.append(&opened_hint);

        let section = Rc::new(Self {
            root,
            admin_box,
            cards_box,
            bind_hint,
            cards: RefCell::new(Vec::new()),
            items,
            url_entry,
            submit,
            opened_hint,
            rows: RefCell::new(Vec::new()),
            armed: RefCell::new(None),
            client: Rc::clone(client),
        });

        {
            // The draft is committed only with something in it.
            let submit = section.submit.clone();
            section.url_entry.connect_changed(move |entry| {
                submit.set_sensitive(!entry.text().trim().is_empty())
            });
        }
        {
            let section_for_submit = Rc::clone(&section);
            section
                .submit
                .connect_clicked(move |_| section_for_submit.install());
        }
        {
            let section_for_activate = Rc::clone(&section);
            section
                .url_entry
                .connect_activate(move |_| section_for_activate.install());
        }
        section
    }

    pub fn widget(&self) -> &gtk::Box {
        &self.root
    }

    /// The admin gate: the list and the install entry are the admin's alone.
    pub fn set_admin(&self, admin: bool) {
        ADMIN.set(admin);
        self.admin_box.set_visible(admin);
    }

    /// Repaint the settings cards from a fresh `fauna.plugins.available` reply.
    pub fn update_cards(self: &Rc<Self>, cards: Vec<PluginCard>) {
        *self.cards.borrow_mut() = cards;
        self.repaint_cards();
    }

    /// The bind card is open in Connected apps → Requests: say where to look.
    pub fn bind_opened(&self) {
        self.bind_hint.set_visible(true);
    }

    /// One `bridge-card` per plugin (`third-party.md` § The principal model →
    /// *Hosted principals*), the shared `plugin_card`'s composition: name,
    /// badge and publisher as text, its verb as the card's
    /// `bridge-action-button` — Connect while unbound, Disconnect while bound,
    /// carrying the plugin's `client_id` and a `bound` value so a reader can
    /// tell the cards apart — and, once bound, its settings through the one
    /// settings renderer (`detail::build_settings_group`). tui leads
    /// (`apps/fauna-tui/src/bridges.rs::plugin_card_elements`).
    fn repaint_cards(self: &Rc<Self>) {
        while let Some(child) = self.cards_box.first_child() {
            self.cards_box.remove(&child);
        }
        let resolve = |text: &fauna_core::localized::LocalizedText| {
            text.resolve(crate::i18n::strings::lookup)
        };
        for card in self.cards.borrow().iter() {
            let item = gtk::Box::new(gtk::Orientation::Vertical, 4);
            item.add_css_class("card");
            set_test_id(&item, ids::BRIDGE_CARD);

            let name = gtk::Label::new(Some(&card.name));
            name.set_halign(gtk::Align::Start);
            name.set_xalign(0.0);
            name.add_css_class("heading");
            item.append(&name);

            let by_line = format!("{}\n{}", resolve(&card.badge), resolve(&card.publisher));
            let publisher = gtk::Label::new(Some(&by_line));
            publisher.set_halign(gtk::Align::Start);
            publisher.set_xalign(0.0);
            publisher.set_wrap(true);
            publisher.add_css_class("dim-label");
            item.append(&publisher);

            let (label, wire_kind, bound) = match &card.action {
                PluginCardAction::Connect => (t::PLUGIN_CONNECT, "fauna.plugins.bind", false),
                PluginCardAction::Disconnect { .. } => {
                    (t::PLUGIN_DISCONNECT, "fauna.principals.revoke", true)
                }
            };
            let action = gtk::Button::builder()
                .label(label)
                .halign(gtk::Align::End)
                .css_classes([if bound {
                    "destructive-action"
                } else {
                    "suggested-action"
                }])
                .build();
            set_test_id(&action, ids::BRIDGE_ACTION_BUTTON);
            set_test_attr_text(&action, "client_id", &card.client_id);
            set_test_attr(&action, "bound", if bound { "true" } else { "false" });
            crate::offline_gate::declare_wire_kind(&action, wire_kind);
            {
                let section = Rc::clone(self);
                let card = card.clone();
                action.connect_clicked(move |_| section.press(&card));
            }
            item.append(&action);

            if !card.settings.is_empty() {
                let client = Rc::clone(&self.client);
                let principal_id = card.principal_id.clone();
                item.append(&super::detail::build_settings_group(
                    &card.settings,
                    Rc::new(move |key, value| {
                        client.set_plugin_setting(principal_id.clone(), key.to_string(), value)
                    }),
                ));
            }
            self.cards_box.append(&item);
        }
    }

    /// The card's verb, through the connected-apps machine — the one approve
    /// path, whose bind card waits in Connected apps → Requests.
    fn press(&self, card: &PluginCard) {
        let Some(machine) = crate::settings::connected_apps::shared_machine() else {
            return;
        };
        self.bind_hint.set_visible(false);
        match &card.action {
            PluginCardAction::Connect => self
                .client
                .connect_plugin(machine, card.principal_id.clone()),
            PluginCardAction::Disconnect {
                binding_principal_id,
            } => self
                .client
                .disconnect_plugin(machine, binding_principal_id.clone()),
        }
    }

    /// Repaint from a fresh `fauna.plugins.list` reply.
    pub fn update(self: &Rc<Self>, rows: Vec<InstalledPluginRow>) {
        *self.rows.borrow_mut() = rows;
        *self.armed.borrow_mut() = None;
        self.repaint();
    }

    /// The install card is open in Connected apps → Requests: clear the draft
    /// and say where to look.
    pub fn install_opened(&self) {
        self.url_entry.set_text("");
        self.opened_hint.set_visible(true);
    }

    fn install(&self) {
        let url = self.url_entry.text().trim().to_string();
        if url.is_empty() {
            return;
        }
        self.opened_hint.set_visible(false);
        self.client.install_plugin(url);
    }

    fn repaint(self: &Rc<Self>) {
        while let Some(child) = self.items.first_child() {
            self.items.remove(&child);
        }
        let resolve = |text: &fauna_core::localized::LocalizedText| {
            text.resolve(crate::i18n::strings::lookup)
        };
        let rows = self.rows.borrow();
        if rows.is_empty() {
            let empty = gtk::Label::new(Some(t::PLUGINS_EMPTY));
            empty.set_halign(gtk::Align::Start);
            empty.add_css_class("dim-label");
            self.items.append(&empty);
        }
        let armed = self.armed.borrow().clone();
        for row in rows.iter() {
            let item = gtk::Box::new(gtk::Orientation::Vertical, 2);
            item.add_css_class("card");
            set_test_id(&item, ids::BRIDGE_PLUGIN_ITEM);

            let name = gtk::Label::new(Some(&row.name));
            name.set_halign(gtk::Align::Start);
            name.add_css_class("heading");
            set_test_id(&name, ids::BRIDGE_PLUGIN_NAME);
            set_test_attr_text(&name, "client_id", &row.client_id);
            item.append(&name);

            let publisher = gtk::Label::new(Some(&resolve(&plugin_publisher_text(row))));
            publisher.set_halign(gtk::Align::Start);
            publisher.set_xalign(0.0);
            publisher.set_wrap(true);
            set_test_id(&publisher, ids::BRIDGE_PLUGIN_PUBLISHER);
            item.append(&publisher);

            let status_text = plugin_status_lines(row)
                .iter()
                .map(resolve)
                .collect::<Vec<_>>()
                .join("\n");
            let status = gtk::Label::new(Some(&status_text));
            status.set_halign(gtk::Align::Start);
            status.set_xalign(0.0);
            status.set_wrap(true);
            status.add_css_class("dim-label");
            set_test_id(&status, ids::BRIDGE_PLUGIN_STATUS);
            // `state` is read off the marker class (`automation::agent::attr`),
            // which takes a CSS-identifier-safe token: the runner's own states are
            // already one, and one a newer nest invents is flattened to it.
            set_test_attr(&status, "state", &css_token(&row.state));
            item.append(&status);

            let is_armed = armed.as_deref() == Some(row.principal_id.as_slice());
            if is_armed {
                let prompt = gtk::Label::new(Some(&t::plugin_uninstall_prompt(&row.name)));
                prompt.set_halign(gtk::Align::Start);
                prompt.set_xalign(0.0);
                prompt.set_wrap(true);
                item.append(&prompt);
            }
            let actions = gtk::Box::new(gtk::Orientation::Horizontal, 6);
            actions.set_halign(gtk::Align::End);
            let uninstall = gtk::Button::builder()
                .label(if is_armed {
                    t::PLUGIN_UNINSTALL_CONFIRM
                } else {
                    t::PLUGIN_UNINSTALL
                })
                .css_classes(["destructive-action"])
                .build();
            set_test_id(&uninstall, ids::BRIDGE_PLUGIN_UNINSTALL);
            crate::offline_gate::declare_wire_kind(&uninstall, "fauna.plugins.uninstall");
            {
                let section = Rc::clone(self);
                let principal_id = row.principal_id.clone();
                uninstall.connect_clicked(move |_| {
                    let section = Rc::clone(&section);
                    let principal_id = principal_id.clone();
                    // Repainting removes this very button, so defer past the
                    // click's own emission.
                    glib::idle_add_local_once(move || section.uninstall(principal_id));
                });
            }
            actions.append(&uninstall);
            if is_armed {
                let keep = gtk::Button::builder()
                    .label(t::PLUGIN_UNINSTALL_CANCEL)
                    .css_classes(["flat"])
                    .build();
                let section = Rc::clone(self);
                keep.connect_clicked(move |_| {
                    let section = Rc::clone(&section);
                    glib::idle_add_local_once(move || {
                        *section.armed.borrow_mut() = None;
                        section.repaint();
                    });
                });
                actions.append(&keep);
            }
            item.append(&actions);
            self.items.append(&item);
        }
    }

    /// The first press arms the inline confirm; the armed press uninstalls
    /// (the reply re-reads the list).
    fn uninstall(self: &Rc<Self>, principal_id: Vec<u8>) {
        if self.armed.borrow().as_deref() != Some(principal_id.as_slice()) {
            *self.armed.borrow_mut() = Some(principal_id);
            self.repaint();
            return;
        }
        *self.armed.borrow_mut() = None;
        self.client.uninstall_plugin(principal_id);
    }
}
