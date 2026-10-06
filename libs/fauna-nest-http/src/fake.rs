//! In-memory [`NestContentApi`] for consumers' logic tests.
//!
//! Mirrors `fauna_onboarding_machine::nest_api::FakeNestApi`: each
//! `(verb, path)` has a canned-response slot callers pre-populate via
//! [`FakeNestContentApi::set`]; an unset path returns an empty `200` body so
//! happy-path tests that don't care about the response shape stay terse.
//! Every call is recorded in [`FakeNestContentApi::calls`] for assertions.
//!
//! Gated behind `#[cfg(any(test, debug_assertions, feature = "test-helpers"))]`
//! — a consumer (e.g. `apps/fauna-linux`) testing logic that goes through the
//! trait turns on `fauna-nest-http/test-helpers` as a dev-dep feature.

#![cfg(any(test, debug_assertions, feature = "test-helpers"))]

use std::collections::HashMap;
use std::sync::Mutex;

use async_trait::async_trait;
use bytes::Bytes;

use crate::content::NestContentApi;
use crate::error::ApiError;

/// HTTP verb a [`FakeNestContentApi`] slot is keyed on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Verb {
    Get,
    GetWithQuery,
    PostJson,
    PostBytes,
    PostBytesKeyed,
    PostMultipartBlob,
    PutBytes,
    PutJson,
    PatchJson,
    Delete,
    HeadC2pa,
}

#[derive(Default)]
pub struct FakeNestContentApi {
    responses: Mutex<HashMap<(Verb, String), Result<Bytes, ApiError>>>,
    /// `head_has_c2pa`'s canned responses — kept apart from `responses` because
    /// the return type is `bool`, not `Bytes`.
    c2pa_responses: Mutex<HashMap<String, Result<bool, ApiError>>>,
    calls: Mutex<Vec<(Verb, String)>>,
}

impl FakeNestContentApi {
    pub fn new() -> Self {
        Self::default()
    }

    /// Pre-populate the response for one `(verb, path)`.
    pub fn set(&self, verb: Verb, path: &str, response: Result<Bytes, ApiError>) {
        self.responses
            .lock()
            .unwrap()
            .insert((verb, path.to_string()), response);
    }

    /// Convenience: a `2xx` response with `body`.
    pub fn set_ok(&self, verb: Verb, path: &str, body: impl Into<Bytes>) {
        self.set(verb, path, Ok(body.into()));
    }

    /// Convenience: an [`ApiError::Status`] response.
    pub fn set_status(&self, verb: Verb, path: &str, code: u16, message: &str) {
        self.set(
            verb,
            path,
            Err(ApiError::Status {
                code,
                message: message.to_string(),
            }),
        );
    }

    /// Pre-populate `head_has_c2pa`'s response for one path.
    pub fn set_c2pa(&self, path: &str, response: Result<bool, ApiError>) {
        self.c2pa_responses
            .lock()
            .unwrap()
            .insert(path.to_string(), response);
    }

    /// The `(verb, path)` of every call made so far, in order.
    pub fn calls(&self) -> Vec<(Verb, String)> {
        self.calls.lock().unwrap().clone()
    }

    fn dispatch(&self, verb: Verb, path: &str) -> Result<Bytes, ApiError> {
        self.calls.lock().unwrap().push((verb, path.to_string()));
        self.responses
            .lock()
            .unwrap()
            .get(&(verb, path.to_string()))
            .cloned()
            .unwrap_or_else(|| Ok(Bytes::new()))
    }
}

#[async_trait]
impl NestContentApi for FakeNestContentApi {
    async fn get(&self, path: &str) -> Result<Bytes, ApiError> {
        self.dispatch(Verb::Get, path)
    }
    async fn get_with_query(
        &self,
        path: &str,
        _params: &[(&str, &str)],
    ) -> Result<Bytes, ApiError> {
        self.dispatch(Verb::GetWithQuery, path)
    }
    async fn post_json(&self, path: &str, _body: &serde_json::Value) -> Result<Bytes, ApiError> {
        self.dispatch(Verb::PostJson, path)
    }
    async fn post_bytes(
        &self,
        path: &str,
        _content_type: &str,
        _body: Vec<u8>,
    ) -> Result<Bytes, ApiError> {
        self.dispatch(Verb::PostBytes, path)
    }
    async fn post_bytes_keyed(
        &self,
        path: &str,
        _content_hash_hex: &str,
        _body: Vec<u8>,
    ) -> Result<Bytes, ApiError> {
        self.dispatch(Verb::PostBytesKeyed, path)
    }
    async fn post_multipart_blob(
        &self,
        path: &str,
        _sidecar_cbor: Vec<u8>,
        _sealed_bytes: Vec<u8>,
    ) -> Result<Bytes, ApiError> {
        self.dispatch(Verb::PostMultipartBlob, path)
    }
    async fn put_bytes(
        &self,
        path: &str,
        _content_type: &str,
        _body: Vec<u8>,
    ) -> Result<Bytes, ApiError> {
        self.dispatch(Verb::PutBytes, path)
    }
    async fn put_json(&self, path: &str, _body: &serde_json::Value) -> Result<Bytes, ApiError> {
        self.dispatch(Verb::PutJson, path)
    }
    async fn patch_json(&self, path: &str, _body: &serde_json::Value) -> Result<Bytes, ApiError> {
        self.dispatch(Verb::PatchJson, path)
    }
    async fn delete(&self, path: &str) -> Result<Bytes, ApiError> {
        self.dispatch(Verb::Delete, path)
    }
    async fn head_has_c2pa(&self, path: &str) -> Result<bool, ApiError> {
        self.calls
            .lock()
            .unwrap()
            .push((Verb::HeadC2pa, path.to_string()));
        self.c2pa_responses
            .lock()
            .unwrap()
            .get(path)
            .cloned()
            .unwrap_or(Ok(false))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn unset_path_returns_empty_ok_and_records_the_call() {
        let fake = FakeNestContentApi::new();
        assert_eq!(&fake.get("/api/v1/quota").await.unwrap()[..], b"");
        assert_eq!(fake.calls(), vec![(Verb::Get, "/api/v1/quota".to_string())]);
    }

    #[tokio::test]
    async fn unset_c2pa_path_defaults_to_false_and_records_the_call() {
        let fake = FakeNestContentApi::new();
        assert!(!fake.head_has_c2pa("/api/v1/blob/deadbeef").await.unwrap());
        assert_eq!(
            fake.calls(),
            vec![(Verb::HeadC2pa, "/api/v1/blob/deadbeef".to_string())]
        );
    }

    #[tokio::test]
    async fn canned_c2pa_response_is_returned() {
        let fake = FakeNestContentApi::new();
        fake.set_c2pa("/api/v1/blob/deadbeef", Ok(true));
        assert!(fake.head_has_c2pa("/api/v1/blob/deadbeef").await.unwrap());
    }

    #[tokio::test]
    async fn canned_status_error_is_returned() {
        let fake = FakeNestContentApi::new();
        fake.set_status(Verb::PutJson, "/api/v1/profile/handle", 409, "handle taken");
        let err = fake
            .put_json("/api/v1/profile/handle", &serde_json::json!({}))
            .await
            .unwrap_err();
        assert_eq!(
            err,
            ApiError::Status {
                code: 409,
                message: "handle taken".into()
            }
        );
    }
}
