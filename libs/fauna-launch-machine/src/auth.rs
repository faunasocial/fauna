//! Pre-identity **WS-RPC** auth ceremony for the launch flow — the
//! `fauna.auth.challenge` + `fauna.auth.verify` kinds on the anonymous
//! connection (`GET /api/v1/ws`, no bearer; transport.md § Pre-identity).
//!
//! **One ceremony for every bearer this machine mints** — the launch sign-in,
//! the TTL-scheduled refresh and the 401-reactive refresh alike (`login.md`
//! § When to use which). The refreshes used to take the single-round-trip
//! `fauna.auth.handshake`, whose client timestamp the nest holds to ±30 s; on a
//! device whose clock is hours wrong that bounced a freshly signed-in app the
//! moment its first refresh fired, while the launch — nonce-signed, no
//! timestamp — had sailed through. The silent challenge is immune to client
//! clock skew by construction, and an extra round trip per hour is nothing, so
//! the handshake path was removed from this crate (2026-09-21) rather than kept
//! beside its clock-immune twin.
//!
//! Two layers:
//!  * `run_silent_challenge<R>` — the generic ceremony over any
//!    `RpcRequester`, shared from `fauna_protocol::auth` and re-exported here;
//!    the real WS round-trip is proven by the tier_3
//!    `bins/fauna-nest/tests/launch_machine_auth_roundtrip.rs`.
//!  * `connect_silent_challenge` — the per-target connect-then-run wrapper
//!    (native `fauna-anon-client`, wasm `fauna-rpc-wasm`), mirroring the split
//!    in `probe.rs`. The production [`crate::connector::WsAuthConnector`] calls
//!    it.
//!
//! **Per-request `client_nonce`:** natively the connector folds a fresh nonce
//! into the nest's cert-binding proof and graduates the verify reply against the
//! captured TLS cert (channel binding, security.md § Transport trust, Axis 1); on
//! wasm the browser validates TLS itself, so the `cert_binding` reply is ignored.

// The silent-challenge ceremony (`fauna.auth.{challenge,verify}`) lives in
// `fauna-protocol::auth` so `fauna-onboarding-machine`'s handle-check probe can
// share it without forming the `fauna-client → fauna-nest-http →
// fauna-launch-machine` Cargo cycle. Re-export it so this crate's public surface
// (`fauna_launch_machine::SilentChallengeOutcome`, consumed by `fauna-nest-http`
// + the `AuthConnector` trait below) is unchanged.
pub use fauna_protocol::auth::SilentChallengeOutcome;

// ---------------------------------------------------------------------------
// Outcome types
// ---------------------------------------------------------------------------

/// What a **post-auth background** silent challenge learned, classified for the
/// one decision `security.md` § Post-auth surfacing asks of every app:
/// *does this escalate to the blocking `launch_identity_changed` surface, or
/// stay logged-and-swallowed?*
///
/// Variants rather than a `Result`, deliberately. An escalating verdict must be
/// reachable **only** through its own arm: encoded as an `Err` it is
/// indistinguishable at the call site from a network blip, which is exactly how
/// it used to be swallowed on every app. [`Self::Failed`] is the explicit
/// "logged and swallowed" arm, so a caller that handles every arm cannot
/// accidentally lump an escalating verdict in with the faults. Three arms
/// escalate — [`Self::IdentityChanged`], [`Self::Superseded`] and
/// [`Self::NotRegistered`] — and they escalate for the same underlying reason:
/// the session is already de-facto dead.
#[derive(Debug, Clone)]
pub enum SilentSignInVerdict {
    /// The nest confirmed the identity; refresh the cached server data.
    Refreshed {
        handle: String,
        domain: String,
        tier: String,
    },
    /// `fauna.auth.not_registered` — the nest no longer signs this identity
    /// in: its user was suspended or removed while signed in (the client
    /// cannot tell which — no oracle on the wire). **The third outcome that
    /// escalates**, for [`Self::Superseded`]'s reason: every connection this
    /// identity opens from here on is refused. This classifier serves the
    /// post-auth background refresh only, so the refusal is never onboarding's
    /// "not registered yet"; the client lands the launch surface's
    /// previously-signed-in row, whose Retry is the way back in after the
    /// admin's restore (`onboarding.md` § App-launch routing).
    NotRegistered,
    /// The nest's pinned deployment identity changed mid-session. **The one
    /// outcome that escalates.** The session is already de-facto dead (its
    /// connections can no longer graduate), so the client blocks on the launch
    /// surface rather than painting a banner over a broken session.
    IdentityChanged,
    /// Transport fault, invalid secret, or a degraded nest — every failure class
    /// that is *not* an identity verdict. Logged and swallowed by the caller: a
    /// background refresh must never tear down a healthy session over a fault.
    Failed { error: String },
    /// The identity was succeeded mid-session. **The second outcome that
    /// escalates**, and for the same reason [`Self::IdentityChanged`] does: the
    /// session is already de-facto dead — every connection this identity opens
    /// from here on is refused — so the client blocks on the launch surface
    /// rather than painting a banner over a session that cannot work. Unlike
    /// the MITM verdict, the way out is not re-trust but importing the
    /// successor (`identity-succession.md` § Propagation → *Own device fleet*).
    Superseded { new_actor_id_hex: String },
    /// The account is locked out until `locked_until_secs`
    /// (`fauna.auth.account_locked`). **Terminal, not swallowed**: every
    /// connection this identity opens before then is refused, so — like
    /// [`Self::Superseded`] — the caller blocks on the locked surface rather
    /// than logging it as a fault.
    Locked { locked_until_secs: u64 },
}

