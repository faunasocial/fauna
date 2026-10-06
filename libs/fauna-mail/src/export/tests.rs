//! Serializer + container tests.
//!
//! The load-bearing ones are the determinism goldens: `docs/goal/behavior/
//! mail-export.md` § Goal makes byte-identical output a *product* property (the
//! user self-verifies their own export), and § Container shape lists exactly
//! which inputs are designed out. A pinned digest is the only assertion that
//! actually catches a wall-clock, hostname or iteration-order leak, because
//! every such leak still produces a perfectly valid archive.

use super::*;

const CRLF_MESSAGE: &[u8] = b"Return-Path: <sender@example.com>\r\n\
From: Sender Name <sender@example.com>\r\n\
To: user@fauna.example\r\n\
Message-ID: <abc123@example.com>\r\n\
Subject: Hello\r\n\
Received: from relay.example.com by nest.example\r\n\
\r\n\
Body line one.\r\n\
From the top, this line needs escaping.\r\n\
>From already quoted.\r\n";

fn message(mailbox: &str, uid: u32, epoch: i64, flags: &[&str]) -> ExportMessage {
    ExportMessage {
        mailbox: mailbox.to_string(),
        flags: flags.iter().map(|f| (*f).to_string()).collect(),
        body: CRLF_MESSAGE.to_vec(),
        internal_date_epoch: epoch,
        uid,
        uid_validity: 7,
    }
}

/// A message whose bytes differ, so digest-derived names differ too.
fn distinct(mailbox: &str, uid: u32, epoch: i64, subject: &str) -> ExportMessage {
    let body = format!(
        "From: sender@example.com\r\nMessage-ID: <{subject}@example.com>\r\n\
         Subject: {subject}\r\n\r\nBody of {subject}.\r\n"
    );
    ExportMessage {
        mailbox: mailbox.to_string(),
        flags: vec!["\\Seen".to_string()],
        body: body.into_bytes(),
        internal_date_epoch: epoch,
        uid,
        uid_validity: 7,
    }
}

fn entry<'a>(entries: &'a [ExportEntry], suffix: &str) -> &'a ExportEntry {
    entries
        .iter()
        .find(|e| e.path.ends_with(suffix))
        .unwrap_or_else(|| {
            panic!(
                "no entry ending {suffix:?}; have {:?}",
                entries.iter().map(|e| &e.path).collect::<Vec<_>>()
            )
        })
}

fn text(entry: &ExportEntry) -> String {
    String::from_utf8(entry.bytes.clone()).expect("utf-8 entry")
}

// ── mbox ────────────────────────────────────────────────────────────────────

#[test]
fn mbox_writes_one_file_per_mailbox_with_the_rfc4155_separator() {
    let entries = serialize_all(
        ExportFormat::Mbox,
        ExportOptions::new("alice"),
        &[
            message("INBOX", 1, 837_596_665, &["\\Seen"]),
            message("Sent", 2, 837_596_700, &[]),
        ],
    )
    .expect("serialize");

    let inbox = entry(&entries, "alice-mbox/INBOX.mbox");
    assert!(
        text(inbox).starts_with("From sender@example.com Wed Jul 17 09:44:25 1996\n"),
        "separator line: {:?}",
        &text(inbox)[..64]
    );
    assert!(entries.iter().any(|e| e.path == "alice-mbox/Sent.mbox"));
    // The mailbox file is dated by the mail it holds, never by the clock.
    assert_eq!(inbox.mtime_epoch, 837_596_665);
}

#[test]
fn mbox_escapes_from_lines_the_reversible_mboxrd_way() {
    let entries = serialize_all(
        ExportFormat::Mbox,
        ExportOptions::new("alice"),
        &[message("INBOX", 1, 837_596_665, &[])],
    )
    .expect("serialize");
    let body = text(entry(&entries, "INBOX.mbox"));

    assert!(
        body.contains("\n>From the top, this line needs escaping.\n"),
        "unquoted `From ` gains one `>`"
    );
    assert!(
        body.contains("\n>>From already quoted.\n"),
        "an already-quoted `>From ` gains another `>` — mboxrd, not mboxo"
    );
}

#[test]
fn mbox_carries_imap_flags_in_the_three_conventional_headers() {
    let entries = serialize_all(
        ExportFormat::Mbox,
        ExportOptions::new("alice"),
        &[message(
            "INBOX",
            1,
            837_596_665,
            &["\\Seen", "\\Answered", "\\Flagged"],
        )],
    )
    .expect("serialize");
    let body = text(entry(&entries, "INBOX.mbox"));

    assert!(body.contains("\nStatus: RO\n"), "{body}");
    assert!(body.contains("\nX-Status: AF\n"), "{body}");
    // 0x0001 read | 0x0002 replied | 0x0004 marked.
    assert!(body.contains("\nX-Mozilla-Status: 0007\n"), "{body}");
}

#[test]
fn mbox_drops_any_flag_headers_the_message_already_carried() {
    let mut msg = message("INBOX", 1, 837_596_665, &["\\Seen"]);
    msg.body = b"From: sender@example.com\r\nStatus: XX\r\nX-Mozilla-Status: ffff\r\n\r\nBody.\r\n"
        .to_vec();
    let entries =
        serialize_all(ExportFormat::Mbox, ExportOptions::new("alice"), &[msg]).expect("serialize");
    let body = text(entry(&entries, "INBOX.mbox"));

    assert!(
        !body.contains("Status: XX"),
        "stale Status survived: {body}"
    );
    assert!(
        !body.contains("ffff"),
        "stale X-Mozilla-Status survived: {body}"
    );
    assert_eq!(body.matches("Status: RO").count(), 1, "{body}");
}

#[test]
fn mbox_normalizes_crlf_to_lf_because_mbox_is_a_unix_text_format() {
    let entries = serialize_all(
        ExportFormat::Mbox,
        ExportOptions::new("alice"),
        &[message("INBOX", 1, 837_596_665, &[])],
    )
    .expect("serialize");
    assert!(
        !entry(&entries, "INBOX.mbox").bytes.contains(&b'\r'),
        "no CR survives into an mbox file"
    );
}

#[test]
fn mbox_falls_back_to_mailer_daemon_without_a_usable_sender() {
    let mut msg = message("INBOX", 1, 0, &[]);
    msg.body = b"Subject: no sender at all\r\n\r\nBody.\r\n".to_vec();
    let entries =
        serialize_all(ExportFormat::Mbox, ExportOptions::new("alice"), &[msg]).expect("serialize");
    assert!(
        text(entry(&entries, "INBOX.mbox")).starts_with("From MAILER-DAEMON "),
        "{}",
        text(entry(&entries, "INBOX.mbox"))
    );
}

// ── Maildir++ ───────────────────────────────────────────────────────────────

#[test]
fn maildir_filename_carries_the_three_pinned_components() {
    let entries = serialize_all(
        ExportFormat::MaildirPlus,
        ExportOptions::new("alice"),
        &[message(
            "INBOX",
            1,
            837_596_665,
            &["\\Seen", "\\Flagged", "\\Draft"],
        )],
    )
    .expect("serialize");

    let file = entries
        .iter()
        .find(|e| !e.is_dir && e.path.contains("/cur/"))
        .expect("one cur/ file");
    let name = file.path.rsplit('/').next().unwrap();
    let (stem, flags) = name.split_once(":2,").expect("colon-2 marker");
    let mut parts = stem.splitn(2, '.');

    assert_eq!(
        parts.next(),
        Some("837596665"),
        "<unix-time> is INTERNALDATE"
    );
    let rest = parts.next().expect("<unique>.<host>");
    let (unique, host) = rest.split_once('.').expect("<unique> then <host>");
    assert_eq!(host, "fauna.invalid", "<host> is the pinned literal");
    assert_eq!(unique.len(), 16, "<unique> is 16 hex chars: {unique}");
    assert!(unique.chars().all(|c| c.is_ascii_hexdigit()));
    // D(raft) F(lagged) S(een), ASCII-ascending, no separator.
    assert_eq!(flags, "DFS");
}

#[test]
fn maildir_puts_every_message_in_cur_and_creates_new_and_tmp_empty() {
    let entries = serialize_all(
        ExportFormat::MaildirPlus,
        ExportOptions::new("alice"),
        &[message("INBOX", 1, 837_596_665, &[])],
    )
    .expect("serialize");

    for sub in ["cur", "new", "tmp"] {
        let path = format!("alice-maildir/INBOX/{sub}/");
        assert!(
            entries.iter().any(|e| e.path == path && e.is_dir),
            "missing {path}"
        );
    }
    assert_eq!(
        entries
            .iter()
            .filter(|e| !e.is_dir && e.path.contains("/new/"))
            .count(),
        0,
        "an unseen message still goes to cur/, so its flags survive"
    );
}

#[test]
fn maildir_message_bytes_are_byte_for_byte_the_original() {
    let entries = serialize_all(
        ExportFormat::MaildirPlus,
        ExportOptions::new("alice"),
        &[message("INBOX", 1, 837_596_665, &[])],
    )
    .expect("serialize");
    let file = entries
        .iter()
        .find(|e| !e.is_dir && e.path.contains("/cur/"))
        .unwrap();
    assert_eq!(file.bytes, CRLF_MESSAGE, "Maildir++ preserves CRLF exactly");
}

#[test]
fn maildir_breaks_a_duplicate_message_collision_deterministically() {
    // Two byte-identical messages in one mailbox share a digest by
    // construction — the `-<n>` suffix is what keeps both in the archive.
    let entries = serialize_all(
        ExportFormat::MaildirPlus,
        ExportOptions::new("alice"),
        &[
            message("INBOX", 1, 837_596_665, &[]),
            message("INBOX", 2, 837_596_665, &[]),
        ],
    )
    .expect("serialize");

    let files: Vec<_> = entries
        .iter()
        .filter(|e| !e.is_dir && e.path.contains("/cur/"))
        .collect();
    assert_eq!(files.len(), 2, "both duplicates land");
    assert_ne!(files[0].path, files[1].path);
    assert!(files[1].path.contains("-2."), "{}", files[1].path);
}

