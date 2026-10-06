//! S3-compatible blob store implementation.
//!
//! **Reserved, no production caller today.** Its one caller, the admin
//! storage migration, was retired 2026-09-26 (the primary blob backend is
//! artifact-set local disk — `nest/common.md` § Blob Store → Backend). It is
//! kept compiling and tested for the deferred S3 backup-destination pass
//! (`behavior/backup-destinations.md` § Second destination kind). As a `pub`
//! item of the library crate it trips no `dead_code` lint, so it needs no allow.

use std::sync::OnceLock;

use anyhow::Result;
use async_trait::async_trait;
use aws_sdk_s3::Client;
use aws_sdk_s3::config::SharedHttpClient;
use aws_sdk_s3::primitives::ByteStream;
use aws_smithy_http_client::{Builder as HttpClientBuilder, tls};
use fauna_core::data::ContentHash;

use crate::blob_store::BlobStoreBackend;

/// The HTTPS client every `S3BlobStore` shares.
///
/// The nest deliberately takes neither of `aws-sdk-s3`'s TLS features and
/// builds this itself, so that the S3 path uses **rustls 0.23 with `ring`** —
/// the same TLS stack and the same crypto provider as the rest of the nest.
/// Taking the SDK's `rustls` feature instead would link a second, older TLS
/// stack (rustls 0.21 + hyper 0.14), and its `default-https-client` feature
/// would enable rustls' `aws_lc_rs` alongside the workspace's `ring`, which
/// leaves rustls with no crate-feature default provider at all. Full rationale
/// in the workspace manifest's `aws-smithy-http-client` entry.
///
/// Shared rather than per-store because `S3BlobStore::new` is called once per
/// user config, and a shared client means one connection pool instead of N.
fn https_client() -> SharedHttpClient {
    static CLIENT: OnceLock<SharedHttpClient> = OnceLock::new();
    CLIENT
        .get_or_init(|| {
            HttpClientBuilder::new()
                .tls_provider(tls::Provider::Rustls(
                    tls::rustls_provider::CryptoMode::Ring,
                ))
                .build_https()
        })
        .clone()
}

pub struct S3BlobStore {
    client: Client,
    bucket: String,
    prefix: String,
}

impl S3BlobStore {
    /// Create a new S3BlobStore with explicit parameters.
    /// Used for per-user stores.
    pub async fn new(
        bucket: String,
        prefix: String,
        endpoint: &str,
        region: &str,
        access_key_id: &str,
        secret_access_key: &str,
    ) -> Result<Self> {
        let creds = aws_sdk_s3::config::Credentials::new(
            access_key_id,
            secret_access_key,
            None,
            None,
            "fauna-user-config",
        );
        let s3_config = aws_sdk_s3::config::Builder::new()
            // Required, and its absence is a *runtime panic* rather than a
            // build error: `aws-sdk-s3` demands a behavior major version from
            // either this call or its `behavior-version-latest` feature, and
            // the nest takes the SDK with `default-features = false`, so it
            // gets neither by default. Set explicitly here rather than via the
            // feature, so the requirement is visible at the call site instead
            // of depending on feature unification elsewhere in the graph.
            .behavior_version(aws_sdk_s3::config::BehaviorVersion::latest())
            .endpoint_url(endpoint)
            .region(aws_sdk_s3::config::Region::new(region.to_string()))
            .credentials_provider(creds)
            .force_path_style(true)
            .http_client(https_client())
            .build();
        let client = Client::from_conf(s3_config);
        Ok(Self {
            client,
            bucket,
            prefix,
        })
    }

    fn key(&self, hash: &ContentHash) -> String {
        let hex = hex::encode(hash.digest());
        if self.prefix.is_empty() {
            format!("{}/{}/{}", &hex[..2], &hex[2..4], hex)
        } else {
            format!("{}/{}/{}/{}", self.prefix, &hex[..2], &hex[2..4], hex)
        }
    }
}

