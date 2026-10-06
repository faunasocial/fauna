//! ShareToken — a signed, self-contained file-share capability.
//!
//! A ShareToken binds a content manifest hash, author identity, filename,
//! expiry time, and visibility flag. It signs over its canonical-dag-cbor CID
//! (sign-over-CID, per `docs/goal/architecture/serialization.md`) and travels
//! as the embed-as-bytes `{envelope, bytes}` wire shape, base64url-encoded so
//! it can be safely passed around as a URL segment. The URL is self-contained:
//! the canonical bytes the author signed ride inline alongside the envelope, so
//! a receiver verifies by hash + signature without resolving the CID elsewhere.

use serde::{Deserialize, Serialize};

use crate::data::Timestamp;
#[cfg(test)]
use crate::encoding::EmbedAsBytes;
use crate::encoding::{self, Signed};
use crate::error::{Error, Result};
use crate::identity::{ActorId, ActorKeypair};

/// Default token lifetime: 7 days in seconds.
pub const DEFAULT_EXPIRY_SECS: u64 = 7 * 24 * 60 * 60;

/// The signed content of a capability token authorising access to a shared
/// file. The Ed25519 signature is NOT a field here — it ships in the
/// sign-over-CID envelope alongside these bytes (embed-as-bytes), exactly like
/// every other signed Fauna kind (`Post`, `Profile`, …).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShareToken {
    #[serde(with = "serde_bytes")]
    pub manifest_hash: [u8; 32],
    pub author: ActorId,
    pub filename: String,
    pub expires: u64,
    pub public: bool,
    /// How the link OPENS (`public` keeps saying who may open it): `true` is a
    /// fragment-keyed private link — the file rests and travels sealed, the
    /// link key rides the URL fragment no browser sends, and the nest serves
    /// only ciphertext plus the registered [`KeyEnvelope`]
    /// (`share-links.md` § The private-file extension). Additive: absent
    /// decodes as `false` (a public link), and a `false` token is never
    /// written with the field, so every public link's signed bytes — hence
    /// its registry id — are exactly what they were before the field existed.
    #[serde(default, skip_serializing_if = "is_false")]
    pub key_in_fragment: bool,
}

fn is_false(b: &bool) -> bool {
    !*b
}

impl Signed for ShareToken {
    fn signer_public_key(&self) -> &[u8; 32] {
        &self.author.0
    }
}

impl ShareToken {
    /// Build a new (unsigned) share-token content. Sign + serialise it via
    /// [`to_base64url`](Self::to_base64url) / [`to_url`](Self::to_url) with the
    /// keypair whose identity matches `author`.
    pub fn new(
        manifest_hash: [u8; 32],
        author: ActorId,
        filename: String,
        expires: u64,
        public: bool,
    ) -> Self {
        ShareToken {
            manifest_hash,
            author,
            filename,
            expires,
            public,
            key_in_fragment: false,
        }
    }

    /// Build a fragment-keyed private link's (unsigned) token content. Always
    /// `public` (anyone holding the link opens it) and always an **empty**
    /// `filename`: the token rides the URL path every request log sees, so
    /// the name travels sealed inside the [`KeyEnvelope`] instead.
    pub fn fragment_keyed(manifest_hash: [u8; 32], author: ActorId, expires: u64) -> Self {
        ShareToken {
            manifest_hash,
            author,
            filename: String::new(),
            expires,
            public: true,
            key_in_fragment: true,
        }
    }

    /// Return true if this token has expired (expires <= now in seconds).
    pub fn is_expired(&self) -> bool {
        let now = Timestamp::now_secs() as u64;
        self.expires <= now
    }

