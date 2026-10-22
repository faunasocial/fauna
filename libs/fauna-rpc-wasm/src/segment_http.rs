//! `GET /api/v1/segments/{kind}/{actor}/{id}` (and its `/meta` sidecar) as a
//! **streamed** browser `fetch` — the browser twin of native
//! `fauna_sync_engine::nest_client::SyncClient::segment_body` /
//! `segment_meta_body`, the byte half of the cold device's bulk adoption on
//! web (`account-replica-posture.md` § Replica posture → *Corollary — the cold
//! device*, the web clause's bulk half).
//!
//! A segment has no size ceiling (`message-segment-store.md` § Segment size),
//! so a half is never collected: [`SegmentHalfBody::next_chunk`] hands one
//! transport chunk at a time to the caller, which writes it to the store's
//! segment area, and refuses a body past its bound *while reading it* — a
//! declared `Content-Length` over the bound before any byte moves, a
//! length-less (chunked) body as soon as the running total passes it, the
//! stream cancelled. The bearer is the caller's (`Authorization: Bearer`, a
//! script-held token, no credentials mode): the custodian's route answers a
//! foreign origin under the open, credential-less CORS the content-addressed
//! byte routes answer (`message-segment-store.md` § Implementation status
//! today).
//!
//! The reader drives the `ReadableStream` through `js_sys::Reflect`, the
//! technique `fauna_core::file_download::read_body_capped` uses, so it adds no
//! third-party manifest entry.

use js_sys::{Function, Reflect, Uint8Array};
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;

/// Which half of a segment pair to fetch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SegmentHalfPath {
    /// The `.dat` — the CARv2 container.
    Dat,
    /// The `.meta` sidecar.
    Meta,
}

/// Why a segment half did not stream.
#[derive(Debug, thiserror::Error)]
pub enum SegmentHttpError {
    /// The body is longer than the caller's bound — declared so, or found so
    /// mid-stream.
    #[error("the body exceeds the {max_bytes}-byte bound")]
    TooLarge {
        /// The bound the body passed.
        max_bytes: u64,
    },
    /// The nest answered with a non-2xx status.
    #[error("HTTP {status}")]
    Status {
        /// The HTTP status the nest returned.
        status: u16,
    },
    /// The request or a stream read failed. `context` names which step.
    #[error("{context}: {detail}")]
    Transport {
        context: &'static str,
        detail: String,
    },
}

fn transport(context: &'static str, detail: impl std::fmt::Display) -> SegmentHttpError {
    SegmentHttpError::Transport {
        context,
        detail: detail.to_string(),
    }
}

fn js_err(context: &'static str, e: JsValue) -> SegmentHttpError {
    transport(context, format!("{e:?}"))
}

fn method(target: &JsValue, name: &'static str) -> Result<Function, SegmentHttpError> {
    Reflect::get(target, &JsValue::from_str(name))
        .map_err(|e| js_err(name, e))?
        .dyn_into::<Function>()
        .map_err(|_| transport(name, "not a function"))
}

/// One segment half's body, streamed under its byte bound.
pub struct SegmentHalfBody {
    reader: JsValue,
    read: Function,
    total: u64,
    max_bytes: u64,
}

impl SegmentHalfBody {
    /// The next chunk, or `None` at the end of the body. A chunk that would
    /// carry the body past its bound is refused, not returned, and the stream
    /// is cancelled.
    pub async fn next_chunk(&mut self) -> Result<Option<Vec<u8>>, SegmentHttpError> {
        let step = JsFuture::from(js_sys::Promise::from(
            self.read
                .call0(&self.reader)
                .map_err(|e| js_err("segment body read", e))?,
        ))
        .await
        .map_err(|e| js_err("segment body read", e))?;
        let done = Reflect::get(&step, &JsValue::from_str("done"))
            .map_err(|e| js_err("segment body read", e))?
            .as_bool()
            .unwrap_or(true);
        if done {
            return Ok(None);
        }
        let value = Reflect::get(&step, &JsValue::from_str("value"))
            .map_err(|e| js_err("segment body read", e))?;
        let chunk = Uint8Array::new(&value).to_vec();
        self.total = self.total.saturating_add(chunk.len() as u64);
        if self.total > self.max_bytes {
            // Stop the transfer; a failed cancel changes nothing about the refusal.
            if let Ok(cancel) = method(&self.reader, "cancel") {
                let _ = cancel.call0(&self.reader);
            }
            return Err(SegmentHttpError::TooLarge {
                max_bytes: self.max_bytes,
            });
        }
        Ok(Some(chunk))
    }
}

/// GET one half of segment `(kind, actor_hex, segment_id)` from the nest at
/// `base_url` under `bearer`, bounded at `max_bytes`. Returns the body to
/// stream; a non-2xx answer or a declared length past the bound is refused
/// here, before any body byte is read.
pub async fn segment_half(
    base_url: &str,
    bearer: &str,
    kind: &str,
    actor_hex: &str,
    segment_id: u32,
    half: SegmentHalfPath,
    max_bytes: u64,
) -> Result<SegmentHalfBody, SegmentHttpError> {
    let suffix = match half {
        SegmentHalfPath::Dat => "",
        SegmentHalfPath::Meta => "/meta",
    };
    let url = format!(
        "{}/api/v1/segments/{kind}/{actor_hex}/{segment_id}{suffix}",
        base_url.trim_end_matches('/')
    );
    let resp = gloo_net::http::Request::get(&url)
        .header("authorization", &format!("Bearer {bearer}"))
        .send()
        .await
        .map_err(|e| transport("segment GET", e))?;
    let status = resp.status();
    if !(200..300).contains(&status) {
        return Err(SegmentHttpError::Status { status });
    }
    if let Some(declared) = resp
        .headers()
        .get("content-length")
        .and_then(|v| v.trim().parse::<u64>().ok())
        && declared > max_bytes
    {
        return Err(SegmentHttpError::TooLarge { max_bytes });
    }
    let stream: JsValue = resp
        .body()
        .ok_or_else(|| transport("segment GET", "the reply carries no body stream"))?
        .into();
    let reader = method(&stream, "getReader")?
        .call0(&stream)
        .map_err(|e| js_err("getReader", e))?;
    let read = method(&reader, "read")?;
    Ok(SegmentHalfBody {
        reader,
        read,
        total: 0,
        max_bytes,
    })
}
