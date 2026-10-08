//! The **DAV-store context chokepoint** — `(actor_id, msek, prior_mseks)`, the inputs
//! every encrypted CalDAV/CardDAV op needs (both surfaces read/write the SAME
//! MSEK-keyed store, priority #2 — one msek gate, not two copies).
//!
//! Independent copies of this exact sequence — rebuild the actor keypair,
//! load the actor's mail material, take the MSEK — existed before this
//! module: `fauna-ffi`'s internal `dav_store_context` (serving both
//! `FfiCaldavClient` and `FfiCarddavClient`), tui's `events::caldav_context`,
//! and linux's `client::caldav_context`. All three now delegate here
//! (priority #4: resolve drift toward the richest existing pattern — `fauna-ffi`'s
//! name and doc framing, since it already served two callers). A fourth copy,
//! tui's `address_book::carddav_context` — written after this consolidation
//! for the CardDAV-specific page and never updated to call in — was found and
//! lifted the same way 2026-09-02.
//!
//! The MSEK is read from the account's mail custody (`fauna.state.mail`,
//! through [`MailStore`]).

use zeroize::Zeroizing;

use crate::store_seam::MailStore;

/// The inputs every encrypted CalDAV/CardDAV op needs: the actor, the current
/// MSEK generation (what a write seals to) and the custody's prior generations,
/// newest first (with `msek`, what a read opens through —
/// `DavRecipientKeys::from_mseks(&msek, &prior_mseks)`; `mail-credentials.md`
/// § Rotation and recovery → *DAV bodies across a rotation*, ruling 1: reads
/// walk the ring, writes seal to the current generation alone).
#[derive(PartialEq, Eq)]
pub struct DavStoreContext {
    pub actor_id: [u8; 32],
    pub msek: [u8; 32],
    pub prior_mseks: Zeroizing<Vec<[u8; 32]>>,
}

impl core::fmt::Debug for DavStoreContext {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("DavStoreContext")
            .field("actor_id", &self.actor_id)
            .field("prior_generations", &self.prior_mseks.len())
            .finish_non_exhaustive()
    }
}

/// The [`DavStoreContext`] for an encrypted CalDAV/CardDAV op, or `None` when
/// mail/CalDAV-CardDAV is not enabled yet (no MSEK minted —
/// `MailSettingsMachine::enable_mail`) or the custody read failed. A read
/// failure logs and degrades the same way as the legitimate disabled state
/// (empty calendar/address-book list) rather than erroring the caller — the
/// secret is fine, only the store read didn't complete.
pub async fn dav_store_context(
    mail: &dyn MailStore,
    actor_id: [u8; 32],
) -> Option<DavStoreContext> {
    let mail = match mail.load().await {
        Ok(mail) => mail,
        Err(e) => {
            tracing::error!("dav_store_context: mail custody read failed: {e}");
            return None;
        }
    };
    Some(DavStoreContext {
        actor_id,
        msek: mail.msek?.to_array(),
        prior_mseks: Zeroizing::new(mail.prior_mseks.iter().map(|m| m.to_array()).collect()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_helpers::FakeMailStore;
    use crate::test_nest::block_on;
    use fauna_core::data::MailConfig;

    /// No `msek` minted yet (mail/CalDAV never enabled) — `None`, the
    /// legitimate disabled state, not an error.
    #[test]
    fn no_msek_reads_none() {
        let ctx = block_on(dav_store_context(&FakeMailStore::empty(), [7; 32]));
        assert_eq!(ctx, None);
    }

    /// Once `enable_mail` mints an `msek`, the context carries the caller's
    /// `actor_id` and the SAME `msek` bytes stored — the round-trip a real
    /// CalDAV/CardDAV op depends on.
    #[test]
    fn minted_msek_round_trips_actor_id_and_msek() {
        let mail = FakeMailStore::with(&MailConfig {
            msek: Some([0x99; 32].into()),
            ..MailConfig::default()
        });
        let DavStoreContext {
            actor_id,
            msek,
            prior_mseks,
        } = block_on(dav_store_context(&mail, [8; 32])).expect("context");
        assert_eq!(actor_id, [8; 32]);
        assert_eq!(msek, [0x99; 32]);
        assert!(
            prior_mseks.is_empty(),
            "a never-rotated custody has no grace generations"
        );
    }

    /// After a rotation the context carries the custody's prior generations,
    /// newest first — the ring's inputs, without which every DAV body written
    /// before the rotation fails to open (`mail-credentials.md` § Rotation and
    /// recovery → *DAV bodies across a rotation*, ruling 1).
    #[test]
    fn a_rotated_custody_carries_its_prior_generations() {
        let mail = FakeMailStore::with(&MailConfig {
            msek: Some([0x99; 32].into()),
            prior_mseks: vec![[0x88; 32].into(), [0x77; 32].into()],
            prior_msek_retirements: vec![
                fauna_core::data::PriorMsekRetirement {
                    msek: [0x88; 32].into(),
                    retired_at_unix: 200,
                },
                fauna_core::data::PriorMsekRetirement {
                    msek: [0x77; 32].into(),
                    retired_at_unix: 100,
                },
            ],
            ..MailConfig::default()
        });
        let ctx = block_on(dav_store_context(&mail, [8; 32])).expect("context");
        assert_eq!(ctx.msek, [0x99; 32]);
        assert_eq!(*ctx.prior_mseks, vec![[0x88; 32], [0x77; 32]]);
    }

    /// A custody the runtime cannot read degrades to `None`, like the
    /// disabled state — never a panic, never a stale MSEK.
    #[test]
    fn an_unreadable_custody_reads_none() {
        struct Broken;
        #[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
        #[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
        impl MailStore for Broken {
            async fn load(&self) -> Result<MailConfig, crate::StoreError> {
                Err(crate::StoreError::Load("no runtime".into()))
            }
            async fn load_rows(
                &self,
            ) -> Result<fauna_core::mail_rows::MailRows, crate::StoreError> {
                Err(crate::StoreError::Load("no runtime".into()))
            }
            async fn write_state(
                &self,
                _: fauna_core::mail_rows::MailStateRow,
            ) -> Result<bool, crate::StoreError> {
                unreachable!()
            }
            async fn put_credential(
                &self,
                _: fauna_core::data::MailCredential,
            ) -> Result<bool, crate::StoreError> {
                unreachable!()
            }
            async fn mark_wrapped(
                &self,
                _: String,
                _: fauna_core::data::MsekFingerprint,
            ) -> Result<bool, crate::StoreError> {
                unreachable!()
            }
            async fn revoke(&self, _: String) -> Result<bool, crate::StoreError> {
                unreachable!()
            }
            async fn retire_generation(
                &self,
                _: fauna_core::data::PriorMsekRetirement,
            ) -> Result<bool, crate::StoreError> {
                unreachable!()
            }
        }
        assert_eq!(block_on(dav_store_context(&Broken, [9; 32])), None);
    }
}