#[test]
fn maildir_writes_the_subscriptions_index() {
    let entries = serialize_all(
        ExportFormat::MaildirPlus,
        ExportOptions::new("alice"),
        &[
            distinct("Archive", 1, 100, "a"),
            distinct("INBOX", 2, 200, "b"),
        ],
    )
    .expect("serialize");
    assert_eq!(
        text(entry(&entries, "alice-maildir/subscriptions")),
        "Archive\nINBOX\n"
    );
}

// ── EML zip ─────────────────────────────────────────────────────────────────

#[test]
fn eml_writes_pristine_messages_plus_the_manifest() {
    let entries = serialize_all(
        ExportFormat::EmlZip,
        ExportOptions::new("alice"),
        &[message("INBOX", 42, 837_596_665, &["\\Seen"])],
    )
    .expect("serialize");

    let eml = entry(&entries, "abc123@example.com.eml");
    assert_eq!(eml.bytes, CRLF_MESSAGE, ".eml files are pristine");

    let manifest = text(entry(&entries, "alice-eml/manifest.json"));
    for expected in [
        "\"source_mailbox\": \"INBOX\"",
        "\"imap_uid\": 42",
        "\"imap_uidvalidity\": 7",
        "\"\\\\Seen\"",
        "\"internal_date\": \"1996-07-17T09:44:25Z\"",
        "\"message_id\": \"<abc123@example.com>\"",
        "\"filename\": \"abc123@example.com.eml\"",
    ] {
        assert!(
            manifest.contains(expected),
            "{expected} missing:\n{manifest}"
        );
    }
}

#[test]
fn eml_falls_back_to_the_digest_when_the_message_id_is_unusable() {
    let mut msg = message("INBOX", 1, 0, &[]);
    msg.body = b"From: sender@example.com\r\nMessage-ID: <>\r\n\r\nBody.\r\n".to_vec();
    let entries = serialize_all(ExportFormat::EmlZip, ExportOptions::new("alice"), &[msg])
        .expect("serialize");

    let eml = entries
        .iter()
        .find(|e| e.path.ends_with(".eml"))
        .expect("one eml");
    let stem = eml
        .path
        .rsplit('/')
        .next()
        .unwrap()
        .trim_end_matches(".eml");
    assert_eq!(stem.len(), 16, "digest fallback: {stem}");
    assert!(stem.chars().all(|c| c.is_ascii_hexdigit()), "{stem}");
}

#[test]
fn eml_disambiguates_two_messages_sharing_one_message_id() {
    let entries = serialize_all(
        ExportFormat::EmlZip,
        ExportOptions::new("alice"),
        &[message("INBOX", 1, 100, &[]), message("INBOX", 2, 200, &[])],
    )
    .expect("serialize");
    let names: Vec<_> = entries
        .iter()
        .filter(|e| e.path.ends_with(".eml"))
        .map(|e| e.path.clone())
        .collect();
    assert_eq!(names.len(), 2);
    assert_ne!(names[0], names[1]);
    assert!(names[1].ends_with("-2.eml"), "{}", names[1]);
}

// ── Scope, ordering and path safety ─────────────────────────────────────────

/// A message carrying the ordinary transit complement: every header of the
/// ratified strip set (mixed case, one folded), both fixture IPs, and the
/// headers the owner doc names as deliberately kept — including one of each
/// out-of-scope family (a verdict stamp, an envelope record, a Fauna delivery
/// stamp) so the exclusions are pinned as firmly as the inclusions.
const TRANSIT_MESSAGE: &[u8] = b"Return-Path: <sender@example.com>\r\n\
Delivered-To: user@fauna.example\r\n\
X-Spam-Status: No, score=0.1 required=5.0\r\n\
X-Fauna-Spam-Threshold: 5\r\n\
Received: from relay.example.com (relay.example.com [203.0.113.7])\r\n\
\tby nest.example with ESMTPS id abc\r\n\
X-Received: by 2002:a05:6402:1a2b with SMTP id x1\r\n\
received-spf: pass (nest.example: 203.0.113.7 is authorized) client-ip=203.0.113.7\r\n\
Authentication-Results: nest.example; spf=pass smtp.mailfrom=example.com\r\n\
ARC-Seal: i=1; a=rsa-sha256; d=relay.example.com; cv=none; b=AAAA\r\n\
ARC-Message-Signature: i=1; a=rsa-sha256; d=relay.example.com; b=BBBB\r\n\
ARC-Authentication-Results: i=1; relay.example.com; spf=pass\r\n\
X-Originating-IP: [198.51.100.9]\r\n\
X-ORIGINATING-CLIENT-IP: 198.51.100.9\r\n\
X-Sender-IP: 198.51.100.9\r\n\
X-Forwarded-For: 198.51.100.9\r\n\
X-Real-IP: 198.51.100.9\r\n\
x-client-ip: 198.51.100.9\r\n\
X-Forefront-Antispam-Report: CIP:203.0.113.7;CTRY:NO;SCL:1;H:relay.example.com;PTR:relay.example.com;\r\n\
X-Forefront-Antispam-Report-Untrusted: CIP:203.0.113.7;SFV:NSPM;\r\n\
X-ClientProxiedBy: proxy.example.com (203.0.113.7) To\r\n\
\tmbx.example.com (198.51.100.9)\r\n\
X-MS-Exchange-Organization-OriginalClientIPAddress: 198.51.100.9\r\n\
X-MS-Exchange-Organization-OriginalServerIPAddress: 203.0.113.7\r\n\
X-MS-Exchange-Organization-ConnectingIP: 198.51.100.9\r\n\
X-MS-Exchange-Organization-AuthSource: auth.example.com\r\n\
X-MS-Exchange-CrossTenant-AuthSource: auth.example.com\r\n\
X-MS-Exchange-Transport-CrossTenantHeadersStamped: mbx.example.com\r\n\
X-SES-Outgoing: 2024.01.01-203.0.113.7\r\n\
X-Mailgun-Sending-Ip: 203.0.113.7\r\n\
X-Ovh-Remote: 198.51.100.9 (client.example.net)\r\n\
X-Barracuda-Connect: relay.example.com[203.0.113.7]\r\n\
X-Barracuda-Apparent-Source-IP: 203.0.113.7\r\n\
X-Scanned-By: MIMEDefang 2.84 on 203.0.113.7\r\n\
X-Greylist: delayed 300 seconds by postgrey-1.37 at nest.example; Mon, 01 Jan 2024 00:00:00 +0000\r\n\
X-Virus-Scanned: amavisd-new at nest.example\r\n\
X-Spam-Checker-Version: SpamAssassin 4.0.0 (2022-12-13) on nest.example\r\n\
X-AntiAbuse: Primary Hostname - relay.example.com\r\n\
X-Authenticated-Sender: relay.example.com: sender@example.com\r\n\
X-Get-Message-Sender-Via: relay.example.com: authenticated_id: sender@example.com\r\n\
DKIM-Signature: v=1; a=ed25519-sha256; d=example.com; s=k; h=from:subject; b=CCCC\r\n\
From: Sender Name <sender@example.com>\r\n\
To: user@fauna.example\r\n\
Message-ID: <transit1@example.com>\r\n\
Subject: Hello\r\n\
\r\n\
Received: this line is body text, not a header.\r\n";

/// The header names `TRANSIT_MESSAGE` carries that the strip must keep, each
/// for the reason § UX shape step 2 gives.
const KEPT_BY_THE_STRIP: &[&str] = &[
    "Return-Path",
    "Delivered-To",
    "X-Spam-Status",
    "X-Fauna-Spam-Threshold",
    "DKIM-Signature",
    "From",
    "To",
    "Message-ID",
    "Subject",
];

/// Whether the header section of `bytes` (everything before the first blank
/// line) holds a line starting `name:` — an exact field-name match, so
/// `Received` never answers for `X-Received`, and never a body line.
fn has_header_line(bytes: &[u8], name: &str) -> bool {
    // Leading `\n` so the file's very first header matches like any other.
    let text = format!("\n{}", String::from_utf8_lossy(bytes).to_ascii_lowercase());
    let headers = text
        .split_once("\r\n\r\n")
        .or_else(|| text.split_once("\n\n"))
        .map_or(text.as_str(), |(head, _)| head);
    headers.contains(&format!("\n{}:", name.to_ascii_lowercase()))
}

#[test]
fn strip_headers_removes_exactly_the_ratified_transit_set_in_every_format() {
    for format in [
        ExportFormat::Mbox,
        ExportFormat::MaildirPlus,
        ExportFormat::EmlZip,
    ] {
        let options = ExportOptions {
            actor_handle: "alice".into(),
            strip_headers: true,
        };
        let mut msg = message("INBOX", 1, 100, &[]);
        msg.body = TRANSIT_MESSAGE.to_vec();
        let entries = serialize_all(format, options, &[msg]).expect("serialize");
        let suffix = match format {
            ExportFormat::Mbox => ".mbox",
            ExportFormat::MaildirPlus => ":2,",
            ExportFormat::EmlZip => ".eml",
        };
        let bytes = &entry(&entries, suffix).bytes;

        for name in TRANSIT_STRIP_HEADERS {
            assert!(
                has_header_line(TRANSIT_MESSAGE, name),
                "fixture must carry {name} or this arm proves nothing"
            );
            assert!(!has_header_line(bytes, name), "{format:?} kept {name}");
        }
        for name in KEPT_BY_THE_STRIP {
            assert!(has_header_line(bytes, name), "{format:?} dropped {name}");
        }
        let text = String::from_utf8_lossy(bytes);
        for ip in ["203.0.113.7", "198.51.100.9"] {
            assert!(!text.contains(ip), "{format:?} still ships the IP {ip}");
        }
        assert!(
            text.contains("Received: this line is body text"),
            "{format:?}: the strip reaches past the header section"
        );
    }
}

/// `TRANSIT_MESSAGE` with every line ending in bare LF — the bytes a
/// non-conformant or hostile migration source can hand the store unmodified.
fn transit_message_lf() -> Vec<u8> {
    String::from_utf8_lossy(TRANSIT_MESSAGE)
        .replace("\r\n", "\n")
        .into_bytes()
}

