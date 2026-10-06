//! The export blob's **frame codec** — the client-side seal that makes an
//! export opaque to the box that stores it.
//!
//! Authority: `docs/goal/behavior/mail-export.md` § Blob shape on disk (pinned
//! 2026-09-20) and § Key material.
//!
//! # Why this lives on the client, and only on the client
//!
//! § Export pipeline puts conversion on the user's own app, unconditionally:
//! the nest holds no read key at all, so the bytes it stores must arrive
//! already sealed. § Why a per-session seal states the consequence plainly —
//! *"without the session seal the uploaded chunks would land readable on nest
//! disk"*. So the seal is not applied once at the end; it is applied to every
//! chunk as it is produced, by the same process that produced it. The nest
//! appends the frames this module emits, in order, and parses none of them.
//!
//! # The shape
//!
//! ```text
//! [6-byte preamble: b"FXPT" || format-version u16 BE]   once, at the start
//! then repeated, one per chunk:
//! [ciphertext length u32 BE] [24-byte XChaCha20-Poly1305 nonce] [ciphertext]
//! ```
//!
//! Each frame's plaintext is the next byte slice of the single zstd stream
//! [`super::archive`] produces — never an independently-compressed unit, so a
//! reader must open every frame in order and concatenate before it can
//! decompress anything. Each nonce is drawn fresh, which is what lets a
//! resumed session keep uploading without replaying a counter (and is why the
//! *sealed* file is deliberately not byte-identical across runs, while the
//! plaintext archive is — § Container shape's determinism contract is a claim
//! about the archive, not about this layer).
//!
//! The MAC covers the frame's ciphertext plus a 32-byte associated-data field,
//! [`frame_aad`], binding the preamble, the session id, the format, the
//! frame's index and whether it terminates the blob. That is what turns every
//! rearrangement into a decryption failure rather than a plausible-looking
//! archive:
//!
//! | attack | what fails |
//! |---|---|
//! | swap two frames | both AADs carry the wrong `chunk_idx` |
//! | duplicate a frame | the duplicate's index is already consumed |
//! | drop a frame | every later frame's index is off by one |
//! | truncate the blob | no terminator frame — [`ExportBlobOpener::finish`] refuses |
//! | append junk | it is not a frame the running index authenticates |
//! | claim another format | the `format` half of the AAD differs |
//! | replay under a new version | the preamble is inside the AAD |
//!
//! # The terminator frame
//!
//! [`ExportBlobSealer::finish`] emits one last frame whose plaintext is empty
//! and whose AAD sets `is_final`. It is the blob's commitment to its own
//! length, and it is what keeps § Architectural rules' *no partial-blob
//! download* true without a second AEAD pass over an artifact § Quota
//! composition caps at 10 GiB. A body frame is never empty ([`ExportSealError::
//! EmptyChunk`] refuses one), so a terminator is self-identifying by its
//! ciphertext length and an opener never has to guess.

use chacha20poly1305::aead::rand_core::RngCore;
use chacha20poly1305::aead::{Aead, OsRng, Payload};
use chacha20poly1305::{KeyInit, XChaCha20Poly1305, XNonce};

/// `b"FXPT"` + a `u16` big-endian format version — the first bytes of the
/// blob, and part of every frame's AAD, so it is authenticated despite sitting
/// outside any ciphertext.
pub const EXPORT_BLOB_PREAMBLE: [u8; 6] = [b'F', b'X', b'P', b'T', 0x00, 0x01];

/// The per-session key's length. § Key material mints a 256-bit key
/// client-side when the export session opens, wraps it under the user's actor
/// key, and hands the nest only the wrapped form.
pub const EXPORT_SESSION_KEY_BYTES: usize = 32;

const NONCE_BYTES: usize = 24;
const TAG_BYTES: usize = 16;
/// `len u32 BE` + nonce. The framing overhead a caller budgets per chunk.
pub const FRAME_HEADER_BYTES: usize = 4 + NONCE_BYTES;

