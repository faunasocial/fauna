//! **The opaque carriers the blob-name walk cannot see — walked by declared
//! column TYPE, not by name.**
//!
//! [`super::every_blob_shaped_column_declares_whether_it_holds_actor_ids`]
//! asks of every column the schema *calls* a blob whether its bytes name
//! actors. Its declared residual was every carrier the schema calls something
//! else — `record`, `statement`, `envelope`, `payload`, `witness`, the
//! `*_sealed` and `encrypted_*` families — and no word list closes that: the
//! carriers share no vocabulary, so each added fragment catches a handful and
//! reads as coverage of the rest. What they *do* share is enumerable without a
//! vocabulary at all: the type the schema declares for them. So this walk
//! takes every `BLOB`-typed column in the live schema and requires it to be
//! accounted for.
//!
//! **Two populations are somebody else's and are subtracted, not re-ruled:**
//! a column whose NAME reads as a person belongs to the census walk
//! (`every_actor_shaped_column_is_registered_or_excluded` — registered or
//! excluded there, so re-listing it here would be a second owner for one
//! claim), and a column whose name contains a blob fragment belongs to the blob
//! walk. Everything left is this walk's.
//!
//! **Lives beside `actor_tables.rs` rather than in it** because that file is
//! already several times the whole-file read ceiling and this walk's lists are
//! the longest the registry has; a child module of the test module still sees
//! its helpers.
//!
//! Owner: `succession-repoint-axis.md` § The declared re-point axis.

use super::*;

