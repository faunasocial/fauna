//! The IMAP session state machine: connect → authenticate → enumerate → fetch.
//!
//! Every command here is **read-only against the source server**. Import must
//! not mutate the mailbox it is copying: we `EXAMINE` rather than `SELECT`
//! (RFC 3501 §6.3.2 — read-only, so `\Recent` is not cleared) and fetch with
//! `BODY.PEEK[]` rather than `BODY[]` (so `\Seen` is not set). A user who
//! imports their Gmail must find Gmail exactly as they left it.

use std::collections::{HashMap, HashSet, VecDeque};

use super::connection::{Connection, ServerEvent};
use super::internaldate::parse_internal_date;
use super::throttle::{FetchThrottle, ThrottleDecision};
use super::transport::{ImapClock, ImapTransport};
use super::{FetchedMessage, ImapClientError};
use base64::Engine;
use imap_proto::types::Status;

/// One mailbox on the source server, from `LIST`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceMailbox {
    pub name: String,
    /// `false` when the source flagged it `\Noselect` (Gmail's `[Gmail]`
    /// container, for instance). Unselectable mailboxes hold no messages and
    /// must be skipped, not treated as an empty mailbox.
    pub selectable: bool,
}

/// The state `EXAMINE` reports for the selected mailbox.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MailboxStatus {
    /// Pins the UID namespace. A change across a resume means the source
    /// mailbox was rebuilt and every stored cursor is meaningless.
    pub uid_validity: u32,
    /// Message count, for the wizard's `total_count` estimate.
    pub exists: u32,
}

/// One message the source has, discovered by enumeration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UidEntry {
    pub uid: u32,
    /// `RFC822.SIZE`, when the source reported it. § Per-message flow step 1
    /// says to compare it *"when supported by the source"* — a `None` here
    /// means we cannot know the size before fetching the body.
    pub size: Option<u32>,
}

impl UidEntry {
    /// § Scope & limits: skip messages larger than the effective per-message
    /// ceiling **before** spending bandwidth on the body.
    ///
    /// A message whose size the source did not report is never oversize: we
    /// would rather import a large message than silently drop one on a guess.
    pub fn is_oversize(&self, max_bytes: u64) -> bool {
        self.size.is_some_and(|s| u64::from(s) > max_bytes)
    }
}

/// The result of asking the source for one UID.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FetchOutcome {
    Fetched(Box<FetchedMessage>),
    /// § Failure handling: a per-message source-side failure feeds
    /// `errored_count` and the end-of-import review log. It does **not** end
    /// the session — one unreadable message must not strand the other 49,999.
    Failed {
        uid: u32,
        reason: String,
    },
}

struct Selected {
    mailbox: String,
    uid_validity: u32,
}

/// A live, authenticated-or-not session against one source IMAP server.
///
/// Owns the § Throttling budget, because the cap is per *source server* and
/// this type is the one-per-server object.
pub struct ImapSession<T> {
    conn: Connection<T>,
    capabilities: Vec<String>,
    selected: Option<Selected>,
    throttle: FetchThrottle,
}

impl<T: ImapTransport> ImapSession<T> {
    /// Read the server greeting and probe capabilities.
    ///
    /// `transport` must already be connected **and TLS-wrapped**: this module
    /// never sees plaintext-on-the-wire. A `* BYE` greeting (the server
    /// refusing us outright) surfaces as [`ImapClientError::Eof`].
    pub async fn connect(transport: T) -> Result<Self, ImapClientError> {
        let mut session = Self {
            conn: Connection::new(transport),
            capabilities: Vec::new(),
            selected: None,
            throttle: FetchThrottle::ratified(),
        };

        match session.conn.next_event().await? {
            // `* OK …` = not authenticated; `* PREAUTH …` = already logged in.
            ServerEvent::Data {
                status: Status::Ok | Status::PreAuth,
                ..
            } => {}
            ServerEvent::Data {
                status: Status::Bye,
                ..
            } => return Err(ImapClientError::Eof),
            other => {
                return Err(ImapClientError::UnexpectedResponse(format!(
                    "greeting: {other:?}"
                )));
            }
        }

        session.refresh_capabilities().await?;
        Ok(session)
    }