/// The largest `ciphertext length` an opener will honour — § Blob shape on
/// disk's pinned maximum frame length.
///
/// **Derived, never chosen.** A full chunk's ciphertext is the plaintext slice
/// [`super::stream::EXPORT_CHUNK_BYTES`] *plus* the Poly1305 tag, so a bound
/// set to the chunk size alone would reject a conforming writer's own output.
/// Tying the two together here is what keeps a future chunk-size change from
/// silently outgrowing the cap.
///
/// The length prefix is the one field the opener must trust before the AEAD
/// can check anything, and it arrives from the nest — the party this seal
/// exists to distrust. So it is checked **first**, before the buffer grows
/// toward it and before [`FRAME_HEADER_BYTES`] is added to it: on a 32-bit
/// target (the web app) that addition wraps for a declared length near
/// `u32::MAX`, which turns a refusal into a panic. Same shape and the same
/// reasoning as `fauna_ipc::MAX_FRAME_SIZE` / `checked_frame_len`, whose doc
/// calls the length-prefix policy a project decision kept in one
/// place.
pub const MAX_EXPORT_FRAME_BYTES: usize = super::stream::EXPORT_CHUNK_BYTES + TAG_BYTES;

/// Why an export blob could not be sealed or opened.
///
/// Every open-side variant is terminal for the archive. An export that will
/// not open is a re-run, never a partial restore: handing the user some of
/// their mailbox while implying it is all of it is the one outcome worse than
/// failing.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ExportSealError {
    #[error("export session key must be {EXPORT_SESSION_KEY_BYTES} bytes, got {got}")]
    BadKeyLength { got: usize },
    #[error("a body frame must not be empty (an empty frame is the terminator)")]
    EmptyChunk,
    #[error("the blob does not start with the export preamble")]
    BadPreamble,
    #[error("unsupported export blob version {got} (this build writes {ours})")]
    UnsupportedVersion { got: u16, ours: u16 },
    #[error("frame {idx} declares {declared} ciphertext bytes, below the {TAG_BYTES}-byte tag")]
    ShortFrame { idx: u64, declared: usize },
    #[error(
        "frame {idx} declares {declared} ciphertext bytes, over the {MAX_EXPORT_FRAME_BYTES}-byte \
         maximum frame length"
    )]
    FrameTooLarge { idx: u64, declared: usize },
    #[error(
        "frame {idx} failed to open — wrong key or session, a reordered, duplicated, dropped or \
         tampered frame, or a substituted format"
    )]
    OpenFailed { idx: u64 },
    #[error("bytes follow the terminator frame")]
    TrailingBytes,
    #[error("the blob ends without its terminator frame — the export is incomplete")]
    MissingTerminator,
}

/// The 32-byte associated-data field a frame is sealed under.
///
/// `BLAKE3(preamble || session_id || 0x00 || format || 0x00 || chunk_idx || is_final)`.
/// The `0x00` separators keep the two variable-length fields unambiguous, so
/// no pair of (session, format) values can collide into one AAD by sliding the
/// boundary between them.
pub fn frame_aad(session_id: &str, format: &str, chunk_idx: u64, is_final: bool) -> [u8; 32] {
    let mut h = blake3::Hasher::new();
    h.update(&EXPORT_BLOB_PREAMBLE);
    h.update(session_id.as_bytes());
    h.update(&[0u8]);
    h.update(format.as_bytes());
    h.update(&[0u8]);
    h.update(&chunk_idx.to_be_bytes());
    h.update(&[u8::from(is_final)]);
    *h.finalize().as_bytes()
}

fn cipher(key: &[u8]) -> Result<XChaCha20Poly1305, ExportSealError> {
    XChaCha20Poly1305::new_from_slice(key)
        .map_err(|_| ExportSealError::BadKeyLength { got: key.len() })
}

/// Seals successive byte slices of one zstd stream into the blob's frames.
///
/// Stateful on purpose: the frame index is the thing the AAD binds, and a
/// caller that tracked it itself would be one off-by-one away from an archive
/// that only fails at download time. The nest keeps the same counter
/// independently (`export_sessions.next_chunk_idx`) and refuses an upload that
/// disagrees, so the two have to be derived, never guessed.
pub struct ExportBlobSealer {
    cipher: XChaCha20Poly1305,
    session_id: String,
    format: String,
    next_idx: u64,
}