/// A bare-LF header section frames the way the parser reads it: the strip
/// removes exactly the ratified set, keeps the rest, never reaches the body —
/// and never leaves an empty entry behind, whether the first header is one it
/// strips (`Return-Path` then `Received`, reordered so `Received` leads) or
/// one it keeps.
#[test]
fn strip_headers_frames_a_bare_lf_message_like_a_crlf_one_in_every_format() {
    let lf = transit_message_lf();
    let received_first = {
        let text = String::from_utf8_lossy(&lf).into_owned();
        let (return_path, rest) = text.split_once('\n').expect("first line");
        let (head, body) = rest.split_once("\n\n").expect("separator");
        format!("{head}\n{return_path}\n\n{body}").into_bytes()
    };
    let subject_first = {
        let text = String::from_utf8_lossy(&lf).into_owned();
        format!(
            "Subject: Hello\n{}",
            text.replacen("Subject: Hello\n", "", 1)
        )
        .into_bytes()
    };
    for (label, raw) in [
        ("bare LF", lf),
        ("bare LF, Received first", received_first),
        ("bare LF, Subject first", subject_first),
    ] {
        for format in [
            ExportFormat::Mbox,
            ExportFormat::MaildirPlus,
            ExportFormat::EmlZip,
        ] {
            let options = ExportOptions {
                actor_handle: "alice".into(),
                strip_headers: true,
            };
            let mut msg = message("INBOX", 1, 100, &[]);
            msg.body = raw.clone();
            let entries = serialize_all(format, options, &[msg]).expect("serialize");
            let suffix = match format {
                ExportFormat::Mbox => ".mbox",
                ExportFormat::MaildirPlus => ":2,",
                ExportFormat::EmlZip => ".eml",
            };
            let bytes = &entry(&entries, suffix).bytes;
            for name in TRANSIT_STRIP_HEADERS {
                assert!(has_header_line(&raw, name), "{label}: fixture lacks {name}");
                assert!(
                    !has_header_line(bytes, name),
                    "{label} {format:?} kept {name}"
                );
            }
            for name in KEPT_BY_THE_STRIP {
                assert!(
                    has_header_line(bytes, name),
                    "{label} {format:?} dropped {name}"
                );
            }
            let text = String::from_utf8_lossy(bytes);
            for ip in ["203.0.113.7", "198.51.100.9"] {
                assert!(!text.contains(ip), "{label} {format:?} still ships {ip}");
            }
            assert!(
                text.contains("Received: this line is body text"),
                "{label} {format:?}: the body is gone or was stripped: {text:?}"
            );
        }
    }
}

/// The mbox writer's own flag-header strip walks the same way: a bare-LF
/// message whose first header is `Status:` loses only its flag headers, not
/// every header after them.
#[test]
fn mbox_drops_bare_lf_flag_headers_without_swallowing_the_rest() {
    let mut msg = message("INBOX", 1, 100, &["\\Seen"]);
    msg.body =
        b"Status: O\nX-Status: F\nFrom: sender@example.com\nSubject: Kept\n\nBody kept.\n".to_vec();
    let entries =
        serialize_all(ExportFormat::Mbox, ExportOptions::new("alice"), &[msg]).expect("serialize");
    let text = text(entry(&entries, ".mbox"));
    assert!(text.contains("\nFrom: sender@example.com\n"), "{text:?}");
    assert!(text.contains("\nSubject: Kept\n"), "{text:?}");
    assert!(text.contains("\n\nBody kept.\n"), "{text:?}");
    assert!(!text.contains("Status: O"), "{text:?}");
    assert!(!text.contains("X-Status: F"), "{text:?}");
}

/// A message that is nothing but transit headers strips to nothing. The export
/// refuses rather than writing an empty entry into an archive it would then
/// report complete — fail-closed, the same rule as a stored empty body.
#[test]
fn a_message_the_strip_empties_is_refused_not_written_empty() {
    for raw in [
        &b"Received: from a\r\n"[..],
        &b"Received: from a\n\tby b\n"[..],
    ] {
        for format in [
            ExportFormat::Mbox,
            ExportFormat::MaildirPlus,
            ExportFormat::EmlZip,
        ] {
            let options = ExportOptions {
                actor_handle: "alice".into(),
                strip_headers: true,
            };
            let mut msg = message("INBOX", 1, 100, &[]);
            msg.body = raw.to_vec();
            let err = serialize_all(format, options, &[msg]).expect_err("stripped empty");
            assert!(
                matches!(err, ExportError::EmptyAfterStrip { .. }),
                "{format:?}: {err:?}"
            );
        }
    }
}

/// The owner doc's list and the Rust constant are one list. The doc is the
/// authority (§ UX shape step 2's **Strip set:** line); a header added to
/// either side alone fails here.
#[test]
fn the_strip_set_constant_is_the_owner_docs_list() {
    let doc = include_str!("../../../../docs/goal/behavior/mail-export.md");
    let line = doc
        .lines()
        .find(|l| l.contains("**Strip set:**"))
        .expect("mail-export.md § UX shape step 2 carries a `**Strip set:**` line");
    // The list runs from the marker to the next bold marker on that line.
    let (_, after) = line.split_once("**Strip set:**").expect("marker");
    let list = after.split("**").next().expect("list text");
    let listed: Vec<&str> = list.split('`').skip(1).step_by(2).collect();
    assert_eq!(listed, TRANSIT_STRIP_HEADERS, "doc list vs Rust constant");
}

#[test]
fn strip_headers_off_keeps_full_forensic_fidelity() {
    let entries = serialize_all(
        ExportFormat::EmlZip,
        ExportOptions::new("alice"),
        &[message("INBOX", 1, 100, &[])],
    )
    .expect("serialize");
    assert!(
        String::from_utf8_lossy(&entry(&entries, ".eml").bytes).contains("Received: from relay"),
        "default is preserve"
    );
}

#[test]
fn an_out_of_order_message_is_refused_rather_than_silently_reordered() {
    let mut serializer = ExportSerializer::new(ExportFormat::EmlZip, ExportOptions::new("alice"));
    serializer
        .push(&distinct("INBOX", 2, 200, "b"))
        .expect("first");
    let err = serializer
        .push(&distinct("INBOX", 1, 100, "a"))
        .expect_err("earlier date after a later one breaks the total order");
    assert!(matches!(err, ExportError::OutOfOrder { .. }), "{err:?}");
    // Mailbox order too — `Sent` then `INBOX` is descending by raw bytes.
    let mut serializer = ExportSerializer::new(ExportFormat::EmlZip, ExportOptions::new("alice"));
    serializer
        .push(&distinct("Sent", 1, 100, "a"))
        .expect("first");
    assert!(matches!(
        serializer.push(&distinct("INBOX", 2, 200, "b")),
        Err(ExportError::OutOfOrder { .. })
    ));
}

/// The case the total order changed for (2026-09-21): a message IMPORTED from
/// another provider carries its source's old INTERNALDATE on a freshly-minted,
/// higher UID. The down-leg's only cursor is `after_uid`, so this is exactly
/// what a UID walk of an imported mailbox hands the serializer — and under the
/// old `(mailbox, date, uid)` key it killed the run.
#[test]
fn an_imported_message_with_an_older_date_on_a_higher_uid_is_accepted() {
    let mut serializer = ExportSerializer::new(ExportFormat::EmlZip, ExportOptions::new("alice"));
    serializer
        .push(&distinct("INBOX", 1, 1_700_000_000, "arrived-here"))
        .expect("first");
    serializer
        .push(&distinct(
            "INBOX",
            2,
            900_000_000,
            "imported-from-elsewhere",
        ))
        .expect("an imported message's older date on a later UID is in order");
    // A descending UID inside one mailbox is still refused.
    assert!(matches!(
        serializer.push(&distinct("INBOX", 1, 1_800_000_000, "replayed")),
        Err(ExportError::OutOfOrder { .. })
    ));
}

/// mbox's mailbox file is dated by the mailbox's EARLIEST INTERNALDATE, which
/// under UID ordering is no longer the first message's. Taking the first would
/// make the entry's mtime depend on arrival order rather than on the mail.
#[test]
fn an_mbox_file_is_dated_by_the_mailboxs_earliest_message_not_its_first() {
    let messages = vec![
        distinct("INBOX", 1, 1_700_000_000, "arrived-here"),
        distinct("INBOX", 2, 900_000_000, "imported-from-elsewhere"),
    ];
    let entries = serialize_all(ExportFormat::Mbox, ExportOptions::new("alice"), &messages)
        .expect("serialize");
    let mbox = entries
        .iter()
        .find(|e| e.path.ends_with("INBOX.mbox"))
        .expect("one mbox file for INBOX");
    assert_eq!(
        mbox.mtime_epoch, 900_000_000,
        "the mbox file takes the mailbox's earliest INTERNALDATE"
    );
}

/// The three wire tokens, pinned against the nest's own closed list
/// (`bridge_export_handlers.rs::EXPORT_FORMATS`). The nest cannot import this
/// enum without linking the serializers into its binary — the shape
/// § Export pipeline rejects — so the two lists are kept honest by this test
/// rather than by a shared const.
#[test]
fn the_three_wire_format_tokens_are_the_nests_closed_list() {
    assert_eq!(ExportFormat::Mbox.wire_name(), "mbox");
    assert_eq!(ExportFormat::MaildirPlus.wire_name(), "maildir");
    assert_eq!(ExportFormat::EmlZip.wire_name(), "eml-zip");
    for f in [
        ExportFormat::Mbox,
        ExportFormat::MaildirPlus,
        ExportFormat::EmlZip,
    ] {
        assert_eq!(ExportFormat::from_wire_name(f.wire_name()), Some(f));
    }
    assert_eq!(ExportFormat::from_wire_name("pst"), None);
}

#[test]
fn an_empty_body_is_a_caller_bug_not_a_silent_empty_entry() {
    let mut msg = message("INBOX", 1, 100, &[]);
    msg.body.clear();
    let err = serialize_all(ExportFormat::EmlZip, ExportOptions::new("alice"), &[msg])
        .expect_err("empty body");
    assert!(matches!(err, ExportError::EmptyBody { .. }), "{err:?}");
}