    /// Sign over the canonical-dag-cbor CID of this content and encode the
    /// embed-as-bytes `{envelope, bytes}` wire as a base64url (no-padding)
    /// string. `keypair` must match `self.author`.
    pub fn to_base64url(&self, keypair: &ActorKeypair) -> Result<String> {
        use base64::Engine as _;
        if keypair.actor_id() != self.author {
            return Err(Error::Encoding(
                "share-token signing keypair does not match token author".to_string(),
            ));
        }
        let bytes = encoding::sign_and_pack(keypair, self)?;
        Ok(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes))
    }

    /// Decode and verify a share token from its base64url (no-padding) form.
    ///
    /// Runs the sign-over-CID two-step (BLAKE3 of the inner bytes against the
    /// envelope CID, then Ed25519 over the CID with the token author's key)
    /// before returning. A token whose signature does not verify is rejected
    /// with `Err` — callers cannot hold an unverified `ShareToken`.
    pub fn from_base64url(s: &str) -> Result<Self> {
        encoding::decode_and_verify_base64url(s)
    }

    /// Build a share URL: `{nest_url}/share/{base64url_token}`. Signs with
    /// `keypair` (must match `self.author`).
    pub fn to_url(&self, keypair: &ActorKeypair, nest_url: &str) -> Result<String> {
        let token_str = self.to_base64url(keypair)?;
        let nest_url = nest_url.trim_end_matches('/');
        Ok(format!("{nest_url}/share/{token_str}"))
    }
}

/// Derive the share-link registry token-id from a token's base64url form: the
/// `blake3` of the canonical signed wire bytes (the base64url-decoded
/// `EmbedAsBytes`). This is the `share_tokens.token_id` primary key the nest
/// registers (`fauna.share.create`) and re-derives at `GET /share/{token}` to
/// look up revocation — both sides hash the same bytes, so the ids match.
///
/// This does **not** verify the signature; pair it with
/// [`ShareToken::from_base64url`] (which does) when accepting a token. Returns
/// `Err` only when `s` is not valid base64url.
pub fn token_id_from_base64url(s: &str) -> Result<[u8; 32]> {
    use base64::Engine as _;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(s)
        .map_err(|e| Error::Encoding(format!("base64url decode: {e}")))?;
    Ok(*blake3::hash(&bytes).as_bytes())
}

// ── The fragment-keyed private link's key envelope ──────────────────────────

/// Length of a private link's key — a fresh random secret per link, carried
/// only in the URL fragment (`https://<nest>/share/<token>#<key>`).
pub const LINK_KEY_LEN: usize = 32;

/// The largest sealed [`KeyEnvelope`] the nest registers. An envelope costs
/// ~70 bytes per chunk (a 32-byte key, a 36-byte hash, CBOR framing), so this
/// admits a file of roughly 50 000 chunks — far past any file the sync plane
/// chunks today — while keeping one registration a bounded row.
pub const MAX_KEY_ENVELOPE_BYTES: usize = 4 * 1024 * 1024;

/// Version byte leading every sealed envelope.
const KEY_ENVELOPE_V1: u8 = 1;
/// Associated data binding the AEAD to this one use of the link key.
const KEY_ENVELOPE_AAD: &[u8] = b"fauna.share.key_envelope.v1";
const KEY_ENVELOPE_NONCE_LEN: usize = 12;

/// Mint a fresh private-link key.
pub fn generate_link_key() -> [u8; LINK_KEY_LEN] {
    let mut key = [0u8; LINK_KEY_LEN];
    getrandom::fill(&mut key).expect("getrandom failed");
    key
}

/// The URL-fragment form of a link key: base64url, no padding.
pub fn encode_link_key(key: &[u8; LINK_KEY_LEN]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(key)
}

/// Parse a link key from a URL fragment (a leading `#` is tolerated).
pub fn decode_link_key(fragment: &str) -> Result<[u8; LINK_KEY_LEN]> {
    use base64::Engine as _;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(fragment.trim_start_matches('#'))
        .map_err(|e| Error::Encoding(format!("link key: {e}")))?;
    bytes
        .try_into()
        .map_err(|_| Error::Encoding("link key: not 32 bytes".to_string()))
}

