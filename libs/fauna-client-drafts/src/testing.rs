//! An in-memory `__drafts` nest for tests of this crate and its consumers —
//! the one fake every drafts test reaches for, so the leave-door and launch
//! tests of each rail (`fauna-client-caldav`, the tokio-native app shells) run
//! against the same `fauna.drafts.{get,put}` behaviour `DraftsSync`'s own
//! tests pin. Built only under `cfg(test)` or the `test-helpers` feature, so a
//! shipped artifact carries none of it.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use fauna_protocol::RpcRequester;
use fauna_protocol::drafts::{
    GetDraftsReply, GetDraftsRequest, KIND_GET, KIND_PUT, PutDraftsReply, PutDraftsRequest,
};
use serde_bytes::ByteBuf;

/// Stateful in-memory nest: a `path → opaque sealed blob` map plus a count of
/// the puts it served. Shared behind an `Arc` ([`SharedDraftsNest`]) so two
/// `DraftsSync` instances can model two devices of one identity.
#[derive(Default)]
pub struct MemoryDraftsNest {
    /// Every sealed blob at rest, by drafts path.
    pub stored: Mutex<HashMap<String, Vec<u8>>>,
    /// How many `fauna.drafts.put` calls this nest has served.
    pub puts: Mutex<u32>,
}

/// The [`RpcRequester`] over a shared [`MemoryDraftsNest`].
pub struct SharedDraftsNest(pub Arc<MemoryDraftsNest>);

impl RpcRequester for SharedDraftsNest {
    type Error = std::convert::Infallible;
    async fn request<Req, Reply>(
        &self,
        kind: &'static str,
        payload: Req,
    ) -> Result<Reply, Self::Error>
    where
        Req: serde::Serialize,
        Reply: serde::de::DeserializeOwned,
    {
        let bytes = fauna_protocol::encode_canonical(&payload).expect("encode request");
        let reply = match kind {
            KIND_PUT => {
                let req: PutDraftsRequest =
                    fauna_protocol::decode_strict(&bytes).expect("decode put");
                *self.0.puts.lock().unwrap() += 1;
                self.0
                    .stored
                    .lock()
                    .unwrap()
                    .insert(req.path, req.blob.into_vec());
                fauna_protocol::encode_canonical(&PutDraftsReply {
                    ok: true,
                    extra: Default::default(),
                })
            }
            KIND_GET => {
                let req: GetDraftsRequest =
                    fauna_protocol::decode_strict(&bytes).expect("decode get");
                let blob = self
                    .0
                    .stored
                    .lock()
                    .unwrap()
                    .get(&req.path)
                    .cloned()
                    .map(ByteBuf::from);
                fauna_protocol::encode_canonical(&GetDraftsReply {
                    blob,
                    extra: Default::default(),
                })
            }
            other => panic!("unexpected kind {other}"),
        }
        .expect("encode reply");
        Ok(fauna_protocol::decode_strict(&reply).expect("decode reply"))
    }
}
