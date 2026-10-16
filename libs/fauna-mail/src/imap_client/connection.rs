//! Sans-io response framing over [`imap_proto`].
//!
//! `imap_proto::parse_response` is a pure function from bytes to a *borrowed*
//! `Response<'a>`. Two consequences shape this module:
//!
//! 1. **A truncated response is `Err::Incomplete`, not `Err::Error`.** That is
//!    exactly the feed-more-bytes contract we need: read from the transport,
//!    append, retry. An `Err::Error` means the source server sent something
//!    that is not IMAP, and we fail the session.
//! 2. **`Response<'a>` borrows the read buffer**, so it cannot escape a
//!    `&mut self` method that also drains that buffer. Every response is
//!    therefore projected immediately into an owned [`ServerEvent`] carrying
//!    only the fields the import path uses. Everything above this module is
//!    lifetime-free.

use imap_proto::parser::parse_response;
use imap_proto::types::{
    AttributeValue, Capability, MailboxDatum, NameAttribute, Response, ResponseCode, Status,
};

use super::session::SpecialUse;
use super::transport::ImapTransport;
use super::{ImapClientError, MAX_RESPONSE_BUFFER};

/// Bytes pulled from the transport per `read` call.
const READ_CHUNK: usize = 16 * 1024;

/// The FETCH attributes the import path reads. Everything else the source
/// server volunteers (BODYSTRUCTURE, ENVELOPE, MODSEQ, Gmail labels…) is
/// dropped — we asked for four attributes and tolerate extras.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct FetchAttrs {
    pub(crate) uid: Option<u32>,
    pub(crate) flags: Vec<String>,
    pub(crate) internal_date: Option<String>,
    pub(crate) body: Option<Vec<u8>>,
    pub(crate) rfc822_size: Option<u32>,
}

/// One owned IMAP response, projected to what the import path consumes.
///
/// Not `Clone`: `imap_proto::Status` is not, and an event is consumed once.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ServerEvent {
    /// `* CAPABILITY …`, uppercased for case-insensitive membership tests.
    Capabilities(Vec<String>),
    /// `* LIST (…) "/" "INBOX"`.
    List {
        name: String,
        selectable: bool,
        special_use: Option<SpecialUse>,
    },
    /// `* n EXISTS`.
    Exists(u32),
    /// `* SEARCH 1 2 3`.
    Search(Vec<u32>),
    /// `* n FETCH (…)`.
    Fetch(FetchAttrs),
    /// An untagged status line: `* OK [UIDVALIDITY 1] …`, `* BYE …`.
    Data {
        status: Status,
        uid_validity: Option<u32>,
    },
    /// A tagged completion: `A001 OK …`.
    Done {
        tag: String,
        status: Status,
        information: String,
    },
    /// `+ …` — the server wants a continuation (AUTHENTICATE, literal upload).
    Continue,
    /// `* STATUS <mailbox> (MESSAGES n SIZE n)` — the two items the import's
    /// Confirm estimate reads; any other item is dropped. Parsed here, not by
    /// [`imap_proto`], which refuses RFC 8438's `SIZE` item ([`parse_status`]).
    Status {
        messages: Option<u32>,
        size: Option<u64>,
    },
    /// A well-formed response the import path does not use.
    Ignored,
}

/// A framed IMAP connection: owns the transport, the unparsed byte buffer, and
/// the tag counter.
pub(crate) struct Connection<T> {
    transport: T,
    /// Bytes received but not yet consumed by a complete response.
    buf: Vec<u8>,
    next_tag: u32,
}

impl<T: ImapTransport> Connection<T> {
    pub(crate) fn new(transport: T) -> Self {
        Self {
            transport,
            buf: Vec::new(),
            next_tag: 0,
        }
    }

    /// A fresh command tag, `A0001`-style and monotone within the session.
    pub(crate) fn next_tag(&mut self) -> String {
        self.next_tag += 1;
        format!("A{:04}", self.next_tag)
    }