/// What a fragment-keyed link's holder needs to open the file, and nothing
/// more: the per-chunk keys of exactly the chunks the link's manifest names
/// (`chunk_crypto::chunk_key_for`), their plaintext hashes, the whole file's
/// plaintext hash, and the file's name. Sealed under the link key and
/// registered with the link; the nest stores and serves it opaque
/// (`share-links.md` § The private-file extension, *What the nest stores*).
///
/// The hashes ride here rather than being read off the manifest because the
/// manifest carries them only sealed (`mls-group-key-material.md` § M2
/// *Sealed manifest hashes*): the envelope's AEAD tag is the author's word for
/// them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KeyEnvelope {
    pub filename: String,
    /// The whole file's plaintext hash (`ChunkManifest::file_hash`).
    pub file_hash: crate::data::ContentHash,
    /// The plaintext hash of each chunk, in manifest order.
    pub chunk_hashes: Vec<crate::data::ContentHash>,
    /// `chunk_keys[i]` opens the chunk `chunk_hashes[i]` names.
    pub chunk_keys: Vec<serde_bytes::ByteBuf>,
}

impl KeyEnvelope {
    /// Derive a file's envelope from the root its chunks were sealed under.
    pub fn derive(
        root: &[u8; 32],
        filename: String,
        file_hash: crate::data::ContentHash,
        chunk_hashes: Vec<crate::data::ContentHash>,
    ) -> Self {
        let chunk_keys = chunk_hashes
            .iter()
            .map(|h| serde_bytes::ByteBuf::from(crate::chunk_crypto::chunk_key_for(root, h)))
            .collect();
        KeyEnvelope {
            filename,
            file_hash,
            chunk_hashes,
            chunk_keys,
        }
    }

    /// The key for chunk `i`, if the envelope names one of the right size.
    pub fn chunk_key(&self, i: usize) -> Option<[u8; 32]> {
        self.chunk_keys.get(i)?.as_slice().try_into().ok()
    }

    /// Seal under `link_key` (ChaCha20-Poly1305, a fresh random nonce — the
    /// key seals this one envelope and is used for nothing else):
    /// `v ‖ nonce ‖ ciphertext`.
    pub fn seal(&self, link_key: &[u8; LINK_KEY_LEN]) -> Result<Vec<u8>> {
        use chacha20poly1305::{
            ChaCha20Poly1305,
            aead::{Aead, AeadCore, KeyInit, OsRng, Payload},
        };
        let plain = encoding::canonical_encode(self)?;
        let cipher = ChaCha20Poly1305::new_from_slice(link_key).expect("32-byte key is valid");
        let nonce = ChaCha20Poly1305::generate_nonce(&mut OsRng);
        let ct = cipher
            .encrypt(
                &nonce,
                Payload {
                    msg: &plain,
                    aad: KEY_ENVELOPE_AAD,
                },
            )
            .map_err(|e| Error::Encoding(format!("key envelope seal: {e}")))?;
        let mut out = Vec::with_capacity(1 + KEY_ENVELOPE_NONCE_LEN + ct.len());
        out.push(KEY_ENVELOPE_V1);
        out.extend_from_slice(nonce.as_slice());
        out.extend_from_slice(&ct);
        Ok(out)
    }

    /// Open a sealed envelope with the link key from the URL fragment. Fails
    /// closed on a wrong key, a tampered byte, an unknown version, or an
    /// envelope whose key and hash lists disagree in length.
    pub fn open(link_key: &[u8; LINK_KEY_LEN], sealed: &[u8]) -> Result<Self> {
        use chacha20poly1305::{
            ChaCha20Poly1305, Nonce,
            aead::{Aead, KeyInit, Payload},
        };
        let Some((&v, rest)) = sealed.split_first() else {
            return Err(Error::Encoding("key envelope: empty".to_string()));
        };
        if v != KEY_ENVELOPE_V1 {
            return Err(Error::Encoding(format!(
                "key envelope: unknown version {v} (this build reads v{KEY_ENVELOPE_V1})"
            )));
        }
        if rest.len() < KEY_ENVELOPE_NONCE_LEN {
            return Err(Error::Encoding("key envelope: truncated".to_string()));
        }
        let (nonce, ct) = rest.split_at(KEY_ENVELOPE_NONCE_LEN);
        let cipher = ChaCha20Poly1305::new_from_slice(link_key).expect("32-byte key is valid");
        let plain = cipher
            .decrypt(
                Nonce::from_slice(nonce),
                Payload {
                    msg: ct,
                    aad: KEY_ENVELOPE_AAD,
                },
            )
            .map_err(|_| Error::Encoding("key envelope: does not open under this key".into()))?;
        let envelope: KeyEnvelope = encoding::canonical_decode(&plain)?;
        if envelope.chunk_keys.len() != envelope.chunk_hashes.len()
            || envelope.chunk_keys.iter().any(|k| k.len() != 32)
        {
            return Err(Error::Encoding(
                "key envelope: chunk keys do not pair with chunk hashes".to_string(),
            ));
        }
        Ok(envelope)
    }