    /// Connect over a **plaintext** pipe and negotiate RFC 3501 §6.2.1
    /// `STARTTLS` before anything else — the [`crate::imap_client::TlsMode::StartTls`]
    /// half of § Wizard steps' ratified "TLS mode (implicit / STARTTLS)".
    ///
    /// `transport` must be freshly connected and *not* TLS-wrapped. This drives
    /// the command exchange, then asks the transport to
    /// [`ImapTransport::upgrade_tls`] in place. On return the session is exactly
    /// what [`Self::connect`] leaves behind — TLS established, capabilities
    /// read — so every method above this one is mode-agnostic.
    ///
    /// Three refusals, all of them plaintext-downgrade hazards:
    ///
    /// - **`PREAUTH` greeting** — the server declaring us already authenticated
    ///   *before* TLS. Whatever authenticated us crossed the wire in the clear.
    /// - **Bytes buffered across the handshake** — see
    ///   [`ImapClientError::StartTlsPlaintextInjection`].
    /// - **A rejected `STARTTLS`** — surfaces as [`ImapClientError::Rejected`].
    ///   There is deliberately no plaintext fallback: a MITM can always
    ///   manufacture a `NO`, and falling back would hand it the password.
    ///
    /// Pre-TLS capabilities are never consulted — stripping the `STARTTLS`
    /// advertisement is the classic downgrade, so trusting the pre-TLS
    /// `CAPABILITY` to decide whether to *attempt* TLS defeats the point. We
    /// issue the command unconditionally and read capabilities *inside* TLS
    /// afterwards, as RFC 3501 §6.2.1 requires.
    pub async fn connect_starttls(transport: T) -> Result<Self, ImapClientError> {
        let mut session = Self {
            conn: Connection::new(transport),
            capabilities: Vec::new(),
            selected: None,
            throttle: FetchThrottle::ratified(),
        };

        match session.conn.next_event().await? {
            ServerEvent::Data {
                status: Status::Ok, ..
            } => {}
            ServerEvent::Data {
                status: Status::PreAuth,
                ..
            } => {
                return Err(ImapClientError::Protocol(
                    "the source pre-authenticated a plaintext connection; \
                     refusing to continue without TLS"
                        .into(),
                ));
            }
            ServerEvent::Data {
                status: Status::Bye,
                ..
            } => return Err(ImapClientError::Eof),
            other => {
                return Err(ImapClientError::UnexpectedResponse(format!(
                    "greeting: {other:?}"
                )));
            }
        }

        let tag = session.conn.next_tag();
        session.conn.send_line(&format!("{tag} STARTTLS")).await?;
        session
            .conn
            .run_to_completion(&tag, "STARTTLS", |_| Ok(()))
            .await?;

        // Everything the server sent before the handshake belongs to the
        // *plaintext* stream. A well-behaved server sends nothing between the
        // tagged OK and the handshake, so a non-empty buffer is not a race — it
        // is someone appending commands to the cleartext connection for us to
        // read back as though the authenticated server had sent them inside TLS.
        if session.conn.buffered_len() != 0 {
            return Err(ImapClientError::StartTlsPlaintextInjection);
        }

        session.conn.transport_mut().upgrade_tls().await?;

        // First capabilities we are entitled to believe: the pre-TLS ones were
        // unauthenticated and RFC 3501 §6.2.1 requires discarding them.
        session.refresh_capabilities().await?;
        Ok(session)
    }

    /// Capabilities as uppercase atoms (`IMAP4REV1`, `UIDPLUS`, `AUTH=XOAUTH2`).
    pub fn capabilities(&self) -> &[String] {
        &self.capabilities
    }

    pub fn has_capability(&self, cap: &str) -> bool {
        let want = cap.to_ascii_uppercase();
        self.capabilities.contains(&want)
    }

    async fn refresh_capabilities(&mut self) -> Result<(), ImapClientError> {
        let tag = self.conn.next_tag();
        self.conn.send_line(&format!("{tag} CAPABILITY")).await?;
        let mut caps = Vec::new();
        self.conn
            .run_to_completion(&tag, "CAPABILITY", |ev| {
                if let ServerEvent::Capabilities(c) = ev {
                    caps = c;
                }
                Ok(())
            })
            .await?;
        self.capabilities = caps;
        Ok(())
    }

    /// `LOGIN` with a username and password.
    ///
    /// Refuses when the server advertises `LOGINDISABLED` (RFC 3501 §6.2.3:
    /// the client MUST NOT send `LOGIN` then) — that capability means the
    /// server will reject cleartext credentials, and sending them anyway puts
    /// the user's password on the wire for nothing.
    pub async fn login(&mut self, username: &str, password: &str) -> Result<(), ImapClientError> {
        if self.has_capability("LOGINDISABLED") {
            return Err(ImapClientError::Rejected {
                command: "LOGIN".into(),
                status: "NO".into(),
                text: "the source server advertises LOGINDISABLED".into(),
            });
        }
        let tag = self.conn.next_tag();
        let line = format!(
            "{tag} LOGIN {} {}",
            quote_astring(username, "username")?,
            quote_astring(password, "password")?
        );
        self.conn.send_line(&line).await?;
        self.conn
            .run_to_completion(&tag, "LOGIN", |_| Ok(()))
            .await?;
        // A successful LOGIN invalidates the pre-auth capability list (RFC
        // 3501 §6.2: the server MAY advertise different capabilities once
        // authenticated), so re-probe rather than trust the greeting's set.
        self.refresh_capabilities().await
    }