    /// Write one command line, appending the CRLF terminator.
    ///
    /// `line` may carry credentials (`LOGIN`), so it is never logged here or
    /// echoed into an error — [`ImapClientError::Rejected`] carries only the
    /// verb.
    pub(crate) async fn send_line(&mut self, line: &str) -> Result<(), ImapClientError> {
        self.transport
            .write_all(line.as_bytes())
            .await
            .map_err(ImapClientError::transport)?;
        self.transport
            .write_all(b"\r\n")
            .await
            .map_err(ImapClientError::transport)
    }

    /// Write raw bytes (a literal payload, or an AUTHENTICATE continuation).
    pub(crate) async fn send_raw(&mut self, bytes: &[u8]) -> Result<(), ImapClientError> {
        self.transport
            .write_all(bytes)
            .await
            .map_err(ImapClientError::transport)
    }

    /// Read the next complete response, pulling from the transport as needed.
    ///
    /// `chunk` is heap-allocated and lives outside the loop, not a stack array
    /// re-declared per iteration: a `[0u8; READ_CHUNK]` local held across the
    /// `read().await` below bakes all 16 KiB into this future's inline state —
    /// exactly the small-foreign-FFI-poll-stack hazard
    /// `native-async-execution.md` § The hazard describes, measured once this
    /// method is reached inline from `FfiMailImportClient::connect` (17 KB
    /// un-fixed, matching the doc's ~10.9 KB unsafe baseline). A `Vec<u8>`
    /// keeps only a small handle inline regardless of where it lives.
    pub(crate) async fn next_event(&mut self) -> Result<ServerEvent, ImapClientError> {
        let mut chunk = vec![0u8; READ_CHUNK];
        loop {
            // Parse under an immutable borrow, project to an owned event, and
            // only then drain. `consumed` and `event` outlive the borrow;
            // `Response<'_>` does not.
            enum Step {
                Parsed(usize, ServerEvent),
                NeedMore,
            }
            let step = match parse_status(&self.buf)? {
                StatusLine::Parsed(consumed, event) => Step::Parsed(consumed, event),
                StatusLine::Incomplete => Step::NeedMore,
                StatusLine::NotStatus => match parse_response(&self.buf) {
                    Ok((rest, response)) => {
                        let consumed = self.buf.len() - rest.len();
                        Step::Parsed(consumed, project(response))
                    }
                    Err(nom::Err::Incomplete(_)) => Step::NeedMore,
                    Err(e) => {
                        return Err(ImapClientError::Protocol(describe_parse_error(&e)));
                    }
                },
            };

            if let Step::Parsed(consumed, event) = step {
                self.buf.drain(..consumed);
                return Ok(event);
            }

            // A source server that opens a literal and never fills it must not
            // drive us OOM. Checked before each read, so we hold at most
            // `MAX_RESPONSE_BUFFER + READ_CHUNK` bytes before bailing out.
            if self.buf.len() >= MAX_RESPONSE_BUFFER {
                return Err(ImapClientError::ResponseTooLarge {
                    limit: MAX_RESPONSE_BUFFER,
                });
            }

            let n = self
                .transport
                .read(&mut chunk)
                .await
                .map_err(ImapClientError::transport)?;
            if n == 0 {
                return Err(ImapClientError::Eof);
            }
            self.buf.extend_from_slice(&chunk[..n]);
        }
    }

    /// Read events until the tagged completion for `tag` arrives, handing each
    /// untagged event to `on_event`.
    ///
    /// A non-`OK` completion becomes [`ImapClientError::Rejected`] carrying
    /// `command` — the bare verb, never the argument line.
    pub(crate) async fn run_to_completion(
        &mut self,
        tag: &str,
        command: &str,
        mut on_event: impl FnMut(ServerEvent) -> Result<(), ImapClientError>,
    ) -> Result<(), ImapClientError> {
        loop {
            match self.next_event().await? {
                ServerEvent::Done {
                    tag: done_tag,
                    status,
                    information,
                } => {
                    if done_tag != tag {
                        // Pipelined FETCH is the only place we have several
                        // tags outstanding, and it uses `next_event` directly.
                        return Err(ImapClientError::UnexpectedResponse(format!(
                            "{command}: completion for tag {done_tag}, expected {tag}"
                        )));
                    }
                    return match status {
                        Status::Ok => Ok(()),
                        other => Err(ImapClientError::Rejected {
                            command: command.to_string(),
                            status: format!("{other:?}").to_uppercase(),
                            text: information,
                        }),
                    };
                }
                // `* BYE` means the server is closing; nothing further arrives.
                ServerEvent::Data {
                    status: Status::Bye,
                    ..
                } => return Err(ImapClientError::Eof),
                other => on_event(other)?,
            }
        }
    }
}

