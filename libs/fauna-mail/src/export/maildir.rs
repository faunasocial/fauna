//! The `Maildir++` serializer — the Courier extension (Dovecot/Cyrus/Courier
//! canonical; no IETF RFC).
//!
//! Authority: `docs/goal/behavior/mail-export.md` § Format choices →
//! `Maildir++`, whose three filename components were pinned deterministic
//! 2026-09-20 because the live convention mints them from pid + wall-clock +
//! hostname and § Goal's byte-identical bar forbids all three.
//!
//! # The pinned filename
//!
//! `<unix-time>.<unique>.<host>:2,<flags>`, where:
//!
//! - `<unix-time>` is the message's INTERNALDATE in epoch seconds;
//! - `<unique>` is [`super::helpers::message_digest16`] over the raw message,
//!   suffixed `-<n>` (from 2) only to break a genuine within-mailbox collision;
//! - `<host>` is the fixed literal [`MAILDIR_HOST`] — never the nest's
//!   hostname, which would both leak the deployment into the user's archive and
//!   make the output machine-dependent;
//! - `<flags>` are concatenated with no separator, ASCII-ascending.
//!
//! # Everything lands in `cur/`
//!
//! `new/` and `tmp/` are created empty. A message in `new/` carries no `:2,`
//! suffix *by construction*, so routing unseen mail there would silently drop
//! every flag it holds — losing `\Flagged` on an unread message is a data loss
//! the format need not take. `doveadm` and ImportExportTools NG emit the same
//! shape.
//!
//! # One canonical directory name
//!
//! A mailbox's directory is named once, when the mailbox opens, by the run's
//! [`ComponentNamer`], and that one string is used everywhere after it: the
//! `cur/`/`new/`/`tmp/` placeholders, the message paths, and the
//! `subscriptions` index line. The namer keeps every directory apart from every
//! other — and from the `subscriptions` index beside them, which it holds
//! reserved — both inside the archive and once extracted onto a case- or
//! normalization-insensitive filesystem (§ Format choices → *Mailbox names in
//! archive paths*, → *Archive paths on the extracting filesystem*). That second
//! half is what lets the `<unique>` set restart per directory: two directories
//! the filesystem merged would put the same message's one filename in both, and
//! a copy would be lost. The
//! shape this replaced folded many names onto one directory and then keyed
//! `open` on the *raw* name while writing the *folded* one: two such mailboxes
//! emitted the placeholders twice (the container refused the duplicate and the
//! whole export died), cleared the `<unique>` set mid-directory, and left the
//! index naming mailboxes rather than the directories beside it.

use std::collections::HashSet;

use super::helpers::{has_flag, message_digest16};
use super::paths::ComponentNamer;
use super::{ExportEntry, ExportError, ExportMessage};

/// The pinned `<host>` component. RFC 2606 reserves `.invalid` for names that
/// are guaranteed not to resolve, which is exactly the claim being made.
pub(super) const MAILDIR_HOST: &str = "fauna.invalid";

/// The index file at the export root, beside the mailbox directories
/// (§ Format choices → `Maildir++`).
const SUBSCRIPTIONS: &str = "subscriptions";

/// The colon-2 flag alphabet, already in the ASCII-ascending order the suffix
/// requires, paired with the IMAP flag each encodes.
const FLAG_ALPHABET: [(char, &str); 5] = [
    ('D', "\\Draft"),
    ('F', "\\Flagged"),
    ('R', "\\Answered"),
    ('S', "\\Seen"),
    ('T', "\\Deleted"),
];

pub(super) struct MaildirState {
    /// The mailbox currently being written — `(raw name, its directory)` — and
    /// the `<unique>` components already spent inside it.
    open: Option<(String, String)>,
    used: HashSet<String>,
    /// Names the mailbox directories, with the index file beside them reserved:
    /// a mailbox called `subscriptions`, in any case, climbs the namer's ladder
    /// to `%73ubscriptions` like any other taken name.
    namer: ComponentNamer,
    /// Every directory written, in first-seen (= mailbox-ascending) order —
    /// the `subscriptions` index, which therefore names exactly the
    /// directories the archive holds.
    directories: Vec<String>,
    /// The earliest INTERNALDATE of the run, which dates the index.
    earliest: Option<i64>,
}

impl Default for MaildirState {
    fn default() -> Self {
        Self {
            open: None,
            used: HashSet::new(),
            namer: ComponentNamer::reserving(&[SUBSCRIPTIONS]),
            directories: Vec::new(),
            earliest: None,
        }
    }
}

impl MaildirState {
    pub(super) fn push(
        &mut self,
        root: &str,
        message: &ExportMessage,
        body: &[u8],
    ) -> Result<Vec<ExportEntry>, ExportError> {
        let mut entries = Vec::new();

        // Keyed on the raw mailbox name, so a mailbox is named exactly once.
        let dir = match &self.open {
            Some((mailbox, dir)) if mailbox == &message.mailbox => dir.clone(),
            _ => {
                let dir = self.namer.name(&message.mailbox)?;
                self.open = Some((message.mailbox.clone(), dir.clone()));
                self.used.clear();
                self.directories.push(dir.clone());
                // A well-formed Maildir has all three subdirectories even when
                // two of them stay empty.
                for sub in ["cur", "new", "tmp"] {
                    entries.push(ExportEntry::dir(
                        format!("{root}/{dir}/{sub}/"),
                        message.internal_date_epoch,
                    ));
                }
                dir
            }
        };
        self.earliest = Some(match self.earliest {
            Some(e) => e.min(message.internal_date_epoch),
            None => message.internal_date_epoch,
        });

        let unique = self.mint_unique(body);
        let name = format!(
            "{}.{unique}.{MAILDIR_HOST}:2,{}",
            message.internal_date_epoch,
            flag_suffix(&message.flags)
        );
        entries.push(ExportEntry::file(
            format!("{root}/{dir}/cur/{name}"),
            body.to_vec(),
            message.internal_date_epoch,
        ));
        Ok(entries)
    }

    pub(super) fn finish(self, root: &str) -> Vec<ExportEntry> {
        if self.directories.is_empty() {
            return Vec::new();
        }
        // § Format choices: "Subscribed-mailbox state preserved in a
        // `subscriptions` file at the export root." The scope step's selection
        // *is* the subscription set the user chose to carry out. One line per
        // directory, written as the directory is named — never the raw mailbox
        // name, which would disagree with the tree beside it and, carrying a
        // line break, could forge a line. (The nest refuses CR/LF in a mailbox
        // name at creation; this index does not lean on that — an encoded name
        // cannot hold a control byte at all.)
        let mut body = self.directories.join("\n");
        body.push('\n');
        vec![ExportEntry::file(
            format!("{root}/{SUBSCRIPTIONS}"),
            body.into_bytes(),
            self.earliest.unwrap_or_default(),
        )]
    }

    /// The message digest, disambiguated only when two byte-identical messages
    /// share one mailbox (a genuine duplicate, not a hash collision).
    fn mint_unique(&mut self, body: &[u8]) -> String {
        let base = message_digest16(body);
        if self.used.insert(base.clone()) {
            return base;
        }
        let mut n = 2usize;
        loop {
            let candidate = format!("{base}-{n}");
            if self.used.insert(candidate.clone()) {
                return candidate;
            }
            n += 1;
        }
    }
}

/// The colon-2 suffix: present flags only, concatenated, ASCII-ascending.
fn flag_suffix(flags: &[String]) -> String {
    FLAG_ALPHABET
        .iter()
        .filter(|(_, imap)| has_flag(flags, imap))
        .map(|(ch, _)| *ch)
        .collect()
}