impl ExportBlobSealer {
    /// `session_id` and `format` are the values the session was opened with —
    /// they are authenticated, not merely recorded, so a mismatch at open time
    /// is a decryption failure rather than a mislabelled archive.
    pub fn new(key: &[u8], session_id: &str, format: &str) -> Result<Self, ExportSealError> {
        Ok(Self {
            cipher: cipher(key)?,
            session_id: session_id.to_string(),
            format: format.to_string(),
            next_idx: 0,
        })
    }

    /// The blob's first bytes. The caller prepends these to the first chunk it
    /// uploads — they are part of chunk 0's payload on the wire, not a frame
    /// of their own, because the nest appends what it is given and knows
    /// nothing about a header.
    pub fn preamble(&self) -> [u8; 6] {
        EXPORT_BLOB_PREAMBLE
    }

    /// The index the next [`Self::seal_chunk`] will use — what a resuming
    /// client compares against the session row's `next_chunk_idx`.
    pub fn next_chunk_idx(&self) -> u64 {
        self.next_idx
    }

    /// Seal the next slice of the zstd stream.
    pub fn seal_chunk(&mut self, plaintext: &[u8]) -> Result<Vec<u8>, ExportSealError> {
        if plaintext.is_empty() {
            return Err(ExportSealError::EmptyChunk);
        }
        // The writer-side half of [`MAX_EXPORT_FRAME_BYTES`]. An over-long
        // slice would otherwise mint a frame no conforming opener accepts —
        // and, past 4 GiB, one whose `as u32` length prefix truncates
        // silently into a blob that fails AEAD at download naming nothing.
        // Refusing here fails the export where the caller can still say why.
        if plaintext.len() + TAG_BYTES > MAX_EXPORT_FRAME_BYTES {
            return Err(ExportSealError::FrameTooLarge {
                idx: self.next_idx,
                declared: plaintext.len() + TAG_BYTES,
            });
        }
        let frame = self.frame(plaintext, false)?;
        self.next_idx += 1;
        Ok(frame)
    }

    /// Seal the terminator frame — the blob's commitment to its own length.
    /// Idempotent in the sense that it consumes the sealer: a second call
    /// would mint a second terminator, which an opener refuses as trailing
    /// bytes, so the type makes it unrepresentable instead.
    pub fn finish(self) -> Result<Vec<u8>, ExportSealError> {
        self.frame(&[], true)
    }

    fn frame(&self, plaintext: &[u8], is_final: bool) -> Result<Vec<u8>, ExportSealError> {
        let aad = frame_aad(&self.session_id, &self.format, self.next_idx, is_final);
        let mut nonce = [0u8; NONCE_BYTES];
        OsRng.fill_bytes(&mut nonce);
        let ciphertext = self
            .cipher
            .encrypt(
                XNonce::from_slice(&nonce),
                Payload {
                    msg: plaintext,
                    aad: &aad,
                },
            )
            // Encryption under a valid key and nonce has no failure mode that
            // is not a bug in this module.
            .map_err(|_| ExportSealError::OpenFailed { idx: self.next_idx })?;
        let mut out = Vec::with_capacity(FRAME_HEADER_BYTES + ciphertext.len());
        out.extend_from_slice(&(ciphertext.len() as u32).to_be_bytes());
        out.extend_from_slice(&nonce);
        out.extend_from_slice(&ciphertext);
        Ok(out)
    }
}

/// Hand-written and **redacted**: the struct holds a keyed cipher, and a
/// derived `Debug` on a future field could print key material into a log the
/// box owner reads. The three values here are the ones a caller debugging a
/// stalled upload actually wants, and none of them is a secret (the session id
/// and format already cross the wire in the clear).
impl std::fmt::Debug for ExportBlobSealer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExportBlobSealer")
            .field("session_id", &self.session_id)
            .field("format", &self.format)
            .field("next_chunk_idx", &self.next_idx)
            .finish_non_exhaustive()
    }
}

