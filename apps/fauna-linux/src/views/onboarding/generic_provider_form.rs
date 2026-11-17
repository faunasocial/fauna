//! Generic per-provider credential-form helpers used by `dns_config.rs`
//! and `vps_config.rs`. These pages own their own page-level structure
//! (link, help, verify button, status, etc.) and call into the helpers
//! here just to render the per-field input rows.
//!
//! Two entry points:
//!
//! - [`build_creds_only`] returns a fresh `gtk::Box` populated with input
//!   rows. Use when constructing a one-shot section.
//!
//! - [`populate_creds_into`] appends rows to an *existing* container
//!   without removing/re-adding it. Used by `dns_config.rs`'s stable
//!   skeleton (the `dns-credentials-form` Box stays in the AT-SPI tree
//!   across provider changes; only its inner Entry rows are rebuilt).
//!
//! Each field's test ID is the canonical cross-app
//! `{kind}-credentials-form-{field.id}` (e.g. `dns-credentials-form-api-token`,
//! `vps-credentials-form-secret-api-key`) — matching web / macOS / iOS /
//! Android and the `tests/e2e-unified/ui.yaml` `{dns,vps}-credentials-form`
//! components, so the cross-app onboarding e2e resolves the field globally.
//! The cred *key* passed to `m.set_dns_cred(field.id, …)` stays the raw
//! `field.id`. (The admin DNS-credentials form is a separate surface that keeps
//! the bare `field.id` per its own ui.yaml contract — it does not use this
//! builder.) The legacy `setup-{kind}-{provider}-{field}` ID shape is gone —
//! it belonged to the deleted 5-step wizard.

use adw::prelude::*;
use fauna_onboarding_machine::{CredentialForm, FieldMetaPlain, FieldTypePlain, OnboardingMachine};
use std::sync::Arc;

use crate::i18n::resolve_key as resolve;

/// A `hosted-auth` field's button, handed back to the caller so its per-tick
/// refresh closure can repaint the label/sensitivity from the machine's
/// `hosted_auth_state`/`hosted_auth_can_begin` — mirrors tui's
/// `hosted_auth_button`, but GTK is retained-mode: the widget is built once
/// here and repainted by [`refresh_hosted_auth_buttons`] rather than
/// re-derived on every render (`onboarding.md` § 4).
pub struct HostedAuthHandle {
    pub field_id: String,
    pub button: gtk::Button,
}

/// Which credential set a form writes: the `dns_config` / `vps_config` forms,
/// or one provider's dialog on the choose-providers page
/// (`onboarding-provisioning.md` § 3d). The target names the field-id prefix
/// (`{dns,vps,provider}-credentials-form-{field.id}`, ui.yaml) and routes every
/// write and every hosted sign-in to the matching machine method — one
/// renderer serves all three forms, so they cannot drift apart.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CredTarget {
    Dns,
    Vps,
    Provider(String),
}

impl CredTarget {
    fn prefix(&self) -> &'static str {
        match self {
            CredTarget::Dns => "dns",
            CredTarget::Vps => "vps",
            CredTarget::Provider(_) => "provider",
        }
    }

    /// The built forms' hosted-auth slot; `None` for a provider dialog.
    fn form(&self) -> Option<CredentialForm> {
        match self {
            CredTarget::Dns => Some(CredentialForm::Dns),
            CredTarget::Vps => Some(CredentialForm::Vps),
            CredTarget::Provider(_) => None,
        }
    }

    fn set(&self, m: &OnboardingMachine, field_id: String, value: String) {
        match self {
            CredTarget::Dns => m.set_dns_cred(field_id, value),
            CredTarget::Vps => m.set_vps_cred(field_id, value),
            CredTarget::Provider(id) => m.set_provider_cred(id.clone(), field_id, value),
        }
    }

    fn button_text(&self, m: &OnboardingMachine, field_id: &str) -> String {
        match (self, self.form()) {
            (CredTarget::Provider(id), _) => {
                m.provider_hosted_auth_button_text(id.clone(), field_id.into())
            }
            (_, Some(form)) => m.hosted_auth_button_text(form, field_id.into()),
            (_, None) => unreachable!("only a provider target has no built form"),
        }
    }

    fn can_begin(&self, m: &OnboardingMachine, field_id: &str) -> bool {
        match (self, self.form()) {
            (CredTarget::Provider(id), _) => {
                m.provider_hosted_auth_can_begin(id.clone(), field_id.into())
            }
            (_, Some(form)) => m.hosted_auth_can_begin(form, field_id.into()),
            (_, None) => unreachable!("only a provider target has no built form"),
        }
    }

    /// Begin the hosted sign-in, open its page, wait for it. Errors are
    /// already in the field's `HostedAuthState::Failed` (painted as the button
    /// label on the next tick) — nothing to re-derive here.
    async fn sign_in(&self, m: &OnboardingMachine, field_id: String) {
        match (self, self.form()) {
            (CredTarget::Provider(id), _) => {
                if let Ok(prompt) = m
                    .provider_hosted_auth_begin(id.clone(), field_id.clone())
                    .await
                {
                    crate::url_opener::open(&prompt.verification_url);
                    let _ = m.provider_hosted_auth_wait(id.clone(), field_id).await;
                }
            }
            (_, Some(form)) => {
                if let Ok(prompt) = m.hosted_auth_begin(form, field_id.clone()).await {
                    crate::url_opener::open(&prompt.verification_url);
                    let _ = m.hosted_auth_wait(form, field_id).await;
                }
            }
            (_, None) => {}
        }
    }
}