    /// Reassemble the file from its ciphertext chunks (in manifest order),
    /// opening each with its key and verifying it against its plaintext hash,
    /// then the whole against [`Self::file_hash`] — the viewer's one call, so
    /// no caller hands out unverified bytes.
    pub fn open_file(&self, ciphertexts: &[Vec<u8>]) -> Result<Vec<u8>> {
        if ciphertexts.len() != self.chunk_hashes.len() {
            return Err(Error::Encoding(format!(
                "share file: {} chunks for an envelope naming {}",
                ciphertexts.len(),
                self.chunk_hashes.len()
            )));
        }
        let mut out = Vec::new();
        for (i, (hash, ct)) in self.chunk_hashes.iter().zip(ciphertexts).enumerate() {
            let key = self
                .chunk_key(i)
                .ok_or_else(|| Error::Encoding(format!("share file: no key for chunk {i}")))?;
            let plain = crate::chunk_crypto::open_chunk_with_key(&key, hash, ct)
                .map_err(|e| Error::Encoding(format!("share file: chunk {i}: {e}")))?;
            out.extend_from_slice(&plain);
        }
        if crate::data::ContentHash::of_raw(&out) != self.file_hash {
            return Err(Error::Encoding(
                "share file: reassembled bytes do not match the file hash".to_string(),
            ));
        }
        Ok(out)
    }
}

/// Return a MIME content-type string for the given filename based on its
/// extension. The lookup is case-insensitive. Returns `"application/octet-stream"`
/// for unknown or missing extensions.
pub fn content_type_for_filename(filename: &str) -> &'static str {
    let ext = filename
        .rfind('.')
        .map(|i| &filename[i + 1..])
        .unwrap_or("");

    match ext.to_ascii_lowercase().as_str() {
        // Text
        "txt" => "text/plain",
        "html" | "htm" => "text/html",
        "css" => "text/css",
        "csv" => "text/csv",
        "xml" => "text/xml",
        "md" => "text/markdown",

        // Application
        "json" => "application/json",
        "pdf" => "application/pdf",
        "zip" => "application/zip",
        "gz" => "application/gzip",
        "tar" => "application/x-tar",
        "wasm" => "application/wasm",
        "js" => "application/javascript",
        "doc" => "application/msword",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "xls" => "application/vnd.ms-excel",
        "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "ppt" => "application/vnd.ms-powerpoint",
        "pptx" => "application/vnd.openxmlformats-officedocument.presentationml.presentation",

        // Image
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "svg" => "image/svg+xml",
        "webp" => "image/webp",
        "ico" => "image/x-icon",
        "bmp" => "image/bmp",

        // Audio
        "mp3" => "audio/mpeg",
        "ogg" => "audio/ogg",
        "flac" => "audio/flac",
        "wav" => "audio/wav",
        "m4a" => "audio/mp4",

        // Video
        "mp4" => "video/mp4",
        "webm" => "video/webm",
        "ogv" => "video/ogg",
        "mov" => "video/quicktime",

        // Font
        "ttf" => "font/ttf",
        "otf" => "font/otf",
        "woff" => "font/woff",
        "woff2" => "font/woff2",

        _ => "application/octet-stream",
    }
}