#[test]
fn a_hostile_mailbox_name_cannot_escape_the_archive_root() {
    for (format, root) in [
        (ExportFormat::Mbox, "alice-mbox/"),
        (ExportFormat::MaildirPlus, "alice-maildir/"),
    ] {
        for name in HOSTILE_NAMES {
            let entries = serialize_all(
                format,
                ExportOptions::new("alice"),
                &[message(name, 1, 100, &[])],
            )
            .expect("serialize");
            for e in &entries {
                assert!(e.path.starts_with(root), "escaped the root: {}", e.path);
                // The mailbox is exactly one component below the root — also
                // after a best-fit extractor narrows the path, and with `\` a
                // separator as it is on Windows.
                let narrowed = as_a_best_fit_extractor_narrows_it(&e.path);
                let below = &narrowed[root.len()..];
                let depth = below.trim_end_matches('/').split(['/', '\\']).count();
                let expected = match format {
                    ExportFormat::Mbox => 1,
                    _ if below == "subscriptions" => 1,
                    ExportFormat::MaildirPlus if e.is_dir => 2,
                    _ => 3,
                };
                assert_eq!(depth, expected, "{name:?} split into {narrowed}");
                for component in narrowed.split(['/', '\\']) {
                    assert!(
                        component != "." && component != "..",
                        "traversal component in {narrowed} (archived as {})",
                        e.path
                    );
                }
            }
        }
    }
    // EML zip never puts a mailbox in a path: one flat directory whose stems
    // are an ASCII whitelist, so a look-alike cannot reach a filename.
    let mut in_order = HOSTILE_NAMES.to_vec();
    in_order.sort_unstable();
    let hostile: Vec<_> = in_order
        .iter()
        .enumerate()
        .map(|(uid, name)| message(name, uid as u32 + 1, 100, &[]))
        .collect();
    let entries =
        serialize_all(ExportFormat::EmlZip, ExportOptions::new("alice"), &hostile).expect("eml");
    for e in &entries {
        let narrowed = as_a_best_fit_extractor_narrows_it(&e.path);
        let below = narrowed
            .strip_prefix("alice-eml/")
            .unwrap_or_else(|| panic!("escaped the root: {narrowed}"));
        assert_one_safe_component("an EML entry", below);
    }
}

#[test]
fn a_nested_mailbox_folds_to_the_maildir_dot_convention() {
    let entries = serialize_all(
        ExportFormat::Mbox,
        ExportOptions::new("alice"),
        &[message("Work/Reports", 1, 100, &[])],
    )
    .expect("serialize");
    assert!(
        entries
            .iter()
            .any(|e| e.path == "alice-mbox/Work.Reports.mbox"),
        "{:?}",
        entries.iter().map(|e| &e.path).collect::<Vec<_>>()
    );
}

// ── The container and its determinism goldens ───────────────────────────────

fn corpus() -> Vec<ExportMessage> {
    vec![
        distinct("Archive", 3, 900_000_000, "archived"),
        message("INBOX", 1, 837_596_665, &["\\Seen"]),
        distinct("INBOX", 2, 900_000_100, "second"),
        distinct("Sent", 4, 900_000_200, "sent-one"),
    ]
}

fn blob_digest(format: ExportFormat) -> String {
    let entries = serialize_all(format, ExportOptions::new("alice"), &corpus()).expect("serialize");
    let blob = build_blob(&entries).expect("blob");
    blake3::hash(&blob).to_hex().to_string()
}

/// Render the corpus's entries as `path | mtime | content-digest` lines.
///
/// This — not a digest of the compressed blob — is what the goldens below pin.
/// A blob digest would also move on any `zip` or `zstd` version bump, reding a
/// merge that changed nothing about *our* contract and saying only "two hex
/// strings differ". This rendering moves when and only when a serializer's
/// output moves, and it says which entry moved and how.
fn entry_manifest(format: ExportFormat) -> String {
    serialize_all(format, ExportOptions::new("alice"), &corpus())
        .expect("serialize")
        .iter()
        .map(|e| {
            let digest = if e.is_dir {
                "<dir>".to_string()
            } else {
                blake3::hash(&e.bytes).to_hex()[..16].to_string()
            };
            format!("{} | {} | {}", e.path, e.mtime_epoch, digest)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn the_blob_is_byte_identical_across_runs() {
    for format in [
        ExportFormat::Mbox,
        ExportFormat::MaildirPlus,
        ExportFormat::EmlZip,
    ] {
        assert_eq!(
            blob_digest(format),
            blob_digest(format),
            "{format:?} is not reproducible"
        );
    }
}

// The goldens. A wall-clock, hostname, pid or iteration-order leak still
// produces a perfectly *valid* archive, so only a pinned expectation catches
// one. Regenerating a golden is a deliberate act: it means the archive's bytes
// changed, which is a user-visible change to a format § Goal promises is stable
// for self-verification.

#[test]
fn mbox_matches_its_golden_entry_manifest() {
    assert_eq!(
        entry_manifest(ExportFormat::Mbox),
        "alice-mbox/Archive.mbox | 900000000 | 83fec7e6203a302a\n\
         alice-mbox/INBOX.mbox | 837596665 | 793ff2f9c16e7037\n\
         alice-mbox/Sent.mbox | 900000200 | f13ed8dcf7547115"
    );
}

#[test]
fn maildir_matches_its_golden_entry_manifest() {
    assert_eq!(
        entry_manifest(ExportFormat::MaildirPlus),
        "alice-maildir/Archive/cur/ | 900000000 | <dir>\n\
         alice-maildir/Archive/new/ | 900000000 | <dir>\n\
         alice-maildir/Archive/tmp/ | 900000000 | <dir>\n\
         alice-maildir/Archive/cur/900000000.8a9f8bab66b1f816.fauna.invalid:2,S | 900000000 | 8a9f8bab66b1f816\n\
         alice-maildir/INBOX/cur/ | 837596665 | <dir>\n\
         alice-maildir/INBOX/new/ | 837596665 | <dir>\n\
         alice-maildir/INBOX/tmp/ | 837596665 | <dir>\n\
         alice-maildir/INBOX/cur/837596665.eacc65e522f395f4.fauna.invalid:2,S | 837596665 | eacc65e522f395f4\n\
         alice-maildir/INBOX/cur/900000100.33688ccb59a9cec3.fauna.invalid:2,S | 900000100 | 33688ccb59a9cec3\n\
         alice-maildir/Sent/cur/ | 900000200 | <dir>\n\
         alice-maildir/Sent/new/ | 900000200 | <dir>\n\
         alice-maildir/Sent/tmp/ | 900000200 | <dir>\n\
         alice-maildir/Sent/cur/900000200.9cdad4b2d1ba5894.fauna.invalid:2,S | 900000200 | 9cdad4b2d1ba5894\n\
         alice-maildir/subscriptions | 837596665 | ea2fe7f77d2f13ac"
    );
}

#[test]
fn eml_matches_its_golden_entry_manifest() {
    assert_eq!(
        entry_manifest(ExportFormat::EmlZip),
        "alice-eml/archived@example.com.eml | 900000000 | 8a9f8bab66b1f816\n\
         alice-eml/abc123@example.com.eml | 837596665 | eacc65e522f395f4\n\
         alice-eml/second@example.com.eml | 900000100 | 33688ccb59a9cec3\n\
         alice-eml/sent-one@example.com.eml | 900000200 | 9cdad4b2d1ba5894\n\
         alice-eml/manifest.json | 837596665 | b0beea54463ff6cb"
    );
}

#[test]
fn the_blob_is_a_zip_inside_a_zstd_stream() {
    let entries = serialize_all(ExportFormat::EmlZip, ExportOptions::new("alice"), &corpus())
        .expect("serialize");

    let zip = build_zip(&entries).expect("zip");
    assert_eq!(&zip[..2], b"PK", "the container is a zip");

    let blob = build_blob(&entries).expect("blob");
    // zstd frame magic, little-endian 0xFD2FB528.
    assert_eq!(&blob[..4], &[0x28, 0xB5, 0x2F, 0xFD], "wrapped in zstd");

    let round_tripped = zstd::decode_all(&blob[..]).expect("decode");
    assert_eq!(round_tripped, zip, "the wrapper is the only difference");
}

#[test]
fn zip_entry_timestamps_come_from_the_mail_not_the_clock() {
    // 1996-07-18 — decades before any plausible run of this test. If the
    // container ever reached for the wall clock, the stored year would move.
    let entries = serialize_all(
        ExportFormat::MaildirPlus,
        ExportOptions::new("alice"),
        &[message("INBOX", 1, 837_596_665, &[])],
    )
    .expect("serialize");
    let zip = build_zip(&entries).expect("zip");
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(zip)).expect("read back");
    let file = archive.by_index(0).expect("first entry");
    assert_eq!(file.last_modified().expect("timestamp").year(), 1996);
}

#[test]
fn a_pre_1980_message_clamps_instead_of_falling_back_to_now() {
    // MS-DOS timestamps start at 1980; the `zip` crate's own fallback for an
    // unrepresentable date is the current time, which would break determinism.
    let entries = serialize_all(
        ExportFormat::MaildirPlus,
        ExportOptions::new("alice"),
        &[message("INBOX", 1, -1_000_000_000, &[])],
    )
    .expect("serialize");
    let zip = build_zip(&entries).expect("zip");
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(zip)).expect("read back");
    let file = archive.by_index(0).expect("first entry");
    assert_eq!(file.last_modified().expect("timestamp").year(), 1980);
}

// ── The streaming writer (§ Export pipeline's upload leg) ───────────────────

/// Drive the whole corpus through [`ExportArchiveStream`] at a given chunk
/// size, returning how many chunks came back *before* `finish` and the full
/// concatenation.
fn stream_corpus(format: ExportFormat, chunk_bytes: usize) -> (usize, Vec<u8>) {
    let entries = serialize_all(format, ExportOptions::new("alice"), &corpus()).expect("serialize");
    let mut stream = ExportArchiveStream::new(chunk_bytes).expect("stream");
    let mut blob = Vec::new();
    let mut early = 0usize;
    for entry in &entries {
        stream.push_entry(entry).expect("push entry");
        for chunk in stream.take_full_chunks() {
            assert_eq!(chunk.len(), chunk_bytes, "a full chunk is exactly the size");
            early += 1;
            blob.extend_from_slice(&chunk);
        }
    }
    for chunk in stream.finish().expect("finish") {
        blob.extend_from_slice(&chunk);
    }
    (early, blob)
}

#[test]
fn the_streamed_blob_is_the_blob() {
    // The claim `build_blob`'s delegation rests on: driving entries through the
    // streaming writer produces exactly the container a whole-run caller gets.
    for format in [
        ExportFormat::Mbox,
        ExportFormat::MaildirPlus,
        ExportFormat::EmlZip,
    ] {
        let entries =
            serialize_all(format, ExportOptions::new("alice"), &corpus()).expect("serialize");
        let (_, streamed) = stream_corpus(format, EXPORT_CHUNK_BYTES);
        assert_eq!(
            streamed,
            build_blob(&entries).expect("blob"),
            "{format:?}: the streamed container and the whole-run one differ"
        );
    }
}

#[test]
fn the_chunk_size_does_not_change_the_bytes() {
    // A chunk is a byte SLICE of the one zstd stream (§ Container shape), so
    // where the cuts fall must be invisible in the concatenation. A writer that
    // flushed the encoder per chunk — the shape § Container shape's "never an
    // independently-compressed unit" forbids — would fail here, because each
    // flush ends a block and moves the bytes.
    let (_, whole) = stream_corpus(ExportFormat::EmlZip, EXPORT_CHUNK_BYTES);
    for chunk_bytes in [1, 7, 64, 500, 4096] {
        let (_, cut) = stream_corpus(ExportFormat::EmlZip, chunk_bytes);
        assert_eq!(
            cut, whole,
            "chunk size {chunk_bytes} changed the container's bytes"
        );
    }
}

#[test]
fn chunks_are_emitted_before_finish() {
    // The point of the module: a 10 GiB export must not be buffered before the
    // first byte can be uploaded (§ Quota composition).
    //
    // The volume is deliberate. zstd emits nothing until it has a block's worth
    // of input, so a few-KB corpus comes out entirely at `finish` no matter how
    // the writer is built — a test at that size would pass for a fully
    // buffering writer too, and assert nothing. High-entropy bodies keep the
    // compressed output large enough to cross a real chunk boundary mid-run.
    let mut seed = 0x2545_F491_4F6C_DD1Du64;
    let messages: Vec<ExportMessage> = (0..8)
        .map(|i| {
            let body: Vec<u8> = std::iter::repeat_with(|| {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                seed as u8
            })
            .take(512 * 1024)
            .collect();
            ExportMessage {
                mailbox: "INBOX".into(),
                flags: vec![],
                body: [
                    format!("From: a@example.com\r\nSubject: m{i}\r\n\r\n").into_bytes(),
                    body,
                ]
                .concat(),
                internal_date_epoch: 900_000_000 + i,
                uid: i as u32 + 1,
                uid_validity: 9,
            }
        })
        .collect();

    let entries = serialize_all(ExportFormat::EmlZip, ExportOptions::new("alice"), &messages)
        .expect("serialize");
    let mut stream = ExportArchiveStream::new(64 * 1024).expect("stream");
    let mut early = 0usize;
    let mut blob = Vec::new();
    for entry in &entries {
        stream.push_entry(entry).expect("push entry");
        for chunk in stream.take_full_chunks() {
            early += 1;
            blob.extend_from_slice(&chunk);
        }
    }
    assert!(
        early > 0,
        "nothing was emitted until finish — the writer is buffering the run"
    );
    for chunk in stream.finish().expect("finish") {
        blob.extend_from_slice(&chunk);
    }
    assert_eq!(
        blob,
        build_blob(&entries).expect("blob"),
        "streaming a multi-chunk run must still produce the one container"
    );
}

#[test]
fn an_entry_larger_than_a_chunk_survives_the_header_patch() {
    // The hazard the sink's defer-until-flush drain exists for: the zip writer
    // patches a file's local header *after* writing its body, so a sink that
    // drained mid-body would already have let those header bytes go. With a
    // 64-byte chunk and a 256 KiB message every entry is far larger than a
    // chunk — the shape that broke restore for the nest's `ChannelWriter`
    // before it deferred.
    let big = ExportMessage {
        mailbox: "INBOX".into(),
        flags: vec![],
        body: [
            b"From: a@example.com\r\nSubject: big\r\n\r\n".to_vec(),
            vec![b'x'; 256 * 1024],
        ]
        .concat(),
        internal_date_epoch: 900_000_000,
        uid: 1,
        uid_validity: 9,
    };
    let entries = serialize_all(
        ExportFormat::EmlZip,
        ExportOptions::new("alice"),
        std::slice::from_ref(&big),
    )
    .expect("serialize");

    let mut stream = ExportArchiveStream::new(64).expect("stream");
    let mut blob = Vec::new();
    for entry in &entries {
        stream.push_entry(entry).expect("push entry");
        for chunk in stream.take_full_chunks() {
            blob.extend_from_slice(&chunk);
        }
    }
    for chunk in stream.finish().expect("finish") {
        blob.extend_from_slice(&chunk);
    }

    // Read it back the way a user's unzip would: a corrupted local header reads
    // as a broken archive here, not as a byte-count mismatch.
    let zip = zstd::decode_all(&blob[..]).expect("decode the zstd wrapper");
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(zip)).expect("read the zip back");
    let mut found = false;
    for i in 0..archive.len() {
        let mut file = archive.by_index(i).expect("entry");
        if file.name().ends_with(".eml") {
            let mut out = Vec::new();
            std::io::Read::read_to_end(&mut file, &mut out).expect("read the entry");
            assert_eq!(out, big.body, "the large entry did not survive the stream");
            found = true;
        }
    }
    assert!(found, "no .eml entry in the streamed archive");
}