/// Repaint one hosted-auth button's label + sensitivity from the machine's
/// state. Nothing here is re-derived: both come straight from the machine's
/// getters, same as tui's `hosted_auth_button`.
fn paint_hosted_auth_button(
    button: &gtk::Button,
    m: &OnboardingMachine,
    target: &CredTarget,
    field_id: &str,
) {
    button.set_label(&target.button_text(m, field_id));
    button.set_sensitive(target.can_begin(m, field_id));
}

/// Per-tick repaint for every hosted-auth button a form is currently
/// showing. Called from the page's own refresh closure — the generic
/// renderer here owns no tick of its own (`dns_config.rs`'s
/// `update_provider_section_state`, `vps_config.rs`'s twin and
/// `provider_choice.rs`'s dialog).
pub fn refresh_hosted_auth_buttons(
    handles: &[HostedAuthHandle],
    m: &OnboardingMachine,
    target: &CredTarget,
) {
    for h in handles {
        paint_hosted_auth_button(&h.button, m, target, &h.field_id);
    }
}

/// Build a fresh credentials Box from a `visible_*_fields()` snapshot.
/// `target` routes field-change handlers to `m.set_dns_cred(...)`,
/// `m.set_vps_cred(...)` or `m.set_provider_cred(...)`.
///
/// The returned Box uses `accessible_role(Group)` so AT-SPI exposes it
/// to test bridges. Callers that need a stable container across provider
/// changes should construct the Box themselves and use [`populate_creds_into`]
/// to fill it. The second element is one [`HostedAuthHandle`] per
/// `hosted-auth` field the fields list carried, for the caller's own
/// per-tick refresh.
pub fn build_creds_only(
    fields: &[FieldMetaPlain],
    m: Arc<OnboardingMachine>,
    target: &CredTarget,
) -> (gtk::Box, Vec<HostedAuthHandle>) {
    let container = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(8)
        .accessible_role(gtk::AccessibleRole::Group)
        .build();
    let handles = populate_creds_into(&container, fields, m, target);
    (container, handles)
}

