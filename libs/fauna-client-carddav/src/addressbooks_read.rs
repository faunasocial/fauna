//! The address-book read's two answers — `docs/goal/ui/contacts.md` § Errors &
//! edge cases (*The Address Book segment never waits on a credential*), the
//! address book's instance of the calendar read's rule
//! (`fauna-client-caldav`'s `calendars_read`).
//!
//! A custody read that finds no key is no answer of its own since the key mint
//! at first connect (`docs/goal/behavior/mail-credentials.md` § MSEK lifecycle
//! → *Every account holds the key*, ruling 7): it is the mint in flight, so the
//! read answers *unresolved* for it — the segment paints loading, and the
//! store-change notice that re-runs an unresolved read picks the key up when it
//! lands. An unreadable custody (the account store not up yet) is the same wait.
//!
//! Before this read, every app's load folded "custody not readable yet" and "no
//! key" into an empty list, which painted as an address book with no books —
//! the empty-address-book state contacts.md forbids for a wait.
//!
//! Unlike the calendar, the address book has no lazy provisioning on a read, so
//! a **loaded** answer may hold no books: with the key in hand an empty listing
//! is a real answer, not a refusal.
//!
//! The read keeps the books for a Rust caller ([`AddressbooksRead`]); its
//! payload-free [`AddressbooksReadState`] is the shape the faces hand across
//! beside each face's own rows.

use fauna_client_config::DavCustody;
use fauna_protocol::RpcRequester;
use fauna_protocol::bridge_routing::ListAddressbooksRequest;
use serde::{Deserialize, Serialize};

use crate::{CardDavClient, DavKind, DavRecipientKeys, DecodedAddressbook, ReadAddressbooksError};

/// What the address-book read answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AddressbooksRead {
    /// The read has not finished, the custody read itself failed because the
    /// account store is not up yet, or custody holds no key yet (the mint at
    /// first connect is in flight). A wait: re-run the read when the store
    /// changes. Never painted as an empty address book.
    Unresolved,
    /// The key is there and the listing came back — possibly with no books.
    Loaded {
        addressbooks: Vec<DecodedAddressbook>,
    },
}

/// [`AddressbooksRead`] without its books: what a face hands across and what a
/// page paints on. The serde form is `{"state": "unresolved" | "loaded"}`, the
/// shape the web SPA reads (computed in this process, never decoded off a
/// wire).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum AddressbooksReadState {
    /// Also the state before any read has run: a segment that has not read yet
    /// is loading, never empty.
    #[default]
    Unresolved,
    Loaded,
}

impl AddressbooksRead {
    /// The payload-free answer.
    pub fn state(&self) -> AddressbooksReadState {
        match self {
            AddressbooksRead::Unresolved => AddressbooksReadState::Unresolved,
            AddressbooksRead::Loaded { .. } => AddressbooksReadState::Loaded,
        }
    }

    /// The books when loaded, else none.
    pub fn addressbooks(&self) -> &[DecodedAddressbook] {
        match self {
            AddressbooksRead::Loaded { addressbooks } => addressbooks,
            AddressbooksRead::Unresolved => &[],
        }
    }
}

impl<R: RpcRequester> CardDavClient<R> {
    /// The Address Book segment's book load: the custody answer
    /// ([`fauna_client_config::dav_custody`]) decides *unresolved* without a
    /// call — an unreadable custody and one holding no key yet alike (the mint
    /// is in flight; ruling 7); with the key,
    /// [`Self::list_addressbooks_decoded`] runs over the whole key ring and its
    /// books are *loaded*. A list or unseal error is the call's error, as
    /// before.
    pub async fn read_addressbooks(
        &self,
        custody: &DavCustody,
    ) -> Result<AddressbooksRead, ReadAddressbooksError<R::Error>> {
        let ctx = match custody {
            DavCustody::Unreadable(_) | DavCustody::NoKey => {
                return Ok(AddressbooksRead::Unresolved);
            }
            DavCustody::Ready(ctx) => ctx,
        };
        let keys = DavRecipientKeys::from_mseks(DavKind::Card, &ctx.msek, &ctx.prior_mseks);
        let addressbooks = self
            .list_addressbooks_decoded(
                ListAddressbooksRequest {
                    actor_id: ctx.actor_id.to_vec(),
                },
                &keys,
            )
            .await?;
        Ok(AddressbooksRead::Loaded { addressbooks })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AddressbookMetadata, seal_addressbook_metadata};
    use fauna_client_config::DavStoreContext;
    use fauna_client_testkit::block_on;
    use fauna_protocol::bridge_routing::{AddressbookEntry, ListAddressbooksReply};

