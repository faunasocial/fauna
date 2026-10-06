//! Email authentication verdicts (SPF, DKIM, DMARC, ARC) wrapping `mail-auth`.
//!
//! The async [`verify_inbound`] function takes raw RFC 5322 bytes plus SMTP
//! envelope metadata and returns the four verdicts ([`verify_inbound_with`] is
//! the same pipeline over a caller-supplied resolver, for hermetic tests). This
//! module owns the *verification*; the verdict **types** live once in
//! [`fauna_core::mail_auth`], which owns their wire-shape contract and the
//! UniFFI derives the Go MTA's binding needs, and are re-exported here so
//! every `fauna_mail::auth::DkimVerdict` path keeps working.
//!
//! They used to be defined here *and* in `fauna_protocol::bridge_routing`,
//! hand-mirrored, agreeing only by CBOR round-trip test — across a wire whose
//! two ends are different languages. One definition makes that drift
//! unrepresentable (pinned by
//! `the_verdict_types_have_exactly_one_definition` in `tests/auth_tests.rs`).

pub use fauna_core::mail_auth::{
    ArcVerdict, AuthVerdicts, DkimVerdict, DmarcPolicy, DmarcVerdict, SpfVerdict,
};

// ── AuthError ─────────────────────────────────────────────────────────────────

#[derive(Debug, thiserror::Error)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Error))]
pub enum AuthError {
    #[error("message could not be parsed for auth verification")]
    Unparseable,
    #[error("DNS resolver initialization failed: {0}")]
    ResolverInit(String),
}

// ── verify_inbound ────────────────────────────────────────────────────────────

/// Verify SPF, DKIM, DMARC, and ARC for an inbound message.
///
/// `raw` is the original RFC 5322 bytes (signature verification requires
/// the canonical wire form, so we never accept a re-serialized
/// `ParsedMessage` here). `mail_from` is the SMTP envelope sender address.
/// `client_ip` is the connecting MTA's IP as a string (e.g. `"192.0.2.1"`).
/// `client_helo` is the HELO/EHLO hostname. The DNS resolver is built from
/// system configuration.
///
/// Note: `client_ip` is accepted as `String` because `std::net::IpAddr` has
/// no UniFFI binding. An unparseable IP falls back to `127.0.0.1`.
//
// ⚠ The `///` doc above is part of this export's UniFFI API checksum: changing
// a byte of it panics every committed Go binding at load (`checksum mismatch`)
// until `libs/fauna-mail-go` is regenerated. The same pipeline over a
// caller-supplied resolver is `verify_inbound_with`, below.
#[cfg_attr(feature = "uniffi", fauna_uniffi_async::export)]
pub async fn verify_inbound(
    raw: &[u8],
    mail_from: &str,
    client_ip: &str,
    client_helo: &str,
) -> Result<AuthVerdicts, AuthError> {
    use mail_auth::{AuthenticatedMessage, MessageAuthenticator};

    let auth_msg = AuthenticatedMessage::parse(raw).ok_or(AuthError::Unparseable)?;
    let resolver = MessageAuthenticator::new_system_conf()
        .map_err(|e| AuthError::ResolverInit(e.to_string()))?;
    Ok(verify_parsed(&resolver, &auth_msg, mail_from, client_ip, client_helo).await)
}

/// [`verify_inbound`] over a caller-supplied authenticator instead of one built
/// from the system resolver configuration.
///
/// The seam exists for tests: an authenticator pointed at an in-process DNS
/// responder makes the verdicts a function of the input alone, where the
/// system resolver makes them a function of whether the box can reach DNS
/// (convention 14, `docs/goal/architecture/e2e-latency-independent-assertions.md`).
/// Production reaches the pipeline only through `verify_inbound`, so this is not
/// exported over UniFFI.
pub async fn verify_inbound_with(
    resolver: &mail_auth::MessageAuthenticator,
    raw: &[u8],
    mail_from: &str,
    client_ip: &str,
    client_helo: &str,
) -> Result<AuthVerdicts, AuthError> {
    let auth_msg = mail_auth::AuthenticatedMessage::parse(raw).ok_or(AuthError::Unparseable)?;
    Ok(verify_parsed(resolver, &auth_msg, mail_from, client_ip, client_helo).await)
}