    /// SASL `XOAUTH2` — the OAuth path § Credential handling names for
    /// Microsoft Graph, and what Gmail wants too.
    ///
    /// The bearer token lives in client memory only and never reaches nest.
    pub async fn authenticate_xoauth2(
        &mut self,
        username: &str,
        access_token: &str,
    ) -> Result<(), ImapClientError> {
        if username.contains(['\r', '\n', '\x01']) || access_token.contains(['\r', '\n', '\x01']) {
            return Err(ImapClientError::Protocol(
                "XOAUTH2 credentials must not contain CR, LF, or SASL separators".into(),
            ));
        }
        let initial = base64::engine::general_purpose::STANDARD.encode(format!(
            "user={username}\x01auth=Bearer {access_token}\x01\x01"
        ));

        let tag = self.conn.next_tag();
        if self.has_capability("SASL-IR") {
            self.conn
                .send_line(&format!("{tag} AUTHENTICATE XOAUTH2 {initial}"))
                .await?;
        } else {
            self.conn
                .send_line(&format!("{tag} AUTHENTICATE XOAUTH2"))
                .await?;
            // The server answers `+` (possibly with a challenge we ignore),
            // then we send the initial response on its own line.
            match self.conn.next_event().await? {
                ServerEvent::Continue => {}
                ServerEvent::Done {
                    status,
                    information,
                    ..
                } => {
                    return Err(ImapClientError::Rejected {
                        command: "AUTHENTICATE".into(),
                        status: format!("{status:?}").to_uppercase(),
                        text: information,
                    });
                }
                other => {
                    return Err(ImapClientError::UnexpectedResponse(format!(
                        "AUTHENTICATE: {other:?}"
                    )));
                }
            }
            self.conn.send_line(&initial).await?;
        }

        // An XOAUTH2 failure answers `+ <base64 error json>` and only then a
        // tagged NO; the client must send an empty line to unwedge the SASL
        // exchange before the completion arrives.
        loop {
            match self.conn.next_event().await? {
                ServerEvent::Continue => self.conn.send_raw(b"\r\n").await?,
                ServerEvent::Done {
                    tag: done_tag,
                    status,
                    information,
                } => {
                    if done_tag != tag {
                        return Err(ImapClientError::UnexpectedResponse(format!(
                            "AUTHENTICATE: completion for tag {done_tag}, expected {tag}"
                        )));
                    }
                    if status != Status::Ok {
                        return Err(ImapClientError::Rejected {
                            command: "AUTHENTICATE".into(),
                            status: format!("{status:?}").to_uppercase(),
                            text: information,
                        });
                    }
                    break;
                }
                ServerEvent::Capabilities(c) => self.capabilities = c,
                _ => {}
            }
        }
        self.refresh_capabilities().await
    }

    /// `LIST "" "*"` — every mailbox the account can see.
    pub async fn list_mailboxes(&mut self) -> Result<Vec<SourceMailbox>, ImapClientError> {
        let tag = self.conn.next_tag();
        self.conn
            .send_line(&format!("{tag} LIST \"\" \"*\""))
            .await?;
        let mut out = Vec::new();
        self.conn
            .run_to_completion(&tag, "LIST", |ev| {
                if let ServerEvent::List { name, selectable } = ev {
                    out.push(SourceMailbox { name, selectable });
                }
                Ok(())
            })
            .await?;
        Ok(out)
    }

    /// `EXAMINE` — select `mailbox` **read-only**.
    ///
    /// When `expect_uid_validity` is `Some`, a mismatch is
    /// [`ImapClientError::UidValidityChanged`]: the resume cursor refers to a
    /// UID namespace that no longer exists, and importing against the new one
    /// would duplicate or skip arbitrary messages (§ Progress lives nest-side).
    pub async fn examine(
        &mut self,
        mailbox: &str,
        expect_uid_validity: Option<u32>,
    ) -> Result<MailboxStatus, ImapClientError> {
        let tag = self.conn.next_tag();
        let line = format!("{tag} EXAMINE {}", quote_astring(mailbox, "mailbox")?);
        self.conn.send_line(&line).await?;

        let mut uid_validity = None;
        let mut exists = 0;
        self.conn
            .run_to_completion(&tag, "EXAMINE", |ev| {
                match ev {
                    ServerEvent::Data {
                        uid_validity: Some(v),
                        ..
                    } => uid_validity = Some(v),
                    ServerEvent::Exists(n) => exists = n,
                    _ => {}
                }
                Ok(())
            })
            .await?;

        let uid_validity = uid_validity
            .ok_or_else(|| ImapClientError::UnexpectedResponse("EXAMINE: no UIDVALIDITY".into()))?;
        if let Some(expected) = expect_uid_validity
            && expected != uid_validity
        {
            return Err(ImapClientError::UidValidityChanged {
                mailbox: mailbox.to_string(),
                expected,
                actual: uid_validity,
            });
        }

        self.selected = Some(Selected {
            mailbox: mailbox.to_string(),
            uid_validity,
        });
        Ok(MailboxStatus {
            uid_validity,
            exists,
        })
    }