// ── Mailbox names in archive paths (§ Format choices) ───────────────────────
//
// The path encoding must be injective: two distinct mailboxes on one archive
// path is not a cosmetic clash — the container refuses the duplicate and the
// WHOLE export dies, identically on every retry, naming neither mailbox. These
// run through the streaming container the drive loop uploads from, because
// that is where the refusal surfaces in production: mid-stream, after frames
// have already gone up.

/// The four classes of name that the pre-injective fold collapsed onto one
/// path, each reachable through the nest's mailbox-name validator (it refuses
/// NUL and CR/LF, not these). Every group is in the § Container shape total
/// order already. The separator group is deliberately three names, not two:
/// `A.B` < `A.C` < `A/B` by raw bytes, so the two names that used to fold
/// together are NOT adjacent in the stream — keying the serializer's state on
/// the folded name alone could not have fixed this.
const FOLD_CLASSES: &[(&str, &[&str])] = &[
    ("separator", &["A.B", "A.C", "A/B"]),
    ("reserved character", &["A:B", "A_B"]),
    ("trailing space", &["Reports", "Reports "]),
    ("control byte", &["Reports", "Reports\u{1}"]),
];

/// One byte-identical message per mailbox, same INTERNALDATE — the worst case:
/// the Maildir++ `<unique>` and the EML `Message-ID` stem collide too.
fn one_identical_message_per(mailboxes: &[&str]) -> Vec<ExportMessage> {
    mailboxes
        .iter()
        .map(|name| message(name, 1, 837_596_665, &["\\Seen"]))
        .collect()
}

/// `(name, is_dir)` per archive entry, in archive order.
type ReadBack = Vec<(String, bool)>;

/// Every entry of the archive, as a user's unzip reads it back after the
/// entries went through [`ExportArchiveStream`] in small chunks.
fn read_back_through_the_stream(entries: &[ExportEntry]) -> ReadBack {
    let mut stream = ExportArchiveStream::new(4096).expect("stream");
    let mut blob = Vec::new();
    for entry in entries {
        stream
            .push_entry(entry)
            .unwrap_or_else(|e| panic!("the container refused {:?}: {e}", entry.path));
        for chunk in stream.take_full_chunks() {
            blob.extend_from_slice(&chunk);
        }
    }
    for chunk in stream.finish().expect("finish") {
        blob.extend_from_slice(&chunk);
    }
    let zip = zstd::decode_all(&blob[..]).expect("decode the zstd wrapper");
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(zip)).expect("read the zip back");
    (0..archive.len())
        .map(|i| {
            let file = archive.by_index(i).expect("entry");
            (file.name().to_string(), file.is_dir())
        })
        .collect()
}

/// Export every fold class in `format` and assert each archive reads back with
/// every path distinct and nothing lost. Returns each class's name, mailbox
/// count and read-back entries for the format-specific assertions.
fn every_fold_class_exports(format: ExportFormat) -> Vec<(&'static str, usize, ReadBack)> {
    FOLD_CLASSES
        .iter()
        .map(|(class, mailboxes)| {
            let messages = one_identical_message_per(mailboxes);
            let entries = serialize_all(format, ExportOptions::new("alice"), &messages)
                .unwrap_or_else(|e| panic!("{format:?} / {class}: serialize: {e}"));
            let names = read_back_through_the_stream(&entries);
            assert_eq!(
                names.len(),
                entries.len(),
                "{format:?} / {class}: the archive lost entries"
            );
            let distinct: std::collections::HashSet<_> = names.iter().map(|(n, _)| n).collect();
            assert_eq!(
                distinct.len(),
                names.len(),
                "{format:?} / {class}: two entries share a path: {names:?}"
            );
            (*class, mailboxes.len(), names)
        })
        .collect()
}

#[test]
fn mbox_exports_every_fold_class_to_distinct_files() {
    for (class, mailboxes, names) in every_fold_class_exports(ExportFormat::Mbox) {
        let files = names.iter().filter(|(n, _)| n.ends_with(".mbox")).count();
        assert_eq!(
            files, mailboxes,
            "{class}: one .mbox per mailbox: {names:?}"
        );
    }
}

