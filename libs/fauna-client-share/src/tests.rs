use super::*;
use fauna_client_testkit::{RejectingRequester, block_on};
use fauna_core::chunk::ChunkManifest;
use fauna_core::data::ContentHash;
use fauna_core::encoding::{self, Signed};
use fauna_core::identity::ActorId;
use fauna_protocol::RpcError;

const SECRET: [u8; 32] = [7u8; 32];
const NOW: u64 = 1_800_000_000;
const NEST: &str = "https://nest.example/";

fn author() -> ShareAuthor {
    ShareAuthor::new(SECRET, NEST)
}

fn file() -> LinkFile {
    LinkFile {
        manifest_hash: [0xAB; 32],
        filename: "holiday.jpg".into(),
    }
}

/// The record the nest would answer for `request`.
fn record_for(request: &ShareCreateRequest) -> ShareRecord {
    let token = ShareToken::from_base64url(&request.token).unwrap();
    ShareRecord {
        token_id: hex::encode(token_id_from_base64url(&request.token).unwrap()),
        manifest_hash: hex::encode(token.manifest_hash),
        expires_at: token.expires as i64,
        public: token.public,
        key_in_fragment: token.key_in_fragment,
        revoked: false,
        created_at: NOW as i64,
        filename_sealed: request.filename_sealed.clone(),
        ..Default::default()
    }
}

fn reply_for(request: &ShareCreateRequest) -> ShareCreateReply {
    ShareCreateReply {
        share: record_for(request),
        extra: Default::default(),
    }
}

// ── expiry, eligibility, states ─────────────────────────────────────────────

#[test]
fn the_four_expiries_default_to_seven_days_and_offer_no_never() {
    let values: Vec<_> = EXPIRY_OPTIONS.iter().map(|o| o.value).collect();
    assert_eq!(values, ["1d", "7d", "30d", "1y"]);
    assert_eq!(expiry_secs(DEFAULT_EXPIRY), Some(DEFAULT_EXPIRY_SECS));
    assert_eq!(expiry_secs("never"), None);
    assert_eq!(expiry_secs("1y"), Some(365 * 24 * 3600));
}

#[test]
fn only_a_public_audience_folder_is_eligible() {
    assert!(share_link_eligible(&ShareFolderFacts {
        public_audience: true,
        ..Default::default()
    }));
    assert!(!share_link_eligible(&ShareFolderFacts::default()));
}

#[test]
fn state_derives_from_revoked_expiry_and_the_clock() {
    let rec = |expires_at, revoked| ShareRecord {
        expires_at,
        revoked,
        ..Default::default()
    };
    assert_eq!(link_state(&rec(100, false), 99), LinkState::Active);
    assert_eq!(link_state(&rec(100, false), 100), LinkState::Expired);
    assert_eq!(link_state(&rec(100, true), 99), LinkState::Revoked);
    assert_eq!(link_state(&rec(100, true), 200), LinkState::Revoked);
    assert_eq!(LinkState::Active.as_str(), "active");
}

// ── mint + reveal ───────────────────────────────────────────────────────────

#[test]
fn a_minted_link_signs_the_file_and_seals_its_name_under_the_token_id() {
    let minted = mint_link(&author(), &file(), DEFAULT_EXPIRY_SECS, NOW).unwrap();
    let req = minted.request();
    let token = ShareToken::from_base64url(&req.token).unwrap();
    assert_eq!(token.manifest_hash, [0xAB; 32]);
    assert_eq!(token.filename, "holiday.jpg");
    assert_eq!(token.expires, NOW + DEFAULT_EXPIRY_SECS);
    assert!(token.public && !token.key_in_fragment);
    assert_eq!(token.author, ActorKeypair::from_secret(SECRET).actor_id());

    // The seal opens under the author's owner root, salted by the token id.
    let token_id = token_id_from_base64url(&req.token).unwrap();
    let keys = FileDownloadKeys::owner(BackupKey::derive(&SECRET));
    assert_eq!(
        label_custody::render_share_filename(&keys, &req.filename_sealed, &token_id),
        SealedLabelRender::Sealed("holiday.jpg".into())
    );
}