/// Classify a background [`SilentChallengeOutcome`] — the pure whole of
/// `security.md` § Post-auth surfacing's client-side rule: **only the
/// session-ending verdicts escalate; every other failure class stays
/// swallowed.**
///
/// Shared rather than per-app because this function *is* the rule, and a rule
/// each of seven app shells re-derives is a rule that drifts. It was app-local
/// on linux (the first wired leg, 2026-07-23) until tui became the second
/// consumer; the seam's unreachability is what makes such rules recur.
///
/// The network call is deliberately left outside, so the whole classification is
/// testable with no nest — the same split as `LaunchMachine`'s other pure halves.
pub fn classify_silent_challenge(outcome: SilentChallengeOutcome) -> SilentSignInVerdict {
    match outcome {
        SilentChallengeOutcome::Success(v) => SilentSignInVerdict::Refreshed {
            handle: v.handle,
            domain: v.domain,
            tier: v.tier,
        },
        SilentChallengeOutcome::NotRegistered => SilentSignInVerdict::NotRegistered,
        SilentChallengeOutcome::Transient { error }
        | SilentChallengeOutcome::SecretInvalid { error } => SilentSignInVerdict::Failed { error },
        // `fauna.nest.outdated` — a degraded nest; the localized actionable
        // message rides the swallowed path. A distinct "update required" surface
        // for the *post-auth* case is a separate follow-on; what matters here is
        // that it is not a MITM verdict.
        SilentChallengeOutcome::NeedsUpdate { message } => {
            SilentSignInVerdict::Failed { error: message }
        }
        SilentChallengeOutcome::IdentityChanged {
            host,
            pinned_hex,
            seen_hex,
            fork,
        } => {
            tracing::error!(
                "[identity] nest identity changed for {host} (pinned {pinned_hex}, seen \
                 {seen_hex:?}, fork evidence: {fork}) — blocking the session until the user \
                 re-trusts or walks away"
            );
            SilentSignInVerdict::IdentityChanged
        }
        SilentChallengeOutcome::Superseded { new_actor_id_hex } => {
            tracing::error!(
                "[identity] this identity was succeeded (successor {new_actor_id_hex}) — blocking \
                 the session; the user imports the new identity"
            );
            SilentSignInVerdict::Superseded { new_actor_id_hex }
        }
        SilentChallengeOutcome::Locked { locked_until_secs } => {
            tracing::error!(
                "[identity] this account is locked until {locked_until_secs} — blocking the \
                 session on the locked surface"
            );
            SilentSignInVerdict::Locked { locked_until_secs }
        }
    }
}

// ---------------------------------------------------------------------------
// Per-target connect-then-run wrappers (mirrors probe.rs)
//
// The silent-challenge ceremony itself (`run_silent_challenge`) is shared from
// `fauna-protocol::auth` (re-exported above); these wrappers add the per-target
// `connect`.
// ---------------------------------------------------------------------------

// The "is this graduation failure the identity-changed surface?" table used to
// live here as a private fn. It moved to `fauna_anon_client::
// classify_identity_changed` when the bearer mint became its second consumer
// (`security.md` § Post-auth surfacing's taxonomy leg): the mint path needs the
// identical rule, and two copies of a rule this security-load-bearing is exactly
// the drift `classify_silent_challenge`'s own doc comment warns about.

