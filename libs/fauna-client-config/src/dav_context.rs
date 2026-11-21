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
/// `DavRecipientKeys::from_mseks(kind, &msek, &prior_mseks)`, one ring per DAV
/// kind; `mail-credentials.md`
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

/// What the custody read answered for the DAV store — three answers kept apart,
/// because each means something different to the person (`docs/goal/ui/events.md`
/// § Errors & edge cases → *No calendar to hold an event*, ruling 2).
#[derive(Debug, PartialEq, Eq)]
pub enum DavCustody {
    /// The custody read itself failed (the account store is not up yet). A
    /// wait, not an absence: re-run the read when custody's first listing
    /// lands. Carries the store's error for the log.
    Unreadable(String),
    /// Custody is read and holds no MSEK yet: the mint every account takes at
    /// its first connect (`MailSettingsMachine::ensure_account_key`;
    /// `docs/goal/behavior/mail-credentials.md` § MSEK lifecycle → *Every
    /// account holds the key*) has not landed. A wait for the calendar and
    /// address-book reads, never a resting state; kept apart from
    /// [`DavCustody::Unreadable`] because the mint may run only over a custody
    /// it could read.
    NoKey,
    /// The key is there: the inputs every encrypted CalDAV/CardDAV op needs.
    Ready(DavStoreContext),
}

impl DavCustody {
    /// The context when the key is there, else `None` — the one helper for a
    /// caller that only needs "have a context or not" (a write that refuses
    /// without one, a read that degrades to an empty list). A caller that
    /// paints or gates on the answer matches the enum instead.
    pub fn into_context(self) -> Option<DavStoreContext> {
        match self {
            DavCustody::Ready(ctx) => Some(ctx),
            DavCustody::Unreadable(_) | DavCustody::NoKey => None,
        }
    }
}

/// Read the DAV-store custody: [`DavCustody::Ready`] with the
/// [`DavStoreContext`], [`DavCustody::NoKey`] when no MSEK is minted yet (the
/// first-connect mint has not landed), or [`DavCustody::Unreadable`] when the custody read
/// failed (logged here too, so a caller folding it with
/// [`DavCustody::into_context`] still leaves a trace).
pub async fn dav_custody(mail: &dyn MailStore, actor_id: [u8; 32]) -> DavCustody {
    let mail = match mail.load().await {
        Ok(mail) => mail,
        Err(e) => {
            tracing::error!("dav_custody: mail custody read failed: {e}");
            return DavCustody::Unreadable(e.to_string());
        }
    };
    let Some(msek) = mail.msek else {
        return DavCustody::NoKey;
    };
    DavCustody::Ready(DavStoreContext {
        actor_id,
        msek: msek.to_array(),
        prior_mseks: Zeroizing::new(mail.prior_mseks.iter().map(|m| m.to_array()).collect()),
    })
}

/// [`dav_custody`] folded to "have a context or not"
/// ([`DavCustody::into_context`]): `None` both when no MSEK is minted and when
/// the custody read failed. For the callers that degrade the same way on
/// either (an empty address-book list, a write that refuses); the calendar
/// load keeps the answers apart through `fauna_client_caldav`'s read instead.
pub async fn dav_store_context(
    mail: &dyn MailStore,
    actor_id: [u8; 32],
) -> Option<DavStoreContext> {
    dav_custody(mail, actor_id).await.into_context()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_helpers::FakeMailStore;
    use crate::test_nest::block_on;
    use fauna_core::data::MailConfig;

    /// No `msek` minted yet (mail/CalDAV never enabled) — `NoKey`, the
    /// legitimate disabled state, not an error; the folded helper reads `None`.
    #[test]
    fn no_msek_reads_no_key() {
        let custody = block_on(dav_custody(&FakeMailStore::empty(), [7; 32]));
        assert_eq!(custody, DavCustody::NoKey);
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
        } = match block_on(dav_custody(&mail, [8; 32])) {
            DavCustody::Ready(ctx) => ctx,
            other => panic!("expected Ready, got {other:?}"),
        };
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

    /// A custody the runtime cannot read is `Unreadable` — kept apart from the
    /// disabled state (a wait, not an absence), never a panic, never a stale
    /// MSEK; the folded helper still degrades it to `None`.
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
            async fn clear_rotation(
                &self,
                _: fauna_core::secret::SecretArray32,
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
            async fn mark_token_minted(
                &self,
                _: String,
                _: u64,
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
            async fn publish(&self) -> Result<(), crate::StoreError> {
                unreachable!()
            }
            async fn walk(&self) -> Result<(), crate::StoreError> {
                unreachable!()
            }
        }
        assert!(matches!(
            block_on(dav_custody(&Broken, [9; 32])),
            DavCustody::Unreadable(e) if e.contains("no runtime")
        ));
        assert_eq!(block_on(dav_store_context(&Broken, [9; 32])), None);
    }
}