#[async_trait]
impl BlobStoreBackend for S3BlobStore {
    async fn put(&self, hash: &ContentHash, data: &[u8]) -> Result<()> {
        let key = self.key(hash);
        self.client
            .put_object()
            .bucket(&self.bucket)
            .key(&key)
            .body(ByteStream::from(data.to_vec()))
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("S3 put error: {e}"))?;
        Ok(())
    }

    async fn get(&self, hash: &ContentHash) -> Result<Option<Vec<u8>>> {
        let key = self.key(hash);
        match self
            .client
            .get_object()
            .bucket(&self.bucket)
            .key(&key)
            .send()
            .await
        {
            Ok(output) => {
                let body = output
                    .body
                    .collect()
                    .await
                    .map_err(|e| anyhow::anyhow!("S3 body read error: {e}"))?;
                Ok(Some(body.to_vec()))
            }
            Err(sdk_err) => {
                if sdk_err
                    .as_service_error()
                    .is_some_and(|e| e.is_no_such_key())
                {
                    Ok(None)
                } else {
                    Err(anyhow::anyhow!("S3 get error: {sdk_err}"))
                }
            }
        }
    }

    /// S3 serves byte ranges natively: the request's `Range` header asks for
    /// exactly the bytes a seek wants, never the whole object.
    async fn get_range(
        &self,
        hash: &ContentHash,
        start: u64,
        end_inclusive: u64,
    ) -> Result<Option<Vec<u8>>> {
        if end_inclusive < start {
            return Ok(self.exists(hash).await?.then(Vec::new));
        }
        let key = self.key(hash);
        match self
            .client
            .get_object()
            .bucket(&self.bucket)
            .key(&key)
            .range(format!("bytes={start}-{end_inclusive}"))
            .send()
            .await
        {
            Ok(output) => {
                let body = output
                    .body
                    .collect()
                    .await
                    .map_err(|e| anyhow::anyhow!("S3 body read error: {e}"))?;
                Ok(Some(body.to_vec()))
            }
            Err(sdk_err) => {
                if sdk_err
                    .as_service_error()
                    .is_some_and(|e| e.is_no_such_key())
                {
                    Ok(None)
                } else {
                    Err(anyhow::anyhow!("S3 ranged get error: {sdk_err}"))
                }
            }
        }
    }

    async fn exists(&self, hash: &ContentHash) -> Result<bool> {
        let key = self.key(hash);
        match self
            .client
            .head_object()
            .bucket(&self.bucket)
            .key(&key)
            .send()
            .await
        {
            Ok(_) => Ok(true),
            Err(sdk_err) => {
                if sdk_err.as_service_error().is_some_and(|e| e.is_not_found()) {
                    Ok(false)
                } else {
                    Err(anyhow::anyhow!("S3 head error: {sdk_err}"))
                }
            }
        }
    }

    async fn len(&self, hash: &ContentHash) -> Result<Option<u64>> {
        let key = self.key(hash);
        match self
            .client
            .head_object()
            .bucket(&self.bucket)
            .key(&key)
            .send()
            .await
        {
            // A HEAD with no `content_length` is a malformed response, not a
            // zero-byte object -- report it rather than hand back a length the
            // caller would compare against.
            Ok(out) => Ok(Some(out.content_length().map(|n| n as u64).ok_or_else(
                || anyhow::anyhow!("S3 head returned no content_length for {key}"),
            )?)),
            Err(sdk_err) => {
                if sdk_err.as_service_error().is_some_and(|e| e.is_not_found()) {
                    Ok(None)
                } else {
                    Err(anyhow::anyhow!("S3 head error: {sdk_err}"))
                }
            }
        }
    }

    async fn exists_batch(&self, hashes: &[ContentHash]) -> Result<Vec<bool>> {
        use futures_util::stream::{self, StreamExt};
        const MAX_CONCURRENT_HEAD: usize = 32;
        let futs: Vec<_> = hashes.iter().map(|h| self.exists(h)).collect();
        let results: Vec<Result<bool>> = stream::iter(futs)
            .buffer_unordered(MAX_CONCURRENT_HEAD)
            .collect()
            .await;
        results.into_iter().collect()
    }

    async fn delete(&self, hash: &ContentHash) -> Result<()> {
        let key = self.key(hash);
        self.client
            .delete_object()
            .bucket(&self.bucket)
            .key(&key)
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("S3 delete error: {e}"))?;
        Ok(())
    }

    async fn usage_bytes(&self) -> Result<u64> {
        Ok(0) // tracked via blob_metadata table
    }

    async fn list_all_hashes(&self) -> Result<Vec<ContentHash>> {
        let mut hashes = Vec::new();
        let prefix = if self.prefix.is_empty() {
            None
        } else {
            Some(format!("{}/", self.prefix))
        };
        let mut continuation_token: Option<String> = None;

        loop {
            let mut req = self.client.list_objects_v2().bucket(&self.bucket);
            if let Some(ref p) = prefix {
                req = req.prefix(p);
            }
            if let Some(ref token) = continuation_token {
                req = req.continuation_token(token);
            }

            let resp = req
                .send()
                .await
                .map_err(|e| anyhow::anyhow!("S3 list error: {e}"))?;

            for obj in resp.contents() {
                if let Some(key) = obj.key() {
                    // Key format: {prefix}/{xx}/{yy}/{hash_hex}
                    let hash_hex = key.rsplit('/').next().unwrap_or("");
                    if let Ok(arr) = fauna_core::hex32::decode(hash_hex) {
                        hashes.push(ContentHash::from_digest_raw(arr));
                    }
                }
            }

            // A truncated page MUST carry a next token. If it does not, stop:
            // reassigning `None` here would re-issue the identical tokenless
            // request forever, spinning this task and growing `hashes` without
            // bound. The endpoint is client-supplied (a backup destination
            // carries its own `s3_endpoint` over the wire), so a hostile or
            // merely buggy S3-compatible server must not be able to do that.
            match resp.next_continuation_token() {
                Some(token) if resp.is_truncated() == Some(true) => {
                    continuation_token = Some(token.to_string());
                }
                _ => break,
            }
        }

        Ok(hashes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use wiremock::matchers::{method, path, query_param, query_param_is_missing};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// A `ListObjectsV2` response body over the given hex keys.
    fn list_body(hash_hexes: &[String], next_token: Option<&str>) -> String {
        let contents: String = hash_hexes
            .iter()
            .map(|hex| {
                format!(
                    "<Contents><Key>{}/{}/{hex}</Key></Contents>",
                    &hex[..2],
                    &hex[2..4]
                )
            })
            .collect();
        let truncated = next_token.is_some();
        let token = next_token
            .map(|t| format!("<NextContinuationToken>{t}</NextContinuationToken>"))
            .unwrap_or_default();
        format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\
             <ListBucketResult><Name>test-bucket</Name>\
             <IsTruncated>{truncated}</IsTruncated>{contents}{token}</ListBucketResult>"
        )
    }

    /// A recognisable, stable digest so the expected request path is literal.
    fn test_hash() -> ContentHash {
        ContentHash::from_digest_raw([0xab; 32])
    }

    /// `{hex[..2]}/{hex[2..4]}/{hex}` for an all-`0xab` digest, under the bucket
    /// (the store sets `force_path_style`, so the bucket is the first segment).
    const EXPECTED_PATH: &str = concat!(
        "/test-bucket/ab/ab/",
        "abababababababababababababababababababababababababababababababab"
    );

    async fn store_against(server: &MockServer) -> S3BlobStore {
        S3BlobStore::new(
            "test-bucket".to_string(),
            String::new(),
            &server.uri(),
            "us-east-1",
            "test-access-key",
            "test-secret-key",
        )
        .await
        .expect("store construction")
    }

    /// The load-bearing test for the TLS-stack swap: the SDK client is built
    /// with **our** `http_client` (`https_client()`), and `aws-sdk-s3` fails at
    /// request time — not construction time — when no HTTP client is wired. So
    /// a request that actually arrives at the server proves the supplied client
    /// is accepted and drives real traffic.
    ///
    /// It also pins that `build_https()` still serves plain-`http://` endpoints,
    /// which the nest's `S3BackendConfig.endpoint` allows (a local MinIO is the
    /// motivating case) and which an https-only connector would silently break.
    #[tokio::test]
    async fn put_reaches_the_endpoint_through_the_supplied_http_client() {
        let server = MockServer::start().await;
        Mock::given(method("PUT"))
            .and(path(EXPECTED_PATH))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;

        store_against(&server)
            .await
            .put(&test_hash(), b"blob-bytes")
            .await
            .expect("put should succeed against the mock endpoint");
        // `expect(1)` is asserted on drop.
    }

    #[tokio::test]
    async fn get_returns_the_object_body() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(EXPECTED_PATH))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"blob-bytes".to_vec()))
            .expect(1)
            .mount(&server)
            .await;

        let got = store_against(&server)
            .await
            .get(&test_hash())
            .await
            .expect("get should succeed");
        assert_eq!(got.as_deref(), Some(&b"blob-bytes"[..]));
    }

    /// A missing blob must read as `Ok(None)`, not an error — the GC and
    /// dedup paths branch on it.
    #[tokio::test]
    async fn get_maps_no_such_key_to_none() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(EXPECTED_PATH))
            .respond_with(ResponseTemplate::new(404).set_body_string(
                "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\
                 <Error><Code>NoSuchKey</Code><Message>The specified key does not exist.</Message>\
                 </Error>",
            ))
            .mount(&server)
            .await;

        let got = store_against(&server)
            .await
            .get(&test_hash())
            .await
            .expect("a missing key is not an error");
        assert_eq!(got, None);
    }

    /// The bucket-level list request is path-style with a trailing slash:
    /// `GET /test-bucket/?list-type=2`.
    const EXPECTED_LIST_PATH: &str = "/test-bucket/";

    /// `list_all_hashes` must follow the continuation token across pages.
    #[tokio::test]
    async fn list_all_hashes_follows_pagination() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(EXPECTED_LIST_PATH))
            .and(query_param_is_missing("continuation-token"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(list_body(&["ab".repeat(32)], Some("page-2-token"))),
            )
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(EXPECTED_LIST_PATH))
            .and(query_param("continuation-token", "page-2-token"))
            .respond_with(
                ResponseTemplate::new(200).set_body_string(list_body(&["cd".repeat(32)], None)),
            )
            .mount(&server)
            .await;

        let got = store_against(&server)
            .await
            .list_all_hashes()
            .await
            .expect("list should succeed");
        assert_eq!(
            got,
            vec![
                ContentHash::from_digest_raw([0xab; 32]),
                ContentHash::from_digest_raw([0xcd; 32]),
            ],
            "both pages' hashes should be collected, in order"
        );
    }

    /// A response claiming `IsTruncated=true` while omitting
    /// `NextContinuationToken` must TERMINATE, not re-issue the same tokenless
    /// request forever. The endpoint is client-supplied (backup destinations
    /// carry `s3_endpoint` over the wire), so a hostile or merely buggy
    /// S3-compatible server could otherwise spin a nest task in an infinite
    /// request loop with `hashes` growing without bound.
    ///
    /// The timeout is a generous hang-guard, not a latency assertion — a green
    /// run returns in milliseconds and pays nothing for it (convention 14).
    #[tokio::test]
    async fn list_all_hashes_terminates_when_truncated_without_a_token() {
        const HANG_GUARD: Duration = Duration::from_secs(30);
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(EXPECTED_LIST_PATH))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                list_body(&["ab".repeat(32)], None).replace(
                    "<IsTruncated>false</IsTruncated>",
                    "<IsTruncated>true</IsTruncated>",
                ),
            ))
            .mount(&server)
            .await;

        let store = store_against(&server).await;
        let got = tokio::time::timeout(HANG_GUARD, store.list_all_hashes())
            .await
            .expect("list_all_hashes must terminate on a truncated-but-tokenless response")
            .expect("list should succeed");
        assert_eq!(got, vec![ContentHash::from_digest_raw([0xab; 32])]);
    }

    #[tokio::test]
    async fn exists_reports_presence_from_head() {
        let present = MockServer::start().await;
        Mock::given(method("HEAD"))
            .and(path(EXPECTED_PATH))
            .respond_with(ResponseTemplate::new(200))
            .mount(&present)
            .await;
        assert!(
            store_against(&present)
                .await
                .exists(&test_hash())
                .await
                .expect("head should succeed")
        );

        let absent = MockServer::start().await;
        Mock::given(method("HEAD"))
            .and(path(EXPECTED_PATH))
            .respond_with(ResponseTemplate::new(404))
            .mount(&absent)
            .await;
        assert!(
            !store_against(&absent)
                .await
                .exists(&test_hash())
                .await
                .expect("a 404 head is not an error")
        );
    }
}