/// The socket address a reach hint names for `nest_url` — the hint's IP with
/// the URL's own port, so a dev nest on `:8443` is dialled there and not on 443.
///
/// `None` for a hint that is not a bare IPv4 literal: the hint is a fallback and
/// a malformed one must degrade to "no fallback", never to a dial of something
/// else. Parsing it here (rather than trusting the store) is what makes that
/// true for every caller.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn hint_socket_addr(nest_url: &str, reach_ipv4: &str) -> Option<std::net::SocketAddr> {
    let ip: std::net::Ipv4Addr = reach_ipv4.parse().ok()?;
    let authority = fauna_anon_client::authority_of(nest_url);
    let port = authority
        .rsplit_once(':')
        .and_then(|(_, p)| p.parse::<u16>().ok())
        .unwrap_or(
            // `authority_of` strips four schemes, so a portless `wss://` URL
            // reaches here too and is a TLS URL like `https://` — defaulting it
            // to 80 would dial the wrong port. Not live (the launch path stores
            // `https://{domain}`), fixed at the seam so it cannot become live.
            if nest_url.starts_with("https://") || nest_url.starts_with("wss://") {
                443
            } else {
                80
            },
        );
    Some(std::net::SocketAddr::from((ip, port)))
}

#[cfg(not(target_arch = "wasm32"))]
pub(crate) async fn connect_silent_challenge(
    nest_url: &str,
    reach_ipv4: Option<&str>,
    secret: &[u8],
) -> SilentChallengeOutcome {
    // The hint changes only where the socket goes. `nest_url` is passed through
    // unchanged, so SNI, the `Host` header, the cert the peer must present and
    // the pin key all stay the domain's — which is what makes the later switch
    // to a resolving domain a no-op for identity (`security.md` § Transport
    // trust) rather than a second TOFU question.
    let resolve = reach_ipv4.and_then(|ip| hint_socket_addr(nest_url, ip));
    let client =
        match fauna_anon_client::AnonymousNestClient::connect_resolving(nest_url, resolve).await {
            Ok(c) => c,
            Err(e) => {
                return SilentChallengeOutcome::Transient {
                    error: e.to_string(),
                };
            }
        };
    // The identity this login binds — read off THIS connection before anything
    // is signed (`login.md` § Binding the nest; SPKI-compared on TLS, so a
    // relaying box cannot present the real nest's identity over its own
    // channel). A refusal classifies exactly as the ceremony's own would (a
    // degraded nest's `fauna.nest.outdated` → the update prompt); a box that
    // proves no identity, or fails the binding, is an untrusted connection —
    // retryable, never gone Online over.
    let nest_id = match fauna_anon_client::read_login_binding(&client).await {
        Ok(id) => id,
        Err(fauna_anon_client::AnonClientError::Trust(e)) => {
            return SilentChallengeOutcome::Transient {
                error: format!("nest identity binding: {e}"),
            };
        }
        Err(e) => return fauna_protocol::auth::silent_challenge_error(&e),
    };
    // Fold a fresh client nonce into the nest's cert-binding proof (the NT-1
    // hardening) so the verify reply can be graduated below.
    let client_nonce = fauna_anon_client::fresh_nonce();
    let (outcome, challenge_nonce) = fauna_protocol::auth::run_silent_challenge_with_nonce(
        &client,
        secret,
        Some(&client_nonce),
        &nest_id,
    )
    .await;
    // Graduate the verify reply's channel binding before trusting the minted
    // bearer — the same graduation `fauna_client::ws_challenge_bearer` runs on
    // its handshake reply (security.md § Transport trust + § Connection-teardown
    // rule). Before this, the launch path minted and USED a bearer with no
    // identity check at all: an impersonated self-signed/LAN nest was spoken to
    // until the first token refresh or content connect caught it. Only on
    // success over `https://` — a plaintext loopback dev nest has no cert to
    // bind. Every refresh runs through here too, so a mid-session identity
    // change is caught on the same graduation.
    if let SilentChallengeOutcome::Success(ref reply) = outcome
        && nest_url.starts_with("https://")
    {
        let host = fauna_anon_client::authority_of(nest_url);
        let Some(chal) = challenge_nonce else {
            // Success without a challenge nonce is unreachable (the nonce
            // is decoded before the verify leg) — treat as untrusted.
            return SilentChallengeOutcome::Transient {
                error: "nest channel binding: missing challenge nonce".into(),
            };
        };
        if let Err(e) = fauna_anon_client::graduate_verify_path(
            &client,
            &host,
            &client.captured_cert(),
            &chal,
            &client_nonce,
            reply.cert_binding.as_deref(),
        )
        .await
        {
            if let Some(v) = fauna_anon_client::classify_identity_changed(&e, &host) {
                return SilentChallengeOutcome::IdentityChanged {
                    host,
                    pinned_hex: v.pinned_hex,
                    seen_hex: v.seen_hex,
                    fork: v.fork,
                };
            }
            // Untrusted connection: never go Online over it. Retryable
            // (first-contact binding trouble / a DNS `self=` rotation
            // mid-propagation), per the failure table's "offer Retry"
            // stance — the bearer is dropped either way.
            return SilentChallengeOutcome::Transient {
                error: format!("nest channel binding: {e}"),
            };
        }
    }
    outcome
}