/// Opens an export blob as its bytes arrive, emitting recovered plaintext in
/// order.
///
/// Streaming because the artifact is capped at 10 GiB (§ Quota composition):
/// an opener that needed the whole blob in memory would make a legitimate
/// export unopenable on the device most likely to have produced it. Feed
/// whatever the download yields; the opener buffers a partial frame and emits
/// each chunk's plaintext as soon as its tag verifies.
pub struct ExportBlobOpener {
    cipher: XChaCha20Poly1305,
    session_id: String,
    format: String,
    next_idx: u64,
    buf: Vec<u8>,
    saw_preamble: bool,
    saw_terminator: bool,
}

impl ExportBlobOpener {
    pub fn new(key: &[u8], session_id: &str, format: &str) -> Result<Self, ExportSealError> {
        Ok(Self {
            cipher: cipher(key)?,
            session_id: session_id.to_string(),
            format: format.to_string(),
            next_idx: 0,
            buf: Vec::new(),
            saw_preamble: false,
            saw_terminator: false,
        })
    }

    /// Feed downloaded bytes; returns the plaintext of every frame that
    /// completed. Concatenate the results, in call order, to rebuild the zstd
    /// stream.
    pub fn push(&mut self, bytes: &[u8]) -> Result<Vec<Vec<u8>>, ExportSealError> {
        if self.saw_terminator && !bytes.is_empty() {
            return Err(ExportSealError::TrailingBytes);
        }
        self.buf.extend_from_slice(bytes);
        let mut out = Vec::new();
        if !self.saw_preamble {
            if self.buf.len() < EXPORT_BLOB_PREAMBLE.len() {
                return Ok(out);
            }
            let head = &self.buf[..EXPORT_BLOB_PREAMBLE.len()];
            if head[..4] != EXPORT_BLOB_PREAMBLE[..4] {
                return Err(ExportSealError::BadPreamble);
            }
            let got = u16::from_be_bytes([head[4], head[5]]);
            let ours = u16::from_be_bytes([EXPORT_BLOB_PREAMBLE[4], EXPORT_BLOB_PREAMBLE[5]]);
            if got != ours {
                return Err(ExportSealError::UnsupportedVersion { got, ours });
            }
            self.buf.drain(..EXPORT_BLOB_PREAMBLE.len());
            self.saw_preamble = true;
        }
        while !self.saw_terminator {
            if self.buf.len() < FRAME_HEADER_BYTES {
                break;
            }
            let declared =
                u32::from_be_bytes([self.buf[0], self.buf[1], self.buf[2], self.buf[3]]) as usize;
            // § Blob shape on disk's maximum frame length, checked before the
            // buffer is allowed to grow toward `declared` and before
            // `FRAME_HEADER_BYTES + declared` is computed — that addition
            // wraps on a 32-bit target for a declared length near `u32::MAX`,
            // so the bound is what keeps the refusal a refusal.
            if declared > MAX_EXPORT_FRAME_BYTES {
                return Err(ExportSealError::FrameTooLarge {
                    idx: self.next_idx,
                    declared,
                });
            }
            if declared < TAG_BYTES {
                return Err(ExportSealError::ShortFrame {
                    idx: self.next_idx,
                    declared,
                });
            }
            if self.buf.len() < FRAME_HEADER_BYTES + declared {
                break;
            }
            let nonce = XNonce::clone_from_slice(&self.buf[4..FRAME_HEADER_BYTES]);
            let ct = &self.buf[FRAME_HEADER_BYTES..FRAME_HEADER_BYTES + declared];
            // A terminator carries no plaintext, so its ciphertext is the tag
            // alone — and a body frame is never empty, so the length alone
            // says which AAD to authenticate under. No guessing, and no arm
            // that tries the other one on failure: that would turn a tampered
            // frame into two chances to open.
            let is_final = declared == TAG_BYTES;
            let aad = frame_aad(&self.session_id, &self.format, self.next_idx, is_final);
            let plaintext = self
                .cipher
                .decrypt(&nonce, Payload { msg: ct, aad: &aad })
                .map_err(|_| ExportSealError::OpenFailed { idx: self.next_idx })?;
            self.buf.drain(..FRAME_HEADER_BYTES + declared);
            if is_final {
                self.saw_terminator = true;
            } else {
                self.next_idx += 1;
                out.push(plaintext);
            }
        }
        if self.saw_terminator && !self.buf.is_empty() {
            return Err(ExportSealError::TrailingBytes);
        }
        Ok(out)
    }