    const MSEK: [u8; 32] = [7u8; 32];

    /// A requester that must never be called — a wait answers without a call.
    struct NoCall;

    impl RpcRequester for NoCall {
        type Error = std::convert::Infallible;

        async fn request<Req, Reply>(
            &self,
            kind: &'static str,
            _payload: Req,
        ) -> Result<Reply, Self::Error>
        where
            Req: serde::Serialize,
            Reply: serde::de::DeserializeOwned,
        {
            panic!("an unresolved custody must not call the nest (called {kind})");
        }
    }

    /// Answers `list_addressbooks` with a fixed reply.
    struct Listing(ListAddressbooksReply);

    impl RpcRequester for Listing {
        type Error = std::convert::Infallible;

        async fn request<Req, Reply>(
            &self,
            kind: &'static str,
            _payload: Req,
        ) -> Result<Reply, Self::Error>
        where
            Req: serde::Serialize,
            Reply: serde::de::DeserializeOwned,
        {
            assert_eq!(kind, "fauna.bridges.list_addressbooks");
            let bytes = fauna_protocol::encode_canonical(&self.0).expect("encode");
            Ok(fauna_protocol::decode_strict(&bytes).expect("decode"))
        }
    }

    fn ready() -> DavCustody {
        DavCustody::Ready(DavStoreContext {
            actor_id: [0xAA; 32],
            msek: MSEK,
            prior_mseks: Default::default(),
        })
    }

    #[test]
    fn no_key_is_unresolved_without_a_call() {
        let read = block_on(CardDavClient::new(NoCall).read_addressbooks(&DavCustody::NoKey))
            .expect("read");
        assert_eq!(read, AddressbooksRead::Unresolved);
        assert_eq!(read.state(), AddressbooksReadState::Unresolved);
        assert!(read.addressbooks().is_empty());
    }

    #[test]
    fn an_unreadable_custody_is_unresolved_without_a_call() {
        let read = block_on(
            CardDavClient::new(NoCall)
                .read_addressbooks(&DavCustody::Unreadable("store not up".into())),
        )
        .expect("read");
        assert_eq!(read.state(), AddressbooksReadState::Unresolved);
    }

    #[test]
    fn with_the_key_the_books_are_loaded() {
        let meta = AddressbookMetadata {
            displayname: "Family".into(),
            description: String::new(),
        };
        let reply = ListAddressbooksReply {
            addressbooks: vec![AddressbookEntry {
                addressbook_id: vec![0xAB; 32],
                encrypted_metadata: seal_addressbook_metadata(&meta, &MSEK).expect("seal"),
                ctag: 1,
                highestmodseq: 1,
                card_count: 2,
                created_at: 100,
            }],
        };
        let read =
            block_on(CardDavClient::new(Listing(reply)).read_addressbooks(&ready())).expect("read");
        assert_eq!(read.state(), AddressbooksReadState::Loaded);
        assert_eq!(read.addressbooks().len(), 1);
        assert_eq!(read.addressbooks()[0].metadata.displayname, "Family");
    }

    /// The address book has no lazy provisioning: with the key, an empty
    /// listing is a real answer — loaded, with no books.
    #[test]
    fn with_the_key_an_empty_listing_is_loaded() {
        let read = block_on(
            CardDavClient::new(Listing(ListAddressbooksReply {
                addressbooks: vec![],
            }))
            .read_addressbooks(&ready()),
        )
        .expect("read");
        assert_eq!(read.state(), AddressbooksReadState::Loaded);
        assert!(read.addressbooks().is_empty());
    }

    #[test]
    fn a_segment_that_has_not_read_yet_is_unresolved() {
        assert_eq!(
            AddressbooksReadState::default(),
            AddressbooksReadState::Unresolved
        );
    }
}