/// The URL a reach hint dials on wasm: `nest_url` with its host swapped for the
/// hint's IP, port and scheme untouched.
///
/// A browser exposes no resolver seam — there is no `.resolve()` to install — so
/// unlike the native half the *authority* has to change, which is why web needs
/// the IP bridge cert (`onboarding.md` § Reach hint). What must **not** move
/// with it is the identity key: this URL reaches the socket and nothing else
/// (see [`connect_silent_challenge`]'s wasm arm). `None` for a hint that is not
/// a bare IPv4 literal, exactly as natively: a malformed hint is no fallback,
/// never a different dial.
///
/// Compiled on **every** target although only the wasm arm calls it: the rule is
/// pure string logic, and gating it to `wasm32` would put it where no test in
/// this workspace can execute it — the same reasoning as the pre-claim
/// `WsNestApi::dial_url` it mirrors, and exactly the combination that let this
/// arm's pin-key swap ship unseen.
///
/// Composes `fauna_core::web`'s shared authority cut —
/// [`fauna_core::web::generic_authority`], then [`fauna_core::web::strip_userinfo`],
/// then [`fauna_core::web::split_host_port`] for the port — rather than a
/// private `/`-only split. The authority ends at the first of `/ \ ? #`, and
/// any `userinfo@` is dropped before the port is read, so a `?`/`#`-bearing
/// `nest_url` can no longer have its query or fragment read as a userinfo/host
/// pair and swapped for the real host, nor keep a stale userinfo that silently
/// defeats the hint (`security.md` § Transport trust). The production caller only ever passes the onboarding wizard's
/// `https://{handle_domain}`, with the domain still the pin key
/// (`connect_silent_challenge`'s wasm arm above), but the cut is unconditional
/// so a future caller inherits the same guarantee. Everything from the
/// authority's own end onward — path, query, fragment — is carried through
/// unchanged, never re-added.
#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
pub(crate) fn hint_authority_url(nest_url: &str, reach_ipv4: &str) -> Option<String> {
    if reach_ipv4.parse::<std::net::Ipv4Addr>().is_err() {
        return None;
    }
    let (scheme, after_scheme) = nest_url.split_once("://")?;
    let authority = fauna_core::web::generic_authority(nest_url);
    let stripped = fauna_core::web::strip_userinfo(authority);
    let (_, port) = fauna_core::web::split_host_port(stripped);
    let host = match port {
        Some(p) => format!("{reach_ipv4}:{p}"),
        None => reach_ipv4.to_string(),
    };
    let rest = &after_scheme[authority.len()..];
    Some(format!("{scheme}://{host}{rest}"))
}