/// BLOB columns outside the other two walks whose CONTENTS name actors, each
/// with what deletion and succession do to the ids inside.
///
/// **The ordering rule, which the blob walk never had to state:** a carrier's
/// ruling FOLLOWS the row ruling its table already has in `ACTOR_TABLES` — so
/// a carrier on a table with NO registry entry (the `room_policy_versions`
/// shape, a table the census never opens because no column name reads as a
/// person) needs its ROW ruled first. There is nothing for the bytes to follow
/// until then, and an entry here that invents a row verdict in passing is a
/// ruling made in the wrong registry. **The rule binds only a column whose
/// bytes name an actor:** a digest, row id or deployment key on an unregistered
/// table needs no row verdict invented for it (`bridge_tls_cert_blobs.blob` is the
/// blob walk's precedent) and goes straight to [`NON_ACTOR_OPAQUE_COLUMNS`].
///
/// **A fixed-width id that turns out to be a PERSON does not leave this walk
/// by being registered.** The walk subtracts only what `actor_shaped` or a blob
/// fragment claims by NAME — registering the column, or declaring it a
/// vocabulary blind spot, hides it from nothing here
/// (`segment_records.scope_id` is registered, a declared blind spot, and still
/// seen). So such a column either gets `ACTOR_SHAPED_ROOTS` widened to claim it
/// (after checking what the substring pulls in across the schema), or stays
/// declared HERE, pointing at its census entry.
///
/// **What "names an actor" means for this list, since the first drain had to
/// decide it:** the bytes *structurally* carry an identifier an account can be
/// found by — an actor id, a per-account public key, a DID, a mail address —
/// in the clear or under a seal. Free text a user typed (a calendar's display
/// name, a mail subject) is not that, even though a person's name can be typed
/// into it; a word model trained over whole messages IS, because the address
/// headers are always in the training text.
const ACTOR_BEARING_OPAQUE_COLUMNS: &[(&str, &str, &str)] = &[
    // ── The atproto plane.
    (
        "atproto_authoring_keys",
        "cert",
        "the identity-signed `DeviceAuthorization` delegating the account's authoring \
         sub-key, as plaintext embed-as-bytes the nest decodes on every delegated write: \
         it names the row's OWN actor id (the nest refuses a cert naming anyone but the \
         caller) and the sub-key. FOLLOWS THE ROW -- Purge on deletion, Burn on \
         succession, and Burn is exactly right for the bytes: the cert authorizes the \
         key against the PREDECESSOR's identity key, so there is nothing to re-point",
    ),
    (
        "atproto_authoring_keys",
        "k_pub",
        "the per-account Ed25519 authoring sub-key, minted nest-side one per actor and \
         bound to the owner's identity by `cert` beside it; it rides in the signer \
         authority of every post an external app writes, so it is a stable pseudonymous \
         identifier of the row's own actor. FOLLOWS THE ROW (Purge / Burn) -- the \
         successor's first fetch mints a fresh one",
    ),
    (
        "atproto_native_records",
        "record",
        "the journaled repo records -- every collection with no Fauna concept (follow, \
         like, repost, block, list item, threadgate, third-party NSIDs) -- stored as \
         PLAINTEXT dag-cbor exactly as the bridge validated it. The one carrier of this \
         plane that names OTHER people: a follow or block carries its subject's DID and a \
         like or reply an `at://did:...` URI, and nothing stops that DID being an account \
         this nest hosts (`atproto_identities` resolves it). The ROW follows its ruling \
         (Purge; Move with the DID's family); the BYTES STAY as written, for two reasons \
         that hold together: the row's `cid` is the CID of these bytes, the address the \
         record was published under, so an edited subject is a different record; and the id \
         inside is a DID, which a succession carries to the successor unchanged -- so \
         there is nothing to re-point. A deleted subject leaves a record naming a DID \
         that no longer resolves, which is what a follow of a deleted account is on \
         every PDS",
    ),
    (
        "atproto_preferences",
        "preferences",
        "the body of `putPreferences`, a PLAINTEXT JSON array the nest stores verbatim \
         and never parses: saved feeds and labeler subscriptions are `at://` URIs and \
         DIDs of other accounts, beside free-text muted words. FOLLOWS THE ROW (Purge; \
         Move) and the bytes stay, for `atproto_native_records.record`'s second reason \
         -- what is named is a DID, stable across the named account's succession -- and \
         nothing resolves through them nest-side at all",
    ),
    // ── The forward queue.
    (
        "outbox",
        "payload",
        "the verbatim signed embed-as-bytes `Post` or `Tombstone` awaiting forward to the paired \
         public nest: it names its AUTHOR, every actor the post mentions, and -- on a delegated \
         write -- the authoring sub-key and its cert. FOLLOWS THE ROW on deletion, by the row's \
         own predicate (`outbox` is `Policy::Partial`: everything the deleted author \
         queued goes except a tombstone that can still be sent). On succession the row's finder \
         stamp `author_id` MOVES and these bytes STAY as written, so the two deliberately \
         disagree afterwards: the payload is signed under the retired key and the peer verifies \
         it against the author the bytes name, so an edited author is an unverifiable forward. \
         A MENTIONED actor's id is reached by nothing, exactly as in the stored post it copies",
    ),
    (
        "abuse_report_outbox",
        "payload",
        "a queued `fauna.federation.abuse_report.*` request (canonical dag-cbor): a `deliver` \
         names the reported subject and its author (`subject_actor`, a third party) and carries \
         the reporter's note and excerpt but NEVER the reporter's id -- that never crosses a \
         nest boundary; a `withdraw` or an `outcome` carries only a report ref and an outcome \
         word. The queue has no actor column: an entry follows the report its `report_id` \
         names (`abuse_reports`, reporter `Retain`), and leaves when sent, refused, or at the \
         seven-day retry ceiling. The author's id STAYS as written on succession, exactly as \
         `abuse_reports.subject_actor` does -- the report is about the identity that was \
         reported",
    ),
    // ── The DAV plane. One shape, said once: every column below is a
    // `MailRecordEnvelope` HPKE-sealed by the owner's own client (the MDA
    // session or a Fauna app) to the owner's OWN recipient or index key. The
    // nest verifies the envelope's structure and never opens it; the plaintext
    // header is `{v, kind, hpke{suite, ephemeral key, ct}}` and names nobody.
    // So each FOLLOWS THE ROW (Purge; Move with the three-table sync unit) and
    // the ids inside are reachable by no nest-side walk, by construction.
    (
        "bridge_caldav_events",
        "encrypted_index_hint",
        "the sealed search-token set of the whole iCalendar text, attendee and organizer \
         address fragments included (a Fauna app seals an empty one). FOLLOWS THE ROW",
    ),
    (
        "bridge_caldav_events",
        "encrypted_fauna_ext",
        "the sealed `FaunaEventExt` sidecar, and the one DAV carrier that holds a Fauna \
         ACTOR ID rather than an address: `organizer_actor_id` (64-hex) + \
         `organizer_home_nest_url`, the principal a later inbound CANCEL or updating \
         REQUEST must come from, beside attendee address -> nest-url hints. It names \
         ANOTHER person, under the owner's seal, so no ceremony can reach it. FOLLOWS \
         THE ROW. ⚠ What the ORGANIZER's succession does: the bytes STAY and the \
         client resolves through them -- when the plain comparison \
         (`SchedulingPrincipal::matches`) misses and the nest half still matches, it \
         asks `PrincipalResolver::resolve_successor` for the bound identity's final \
         VERIFIED successor, fetched from the bound nest, and an admitted update \
         re-binds the sidecar (`caldav-server.md` § Who may mutate an existing event \
         over the inbound rail -> *A succeeded organizer*). Never a rewrite from here. \
         Both shipped resolvers perform that walk (native: `NestSchedulingSink`'s, \
         over `fauna_client_recovery::witness::walk_at_nest_url`; web: \
         `WebOrganizerSuccessionDialer`; both behind the shared, per-session \
         memoizing `MemoizedSuccessionResolver`), pinned end to end by \
         `a_succeeded_organizers_cancel_is_honoured_through_the_production_sink`. \
         Where the walk cannot answer -- an unreachable bound nest, an unreadable \
         config -- the cancel is refused `NotTheOrganizer`, the SAFE direction (a \
         comparand that can only match a dead identity admits no one, \
         `channel_commit_watermark.last_commit_sender`'s shape): a stranded event, \
         not a hole",
    ),
    (
        "bridge_carddav_cards",
        "encrypted_index_hint",
        "the sealed search tokens of FN + every EMAIL + every TEL. FOLLOWS THE ROW",
    ),
    (
        "bridge_carddav_cards",
        "encrypted_fauna_ext",
        "the CardDAV sidecar, RESERVED: the schema documents its payload as the \
         `X-FAUNA-ACTOR-ID` linkage, the nest preserves it across a MUA edit, and no \
         production writer produces one today (the MDA never does; the client only \
         reports its presence). Declared bearing on the documented payload rather than \
         on today's empty population, so the first writer inherits a ruling instead of \
         a silence. FOLLOWS THE ROW, sealed as its twins",
    ),
    // ── Import and spam.
    (
        "import_sessions",
        "source_sealed",
        "the import's source descriptor (provider + host + the user's account name \
         there), sealed client-side under the owner's label root; the `SealedLabel` \
         header is `{v, gen, nonce, ct}` and names nobody, and the nest holds no root on \
         this plane. It names the row's OWN actor, at a foreign provider. FOLLOWS THE \
         ROW (Purge; Move)",
    ),
    (
        "spam_models",
        "model_json",
        "the owner's Bayesian model, SEALED by the writing capability holder (the \
         owner's client or AUTH'd MDA session; a `MailRecordEnvelope` to the owner's own \
         key): opaque, header names nobody, and `put_spam_model` refuses a plaintext \
         model. Inside the seal it is a map of 1/2/3-word n-grams -> counts over the \
         WHOLE RFC 5322 text, so it names third parties to the owner's own mail, as \
         that mail does. FOLLOWS THE ROW (Purge; Move). Nothing resolves through a \
         token, so a named correspondent's deletion or succession has nothing here to \
         re-point",
    ),
    (
        "spam_training_history",
        "model_delta_applied",
        "one training event's distinct n-gram set, sealed to the owner's own key by \
         the writing capability holder (`put_spam_model` refuses a plaintext delta). \
         FOLLOWS THE ROW (Purge; Move); rows also age out on the retention sweep",
    ),
    (
        "spam_model_holder_copies",
        "sealed_copy",
        "a `SpamModelCopyBlob` sealed by the contributor's client to the aggregation \
         holder's key: the PLAINTEXT, AEAD-bound index is the contributing owner's actor \
         id -- the row's own `actor_id` a second time, which the nest stores without \
         comparing the two. FOLLOWS THE ROW -- Purge on deletion, Burn on succession -- \
         so the id never outlives the row, and Burn is right for the bytes too: the \
         nest cannot re-seal a copy it cannot open, and the successor's agent seals a \
         fresh one under its own id on its next write",
    ),
    (
        "spam_baseline",
        "model_json",
        "the deployment's merged baseline: always plaintext, the n-gram SUM of every \
         opted-in contributor's model, published by an admin and withdrawn to empty \
         below three contributors. It bears in `spam_models.model_json`'s weak sense \
         only -- a correspondent's address tokens can survive a sum -- and the sum does \
         not say which contributor supplied them. The table is a singleton keyed by no \
         actor, so it can take no ACTOR_TABLES row and this entry carries its ruling \
         (`room_policy_versions`' shape): WITHDRAWN WHOLE ON A CONTRIBUTOR'S DEPARTURE. \
         A sum cannot give one contributor's counts back and names none of them (the \
         never-served `spam_baseline_inclusions` record beside it does), so when a \
         summed contributor (one the last served publish summed) deletes their \
         account, opts out, resets their model or revokes the spam-model grant, the row \
         is deleted at once and serves nothing until an admin publish rebuilds it from \
         the contributors then standing. On deletion `purge_orphaned_actor_rows` \
         withdraws BEFORE it deletes the two rows that say the actor contributed. A \
         succession is NOT a departure -- the model and the opt-in Move, the same \
         person stands as the same contributor, and the sum is left alone. Ruling: \
         `mail-spam.md` § Cold start, Path 2 -> *A contributor's departure withdraws \
         the baseline*",
    ),
    (
        "content",
        "payload",
        "the row's body, in whichever shape its plane stores (empty on the current \
         post path, where the body rests in the segment store): a SIGNED post whose \
         first field is its author, under that author's signature; an inbox envelope \
         the nest encodes itself, which can name the sharer's actor id and handle or \
         carry a signed room invite whole; a takedown witness naming the author; and \
         a bridge row's plaintext JSON, which names no actor id but does hold the \
         mail's sender and recipient ADDRESSES. FOLLOWS THE ROW on deletion — \
         `content.author` is `Retain` because deletion routes through the \
         federation-aware per-post retraction, and the bytes go when that retraction \
         removes the row. On succession the ROW moves and the BYTES STAY as written, \
         for `current_key_blobs.blob_data`'s reason: a signed post naming the \
         predecessor is true and unforgeable, and re-pointing the id would break the \
         signature every reader verifies",
    ),
    (
        "room_invites",
        "signed_invite",
        "the inviter's canonical `SignedRoomInvite`, stored verbatim: it names the \
         INVITEE and the INVITER a second time, under the inviter's Ed25519 signature \
         (the invite door verified it and bound its signer to the authenticated caller \
         before storing it). Written once and read back by NO nest-side statement -- \
         evidence at rest, and what a cross-nest leg will carry unchanged. FOLLOWS THE \
         ROW, which the ordering rule above kept waiting until `room_invites` had a row \
         ruling to follow: Purge with the deleted invitee; on succession the ROW moves \
         and the BYTES STAY as signed, the `rooms.owner_id` / `policy_blob` split -- \
         re-pointing either id would break the signature. So a moved row's bytes name \
         the PREDECESSOR as invitee, and a future verifier that compares that name \
         with the accepting principal must resolve it through the succession chain, \
         the floor's rule for every name in a signed policy, never by plain equality. \
         A DELETED inviter's id rests here and in `inviter_id` until the row goes \
         (`SUCCESSION_REFERENCES`, that column's entry). Its sibling ids left this \
         walk the day the census vocabulary gained `inviter` / `invitee`",
    ),
    (
        "room_members",
        "reception_pubkey",
        "a PUBLIC KEY OF THE ROW'S OWN ACTOR, the sync section's bearer rule below: the \
         member's X-Wing group-reception key, the wrap target, per-account and \
         deliberately reused across rooms, so equal bytes on two rosters are one account \
         (re-ruled 2026-09-22 from the non-actor list, where the reason already said \
         so). FOLLOWS THE ROW, `principal_id`'s: Purge on deletion; on succession \
         Partial, and the key is why the floor leg seats the successor with NO wrap \
         target -- it derives from the seed the ceremony retires -- while the \
         predecessor's Removed-absorbed row keeps it as history, and a MIRROR seat \
         stays with its key",
    ),
    (
        "labelers",
        "labeler_id",
        "the SAME BYTES as the row's registered `publisher_actor` — the artifact's \
         self-signed `algorithm_id` key (`publish_labeler_core` writes both from one \
         value), by that column's ruling never an enrolled account. FOLLOWS IT: Stay, \
         and its deletion caveat with it",
    ),
    (
        "labeler_list_entries",
        "labeler_id",
        "the List artifact's `algorithm_id` key — the SAME BYTES as \
         `labelers.labeler_id`, on a DERIVED projection of that row's `wasm_bytes` \
         (decoded at publish, replaced in the publish transaction, recreatable from the \
         artifact). The table has no registry entry and needs none invented: its row \
         IS ruled, by the table it projects — it FOLLOWS `labelers` (Stay; that entry's \
         deletion caveat with it), and by that ruling the key is never an enrolled \
         account, so no ceremony or deletion ever names it",
    ),
    (
        "labeler_subscriptions",
        "labeler_id",
        "the subscribed labeler's `algorithm_id` key — a reference to a published, \
         fetchable-by-anyone artifact, not to the row's owner. The row is \
         `owner_actor`'s (Purge / Move); this id names the COUNTERPARTY artifact and \
         is untouched by either, which is right: the successor is subscribed to the \
         same labeler",
    ),
    //
    // ── The sync, backup, custody, recovery and nest planes, and everything else outside the
    // conversation, curation, bridge and mail planes. Alphabetical. A PUBLIC KEY OF THE ROW'S OWN
    // ACTOR is ruled a bearer here -- a per-account key names its holder exactly as an id does --
    // and follows its row.
    (
        "actor_epoch_seal_keys",
        "mlkem_ek",
        "a PUBLIC KEY OF THE ROW'S OWN ACTOR, which names a person exactly as an id does: the \
         1184-byte ML-KEM-768 encapsulation key of one weekly mail-sealing epoch, derived from the \
         actor's MSEK and published by their own client (a user caller may provision only itself). \
         FOLLOWS THE ROW: Purge on deletion; Stay on succession, where staying is the row's \
         load-bearing ruling -- the keys derive from what the thief read, and a retired address \
         with no fresh key is what arms the succession-pending tempfail",
    ),
    (
        "actor_epoch_seal_keys",
        "mls_pubkey",
        "the 32-byte X25519 HPKE half of the same epoch key, same derivation, same actor. FOLLOWS \
         THE ROW (Purge / Stay), for `mlkem_ek`'s reason",
    ),
    (
        "actor_index_pubkeys",
        "index_pubkey",
        "DARK: a 32-byte public key of the row's own actor that the MTA would seal an index hint \
         to -- no production writer, tests pre-seed it. FOLLOWS THE ROW (Purge / Stay), whose \
         ruling is itself conditional on the table staying dark; whoever adds the producer \
         re-checks whether the private half derives from the MSEK",
    ),
    (
        "actor_mls_pubkeys",
        "mlkem_ek",
        "the actor's STANDING ML-KEM-768 encapsulation key (1184 bytes, MSEK-derived, \
         self-registered at enable-mail). A public key of the row's own actor. FOLLOWS THE ROW \
         (Purge / Stay) -- the registry's own warning applies to the bytes as much as to the row: \
         do NOT finish the job by deleting these",
    ),
    (
        "actor_mls_pubkeys",
        "mls_pubkey",
        "the standing 32-byte X25519 HPKE recipient key, \
         `derive_key(RECIPIENT_HPKE_DERIVE_CONTEXT, msek)`. FOLLOWS THE ROW (Purge / Stay), for \
         `mlkem_ek`'s reason",
    ),
    (
        "actor_successions",
        "statement",
        "the verbatim `SignedIdentitySuccession`: it names `old_actor_id`, `new_actor_id` and the \
         old identity's `recovery_pubkey`, under the RecoveryKey's and the successor's signatures. \
         The two ids are also the row's plain columns, and the bytes follow their ruling: Retain \
         on deletion (the succession audit trail must outlive both accounts) and Stay -- this is \
         the chain every other carrier's 'resolved through the chain' reads, public by \
         construction, replayed byte-for-byte to whoever asks, and unverifiable if edited",
    ),
    (
        "custody_hosting",
        "witness",
        "the verbatim owner-signed `CustodyGrant`: it names the custodied OWNER (its signer) -- a \
         third party, not the host whose row this is -- and `custodian_key`, which at this door \
         must equal this nest's own key. The owner is also the plain column `owner_actor_id`, \
         ruled Stay in SUCCESSION_REFERENCES in words that are about these very bytes: the witness \
         dies with the retired key's trust and the re-mint sweep replaces the ceremony wholesale, \
         so following the pointer would forge a hosting row. FOLLOWS THAT: the bytes stay as \
         signed; the row goes with the HOST (Purge / Burn); an owner's deletion leaves a witness \
         that fails closed at the owner-side handshake until the host's reconcile stops it",
    ),
    (
        "custody_receipts_staged",
        "receipt",
        "the verbatim custodian-signed `CustodyReceipt`: it names `owner` -- the row's OWN actor \
         -- and the attesting `custodian_key` (a device or nest principal, never an account). \
         FOLLOWS THE ROW: Purge on deletion; Burn on succession, where a staged receipt could \
         never verify against the successor's re-minted accept anyway",
    ),
    (
        "device_authorizations",
        "payload",
        "the canonical `DeviceAuthorization` of the subscription plane's `ManageSubscribers` \
         delegation: its first field is the row's OWN actor (forced equal to the bearer at upload) \
         and the record is that actor's signed act; its `device_key` is this nest's key. FOLLOWS \
         THE ROW: Purge; Burn -- the registry burns the row precisely because these bytes name the \
         PREDECESSOR and a move would be known-wrong. Already served unauthenticated by the \
         delegate route, so retention exposes nothing",
    ),
    (
        "email_filters",
        "rules",
        "PLAINTEXT canonical `Vec<EmailFilterRule>`, read by the MTA: `SenderIs { address }` and a \
         `From` `HeaderContains` name CORRESPONDENTS by mail address -- people, though never actor \
         ids, so there is nothing for a re-point to rewrite (a local correspondent's address \
         follows their account by the alias legs, not by this blob). FOLLOWS THE ROW: Purge on \
         deletion; on succession the Partial leg deletes forwarding and auto-reply rows and moves \
         the rest with the rules untouched, which is right -- they are the owner's own sorting \
         preferences",
    ),
    // Member seats trust the folder channel's recorded owner, which a succession does not
    // re-point — that is the
    // readers' question, not this column's; the ruling below is what the bytes are.
    (
        "folders",
        "audience_attestation",
        "the canonical `AudienceAttestation` (v75): an Ed25519 signature by the folder OWNER's \
         identity key over the owner's actor id, the folder id, its name and a counter, and it \
         carries that owner's actor id in the clear -- the row's own `actor_id`. The nest stores \
         and serves it opaquely and no nest decision reads it; it is kept across a flip-back on \
         purpose, so the next mint counts above it. FOLLOWS THE ROW: Purge on deletion; Move(Plain) \
         on succession, and the BYTES STAY as written, because they are a signature under the \
         PREDECESSOR's key -- an edited owner is a forgery, not a re-point. The seat's verifier \
         rebuilds the signed message from the owner IT trusts and never reads the carried one, so \
         on the owner's own seats a moved attestation stops verifying and the folder seals until \
         the successor re-confirms `public`: the safe direction",
    ),
    (
        "foreign_recovery_heads",
        "recovery_pubkey",
        "the RecoveryKey public half of a FOREIGN person at the chain head this nest last verified \
         -- a key of the row's own `actor_id`, which here names an identity this nest does not \
         host. FOLLOWS THE ROW: Stay (it is the anti-downgrade watermark; moving it erases the \
         baseline in the act of using it), and Purge is reachable only if that foreign id ever \
         becomes local",
    ),
    (
        "forward_queue",
        "raw_message",
        "a PLAINTEXT RFC 5322 message held for relay: its headers name senders and recipients by \
         address, the forwarding account's own among them. People, never actor ids -- nothing to \
         re-point. FOLLOWS THE ROW: Purge on deletion; on succession the Partial leg burns \
         forward-all rows and moves the rest, bytes untouched. Bounded by the queue's FIFO ceiling \
         and by promotion",
    ),
    (
        "key_packages",
        "key_package_data",
        "a TLS-serialized MLS KeyPackage (its wire shape is the MLS dependency's; what OUR code \
         asserts before storing is that the leaf credential equals the leaf signature key equals \
         the uploader's actor id). So the bytes name the row's OWN actor, as credential and as \
         key. FOLLOWS THE ROW: Purge; Burn, for the row's own reason -- the package embeds the \
         predecessor's credential and its private half is seed-derived",
    ),
    (
        "knocks",
        "payload",
        "the verbatim arrival body, a signed pair `(ContactRequest, Post)`: both halves name the \
         KNOCKER (`sender` == `author`, enforced before storage) under the knocker's signatures, \
         plus any actors the post mentions; empty when the body exceeded the hold budget. The \
         knocker is also the plain column `sender_id`, a Move(Bespoke) reference. The row moves \
         with the RECIPIENT and `sender_id` follows a succeeded knocker, but the BYTES STAY as \
         signed, for the room plane's reason: a re-pointed id is a body nobody can verify. After a \
         knocker's succession the body names the predecessor, which is true -- the predecessor \
         signed it. Deletion purges the recipient's rows; a deleted knocker's body rides until \
         accept, block or the age sweep",
    ),
    (
        "notifications",
        "content_id",
        "a caller-defined DEDUP TOKEN, heterogeneous by `notif_type`: a post digest, a row id, a \
         `day:category` string -- and two arms that name someone. The family contact-ask arm \
         stores a THIRD party: the 32-byte actor id of the peer a ward asked to add (the row's \
         recipient is the guardian, its `sender_id` the ward), followed by a row id. The Bluesky \
         arm stores the causing record's AT-URI, whose authority is the counterparty's DID. \
         FOLLOWS THE ROW: Purge on deletion, Move with the recipient -- and nothing else ever \
         deletes a notification, so the named peer's id rides in the guardian's row through that \
         peer's own deletion or succession. Nothing rewrites it, and nothing should: the token's \
         job is de-duplication against a freshly built one, so a stale peer id costs at most a \
         second doorbell for the same ask, and what the guardian is shown comes from the family \
         ask itself, not from this token",
    ),
    (
        "obligation_action_records",
        "obligation_id",
        "a second PERSON COLUMN on a registered table, invisible to the census by name: the \
         ISSUING ADMIN's 32-byte actor id on every legal-takedown record (the type agrees -- \
         `ObligationActionRecord.obligation_id: ActorId`), read back into the author's \
         moderation-actions list. The row is keyed by `author_hex` (Retain / Move): records are \
         never deleted. The admin id STAYS through the admin's own deletion and succession, which \
         is the attribution reading of Stay -- this is who issued a legal action, a fact about the \
         retired identity, and an audit record whose issuer could be re-pointed answers a \
         different question. No root reaches it (`obligation` is not a role), so this entry is its \
         declaration",
    ),
    (
        "outbound_mail_queue",
        "raw_message",
        "a PLAINTEXT RFC 5322 message awaiting dispatch: addresses of the submitting account and \
         its recipients, never actor ids (the TLS-report rows are the machine-authored exception). \
         FOLLOWS THE ROW, which is Retain -- a queued message is an obligation to third parties \
         and drains by dispatch or bounce -- and Partial on succession: undispatched forward-all \
         rows burn, the rest are re-attributed with the bytes untouched",
    ),
    (
        "recovery_pending_replacements",
        "new_recovery_pubkey",
        "the RecoveryKey public half the row's own actor proposes to register, denormalized out of \
         `record`. A key of the row's actor. FOLLOWS THE ROW: Purge; Burn inside the ceremony \
         transaction",
    ),
    (
        "recovery_pending_replacements",
        "record",
        "the verbatim `SignedRecoveryKeyRegistration`: it names the row's OWN `actor_id` and \
         `recovery_pubkey` under the identity seed's signature. FOLLOWS THE ROW (Purge / Burn); \
         stored byte-for-byte because the landing sweep appends exactly what was signed",
    ),
    (
        "recovery_registrations",
        "record",
        "the same signed registration, one link of the identity's PUBLIC registration chain \
         (served to anyone who asks). Names the row's own actor and recovery key. FOLLOWS THE ROW: \
         Purge on deletion; Stay on succession, for the row's stated reason -- three consumers \
         read the chain under the retired id, and a moved head would block the successor's own \
         first registration",
    ),
    (
        "recovery_registrations",
        "recovery_pubkey",
        "the recovery public key denormalized out of `record`. FOLLOWS THE ROW (Purge / Stay)",
    ),
    (
        "rpc_idempotency",
        "reply",
        "the canonical reply body of ANY successful non-read RPC on an authenticated connection \
         (64 KiB cap): so it names whoever that reply named, including THIRD PARTIES -- the \
         invite-approval reply carries the approved user's actor id and handle on the approving \
         admin's row. The row follows its caller (Purge / Stay), which cannot reach a third \
         party's id inside it; what bounds THAT is the seven-day retention sweep, and it is the \
         load-bearing half of this ruling: lengthening that window lengthens how long a deleted \
         account's id survives in other callers' replay cache. Never exported, for the registry's \
         own reason",
    ),
    (
        "segment_records",
        "scope_id",
        "NOT a carrier: the table's registered PERSON COLUMN, here only because a rename erased \
         its root -- the raw actor id for the mail, post, calendar and card kinds, a channel id \
         for conv. Its ruling is its ACTOR_TABLES entry (Purge / Move(Plain), both kind-selective \
         only because a channel id can never equal an actor id) and its blindness is declared in \
         VOCABULARY_BLIND_SPOTS. Registration does not hide a column from a type walk, so it is \
         declared here too rather than deleted from the backlog",
    ),
    (
        "subscribe_requests",
        "mlkem_encaps_key",
        "the requesting SUBSCRIBER's ML-KEM-768 encapsulation key, carried on a \
         pending `subscribe` request until approval copies it onto the roster row -- \
         the same key `subscribers.mlkem_encaps_key` below is ruled a bearer for \
         (re-ruled 2026-09-22 from the non-actor list). A key of `subscriber_id`, not \
         of the registered `author_id`, and it FOLLOWS `subscriber_id`: the bespoke \
         move NULLs it as it re-points the reader, and on the READER's deletion the \
         purge walk's `subscribe_requests_purge_for_deleted_subscriber` leg deletes \
         every pending `subscribe` row, the only rows that carry one. The author's \
         deletion purges the row whole",
    ),
    (
        "subscribers",
        "mlkem_encaps_key",
        "the SUBSCRIBER's ML-KEM-768 encapsulation key, derived from the subscriber's identity \
         seed (its X25519 twin IS their actor id). A key of `subscriber_id`, not of the row's \
         registered `author_id`, and it FOLLOWS `subscriber_id`'s reference ruling, which already \
         names it: the bespoke move NULLs this column as it re-points the subscriber, because the \
         key derives from the predecessor's seed. The row is Retain, so a deleted subscriber's key \
         rides until unsubscribe or prune -- a wrap target nobody holds",
    ),
    (
        "sync_changes",
        "device_id",
        "normally a device id, which names no account (see the device-id note below) -- but one \
         value is a PURE FUNCTION OF AN ACTOR ID: the WebDAV pseudo-device, `derive_key(.., \
         actor_id)`, which the mail bridge writes for the account it serves. Anyone holding a \
         candidate actor id can confirm such a row. FOLLOWS THE ROW: Purge on deletion; on Move \
         the row becomes the successor's and this value still derives from the PREDECESSOR -- true \
         attribution of a past write, and nothing compares it to the successor (the bridge derives \
         a fresh pseudo-device from the new id). Re-homed backup rows carry the nest's own key \
         here",
    ),
    (
        "sync_changes",
        "entry_sealed",
        "a client-sealed account-state envelope whose one cleartext header is a key-GENERATION \
         id -- but its PLAINTEXT can name ANOTHER person (re-ruled 2026-09-22 from the non-actor \
         list, whose reason covered only the header): a `fauna.state.custodies-held` value is a \
         `CustodyHeld` whose `owner` is the custodied account's actor id, beside that owner's \
         signed witness, under the HOLDER's seal. The shape of `custody_hosting.witness` and \
         `bridge_caldav_events.encrypted_fauna_ext`, and ruled the same way. FOLLOWS THE ROW, \
         which is the holder's: Purge on the holder's deletion; Move(Plain) on the holder's \
         succession with the bytes untouched, since the nest holds no key to re-seal. The OWNER's \
         deletion or succession reaches nothing here by construction -- no nest-side walk can \
         read a seal -- so the entry names the retired owner until the holder's client retires \
         it, and its witness dies with the retired key's trust exactly as the host-side copy \
         does",
    ),
    (
        "sync_conflicts",
        "details_sealed",
        "a sealed label over machine-authored conflict text, which can embed a device id -- and \
         one device id is a PURE FUNCTION OF AN ACTOR ID: the client writes `declined a remote \
         delete from device {d}`, and `d` can be the WebDAV pseudo-device of the account whose \
         rail issued the delete, `sync_changes.device_id` above (re-ruled 2026-09-22 from the \
         non-actor list, whose reason said `never an account's`). Sealed by the owner's client, \
         so it is confirmable only by that client, and minor for it. The table has no registry \
         entry: the row hangs off `folders` by `folder_id ... ON DELETE CASCADE` and FOLLOWS \
         that row (Purge; Move(Plain) -- the id never changes, so the row goes where its folder \
         goes), and the derived id inside keeps deriving from the predecessor, true attribution \
         of a past delete, compared to nothing",
    ),
    (
        "sync_devices",
        "auth_grant",
        "the verified wire `DeviceAuthorization`: the account's actor id in cleartext, the device \
         key it authorizes, under the account ROOT key's signature -- the strongest actor-naming \
         bytes on the sync plane. FOLLOWS THE ROW: Purge; Stay, and the row's ruling is about \
         these bytes -- a stored authorization names the predecessor and would fail its own \
         `auth.actor_id` check on the successor, so registrations die with the old identity and \
         are re-created at fleet re-import",
    ),
    (
        "sync_devices",
        "device_id",
        "a device id, or the device principal's key on a self-registered placeholder -- and the \
         WebDAV pseudo-device, a pure function of the row's OWN actor id (`sync_changes.device_id` \
         above). FOLLOWS THE ROW (Purge / Stay): the derived value never outlives or leaves the \
         account it derives from",
    ),
    (
        "sync_changes",
        "signer_key",
        "the key the writer signature verifies under: a device principal's public key -- or, \
         for a DIRECT signature, the writer's own ACTOR ID (`change_signature.rs`). FOLLOWS THE \
         ROW: Purge; Move(Plain) with the bytes untouched, because they are inside what was \
         signed for -- re-pointing them would break the signature, and a predecessor's id here \
         is true attribution of a past write (the chain's succession crossing is \
         `mls-group-key-material.md` ruling (8))",
    ),
    (
        "sync_signer_certs",
        "cert",
        "a delegated signer's verified wire `DeviceAuthorization`: the account's actor id in \
         cleartext and the device key, under the account ROOT key's signature -- \
         `sync_devices.auth_grant`'s bytes, remembered for the list replies' side table. FOLLOWS \
         THE ROW: Purge; Move(Plain) with the bytes untouched (the row's own ruling says why: \
         the cert follows the moved `sync_changes` rows it verifies and still names the \
         predecessor). The actor it names is mirrored in cleartext as `cert_actor_id`, the \
         key part that never moves (`SUCCESSION_REFERENCES`, Stay; ruling (8)(i)). A foreign \
         writer's inline cert names a remote account, never reached by either leg",
    ),
    //
    // ── Carriers whose ROW was ruled after the drain that found them (2026-09-21): each was
    // held on the backlog until its table gained the `ACTOR_TABLES` entry it now follows.
    // (`membership_tiers.admin_id`, held with them, left this walk instead: the `admin` root
    // it needed makes it the census walk's.)
    (
        "feature_policies",
        "subject_id",
        "NOT a carrier: the table's registered PERSON COLUMN -- the account a self-imposed \
         feature policy binds, EMPTY on the nest-wide tiers -- under a noun no root can afford \
         (`subject` is the mail plane's word). Its ruling is its ACTOR_TABLES entry (Purge / \
         Move(Plain): a self-exclusion follows the person, and a tighten-only tier is safe to \
         move even out of a compromise) and its blindness is declared in \
         VOCABULARY_BLIND_SPOTS. Declared here because registration hides a column from \
         nothing in a type walk",
    ),
    (
        "worker_replication",
        "payload_key",
        "NOT a carrier: the table's registered column, POLYMORPHIC by `payload_type` -- the \
         inbox RECIPIENT's raw actor id on `inbox` rows, a post content id on the rest. Its \
         ruling is its ACTOR_TABLES entry (Purge / Stay), whose statements are kind-selective \
         only because a content id can never equal an actor id; a declared vocabulary blind \
         spot. The replica the marker describes rests on the paired worker under the same id \
         and is not this nest's to delete",
    ),
];