#[test]
fn the_url_is_revealed_only_against_this_tokens_registration() {
    let minted = mint_link(&author(), &file(), DEFAULT_EXPIRY_SECS, NOW).unwrap();
    let other = mint_link(&author(), &file(), DEFAULT_EXPIRY_SECS, NOW + 1).unwrap();
    // A reply for a different token is not proof of this one's registration.
    let other_reply = reply_for(other.request());
    let token = minted.request().token.clone();
    let this_reply = reply_for(minted.request());
    assert_eq!(
        mint_link(&author(), &file(), DEFAULT_EXPIRY_SECS, NOW)
            .unwrap()
            .reveal(&other_reply),
        None
    );
    assert_eq!(
        minted.reveal(&this_reply),
        Some(format!("https://nest.example/share/{token}"))
    );
}

#[test]
fn an_overflowing_expiry_is_refused_before_anything_is_signed() {
    assert_eq!(
        mint_link(&author(), &file(), u64::MAX, NOW).err(),
        Some(MintError::Expiry)
    );
}

// ── the verified re-derivation ──────────────────────────────────────────────

#[test]
fn copy_re_derives_the_exact_registered_url() {
    // Ed25519 is deterministic, so the re-mint from the row's fields is the
    // very token that was registered.
    let minted = mint_link(&author(), &file(), DEFAULT_EXPIRY_SECS, NOW).unwrap();
    let record = record_for(minted.request());
    let revealed = minted.reveal(&ShareCreateReply {
        share: record.clone(),
        extra: Default::default(),
    });
    assert!(revealed.is_some());
    assert_eq!(link_url(&author(), &record, "holiday.jpg"), revealed);
}

/// A token a FUTURE client minted — the same fields plus one this client
/// does not know. Its id is not a function of the fields a list row returns,
/// so the re-mint cannot reproduce it: Copy is absent, never a wrong link.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct FutureToken {
    #[serde(with = "serde_bytes")]
    manifest_hash: [u8; 32],
    author: ActorId,
    filename: String,
    expires: u64,
    public: bool,
    audience: String,
}

impl Signed for FutureToken {
    fn signer_public_key(&self) -> &[u8; 32] {
        &self.author.0
    }
}

#[test]
fn a_token_with_a_field_this_client_does_not_know_gets_no_copy() {
    use base64::Engine as _;
    let kp = ActorKeypair::from_secret(SECRET);
    let future = FutureToken {
        manifest_hash: [0xAB; 32],
        author: kp.actor_id(),
        filename: "holiday.jpg".into(),
        expires: NOW + DEFAULT_EXPIRY_SECS,
        public: true,
        audience: "friends".into(),
    };
    let bytes = encoding::sign_and_pack(&kp, &future).unwrap();
    let token = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes);
    let record = ShareRecord {
        token_id: hex::encode(token_id_from_base64url(&token).unwrap()),
        manifest_hash: hex::encode([0xAB; 32]),
        expires_at: (NOW + DEFAULT_EXPIRY_SECS) as i64,
        public: true,
        ..Default::default()
    };
    assert_eq!(link_url(&author(), &record, "holiday.jpg"), None);
}

#[test]
fn a_fragment_keyed_or_foreign_link_never_re_derives() {
    let minted = mint_link(&author(), &file(), DEFAULT_EXPIRY_SECS, NOW).unwrap();
    let record = record_for(minted.request());
    // Its key lives only in the URL the author copied.
    let private = ShareRecord {
        key_in_fragment: true,
        ..record.clone()
    };
    assert_eq!(link_url(&author(), &private, "holiday.jpg"), None);
    // Another author's re-mint does not reproduce the id.
    let stranger = ShareAuthor::new([9u8; 32], NEST);
    assert_eq!(link_url(&stranger, &record, "holiday.jpg"), None);
    // A wrong name does not either.
    assert_eq!(link_url(&author(), &record, "other.jpg"), None);
}

// ── list rows ───────────────────────────────────────────────────────────────