#[cfg(target_arch = "wasm32")]
pub(crate) async fn connect_silent_challenge(
    nest_url: &str,
    reach_ipv4: Option<&str>,
    secret: &[u8],
) -> SilentChallengeOutcome {
    // The hint moves the SOCKET and nothing else — the wasm twin of the native
    // arm's resolve override, and the same split the pre-claim path already
    // makes (`WsNestApi::dial_url` to connect, `expected_root_for(&resolved)` to
    // decide identity). `dial` is what `connect` opens; `nest_url` — the domain
    // — stays the pin key.
    //
    // Keying the pin on the dial URL instead is what the first draft did, and it
    // made "the domain is tried first" buy nothing here: the ordering argument
    // works because the *domain's* pin refuses an impostor, so a hint dial that
    // pinned the IP authority was a first contact at a key that had never been
    // pinned — a box answering at a recycled address chose the account's handle,
    // domain and tier and minted its bearer, with the domain's pin never
    // consulted.
    //
    // One key per box is also why there is nothing to carry over when the hint
    // is dropped: the hint dial and the later domain dial resolve the *same* pin
    // (`security.md` § Transport trust), so the switch is silent by construction
    // rather than by a copy between two authorities.
    let dial = reach_ipv4
        .and_then(|ip| hint_authority_url(nest_url, ip))
        .unwrap_or_else(|| nest_url.to_string());
    match fauna_rpc_wasm::AnonymousWsRpcClient::connect(&dial) {
        // The browser can't read the served cert's SPKI, so the machine's wasm
        // path runs the web TOFU-pin model instead of the native channel
        // binding: possession-verify the verify reply's proof and check it
        // against the localStorage pin — the same store, same key space, and
        // same `check_web_nest_identity` core as the SPA's own
        // `challenge_verify_inner` path, so the two web launch
        // paths can never disagree about what is pinned.
        Ok(c) => {
            fauna_client_core::nest_trust::run_pinned_silent_challenge(
                &c,
                secret,
                nest_url,
                &fauna_client_core::nest_trust::LocalStoragePinStore,
            )
            .await
        }
        Err(e) => SilentChallengeOutcome::Transient {
            error: e.to_string(),
        },
    }
}

// ---------------------------------------------------------------------------
// Tests — the post-auth classifier's escalating arm (no nest, no net). The
// `fauna.auth.handshake` ceremony tests that used to live here went with the
// handshake refresh path: every mint this crate makes is the silent challenge
// now (`login.md` § When to use which), exercised in `fauna_protocol::auth`
// and in this crate's `tests/`.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn superseded_silent_challenge_escalates_like_the_identity_verdict() {
        // The second verdict that must reach the launch surface rather than be
        // logged-and-swallowed: the session is already de-facto dead, since
        // every connection this identity opens from here on is refused.
        let verdict = classify_silent_challenge(SilentChallengeOutcome::Superseded {
            new_actor_id_hex: "ab".repeat(32),
        });
        match verdict {
            SilentSignInVerdict::Superseded { new_actor_id_hex } => {
                assert_eq!(new_actor_id_hex, "ab".repeat(32));
            }
            other => panic!("expected Superseded, got {other:?}"),
        }
        // ...and it is NOT the swallowed bucket.
        assert!(!matches!(
            classify_silent_challenge(SilentChallengeOutcome::Superseded {
                new_actor_id_hex: "ab".repeat(32),
            }),
            SilentSignInVerdict::Failed { .. }
        ));
    }
}

#[cfg(test)]
mod silent_sign_in_classification_tests {
    //! `security.md` § Post-auth surfacing: of everything a background silent
    //! challenge can learn, **exactly one** outcome escalates to the blocking
    //! launch surface. The other four stay logged-and-swallowed.
    //!
    //! Both halves are load-bearing and fail in opposite, equally bad ways: an
    //! identity verdict that stays swallowed leaves a possible-MITM session
    //! running (the pre-2026-07-23 behavior), and a transient blip that escalates
    //! tears down a healthy session and shows the user a MITM warning for a flaky
    //! network. So both are pinned.
    //!
    //! Lifted here from `apps/fauna-linux/src/client.rs` when tui became the
    //! second consumer — the rule is shared, so its pins are too.
    use super::{SilentChallengeOutcome, SilentSignInVerdict, classify_silent_challenge};
    use fauna_protocol::auth::VerifyReply;

    #[test]
    fn the_identity_verdict_is_the_one_outcome_that_escalates() {
        let verdict = classify_silent_challenge(SilentChallengeOutcome::IdentityChanged {
            host: "nest.example".into(),
            pinned_hex: "aa".repeat(32),
            seen_hex: Some("bb".repeat(32)),
            fork: false,
        });
        assert!(
            matches!(verdict, SilentSignInVerdict::IdentityChanged),
            "a mid-session identity change must reach the caller as its OWN outcome — \
             folded into the failure arm it is indistinguishable from a network blip, \
             which is exactly how it used to be swallowed"
        );
    }