/// BLOB columns outside the other two walks that name no actor, with why —
/// a fixed-width digest, key, nonce or signature says so in a phrase; a
/// structured value says what was read to establish it.
const NON_ACTOR_OPAQUE_COLUMNS: &[(&str, &str, &str)] = &[
    (
        "nest_replica",
        "replica_id",
        "the singleton 16-byte id `seed_genesis_rows` mints once from `getrandom` \
         with the database; it names THIS nest's replica so a bound peer can key its \
         watermark, never an account or any actor",
    ),
    (
        "third_party_principals",
        "holder_x25519",
        "an external app's attested X25519 PUBLIC key (32 bytes, length-checked at \
         PAR) -- the key capability grants to it are sealed to; names an app, never \
         an account",
    ),
    (
        "third_party_principals",
        "writer_ed25519",
        "an external app's attested Ed25519 PUBLIC writer key (32 bytes) -- the \
         writer a `content.write` grant names; names an app, never an account",
    ),
    (
        "atproto_consent_requests",
        "holder_x25519",
        "the requesting app's attested X25519 PUBLIC key (32 bytes), carried on the \
         consent card until the user answers; names an app, never an account",
    ),
    (
        "atproto_consent_requests",
        "writer_ed25519",
        "the requesting app's attested Ed25519 PUBLIC writer key (32 bytes), carried \
         on the consent card until the user answers; names an app, never an account",
    ),
    (
        "plugin_state",
        "value",
        "a hosted plugin's own working state behind its `state` import, opaque to the \
         nest and deleted with the plugin's principal row; no production writer exists \
         yet (only the store's own tests call `plugin_state_put`), so the host that \
         wires the import re-rules this column if a plugin may keep actor ids in it",
    ),
    (
        "folder_deposit_inbox",
        "sealed",
        "a `DepositEnvelope` (file name, media type, bytes -- all content) HPKE-sealed by \
         the deposit door to the folder owner's registered recipient key -- ciphertext \
         only the owner's client opens; it names no actor. The depositor is the column \
         beside it, a principal handle the census excludes",
    ),
    (
        "third_party_principals",
        "publisher_key",
        "a hosted plugin publisher's PUBLIC signing key, copied from its manifest; \
         names a publisher, never an account (NULL on every row today)",
    ),
    // ── Row ids of the mail-routing plane: every one is minted as a fresh
    // `Uuid::new_v4()` at its single insert site (or is a copy of one, as a
    // foreign key), derived from nothing. The person on each of these tables,
    // where there is one, is a separate column the census already rules.
    (
        "account_aliases",
        "alias_id",
        "random UUIDv4 row id, minted at each of the five alias writers",
    ),
    (
        "alias_hits",
        "alias_id",
        "the matched alias's row id -- a cascade FK to `account_aliases`",
    ),
    ("alias_hits", "hit_id", "random UUIDv4 row id"),
    (
        "mail_domains",
        "domain_id",
        "random UUIDv4 row id of a DOMAIN (not derived from its name)",
    ),
    ("mail_domain_renames", "rename_id", "random UUIDv4 row id"),
    (
        "mail_domain_renames",
        "old_primary_domain_id",
        "a copy of a `mail_domains.domain_id`",
    ),
    (
        "mail_domain_renames",
        "new_primary_domain_id",
        "a copy of a `mail_domains.domain_id`",
    ),
    ("mail_lists", "list_id", "random UUIDv4 row id"),
    (
        "mail_lists",
        "alias_id",
        "the list's `kind='list'` alias row id, minted in the same transaction",
    ),
    ("mail_list_sends", "send_id", "random UUIDv4 row id"),
    ("mail_list_sends", "list_id", "cascade FK to `mail_lists`"),
    (
        "mail_list_members",
        "member_id",
        "random UUIDv4 row id -- never an account: a member is an ADDRESS \
         (`recipient_address`, excluded by name in the census), and the add door \
         refuses an address on a hosted domain",
    ),
    ("mail_list_members", "list_id", "cascade FK to `mail_lists`"),
    (
        "mail_srs_secrets",
        "secret",
        "the deployment's SRS HMAC key: 32 `getrandom` bytes, seeded at first open and \
         on admin rotation, nothing per-user",
    ),
    (
        "mail_list_unsubscribe_secrets",
        "secret",
        "the deployment's one-click-unsubscribe HMAC key: 32 `getrandom` bytes, as \
         `mail_srs_secrets.secret`",
    ),
    // ── The atproto plane's ids and key material.
    (
        "atproto_authoring_keys",
        "k_secret_wrapped",
        "the secret half of `k_pub`, wrapped under the nest's own deployment-seed KEK: \
         `version || nonce || ciphertext`, no index and no associated data, so nothing \
         in the bytes says whose -- ownership is the row's `actor_id` alone",
    ),
    (
        "atproto_blobs",
        "media_ref",
        "the blake3 content hash of the uploaded media bytes (exactly 32 bytes, \
         length-checked at the door)",
    ),
    (
        "atproto_consent_requests",
        "consent_id",
        "32 `getrandom` bytes",
    ),
    (
        "atproto_oauth_grants",
        "grant_id",
        "16 CSPRNG bytes from whichever authorization server issued the grant (the \
         nest's `mint_jti` or the bridge's) -- bearer-adjacent, since it doubles as the \
         refresh-family id, and naming nobody",
    ),
    (
        "atproto_sessions",
        "session_id",
        "16 CSPRNG bytes (on the OAuth path, the grant id above)",
    ),
    (
        "atproto_sessions",
        "current_refresh_jti",
        "16 CSPRNG bytes, rotated on refresh. The JWT that carries it names the DID and \
         the actor; that token is the client's and never rests in this column",
    ),
    // ── The DAV plane's ids and digests.
    (
        "bridge_caldav_calendars",
        "calendar_id",
        "client-derived: unkeyed `blake3(slug)` of the collection's URL segment \
         (`dav_identity::collection_id`; `personal` for the lazy default), or a 64-hex \
         segment decoded as-is. A collection NAME's digest -- guessable by design, and \
         no principal is in the preimage",
    ),
    (
        "bridge_caldav_calendars",
        "encrypted_metadata",
        "display name, colour and description, sealed by the owner's client to the \
         owner's own key: free text, no structured id, never opened by the nest",
    ),
    (
        "bridge_caldav_events",
        "calendar_id",
        "as `bridge_caldav_calendars.calendar_id`",
    ),
    (
        "bridge_caldav_events",
        "event_id",
        "nest-derived at ingest: domain-tagged blake3 over (actor id, timestamp, sealed \
         body). The actor id is a preimage input to a one-way digest and already sits in \
         the row's own `actor_id` column, so the digest says nothing the row does not",
    ),
    (
        "bridge_caldav_events",
        "uid_hash",
        "UNKEYED `blake3(UID)` -- pinned unsalted on purpose, because the Go MDA applies \
         a bare blake3 to the same key. It holds no id, but it is a confirmation oracle, \
         not a secret: whoever holds a candidate UID (some MUAs mint `<token>@<host>`) \
         can test it against the row. The plaintext UID rests only inside the sealed body",
    ),
    (
        "bridge_caldav_events",
        "record_cid",
        "the CID of the canonical segment record, i.e. a hash over CIPHERTEXT only -- \
         and HPKE is randomized per seal, so not even an equality handle across owners",
    ),
    (
        "bridge_caldav_expunged",
        "calendar_id",
        "as `bridge_caldav_calendars.calendar_id`",
    ),
    (
        "bridge_caldav_expunged",
        "event_id",
        "the dead row's `event_id`, kept for the RFC 6578 sync report",
    ),
    (
        "bridge_caldav_expunged",
        "uid_hash",
        "the dead row's `uid_hash`, oracle caveat included; no body, hint, sidecar or \
         plaintext UID survives into a tombstone",
    ),
    (
        "bridge_carddav_addressbooks",
        "addressbook_id",
        "the same `collection_id` rule as a calendar id (`contacts` for the lazy default)",
    ),
    (
        "bridge_carddav_addressbooks",
        "encrypted_metadata",
        "display name and description, sealed to the owner's own key: free text, as its \
         CalDAV twin",
    ),
    (
        "bridge_carddav_cards",
        "addressbook_id",
        "as `bridge_carddav_addressbooks.addressbook_id`",
    ),
    (
        "bridge_carddav_cards",
        "card_id",
        "`bridge_caldav_events.event_id`'s twin under its own domain tag",
    ),
    (
        "bridge_carddav_cards",
        "uid_hash",
        "unkeyed `blake3(vCard UID)` -- one rule for both rails, same oracle caveat",
    ),
    (
        "bridge_carddav_cards",
        "record_cid",
        "the CID of the canonical contacts-segment record: a hash over ciphertext",
    ),
    (
        "bridge_carddav_expunged",
        "addressbook_id",
        "as `bridge_carddav_addressbooks.addressbook_id`",
    ),
    (
        "bridge_carddav_expunged",
        "card_id",
        "the dead row's `card_id`",
    ),
    (
        "bridge_carddav_expunged",
        "uid_hash",
        "the dead row's `uid_hash`, as its CalDAV twin",
    ),
    // ── The rest of the bridge plane.
    (
        "bridge_audit_events",
        "idempotency_hash",
        "SHA-256 over a domain tag + the framed event tuple. Both the bridge principal \
         and the actor id are preimage inputs; the digest is one-way and the row carries \
         `actor_id` in the clear beside it",
    ),
    (
        "bridge_session_close_events",
        "idempotency_hash",
        "`bridge_audit_events.idempotency_hash`'s twin under its own domain tag",
    ),
    (
        "bridge_imap_messages",
        "message_id",
        "the content hash of the SEALED mail record as the segment store filed it -- a \
         digest of ciphertext",
    ),
    (
        "bridge_index_map",
        "content_id",
        "`blake3(\"{content_type}:{natural_id}\")` over a foreign item id (a nostr event \
         id, an ActivityPub object URL): no Fauna actor anywhere in the preimage. The \
         table is live -- the nostr store and the ActivityPub inbox write it",
    ),
    (
        "bridge_index_map",
        "post_id",
        "a content address: the `content.id` of the bridged post the transit point rested \
         (`blake3` of its canonical body), NULL for the relay-store plane. The body names its \
         (synthetic, foreign) author; its hash does not",
    ),
    // `bridge_service_users` is one row per bridge PROCESS keypair (MTA, MDA,
    // content processor, atproto PDS host; `in_process = 1` is the nest's own
    // holder). Its one person is `approved_by_actor_id`, ruled `Stay` in
    // SUCCESSION_REFERENCES. ⚠ The Ed25519 key lives in the same 32-byte
    // principal namespace as an actor id -- it is what every `bridge_actor_id`
    // column holds -- and is never a `users` row: a service principal, naming
    // a process.
    (
        "bridge_service_users",
        "ed25519_pubkey",
        "the bridge process's own identity key, pre-registered by an admin",
    ),
    (
        "bridge_service_users",
        "x25519_pubkey",
        "the bridge process's own seal-target key, bound set-once at self-attestation",
    ),
    (
        "bridge_service_users",
        "mlkem_ek",
        "the bridge process's own ML-KEM encapsulation key, derived from its identity seed",
    ),
    (
        "delivery_receipts",
        "content_hash",
        "a served video segment's content hash",
    ),
    (
        "delivery_receipts",
        "server_nest",
        "a NEST id -- a nest's own identity key, not an account's. Read from the \
         writer's contract alone: `record_peer_delivery_receipt` has no production \
         caller (credit scoring is deferred), so the table is empty outside tests",
    ),
    (
        "delivery_receipts",
        "requesting_nest",
        "a nest id, as `server_nest`",
    ),
    (
        "import_sessions",
        "source_hash",
        "`blake3::derive_key(\"fauna.import-source.v1\", descriptor)` -- a digest, so it \
         holds no id, but UNKEYED over a low-entropy string that contains the user's \
         account name at the source provider: an equality handle and a confirmation \
         oracle for anyone who can guess the descriptor. The descriptor itself rests \
         only in `source_sealed`",
    ),
    (
        "spam_model_holder_copies",
        "holder_pubkey",
        "the aggregation holder's X25519 key -- an approved content-processor (or MDA) \
         bridge service user, the identity `capability_grants.holder_pubkey` keys grants \
         by and documented not an actor. The door checks only its length, so a client \
         could rest 32 arbitrary bytes here; they would pair with no grant and be served \
         to no one",
    ),
    (
        "spam_training_history",
        "history_id",
        "random UUIDv4 row id",
    ),
    (
        "spam_training_history",
        "message_id",
        "the trained message's `bridge_imap_messages.message_id` -- a ciphertext digest",
    ),
    (
        "spam_training_history",
        "sealed_subject",
        "the trained message's subject line, sealed by the writing capability holder \
         to the owner's own key (never empty -- the only place the subject rests): \
         free text, header names nobody, never opened by the nest",
    ),
    // ── The conversation plane (2026-09-21). Four id families recur, so each
    // is derived ONCE here and the entries below point at it:
    //
    // ROOM ID — ceremony-born: `blake3::derive_key` over the canonical
    // `RoomBirthCore { owner, salt }` (`fauna_mls::room_policy`), re-derived by
    // the nest rather than trusted; mirror-born: the MLS channel id. So the
    // ceremony arm is a one-way hash OVER the founder's actor id, salted with
    // 32 random bytes — and the salt rests beside it, which is the point: the
    // row is a checkable statement about `rooms.owner_id`, a registered person
    // column. The id itself names a room.
    //
    // CHANNEL ID — `blake3::derive_key("fauna.channel.v1", mls_group_id)` over
    // a randomly minted group id; never a hash over a channel's members. A
    // community room's channel id IS its room id (above). ⚠ Declared, because
    // it would change this ruling: `DeviceSyncChannel::expected_channel_id` is
    // an UNSALTED hash of one actor id — an enumerable pseudonym for a person —
    // and is dormant today (no production caller). The session that wakes it
    // re-rules every `channel_id` entry here.
    //
    // ROSTER ENTRY ID — `blake3::derive_key` over `RoomEntryCore { room,
    // principal, seated_at_ms }`: a SLOT, derived from the member's actor id
    // and never equal to it; the seating stamp is in the preimage so a
    // re-admission mints a fresh slot and old wraps stay inert. On a first
    // seating it is recomputable from the row's own plaintext columns, and it
    // joins to `room_members.principal_id` — which is a registered person
    // column, so the slot adds nothing the row does not already say.
    //
    // GENERATION ID — `blake3::derive_key` over the canonical `GroupMintCore`,
    // which names the minter and the member ENTRY ids; a digest, with the
    // minter's plaintext twin already ruled as `room_generations.minted_by`.
    ("rooms", "room_id", "a ROOM ID (derived above)"),
    (
        "rooms",
        "birth_salt",
        "32 client-minted random bytes — the other half of the room id's commitment \
         to `owner_id`; NULL on a mirror-born room",
    ),
    ("room_members", "room_id", "a ROOM ID"),
    // The bridged-conversation family (registered 2026-10-03). A bridged
    // room's id is a keyed BLAKE3 derivation over the account, the bridge id
    // and the far room id (`bridged_conversations::bridged_room_id`) — a
    // digest, never equal to the account id, which rides beside it as the
    // registered `actor_id`.
    (
        "bridge_conversation_rooms",
        "room_id",
        "a bridged ROOM ID (derived above)",
    ),
    (
        "bridge_conversation_rooms",
        "bridge_x25519",
        "the bridge principal's attested X25519 public key, snapshotted -- an app's \
         key, never an account's",
    ),
    (
        "bridge_conversation_messages",
        "room_id",
        "a bridged ROOM ID",
    ),
    (
        "bridge_conversation_messages",
        "sealed_content",
        "the message body HPKE-sealed to the account's own recipient key -- \
         ciphertext only the account's client opens; it names no actor",
    ),
    ("bridge_conversation_outbox", "room_id", "a bridged ROOM ID"),
    (
        "bridge_conversation_outbox",
        "ciphertext",
        "the outbound message sealed to the bridge principal's X25519 key -- \
         ciphertext only the bridge opens; it names no actor",
    ),
    (
        "first_party_bridge_keys",
        "secret_wrapped",
        "an in-process leg's own X25519 secret (today the Nostr DM leg's), minted \
         from the OS RNG and wrapped under the deployment seed -- the nest's key \
         for the leg, never an account's",
    ),
    (
        "first_party_bridge_keys",
        "x25519_public",
        "that leg's X25519 public key, what its rooms' `bridge_x25519` names",
    ),
    (
        "room_members",
        "entry_id",
        "a ROSTER ENTRY ID (derived above)",
    ),
    ("room_generations", "room_id", "a ROOM ID"),
    (
        "room_generations",
        "generation_id",
        "a GENERATION ID (derived above)",
    ),
    (
        "room_generations",
        "parent_id",
        "the predecessor's GENERATION ID",
    ),
    (
        "room_generations",
        "key_commitment",
        "`blake3::derive_key` over a 32-byte RANDOM generation key",
    ),
    ("room_generation_wraps", "room_id", "a ROOM ID"),
    ("room_generation_wraps", "generation_id", "a GENERATION ID"),
    (
        "room_generation_wraps",
        "entry_id",
        "a ROSTER ENTRY ID — the same value as `room_members.entry_id`, which the \
         two tables join on",
    ),
    (
        "room_generation_wraps",
        "wrap",
        "the generation key sealed to one entry: canonical CBOR `{version, enc, ct}` \
         and nothing else. The associated data `(generation_id, entry_id)` is \
         COMPUTED at open, never carried, so — unlike the MUA-plane blobs — there \
         is no plaintext index in the bytes; the nest opens only its own row",
    ),
    ("room_invites", "room_id", "a ROOM ID"),
    ("room_message_views", "room_id", "a ROOM ID"),
    (
        "room_message_views",
        "doc_key",
        "`blake3` over `room-message:<room id>:<log position>` — no person in the \
         preimage",
    ),
    ("room_message_views", "generation_id", "a GENERATION ID"),
    ("room_post_views", "room_id", "a ROOM ID"),
    (
        "room_post_views",
        "post_id",
        "a content address: `blake3` of the signed post body. The body names its \
         author; its hash does not",
    ),
    (
        "room_post_views",
        "doc_key",
        "`blake3` over `room-post:<room id>:<post id>`",
    ),
    ("room_post_views", "generation_id", "a GENERATION ID"),
    ("room_policy_versions", "room_id", "a ROOM ID"),
    (
        "actor_channels",
        "channel_id",
        "a CHANNEL ID (derived above)",
    ),
    ("channel_commit_watermark", "channel_id", "a CHANNEL ID"),
    ("channel_foreign_members", "channel_id", "a CHANNEL ID"),
    (
        "channel_foreign_members",
        "home_nest_id",
        "a peer DEPLOYMENT's identity key, read off its `fauna.nest.info` — a box, \
         never an account (the row's own ruling says the same: that peer's address, \
         not the member's data). Structurally an actor id, and on a one-person \
         self-hosted nest a 1:1 correlate of that person's server: declared, not \
         ruled, since no succession or deletion is the nest's to have",
    ),
    ("conv_attachment_refs", "channel_id", "a CHANNEL ID"),
    ("conv_record_authors", "channel_id", "a CHANNEL ID"),
    // ── The content and curation planes (2026-09-21). One family recurs:
    //
    // CONTENT ID — a 32-byte content address, always caller-computed and
    // written through the one choke point `insert_content`: `blake3` of the
    // post's wire bytes; of `"<type>:<natural id>"` for a document or profile
    // row; of a domain tag + bridge type + actor id + external id for a bridge
    // row; of recipient + time + a process nonce + payload for an inbox row.
    // ⚠ Declared: the bridge and profile derivations hash an actor id UNSALTED,
    // so those ids are recomputable by anyone holding the inputs — a
    // linkability oracle, not an identity. The person on every such row is a
    // plain registered column beside it.
    ("content", "id", "a CONTENT ID (derived above)"),
    (
        "content_fts_data",
        "block",
        "an SQLite-managed FTS5 shadow table of `content_fts` (`db/schema.rs`) — no \
         code of ours writes it, so there is no writer to read and no row to rule. \
         Holds NO actor id: every native-post writer indexes an empty author. ⚠ What \
         it does hold is tokenized TEXT, and that includes display names and bios, \
         and for bridged content the foreign identity (a mail address, a nostr key, a \
         fediverse actor URI). It is a derived index of rows ruled on their own \
         planes, reachable only by `DELETE FROM content_fts WHERE rowid` through \
         `content_fts_map`; whether every purge path reaches it is the content \
         plane's question, not this walk's",
    ),
    (
        "content_fts_docsize",
        "sz",
        "the same shadow table family: per-document token counts",
    ),
    (
        "content_fts_map",
        "content_id",
        "a CONTENT ID pinning an FTS rowid. ⚠ For a `profile` row it is \
         blake3(\"profile:\" + hex(actor)) with NO `content` row and no person \
         column beside it — the one derived-key shape here whose person is \
         reachable only through the digest. Its lifecycle is `users.handle`'s, \
         kept by `fts::sync_profile_row` at every handle write/clear and user \
         deletion and re-derived each boot by `fts::reconcile_profile_rows`",
    ),
    // ── `content_labels`' three columns NO WRITER FILLS. All three writers
    // (the attach door, the room labeler pass, the channel anomaly label) pass
    // NULL, NULL and empty bytes. Each is ruled AS WRITTEN, and each entry
    // expires with its premise: the session that gives one a writer re-rules it
    // here, because what it would then hold is a person.
    (
        "content_labels",
        "attestation_data",
        "always NULL. Its would-be shape for a `ThresholdRevealed` attestation \
         carries AUDITOR signatures, which would name their signers — the session \
         that writes one re-rules this column as a bearer",
    ),
    (
        "content_labels",
        "obligation_id",
        "always NULL. ⚠ The one column here whose declared type and live writers \
         disagree: the wire type is `Option<ActorId>`, and the sibling \
         `obligation_action_records` treats an obligation id as the ISSUING ADMIN's \
         actor id (ruled a bearer that stays). A writer makes this a person \
         reference, owed a `SUCCESSION_REFERENCES` ruling of its own",
    ),
    (
        "content_labels",
        "signature",
        "always EMPTY bytes: nothing signs a label row and nothing verifies one \
         (`moderation.md` — a hand-attached label is unsigned by design). A \
         signature that arrives changes `classifier_id`'s succession reason, which \
         rests on there being nothing to contradict",
    ),
    (
        "content_links",
        "source_id",
        "a CONTENT ID — the content an edge hangs off",
    ),
    (
        "content_links",
        "target_id",
        "a CONTENT ID, or a 32-byte channel id — never an actor id: \
         mentions and follows are not modelled as links, and the row's person is \
         its registered `actor_id`",
    ),
    (
        "content_links",
        "metadata",
        "a JSON side-car: `{mailbox}` on an inbox delivery, the one live shape. A \
         second, bridge shape `{external_id, sender, recipient, subject, …}` is \
         written by `db/bridge.rs::insert_bridge_message`, which today has only test \
         callers. No actor id in either -- but declared for what the bridge shape \
         would be if a producer returns: a mail's addresses and subject in the clear, \
         under a column called `metadata`",
    ),
    ("content_meta", "content_id", "a CONTENT ID"),
    (
        "content_meta",
        "gated_room",
        "a ROOM ID or MLS channel id (derived above)",
    ),
    (
        "content_reports",
        "content_hash",
        "the reported item's CONTENT ID (a mail's report hash); the reporter is the \
         row's registered `reporter`",
    ),
    (
        "content_scores",
        "content_id",
        "a CONTENT ID, or on the room plane a composite `room id ‖ position` / \
         `room id ‖ post id` — no person in either",
    ),
    (
        "engagement_events",
        "event_id",
        "a dedup digest. ⚠ Declared: the toggle form is an UNSALTED `blake3` over \
         (actor id, content id, tag), so it is a membership oracle for anyone who can \
         read the table and guess the pair; the actor is the row's registered \
         `actor_id` either way",
    ),
    ("engagement_events", "content_id", "a CONTENT ID"),
    (
        "engagement_events",
        "event_data",
        "always NULL — every production caller passes none and no shape is defined. \
         The session that gives it one rules it",
    ),
    (
        "feeds",
        "rules",
        "the nest-encoded canonical `Vec<FilterRule>`: tags, terms, counts, flags and \
         label categories. The two author-set variants carry a 32-byte HASH of a \
         published actor set, never an actor id, and nothing in the tree authors \
         them. Contributor lists live in their own tables",
    ),
    (
        "feeds",
        "composition",
        "`Vec<CompositionEntry { factor, weight_permille }>` — a factor key may embed \
         a labeler's `algorithm_id` as hex text, which is an artifact key",
    ),
    (
        "labeler_list_entries",
        "content_id",
        "a CONTENT ID a published List scores",
    ),
    (
        "labeler_subscriptions",
        "grant_id",
        "a 16-byte random handle the minting client chose; nothing resolves through it",
    ),
    (
        "labelers",
        "wasm_hash",
        "the content hash of the artifact bytes",
    ),
    (
        "labelers",
        "wasm_bytes",
        "the artifact: a WASM module, a canonical List of (CONTENT ID, score) pairs \
         with an optional publisher-chosen name, or a bounded vocabulary — no actor \
         id in any of the three by shape, and the bytes are hash- and signature-bound \
         to the `algorithm_id` stored beside them. Declared: they are PUBLISHER-AUTHORED, \
         so the name, the vocabulary and a module's data can hold any string the \
         publisher chose, an id included -- attested only as the publisher's own words",
    ),
    //
    // ── The sync, backup, custody, recovery and nest planes and the remainder, grouped by SHAPE,
    // each shape said once.
    //
    // SEALED LABELS -- canonical `SealedLabel { v, gen, nonce, ct }` (`fauna_core::path_crypto`),
    // sealed by the owner's CLIENT under a root the nest never holds. The cleartext header is a
    // version, a key-generation NUMBER and a nonce; the associated data (version, salt, field tag)
    // is derived, never stored, and the salt is a path or name hash or a row id -- never an actor
    // id.
    (
        "folders",
        "name_sealed",
        "a sealed label over the set name (convergent under `name_hash`)",
    ),
    (
        "folders",
        "include_paths_sealed",
        "a sealed label over the include list, salted by the folder id",
    ),
    (
        "folders",
        "exclude_paths_sealed",
        "a sealed label over the exclude list, salted by the folder id",
    ),
    (
        "folders",
        "retention_policy_sealed",
        "a sealed label over the retention-policy JSON",
    ),
    (
        "snapshots",
        "tags_sealed",
        "a sealed label over the tag list",
    ),
    (
        "snapshot_files",
        "path_sealed",
        "a sealed path label, copied keylessly off `sync_changes` at snapshot creation",
    ),
    (
        "sync_changes",
        "path_sealed",
        "a sealed path label; the nest copies it between tables and never mints one",
    ),
    (
        "sync_conflicts",
        "path_sealed",
        "a sealed path label under the shared path-plane tag",
    ),
    (
        "sync_devices",
        "label_sealed",
        "a sealed label over the user-chosen device name, salted by the device id",
    ),
    (
        "backup_custody",
        "path_sealed",
        "a sealed path label copied verbatim off the source row by the folder-mirror leg; the \
         segment leg stores none",
    ),
    (
        "share_tokens",
        "filename_sealed",
        "the shared file's name, sealed by the author's client under their owner root for \
         their own share list -- the only form the name rests in, stored verbatim-opaque",
    ),
    (
        "share_tokens",
        "key_envelope",
        "a fragment-keyed private link's KeyEnvelope, AEAD-sealed by the author's client under a \
         link key the nest never sees (fauna_core::share::KeyEnvelope): the linked file's per-chunk \
         keys, its plaintext hashes and its name -- no actor id inside",
    ),
    //
    // DIGESTS and content addresses -- one-way. Where a preimage names someone the entry says so,
    // and that the preimage is not stored.
    (
        "folders",
        "name_hash",
        "`derive_key(\"fauna.set-name.v1\", name)`. The succession rename that prefixes a retired \
         id onto a reserved name does NOT recompute it, so no digest of an actor id is ever \
         written",
    ),
    (
        "snapshot_files",
        "manifest_hash",
        "BLAKE3 content address of a chunk manifest, copied off `sync_changes`",
    ),
    (
        "snapshot_files",
        "path_hash",
        "BLAKE3 of the folder-relative path",
    ),
    (
        "sync_changes",
        "manifest_hash",
        "BLAKE3 of the canonical chunk manifest, or of the sealed blob on the reserved rails",
    ),
    (
        "sync_changes",
        "path_hash",
        "BLAKE3 of the path; on account-state rows a keyed item blind under the owner's backup key",
    ),
    (
        "sync_conflict_candidates",
        "manifest_hash",
        "BLAKE3 of a candidate version's manifest",
    ),
    ("sync_conflicts", "path_hash", "BLAKE3 of the path"),
    (
        "sync_conflicts",
        "winning_manifest_hash",
        "BLAKE3 of the winning version's manifest",
    ),
    (
        "share_tokens",
        "manifest_hash",
        "BLAKE3 content address of the shared file's manifest",
    ),
    (
        "share_tokens",
        "token_id",
        "BLAKE3 of the signed token blob. The preimage names the author and is never stored; the \
         registry's own note says a digest does not reconstruct it",
    ),
    (
        "blob_metadata",
        "hash",
        "the blob store's BLAKE3 content address -- a flat plane with no owner column at all",
    ),
    (
        "blob_metadata",
        "thumbnail_hash",
        "the uploader-declared content address of a separate sealed thumbnail",
    ),
    (
        "backup_custody",
        "manifest_hash",
        "BLAKE3 content address of the held manifest blob",
    ),
    (
        "backup_custody",
        "path_hash",
        "BLAKE3 of a machine-synthetic custody path. For segment custody the preimage is a \
         template over the hex of the owner's actor id, so a holder of that id can recompute a row \
         -- a confirmation, never a disclosure, and after a Move it still derives from the \
         predecessor, unobservably",
    ),
    (
        "backup_custody_generations",
        "manifest_hash",
        "the superseded custody row's manifest hash, moved not re-derived",
    ),
    (
        "backup_custody_generations",
        "path_hash",
        "the superseded custody row's path hash",
    ),
    (
        "generation_escrow_wraps",
        "generation_id",
        "`derive_key` over the canonical mint core -- whose member and minter fields are DEVICE \
         ids, and which is not stored",
    ),
    (
        "generation_escrow_wraps",
        "wrap_hash",
        "BLAKE3 of the deposited wrap, computed nest-side as the idempotency key",
    ),
    (
        "recovery_pending_replacements",
        "record_digest",
        "BLAKE3 of the verbatim `record` bytes, an idempotence key",
    ),
    (
        "segment_records",
        "record_cid",
        "a 36-byte CID over already-sealed record bytes",
    ),
    (
        "segment_records",
        "report_hash",
        "BLAKE3 over a message's canonicalized subject and body -- cross-user identical by design, \
         no identity input",
    ),
    (
        "pending_actions",
        "chain_hash",
        "a SHA-256 chain digest. The row's actor id is a preimage INPUT, not recoverable, and \
         nothing ever re-verifies the chain",
    ),
    (
        "legal_takedown_deleted_posts",
        "post_id",
        "a post's BLAKE3 content address",
    ),
    (
        "message_scan_results",
        "message_id",
        "the filing CID digest of the sealed record, or on a perimeter reject a BLAKE3 over \
         scanner signature, time and sender DOMAIN",
    ),
    (
        "guardian_mail_holds",
        "message_id",
        "the content hash of the held sealed record; the registry already notes that naming it \
         buys nothing",
    ),
    (
        "web_rendered_sealed",
        "post_id",
        "a post's BLAKE3 content address, a render-key derivation input",
    ),
    (
        "video_cache_sources",
        "content_hash",
        "a video segment's content address; the one writer has no production caller",
    ),
    (
        "peer_content_reports",
        "content_hash",
        "a report-hash or post id from a peer's k-gated export; no reporter identity crosses the \
         wire",
    ),
    (
        "peer_content_trends",
        "content_id",
        "a post content id from a peer's trend export; engager identities never travel",
    ),
    (
        "region_log_anchor",
        "head",
        "a 32-byte git SHA-256 commit id: the transparency-log head last accepted",
    ),
    //
    // CHANNEL and group handles -- a group is not a person; its roster lives in registered plain
    // columns.
    (
        "folders",
        "mls_group_id",
        "the raw MLS group id the set is bound to: client-supplied, opaque, a group HANDLE (the \
         registry's own words) and a roster-linkability one, not a person",
    ),
    (
        "folder_channel_claims",
        "channel_id",
        "`derive_key(\"fauna.channel.v1\", group_id)`, derived server-side. The claimant is the \
         plain column `claimed_by`",
    ),
    (
        "folder_content_keys",
        "channel_id",
        "the same derived channel id. The table has no person column; it is torn down per channel \
         with the bound folder",
    ),
    (
        "folder_member_access",
        "channel_id",
        "the same derived channel id. The grantee is the plain column `actor_id`",
    ),
    //
    // RANDOM ids, nonces and constants.
    (
        "capability_grants",
        "grant_id",
        "16 random bytes the minting client chooses",
    ),
    (
        "custody_hosting",
        "grant_id",
        "the ceremony's 16 random bytes, in the capability-grant id space",
    ),
    (
        "custody_receipts_staged",
        "grant_id",
        "the same 16 random ceremony bytes",
    ),
    (
        "rpc_idempotency",
        "idem_key",
        "16 random bytes minted by the client per call; never derived from the caller",
    ),
    (
        "namespace_entries",
        "entry_id",
        "the ASCII constant `fauna.lan-tls.imap-caldav.v1` from the one honest producer -- but \
         `fauna.federation.sync.push` stores ANY string a paired peer relays here (the door binds \
         the namespace to the paired actor, not the entry id), so the column is peer-chosen text \
         and is declared as that. No nest decision reads it as an identity, and whatever it holds \
         sits under the owning actor's own `namespace` and goes with that row (Purge / Burn)",
    ),
    //
    // SECRETS, wraps and signatures -- key material is a confidentiality question (the export axis
    // rules it), not a naming one: none of these carries an identifier.
    (
        "folder_content_keys",
        "sealed",
        "nonce + AEAD over the set's content-key generations `{version, key, rotated_at}`, keyed \
         by the group's epoch export -- which the nest never holds. No associated data, no index",
    ),
    (
        "generation_escrow_wraps",
        "wrap",
        "`{v, enc, ct}`: a generation key sealed CLIENT-side to the identity's X-Wing escrow key. \
         The recipient binding is computed associated data, never stored",
    ),
    (
        "nest_backup_keys",
        "backup_key",
        "a 32-byte SYMMETRIC secret the owner's client derives from its seed: key material FOR a \
         named person (hence the row's Burn and its export withholding), but no identifier",
    ),
    (
        "namespace_entries",
        "ciphertext",
        "a TLS certificate bundle sealed by the client to a nest's key. Its plaintext index names \
         a DOMAIN and two synthetic bridge labels. The one originating producer is the \
         LAN-certificate publish; relays copy it verbatim. The PERSON on this table is \
         `namespace`, the publishing actor's id, which the census registers (Purge / Burn)",
    ),
    (
        "nest_room_read_key",
        "ikm_wrapped",
        "32 random bytes of the nest's own KEM seed, wrapped under a key derived from the \
         deployment seed",
    ),
    (
        "oauth_issuer_keys",
        "secret_wrapped",
        "the nest's OAuth issuer P-256 scalar, wrapped under the deployment seed",
    ),
    (
        "oauth_session_secret",
        "secret_wrapped",
        "the nest's random refresh-token MAC key, wrapped under the deployment seed",
    ),
    (
        "media_ticket_secret",
        "secret_wrapped",
        "the nest's random media playback-ticket MAC key, wrapped under the deployment seed",
    ),
    (
        "mail_dkim_keys",
        "key_wrapped",
        "a mail domain's DKIM private signing key (PKCS#8), wrapped under the deployment \
         seed -- keyed by (domain, selector), the deployment's identity to peer MTAs, \
         never an account's",
    ),
    (
        "vapid_keypair",
        "pem",
        "the PKCS#8 PEM of the nest's own Web-Push application-server key",
    ),
    (
        "obligation_action_records",
        "signature",
        "EMPTY in every production row: the only live writers bind a zero-length blob, and the \
         signed arm has no caller outside tests",
    ),
    //
    // NEST identities -- the same byte shape as an actor id (a 32-byte Ed25519 key), sometimes the
    // same TYPE name (`nest_actor_id`), and never the same thing: every mint of a deployment seed
    // is a fresh CSPRNG draw (first boot, provisioning, rotation) and none derives from an account
    // key. A server is not a person.
    (
        "nest_keypair",
        "public_key",
        "THIS nest's deployment public key",
    ),
    (
        "nest_keypair",
        "secret_key",
        "THIS nest's raw deployment seed -- the root its other secrets are wrapped under",
    ),
    (
        "nest_room_read_key",
        "public_key",
        "the nest's own 1216-byte X-Wing reception key, published so room members can wrap to it",
    ),
    (
        "nest_rotation_log",
        "statement",
        "the verbatim `SignedNestRotation`: two generations of THIS nest's key and their two \
         signatures. Its fields are spelled `*_nest_actor_id`, which is the trap -- the census \
         already excludes the table's plain columns for the same reason",
    ),
    (
        "nest_addresses",
        "nest_id",
        "a peer nest's key, signature-proven by the federation hello or asserted beside one that \
         was",
    ),
    (
        "nest_pairings",
        "private_nest_id",
        "the paired nest's key. ⚠ Client-supplied and UNVALIDATED at the door -- but only ever \
         matched against a channel-verified peer identity, so any other 32 bytes are inert. The \
         person is the plain column `actor_id`",
    ),
    (
        "succession_owed_nests",
        "nest_id",
        "a `nest_pairings.private_nest_id` copied verbatim by the succession transaction before \
         the burn: a paired nest's key, as unvalidated as its source. Never dialled or matched by \
         this nest -- served to the successor and deleted on its settle. The person is the plain \
         column `old_actor_id`",
    ),
    (
        "nest_trust",
        "nest_id",
        "DARK: a nest id by the writer's signature, and that writer has no production caller",
    ),
    (
        "nest_engagement_stats",
        "nest_id",
        "DARK: a nest id by the writer's signature, and that writer has no production caller",
    ),
    (
        "exchange_peers",
        "nest_id",
        "the report/trend-exchange partner's key, from its node info",
    ),
    (
        "backup_destinations",
        "nest_id",
        "the destination nest's key, pinned as the expected federation peer; EMPTY for a \
         client-device destination",
    ),
    (
        "backup_writer_grants",
        "writer_nest_id",
        "the source nest's key. The migration's own words: a nest id is self-minted and free, \
         attribution and never permission",
    ),
    (
        "foreign_recovery_heads",
        "anchor_nest_id",
        "the foreign identity's home-nest key, pinned write-once at first contact",
    ),
    (
        "device_authorizations",
        "device_key",
        "THIS nest's signing key, despite the name: the upload door refuses any other value",
    ),
    //
    // DEVICE ids and device keys name a machine seat, not an account. A sync device id is
    // `derive_domain_key("fauna.sync.device-id.v1", install_secret, actor_id)` -- keyed by a
    // secret that never leaves the machine, so the bytes resolve to nobody and two accounts on one
    // install are unlinkable; a device KEY is a fresh random keypair. What links either to a
    // person is the `sync_devices` row beside it, which IS registered and purged -- after which
    // the value is an orphan pseudonym. The one device-id value that is a function of an actor id
    // alone is ruled in the bearing list (`sync_changes.device_id`).
    (
        "folder_members",
        "device_id",
        "a client-asserted sync device id; the table has no person column and follows its folder",
    ),
    (
        "snapshots",
        "device_id",
        "a client-asserted sync device id, NULL on every nest-created snapshot; the table follows \
         its folder",
    ),
    (
        "upload_leases",
        "device_id",
        "the lease holder's sync device id, on a row with a 300-second life",
    ),
    (
        "sync_conflicts",
        "device_id",
        "the REPORTING device's id, client-asserted and not even length-checked",
    ),
    (
        "sync_conflict_candidates",
        "device_id",
        "the diverging version's device id, client-asserted and not length-checked",
    ),
    (
        "sync_changes",
        "origin_writer",
        "the authoring store principal's key on account-state rows; byte-identical to that \
         machine's `sync_devices.auth_device_key`, which is how the nest resolves it",
    ),
    (
        "sync_devices",
        "auth_device_key",
        "the renewal or store-principal public key the row's grant authorizes",
    ),
    (
        "revoked_device_grants",
        "auth_device_key",
        "a revoked renewal key -- a tombstone exists to be matched and refused",
    ),
    (
        "sync_signer_certs",
        "device_key",
        "a delegated change-record signer's device principal public key -- the \
         `sync_devices.auth_device_key` value its cert authorizes",
    ),
    (
        "sync_changes",
        "signature",
        "a 64-byte Ed25519 writer signature over the row's `SignedChange` statement -- a \
         signature holds no key and names nobody `signer_key` does not",
    ),
    (
        "folders",
        "set_nonce",
        "32 client-minted random bytes -- the set binding writer signatures are built under",
    ),
    (
        "state_walk_marks",
        "walker",
        "a device writer key, client-supplied and unauthenticated; it counts only when it joins a \
         live `sync_devices` grant of the calling account. No statement anywhere deletes these \
         rows -- inert residue, since an unmatched mark counts for nothing",
    ),
    (
        "capability_grants",
        "holder_pubkey",
        "the key a grant is sealed or addressed to: a bridge service user's X25519 key, or for \
         custody the custodian's device-principal key. The registry's own note calls it the ledger \
         half, and documents it as not an actor identity",
    ),
    //
    // STRUCTURED values, each read field by field.
    (
        "snapshots",
        "message_manifest",
        "plaintext `Manifest { format versions, kind, segment ids and counters }` of a \
         message-kind segment store",
    ),
    (
        "snapshots",
        "placement_manifest",
        "plaintext mail, calendar or card placement: mailbox names, uids, flags, per-collection \
         ids, record ids and sizes. The owner's metadata in the clear -- a disclosure fact for the \
         export axis -- but no actor id and no key",
    ),
    (
        "feature_policies",
        "document",
        "canonical `FeaturePolicy`: an allow/deny and numeric windowed bounds. The subject is \
         deliberately NOT folded in -- it is `subject_id`, still on the backlog",
    ),
    (
        "guardian_policies",
        "features_document",
        "a map from feature-key strings to the same numeric `FeaturePolicy`; ward and guardian are \
         plain columns",
    ),
    (
        "export_sessions",
        "scope_descriptor",
        "client-authored `ExportScope { mailboxes, date_from, date_to, strip_headers }`, stored \
         unparsed",
    ),
    (
        "tlsrpt_outbound_reports",
        "payload_json",
        "the RFC 8460 aggregate report as PLAIN JSON (the DDL comment says gzipped; the writer \
         stores it uncompressed): per-domain session counts, and a `postmaster@` role contact",
    ),
    (
        "region_artifacts",
        "envelope",
        "a verified `PolicyArtifact`: region, authority key id, feature bounds or content rules, \
         scorer lists of `(content id, score)`, the region authority's signature. Residual, \
         declared: the forward-compat `extra` maps and a wasm scorer's bytes are carried unread",
    ),
    (
        "region_relay_cache",
        "envelope",
        "the same `PolicyArtifact`, forwarded undecoded; the demand row that creates it records no \
         requester",
    ),
    (
        "region_relay_cache",
        "evidence",
        "`InclusionEvidence`: commit ids, raw git commit and tree objects, a checkpoint, and \
         cosignatures by enrolled LOG WITNESSES -- infrastructure operators, not accounts. The git \
         object bytes are the log's own and are only ever re-hashed",
    ),
    //
    // DARK columns -- no production writer. Each says what was searched for; a producer added
    // later re-rules its column.
    (
        "restore_history",
        "source_member_id",
        "NULL in every production row: the one caller passes `None` and nothing in the tree \
         produces a value. ⚠ The registry already names it the one column here that could come to \
         name someone else -- the commit that first passes a value MOVES this entry to the bearing \
         list with a ruling",
    ),
];

