//! FFI-wrapper tests for `select_signing_domain` — the owned-types adapter the
//! Go MTA's per-domain DKIM registry set calls to pick a message's `d=` signing
//! domain by From: header domain (RFC 6376 §3.6,
//! `docs/goal/behavior/mail-multidomain.md` § Signing-key selection at outbound
//! time). The selection *rule* itself is unit-tested in
//! `libs/fauna-mail/src/outbound/dkim.rs`; these pin the FFI boundary contract
//! the Go side depends on: owned `Vec<String>` in, `Option<String>` out, `None`
//! == "not local → 550 5.7.7".

use fauna_ffi::*;

fn locals() -> Vec<String> {
    vec!["example.com".into(), "other.test".into()]
}

#[test]
fn exact_match_returns_that_domain() {
    assert_eq!(
        select_signing_domain("example.com".into(), locals()),
        Some("example.com".to_string())
    );
}

#[test]
fn case_and_trailing_dot_insensitive() {
    assert_eq!(
        select_signing_domain("Example.COM.".into(), locals()),
        Some("example.com".to_string())
    );
}

#[test]
fn subdomain_signs_under_closest_parent() {
    assert_eq!(
        select_signing_domain("mail.example.com".into(), locals()),
        Some("example.com".to_string())
    );
}

#[test]
fn non_local_returns_none() {
    // None is the Go MTA's signal to reject with `550 5.7.7 From: domain not local`.
    assert_eq!(select_signing_domain("notlocal.org".into(), locals()), None);
    // A suffix that is not a dot-delimited subdomain is also not local.
    assert_eq!(
        select_signing_domain("notexample.com".into(), locals()),
        None
    );
}

#[test]
fn empty_local_domains_returns_none() {
    // A deployment with no projected local domains can never match → unsigned /
    // reject, never a panic.
    assert_eq!(select_signing_domain("example.com".into(), vec![]), None);
}