    #[test]
    fn transient_and_secret_failures_stay_swallowed() {
        for outcome in [
            SilentChallengeOutcome::Transient {
                error: "connection reset".into(),
            },
            SilentChallengeOutcome::SecretInvalid {
                error: "bad secret".into(),
            },
            SilentChallengeOutcome::NeedsUpdate {
                message: "nest is outdated".into(),
            },
        ] {
            let label = format!("{outcome:?}");
            assert!(
                matches!(
                    classify_silent_challenge(outcome),
                    SilentSignInVerdict::Failed { .. }
                ),
                "{label} must stay on the swallowed failure path — escalating it would \
                 tear down a healthy session and cry MITM over a fault"
            );
        }
    }

    #[test]
    fn success_and_not_registered_keep_their_meanings() {
        let refreshed = classify_silent_challenge(SilentChallengeOutcome::Success(VerifyReply {
            handle: "alice".into(),
            domain: "example.test".into(),
            tier: "free".into(),
            ..Default::default()
        }));
        match refreshed {
            SilentSignInVerdict::Refreshed {
                handle,
                domain,
                tier,
            } => {
                assert_eq!(
                    (handle.as_str(), domain.as_str(), tier.as_str()),
                    ("alice", "example.test", "free")
                );
            }
            other => panic!("success must refresh the cache; got {other:?}"),
        }
        assert!(
            matches!(
                classify_silent_challenge(SilentChallengeOutcome::NotRegistered),
                SilentSignInVerdict::NotRegistered
            ),
            "not-registered keeps its own verdict — the one a signed-in session escalates"
        );
    }

    /// The arm linux's `Result<_, anyhow::Error>` shape could not express: a
    /// swallowed failure and the identity verdict are DIFFERENT variants, not
    /// two spellings of "the call didn't succeed". A caller matching on the enum
    /// cannot accidentally treat one as the other.
    #[test]
    fn a_swallowed_failure_is_never_confusable_with_the_identity_verdict() {
        let fault = classify_silent_challenge(SilentChallengeOutcome::Transient {
            error: "connection reset".into(),
        });
        assert!(
            !matches!(fault, SilentSignInVerdict::IdentityChanged),
            "a transport fault must never reach the escalation arm"
        );
        let verdict = classify_silent_challenge(SilentChallengeOutcome::IdentityChanged {
            host: "nest.example".into(),
            pinned_hex: "aa".repeat(32),
            seen_hex: None,
            fork: false,
        });
        assert!(
            !matches!(verdict, SilentSignInVerdict::Failed { .. }),
            "the identity verdict must never reach the swallowed arm — the \
             withdrawn-identity case (seen_hex: None) included"
        );
    }
}

#[cfg(test)]
mod reach_hint_address_tests {
    //! The reach hint's two address helpers, and the one rule both arms share:
    //! **the hint moves the socket, never the identity key**
    //! (`behavior/onboarding.md` § Reach hint, `architecture/security.md`
    //! § Transport trust).
    //!
    //! Natively that is free — `connect_resolving` takes the URL and the socket
    //! address as separate arguments, so nothing keyed by the URL can move. On
    //! wasm the authority itself has to change, so the two travel in one string
    //! and keeping them apart is a discipline rather than a signature. The first
    //! draft did not keep them apart: it shadowed `nest_url` with the dial URL
    //! and handed that to `run_pinned_silent_challenge` as the pin key, making a
    //! hint dial a first contact at a never-pinned key.
    //! The last test here reads that arm's source, because the wasm arm compiles
    //! only under `target_arch = "wasm32"` and no test in this workspace can
    //! execute it.
    use super::{hint_authority_url, hint_socket_addr};

    const HINT: &str = "203.0.113.7";

    #[test]
    fn the_wasm_dial_url_swaps_the_host_and_keeps_everything_else() {
        // Scheme, port and path are the nest's, not the hint's: a dev nest on
        // `:8443/rpc` must still be dialled there.
        assert_eq!(
            hint_authority_url("https://box.example.com", HINT).as_deref(),
            Some("https://203.0.113.7")
        );
        assert_eq!(
            hint_authority_url("https://box.example.com:8443", HINT).as_deref(),
            Some("https://203.0.113.7:8443")
        );
        assert_eq!(
            hint_authority_url("https://box.example.com:8443/rpc", HINT).as_deref(),
            Some("https://203.0.113.7:8443/rpc")
        );
    }

