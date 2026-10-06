//! In-memory fake `FolderNestApi` for wizard-lifecycle tests.
//!
//! Records every call (method + args) so tests can assert the exact
//! `CreateFolderRequest` / per-device `SetPlaceRequest` sequence `submit()`
//! produces, and lets tests fixture failures per method.

#![cfg(any(test, feature = "test-helpers"))]

use std::sync::Mutex;

use super::types::*;
use async_trait::async_trait;

/// One recorded call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FakeCall {
    CreateFolder {
        req: CreateFolderRequest,
    },
    SetPlace {
        folder: String,
        place: SetPlaceRequest,
    },
}

#[derive(Debug, Default)]
pub struct FakeFolderNestApi {
    /// Pre-set the `list_folder_rows` result. `None` → return
    /// [`Self::folder_rows`] (empty by default).
    list_response: Mutex<Option<Result<Vec<FolderRow>, FolderApiError>>>,
    /// The rows a successful list returns. A successful `create_folder`
    /// appends its row here (ids run on from the last), the way a real nest
    /// lists what it just created — the resolver re-lists after a create to
    /// learn the new set's id.
    folder_rows: Mutex<Vec<FolderRow>>,
    /// Pre-set the create result. `None` → succeed.
    create_response: Mutex<Option<Result<(), FolderApiError>>>,
    /// Pre-set per-device-id place-set results, keyed by device_id hex.
    /// Any device not in the map succeeds.
    place_responses: Mutex<std::collections::HashMap<String, Result<(), FolderApiError>>>,
    calls: Mutex<Vec<FakeCall>>,
}

impl FakeFolderNestApi {
    pub fn new() -> Self {
        Self::default()
    }

    /// Fixture the rows a successful `list_folder_rows` returns.
    pub fn set_folder_rows(&self, rows: Vec<FolderRow>) {
        *self.folder_rows.lock().unwrap() = rows;
    }

    /// Fixture the rows by name alone, ids `1..` in order — for tests that
    /// care only about names.
    pub fn set_folder_names(&self, names: Vec<String>) {
        self.set_folder_rows(
            names
                .into_iter()
                .enumerate()
                .map(|(i, name)| FolderRow {
                    id: i as i64 + 1,
                    name,
                })
                .collect(),
        );
    }

    /// Fixture a `list_folder_rows` failure (or an explicit success list).
    pub fn set_list_response(&self, r: Result<Vec<FolderRow>, FolderApiError>) {
        *self.list_response.lock().unwrap() = Some(r);
    }

    pub fn set_create_response(&self, r: Result<(), FolderApiError>) {
        *self.create_response.lock().unwrap() = Some(r);
    }

    /// Make the place-set for `device_id` return `r`.
    pub fn set_place_response(&self, device_id: &str, r: Result<(), FolderApiError>) {
        self.place_responses
            .lock()
            .unwrap()
            .insert(device_id.to_string(), r);
    }

    /// All recorded calls, in order.
    pub fn calls(&self) -> Vec<FakeCall> {
        self.calls.lock().unwrap().clone()
    }
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
impl super::FolderNestApi for FakeFolderNestApi {
    // Not recorded as a `FakeCall` — the list is a read, and the wizard's own
    // call-sequence assertions are about writes.
    async fn list_folder_rows(&self) -> Result<Vec<FolderRow>, FolderApiError> {
        self.list_response
            .lock()
            .unwrap()
            .clone()
            .unwrap_or_else(|| Ok(self.folder_rows.lock().unwrap().clone()))
    }

    async fn create_folder(&self, req: CreateFolderRequest) -> Result<(), FolderApiError> {
        let name = req.name.clone();
        self.calls
            .lock()
            .unwrap()
            .push(FakeCall::CreateFolder { req });
        let result = self
            .create_response
            .lock()
            .unwrap()
            .clone()
            .unwrap_or(Ok(()));
        if result.is_ok() {
            let mut rows = self.folder_rows.lock().unwrap();
            let id = rows.iter().map(|r| r.id).max().unwrap_or(0) + 1;
            rows.push(FolderRow { id, name });
        }
        result
    }

    async fn set_place(&self, folder: &str, place: SetPlaceRequest) -> Result<(), FolderApiError> {
        let device_id = place.device_id.clone();
        self.calls.lock().unwrap().push(FakeCall::SetPlace {
            folder: folder.to_string(),
            place,
        });
        self.place_responses
            .lock()
            .unwrap()
            .get(&device_id)
            .cloned()
            .unwrap_or(Ok(()))
    }
}