/// The DKIM → SPF → DMARC → ARC pipeline both entry points share.
async fn verify_parsed(
    resolver: &mail_auth::MessageAuthenticator,
    auth_msg: &mail_auth::AuthenticatedMessage<'_>,
    mail_from: &str,
    client_ip: &str,
    client_helo: &str,
) -> AuthVerdicts {
    use mail_auth::{dmarc::verify::DmarcParameters, spf::verify::SpfParameters};
    use std::net::IpAddr;

    let ip: IpAddr = client_ip.parse().unwrap_or(IpAddr::from([127, 0, 0, 1]));

    // DKIM — does not require DNS when there are no DKIM-Signature headers.
    let dkim_output = resolver.verify_dkim(auth_msg).await;
    let dkim = dkim_verdict_from(&dkim_output);

    // SPF — extract just the domain for the DMARC call later.
    let mail_from_domain = mail_from
        .rsplit_once('@')
        .map(|(_, d)| d.trim_matches(|c: char| c == '<' || c == '>'))
        .unwrap_or(client_helo);

    let spf_output = resolver
        .verify_spf(SpfParameters::verify_mail_from(
            ip,
            client_helo,
            client_helo,
            mail_from,
        ))
        .await;
    let spf = spf_verdict_from(&spf_output);

    // DMARC — third arg is the mail-from domain (not the full address).
    let dmarc_output = resolver
        .verify_dmarc(DmarcParameters::new(
            auth_msg,
            &dkim_output,
            mail_from_domain,
            &spf_output,
        ))
        .await;
    let dmarc = dmarc_verdict_from(&dmarc_output);

    // ARC — result reuses DkimResult variants.
    let arc_output = resolver.verify_arc(auth_msg).await;
    let arc = arc_verdict_from(&arc_output);

    AuthVerdicts {
        dkim,
        spf,
        dmarc,
        arc,
    }
}

// ── verdict mapping helpers ───────────────────────────────────────────────────