    #[test]
    fn the_wasm_dial_url_never_reads_past_the_authority_terminator_as_host() {
        // A `?`/`#` in the nest URL is not a metacharacter the old `/`-only
        // split stopped at: `nest.example.com?:x@evil.example/` cut its
        // authority at the trailing `/`, so `rsplit_once(':')` read the `:`
        // right after the `?` as a port separator and swapped the hint IP in
        // ahead of it — `https://203.0.113.7:x@evil.example`, which a URL
        // parser resolves to host `evil.example`, not the nest's
        // (`security.md` § Transport trust).
        assert_eq!(
            hint_authority_url("https://nest.example.com?:x@evil.example/", HINT).as_deref(),
            Some("https://203.0.113.7?:x@evil.example/"),
            "a query-string `@` must never become the dialed host"
        );
        assert_eq!(
            hint_authority_url("https://nest.example.com#:x@evil.example/", HINT).as_deref(),
            Some("https://203.0.113.7#:x@evil.example/"),
            "a fragment `@` must never become the dialed host"
        );
        // A `\` is a WHATWG authority terminator too, same as `/ ? #`
        // (`fauna_core::web::authority_len`): a narrower cut that stopped
        // short of it would read `\:x@evil.example` into the cut authority
        // instead of stopping at the `\`. The dialed *host* would stay the
        // hint IP either way — this function always substitutes it, never
        // the parsed host — so the harm is not an attacker-controlled host;
        // it is a wrong authority *length*, which corrupts `rest` (the
        // path/query/fragment carried through unchanged) and silently drops
        // or mangles the tail this function otherwise preserves.
        assert_eq!(
            hint_authority_url("https://nest.example.com\\:x@evil.example/", HINT).as_deref(),
            Some("https://203.0.113.7\\:x@evil.example/"),
            "a backslash-separated `@` must never become the dialed host"
        );
    }

    #[test]
    fn the_wasm_dial_url_stops_at_every_authority_terminator() {
        // Shared cases so a narrower `authority_len` reds this alongside the
        // other five callers . Every case's
        // authority is `nest.example.com`; the dialed host is always the
        // hint IP (this function never reads the parsed host), so the
        // expected output just swaps the scheme's host for the hint and
        // carries everything from the terminator onward through unchanged.
        for &(url, expected_host) in fauna_core::web::AUTHORITY_TERMINATOR_CASES {
            let rest = url
                .strip_prefix("https://")
                .and_then(|s| s.strip_prefix(expected_host))
                .unwrap_or_else(|| {
                    panic!("case {url} does not start with https://{expected_host}")
                });
            let expected = format!("https://{HINT}{rest}");
            assert_eq!(
                hint_authority_url(url, HINT).as_deref(),
                Some(expected.as_str()),
                "{url}"
            );
        }
    }

    #[test]
    fn the_wasm_dial_url_drops_userinfo_and_handles_a_portless_bracketed_ipv6_authority() {
        // Userinfo names a credential, never the host, and the old split kept
        // it: `rsplit_once(':')` on `user:pw@nest.example.com` read the FIRST
        // colon as the port separator, so the hint IP landed ahead of
        // `pw@nest.example.com` and the resulting URL's host stayed the real
        // nest's — the hint silently ignored rather than dialed.
        assert_eq!(
            hint_authority_url("https://user:pw@nest.example.com/rpc", HINT).as_deref(),
            Some("https://203.0.113.7/rpc"),
            "userinfo must not survive into the dialed authority"
        );
        // A bracketed IPv6 authority with no port has two colons inside the
        // brackets, so the old split's `rsplit_once(':')` found one of those
        // and produced the malformed `https://203.0.113.7:1]`.
        assert_eq!(
            hint_authority_url("https://[2001:db8::1]", HINT).as_deref(),
            Some("https://203.0.113.7")
        );
    }

    #[test]
    fn a_hint_that_is_not_an_ipv4_literal_is_no_fallback_on_either_arm() {
        // A malformed hint must degrade to "no fallback", never to a dial of
        // something else — so it is parsed at the seam, not trusted from the
        // store. Both arms, because both are reached from the same slot.
        for bad in [
            "box.attacker.example",
            "203.0.113.7:443",
            "::1",
            "",
            "0x7f000001",
        ] {
            assert_eq!(
                hint_authority_url("https://box.example.com", bad),
                None,
                "wasm arm accepted a non-IPv4 hint: {bad:?}"
            );
            assert_eq!(
                hint_socket_addr("https://box.example.com", bad),
                None,
                "native arm accepted a non-IPv4 hint: {bad:?}"
            );
        }
    }