#[test]
fn maildir_exports_every_fold_class_to_distinct_directories() {
    for (class, mailboxes, names) in every_fold_class_exports(ExportFormat::MaildirPlus) {
        let messages = names
            .iter()
            .filter(|(n, d)| !d && n.contains("/cur/"))
            .count();
        assert_eq!(
            messages, mailboxes,
            "{class}: one message per mailbox: {names:?}"
        );
        let trees = names
            .iter()
            .filter(|(n, d)| *d && n.ends_with("/tmp/"))
            .count();
        assert_eq!(trees, mailboxes, "{class}: one tree per mailbox: {names:?}");
    }
}

#[test]
fn eml_exports_every_fold_class_as_a_regression_pin() {
    // A pin, not a repair: the EML path never carried the mailbox (a flat
    // directory and a run-wide filename set), so these always exported. Pinned
    // so a future change to the EML layout inherits the same witness.
    for (class, mailboxes, names) in every_fold_class_exports(ExportFormat::EmlZip) {
        let emls = names.iter().filter(|(n, _)| n.ends_with(".eml")).count();
        assert_eq!(emls, mailboxes, "{class}: one .eml per message: {names:?}");
    }
}

#[test]
fn the_maildir_subscriptions_index_names_exactly_the_directories() {
    for (class, mailboxes) in FOLD_CLASSES {
        let entries = serialize_all(
            ExportFormat::MaildirPlus,
            ExportOptions::new("alice"),
            &one_identical_message_per(mailboxes),
        )
        .expect("serialize");
        let directories: Vec<String> = entries
            .iter()
            .filter(|e| e.is_dir && e.path.ends_with("/cur/"))
            .map(|e| {
                e.path
                    .strip_prefix("alice-maildir/")
                    .and_then(|p| p.strip_suffix("/cur/"))
                    .expect("<root>/<dir>/cur/")
                    .to_string()
            })
            .collect();
        let index = text(entry(&entries, "alice-maildir/subscriptions"));
        let lines: Vec<&str> = index.lines().collect();
        assert_eq!(
            lines, directories,
            "{class}: the index is not the tree beside it"
        );
    }
}

/// The inverse of `paths::encode_path_component`, kept test-side: nothing in
/// the product decodes a path (a future re-import reads the EML manifest), but
/// a decoder that round-trips is what *proves* the encoding injective.
fn decode_path_component(component: &str) -> String {
    if component == "%" {
        return String::new();
    }
    let bytes = component.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while at < bytes.len() {
        match bytes[at] {
            b'.' => {
                out.push(b'/');
                at += 1;
            }
            b'%' => {
                let hex = std::str::from_utf8(&bytes[at + 1..at + 3]).expect("two hex digits");
                assert_eq!(hex, hex.to_ascii_uppercase(), "escapes are uppercase");
                out.push(u8::from_str_radix(hex, 16).expect("hex"));
                at += 3;
            }
            b => {
                out.push(b);
                at += 1;
            }
        }
    }
    String::from_utf8(out).expect("utf-8")
}

/// What an extractor narrowing a UTF-16 path to a Windows ANSI code page with
/// best-fit mapping makes of it — the union over code pages, since the user
/// picks the machine: the whole fullwidth ASCII block U+FF01–U+FF5E becomes the
/// ASCII it looks like, `¥` becomes `\` (code page 932, where byte 0x5C is the
/// yen sign) and `₩` likewise (949), the slash and backslash operators become
/// the separator they look like, and the dot look-alikes (`﹒` `․` `‥` `…`)
/// the dots they look like. Everything else narrows to its compatibility
/// decomposition with the combining marks dropped (`ü` → `u`, `﹕` → `:`), the
/// most any best-fit table does. Written independently of the product's escape
/// set, so a look-alike the encoder keeps verbatim is judged by what it narrows
/// to.
fn as_a_best_fit_extractor_narrows_it(path: &str) -> String {
    use unicode_normalization::{UnicodeNormalization, char::is_combining_mark};
    path.chars()
        .flat_map(|ch| -> Vec<char> {
            match ch {
                '\u{FF01}'..='\u{FF5E}' => {
                    vec![char::from_u32(ch as u32 - 0xFEE0).expect("ASCII")]
                }
                '¥' | '₩' | '∖' | '⧵' | '﹨' => vec!['\\'],
                '∕' | '⁄' => vec!['/'],
                '﹒' | '․' => vec!['.'],
                '‥' => vec!['.', '.'],
                '…' => vec!['.', '.', '.'],
                c => std::iter::once(c)
                    .nfkd()
                    .filter(|d| !is_combining_mark(*d))
                    .collect(),
            }
        })
        .collect()
}

/// Names chosen to break a naive encoding: every escaped character, the
/// separator at both ends and doubled, the escape character itself and a
/// literal that looks like an escape, whitespace at both ends, controls,
/// traversal, non-ASCII, the empty name, and the look-alikes — fullwidth,
/// small-form, dot-leader and operator — a best-fit extractor narrows back to
/// separators, dots and reserved characters.
const HOSTILE_NAMES: &[&str] = &[
    "",
    "/",
    "//",
    "/a",
    "a/",
    "a//b",
    ".",
    "..",
    "...",
    ".hidden",
    "%",
    "%2E",
    "%2F",
    "A.B",
    "A/B",
    "A%2EB",
    "A:B",
    "A_B",
    "a\\b",
    "*?\"<>|",
    "Reports",
    "Reports ",
    "Reports  ",
    " Reports",
    "Reports.",
    "Reports\u{1}",
    "Reports\u{7f}",
    "INBOX\nFakeFolder",
    "../../etc/passwd",
    "Entwürfe/Q1",
    "日本語",
    "A B",
    "a／．．／．．／b",
    "．．",
    "．hidden",
    "Reports．",
    "x＼．．＼y",
    "x／//／y",
    "C：x",
    "＊？＂＜＞｜",
    "％41ux",
    "a///b",
    "a¥b",
    "x¥//¥y",
    "x₩/₩y",
    "a∕..∕b",
    "a⁄b",
    "a∖b",
    "a⧵b﹨c",
    "﹒﹒",
    "․․",
    "‥",
    "…",
    "﹒hidden",
    "Reports﹒",
    "x﹒﹒/﹒﹒y",
    "a﹕b",
    "﹪41ux",
];

#[test]
fn no_encoded_name_holds_two_adjacent_dots_whatever_the_nest_allowed() {
    // A `.` in the output is only ever one folded interior `/`, and no kept
    // character narrows to a `.`, so no component holds `..` — as encoded, nor
    // once a best-fit extractor has narrowed the look-alikes around it — even
    // from a `//` the nest's own name validation would have refused.
    let joined = HOSTILE_NAMES.iter().flat_map(|a| {
        std::iter::once(a.to_string()).chain(HOSTILE_NAMES.iter().map(move |b| format!("{a}/{b}")))
    });
    for name in joined {
        let encoded = paths::encode_path_component(&name);
        assert!(!encoded.contains(".."), "{name:?} → {encoded:?} holds `..`");
        let narrowed = as_a_best_fit_extractor_narrows_it(&encoded);
        for component in narrowed.split(['/', '\\']) {
            assert!(
                component != "." && component != "..",
                "{name:?} → {encoded:?} narrows to the traversal {narrowed:?}"
            );
        }
    }
    assert_eq!(paths::encode_path_component("a//b"), "a.%2Fb");
}

#[test]
fn the_path_encoding_round_trips_so_it_is_injective() {
    let mut seen = std::collections::HashMap::new();
    for name in HOSTILE_NAMES {
        let encoded = paths::encode_path_component(name);
        assert_eq!(
            decode_path_component(&encoded),
            *name,
            "{name:?} → {encoded:?} does not decode back"
        );
        if let Some(other) = seen.insert(encoded.clone(), *name) {
            panic!("{other:?} and {name:?} share the path component {encoded:?}");
        }
    }
}

#[test]
fn the_path_encoding_is_always_one_safe_component() {
    for name in HOSTILE_NAMES {
        let encoded = paths::encode_path_component(name);
        let narrowed = as_a_best_fit_extractor_narrows_it(&encoded);
        // A narrowed `％` would forge an escape: the path would decode to
        // another name than the one it holds.
        assert_eq!(
            narrowed.matches('%').count(),
            encoded.matches('%').count(),
            "{name:?} → {encoded:?} narrows to a forged escape {narrowed:?}"
        );
        for c in [encoded, narrowed] {
            assert_one_safe_component(name, &c);
        }
    }
}

/// `c`, the component `name` encoded to — or what an extractor made of it — is
/// one safe path component on every filesystem the archive can land on.
fn assert_one_safe_component(name: &str, c: &str) {
    assert!(!c.is_empty(), "{name:?} encoded empty");
    assert!(c != "." && c != "..", "{name:?} → traversal {c:?}");
    assert!(!c.contains(['/', '\\']), "{name:?} → separator in {c:?}");
    assert!(!c.starts_with('.'), "{name:?} → leading dot {c:?}");
    // Windows drops trailing dots and spaces on extraction, which would
    // fold two components back together after the archive left us.
    assert!(
        !c.ends_with(['.', ' ']),
        "{name:?} → {c:?} loses a byte on Windows"
    );
    assert!(
        !c.chars().any(|ch| ch.is_ascii_control()),
        "{name:?} → control in {c:?}"
    );
    assert!(
        !c.contains([':', '*', '?', '"', '<', '>', '|']),
        "{name:?} → reserved character in {c:?}"
    );
}

