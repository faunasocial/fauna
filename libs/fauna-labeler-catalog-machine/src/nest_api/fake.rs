//! In-memory fake for the page seam, for `LabelerCatalogMachine` lifecycle
//! tests. Records every gesture (method + args) so tests can assert the exact
//! call shape each gesture produces, and fixtures failures per method.

#![cfg(any(test, feature = "test-helpers"))]

use std::sync::Mutex;

use fauna_client_bridges::HolderInfo;

use super::{InspectResult, LabelerCatalogApiError, LabelerCatalogNestApi};
use crate::snapshots::LabelerCatalogEntry;

/// One recorded gesture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FakeCall {
    Inspect {
        labeler_id: Vec<u8>,
    },
    Subscribe {
        labeler_id: Vec<u8>,
        grant_id: Option<[u8; 16]>,
    },
    Unsubscribe {
        labeler_id: Vec<u8>,
    },
    /// `fauna.capabilities.mint` with the canonical `GrantBlob` bytes.
    MintGrant {
        grant_blob: Vec<u8>,
    },
    /// `fauna.capabilities.revoke`.
    RevokeGrant {
        grant_id: [u8; 16],
    },
    /// The holder-roster read the subscribe mint seals against.
    ContentProcessorHolders,
}

/// One fixtured `inspect` response, keyed by the `labeler_id` it answers for.
type InspectFixture = (Vec<u8>, Result<InspectResult, LabelerCatalogApiError>);

#[derive(Debug, Default)]
pub struct FakeLabelerCatalogNestApi {
    entries: Mutex<Vec<LabelerCatalogEntry>>,
    /// `Some` overrides the next `inspect` for a given `labeler_id` (hex).
    inspect_responses: Mutex<Vec<InspectFixture>>,
    subscribe_response: Mutex<Option<Result<(), LabelerCatalogApiError>>>,
    unsubscribe_response: Mutex<Option<Result<(), LabelerCatalogApiError>>>,
    /// `Some` makes `list` fail (refresh-error path).
    list_response: Mutex<Option<LabelerCatalogApiError>>,
    /// The content-processor roster `content_processor_holders` answers with
    /// (empty by default — no holder enrolled).
    holders: Mutex<Vec<HolderInfo>>,
    mint_grant_response: Mutex<Option<Result<(), LabelerCatalogApiError>>>,
    revoke_grant_response: Mutex<Option<Result<(), LabelerCatalogApiError>>>,
    calls: Mutex<Vec<FakeCall>>,
}

impl FakeLabelerCatalogNestApi {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_entries(&self, entries: Vec<LabelerCatalogEntry>) {
        *self.entries.lock().unwrap() = entries;
    }
    pub fn fail_list(&self, err: LabelerCatalogApiError) {
        *self.list_response.lock().unwrap() = Some(err);
    }
    pub fn set_inspect_response(
        &self,
        labeler_id: Vec<u8>,
        r: Result<InspectResult, LabelerCatalogApiError>,
    ) {
        self.inspect_responses.lock().unwrap().push((labeler_id, r));
    }
    pub fn set_subscribe_response(&self, r: Result<(), LabelerCatalogApiError>) {
        *self.subscribe_response.lock().unwrap() = Some(r);
    }
    pub fn set_unsubscribe_response(&self, r: Result<(), LabelerCatalogApiError>) {
        *self.unsubscribe_response.lock().unwrap() = Some(r);
    }
    /// The holder roster the next `content_processor_holders` answers with.
    pub fn set_holders(&self, holders: Vec<HolderInfo>) {
        *self.holders.lock().unwrap() = holders;
    }
    pub fn set_mint_grant_response(&self, r: Result<(), LabelerCatalogApiError>) {
        *self.mint_grant_response.lock().unwrap() = Some(r);
    }
    pub fn set_revoke_grant_response(&self, r: Result<(), LabelerCatalogApiError>) {
        *self.revoke_grant_response.lock().unwrap() = Some(r);
    }

    /// All recorded gestures, in order.
    pub fn calls(&self) -> Vec<FakeCall> {
        self.calls.lock().unwrap().clone()
    }
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
impl LabelerCatalogNestApi for FakeLabelerCatalogNestApi {
    async fn list(&self) -> Result<Vec<LabelerCatalogEntry>, LabelerCatalogApiError> {
        match self.list_response.lock().unwrap().clone() {
            Some(e) => Err(e),
            None => Ok(self.entries.lock().unwrap().clone()),
        }
    }

    async fn inspect(&self, labeler_id: Vec<u8>) -> Result<InspectResult, LabelerCatalogApiError> {
        self.calls.lock().unwrap().push(FakeCall::Inspect {
            labeler_id: labeler_id.clone(),
        });
        let mut responses = self.inspect_responses.lock().unwrap();
        if let Some(pos) = responses.iter().position(|(id, _)| *id == labeler_id) {
            return responses.remove(pos).1;
        }
        Err(LabelerCatalogApiError::NotFound {
            detail: "no fixture set for this labeler_id".into(),
        })
    }

    async fn subscribe(
        &self,
        labeler_id: Vec<u8>,
        grant_id: Option<[u8; 16]>,
    ) -> Result<(), LabelerCatalogApiError> {
        self.calls.lock().unwrap().push(FakeCall::Subscribe {
            labeler_id,
            grant_id,
        });
        self.subscribe_response
            .lock()
            .unwrap()
            .clone()
            .unwrap_or(Ok(()))
    }

    async fn unsubscribe(&self, labeler_id: Vec<u8>) -> Result<(), LabelerCatalogApiError> {
        self.calls
            .lock()
            .unwrap()
            .push(FakeCall::Unsubscribe { labeler_id });
        self.unsubscribe_response
            .lock()
            .unwrap()
            .clone()
            .unwrap_or(Ok(()))
    }

    async fn mint_grant(&self, grant_blob: Vec<u8>) -> Result<(), LabelerCatalogApiError> {
        self.calls
            .lock()
            .unwrap()
            .push(FakeCall::MintGrant { grant_blob });
        self.mint_grant_response
            .lock()
            .unwrap()
            .clone()
            .unwrap_or(Ok(()))
    }

    async fn revoke_grant(&self, grant_id: [u8; 16]) -> Result<(), LabelerCatalogApiError> {
        self.calls
            .lock()
            .unwrap()
            .push(FakeCall::RevokeGrant { grant_id });
        self.revoke_grant_response
            .lock()
            .unwrap()
            .clone()
            .unwrap_or(Ok(()))
    }

    async fn content_processor_holders(&self) -> Result<Vec<HolderInfo>, LabelerCatalogApiError> {
        self.calls
            .lock()
            .unwrap()
            .push(FakeCall::ContentProcessorHolders);
        Ok(self.holders.lock().unwrap().clone())
    }
}