#[test]
fn rows_render_the_sealed_name_and_copy_only_active_rows() {
    let a = author();
    let active = record_for(
        mint_link(&a, &file(), DEFAULT_EXPIRY_SECS, NOW)
            .unwrap()
            .request(),
    );
    let revoked = ShareRecord {
        revoked: true,
        ..record_for(mint_link(&a, &file(), 30 * DAY, NOW).unwrap().request())
    };
    let expired = record_for(
        mint_link(&a, &file(), DAY, NOW - 2 * DAY)
            .unwrap()
            .request(),
    );
    let rows = link_rows(&a, &[active.clone(), revoked, expired], NOW as i64);
    assert_eq!(rows.len(), 3);
    assert!(rows.iter().all(|r| r.filename == "holiday.jpg"));
    assert_eq!(
        rows.iter().map(|r| r.state).collect::<Vec<_>>(),
        [LinkState::Active, LinkState::Revoked, LinkState::Expired]
    );
    assert!(rows[0].url.is_some());
    assert!(rows[1].url.is_none() && rows[2].url.is_none());
    assert_eq!(rows[0].token_id, active.token_id);
}

#[test]
fn a_row_this_reader_cannot_open_is_omitted() {
    let a = author();
    // Sealed under someone else's root: omitted.
    let stranger = ShareAuthor::new([9u8; 32], NEST);
    let foreign = record_for(mint_link(&stranger, &file(), DAY, NOW).unwrap().request());
    assert!(link_rows(&a, &[foreign], NOW as i64).is_empty());
}

// ── the typed calls ─────────────────────────────────────────────────────────

/// Answers `fauna.share.create` for whatever token it is sent.
struct EchoRegistry;

impl RpcRequester for EchoRegistry {
    type Error = String;

