//! RFC 2369 + RFC 8058 list-header construction
//! (`docs/goal/behavior/mail-mass-mailing.md` § RFC 2369 list headers +
//! § RFC 8058 one-click unsubscribe).
//!
//! Pure header-string building (no I/O) — the nest stamps these per recipient
//! in the `send_list_message` fan-out, replacing any List-* the submitting
//! client wrote (§ "the nest's stamp is authoritative"; there is no
//! submission-time MTA stamping — a `kind='list'` address can't be submitted
//! over raw SMTP at all). Returns name/value pairs the outbound path prepends
//! to the raw message (the same build-then-prepend idiom as DKIM/ARC
//! signing), rather than mutating a header map.

/// Inputs for the RFC 2369 + RFC 8058 list-header set on an outbound list
/// message. All user-controlled string fields are sanitized against header
/// injection (CR/LF + control chars stripped) when rendered.
pub struct ListHeaderInputs<'a> {
    /// `mail_lists.list_friendly_name` — the List-Id phrase. When blank, the
    /// `list_id_label` is used as the phrase instead.
    pub friendly_name: &'a str,
    /// Stable list identifier used as the List-Id phrase fallback and the
    /// default List-Help path — the `list_id` UUID as a string. Never PII
    /// (§ Don't "ship list-id with personally-identifying information").
    pub list_id_label: &'a str,
    /// The list's local-part (`account_aliases.pattern`), e.g. `bob-weekly`.
    pub list_pattern: &'a str,
    /// The domain the list was created under (`account_aliases.local_domain`)
    /// — the List-Id host and the `unsubscribe+<token>@` mailto domain
    /// (`<our-domain>` / `<local-domain>` in the spec; they are the same).
    pub list_domain: &'a str,
    /// The deployment primary domain — anchors the one HTTPS unsubscribe
    /// endpoint regardless of how many local domains exist (§ The HTTPS
    /// endpoint) and the default per-list List-Help page.
    pub primary_domain: &'a str,
    /// The per-subscription one-click token (`UnsubscribeTokenGenerator`).
    /// base64url, so it is URL-safe and a safe `unsubscribe+<token>` suffix.
    pub token: &'a str,
    /// `mail_lists.list_help_url` override; when `None`, defaults to a per-list
    /// help page on the primary domain.
    pub list_help_url: Option<&'a str>,
    /// `mail_lists.list_archive_url`; when `None` the List-Archive header is
    /// **omitted** (§ "don't fabricate an archive URL").
    pub list_archive_url: Option<&'a str>,
}