    /// `UID FETCH <from_uid>:* (UID RFC822.SIZE)` — the resume-aware
    /// enumeration of § Resume protocol step 3.
    ///
    /// **RFC 3501 §6.4.8 trap:** a `<n>:*` UID range "always includes the UID
    /// of the last message in the mailbox, even if `n` is higher than any
    /// assigned UID value". So resuming past the final message re-yields that
    /// final message. Entries below `from_uid` are therefore filtered out
    /// here — without this, every resume re-imports the last message of every
    /// mailbox (dedup would mask it, and a `skip_dedup` import would not).
    pub async fn enumerate_uids(
        &mut self,
        from_uid: u32,
    ) -> Result<Vec<UidEntry>, ImapClientError> {
        self.require_selected("UID FETCH")?;
        let tag = self.conn.next_tag();
        self.conn
            .send_line(&format!("{tag} UID FETCH {from_uid}:* (UID RFC822.SIZE)"))
            .await?;

        let mut out = Vec::new();
        self.conn
            .run_to_completion(&tag, "UID FETCH", |ev| {
                if let ServerEvent::Fetch(a) = ev
                    && let Some(uid) = a.uid
                    && uid >= from_uid
                {
                    out.push(UidEntry {
                        uid,
                        size: a.rfc822_size,
                    });
                }
                Ok(())
            })
            .await?;

        out.sort_unstable_by_key(|e| e.uid);
        out.dedup_by_key(|e| e.uid);
        Ok(out)
    }

    /// Fetch `uids` from the selected mailbox under § Throttling's caps,
    /// handing each outcome to `on_outcome` as it lands.
    ///
    /// FETCH is **pipelined**: up to 4 commands are outstanding at once (the
    /// concurrency cap), which is what "at most 4 concurrent FETCH operations"
    /// means over one IMAP session. Correlation is by tag — each untagged
    /// `FETCH` response carries the `UID` we asked for, and RFC 3501 §7
    /// guarantees a command's untagged data precedes its tagged completion.
    pub async fn fetch_messages<C: ImapClock>(
        &mut self,
        uids: &[u32],
        clock: &C,
        mut on_outcome: impl FnMut(FetchOutcome),
    ) -> Result<(), ImapClientError> {
        let (mailbox, uid_validity) = {
            let s = self.require_selected("UID FETCH")?;
            (s.mailbox.clone(), s.uid_validity)
        };

        let mut pending: VecDeque<u32> = uids.iter().copied().collect();
        let mut in_flight: HashMap<String, u32> = HashMap::new();
        let mut delivered: HashSet<u32> = HashSet::new();

        while !pending.is_empty() || !in_flight.is_empty() {
            // Issue as many FETCHes as both caps allow before reading.
            while let Some(&uid) = pending.front() {
                // One clock read per decision: polling at `t` and recording the
                // start at `t + ε` would let the rolling window drift later than
                // the decision that admitted it.
                let now = clock.now_ms();
                match self.throttle.poll(now) {
                    ThrottleDecision::Go => {
                        pending.pop_front();
                        let tag = self.conn.next_tag();
                        self.conn
                            .send_line(&format!(
                                "{tag} UID FETCH {uid} (UID FLAGS INTERNALDATE BODY.PEEK[])"
                            ))
                            .await?;
                        self.throttle.on_start(now);
                        in_flight.insert(tag, uid);
                    }
                    // Reaping a response makes progress; sleeping does not.
                    ThrottleDecision::AwaitInFlight => break,
                    ThrottleDecision::SleepMs(ms) => {
                        if in_flight.is_empty() {
                            clock.sleep_ms(ms).await;
                        } else {
                            // Drain what is already outstanding first: those
                            // bytes are on their way regardless.
                            break;
                        }
                    }
                }
            }

            if in_flight.is_empty() {
                continue; // Rate-capped with nothing outstanding: loop to sleep.
            }

            match self.conn.next_event().await? {
                ServerEvent::Fetch(a) => {
                    // Unsolicited FETCH (a flag update) has no body; it is not
                    // an answer to any of our commands.
                    let Some(body) = a.body else { continue };
                    let Some(uid) = a.uid else {
                        return Err(ImapClientError::UnexpectedResponse(
                            "UID FETCH: body with no UID".into(),
                        ));
                    };
                    let internal_date_epoch = match a.internal_date.as_deref() {
                        Some(d) => parse_internal_date(d)?,
                        None => {
                            return Err(ImapClientError::UnexpectedResponse(
                                "UID FETCH: body with no INTERNALDATE".into(),
                            ));
                        }
                    };
                    delivered.insert(uid);
                    on_outcome(FetchOutcome::Fetched(Box::new(FetchedMessage {
                        mailbox: mailbox.clone(),
                        uid,
                        uid_validity,
                        // `\Recent` is a per-session artifact of the *source*
                        // and nest's `validate_item` rejects the whole message
                        // over it. Drop it here, not at the wire.
                        flags: a
                            .flags
                            .into_iter()
                            .filter(|f| !f.eq_ignore_ascii_case("\\Recent"))
                            .collect(),
                        internal_date_epoch,
                        body,
                    })));
                }
                ServerEvent::Done {
                    tag,
                    status,
                    information,
                    ..
                } => {
                    let Some(uid) = in_flight.remove(&tag) else {
                        return Err(ImapClientError::UnexpectedResponse(format!(
                            "UID FETCH: completion for unknown tag {tag}"
                        )));
                    };
                    self.throttle.on_complete();
                    if status != Status::Ok {
                        on_outcome(FetchOutcome::Failed {
                            uid,
                            reason: format!("source rejected FETCH: {information}"),
                        });
                    } else if !delivered.contains(&uid) {
                        // OK, but the server sent no body: the message was
                        // expunged between enumeration and fetch. Common on a
                        // live mailbox; per-message error, not session-fatal.
                        on_outcome(FetchOutcome::Failed {
                            uid,
                            reason: "source returned no body (message expunged?)".into(),
                        });
                    }
                }
                ServerEvent::Data {
                    status: Status::Bye,
                    ..
                } => return Err(ImapClientError::Eof),
                _ => {}
            }
        }
        Ok(())
    }