    async fn request<Req, Reply>(&self, kind: &'static str, payload: Req) -> Result<Reply, String>
    where
        Req: serde::Serialize,
        Reply: serde::de::DeserializeOwned,
    {
        assert_eq!(kind, "fauna.share.create");
        let bytes = fauna_protocol::encode_canonical(&payload).unwrap();
        let req: ShareCreateRequest = fauna_protocol::decode_strict(&bytes).unwrap();
        let reply = fauna_protocol::encode_canonical(&reply_for(&req)).unwrap();
        Ok(fauna_protocol::decode_strict(&reply).unwrap())
    }
}

#[test]
fn create_link_reveals_the_url_after_a_successful_registration() {
    let client = ShareClient::new(EchoRegistry);
    let (record, url) = block_on(client.create_link(&author(), &file(), DEFAULT_EXPIRY_SECS, NOW))
        .expect("created");
    assert!(url.starts_with("https://nest.example/share/"));
    assert_eq!(link_url(&author(), &record, "holiday.jpg"), Some(url));
}

#[test]
fn a_failed_registration_yields_no_url() {
    let client = ShareClient::new(RejectingRequester::new().reject(
        "fauna.share.create",
        RpcError::new("fauna.share.invalid_request", "error.share.invalid_request"),
    ));
    let err = block_on(client.create_link(&author(), &file(), DEFAULT_EXPIRY_SECS, NOW))
        .expect_err("refused");
    assert!(matches!(err, CreateError::Rpc(_)));
}

// ── the fragment-keyed private-link arm ─────────────────────────────────────

/// A two-chunk file, its chunks sealed as an owner-only set seals them
/// (`convergent_chunk_root`), and its manifest in the sealed wire form the nest
/// hands back — `file_hash`/`chunk_hashes` blank until unsealed.
fn sealed_file(secret: [u8; 32]) -> (Vec<u8>, ChunkManifest, Vec<Vec<u8>>) {
    let content: Vec<u8> = (0..90_000u32).map(|i| (i % 251) as u8).collect();
    let root = BackupKey::derive(&secret).convergent_chunk_root();
    let mut hashes = Vec::new();
    let mut stored = Vec::new();
    let mut ciphertexts = Vec::new();
    for c in content.chunks(50_000) {
        let hash = ContentHash::of_raw(c);
        let (store_key, body) = fauna_core::chunk_seal::seal_chunk_body(&hash, c, &root).unwrap();
        hashes.push(hash);
        stored.push(store_key);
        ciphertexts.push(body);
    }
    let manifest = ChunkManifest {
        file_hash: ContentHash::of_raw(&content),
        total_size: content.len() as u64,
        chunk_sizes: content.chunks(50_000).map(|c| c.len() as u64).collect(),
        chunk_hashes: hashes,
        stored_hashes: Some(stored),
        sealed_hashes: None,
        min_reader: None,
    }
    .seal_hashes(&root)
    .unwrap();
    (content, manifest, ciphertexts)
}

/// Open a minted private link the way the viewer does: the envelope the nest
/// stores, under the key from the revealed URL's fragment, over the stored
/// ciphertext.
fn open_as_viewer(minted: MintedLink, ciphertexts: &[Vec<u8>]) -> (String, Vec<u8>) {
    let request = minted.request().clone();
    let url = minted.reveal(&reply_for(&request)).expect("revealed");
    let (_, fragment) = url.split_once('#').expect("a fragment");
    let envelope = open_envelope(fragment, request.key_envelope.as_ref().unwrap()).unwrap();
    let bytes = envelope.open_file(ciphertexts).unwrap();
    (envelope.filename, bytes)
}

#[test]
fn an_unbound_folder_whose_owner_root_the_seat_holds_is_eligible() {
    assert!(share_link_eligible(&ShareFolderFacts {
        unbound: true,
        owner_root_held: true,
        ..Default::default()
    }));
    // A bound folder (shared with named people) is not, root or no root.
    assert!(!share_link_eligible(&ShareFolderFacts {
        owner_root_held: true,
        ..Default::default()
    }));
    // Nor an unbound folder this seat cannot open.
    assert!(!share_link_eligible(&ShareFolderFacts {
        unbound: true,
        ..Default::default()
    }));
}

#[test]
fn a_private_link_carries_its_key_only_in_the_fragment_and_opens_the_file() {
    let (content, manifest, ciphertexts) = sealed_file(SECRET);
    let minted = mint_private_link(&author(), &file(), manifest, DEFAULT_EXPIRY_SECS, NOW).unwrap();
    let request = minted.request().clone();

    // The token declares the fragment key and names no file.
    let token = ShareToken::from_base64url(&request.token).unwrap();
    assert!(token.key_in_fragment && token.public);
    assert_eq!(token.filename, "");
    assert_eq!(token.manifest_hash, file().manifest_hash);
    // The name still rides the sealed list field, under the token id.
    let id = token_id_from_base64url(&request.token).unwrap();
    assert_eq!(
        label_custody::render_share_filename(&author().read_keys(), &request.filename_sealed, &id),
        SealedLabelRender::Sealed("holiday.jpg".into())
    );

    let url = minted.reveal(&reply_for(&request)).expect("revealed");
    let (path, fragment) = url.split_once('#').expect("a fragment");
    assert_eq!(
        path,
        format!("https://nest.example/share/{}", request.token)
    );

    // The envelope the nest stores opens under the fragment's key alone, and
    // decrypts the stored ciphertext to exactly the file.
    let sealed = request.key_envelope.as_ref().expect("an envelope");
    let envelope = open_envelope(fragment, sealed).unwrap();
    assert_eq!(envelope.filename, "holiday.jpg");
    assert_eq!(envelope.open_file(&ciphertexts).unwrap(), content);
    // Nothing the nest is sent carries the key.
    let key = decode_link_key(fragment).unwrap();
    assert!(!request.token.contains(fragment));
    assert!(!sealed.windows(LINK_KEY_LEN).any(|w| w == key));
    // A wrong key opens nothing.
    assert!(open_envelope(&fauna_core::share::encode_link_key(&[1; 32]), sealed).is_err());
}

#[test]
fn a_private_link_reveals_only_against_its_own_registration() {
    let (_, manifest, _) = sealed_file(SECRET);
    let minted = mint_private_link(&author(), &file(), manifest.clone(), DAY, NOW).unwrap();
    let other = mint_private_link(&author(), &file(), manifest, DAY, NOW + 1).unwrap();
    assert!(minted.reveal(&reply_for(other.request())).is_none());
}

#[test]
fn a_plaintext_manifest_or_a_foreign_root_mints_no_private_link() {
    let (_, sealed, _) = sealed_file(SECRET);
    let root = BackupKey::derive(&SECRET).convergent_chunk_root();
    let mut plaintext = sealed.unseal_hashes(&root).unwrap();
    plaintext.stored_hashes = None;
    assert!(matches!(
        mint_private_link(&author(), &file(), plaintext, DAY, NOW).err(),
        Some(MintError::Manifest(_))
    ));
    // A file sealed under someone else's root does not open under this seat's.
    let (_, foreign, _) = sealed_file([9u8; 32]);
    assert!(matches!(
        mint_private_link(&author(), &file(), foreign, DAY, NOW).err(),
        Some(MintError::Manifest(_))
    ));
}

#[test]
fn a_file_sealed_before_succession_links_under_the_predecessor_root() {
    let predecessor = [5u8; 32];
    let (content, manifest, ciphertexts) = sealed_file(predecessor);
    let a = author().with_predecessors(vec![BackupKey::derive(&predecessor)]);
    let minted = mint_private_link(&a, &file(), manifest, DAY, NOW).unwrap();
    assert_eq!(
        open_as_viewer(minted, &ciphertexts),
        ("holiday.jpg".to_string(), content)
    );
}

#[test]
fn create_private_link_reveals_the_fragment_url_after_registration() {
    let (_, manifest, _) = sealed_file(SECRET);
    let client = ShareClient::new(EchoRegistry);
    let (record, url) = block_on(client.create_private_link(
        &author(),
        &file(),
        manifest,
        DEFAULT_EXPIRY_SECS,
        NOW,
    ))
    .expect("created");
    assert!(url.contains('#'));
    // The list cannot re-copy it — the key is not on the nest.
    assert!(record.key_in_fragment);
    assert_eq!(link_url(&author(), &record, "holiday.jpg"), None);
}

// ── the viewer side (`viewer`) ──────────────────────────────────────────────

/// A registered private link as the viewer meets it: the token from the URL
/// path, the key from its fragment, the manifest path's canonical answer, and
/// the stored ciphertext chunks.
struct ServedLink {
    content: Vec<u8>,
    token: String,
    fragment: String,
    fragment_manifest: Vec<u8>,
    ciphertexts: Vec<Vec<u8>>,
}

fn served_link(secret: [u8; 32], filename: &str) -> ServedLink {
    let (content, manifest, ciphertexts) = sealed_file(secret);
    let raw = encoding::canonical_encode(&manifest).unwrap();
    let linked = LinkFile {
        manifest_hash: ContentHash::of_raw(&raw).digest(),
        filename: filename.into(),
    };
    let a = ShareAuthor::new(secret, NEST);
    let minted = mint_private_link(&a, &linked, manifest, DAY, NOW).unwrap();
    let request = minted.request().clone();
    let url = minted.reveal(&reply_for(&request)).expect("revealed");
    let (_, fragment) = url.split_once('#').expect("a fragment");
    let fragment_manifest = fauna_protocol::encode_canonical(&share::ShareFragmentManifest {
        manifest: serde_bytes::ByteBuf::from(raw),
        key_envelope: request.key_envelope.clone().expect("an envelope"),
        extra: Default::default(),
    })
    .unwrap()
    .to_vec();
    ServedLink {
        content,
        token: request.token,
        fragment: fragment.to_string(),
        fragment_manifest,
        ciphertexts,
    }
}

#[test]
fn the_viewer_starts_only_on_a_share_path_with_a_fragment() {
    use viewer::{ViewerStart, viewer_start};
    let open = ViewerStart::Open {
        token: "abc".into(),
        fragment: "k3y".into(),
    };
    assert_eq!(viewer_start("/share/abc", "#k3y"), open);
    assert_eq!(viewer_start("/share/abc/", "k3y"), open);
    // An unfurler's fetch carries no fragment: the generic page.
    assert_eq!(viewer_start("/share/abc", ""), ViewerStart::Generic);
    assert_eq!(viewer_start("/share/abc", "#"), ViewerStart::Generic);
    // Anything but `/share/<token>` is not a link.
    assert_eq!(
        viewer_start("/app/share-viewer.html", "#k"),
        ViewerStart::Generic
    );
    assert_eq!(viewer_start("/share/", "#k"), ViewerStart::Generic);
    assert_eq!(
        viewer_start("/share/a/manifest", "#k"),
        ViewerStart::Generic
    );
}

#[test]
fn the_viewers_requests_are_same_origin_paths_without_the_key() {
    assert_eq!(viewer::manifest_path("tok"), "/share/tok/manifest");
    assert_eq!(viewer::chunk_path("tok", 3), "/share/tok/chunk/3");
}

#[test]
fn the_viewer_opens_a_private_link_to_exactly_the_file() {
    let link = served_link(SECRET, "holiday-plan.txt");
    let opened =
        viewer::open_share(&link.token, &link.fragment, &link.fragment_manifest).expect("opens");
    assert_eq!(opened.filename(), "holiday-plan.txt");
    assert_eq!(opened.total_size(), link.content.len() as u64);
    assert_eq!(opened.size_text(), "87.9 KB");
    assert_eq!(opened.chunk_count(), link.ciphertexts.len());
    assert_eq!(opened.content_type(), "text/plain");
    assert_eq!(opened.preview(), viewer::PreviewKind::Text);
    assert_eq!(opened.assemble(&link.ciphertexts).unwrap(), link.content);
}

#[test]
fn the_viewer_fails_closed_on_a_wrong_key_or_altered_bytes() {
    use viewer::{ViewerError, open_share};
    let link = served_link(SECRET, "a.txt");
    let damaged = Some(ViewerError::Damaged);
    // A wrong key, and a truncated one.
    let wrong = fauna_core::share::encode_link_key(&[1; 32]);
    assert_eq!(
        open_share(&link.token, &wrong, &link.fragment_manifest).err(),
        damaged
    );
    assert_eq!(
        open_share(&link.token, &link.fragment[..10], &link.fragment_manifest).err(),
        damaged
    );
    // Another link's manifest answer under this link's token: not the
    // manifest the signed token names.
    let other = served_link([9; 32], "b.txt");
    assert_eq!(
        open_share(&link.token, &link.fragment, &other.fragment_manifest).err(),
        damaged
    );
    // A public link's token is not a private link; garbage is no link.
    let public = ShareToken::new(
        [1; 32],
        author().keypair.actor_id(),
        "a.txt".into(),
        NOW + DAY,
        true,
    )
    .to_base64url(&author().keypair)
    .unwrap();
    let not_a_link = Some(ViewerError::NotALink);
    assert_eq!(
        open_share(&public, &link.fragment, &link.fragment_manifest).err(),
        not_a_link
    );
    assert_eq!(
        open_share("not-a-token", &link.fragment, &link.fragment_manifest).err(),
        not_a_link
    );
    // An altered chunk, a missing one, or chunks out of order.
    let opened = open_share(&link.token, &link.fragment, &link.fragment_manifest).unwrap();
    let mut altered = link.ciphertexts.clone();
    altered[0][20] ^= 1;
    assert_eq!(opened.assemble(&altered).err(), damaged);
    assert_eq!(opened.assemble(&link.ciphertexts[..1]).err(), damaged);
    let mut reordered = link.ciphertexts.clone();
    reordered.reverse();
    assert_eq!(opened.assemble(&reordered).err(), damaged);
}

/// Rule 2: only what a browser renders without executing anything previews
/// inline, decided over the one type oracle — never a second extension list.
#[test]
fn only_images_sound_video_and_plain_text_preview_inline() {
    use viewer::{PreviewKind, preview_kind};
    for (name, kind) in [
        ("a.jpg", PreviewKind::Image),
        ("a.PNG", PreviewKind::Image),
        ("a.gif", PreviewKind::Image),
        ("a.webp", PreviewKind::Image),
        ("a.mp3", PreviewKind::Audio),
        ("a.mp4", PreviewKind::Video),
        ("a.webm", PreviewKind::Video),
        ("a.txt", PreviewKind::Text),
        ("a.svg", PreviewKind::None),
        ("a.html", PreviewKind::None),
        ("a.htm", PreviewKind::None),
        ("a.xml", PreviewKind::None),
        ("a.md", PreviewKind::None),
        ("a.csv", PreviewKind::None),
        ("a.css", PreviewKind::None),
        ("a.js", PreviewKind::None),
        ("a.pdf", PreviewKind::None),
        ("a.json", PreviewKind::None),
        ("a.zip", PreviewKind::None),
        ("noextension", PreviewKind::None),
    ] {
        assert_eq!(preview_kind(name), kind, "{name}");
    }
}

#[test]
fn the_viewer_says_each_refusal_plainly() {
    use fauna_i18n::strings::share_viewer as s;
    assert_eq!(viewer::status_text(410), s::GONE);
    assert_eq!(viewer::status_text(451), s::WITHHELD);
    assert_eq!(viewer::status_text(404), s::NOT_FOUND);
    assert_eq!(viewer::status_text(500), s::UNAVAILABLE);
    assert_eq!(viewer::ViewerError::Damaged.text(), s::DAMAGED);
}