    /// Assert the blob was complete. **The call that makes § Architectural
    /// rules' no-partial-blob-download rule true**: a download cut short, or
    /// one taken of a session still running, ends without a terminator frame
    /// and must be refused rather than handed to the user as their mailbox.
    pub fn finish(self) -> Result<(), ExportSealError> {
        if !self.saw_terminator {
            return Err(ExportSealError::MissingTerminator);
        }
        if !self.buf.is_empty() {
            return Err(ExportSealError::TrailingBytes);
        }
        Ok(())
    }

    /// How many body frames have opened so far.
    pub fn frames_opened(&self) -> u64 {
        self.next_idx
    }
}

/// Redacted for the same reason as [`ExportBlobSealer`]'s, and additionally
/// never printing `buf`: a partially-buffered frame is ciphertext, but the
/// plaintext it opens to is the user's mail, and a `Debug` that grew into
/// printing recovered bytes would put it in a log.
impl std::fmt::Debug for ExportBlobOpener {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExportBlobOpener")
            .field("session_id", &self.session_id)
            .field("format", &self.format)
            .field("frames_opened", &self.next_idx)
            .field("buffered_bytes", &self.buf.len())
            .field("saw_terminator", &self.saw_terminator)
            .finish_non_exhaustive()
    }
}