    #[test]
    fn the_native_socket_takes_the_urls_own_port() {
        assert_eq!(
            hint_socket_addr("https://box.example.com:8443", HINT)
                .map(|a| a.to_string())
                .as_deref(),
            Some("203.0.113.7:8443")
        );
    }

    #[test]
    fn a_portless_url_defaults_by_scheme_and_wss_is_a_tls_scheme() {
        // `authority_of` strips four schemes, so all four reach the default.
        // `wss://` used to fall to the `else` arm and dial 80.
        for tls in ["https://box.example.com", "wss://box.example.com"] {
            assert_eq!(
                hint_socket_addr(tls, HINT).map(|a| a.port()),
                Some(443),
                "{tls} must default to the TLS port"
            );
        }
        for plain in ["http://box.example.com", "ws://box.example.com"] {
            assert_eq!(
                hint_socket_addr(plain, HINT).map(|a| a.port()),
                Some(80),
                "{plain} must default to the cleartext port"
            );
        }
    }

    /// A hint dial and a domain dial must resolve the **same** pin key, so the
    /// account's existing domain pin is what judges whatever answers the hint —
    /// and so there is nothing to carry over when the hint is dropped.
    ///
    /// This is a *source-level* pin, deliberately, and mirrors
    /// `WsNestApi::the_wasm_core_dials_the_reach_address_and_keys_trust_on_the_identity_url`:
    /// the defect is a **swap between two strings** in an arm no native test can
    /// execute. A behavioural test cannot see it; reading the arm can.
    ///
    /// If this fails after a refactor, the fix is to keep the dial URL out of
    /// the identity argument — not to relax the assertion.
    #[test]
    fn the_wasm_arm_dials_the_hint_and_pins_on_the_domain() {
        let src = include_str!("auth.rs");
        let (_, wasm) = src
            .split_once(
                "#[cfg(target_arch = \"wasm32\")]\npub(crate) async fn connect_silent_challenge(",
            )
            .expect("the wasm connect_silent_challenge is where the hint dial lives");
        let wasm = wasm
            .split_once("\n#[cfg(test)]")
            .map(|(before, _)| before)
            .unwrap_or(wasm);

        assert!(
            wasm.contains("AnonymousWsRpcClient::connect(&dial)"),
            "the wasm arm must CONNECT to the dial URL — otherwise the hint does \
             nothing and the app waits on DNS for a box that is already up"
        );
        // The PROPERTY, not the spelling the defect happened to use. `nest_url`
        // is a parameter of this arm, so the honest invariant is that the arm
        // never rebinds it at all — which catches `= dial`, `= &dial`,
        // `= dial.as_str()`, `: &str = &dial` and every other shape in one
        // line. The first draft of this pin matched the single string
        // `"let nest_url = dial"`, and a one-borrow variant walked straight past
        // it with all five tests green — guard the property the defect violated,
        // never the text it was written in.
        assert!(
            !wasm.contains("let nest_url"),
            "the dial URL must never shadow `nest_url`: everything below the \
             shadow silently becomes the hint's, including the pin key"
        );
        let (_, pinned) = wasm
            .split_once("run_pinned_silent_challenge(")
            .expect("the wasm arm runs the pinned silent challenge");
        let args: Vec<&str> = pinned
            .split_once(')')
            .map(|(a, _)| a)
            .unwrap_or(pinned)
            .split(',')
            .map(str::trim)
            .collect();
        // Deliberately exact: a harmless refactor to `&nest_url` or
        // `nest_url.as_str()` reds this, and that is the safe direction — the
        // reader then re-reads the arm and re-affirms the key, which is cheap.
        // Widening it to "contains nest_url" would accept `hint_authority_url(
        // nest_url, ip)` as the pin key, which is the defect itself.
        assert_eq!(
            args.get(2).copied(),
            Some("nest_url"),
            "the third argument is the PIN KEY and must be the domain \
             `nest_url`, never the dial URL — a hint dial keyed on the dial URL \
             is a first contact at a key that was never pinned, so the account's \
             domain pin never gets to refuse an impostor; got {args:?}"
        );
    }
}