fn dkim_verdict_from(out: &[mail_auth::DkimOutput<'_>]) -> DkimVerdict {
    use mail_auth::DkimResult;
    if out.is_empty() {
        return DkimVerdict::None;
    }
    if out.iter().any(|o| matches!(o.result(), DkimResult::Pass)) {
        return DkimVerdict::Pass;
    }
    match out[0].result() {
        DkimResult::Pass => unreachable!("any-pass guard above already returned"),
        DkimResult::Neutral(_) => DkimVerdict::Neutral,
        DkimResult::Fail(e) => DkimVerdict::Fail {
            reason: e.to_string(),
        },
        DkimResult::PermError(_) => DkimVerdict::PermError,
        DkimResult::TempError(_) => DkimVerdict::TempError,
        DkimResult::None => DkimVerdict::None,
    }
}

fn spf_verdict_from(out: &mail_auth::SpfOutput) -> SpfVerdict {
    use mail_auth::SpfResult;
    match out.result() {
        SpfResult::Pass => SpfVerdict::Pass,
        SpfResult::Fail => SpfVerdict::Fail,
        SpfResult::SoftFail => SpfVerdict::SoftFail,
        SpfResult::Neutral => SpfVerdict::Neutral,
        SpfResult::None => SpfVerdict::None,
        SpfResult::TempError => SpfVerdict::TempError,
        SpfResult::PermError => SpfVerdict::PermError,
    }
}

fn dmarc_verdict_from(out: &mail_auth::DmarcOutput) -> DmarcVerdict {
    classify_dmarc(out.dkim_result(), out.spf_result(), out.policy())
}

/// Map a DMARC `mail_auth` result triple — the aligned DKIM result, the aligned
/// SPF result, and the published policy — to our `DmarcVerdict`.
///
/// Split out from `dmarc_verdict_from` so it is unit-testable: a `DmarcOutput`'s
/// `policy` field has no public setter, so the published-policy cases (esp. the
/// unauthenticated-under-`p=reject` spoofing case below) can only be exercised
/// end-to-end through DNS otherwise.
///
/// Key subtlety (and a fixed bug): mail-auth assigns `dkim_result`/`spf_result`
/// a non-`None` value in **only two places** (`mail-auth/src/dmarc/verify.rs`):
/// (a) the alignment block — which runs only when a mechanism PASSED, and sets
/// `spf_result` only if SPF passed / `dkim_result` only if DKIM passed, to
/// `Pass` or `Fail(NotAligned)` (never an error); and (b) the DMARC-record
/// lookup-error path (`verify.rs:73–79`), which sets **both** slots to the
/// **same** `Temp`/`PermError` and leaves `policy` at its default
/// (`Policy::None`). An *unauthenticated* message — no DKIM signature, SPF not
/// `Pass` — runs neither, so it arrives here with BOTH results `None` while
/// `policy` carries the published `p=`. That is the classic spoofing case:
/// under `p=reject`/`p=quarantine` it is a DMARC **failure** (RFC 7489 §6.6.2 —
/// an unauthenticated message is subject to the domain's policy), so the verdict
/// must key on `policy`, not on a `Fail` result. The prior mapping returned
/// `None` whenever neither result was `Fail`, leaving `p=reject` spoofing of a
/// local domain unenforced (the SPF-hardfail gate only caught it when the
/// spoofed domain also published `-all`).
///
/// Reachability matrix (consequence of the two-places rule above): an
/// **asymmetric** `Temp`/`PermError` triple — one slot erroring while the other
/// is `None`/`Pass`/`Fail` — is **not produced by mail-auth in practice**, since
/// the only error path sets both slots identically (with `policy` defaulted). So
/// the two error-slot tests below (`dkim_slot_resolver_error_is_not_overridden…`
/// and `unactionable_spf_error_with_absent_dkim…`) pin *defensive* behavior on
/// inputs that never occur, and the resulting fail-open (error in the DKIM slot →
/// `Temp`/`PermError`, ignoring policy) vs. fail-closed (error in the SPF slot
/// under an enforcing policy → `Fail{policy}`) asymmetry is **moot, not a latent
/// availability bug** — do not "fix" it by churning the match. The only reachable
/// error triple is the symmetric one (`resolver_error_surfaces_as_temp_or_perm`).
fn classify_dmarc(
    dkim_result: &mail_auth::DmarcResult,
    spf_result: &mail_auth::DmarcResult,
    policy: mail_auth::dmarc::Policy,
) -> DmarcVerdict {
    use mail_auth::{DmarcResult, dmarc::Policy};

    let map_policy = |p: Policy| match p {
        Policy::None | Policy::Unspecified => DmarcPolicy::None,
        Policy::Quarantine => DmarcPolicy::Quarantine,
        Policy::Reject => DmarcPolicy::Reject,
    };

    // A DMARC pass requires either DKIM or SPF alignment to pass.
    if *dkim_result == DmarcResult::Pass || *spf_result == DmarcResult::Pass {
        return DmarcVerdict::Pass;
    }
    // NOTE: When DKIM is None and SPF is TempError/PermError, we report
    // DmarcVerdict::None rather than propagating the SPF error. This matches
    // the daemon's existing behavior in handler.rs and reflects DMARC's
    // "either DKIM or SPF must pass" model — a transient error on one side
    // when the other is absent isn't actionable as a DMARC verdict.
    match dkim_result {
        DmarcResult::Fail(_) | DmarcResult::None => {
            // A mechanism was *disowned* (Fail) — apply the published policy
            // verbatim (unchanged behavior; a disowned mechanism under p=none is
            // still Fail{None}, which the SPF/DKIM gates treat as "not decided").
            if matches!(dkim_result, DmarcResult::Fail(_))
                || matches!(spf_result, DmarcResult::Fail(_))
            {
                DmarcVerdict::Fail {
                    policy: map_policy(policy),
                }
            } else {
                // Neither passed nor was disowned ⇒ unauthenticated for the From
                // domain. The published policy governs (see the doc comment): an
                // enforcing policy is a DMARC failure; p=none / no record is None.
                match map_policy(policy) {
                    DmarcPolicy::None => DmarcVerdict::None,
                    policy => DmarcVerdict::Fail { policy },
                }
            }
        }
        DmarcResult::TempError(_) => DmarcVerdict::TempError,
        DmarcResult::PermError(_) => DmarcVerdict::PermError,
        DmarcResult::Pass => DmarcVerdict::Pass, // already handled above
    }
}

fn arc_verdict_from(out: &mail_auth::ArcOutput<'_>) -> ArcVerdict {
    use mail_auth::DkimResult;
    match out.result() {
        DkimResult::Pass => ArcVerdict::Pass,
        DkimResult::Fail(_) => ArcVerdict::Fail,
        DkimResult::TempError(_) => ArcVerdict::TempError,
        DkimResult::PermError(_) => ArcVerdict::PermError,
        DkimResult::None | DkimResult::Neutral(_) => ArcVerdict::None,
    }
}

#[cfg(test)]
mod tests {
    use super::{DmarcPolicy, DmarcVerdict, classify_dmarc};
    use mail_auth::{DmarcResult, Error, dmarc::Policy};

    // The aligned-pass cases short-circuit to Pass regardless of policy.
    #[test]
    fn dmarc_pass_when_a_mechanism_aligns() {
        assert_eq!(
            classify_dmarc(&DmarcResult::Pass, &DmarcResult::None, Policy::Reject),
            DmarcVerdict::Pass
        );
        assert_eq!(
            classify_dmarc(&DmarcResult::None, &DmarcResult::Pass, Policy::Reject),
            DmarcVerdict::Pass
        );
    }

    // The fixed bug: an UNAUTHENTICATED message (both DMARC results None — the
    // shape mail-auth produces when no mechanism passed) under an enforcing
    // published policy is a DMARC failure carrying that policy. This is the
    // local-domain-spoofing case the tier_4 anti-spoofing guard exercises.
    #[test]
    fn unauthenticated_under_enforcing_policy_fails_with_that_policy() {
        assert_eq!(
            classify_dmarc(&DmarcResult::None, &DmarcResult::None, Policy::Reject),
            DmarcVerdict::Fail {
                policy: DmarcPolicy::Reject
            }
        );
        assert_eq!(
            classify_dmarc(&DmarcResult::None, &DmarcResult::None, Policy::Quarantine),
            DmarcVerdict::Fail {
                policy: DmarcPolicy::Quarantine
            }
        );
    }

    // No enforcing policy (p=none / no record) on an unauthenticated message is
    // not a reject — stays None so the SPF/DKIM gates decide.
    #[test]
    fn unauthenticated_under_no_policy_is_none() {
        assert_eq!(
            classify_dmarc(&DmarcResult::None, &DmarcResult::None, Policy::None),
            DmarcVerdict::None
        );
        assert_eq!(
            classify_dmarc(&DmarcResult::None, &DmarcResult::None, Policy::Unspecified),
            DmarcVerdict::None
        );
    }

    // A *disowned* mechanism (Fail) keeps the published policy verbatim — even
    // p=none yields Fail{None} (unchanged behavior; the SPF/DKIM gates treat
    // Fail{None} as "not decided" exactly like None).
    #[test]
    fn disowned_mechanism_carries_published_policy() {
        assert_eq!(
            classify_dmarc(
                &DmarcResult::None,
                &DmarcResult::Fail(Error::NotAligned),
                Policy::Reject
            ),
            DmarcVerdict::Fail {
                policy: DmarcPolicy::Reject
            }
        );
        assert_eq!(
            classify_dmarc(
                &DmarcResult::Fail(Error::NotAligned),
                &DmarcResult::None,
                Policy::None
            ),
            DmarcVerdict::Fail {
                policy: DmarcPolicy::None
            }
        );
    }

    // Resolver error on the DMARC record lookup (mail-auth sets the DKIM result
    // to the error) surfaces as Temp/PermError so the caller can tempfail.
    #[test]
    fn resolver_error_surfaces_as_temp_or_perm() {
        assert_eq!(
            classify_dmarc(
                &DmarcResult::TempError(Error::DnsError("boom".into())),
                &DmarcResult::TempError(Error::DnsError("boom".into())),
                Policy::None
            ),
            DmarcVerdict::TempError
        );
        assert_eq!(
            classify_dmarc(
                &DmarcResult::PermError(Error::DnsError("boom".into())),
                &DmarcResult::PermError(Error::DnsError("boom".into())),
                Policy::None
            ),
            DmarcVerdict::PermError
        );
    }

    // A resolver error that lands in the DKIM slot surfaces as Temp/PermError and
    // the published policy does NOT override it: `classify_dmarc` matches on
    // `dkim_result`, so the Temp/PermError arm wins over the policy branch even
    // under an enforcing p=reject / p=quarantine (fail-open on a DNS blip rather
    // than rejecting when DMARC evaluation itself errored). The existing
    // `resolver_error_surfaces_as_temp_or_perm` only covers errors in BOTH slots
    // under p=none; this pins the dkim-only slot under an enforcing policy.
    //
    // NOTE — DEFENSIVE, NOT REACHABLE: mail-auth never produces this asymmetric
    // (dkim=Error, spf=None) triple. Its only error path (`verify.rs:73–79`) sets
    // BOTH slots to the same error with policy defaulted, so an enforcing policy
    // never co-occurs with a one-slot error. Pins classify_dmarc's behavior on the
    // input for refactor-safety; see the `classify_dmarc` doc § Reachability matrix.
    #[test]
    fn dkim_slot_resolver_error_is_not_overridden_by_enforcing_policy() {
        assert_eq!(
            classify_dmarc(
                &DmarcResult::TempError(Error::DnsError("boom".into())),
                &DmarcResult::None,
                Policy::Reject
            ),
            DmarcVerdict::TempError
        );
        assert_eq!(
            classify_dmarc(
                &DmarcResult::PermError(Error::DnsError("boom".into())),
                &DmarcResult::None,
                Policy::Quarantine
            ),
            DmarcVerdict::PermError
        );
    }

    // The documented NOTE behavior (the source comment inside `classify_dmarc`'s
    // match): a transient/permanent SPF error when DKIM is absent is "not
    // actionable as a DMARC verdict" under p=none. Both results funnel through the
    // `dkim_result == None` arm, neither is `Fail`, and a non-enforcing policy
    // keeps the verdict None — so the SPF/DKIM gates decide, not DMARC. The
    // contract is spelled out in the source but was otherwise unpinned; a refactor
    // that began keying on the SPF slot's error state would silently flip it.
    //
    // NOTE — DEFENSIVE, NOT REACHABLE: like its dkim-slot sibling above, the
    // (dkim=None, spf=Error) triple is not produced by mail-auth (errors populate
    // both slots symmetrically — `verify.rs:73–79`). The converse-asymmetry concern
    // a prior session flagged (the same triple under p=reject → Fail{Reject} rather
    // than tempfailing) is therefore moot: that input cannot occur. See the
    // `classify_dmarc` doc § Reachability matrix.
    #[test]
    fn unactionable_spf_error_with_absent_dkim_under_no_policy_is_none() {
        assert_eq!(
            classify_dmarc(
                &DmarcResult::None,
                &DmarcResult::TempError(Error::DnsError("boom".into())),
                Policy::None
            ),
            DmarcVerdict::None
        );
        assert_eq!(
            classify_dmarc(
                &DmarcResult::None,
                &DmarcResult::PermError(Error::DnsError("boom".into())),
                Policy::None
            ),
            DmarcVerdict::None
        );
    }
}
