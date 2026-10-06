//! The `hosted-auth` credential field's sign-in — a bundled provider's RFC 8628
//! device-authorization flow (`bundled-provider-api.md` § Authentication).
//!
//! The field type (`registry.md` § Bundled provider) is a button, not an input:
//! the app runs the provider's device flow and the resulting Bearer token
//! becomes the field's credential value. Two machines render such a form —
//! the onboarding wizard's `dns_config`/`vps_config` ([`crate::OnboardingMachine`])
//! and the retire view's credential state ([`crate::NestRetireMachine`], which
//! reuses `vps_config`'s controls by id) — so the flow itself lives here, once,
//! and each machine keeps only where the state and the token land.

use fauna_provisioning::bundled_api::{
    DevicePoll, checked_base_url, device_authorize, device_token,
};

use crate::state::{HostedAuthPrompt, HostedAuthState};

/// The credential field a hosted sign-in reads its authorization server from:
/// the sibling `base-url` text field (`registry.md` § Bundled provider). A
/// future curated intermediary with a fixed base would carry it on its
/// registry entry instead; today every hosted-auth provider is BYO.
pub(crate) const BASE_URL_FIELD: &str = "base-url";

/// A device-authorization attempt in flight between [`begin`] and [`wait`].
pub(crate) struct PendingDeviceAuth {
    base_url: String,
    device_code: String,
    interval_secs: u64,
    /// RFC 8628 `expires_in` ÷ `interval` — the poll budget. Counting polls
    /// rather than reading a clock keeps the flow identical on every target
    /// (`fauna_sleep` is the one time primitive shared Rust has on wasm too).
    polls_left: u64,
}

/// Step 1: validate the typed base URL and POST the device-authorization
/// request. `Ok` carries what the app must open and show, plus the attempt to
/// hand to [`wait`]; `Err` is the field's failure message.
pub(crate) async fn begin(
    http: &reqwest::Client,
    raw_base: Option<String>,
) -> Result<(HostedAuthPrompt, PendingDeviceAuth), String> {
    let Some(raw_base) = raw_base else {
        return Err("enter the provider address before signing in".into());
    };
    // Spec § Endpoints requires `https://` — reject here, as a field error on
    // the credential form, rather than let an unchecked scheme ride the
    // device-authorization POST that follows.
    let base = checked_base_url(&raw_base).map_err(|e| e.to_string())?;
    let d = device_authorize(http, &base)
        .await
        .map_err(|e| e.to_string())?;
    let prompt = HostedAuthPrompt {
        verification_url: d.open_url().to_string(),
        user_code: d.user_code.clone(),
    };
    let interval_secs = d.interval.max(1);
    let polls_left = (d.expires_in / interval_secs).max(1);
    Ok((
        prompt,
        PendingDeviceAuth {
            base_url: base,
            device_code: d.device_code,
            interval_secs,
            polls_left,
        },
    ))
}

/// Step 2: poll the token endpoint at the server's interval until the user
/// approves (`Ok(token)`) or the attempt ends (`Err(message)`).
///
/// ⚠ The step markers below are not debug leftovers, and they are the
/// SHARED-RUST half of a diagnosis every app pays for. Seen from outside a
/// stalled device flow looks like one thing: the provider fake receives no
/// `/auth/token` request. Three different failures produce that picture — the
/// future is never polled again after the sleep registers, the sleep returns
/// but the HTTP call never reaches the wire, or the poll runs fine and the
/// provider keeps answering `Pending` — and one line either side of each await
/// separates them. Redaction (observability.md § Persistence & privacy):
/// counters and the response VARIANT only — never the device code, the base
/// URL, or the token.
pub(crate) async fn wait(
    http: &reqwest::Client,
    mut pending: PendingDeviceAuth,
    field_label: &str,
) -> Result<String, String> {
    let mut attempt = 0u32;
    tracing::info!(
        target: "fauna_onboarding",
        "hosted-auth wait: entered ({field_label}, {} polls left, {}s interval)",
        pending.polls_left, pending.interval_secs,
    );
    loop {
        if pending.polls_left == 0 {
            return Err("the sign-in code expired before it was approved — try again".into());
        }
        pending.polls_left -= 1;
        attempt += 1;
        tracing::info!(
            target: "fauna_onboarding",
            "hosted-auth wait: sleeping {}s before poll {attempt}",
            pending.interval_secs,
        );
        fauna_sleep::sleep(std::time::Duration::from_secs(pending.interval_secs)).await;
        tracing::info!(
            target: "fauna_onboarding",
            "hosted-auth wait: woke; issuing poll {attempt}",
        );
        let answer = device_token(http, &pending.base_url, &pending.device_code).await;
        tracing::info!(
            target: "fauna_onboarding",
            "hosted-auth wait: poll {attempt} answered {}",
            match &answer {
                Ok(DevicePoll::Token(_)) => "Token",
                Ok(DevicePoll::Pending) => "Pending",
                Ok(DevicePoll::SlowDown) => "SlowDown",
                Ok(DevicePoll::Expired) => "Expired",
                Ok(DevicePoll::Denied) => "Denied",
                Err(_) => "transport error",
            },
        );
        match answer {
            Ok(DevicePoll::Token(token)) => return Ok(token),
            Ok(DevicePoll::Pending) => continue,
            // RFC 8628 § 3.5: back off by 5 s and keep going.
            Ok(DevicePoll::SlowDown) => pending.interval_secs += 5,
            Ok(DevicePoll::Expired) => {
                return Err("the sign-in code expired before it was approved — try again".into());
            }
            Ok(DevicePoll::Denied) => return Err("the sign-in was declined at the provider".into()),
            Err(e) => return Err(e.to_string()),
        }
    }
}

/// The `hosted-auth` button's label, derived purely from its state — one
/// mapping for every machine that renders such a field (`onboarding.md` § 4).
pub fn button_text(state: &HostedAuthState) -> String {
    use fauna_i18n::strings::provisioning::hosted_auth as h;
    match state {
        HostedAuthState::Idle => h::CONNECT.to_string(),
        HostedAuthState::Pending { user_code, .. } => h::pending(user_code),
        HostedAuthState::Connected => h::CONNECTED.to_string(),
        HostedAuthState::Failed { message } => h::failed(message),
    }
}