/// Whether `filename` names an audio or video file — derived from
/// [`content_type_for_filename`]'s single-source extension map (never a parallel
/// extension list, so the two can't drift). The tui external-media handoff's
/// trigger predicate (`apps/tui.md` § External media handoff); named by the
/// `media-external-open-button` ui.yaml registry entry.
pub fn is_audio_video_filename(filename: &str) -> bool {
    let content_type = content_type_for_filename(filename);
    content_type.starts_with("audio/") || content_type.starts_with("video/")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_keypair() -> ActorKeypair {
        ActorKeypair::generate()
    }

    fn sample_token(keypair: &ActorKeypair) -> ShareToken {
        ShareToken::new(
            [0xAB; 32],
            keypair.actor_id(),
            "report.pdf".to_string(),
            u64::MAX,
            true,
        )
    }

    #[test]
    fn create_and_verify() {
        let kp = test_keypair();
        let token = sample_token(&kp);
        // Sign + encode, then decode-which-verifies: a clean round-trip proves
        // the sign-over-CID envelope was produced and verifies.
        let url = token.to_base64url(&kp).unwrap();
        let decoded = ShareToken::from_base64url(&url).unwrap();
        assert_eq!(decoded.filename, token.filename);
        assert_eq!(decoded.author, token.author);
    }

    #[test]
    fn tampered_token_fails_verification() {
        use base64::Engine as _;
        let kp = test_keypair();
        let token = sample_token(&kp);
        let good = token.to_base64url(&kp).unwrap();

        // Tamper the signed inner content bytes without re-signing — the
        // BLAKE3 hash no longer matches the envelope CID, so verify rejects.
        let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(&good)
            .unwrap();
        let mut wire: EmbedAsBytes = encoding::canonical_decode(&bytes).unwrap();
        let last = wire.bytes.len() - 1;
        wire.bytes[last] ^= 0xff;
        let tampered = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(encoding::canonical_encode(&wire).unwrap());

        assert!(ShareToken::from_base64url(&tampered).is_err());
    }

    #[test]
    fn expired_token() {
        let kp = test_keypair();
        let token = ShareToken::new([0u8; 32], kp.actor_id(), "a.txt".to_string(), 0, false);
        assert!(token.is_expired());
    }

    #[test]
    fn non_expired_token() {
        let kp = test_keypair();
        let token = ShareToken::new(
            [0u8; 32],
            kp.actor_id(),
            "a.txt".to_string(),
            u64::MAX,
            false,
        );
        assert!(!token.is_expired());
    }

    #[test]
    fn base64url_roundtrip() {
        let kp = test_keypair();
        let token = sample_token(&kp);
        let encoded = token.to_base64url(&kp).unwrap();
        // Must not contain padding
        assert!(!encoded.contains('='));
        let decoded = ShareToken::from_base64url(&encoded).unwrap();
        assert_eq!(decoded.filename, token.filename);
        assert_eq!(decoded.manifest_hash, token.manifest_hash);
    }

    #[test]
    fn signing_keypair_must_match_author() {
        let author_kp = test_keypair();
        let other_kp = test_keypair();
        let token = sample_token(&author_kp);
        // Signing with a keypair whose identity differs from `author` is a
        // producer bug — rejected up front rather than producing a token that
        // silently fails verification downstream.
        assert!(token.to_base64url(&other_kp).is_err());
    }

    /// Layer 6 (Domain C): the share-token URL form must be strict canonical
    /// dag-cbor in the embed-as-bytes shape (sign-over-CID), not BARE. The
    /// base64url-decoded bytes must pass `fauna_cbor::decode_strict` as an
    /// `EmbedAsBytes` envelope. Pre-flip (BARE) this is RED with `NotCanonical`;
    /// post-flip it is GREEN. A plain encode→decode round-trip does NOT
    /// discriminate — this asserts the *production* wire is canonical dag-cbor.
    #[test]
    fn share_token_url_is_canonical_dag_cbor() {
        use base64::Engine as _;
        let kp = test_keypair();
        let token = sample_token(&kp);
        let encoded = token.to_base64url(&kp).unwrap();
        let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(&encoded)
            .unwrap();
        let wire: EmbedAsBytes = fauna_cbor::decode_strict(&bytes)
            .expect("share-token URL must be strict canonical dag-cbor embed-as-bytes");
        // The inner signed bytes verify against the author and decode back to
        // the same content.
        let (inner, env) = wire.into_signed().unwrap();
        let decoded: ShareToken = encoding::decode_signed_bytes(&inner).unwrap();
        encoding::verify_envelope(&decoded, &inner, &env)
            .expect("embedded sign-over-CID envelope must verify");
        assert_eq!(decoded.filename, token.filename);
        assert_eq!(decoded.manifest_hash, token.manifest_hash);
        assert_eq!(decoded.author, token.author);
    }

    #[test]
    fn to_url_builds_correct_url() {
        let kp = test_keypair();
        let token = sample_token(&kp);
        let url = token.to_url(&kp, "https://nest.fauna.social").unwrap();
        assert!(url.starts_with("https://nest.fauna.social/share/"));
        // Extract the token portion and decode it (decode verifies the sig).
        let token_str = url
            .strip_prefix("https://nest.fauna.social/share/")
            .unwrap();
        let decoded = ShareToken::from_base64url(token_str).unwrap();
        assert_eq!(decoded.filename, token.filename);
    }

    #[test]
    fn token_id_is_deterministic_and_matches_blake3_of_wire() {
        use base64::Engine as _;
        let kp = test_keypair();
        let token = sample_token(&kp);
        let encoded = token.to_base64url(&kp).unwrap();

        // Stable across calls.
        let id1 = token_id_from_base64url(&encoded).unwrap();
        let id2 = token_id_from_base64url(&encoded).unwrap();
        assert_eq!(id1, id2);

        // Equals blake3 of the base64url-decoded canonical wire bytes — the
        // value the GET path recomputes from the URL segment.
        let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(&encoded)
            .unwrap();
        assert_eq!(id1, *blake3::hash(&bytes).as_bytes());

        // Invalid base64url is an error, not a panic.
        assert!(token_id_from_base64url("not valid base64!!").is_err());
    }

    /// The token field is additive in both directions: a fragment-keyed token
    /// round-trips with the declaration; a public one round-trips without it
    /// AND is never written with it, so its signed bytes (hence its registry
    /// id) are what a writer without the field produces — decoded here through a
    /// struct that has no such field, as an older nest would.
    #[test]
    fn key_in_fragment_round_trips_and_public_tokens_never_carry_it() {
        #[derive(Serialize)]
        struct PreFieldToken {
            #[serde(with = "serde_bytes")]
            manifest_hash: [u8; 32],
            author: ActorId,
            filename: String,
            expires: u64,
            public: bool,
        }
        let kp = test_keypair();
        let private = ShareToken::fragment_keyed([0xCD; 32], kp.actor_id(), u64::MAX);
        let decoded = ShareToken::from_base64url(&private.to_base64url(&kp).unwrap()).unwrap();
        assert!(decoded.key_in_fragment);
        assert!(decoded.public);
        assert!(decoded.filename.is_empty(), "the name rides the envelope");

        let public = sample_token(&kp);
        assert!(
            !ShareToken::from_base64url(&public.to_base64url(&kp).unwrap())
                .unwrap()
                .key_in_fragment
        );
        let pre = PreFieldToken {
            manifest_hash: public.manifest_hash,
            author: public.author,
            filename: public.filename.clone(),
            expires: public.expires,
            public: public.public,
        };
        assert_eq!(
            encoding::canonical_encode(&public).unwrap(),
            encoding::canonical_encode(&pre).unwrap(),
            "a public token's content bytes must be exactly the pre-field shape"
        );
        // And a field-less token's bytes decode as a public link.
        let old: ShareToken =
            encoding::canonical_decode(&encoding::canonical_encode(&pre).unwrap()).unwrap();
        assert!(!old.key_in_fragment);
    }

    fn sealed_file(root: &[u8; 32], plain: &[u8]) -> (KeyEnvelope, Vec<Vec<u8>>) {
        use crate::data::ContentHash;
        let parts: Vec<&[u8]> = plain.chunks(700).collect();
        let hashes: Vec<ContentHash> = parts.iter().map(|p| ContentHash::of_raw(p)).collect();
        let bodies = parts
            .iter()
            .zip(&hashes)
            .map(|(p, h)| crate::chunk_seal::seal_chunk_body(h, p, root).unwrap().1)
            .collect();
        let env = KeyEnvelope::derive(
            root,
            "holiday.jpg".into(),
            ContentHash::of_raw(plain),
            hashes,
        );
        (env, bodies)
    }

    #[test]
    fn an_envelope_opens_the_file_under_its_link_key_only() {
        let root = [0x33u8; 32];
        let plain: Vec<u8> = (0..2000u32).map(|i| (i * 31 % 251) as u8).collect();
        let (env, bodies) = sealed_file(&root, &plain);
        let key = generate_link_key();
        let sealed = env.seal(&key).unwrap();

        let opened = KeyEnvelope::open(&key, &sealed).unwrap();
        assert_eq!(opened, env);
        assert_eq!(opened.filename, "holiday.jpg");
        assert_eq!(opened.open_file(&bodies).unwrap(), plain);
        // The sealed bytes carry no plaintext name and no chunk key.
        assert!(!sealed.windows(7).any(|w| w == b"holiday"));
        assert!(
            !sealed
                .windows(32)
                .any(|w| w == env.chunk_keys[0].as_slice())
        );

        // Wrong key, tampered byte: fail closed.
        assert!(KeyEnvelope::open(&generate_link_key(), &sealed).is_err());
        let mut tampered = sealed.clone();
        *tampered.last_mut().unwrap() ^= 1;
        assert!(KeyEnvelope::open(&key, &tampered).is_err());
        assert!(KeyEnvelope::open(&key, &[]).is_err());
    }

    #[test]
    fn open_file_refuses_a_swapped_or_missing_chunk() {
        let root = [0x33u8; 32];
        let plain: Vec<u8> = (0..2000u32).map(|i| (i * 7 % 253) as u8).collect();
        let (env, mut bodies) = sealed_file(&root, &plain);
        assert!(bodies.len() >= 2);
        assert!(env.open_file(&bodies[1..]).is_err(), "a chunk short");
        bodies.swap(0, 1);
        assert!(env.open_file(&bodies).is_err(), "chunks out of order");
    }

    #[test]
    fn a_link_key_round_trips_through_the_fragment() {
        let key = generate_link_key();
        let fragment = encode_link_key(&key);
        assert!(!fragment.contains('='));
        assert_eq!(decode_link_key(&fragment).unwrap(), key);
        assert_eq!(decode_link_key(&format!("#{fragment}")).unwrap(), key);
        assert!(decode_link_key("c2hvcnQ").is_err());
    }

    #[test]
    fn is_audio_video_follows_the_content_type_map() {
        // Every audio/video row of the map answers true, case-insensitively.
        for name in [
            "clip.mp4",
            "clip.webm",
            "clip.ogv",
            "clip.mov",
            "song.mp3",
            "song.OGG",
            "song.flac",
            "song.wav",
            "song.m4a",
        ] {
            assert!(is_audio_video_filename(name), "{name} is AV");
        }
        // Images, documents, unknown and extension-less are not.
        for name in [
            "photo.jpg",
            "notes.txt",
            "doc.pdf",
            "binary.xyz",
            "Makefile",
        ] {
            assert!(!is_audio_video_filename(name), "{name} is not AV");
        }
    }

    #[test]
    fn content_type_common_extensions() {
        assert_eq!(content_type_for_filename("document.pdf"), "application/pdf");
        // Case-insensitive
        assert_eq!(content_type_for_filename("photo.PNG"), "image/png");
        assert_eq!(content_type_for_filename("data.json"), "application/json");
        assert_eq!(content_type_for_filename("archive.zip"), "application/zip");
        // Unknown extension
        assert_eq!(
            content_type_for_filename("binary.xyz"),
            "application/octet-stream"
        );
        // No extension
        assert_eq!(
            content_type_for_filename("Makefile"),
            "application/octet-stream"
        );
        // docx
        assert_eq!(
            content_type_for_filename("report.docx"),
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document"
        );
    }
}