impl<T> Connection<T> {
    /// The transport itself, for the one operation that mutates the pipe rather
    /// than writing to it: the in-place STARTTLS upgrade.
    pub(crate) fn transport_mut(&mut self) -> &mut T {
        &mut self.transport
    }

    /// Unparsed bytes still held from the *pre*-TLS stream.
    ///
    /// After a `STARTTLS` tagged `OK` this must be zero: anything left was
    /// written by whoever controlled the plaintext connection, and reading it
    /// back after the handshake would credit it to the authenticated server.
    /// See [`crate::imap_client::ImapClientError::StartTlsPlaintextInjection`].
    pub(crate) fn buffered_len(&self) -> usize {
        self.buf.len()
    }
}

#[cfg(test)]
impl<T> Connection<T> {
    /// Inspect the transport, so a test can assert on the exact bytes the
    /// client put on the wire (e.g. that a password never reached it).
    pub(crate) fn transport(&self) -> &T {
        &self.transport
    }
}

/// nom's `Error` carries the whole remaining input; rendering it verbatim would
/// splice message bodies — possibly credentials from a failed AUTHENTICATE —
/// into an error string that ends up in a UI and a log. Report the kind only.
fn describe_parse_error(e: &nom::Err<nom::error::Error<&[u8]>>) -> String {
    match e {
        nom::Err::Incomplete(_) => "truncated response".to_string(),
        nom::Err::Error(inner) | nom::Err::Failure(inner) => {
            format!(
                "{:?} with {} bytes unconsumed",
                inner.code,
                inner.input.len()
            )
        }
    }
}

/// What [`parse_status`] made of the front of the read buffer.
enum StatusLine {
    /// The buffer does not start with an untagged `STATUS` response.
    NotStatus,
    /// It does, and the rest of the line has not arrived yet.
    Incomplete,
    /// The whole response, `consumed` bytes long.
    Parsed(usize, ServerEvent),
}