// ⚠ **This walk HAD a backlog, and it is gone — do not bring it back.** It
// landed with an `UNEXAMINED_OPAQUE_COLUMNS` list pinned by an exact count:
// every `BLOB` column no walk had ever looked at, named as unexamined rather
// than guessed. It was drained to zero on 2026-09-21 — every column READ from
// its writer, the last ones held until their tables' ROWS were ruled in the
// registry proper (the ordering rule above) — and the list, its constant and
// its only-shrinks test were deleted with the last entry, exactly as the blob
// walk's were: so that "unexamined" is no verdict a column can take. A new
// `BLOB` column is ruled in the commit that adds it, into one of the two lists
// above. What each drained column holds: `succession-repoint-axis.md` § The
// declared re-point axis, the type walk's blockquotes.

fn declared_blob(declared_type: &str) -> bool {
    declared_type.to_ascii_uppercase().contains("BLOB")
}

#[test]
fn every_blob_typed_column_outside_the_name_walks_declares_what_it_holds() {
    let db = CacheDb::open_in_memory().unwrap();
    let conn = db.conn.blocking_lock();
    let mut tables_stmt = conn
        .prepare(
            "SELECT name FROM sqlite_master \
             WHERE type = 'table' AND name NOT LIKE 'sqlite_%'",
        )
        .unwrap();
    let mut all_tables: Vec<String> = tables_stmt
        .query_map([], |r| r.get::<_, String>(0))
        .unwrap()
        .filter_map(|r| r.ok())
        .collect();
    drop(tables_stmt);
    all_tables.sort();

    let declared: HashSet<(&str, &str)> = ACTOR_BEARING_OPAQUE_COLUMNS
        .iter()
        .chain(NON_ACTOR_OPAQUE_COLUMNS.iter())
        .map(|(t, c, _)| (*t, *c))
        .collect();

    let mut undeclared = Vec::new();
    let mut seen: HashSet<(String, String)> = HashSet::new();
    for table in &all_tables {
        for (col, declared_type, ..) in table_columns(&conn, table) {
            if !declared_blob(&declared_type)
                || actor_shaped(&col)
                || BLOB_SHAPED_COLUMN_FRAGMENTS.iter().any(|f| col.contains(f))
            {
                continue;
            }
            seen.insert((table.clone(), col.clone()));
            if !declared.contains(&(table.as_str(), col.as_str())) {
                undeclared.push(format!("(\"{table}\", \"{col}\"),"));
            }
        }
    }
    assert!(
        undeclared.is_empty(),
        "BLOB-typed column(s) that declare nothing about what they hold — read the \
         writer, then add each to ACTOR_BEARING_OPAQUE_COLUMNS (its bytes name actors: \
         say what deletion and succession do to them, following the row ruling its \
         table already has) or to NON_ACTOR_OPAQUE_COLUMNS (they do not: say why). \
         ⚠ There is no `unexamined` list to park it on -- this walk's backlog was \
         drained and deleted, and a column added today was written by someone who \
         knows what is in it:\n{}",
        undeclared.join("\n")
    );

    // The converse, as in the blob walk: a declaration naming a column the
    // schema no longer has — or one that has since become another walk's — is
    // a rule that quietly stopped applying.
    let mut vanished: Vec<String> = declared
        .iter()
        .filter(|(t, _)| !FEATURE_GATED_ELSEWHERE.contains(t))
        .filter(|(t, c)| !seen.contains(&((*t).to_string(), (*c).to_string())))
        .map(|(t, c)| format!("{t}.{c}"))
        .collect();
    vanished.sort();
    assert!(
        vanished.is_empty(),
        "opaque-carrier declaration(s) naming a column this walk does not see — the \
         column is gone, is no longer BLOB-typed, or now belongs to the census or the \
         blob walk: {vanished:?}"
    );

    // One column, one verdict: "its bytes name actors" and "they do not" cannot
    // both be declared.
    let mut twice: Vec<(&str, &str)> = ACTOR_BEARING_OPAQUE_COLUMNS
        .iter()
        .map(|(t, c, _)| (*t, *c))
        .filter(|b| {
            NON_ACTOR_OPAQUE_COLUMNS
                .iter()
                .any(|(t, c, _)| (*t, *c) == *b)
        })
        .collect();
    twice.sort();
    assert!(
        twice.is_empty(),
        "column(s) declared BOTH actor-bearing and not: {twice:?}"
    );
}