/// Open a whole blob held in memory — the convenience an opener with the
/// bytes already in hand wants (tests, and a small export a client chose to
/// buffer). A 10 GiB download uses [`ExportBlobOpener`] directly.
pub fn open_export_blob(
    key: &[u8],
    session_id: &str,
    format: &str,
    blob: &[u8],
) -> Result<Vec<u8>, ExportSealError> {
    let mut opener = ExportBlobOpener::new(key, session_id, format)?;
    let mut out = Vec::new();
    for chunk in opener.push(blob)? {
        out.extend_from_slice(&chunk);
    }
    opener.finish()?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: [u8; 32] = [0x5A; 32];
    const SID: &str = "0198c0de-dead-beef";

    /// Build a whole blob from `chunks`, as the drive loop does: preamble,
    /// then one frame per chunk, then the terminator.
    fn seal_all(key: &[u8], sid: &str, format: &str, chunks: &[&[u8]]) -> Vec<u8> {
        let mut sealer = ExportBlobSealer::new(key, sid, format).unwrap();
        let mut blob = sealer.preamble().to_vec();
        for c in chunks {
            blob.extend_from_slice(&sealer.seal_chunk(c).unwrap());
        }
        blob.extend_from_slice(&sealer.finish().unwrap());
        blob
    }

    #[test]
    fn a_sealed_blob_opens_to_the_concatenated_stream() {
        let blob = seal_all(&KEY, SID, "mbox", &[b"zstd-0", b"zstd-1", b"zstd-2"]);
        assert_eq!(
            open_export_blob(&KEY, SID, "mbox", &blob).unwrap(),
            b"zstd-0zstd-1zstd-2"
        );
    }

    /// The nest holds no key, so nothing it stores may be readable. The
    /// plaintext of a chunk must not appear anywhere in the blob.
    #[test]
    fn the_blob_carries_no_plaintext() {
        let blob = seal_all(&KEY, SID, "mbox", &[b"Subject: the user's mail"]);
        assert!(
            !blob.windows(b"Subject:".len()).any(|w| w == b"Subject:"),
            "a chunk's plaintext must not survive into the sealed blob"
        );
    }

    /// § Container shape's determinism contract is a claim about the PLAINTEXT
    /// archive. The sealed file draws a fresh nonce per frame, so two seals of
    /// identical input differ — and a later test must never assert otherwise.
    #[test]
    fn two_seals_of_the_same_input_differ_but_open_the_same() {
        let a = seal_all(&KEY, SID, "mbox", &[b"same"]);
        let b = seal_all(&KEY, SID, "mbox", &[b"same"]);
        assert_ne!(a, b, "each frame must draw a fresh nonce");
        assert_eq!(
            open_export_blob(&KEY, SID, "mbox", &a).unwrap(),
            open_export_blob(&KEY, SID, "mbox", &b).unwrap()
        );
    }

    /// The whole point of binding the index into the AAD: every rearrangement
    /// of the frames the nest concatenated is a decryption failure, not a
    /// plausible archive.
    #[test]
    fn reorder_duplicate_and_drop_all_fail_to_open() {
        let mut sealer = ExportBlobSealer::new(&KEY, SID, "mbox").unwrap();
        let pre = sealer.preamble().to_vec();
        let f0 = sealer.seal_chunk(b"AAAA").unwrap();
        let f1 = sealer.seal_chunk(b"BBBB").unwrap();
        let f2 = sealer.seal_chunk(b"CCCC").unwrap();
        let end = sealer.finish().unwrap();

        let cat = |parts: &[&[u8]]| -> Vec<u8> { parts.concat() };
        for (label, blob) in [
            ("swapped", cat(&[&pre, &f1, &f0, &f2, &end])),
            ("duplicated", cat(&[&pre, &f0, &f0, &f1, &f2, &end])),
            ("dropped", cat(&[&pre, &f0, &f2, &end])),
        ] {
            let err = open_export_blob(&KEY, SID, "mbox", &blob).unwrap_err();
            assert!(
                matches!(err, ExportSealError::OpenFailed { .. }),
                "{label} must fail to open, got {err:?}"
            );
        }
        // The untouched order still opens, so the assertions above are about
        // the rearrangement and not about a broken fixture.
        assert_eq!(
            open_export_blob(&KEY, SID, "mbox", &cat(&[&pre, &f0, &f1, &f2, &end])).unwrap(),
            b"AAAABBBBCCCC"
        );
    }

    /// § Architectural rules: "a user mid-export cannot download a partial
    /// blob". A truncated archive has no terminator, and the opener refuses it
    /// rather than handing back the mail it *could* read.
    #[test]
    fn a_truncated_blob_is_refused_even_though_its_frames_open() {
        let blob = seal_all(&KEY, SID, "mbox", &[b"AAAA", b"BBBB"]);
        let cut = &blob[..blob.len() - 1];

        let mut opener = ExportBlobOpener::new(&KEY, SID, "mbox").unwrap();
        let opened = opener.push(cut).unwrap();
        assert_eq!(opened.concat(), b"AAAABBBB", "the whole frames do open");
        assert_eq!(
            opener.finish().unwrap_err(),
            ExportSealError::MissingTerminator,
            "but the archive is incomplete and must be refused"
        );
    }

    #[test]
    fn bytes_after_the_terminator_are_refused() {
        let mut blob = seal_all(&KEY, SID, "mbox", &[b"AAAA"]);
        blob.extend_from_slice(b"junk");
        assert_eq!(
            open_export_blob(&KEY, SID, "mbox", &blob).unwrap_err(),
            ExportSealError::TrailingBytes
        );
    }

    /// § Blob shape on disk: the AAD binds the format, so a blob relabelled as
    /// another format fails to open. That is the format-substitution defence
    /// the original single-pass design named, preserved frame by frame.
    #[test]
    fn a_substituted_format_session_or_key_fails_to_open() {
        let blob = seal_all(&KEY, SID, "mbox", &[b"AAAA"]);
        assert!(open_export_blob(&KEY, SID, "eml-zip", &blob).is_err());
        assert!(open_export_blob(&KEY, "another-session", "mbox", &blob).is_err());
        assert!(open_export_blob(&[0x11; 32], SID, "mbox", &blob).is_err());
    }

    /// The preamble sits outside every ciphertext but inside every AAD, so
    /// nothing on disk is unauthenticated: flipping the version byte is
    /// reported as a version the build cannot read, and corrupting the magic
    /// is reported as "not an export blob" — neither degrades to a partial
    /// open.
    #[test]
    fn the_preamble_is_checked_and_authenticated() {
        let good = seal_all(&KEY, SID, "mbox", &[b"AAAA"]);

        let mut bad_magic = good.clone();
        bad_magic[0] = b'X';
        assert_eq!(
            open_export_blob(&KEY, SID, "mbox", &bad_magic).unwrap_err(),
            ExportSealError::BadPreamble
        );

        let mut bad_version = good.clone();
        bad_version[5] = 0x02;
        assert_eq!(
            open_export_blob(&KEY, SID, "mbox", &bad_version).unwrap_err(),
            ExportSealError::UnsupportedVersion { got: 2, ours: 1 }
        );
    }

    /// The opener is fed by a download, so it must tolerate any split — down
    /// to one byte at a time — and emit the same plaintext in the same order.
    #[test]
    fn any_split_of_the_download_opens_identically() {
        let blob = seal_all(&KEY, SID, "maildir", &[b"AAAA", b"BB", b"CCCCCC"]);
        for step in [1usize, 3, 7, 64, blob.len()] {
            let mut opener = ExportBlobOpener::new(&KEY, SID, "maildir").unwrap();
            let mut out = Vec::new();
            for part in blob.chunks(step) {
                for frame in opener.push(part).unwrap() {
                    out.extend_from_slice(&frame);
                }
            }
            opener.finish().unwrap();
            assert_eq!(out, b"AAAABBCCCCCC", "split every {step} bytes");
        }
    }

    /// A body frame is never empty, which is what makes the terminator
    /// self-identifying by length alone — the opener never has to try both
    /// AADs, and so never gets two chances to open a tampered frame.
    #[test]
    fn an_empty_body_chunk_is_refused_so_the_terminator_stays_unambiguous() {
        let mut sealer = ExportBlobSealer::new(&KEY, SID, "mbox").unwrap();
        assert_eq!(
            sealer.seal_chunk(b"").unwrap_err(),
            ExportSealError::EmptyChunk
        );
        // And the index did not move, so the refusal costs the caller nothing.
        assert_eq!(sealer.next_chunk_idx(), 0);
    }

    #[test]
    fn a_wrong_length_key_is_refused_at_construction() {
        assert_eq!(
            ExportBlobSealer::new(&[0u8; 16], SID, "mbox").unwrap_err(),
            ExportSealError::BadKeyLength { got: 16 }
        );
        assert_eq!(
            ExportBlobOpener::new(&[0u8; 31], SID, "mbox").unwrap_err(),
            ExportSealError::BadKeyLength { got: 31 }
        );
    }

    /// The sealer's counter is the one the nest's `export_sessions.
    /// next_chunk_idx` has to agree with, so it is readable rather than
    /// inferred.
    #[test]
    fn the_sealers_index_tracks_the_frames_it_has_emitted() {
        let mut sealer = ExportBlobSealer::new(&KEY, SID, "mbox").unwrap();
        assert_eq!(sealer.next_chunk_idx(), 0);
        sealer.seal_chunk(b"A").unwrap();
        assert_eq!(sealer.next_chunk_idx(), 1);
        sealer.seal_chunk(b"B").unwrap();
        assert_eq!(sealer.next_chunk_idx(), 2);
    }
    /// A frame header claiming more than [`MAX_EXPORT_FRAME_BYTES`], planted
    /// in the `fauna-ipc` regression test's shape (`MAX + 1`, assert the
    /// refusal).
    ///
    /// The assertion that matters is **where** it is refused: the opener is
    /// fed the 28-byte header and nothing else, so the refusal happens before
    /// a single ciphertext byte has been buffered. An opener that waited for
    /// `declared` bytes before judging them would accumulate 16 MiB here and
    /// 4 GiB on a hostile blob — which is exactly the whole-archive-in-memory
    /// opener the streaming design exists to avoid.
    #[test]
    fn a_frame_declaring_more_than_the_maximum_is_refused_before_it_is_buffered() {
        let mut blob = EXPORT_BLOB_PREAMBLE.to_vec();
        blob.extend_from_slice(&((MAX_EXPORT_FRAME_BYTES + 1) as u32).to_be_bytes());
        blob.extend_from_slice(&[0u8; NONCE_BYTES]);
        assert_eq!(blob.len(), EXPORT_BLOB_PREAMBLE.len() + FRAME_HEADER_BYTES);

        let mut opener = ExportBlobOpener::new(&KEY, SID, "mbox").unwrap();
        assert_eq!(
            opener.push(&blob),
            Err(ExportSealError::FrameTooLarge {
                idx: 0,
                declared: MAX_EXPORT_FRAME_BYTES + 1,
            })
        );
    }

    /// The 32-bit consequence, and the reason the bound is checked before the
    /// header size is added to `declared`: on wasm32 `usize` is 32 bits, so
    /// `FRAME_HEADER_BYTES + declared` wraps for a declared length near
    /// `u32::MAX` — in release into a slice index whose start exceeds its end
    /// (a panic, i.e. a trap on wasm), in debug into an overflow panic. This
    /// test runs on a 64-bit host where the addition would merely be large,
    /// so what it pins is the *ordering*: the bound is consulted first, and
    /// the arithmetic the wrap lives in is never reached.
    #[test]
    fn a_declared_length_near_u32_max_is_refused_rather_than_wrapping() {
        let mut blob = EXPORT_BLOB_PREAMBLE.to_vec();
        blob.extend_from_slice(&u32::MAX.to_be_bytes());
        blob.extend_from_slice(&[0u8; NONCE_BYTES]);

        let mut opener = ExportBlobOpener::new(&KEY, SID, "mbox").unwrap();
        assert_eq!(
            opener.push(&blob),
            Err(ExportSealError::FrameTooLarge {
                idx: 0,
                declared: u32::MAX as usize,
            })
        );
    }

    /// The trap on the other side of the bound: a full chunk's *declared*
    /// length is the plaintext slice plus the 16-byte Poly1305 tag, so a
    /// maximum set to the chunk size alone would reject a conforming writer's
    /// own output. Seals a full [`super::super::stream::EXPORT_CHUNK_BYTES`]
    /// chunk and opens it.
    #[test]
    fn the_maximum_admits_a_conforming_writers_own_full_chunk() {
        assert_eq!(
            MAX_EXPORT_FRAME_BYTES,
            super::super::stream::EXPORT_CHUNK_BYTES + TAG_BYTES,
            "the bound is derived from the chunk size, never chosen beside it"
        );
        let full = vec![0xA5u8; super::super::stream::EXPORT_CHUNK_BYTES];
        let blob = seal_all(&KEY, SID, "mbox", &[&full]);

        // The frame really is at the bound, not under it.
        let declared_at = EXPORT_BLOB_PREAMBLE.len();
        let declared = u32::from_be_bytes([
            blob[declared_at],
            blob[declared_at + 1],
            blob[declared_at + 2],
            blob[declared_at + 3],
        ]) as usize;
        assert_eq!(declared, MAX_EXPORT_FRAME_BYTES);

        assert_eq!(open_export_blob(&KEY, SID, "mbox", &blob).unwrap(), full);
    }

    /// The writer-side half. A caller handing the sealer an over-long slice
    /// would mint a frame no conforming opener accepts; refusing at seal time
    /// fails the export where the caller can still say which chunk it was.
    #[test]
    fn the_sealer_refuses_a_chunk_that_would_exceed_the_maximum_frame() {
        let over = vec![0u8; super::super::stream::EXPORT_CHUNK_BYTES + 1];
        let mut sealer = ExportBlobSealer::new(&KEY, SID, "mbox").unwrap();
        assert_eq!(
            sealer.seal_chunk(&over),
            Err(ExportSealError::FrameTooLarge {
                idx: 0,
                declared: super::super::stream::EXPORT_CHUNK_BYTES + 1 + TAG_BYTES,
            })
        );
    }
}