#[test]
fn the_path_encoding_leaves_ordinary_names_and_every_valid_handle_alone() {
    // The archive a user already expects: plain names are their own path, the
    // hierarchy delimiter keeps its Maildir++ `.` fold, and non-ASCII is kept
    // (zip stores UTF-8 names). Valid handles are `[a-z0-9-]`, so the root
    // directory `<handle>-<format>` never gains an escape.
    for (name, component) in [
        ("INBOX", "INBOX"),
        ("Sent", "Sent"),
        ("Custom-Project-A", "Custom-Project-A"),
        ("Work/Reports", "Work.Reports"),
        ("Work/Reports/2026", "Work.Reports.2026"),
        ("A B", "A B"),
        ("Entwürfe", "Entwürfe"),
        ("Entwürfe/Q1", "Entwürfe.Q1"),
        ("日本語", "日本語"),
        // Fullwidth letters, digits and brackets narrow to nothing unsafe.
        ("Ｑ１（下書き）", "Ｑ１（下書き）"),
        ("alice", "alice"),
        ("a1-b2-c3", "a1-b2-c3"),
    ] {
        assert_eq!(paths::encode_path_component(name), component, "{name:?}");
    }
    // …and the characters that would otherwise collide are escaped, not folded.
    for (name, component) in [
        ("A.B", "A%2EB"),
        ("A:B", "A%3AB"),
        ("Reports ", "Reports%20"),
        ("Reports\u{1}", "Reports%01"),
        ("100%", "100%25"),
        ("/", "%2F"),
        ("", "%"),
        // …and so are their fullwidth twins, per UTF-8 byte.
        ("2026／09", "2026%EF%BC%8F09"),
        ("．．", "%EF%BC%8E%EF%BC%8E"),
        ("C：x", "C%EF%BC%9Ax"),
        // …and every other character that decomposes to one, like the dot
        // look-alikes.
        ("﹒﹒", "%EF%B9%92%EF%B9%92"),
        ("a․b", "a%E2%80%A4b"),
        ("Q1…", "Q1%E2%80%A6"),
    ] {
        assert_eq!(paths::encode_path_component(name), component, "{name:?}");
    }
}

#[test]
fn a_mailbox_named_subscriptions_cannot_shadow_the_maildir_index() {
    // The index file `subscriptions` sits at the root beside the mailbox
    // directories; a directory of the same name — in either case, since the
    // archive is extracted onto case-insensitive filesystems too — could not be
    // created beside it.
    let entries = serialize_all(
        ExportFormat::MaildirPlus,
        ExportOptions::new("alice"),
        &one_identical_message_per(&["Subscriptions", "subscriptions"]),
    )
    .expect("serialize");
    let names = read_back_through_the_stream(&entries);
    for (name, is_dir) in &names {
        let top = name
            .strip_prefix("alice-maildir/")
            .expect("under the root")
            .split('/')
            .next()
            .unwrap();
        if top.eq_ignore_ascii_case("subscriptions") {
            assert!(
                !is_dir && name == "alice-maildir/subscriptions",
                "{names:?}"
            );
        }
    }
    assert_eq!(
        text(entry(&entries, "alice-maildir/subscriptions")),
        "%53ubscriptions\n%73ubscriptions\n"
    );
}

// ── Archive paths on the extracting filesystem (§ Format choices) ───────────
//
// Distinct inside the zip is not distinct once extracted: default APFS, HFS+,
// NTFS and FAT fold case, macOS folds canonically-equivalent Unicode, Windows
// reserves device names, and most filesystems cap a component at 255 bytes.
// Every witness below asserts on the archive's paths the way such a filesystem
// would see them, never on the encoder's own notion of "taken".

/// A test-side fold at least as coarse as the filesystems named above, written
/// independently of `paths::extraction_key` so the witnesses do not grade the
/// product against itself: canonical decomposition, then lowercase.
fn as_a_folding_filesystem_sees_it(path: &str) -> String {
    use unicode_normalization::UnicodeNormalization;
    path.nfd().flat_map(char::to_lowercase).nfc().collect()
}

/// The Windows device names, matched the way Windows matches them: the part of
/// a component before its first `.`, trailing spaces ignored, any case.
fn is_a_windows_device_name(component: &str) -> bool {
    let stem = component
        .split('.')
        .next()
        .unwrap_or(component)
        .trim_end_matches(' ')
        .to_ascii_uppercase();
    let numbered = |prefix: &str| {
        stem.strip_prefix(prefix).is_some_and(|rest| {
            let mut chars = rest.chars();
            matches!(
                (chars.next(), chars.next()),
                (Some('0'..='9' | '¹' | '²' | '³'), None)
            )
        })
    };
    matches!(
        stem.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
    ) || numbered("COM")
        || numbered("LPT")
}

/// Assert an archive survives extraction onto a folding filesystem: every entry
/// keeps a path of its own under the fold, no component is a device name, and
/// no component is longer than the 255 bytes most filesystems allow.
fn assert_extractable(context: &str, entries: &[ExportEntry]) {
    let names = read_back_through_the_stream(entries);
    assert_eq!(names.len(), entries.len(), "{context}: entries lost");
    let mut seen = std::collections::HashMap::new();
    for (name, _) in &names {
        let folded = as_a_folding_filesystem_sees_it(name.trim_end_matches('/'));
        if let Some(other) = seen.insert(folded, name) {
            panic!("{context}: {other:?} and {name:?} extract to one path");
        }
        for component in name.split('/').filter(|c| !c.is_empty()) {
            assert!(
                !is_a_windows_device_name(component),
                "{context}: {component:?} in {name:?} is a Windows device name"
            );
            assert!(
                component.len() <= 255,
                "{context}: a {}-byte component in {name:?}",
                component.len()
            );
        }
    }
}

/// The top-level components under the archive root, in archive order, without
/// repeats — the mailbox files or directories.
fn top_components(root: &str, entries: &[ExportEntry]) -> Vec<String> {
    let mut tops: Vec<String> = Vec::new();
    for entry in entries {
        let top = entry
            .path
            .strip_prefix(root)
            .and_then(|p| p.strip_prefix('/'))
            .expect("under the root")
            .split('/')
            .next()
            .unwrap()
            .to_string();
        if !tops.contains(&top) {
            tops.push(top);
        }
    }
    tops
}

/// The extraction classes, each group already in the § Container shape total
/// order. Every mailbox carries the SAME message — the worst case, because the
/// Maildir++ `<unique>` then collides too, and two case-variant directories
/// merged by the filesystem would silently keep one copy.
const EXTRACTION_CLASSES: &[(&str, &[&str])] = &[
    ("case", &["Work", "work"]),
    ("case, four deep", &["WORk", "WOrk", "Work", "work"]),
    ("case beyond ASCII", &["Été", "été"]),
    ("normalization", &["Cafe\u{301}", "Caf\u{e9}"]),
    ("case and normalization", &["CAFE\u{301}", "caf\u{e9}"]),
    (
        "device names",
        &["AUX", "Aux/Reports", "COM1", "LPT¹", "con", "nul"],
    ),
    ("first character already escaped", &[":Work", ":work"]),
];

#[test]
fn every_extraction_class_survives_a_folding_filesystem_in_every_format() {
    for format in [
        ExportFormat::Mbox,
        ExportFormat::MaildirPlus,
        ExportFormat::EmlZip,
    ] {
        for (class, mailboxes) in EXTRACTION_CLASSES {
            let entries = serialize_all(
                format,
                ExportOptions::new("alice"),
                &one_identical_message_per(mailboxes),
            )
            .unwrap_or_else(|e| panic!("{format:?} / {class}: {e}"));
            assert_extractable(&format!("{format:?} / {class}"), &entries);
        }
    }
}

#[test]
fn a_disambiguated_mailbox_path_still_decodes_to_its_name() {
    // The escape ladder only ever escapes MORE characters, and decoding is
    // context-free, so every mailbox is still recovered exactly from its path —
    // the round-trip § Mailbox names in archive paths promises.
    for (class, mailboxes) in EXTRACTION_CLASSES {
        let entries = serialize_all(
            ExportFormat::Mbox,
            ExportOptions::new("alice"),
            &one_identical_message_per(mailboxes),
        )
        .expect("serialize");
        let decoded: Vec<String> = top_components("alice-mbox", &entries)
            .iter()
            .map(|c| decode_path_component(c.strip_suffix(".mbox").expect(".mbox")))
            .collect();
        assert_eq!(&decoded, mailboxes, "{class}");
    }
}

#[test]
fn the_first_mailbox_of_a_fold_class_keeps_its_name_and_a_lone_one_always_does() {
    let tops = |mailboxes: &[&str]| {
        let entries = serialize_all(
            ExportFormat::Mbox,
            ExportOptions::new("alice"),
            &one_identical_message_per(mailboxes),
        )
        .expect("serialize");
        top_components("alice-mbox", &entries)
    };
    // The second arrival has its first character escaped — still readable.
    assert_eq!(tops(&["Work", "work"]), ["Work.mbox", "%77ork.mbox"]);
    // Exported alone, the same mailbox is its own name: the path depends on the
    // scope, which is the price of leaving every ordinary name untouched.
    assert_eq!(tops(&["work"]), ["work.mbox"]);
    // The ladder's last rung, when the first-character escape is taken too.
    assert_eq!(
        tops(&["WORk", "WOrk", "Work", "work"]),
        [
            "WORk.mbox",
            "%57Ork.mbox",
            "%57%6F%72%6B.mbox",
            "%77ork.mbox"
        ]
    );
    // A device name escapes its first character whatever else is exported, and
    // the check reads the part before the first `.` — which a folded `/` makes.
    assert_eq!(
        tops(&["Aux", "Aux/Reports", "Auxiliary"]),
        ["%41ux.mbox", "%41ux.Reports.mbox", "Auxiliary.mbox"]
    );
    // The look-alike escape holds on the second rung too, not only the first:
    // the rung spends its escape on the first character and keeps `／` escaped.
    assert_eq!(
        tops(&["Work／1", "work／1"]),
        ["Work%EF%BC%8F1.mbox", "%77ork%EF%BC%8F1.mbox"]
    );
}

#[test]
fn the_same_message_in_two_case_variant_maildirs_keeps_both_copies() {
    let entries = serialize_all(
        ExportFormat::MaildirPlus,
        ExportOptions::new("alice"),
        &one_identical_message_per(&["Work", "work"]),
    )
    .expect("serialize");
    let messages: Vec<String> = entries
        .iter()
        .filter(|e| !e.is_dir && e.path.contains("/cur/"))
        .map(|e| as_a_folding_filesystem_sees_it(&e.path))
        .collect();
    assert_eq!(messages.len(), 2);
    assert_ne!(
        messages[0], messages[1],
        "one copy is lost on a case-folding filesystem"
    );
    assert_eq!(
        text(entry(&entries, "alice-maildir/subscriptions")),
        "Work\n%77ork\n"
    );
}