/// An untagged `STATUS` response, read here rather than by
/// [`parse_response`]: the version of the parser this crate uses knows only
/// RFC 3501's status items and refuses a line carrying RFC 8438's `SIZE` as
/// not IMAP at all, which would end the session over the one item a
/// `STATUS=SIZE` server answers best. Strict where it reads — mailbox name
/// (atom, quoted string or literal), then a parenthesised list of
/// `item number` pairs, then CRLF — and tolerant of items it does not use.
fn parse_status(buf: &[u8]) -> Result<StatusLine, ImapClientError> {
    const PREFIX: &[u8] = b"* STATUS ";
    let bad = |why: &str| ImapClientError::Protocol(format!("STATUS: {why}"));
    let head = buf.len().min(PREFIX.len());
    if !buf[..head].eq_ignore_ascii_case(&PREFIX[..head]) {
        return Ok(StatusLine::NotStatus);
    }
    if buf.len() < PREFIX.len() {
        return Ok(StatusLine::Incomplete);
    }
    let mut i = PREFIX.len();
    // The mailbox name, which the estimate never reads: the caller asked
    // about one mailbox and the reply is about that one.
    match buf.get(i) {
        None => return Ok(StatusLine::Incomplete),
        Some(b'"') => {
            i += 1;
            loop {
                match buf.get(i) {
                    None => return Ok(StatusLine::Incomplete),
                    Some(b'\\') => i += 2,
                    Some(b'"') => {
                        i += 1;
                        break;
                    }
                    Some(b'\r' | b'\n') => return Err(bad("unterminated mailbox name")),
                    Some(_) => i += 1,
                }
            }
        }
        Some(b'{') => {
            let Some(close) = buf[i..].iter().position(|&b| b == b'}') else {
                return Ok(StatusLine::Incomplete);
            };
            let len: usize = std::str::from_utf8(&buf[i + 1..i + close])
                .ok()
                .and_then(|s| s.parse().ok())
                .filter(|&n| n <= MAX_RESPONSE_BUFFER)
                .ok_or_else(|| bad("bad literal length"))?;
            i += close + 1;
            match buf.get(i..i + 2) {
                None => return Ok(StatusLine::Incomplete),
                Some(b"\r\n") => i += 2 + len,
                Some(_) => return Err(bad("literal not followed by CRLF")),
            }
        }
        Some(_) => loop {
            match buf.get(i) {
                None => return Ok(StatusLine::Incomplete),
                Some(b' ') => break,
                Some(b'\r' | b'\n') => return Err(bad("no status list")),
                Some(_) => i += 1,
            }
        },
    }
    match buf.get(i..i + 2) {
        None => return Ok(StatusLine::Incomplete),
        Some(b" (") => i += 2,
        Some(_) => return Err(bad("no status list")),
    }
    let start = i;
    loop {
        match buf.get(i) {
            None => return Ok(StatusLine::Incomplete),
            Some(b')') => break,
            Some(b'\r' | b'\n') => return Err(bad("unterminated status list")),
            Some(_) => i += 1,
        }
    }
    let items = &buf[start..i];
    i += 1;
    match buf.get(i..i + 2) {
        None => return Ok(StatusLine::Incomplete),
        Some(b"\r\n") => i += 2,
        Some(_) => return Err(bad("trailing bytes")),
    }
    let items = std::str::from_utf8(items).map_err(|_| bad("non-ASCII status list"))?;
    let mut words = items.split_ascii_whitespace();
    let (mut messages, mut size) = (None, None);
    while let Some(name) = words.next() {
        let value: u64 = words
            .next()
            .and_then(|v| v.parse().ok())
            .ok_or_else(|| bad("status item without a number"))?;
        if name.eq_ignore_ascii_case("MESSAGES") {
            messages = Some(u32::try_from(value).map_err(|_| bad("MESSAGES out of range"))?);
        } else if name.eq_ignore_ascii_case("SIZE") {
            size = Some(value);
        }
    }
    Ok(StatusLine::Parsed(
        i,
        ServerEvent::Status { messages, size },
    ))
}

/// One `LIST` name attribute's special-use role, if it is one. Gmail's
/// `\Important` is no RFC 6154 attribute, so it arrives as an extension —
/// matched case-insensitively, with or without its backslash.
fn special_use(attr: &NameAttribute<'_>) -> Option<SpecialUse> {
    Some(match attr {
        NameAttribute::All => SpecialUse::All,
        NameAttribute::Archive => SpecialUse::Archive,
        NameAttribute::Drafts => SpecialUse::Drafts,
        NameAttribute::Flagged => SpecialUse::Flagged,
        NameAttribute::Junk => SpecialUse::Junk,
        NameAttribute::Sent => SpecialUse::Sent,
        NameAttribute::Trash => SpecialUse::Trash,
        NameAttribute::Extension(ext)
            if ext
                .trim_start_matches('\\')
                .eq_ignore_ascii_case("important") =>
        {
            SpecialUse::Important
        }
        _ => return None,
    })
}

/// Project a borrowed `Response` into an owned [`ServerEvent`].
fn project(response: Response<'_>) -> ServerEvent {
    match response {
        Response::Capabilities(caps) => ServerEvent::Capabilities(
            caps.into_iter()
                .map(|c| match c {
                    Capability::Imap4rev1 => "IMAP4REV1".to_string(),
                    Capability::Auth(a) => format!("AUTH={}", a.to_uppercase()),
                    Capability::Atom(a) => a.to_uppercase(),
                })
                .collect(),
        ),
        Response::MailboxData(MailboxDatum::List {
            name_attributes,
            name,
            ..
        }) => ServerEvent::List {
            name: name.into_owned(),
            selectable: !name_attributes
                .iter()
                .any(|a| matches!(a, NameAttribute::NoSelect)),
            special_use: name_attributes.iter().find_map(special_use),
        },
        Response::MailboxData(MailboxDatum::Exists(n)) => ServerEvent::Exists(n),
        Response::MailboxData(MailboxDatum::Search(uids)) => ServerEvent::Search(uids),
        Response::Fetch(_seq, attrs) => ServerEvent::Fetch(project_attrs(attrs)),
        Response::Data { status, code, .. } => ServerEvent::Data {
            status,
            uid_validity: match code {
                Some(ResponseCode::UidValidity(v)) => Some(v),
                _ => None,
            },
        },
        Response::Done {
            tag,
            status,
            information,
            ..
        } => ServerEvent::Done {
            tag: tag.0,
            status,
            information: information.map(|i| i.into_owned()).unwrap_or_default(),
        },
        Response::Continue { .. } => ServerEvent::Continue,
        _ => ServerEvent::Ignored,
    }
}