/// Append per-field labeled rows to an existing container — an Entry for
/// most field types, a button for `hosted-auth` (the bundled provider's
/// hosted sign-in, `onboarding.md` § 4). Used by `dns_config.rs` to populate
/// a stable `dns-credentials-form` Box on provider change without
/// removing/re-adding the outer container — the container stays in the
/// AT-SPI tree, so `wait_for("dns-credentials-form")` resolves immediately.
/// Caller is responsible for clearing the container's children before
/// calling this if it had previous content. Returns one [`HostedAuthHandle`]
/// per `hosted-auth` field built, so the caller can repaint them on its own
/// per-tick refresh.
pub fn populate_creds_into(
    container: &gtk::Box,
    fields: &[FieldMetaPlain],
    m: Arc<OnboardingMachine>,
    target: &CredTarget,
) -> Vec<HostedAuthHandle> {
    let mut handles = Vec::new();
    for field in fields {
        // accessible_role(Group) on the row Box so AT-SPI walks into
        // it. Plain gtk::Box::new defaults to role Generic, which some
        // compositors omit from the AT-SPI tree.
        let row = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(4)
            .accessible_role(gtk::AccessibleRole::Group)
            .build();

        let label = gtk::Label::new(Some(&resolve(&field.label_key)));
        label.set_halign(gtk::Align::Start);
        row.append(&label);

        // Canonical cross-app field test ID: `{kind}-credentials-form-
        // {field.id}` (matches web/apple/android/tui + ui.yaml).
        let test_id = format!("{}-credentials-form-{}", target.prefix(), field.id);

        if field.field_type == FieldTypePlain::HostedAuth {
            // A `hosted-auth` field is a button, not an input — the bundled
            // provider's hosted sign-in, tui's `hosted_auth_button` shape
            // one-to-one: same derived id, same begin → open → wait
            // sequence. Label + pressability come from the machine and are
            // repainted every tick by `refresh_hosted_auth_buttons`.
            let button = gtk::Button::new();
            crate::testid::set_test_id(&button, &test_id);
            paint_hosted_auth_button(&button, &m, target, &field.id);
            {
                let m = m.clone();
                let field_id = field.id.clone();
                let target = target.clone();
                button.connect_clicked(move |_| {
                    let m = m.clone();
                    let field_id = field_id.clone();
                    let target = target.clone();
                    crate::async_helper::run_on_tokio(
                        async move { target.sign_in(&m, field_id).await },
                        |_| {},
                    );
                });
            }
            row.append(&button);
            container.append(&row);
            handles.push(HostedAuthHandle {
                field_id: field.id.clone(),
                button,
            });
            continue;
        }

        let entry = gtk::Entry::builder().hexpand(true).build();
        // `Secret` degrades to a masked entry; `HostedAuth` never reaches
        // here now that it renders as the button above.
        if field.field_type == FieldTypePlain::Secret {
            entry.set_visibility(false);
        }
        // The cred *key* passed to the target's setter below stays the raw
        // field.id.
        crate::testid::set_test_id(&entry, &test_id);
        {
            let m = m.clone();
            let field_id = field.id.clone();
            let target = target.clone();
            entry.connect_changed(move |e| {
                target.set(&m, field_id.clone(), e.text().to_string());
            });
        }
        row.append(&entry);
        container.append(&row);
    }
    handles
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_onboarding_machine::OnboardingMachine;
    use fauna_onboarding_machine::observer::NullObserver;
    use fauna_provisioning::{PROVIDERS, ProviderId};

    /// Walk the container subtree collecting every `gtk::Entry`.
    fn collect_entries(root: &gtk::Box) -> Vec<gtk::Entry> {
        fn walk(w: &gtk::Widget, out: &mut Vec<gtk::Entry>) {
            if let Ok(e) = w.clone().downcast::<gtk::Entry>() {
                out.push(e);
                return;
            }
            let mut c = w.first_child();
            while let Some(child) = c {
                walk(&child, out);
                c = child.next_sibling();
            }
        }
        let mut out = Vec::new();
        let mut child = root.first_child();
        while let Some(w) = child {
            walk(&w, &mut out);
            child = w.next_sibling();
        }
        out
    }

    /// Verify `populate_creds_into` produces one Entry per declared field
    /// for Porkbun, with each Entry's widget_name set to the canonical
    /// cross-app `dns-credentials-form-{field.id}` (matching web/apple/
    /// android + ui.yaml). Belt-and-suspenders for the providers.yaml →
    /// ui.yaml pipeline.
    #[test]
    fn porkbun_creds_form_widget_ids_match_declared_fields() {
        crate::testid::run_on_gtk_thread(|| {
            let porkbun = PROVIDERS
                .iter()
                .find(|p| p.id == ProviderId::Porkbun)
                .unwrap();
            // Synthesize the visible-fields list the live code passes to
            // populate_creds_into (the field-visibility filter doesn't apply
            // for porkbun's all-Dns fields, so taking them all is correct).
            let visible: Vec<FieldMetaPlain> =
                porkbun.fields.iter().map(FieldMetaPlain::from).collect();
            let m = OnboardingMachine::new(Arc::new(NullObserver));
            let (form, _handles) = build_creds_only(&visible, m, &CredTarget::Dns);
            let entries = collect_entries(&form);
            assert_eq!(entries.len(), porkbun.fields.len(), "one Entry per field");
            for field in porkbun.fields.iter() {
                let expected = format!("dns-credentials-form-{}", field.id);
                assert!(
                    entries.iter().any(|e| e.widget_name() == expected.as_str()),
                    "missing Entry with widget_name = {:?}; have: {:?}",
                    expected,
                    entries
                        .iter()
                        .map(|e| e.widget_name().to_string())
                        .collect::<Vec<_>>(),
                );
            }
        });
    }
}