#[test]
fn an_over_long_mailbox_name_is_cut_to_a_component_that_still_tells_it_apart() {
    // 255 bytes is the nest's own cap on a mailbox name. `.mbox` alone pushes a
    // plain one past a 255-byte component, and escapes can triple it.
    let plain_a = format!("{}a", "n".repeat(254));
    let plain_b = format!("{}b", "n".repeat(254));
    let escaped = ":".repeat(255);
    let wide = "日".repeat(85);
    let mut mailboxes = [
        plain_a.as_str(),
        plain_b.as_str(),
        escaped.as_str(),
        wide.as_str(),
    ];
    mailboxes.sort_by_key(|m| m.as_bytes().to_vec());

    for format in [ExportFormat::Mbox, ExportFormat::MaildirPlus] {
        let entries = serialize_all(
            format,
            ExportOptions::new("alice"),
            &one_identical_message_per(&mailboxes),
        )
        .expect("serialize");
        assert_extractable(&format!("{format:?} / length"), &entries);

        let root = entries[0].path.split('/').next().unwrap().to_string();
        let tops: Vec<String> = top_components(&root, &entries)
            .into_iter()
            .filter(|t| t != "subscriptions")
            .collect();
        assert_eq!(
            tops.len(),
            mailboxes.len(),
            "one path per mailbox: {tops:?}"
        );
        for top in &tops {
            let name = top.strip_suffix(".mbox").unwrap_or(top);
            // The cut keeps a readable prefix, never splits an escape or a
            // character, and ends in the marker + the raw name's digest.
            let (prefix, digest) = name.rsplit_once("%~").expect("a cut name carries `%~`");
            assert_eq!(digest.len(), 32, "{name}");
            assert!(digest.chars().all(|c| c.is_ascii_hexdigit()), "{name}");
            assert!(!prefix.is_empty(), "{name}");
            decode_path_component(prefix);
        }
    }
    // A name that fits is never cut.
    let fits = "n".repeat(200);
    let entries = serialize_all(
        ExportFormat::Mbox,
        ExportOptions::new("alice"),
        &one_identical_message_per(&[fits.as_str()]),
    )
    .expect("serialize");
    assert_eq!(
        top_components("alice-mbox", &entries),
        [format!("{fits}.mbox")]
    );
}

#[test]
fn eml_filenames_survive_a_folding_filesystem_too() {
    // The EML stem comes from the Message-ID, which is case-sensitive and can
    // spell a device name; the run-wide filename set used to compare exactly.
    let with_id = |uid: u32, id: &str| {
        let mut msg = message("INBOX", uid, 100, &[]);
        msg.body =
            format!("From: s@example.com\r\nMessage-ID: {id}\r\n\r\nBody {uid}.\r\n").into_bytes();
        msg
    };
    let entries = serialize_all(
        ExportFormat::EmlZip,
        ExportOptions::new("alice"),
        &[
            with_id(1, "<ABC@example.com>"),
            with_id(2, "<abc@example.com>"),
            with_id(3, "<CON>"),
            with_id(4, "<aux.17@example.com>"),
            with_id(5, "<Auxiliary@example.com>"),
        ],
    )
    .expect("serialize");
    assert_extractable("EmlZip / message ids", &entries);

    let names: Vec<&str> = entries
        .iter()
        .map(|e| e.path.strip_prefix("alice-eml/").expect("root"))
        .collect();
    assert_eq!(names[0], "ABC@example.com.eml");
    assert_eq!(names[1], "abc@example.com-2.eml");
    // A device stem declines to the digest, like any other unusable identifier.
    for digest_named in [names[2], names[3]] {
        let stem = digest_named.trim_end_matches(".eml");
        assert_eq!(stem.len(), 16, "{digest_named}");
        assert!(
            stem.chars().all(|c| c.is_ascii_hexdigit()),
            "{digest_named}"
        );
    }
    assert_eq!(names[4], "Auxiliary@example.com.eml");
}

// ── Container limits: ZIP64 on the streaming path (§ Quota composition) ─────
//
// A classic zip counts entries in 16 bits and addresses offsets and sizes in
// 32; an export capped at 10 GiB, one entry per message, can exceed either.
// Nothing here selects ZIP64 for the *archive* (only `large_file` per entry),
// so whether the container promotes on its own is a property of the `zip`
// dependency — settled here by building such an archive through the
// forward-only `ExportArchiveStream` the drive loop uploads from and reading it
// back, never by reading the dependency's source. The forward-only sink is the
// point: a writer that promoted by seeking back to rewrite an early header
// would fail here even where the buffered `build_zip` path succeeds.

/// Drive `count` entries through the streaming container and return the
/// decompressed zip. `entry` makes the i-th one.
fn stream_entries(count: usize, entry: impl Fn(usize) -> ExportEntry) -> Vec<u8> {
    let mut stream = ExportArchiveStream::new(EXPORT_CHUNK_BYTES).expect("stream");
    let mut blob = Vec::new();
    for i in 0..count {
        stream.push_entry(&entry(i)).expect("push entry");
        for chunk in stream.take_full_chunks() {
            blob.extend_from_slice(&chunk);
        }
    }
    for chunk in stream.finish().expect("finish") {
        blob.extend_from_slice(&chunk);
    }
    zstd::decode_all(&blob[..]).expect("decode the zstd wrapper")
}

/// The ZIP64 end-of-central-directory record's signature (`PK\x06\x06`).
const ZIP64_EOCD: &[u8] = b"PK\x06\x06";

#[test]
fn an_archive_over_65535_entries_is_promoted_to_zip64_and_reads_back() {
    const ENTRIES: usize = u16::MAX as usize + 1_000;
    let zip = stream_entries(ENTRIES, |i| {
        ExportEntry::file(
            format!("alice-eml/{i:06}.eml"),
            format!("Subject: m{i}\r\n\r\nbody {i}\r\n").into_bytes(),
            900_000_000,
        )
    });

    // The format-level witness: a classic end record cannot hold the count, so
    // the archive must carry the ZIP64 one. Any reader then agrees on the
    // count, not just the `zip` crate that wrote it.
    let tail = &zip[zip.len().saturating_sub(4096)..];
    assert!(
        tail.windows(4).any(|w| w == ZIP64_EOCD),
        "{ENTRIES} entries but no ZIP64 end-of-central-directory record"
    );

    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(zip)).expect("read the zip back");
    assert_eq!(archive.len(), ENTRIES, "entries lost past 65 535");
    let last = ENTRIES - 1;
    let mut file = archive
        .by_name(&format!("alice-eml/{last:06}.eml"))
        .expect("the last entry is addressable");
    let mut out = Vec::new();
    std::io::Read::read_to_end(&mut file, &mut out).expect("read the last entry");
    assert_eq!(
        out,
        format!("Subject: m{last}\r\n\r\nbody {last}\r\n").into_bytes()
    );
}

/// Past 4 GiB of archive, entry offsets no longer fit 32 bits.
///
/// Ignored because it moves over 4 GiB through an unoptimized zstd and CRC
/// and writes the decompressed archive to the temp directory — about forty
/// seconds of one core in a debug build (measured 2026-09-21, green: the
/// container emits the ZIP64 records unprompted, and the entry past 4 GiB
/// reads back CRC-checked). Run it deliberately:
/// `cargo test -p fauna-mail --features mail-export --lib -- --ignored an_archive_over_4_gib`.
#[test]
#[ignore = "moves >4 GiB through the container; run deliberately (see the doc comment)"]
fn an_archive_over_4_gib_is_promoted_to_zip64_and_reads_back() {
    use std::io::{Read, Seek, SeekFrom, Write};

    const ENTRY_BYTES: usize = 64 * 1024 * 1024;
    // 66 × 64 MiB = 4.125 GiB: every entry is small enough that production
    // would never set `large_file` on it, yet the last ones sit past 4 GiB.
    const ENTRIES: usize = 66;
    let body = vec![b'x'; ENTRY_BYTES];

    let mut stream = ExportArchiveStream::new(EXPORT_CHUNK_BYTES).expect("stream");
    let mut decompressed = tempfile::tempfile().expect("temp file");
    let mut decoder = zstd::stream::write::Decoder::new(&mut decompressed).expect("decoder");
    let mut entry = ExportEntry::file(String::new(), body, 900_000_000);
    for i in 0..ENTRIES {
        entry.path = format!("alice-eml/{i:03}.eml");
        stream.push_entry(&entry).expect("push entry");
        for chunk in stream.take_full_chunks() {
            decoder.write_all(&chunk).expect("decode chunk");
        }
    }
    for chunk in stream.finish().expect("finish") {
        decoder.write_all(&chunk).expect("decode chunk");
    }
    decoder.flush().expect("flush decoder");
    drop(decoder);

    let size = decompressed.seek(SeekFrom::End(0)).expect("size");
    assert!(
        size > u64::from(u32::MAX),
        "the archive is only {size} bytes"
    );
    decompressed
        .seek(SeekFrom::Start(size.saturating_sub(4096)))
        .expect("seek to the tail");
    let mut tail = Vec::new();
    decompressed.read_to_end(&mut tail).expect("read the tail");
    assert!(
        tail.windows(4).any(|w| w == ZIP64_EOCD),
        "a {size}-byte archive but no ZIP64 end-of-central-directory record"
    );

    decompressed.rewind().expect("rewind");
    let mut archive = zip::ZipArchive::new(decompressed).expect("read the zip back");
    assert_eq!(archive.len(), ENTRIES);
    let mut file = archive
        .by_name(&format!("alice-eml/{:03}.eml", ENTRIES - 1))
        .expect("the last entry, past 4 GiB, is addressable");
    let mut read = 0usize;
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = file
            .read(&mut buf)
            .expect("read past 4 GiB (CRC checked at EOF)");
        if n == 0 {
            break;
        }
        assert!(
            buf[..n].iter().all(|&b| b == b'x'),
            "wrong bytes past 4 GiB"
        );
        read += n;
    }
    assert_eq!(read, ENTRY_BYTES);
}