fn project_attrs(attrs: Vec<AttributeValue<'_>>) -> FetchAttrs {
    let mut out = FetchAttrs::default();
    for attr in attrs {
        match attr {
            AttributeValue::Uid(u) => out.uid = Some(u),
            AttributeValue::Rfc822Size(s) => out.rfc822_size = Some(s),
            AttributeValue::InternalDate(d) => out.internal_date = Some(d.into_owned()),
            AttributeValue::Flags(f) => {
                out.flags = f.into_iter().map(|x| x.into_owned()).collect();
            }
            // We ask for `BODY.PEEK[]`, which the server answers as `BODY[]`
            // with no section path. A sectioned body means the server answered
            // a question we did not ask; ignore it rather than mistake it for
            // the full message.
            AttributeValue::BodySection {
                section: None,
                data: Some(d),
                ..
            } => out.body = Some(d.into_owned()),
            // Some servers answer a bare `RFC822` for `BODY.PEEK[]`.
            AttributeValue::Rfc822(Some(d)) if out.body.is_none() => {
                out.body = Some(d.into_owned());
            }
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::imap_client::transport::testing::{ScriptedTransport, block_on};

    #[test]
    fn a_response_split_across_reads_is_reassembled() {
        // The whole point of sans-io: the literal straddles three transport
        // reads. A naive line-oriented reader would corrupt the body here.
        let t = ScriptedTransport::new([
            &b"* 1 FETCH (UID 42 BODY[] {21}\r\nSub"[..],
            &b"ject: hi\r\n\r\nbo"[..],
            &b"dy\r\n)\r\n"[..],
        ]);
        let mut c = Connection::new(t);
        let ev = block_on(c.next_event()).expect("reassembles");
        match ev {
            ServerEvent::Fetch(a) => {
                assert_eq!(a.uid, Some(42));
                assert_eq!(a.body.as_deref(), Some(&b"Subject: hi\r\n\r\nbody\r\n"[..]));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_literal_containing_crlf_and_a_close_paren_is_not_truncated() {
        // A body whose bytes look like IMAP framing. The literal's declared
        // length is the only thing that may end it.
        let body = b")\r\n* 2 FETCH (UID 99 BODY[] {3}\r\nno)\r\n";
        let mut wire = format!("* 1 FETCH (UID 7 BODY[] {{{}}}\r\n", body.len()).into_bytes();
        wire.extend_from_slice(body);
        wire.extend_from_slice(b")\r\n");
        let mut c = Connection::new(ScriptedTransport::new([wire]));
        match block_on(c.next_event()).expect("parses") {
            ServerEvent::Fetch(a) => {
                assert_eq!(a.uid, Some(7));
                assert_eq!(a.body.as_deref(), Some(&body[..]));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn two_responses_in_one_read_are_yielded_one_at_a_time() {
        let t = ScriptedTransport::new([&b"* 3 EXISTS\r\nA0001 OK done\r\n"[..]]);
        let mut c = Connection::new(t);
        assert_eq!(block_on(c.next_event()).unwrap(), ServerEvent::Exists(3));
        match block_on(c.next_event()).unwrap() {
            ServerEvent::Done { tag, status, .. } => {
                assert_eq!(tag, "A0001");
                assert_eq!(status, Status::Ok);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn eof_mid_response_is_reported_not_silently_truncated() {
        let mut c = Connection::new(ScriptedTransport::new([
            &b"* 1 FETCH (UID 42 BODY[] {99}\r\nshort"[..],
        ]));
        assert!(matches!(
            block_on(c.next_event()),
            Err(ImapClientError::Eof)
        ));
    }

    #[test]
    fn garbage_is_a_protocol_error_and_never_echoes_the_input() {
        let mut c = Connection::new(ScriptedTransport::new([&b"this is not imap\r\n"[..]]));
        let err = block_on(c.next_event()).expect_err("must reject");
        let rendered = err.to_string();
        assert!(matches!(err, ImapClientError::Protocol(_)), "{err:?}");
        assert!(
            !rendered.contains("this is not imap"),
            "parse errors must not splice the input into the message: {rendered}"
        );
    }

    #[test]
    fn tags_are_monotone_and_zero_padded() {
        let mut c = Connection::new(ScriptedTransport::new([&b""[..]]));
        assert_eq!(c.next_tag(), "A0001");
        assert_eq!(c.next_tag(), "A0002");
    }

    #[test]
    fn a_rejected_command_reports_the_verb_but_never_the_arguments() {
        // `LOGIN alice hunter2` must not leak `hunter2` into the error.
        let mut c = Connection::new(ScriptedTransport::new([
            &b"A0001 NO [AUTHENTICATIONFAILED] Invalid credentials\r\n"[..],
        ]));
        let err = block_on(c.run_to_completion("A0001", "LOGIN", |_| Ok(())))
            .expect_err("NO must surface");
        match &err {
            ImapClientError::Rejected {
                command,
                status,
                text,
            } => {
                assert_eq!(command, "LOGIN");
                assert_eq!(status, "NO");
                assert!(text.contains("Invalid credentials"), "{text}");
            }
            other => panic!("{other:?}"),
        }
        assert!(!err.to_string().contains("hunter2"));
    }

    #[test]
    fn an_untagged_bye_ends_the_session() {
        let mut c = Connection::new(ScriptedTransport::new([
            &b"* BYE Autologout; idle for too long\r\n"[..],
        ]));
        assert!(matches!(
            block_on(c.run_to_completion("A0001", "SELECT", |_| Ok(()))),
            Err(ImapClientError::Eof)
        ));
    }

    #[test]
    fn capabilities_are_uppercased_for_case_insensitive_lookup() {
        let mut c = Connection::new(ScriptedTransport::new([
            &b"* CAPABILITY IMAP4rev1 uidplus AUTH=xoauth2\r\n"[..],
        ]));
        match block_on(c.next_event()).unwrap() {
            ServerEvent::Capabilities(caps) => {
                assert!(caps.contains(&"IMAP4REV1".to_string()), "{caps:?}");
                assert!(caps.contains(&"UIDPLUS".to_string()), "{caps:?}");
                assert!(caps.contains(&"AUTH=XOAUTH2".to_string()), "{caps:?}");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn uidvalidity_rides_the_untagged_ok_response_code() {
        let mut c = Connection::new(ScriptedTransport::new([
            &b"* OK [UIDVALIDITY 3857529045] UIDs valid\r\n"[..],
        ]));
        assert_eq!(
            block_on(c.next_event()).unwrap(),
            ServerEvent::Data {
                status: Status::Ok,
                uid_validity: Some(3_857_529_045),
            }
        );
    }

    #[test]
    fn noselect_mailboxes_are_flagged_unselectable() {
        let mut c = Connection::new(ScriptedTransport::new([
            &b"* LIST (\\Noselect \\HasChildren) \"/\" \"[Gmail]\"\r\n* LIST () \"/\" \"INBOX\"\r\n"[..],
        ]));
        assert_eq!(
            block_on(c.next_event()).unwrap(),
            ServerEvent::List {
                name: "[Gmail]".into(),
                selectable: false,
                special_use: None,
            }
        );
        assert_eq!(
            block_on(c.next_event()).unwrap(),
            ServerEvent::List {
                name: "INBOX".into(),
                selectable: true,
                special_use: None,
            }
        );
    }

    /// `mailbox-migration.md` § Wizard steps step 3: the discard and
    /// duplicate mailboxes are recognised by the role the source advertises,
    /// whatever the name — a localized Gmail lists none of the English ones.
    #[test]
    fn special_use_attributes_are_projected_whatever_the_name() {
        let script = concat!(
            "* LIST (\\HasNoChildren \\Trash) \"/\" \"[Gmail]/Papierkorb\"\r\n",
            "* LIST (\\HasNoChildren \\Junk) \"/\" \"[Gmail]/Spam\"\r\n",
            "* LIST (\\All \\HasNoChildren) \"/\" \"[Gmail]/Alle Nachrichten\"\r\n",
            "* LIST (\\Flagged \\HasNoChildren) \"/\" \"[Gmail]/Markiert\"\r\n",
            "* LIST (\\HasNoChildren \\Important) \"/\" \"[Gmail]/Wichtig\"\r\n",
            "* LIST (\\HasNoChildren \\Sent) \"/\" \"[Gmail]/Gesendet\"\r\n",
        );
        let mut c = Connection::new(ScriptedTransport::new([script.as_bytes()]));
        let mut got = Vec::new();
        for _ in 0..6 {
            match block_on(c.next_event()).unwrap() {
                ServerEvent::List { special_use, .. } => got.push(special_use),
                other => panic!("expected LIST, got {other:?}"),
            }
        }
        assert_eq!(
            got,
            [
                Some(SpecialUse::Trash),
                Some(SpecialUse::Junk),
                Some(SpecialUse::All),
                Some(SpecialUse::Flagged),
                Some(SpecialUse::Important),
                Some(SpecialUse::Sent),
            ]
        );
    }

    fn status_of(wire: &[u8]) -> Result<ServerEvent, ImapClientError> {
        let mut c = Connection::new(ScriptedTransport::new([wire.to_vec()]));
        block_on(c.next_event())
    }

    #[test]
    fn a_status_reply_with_rfc8438_size_is_read_not_refused() {
        // The line the response parser would reject as not IMAP.
        assert_eq!(
            status_of(b"* STATUS INBOX (MESSAGES 3 SIZE 12345)\r\n").unwrap(),
            ServerEvent::Status {
                messages: Some(3),
                size: Some(12345)
            }
        );
    }

    #[test]
    fn a_status_reply_reads_any_mailbox_form_and_skips_unused_items() {
        for wire in [
            &b"* STATUS \"Sent Items\" (UIDNEXT 9 MESSAGES 3)\r\n"[..],
            &b"* status \"a \\\"q\\\" box\" (MESSAGES 3 UNSEEN 1)\r\n"[..],
            &b"* STATUS {5}\r\nIN)BX (MESSAGES 3)\r\n"[..],
        ] {
            assert_eq!(
                status_of(wire).unwrap(),
                ServerEvent::Status {
                    messages: Some(3),
                    size: None
                },
                "{}",
                String::from_utf8_lossy(wire)
            );
        }
    }

    #[test]
    fn a_status_reply_split_across_reads_is_reassembled() {
        let t = ScriptedTransport::new([
            &b"* STA"[..],
            &b"TUS {5}\r\nIN"[..],
            &b"BOX (MESSAGES 4 SI"[..],
            &b"ZE 99)\r\nA0001 OK done\r\n"[..],
        ]);
        let mut c = Connection::new(t);
        assert_eq!(
            block_on(c.next_event()).unwrap(),
            ServerEvent::Status {
                messages: Some(4),
                size: Some(99)
            }
        );
        assert!(matches!(
            block_on(c.next_event()).unwrap(),
            ServerEvent::Done { .. }
        ));
    }

    #[test]
    fn a_malformed_status_reply_is_a_protocol_error() {
        for wire in [
            &b"* STATUS INBOX (MESSAGES)\r\n"[..],
            &b"* STATUS INBOX (MESSAGES many)\r\n"[..],
            &b"* STATUS INBOX\r\n"[..],
            &b"* STATUS INBOX (MESSAGES 1) x\r\n"[..],
        ] {
            assert!(
                matches!(status_of(wire), Err(ImapClientError::Protocol(_))),
                "{}",
                String::from_utf8_lossy(wire)
            );
        }
    }
}