    /// `LOGOUT` — ends the session and, per § Credential handling, the
    /// lifetime of the source credentials.
    pub async fn logout(&mut self) -> Result<(), ImapClientError> {
        let tag = self.conn.next_tag();
        self.conn.send_line(&format!("{tag} LOGOUT")).await?;
        // The server answers `* BYE` then a tagged OK. `run_to_completion`
        // maps `* BYE` to `Eof`, which is the expected, successful end here.
        match self
            .conn
            .run_to_completion(&tag, "LOGOUT", |_| Ok(()))
            .await
        {
            Ok(()) | Err(ImapClientError::Eof) => Ok(()),
            Err(e) => Err(e),
        }
    }

    fn require_selected(&self, command: &str) -> Result<&Selected, ImapClientError> {
        self.selected
            .as_ref()
            .ok_or_else(|| ImapClientError::UnexpectedResponse(format!("{command} before EXAMINE")))
    }
}

/// Render `s` as an IMAP `quoted` string.
///
/// **This is the command-injection boundary.** A password or mailbox name
/// carrying a bare CRLF would otherwise close the current command line and let
/// the remainder be parsed as a fresh IMAP command — from a value the user
/// pasted out of a password manager, or a mailbox name the *source server*
/// chose. RFC 3501's `QUOTED-CHAR` excludes CR and LF precisely because they
/// cannot be escaped, so a value containing them has no valid encoding and is
/// rejected rather than mangled. `"` and `\` are escaped per §4.3.
fn quote_astring(s: &str, what: &str) -> Result<String, ImapClientError> {
    if s.contains(['\r', '\n', '\0']) {
        return Err(ImapClientError::Protocol(format!(
            "{what} must not contain CR, LF, or NUL"
        )));
    }
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        if c == '"' || c == '\\' {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('"');
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::imap_client::transport::testing::{FakeClock, ScriptedTransport, block_on};

    impl ImapSession<ScriptedTransport> {
        /// Every byte this client wrote to the source server.
        fn conn_sent(&self) -> String {
            self.conn.transport().sent()
        }
    }

    const GREETING: &[u8] = b"* OK [CAPABILITY IMAP4rev1] ready\r\n";
    const CAPS: &[u8] = b"* CAPABILITY IMAP4rev1 UIDPLUS\r\nA0001 OK done\r\n";

    /// A FETCH response for `uid`, with a body and an internaldate.
    fn fetch_response(tag: &str, uid: u32, body: &str) -> Vec<u8> {
        let mut v = format!(
            "* {uid} FETCH (UID {uid} FLAGS (\\Seen) INTERNALDATE \"17-Jul-1996 02:44:25 -0700\" BODY[] {{{}}}\r\n",
            body.len()
        )
        .into_bytes();
        v.extend_from_slice(body.as_bytes());
        v.extend_from_slice(format!(")\r\n{tag} OK FETCH completed\r\n").as_bytes());
        v
    }

    fn connected(extra: Vec<Vec<u8>>) -> ImapSession<ScriptedTransport> {
        let mut chunks = vec![GREETING.to_vec(), CAPS.to_vec()];
        chunks.extend(extra);
        block_on(ImapSession::connect(ScriptedTransport::new(chunks))).expect("connects")
    }

    #[test]
    fn connect_reads_the_greeting_then_probes_capabilities() {
        let s = connected(vec![]);
        assert!(s.has_capability("IMAP4REV1"));
        assert!(s.has_capability("uidplus"), "lookup is case-insensitive");
        assert!(!s.has_capability("LOGINDISABLED"));
    }

    #[test]
    fn a_bye_greeting_is_eof_not_a_hang() {
        let t = ScriptedTransport::new([&b"* BYE Too many connections\r\n"[..]]);
        assert!(matches!(
            block_on(ImapSession::connect(t)),
            Err(ImapClientError::Eof)
        ));
    }

    #[test]
    fn examine_is_used_instead_of_select_so_the_source_is_not_mutated() {
        let mut s = connected(vec![
            b"* 172 EXISTS\r\n* OK [UIDVALIDITY 3857529045] valid\r\nA0002 OK [READ-ONLY] done\r\n"
                .to_vec(),
        ]);
        let st = block_on(s.examine("INBOX", None)).expect("examines");
        assert_eq!(st.uid_validity, 3_857_529_045);
        assert_eq!(st.exists, 172);
        let sent = s.conn_sent();
        assert!(sent.contains("A0002 EXAMINE \"INBOX\""), "{sent}");
        assert!(!sent.contains("SELECT"), "SELECT would clear \\Recent");
    }

    #[test]
    fn examine_rejects_a_rotated_uidvalidity_against_a_stored_cursor() {
        let mut s = connected(vec![
            b"* OK [UIDVALIDITY 999] valid\r\nA0002 OK done\r\n".to_vec(),
        ]);
        match block_on(s.examine("INBOX", Some(111))) {
            Err(ImapClientError::UidValidityChanged {
                mailbox,
                expected,
                actual,
            }) => {
                assert_eq!(mailbox, "INBOX");
                assert_eq!((expected, actual), (111, 999));
            }
            other => panic!("resume against a rebuilt mailbox must abort: {other:?}"),
        }
    }

    #[test]
    fn examine_without_uidvalidity_is_a_contract_violation() {
        let mut s = connected(vec![b"* 5 EXISTS\r\nA0002 OK done\r\n".to_vec()]);
        assert!(matches!(
            block_on(s.examine("INBOX", None)),
            Err(ImapClientError::UnexpectedResponse(_))
        ));
    }

    #[test]
    fn enumerate_filters_the_rfc3501_star_range_artifact() {
        // RFC 3501 §6.4.8: `900:*` still returns the last message (UID 42)
        // even though 42 < 900. Resuming past the end must yield nothing, not
        // re-import the final message of every mailbox.
        let mut s = connected(vec![
            b"* OK [UIDVALIDITY 1] v\r\nA0002 OK done\r\n".to_vec(),
            b"* 1 FETCH (UID 42 RFC822.SIZE 5000)\r\nA0003 OK done\r\n".to_vec(),
        ]);
        block_on(s.examine("INBOX", None)).unwrap();
        let entries = block_on(s.enumerate_uids(900)).expect("enumerates");
        assert!(
            entries.is_empty(),
            "the `n:*` trap re-yielded the last message: {entries:?}"
        );
    }

    #[test]
    fn enumerate_keeps_in_range_uids_with_sizes_sorted() {
        let mut s = connected(vec![
            b"* OK [UIDVALIDITY 1] v\r\nA0002 OK done\r\n".to_vec(),
            b"* 2 FETCH (UID 9 RFC822.SIZE 500)\r\n* 1 FETCH (UID 5 RFC822.SIZE 100)\r\n* 3 FETCH (UID 7)\r\nA0003 OK done\r\n".to_vec(),
        ]);
        block_on(s.examine("INBOX", None)).unwrap();
        let e = block_on(s.enumerate_uids(1)).expect("enumerates");
        assert_eq!(
            e,
            vec![
                UidEntry {
                    uid: 5,
                    size: Some(100)
                },
                // No RFC822.SIZE: the source did not report it.
                UidEntry { uid: 7, size: None },
                UidEntry {
                    uid: 9,
                    size: Some(500)
                },
            ]
        );
    }

    #[test]
    fn oversize_is_decided_before_the_body_is_fetched() {
        let big = UidEntry {
            uid: 1,
            size: Some(60 * 1024 * 1024),
        };
        let ok = UidEntry {
            uid: 2,
            size: Some(1024),
        };
        let unknown = UidEntry { uid: 3, size: None };
        let limit = crate::imap_client::DEFAULT_MAX_MESSAGE_BYTES;
        assert!(big.is_oversize(limit));
        assert!(!ok.is_oversize(limit));
        // "when supported by the source" — an unknown size is never oversize.
        assert!(!unknown.is_oversize(limit));
    }

    #[test]
    fn fetch_uses_body_peek_and_strips_recent() {
        let body_resp = b"* 1 FETCH (UID 5 FLAGS (\\Seen \\Recent) INTERNALDATE \"17-Jul-1996 02:44:25 -0700\" BODY[] {5}\r\nhello)\r\nA0003 OK done\r\n".to_vec();
        let mut s = connected(vec![
            b"* OK [UIDVALIDITY 7] v\r\nA0002 OK done\r\n".to_vec(),
            body_resp,
        ]);
        block_on(s.examine("INBOX", None)).unwrap();

        let clock = FakeClock::new();
        let mut got = Vec::new();
        block_on(s.fetch_messages(&[5], &clock, |o| got.push(o))).expect("fetches");

        let sent = s.conn_sent();
        assert!(
            sent.contains("BODY.PEEK[]"),
            "BODY[] would set \\Seen on the source: {sent}"
        );
        match &got[..] {
            [FetchOutcome::Fetched(m)] => {
                assert_eq!(m.uid, 5);
                assert_eq!(m.uid_validity, 7);
                assert_eq!(m.body, b"hello");
                assert_eq!(m.internal_date_epoch, 837_596_665);
                assert_eq!(
                    m.flags,
                    vec!["\\Seen".to_string()],
                    "\\Recent must be stripped"
                );
                assert_eq!(m.mailbox, "INBOX");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_per_message_no_does_not_end_the_session() {
        // § Failure handling: one unreadable message feeds `errored_count`;
        // the other messages still import.
        let mut s = connected(vec![
            b"* OK [UIDVALIDITY 1] v\r\nA0002 OK done\r\n".to_vec(),
            b"A0003 NO [SERVERBUG] cannot read message\r\n".to_vec(),
            fetch_response("A0004", 6, "ok"),
        ]);
        block_on(s.examine("INBOX", None)).unwrap();
        let clock = FakeClock::new();
        let mut got = Vec::new();
        block_on(s.fetch_messages(&[5, 6], &clock, |o| got.push(o))).expect("survives");

        assert_eq!(got.len(), 2);
        assert!(
            matches!(&got[0], FetchOutcome::Failed { uid: 5, .. }),
            "{:?}",
            got[0]
        );
        assert!(matches!(&got[1], FetchOutcome::Fetched(m) if m.uid == 6));
    }

    #[test]
    fn an_ok_with_no_body_is_a_per_message_failure_not_a_silent_drop() {
        // The message was expunged between enumeration and fetch.
        let mut s = connected(vec![
            b"* OK [UIDVALIDITY 1] v\r\nA0002 OK done\r\n".to_vec(),
            b"A0003 OK FETCH completed\r\n".to_vec(),
        ]);
        block_on(s.examine("INBOX", None)).unwrap();
        let clock = FakeClock::new();
        let mut got = Vec::new();
        block_on(s.fetch_messages(&[5], &clock, |o| got.push(o))).expect("survives");
        match &got[..] {
            [FetchOutcome::Failed { uid: 5, reason }] => assert!(reason.contains("no body")),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn fetch_pipelines_up_to_the_concurrency_cap() {
        // Four FETCH commands must be on the wire before the first response is
        // consumed — that is what "4 concurrent FETCH" buys.
        let mut responses = Vec::new();
        for (i, uid) in (1..=4u32).enumerate() {
            responses.push(fetch_response(&format!("A{:04}", i + 3), uid, "x"));
        }
        let mut s = connected(
            vec![b"* OK [UIDVALIDITY 1] v\r\nA0002 OK done\r\n".to_vec()]
                .into_iter()
                .chain(responses)
                .collect::<Vec<_>>(),
        );
        block_on(s.examine("INBOX", None)).unwrap();

        let clock = FakeClock::new();
        let mut got = Vec::new();
        block_on(s.fetch_messages(&[1, 2, 3, 4], &clock, |o| got.push(o))).expect("fetches");
        assert_eq!(got.len(), 4);

        let sent = s.conn_sent();
        for uid in 1..=4 {
            assert!(
                sent.contains(&format!("UID FETCH {uid} (UID FLAGS")),
                "{sent}"
            );
        }
        assert!(
            clock.slept.borrow().is_empty(),
            "4 messages must not rate-limit"
        );
    }

    #[test]
    fn the_rate_cap_sleeps_rather_than_hammering_the_source() {
        // 101 messages: the 101st must wait out the rolling minute.
        let mut chunks = vec![b"* OK [UIDVALIDITY 1] v\r\nA0002 OK done\r\n".to_vec()];
        for i in 0..101u32 {
            chunks.push(fetch_response(&format!("A{:04}", i + 3), i + 1, "x"));
        }
        let mut s = connected(chunks);
        block_on(s.examine("INBOX", None)).unwrap();

        let clock = FakeClock::new();
        let uids: Vec<u32> = (1..=101).collect();
        let mut n = 0;
        block_on(s.fetch_messages(&uids, &clock, |_| n += 1)).expect("fetches");
        assert_eq!(n, 101);
        assert_eq!(
            clock.slept.borrow().len(),
            1,
            "exactly one sleep, at the 101st message"
        );
        assert_eq!(clock.slept.borrow()[0], 60_000, "a full rolling minute");
    }

    #[test]
    fn login_refuses_when_the_server_advertises_logindisabled() {
        let t = ScriptedTransport::new([
            &b"* OK ready\r\n"[..],
            &b"* CAPABILITY IMAP4rev1 LOGINDISABLED\r\nA0001 OK done\r\n"[..],
        ]);
        let mut s = block_on(ImapSession::connect(t)).unwrap();
        let err = block_on(s.login("alice", "hunter2")).expect_err("must refuse");
        assert!(matches!(err, ImapClientError::Rejected { .. }));
        // The password must never have reached the wire.
        assert!(!s.conn_sent().contains("hunter2"));
    }

    #[test]
    fn a_crlf_in_a_password_cannot_inject_an_imap_command() {
        // The injection vector: a password manager value containing CRLF would
        // otherwise terminate the LOGIN line and run `DELETE INBOX`.
        let mut s = connected(vec![]);
        let evil = "hunter2\r\nA0003 DELETE \"INBOX\"";
        let err = block_on(s.login("alice", evil)).expect_err("must reject");
        assert!(matches!(err, ImapClientError::Protocol(_)), "{err:?}");
        assert!(
            !s.conn_sent().contains("DELETE"),
            "no bytes may reach the wire: {}",
            s.conn_sent()
        );
    }

    #[test]
    fn a_crlf_in_a_server_chosen_mailbox_name_cannot_inject_either() {
        // The source server picks mailbox names; a malicious one must not be
        // able to drive our next command.
        let mut s = connected(vec![]);
        let err = block_on(s.examine("IN\r\nA0003 LOGOUT", None)).expect_err("must reject");
        assert!(matches!(err, ImapClientError::Protocol(_)), "{err:?}");
        assert!(!s.conn_sent().contains("LOGOUT"));
    }

    #[test]
    fn quoting_escapes_the_two_quoted_specials() {
        assert_eq!(quote_astring(r#"a"b\c"#, "x").unwrap(), r#""a\"b\\c""#);
        assert_eq!(quote_astring("plain", "x").unwrap(), "\"plain\"");
        assert!(quote_astring("a\rb", "x").is_err());
        assert!(quote_astring("a\nb", "x").is_err());
        assert!(quote_astring("a\0b", "x").is_err());
    }

    #[test]
    fn xoauth2_uses_sasl_ir_when_advertised() {
        let t = ScriptedTransport::new([
            &b"* OK ready\r\n"[..],
            &b"* CAPABILITY IMAP4rev1 SASL-IR AUTH=XOAUTH2\r\nA0001 OK done\r\n"[..],
            &b"A0002 OK authenticated\r\n"[..],
            &b"* CAPABILITY IMAP4rev1\r\nA0003 OK done\r\n"[..],
        ]);
        let mut s = block_on(ImapSession::connect(t)).unwrap();
        block_on(s.authenticate_xoauth2("alice@example.com", "tok")).expect("authenticates");

        let sent = s.conn_sent();
        // Exactly the SASL XOAUTH2 initial-response encoding.
        let expected = base64::engine::general_purpose::STANDARD
            .encode("user=alice@example.com\x01auth=Bearer tok\x01\x01");
        assert!(
            sent.contains(&format!("A0002 AUTHENTICATE XOAUTH2 {expected}")),
            "{sent}"
        );
        // The raw bearer token never appears on the wire in the clear.
        assert!(!sent.contains("Bearer tok"), "{sent}");
    }

    #[test]
    fn xoauth2_falls_back_to_a_continuation_without_sasl_ir() {
        let t = ScriptedTransport::new([
            &b"* OK ready\r\n"[..],
            &b"* CAPABILITY IMAP4rev1 AUTH=XOAUTH2\r\nA0001 OK done\r\n"[..],
            &b"+ go ahead\r\n"[..],
            &b"A0002 OK authenticated\r\n"[..],
            &b"* CAPABILITY IMAP4rev1\r\nA0003 OK done\r\n"[..],
        ]);
        let mut s = block_on(ImapSession::connect(t)).unwrap();
        block_on(s.authenticate_xoauth2("alice", "tok")).expect("authenticates");
        assert!(s.conn_sent().contains("A0002 AUTHENTICATE XOAUTH2\r\n"));
    }

    #[test]
    fn logout_treats_the_bye_as_success() {
        let mut s = connected(vec![
            b"* BYE Logging out\r\nA0002 OK LOGOUT completed\r\n".to_vec(),
        ]);
        block_on(s.logout()).expect("logout is not a failure");
    }

    #[test]
    fn commands_before_examine_are_rejected() {
        let mut s = connected(vec![]);
        assert!(matches!(
            block_on(s.enumerate_uids(1)),
            Err(ImapClientError::UnexpectedResponse(_))
        ));
    }
}
