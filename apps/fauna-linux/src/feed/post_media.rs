//! Opening a feed post image's fetched blob before it is decoded — the linux
//! leg of the tier-sealed post-media read (`docs/goal/ui/media.md` § Encryption
//! at rest: one per-post key seals a restricted post's body and all its
//! attachments together, so a reader who can open the body can open the
//! attachments by construction).
//!
//! Split out of `views::feed::post_list::build_post_image` so the routing
//! decision is pinned headlessly: turning the returned bytes into a
//! `gdk::Texture` needs a display, the decision does not. windows draws the
//! same line (`FaunaApp.Core.Helpers.PostMediaOpen`), and tui's
//! `Op::FetchImage` makes the same call inline.

use fauna_core::load_cache::Finished;

use crate::feed::host::LinuxFeedManager;
use crate::nest_content_api::ApiError;

/// The bytes to decode for a post image, given what its by-hash blob GET
/// returned.
///
/// **Every post-image fetch goes through here, gated post or not** — there is
/// no is-this-post-restricted argument, and that is the design (`media.md`
/// § Encryption at rest → *Rendering a sealed attachment*, the apps whose image
/// path holds the bytes): a public post's blob is plaintext on the wire and
/// comes straight back, a tier-restricted post's attachment is opened under the
/// per-post key its body opened under, and an unregistered hash — public media,
/// or a gated post's item before its detail-open unlock — passes through
/// untouched.
///
/// [`Finished::Failed`] means the nest refused the blob or a known-sealed item
/// did not open; [`Finished::Transient`] means the nest could not serve it right
/// now ([`ApiError::is_transient`]), so the per-hash cache forgets the attempt
/// and the next build asks again. Either way the caller keeps its empty
/// picture; it never hands ciphertext to the decoder, where a key failure would
/// look exactly like a corrupt image.
///
/// The manager fetches nothing (it is WS-RPC-only), so the GET stays
/// `FaunaClient::fetch_blob_bytes` — which is also why the open happens here
/// and not inside that helper: `views::document::build_preview_image` shares
/// it from the conversations bubble, where no feed manager exists.
pub fn open_fetched_post_image(
    manager: &LinuxFeedManager,
    hash: &str,
    fetched: Result<Vec<u8>, ApiError>,
) -> Finished<Vec<u8>> {
    match fetched {
        Ok(bytes) => manager.open_media_bytes(hash, bytes).into(),
        Err(e) if e.is_transient() => Finished::Transient,
        Err(_) => Finished::Failed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use fauna_client::NestClient;
    use fauna_core::identity::ActorKeypair;
    use fauna_core::subscription::crypto::encrypt_content;

    const PER_POST_KEY: [u8; 32] = [0x5a; 32];
    const PHOTO: &[u8] = b"\x89PNG\r\n\x1a\n-the-subscriber-only-photo";

    fn offline_manager() -> LinuxFeedManager {
        fauna_feed::FeedManager::new(
            NestClient::new("http://127.0.0.1:1".to_string(), ActorKeypair::generate()),
            [7u8; 32],
        )
    }

    /// The blob hash an app fetches the item by (the manager keys on it).
    fn item_hash() -> String {
        hex::encode([0xaau8; 32])
    }

    /// The flow this leg exists for: GET blob/<hash> → the manager opens it
    /// under the per-post key the body's unlock registered → the photo is what
    /// reaches the decoder. A leg that skipped the manager would hand the
    /// decoder ciphertext and paint nothing — the blank card a gated photo post
    /// showed before.
    #[test]
    fn a_sealed_item_opens_before_anything_decodes() {
        let manager = offline_manager();
        manager.register_sealed_media_for_test(&item_hash(), PER_POST_KEY);
        let sealed = encrypt_content(&PER_POST_KEY, PHOTO);

        assert_eq!(
            open_fetched_post_image(&manager, &item_hash(), Ok(sealed)),
            Finished::Loaded(PHOTO.to_vec()),
        );
    }

    /// The pass-through arm, which is what lets the post card carry no
    /// is-this-post-gated branch: a hash this reader holds no key for is public
    /// media, plaintext on the wire.
    #[test]
    fn a_public_posts_bytes_pass_through_unchanged() {
        assert_eq!(
            open_fetched_post_image(&offline_manager(), &item_hash(), Ok(PHOTO.to_vec())),
            Finished::Loaded(PHOTO.to_vec()),
        );
    }

    /// A known-sealed item that does not open is `Failed`, never the ciphertext:
    /// decoding AEAD bytes fails exactly like a corrupt image would, hiding a
    /// real key failure behind the same blank card.
    #[test]
    fn a_sealed_item_that_does_not_open_fails_never_yielding_the_ciphertext() {
        let manager = offline_manager();
        manager.register_sealed_media_for_test(&item_hash(), PER_POST_KEY);
        let sealed_under_another_key = encrypt_content(&[0x11; 32], PHOTO);

        assert_eq!(
            open_fetched_post_image(&manager, &item_hash(), Ok(sealed_under_another_key)),
            Finished::Failed,
        );
    }

    /// A GET the nest refused paints nothing, terminally.
    #[test]
    fn a_refused_fetch_is_settled() {
        let refused = ApiError::Status {
            code: 404,
            message: "no such blob".into(),
        };
        assert_eq!(
            open_fetched_post_image(&offline_manager(), &item_hash(), Err(refused)),
            Finished::Failed,
        );
    }

    /// A GET that failed on the way says nothing about the blob: the next
    /// build must be free to ask again.
    #[test]
    fn a_transient_fetch_failure_is_left_unsettled() {
        let down = ApiError::Transport("nest down".into());
        assert_eq!(
            open_fetched_post_image(&offline_manager(), &item_hash(), Err(down)),
            Finished::Transient,
        );
    }
}