/// Build the ordered RFC 2369 + RFC 8058 header set for a list-mode outbound:
/// `List-Id`, `List-Help`, `List-Archive` (only when set), `List-Unsubscribe`
/// (mailto + https), `List-Unsubscribe-Post`, `Precedence: bulk`.
pub fn list_headers(inputs: &ListHeaderInputs) -> Vec<(&'static str, String)> {
    let phrase = if inputs.friendly_name.trim().is_empty() {
        inputs.list_id_label
    } else {
        inputs.friendly_name
    };

    let mut headers: Vec<(&'static str, String)> = Vec::with_capacity(6);

    // RFC 2919: List-Id = phrase "<" list-id ">". The spec uses the list's
    // posting address (`<pattern@local-domain>`) as the bracketed identifier.
    headers.push((
        "List-Id",
        format!(
            "{} <{}@{}>",
            quoted_phrase(phrase),
            sanitize(inputs.list_pattern),
            sanitize(inputs.list_domain),
        ),
    ));

    // List-Help: per-list override, else a default help page on the primary
    // domain's one HTTPS endpoint.
    let help = match inputs.list_help_url {
        Some(url) => sanitize(url),
        None => format!(
            "https://{}/list/{}/help",
            sanitize(inputs.primary_domain),
            sanitize(inputs.list_id_label),
        ),
    };
    headers.push(("List-Help", format!("<{help}>")));

    // List-Archive: only when set — never fabricate one (§ "don't fabricate an
    // archive URL").
    if let Some(url) = inputs.list_archive_url {
        headers.push(("List-Archive", format!("<{}>", sanitize(url))));
    }

    // RFC 8058: mailto fallback + the One-Click https form, both carrying the
    // per-subscription token. The token is base64url (no `+`), so the
    // `unsubscribe+<token>` local-part is unambiguous.
    let token = sanitize(inputs.token);
    headers.push((
        "List-Unsubscribe",
        format!(
            "<mailto:unsubscribe+{token}@{domain}>, <https://{primary}/list/unsubscribe?t={token}>",
            domain = sanitize(inputs.list_domain),
            primary = sanitize(inputs.primary_domain),
        ),
    ));
    headers.push((
        "List-Unsubscribe-Post",
        "List-Unsubscribe=One-Click".to_string(),
    ));

    // RFC 3834 §3.1.7: bulk auto-mail signal.
    headers.push(("Precedence", "bulk".to_string()));

    headers
}

/// Strip CR/LF and other control characters from a header value — defeats
/// header-injection forgery via a user-controlled field (friendly name, URLs).
fn sanitize(v: &str) -> String {
    fauna_core::control_chars::strip_control_chars(v).into_owned()
}

/// Render an RFC 5322 quoted-string phrase: control chars dropped, `"`/`\`
/// backslash-escaped.
fn quoted_phrase(v: &str) -> String {
    let mut out = String::with_capacity(v.len() + 2);
    out.push('"');
    for c in v.chars() {
        if c.is_control() {
            continue;
        }
        if c == '"' || c == '\\' {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> ListHeaderInputs<'static> {
        ListHeaderInputs {
            friendly_name: "Bob's Weekly",
            list_id_label: "11111111-1111-1111-1111-111111111111",
            list_pattern: "bob-weekly",
            list_domain: "fauna.example",
            primary_domain: "fauna.example",
            token: "abcdEFGH1234_-zyXW9876543210ABCD", // gitleaks:allow
            list_help_url: None,
            list_archive_url: None,
        }
    }

    fn find<'a>(hs: &'a [(&'static str, String)], name: &str) -> Option<&'a str> {
        hs.iter().find(|(n, _)| *n == name).map(|(_, v)| v.as_str())
    }

    #[test]
    fn stamps_full_set_with_archive() {
        let inputs = ListHeaderInputs {
            list_archive_url: Some("https://archive.example.com/bob"),
            list_help_url: Some("https://help.example.com/bob"),
            ..base()
        };
        let hs = list_headers(&inputs);
        assert_eq!(
            find(&hs, "List-Id"),
            Some("\"Bob's Weekly\" <bob-weekly@fauna.example>")
        );
        assert_eq!(
            find(&hs, "List-Help"),
            Some("<https://help.example.com/bob>")
        );
        assert_eq!(
            find(&hs, "List-Archive"),
            Some("<https://archive.example.com/bob>")
        );
        assert_eq!(
            find(&hs, "List-Unsubscribe"),
            Some(
                "<mailto:unsubscribe+abcdEFGH1234_-zyXW9876543210ABCD@fauna.example>, \
                 <https://fauna.example/list/unsubscribe?t=abcdEFGH1234_-zyXW9876543210ABCD>"
            )
        );
        assert_eq!(
            find(&hs, "List-Unsubscribe-Post"),
            Some("List-Unsubscribe=One-Click")
        );
        assert_eq!(find(&hs, "Precedence"), Some("bulk"));
    }

    #[test]
    fn archive_omitted_when_absent() {
        let hs = list_headers(&base());
        assert!(find(&hs, "List-Archive").is_none());
    }

    #[test]
    fn help_defaults_to_per_list_page_on_primary() {
        let hs = list_headers(&base());
        assert_eq!(
            find(&hs, "List-Help"),
            Some("<https://fauna.example/list/11111111-1111-1111-1111-111111111111/help>")
        );
    }

    #[test]
    fn blank_friendly_name_falls_back_to_list_id_label() {
        let inputs = ListHeaderInputs {
            friendly_name: "   ",
            ..base()
        };
        let hs = list_headers(&inputs);
        assert_eq!(
            find(&hs, "List-Id"),
            Some("\"11111111-1111-1111-1111-111111111111\" <bob-weekly@fauna.example>")
        );
    }

    #[test]
    fn list_domain_and_primary_can_differ() {
        let inputs = ListHeaderInputs {
            list_domain: "newsletter.example",
            primary_domain: "fauna.example",
            ..base()
        };
        let hs = list_headers(&inputs);
        // List-Id + mailto use the list's own domain; the https endpoint uses
        // the deployment primary.
        assert_eq!(
            find(&hs, "List-Id"),
            Some("\"Bob's Weekly\" <bob-weekly@newsletter.example>")
        );
        let unsub = find(&hs, "List-Unsubscribe").unwrap();
        assert!(unsub.contains("unsubscribe+abcdEFGH1234_-zyXW9876543210ABCD@newsletter.example"));
        assert!(unsub.contains("https://fauna.example/list/unsubscribe?t="));
    }

    #[test]
    fn sanitizes_header_injection_in_friendly_name() {
        let inputs = ListHeaderInputs {
            friendly_name: "Evil\r\nBcc: victim@example.com",
            ..base()
        };
        let hs = list_headers(&inputs);
        let list_id = find(&hs, "List-Id").unwrap();
        assert!(!list_id.contains('\r'), "CR leaked: {list_id}");
        assert!(!list_id.contains('\n'), "LF leaked: {list_id}");
        assert!(
            list_id.contains("Bcc:"),
            "phrase text kept, only CR/LF stripped"
        );
    }

    #[test]
    fn quoted_phrase_escapes_quotes_and_backslashes() {
        assert_eq!(quoted_phrase(r#"a"b\c"#), r#""a\"b\\c""#);
    }
}
