//! The length-prefixed dag-cbor frame loop, shared by every async IPC server.
//!
//! `[u32 LE length][canonical dag-cbor payload]` — the same frame the blocking
//! [`SyncPipeClient`](crate::sync_pipe_client) speaks, read and written here on
//! tokio streams so both transports ([`unix_transport`](crate::unix_transport)
//! on linux/macOS, [`pipe_transport`](crate::pipe_transport) on windows) use one
//! copy.
//!
//! **This is where [`MAX_FRAME_SIZE`](crate::MAX_FRAME_SIZE) is enforced
//! server-side** (via [`crate::checked_frame_len`], shared with the sync
//! [`read_frame`](crate::sync_pipe_client::read_frame) twin). Before this
//! module the windows pipe servers each shadowed it with a private local
//! `const` of the same value, so the crate's public constant — the one every
//! *client* reads (`sync_pipe_client`, `unix_transport`) and the one
//! `apps/windows.md` § IPC documents — bound the clients only. Raising it there
//! would have left the servers rejecting at the old bound: a protocol-wide break
//! introduced by editing the constant that looks authoritative, with nothing in
//! the tree to catch it. One reader, one owner.

use std::io;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::{broadcast, mpsc};

use crate::{RefuseUndecodedRequest, encode_frame};

/// Depth of the outgoing-frame channel `handle_conn`'s read loop and event
/// forwarder both feed into the writer task. Bounded so a slow/wedged client
/// still applies backpressure (a full channel blocks the sender, same as the
/// old code blocking on `write_frame(...).await` directly) rather than
/// growing without limit.
const WRITE_QUEUE_DEPTH: usize = 32;

/// Read one length-prefixed frame, returning the payload bytes (prefix stripped).
/// The async sibling of `sync_pipe_client::read_frame`.
///
/// An oversized length prefix is `InvalidData` — the caller drops the connection
/// rather than allocating whatever a peer asked for.
///
/// TODO(phase-2): on the sync agent's `ProvisionCapability` path this buffer (and
/// serde's deserialize scratch) hold un-zeroized capability key / bearer bytes. A
/// `Zeroizing<Vec<u8>>` buffer plus a zeroizing deserializer would close the
/// residual; the in-struct copies are already zeroize-on-drop.
pub(crate) async fn read_frame<R: AsyncReadExt + Unpin>(reader: &mut R) -> io::Result<Vec<u8>> {
    let mut len_buf = [0u8; 4];
    reader.read_exact(&mut len_buf).await?;
    let len = crate::checked_frame_len(len_buf)?;
    let mut payload = vec![0u8; len];
    reader.read_exact(&mut payload).await?;
    Ok(payload)
}

/// Serve one connection: interleave request→response round-trips with pushed
/// events until the client closes the connection or sends a frame with no
/// readable `id`. The loop [`unix_transport`](crate::unix_transport)'s sockets and
/// [`pipe_transport`](crate::pipe_transport)'s named pipes both drive —
/// they differ only in how the stream is obtained, never in how it's served
/// once split into a plain [`AsyncRead`]/[`AsyncWrite`] pair.
///
/// The read loop never races `read_frame` against anything else.
/// `AsyncReadExt::read_exact` is not cancellation-safe: the previous shape
/// built a **fresh** `read_frame` future every iteration and raced it in a
/// `select!` against `event_rx.recv()`, so a request frame split across reads
/// lost whatever bytes that future had already consumed the moment the event
/// branch won — sometimes surfacing as a clean close (garbage decoded as an
/// oversized length), sometimes as a `read_exact` left parked forever on
/// bytes the client already sent as part of the first frame and will never
/// send again. Responses and pushed events instead funnel through
/// one channel to a dedicated writer task, so the read loop only ever awaits
/// the next frame from the client and a partial read simply resumes next
/// poll, whatever else is happening on the connection.
///
/// **A request whose method this build cannot decode is refused, not fatal.**
/// An app newer than this agent names verbs the agent has never heard of; the
/// frame's `id` still decodes, so that one request is answered with
/// [`RefuseUndecodedRequest`]'s typed refusal and the connection keeps serving
/// (`transport.md` § Rule 3 in full). Dropping the connection instead made the
/// app report "agent unreachable", indistinguishable from a dead agent.
pub(crate) async fn handle_conn<S, Req, Resp, Ev, H, F>(
    stream: S,
    handler: H,
    mut event_rx: broadcast::Receiver<Ev>,
) where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    Req: serde::de::DeserializeOwned + Send + 'static,
    Resp: serde::Serialize + RefuseUndecodedRequest + Send + 'static,
    Ev: serde::Serialize + Clone + Send + 'static,
    H: Fn(Req) -> F + Send + 'static,
    F: std::future::Future<Output = Resp> + Send + 'static,
{
    tracing::debug!("client connected");
    let (mut reader, mut writer) = tokio::io::split(stream);

    // Both the read loop (responses) and the event forwarder below encode
    // eagerly and hand off owned bytes, so the writer task needs no knowledge
    // of `Resp`/`Ev` and never holds a reference across an await.
    let (frame_tx, mut frame_rx) = mpsc::channel::<Vec<u8>>(WRITE_QUEUE_DEPTH);

    let writer_task = tokio::spawn(async move {
        while let Some(frame) = frame_rx.recv().await {
            if writer.write_all(&frame).await.is_err() {
                break; // client gone; nothing left to write to
            }
        }
    });

    let event_frame_tx = frame_tx.clone();
    let event_task = tokio::spawn(async move {
        loop {
            match event_rx.recv().await {
                Ok(evt) => match encode_frame(&evt) {
                    Ok(frame) => {
                        if event_frame_tx.send(frame).await.is_err() {
                            break; // writer task gone (a write already failed)
                        }
                    }
                    Err(e) => tracing::warn!("failed to encode event: {e}"),
                },
                // A slow client that fell behind: skip the dropped events, keep serving.
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    });

    // Whatever a handler ties to this connection
    // (`conn_scope::hold_for_connection`) lives until the loop below ends — the
    // client closing, or a frame we cannot read — and drops with `scope`.
    let scope = std::sync::Arc::new(crate::conn_scope::ConnectionScope::default());

    loop {
        let payload = match read_frame(&mut reader).await {
            Ok(p) => p,
            Err(_) => break, // client closed / EOF / oversized
        };
        let resp = match crate::decode_payload::<Req>(&payload) {
            Ok(req) => scope.run(handler(req)).await,
            Err(e) => match crate::decode_frame_id(&payload) {
                Some(id) => {
                    tracing::warn!("refusing request {id}: its method does not decode ({e})");
                    Resp::refuse_undecoded_request(id)
                }
                None => {
                    tracing::warn!("failed to decode request: {e}");
                    break;
                }
            },
        };
        let frame = match encode_frame(&resp) {
            Ok(f) => f,
            Err(e) => {
                tracing::warn!("failed to encode response: {e}");
                break;
            }
        };
        if frame_tx.send(frame).await.is_err() {
            break; // writer task gone (a write already failed)
        }
    }

    // The event forwarder has no other exit signal (the broadcast channel
    // normally outlives any one connection), so it's cancelled explicitly;
    // dropping our own `frame_tx` clone then lets the writer task's channel
    // close once the event task's clone drops too, so it flushes whatever was
    // already queued and exits.
    event_task.abort();
    drop(frame_tx);
    let _ = writer_task.await;
    tracing::debug!("client disconnected");
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use tokio::io::duplex;
    use tokio::sync::broadcast;

    use super::*;
    use crate::sync::{
        Event, EventKind, FileStatus, Request, RequestMethod, Response, ResponsePayload,
        ResponseResult, SyncStatusInfo,
    };

    async fn echo_get_sync_status(req: Request) -> Response {
        match req.method {
            RequestMethod::GetSyncStatus => Response {
                id: req.id,
                result: ResponseResult::Ok(ResponsePayload::SyncStatus(SyncStatusInfo {
                    connected: true,
                    ..Default::default()
                })),
            },
            _ => Response {
                id: req.id,
                result: ResponseResult::Err("unhandled".into()),
            },
        }
    }

    /// Read frames until both a `Response` and an `Event` have been seen —
    /// order isn't guaranteed (the writer task and the event forwarder race) —
    /// and return the response, or `None` if the stream closes first.
    /// Distinguishing the two frame kinds by "which type decodes" mirrors
    /// `sync_pipe_client::route_frame`, the real client's own trick for
    /// demultiplexing one stream carrying both.
    async fn collect_response_and_event<R: AsyncRead + Unpin>(reader: &mut R) -> Option<Response> {
        let mut response = None;
        let mut saw_event = false;
        while response.is_none() || !saw_event {
            let payload = read_frame(reader).await.ok()?;
            if let Ok(resp) = crate::decode_payload::<Response>(&payload) {
                response = Some(resp);
            } else if crate::decode_payload::<Event>(&payload).is_ok() {
                saw_event = true;
            } else {
                panic!("frame decoded as neither Response nor Event");
            }
        }
        response
    }

    /// tier_1: a request frame split across two writes, with a broadcast event
    /// landing in the gap, must still round-trip. Confirmed red on the
    /// `select!`-per-iteration loop this replaces: `read_exact` isn't
    /// cancellation-safe, so a fresh `read_frame` future built every iteration
    /// loses whatever it already read from the split frame the moment the event
    /// branch wins the race — and the failure isn't always a clean close.
    /// Depending on how the leftover bytes decode as the next "length prefix",
    /// the old loop can also leave a `read_exact` parked forever on bytes the
    /// client already sent as part of the first frame and will never send
    /// again. Both shapes look identical from here — no reply ever arrives —
    /// so the assertion is a bounded wait, never a specific error variant.
    #[tokio::test]
    async fn split_request_frame_survives_an_event_landing_mid_frame() {
        let (client, server) = duplex(8192);
        let (event_tx, event_rx) = broadcast::channel::<Event>(4);

        tokio::spawn(handle_conn(server, echo_get_sync_status, event_rx));

        let (mut client_read, mut client_write) = tokio::io::split(client);

        let req = Request {
            id: 7,
            method: RequestMethod::GetSyncStatus,
        };
        let frame = encode_frame(&req).unwrap();
        let split = frame.len() / 2;
        assert!(
            split > 4 && split < frame.len(),
            "split point must land inside the payload, past the length prefix"
        );

        client_write.write_all(&frame[..split]).await.unwrap();
        // Give the server's read loop a chance to actually park mid-`read_exact`
        // on the incomplete frame before the event lands.
        tokio::time::sleep(Duration::from_millis(20)).await;
        event_tx
            .send(Event {
                event: EventKind::FileStatusChanged {
                    path: "report.bin".into(),
                    status: FileStatus::Synced,
                },
            })
            .unwrap();
        tokio::time::sleep(Duration::from_millis(20)).await;
        client_write.write_all(&frame[split..]).await.unwrap();

        let resp = match tokio::time::timeout(
            Duration::from_secs(2),
            collect_response_and_event(&mut client_read),
        )
        .await
        {
            Ok(Some(resp)) => resp,
            Ok(None) => panic!(
                "no reply — the connection closed before a reply arrived \
                 (the pre-fix loop decoded the split frame's leftover bytes \
                 as garbage and dropped the connection)"
            ),
            Err(_) => panic!(
                "no reply within the timeout — the pre-fix loop left a \
                 `read_exact` parked forever on bytes the client already sent"
            ),
        };

        assert_eq!(resp.id, 7);
        assert!(matches!(
            resp.result,
            ResponseResult::Ok(ResponsePayload::SyncStatus(_))
        ));
    }

    /// tier_1: a request naming a method this build does not know — an app
    /// newer than the agent — is refused for that one id with the typed
    /// refusal, and the connection keeps serving: the next request is answered
    /// (`transport.md` § Rule 3 in full). The newer app is a test-only twin of
    /// the request envelope with one extra verb.
    #[tokio::test]
    async fn an_unknown_method_is_refused_for_its_id_and_the_connection_keeps_serving() {
        #[derive(serde::Serialize)]
        enum NewerMethod {
            AddedInANewerApp { n: u32 },
        }
        #[derive(serde::Serialize)]
        struct NewerRequest {
            id: u64,
            method: NewerMethod,
        }

        let (client, server) = duplex(8192);
        let (_event_tx, event_rx) = broadcast::channel::<Event>(4);
        tokio::spawn(handle_conn(server, echo_get_sync_status, event_rx));
        let (mut client_read, mut client_write) = tokio::io::split(client);

        let next_reply = |payload: Vec<u8>| crate::decode_payload::<Response>(&payload).unwrap();

        client_write
            .write_all(
                &encode_frame(&NewerRequest {
                    id: 41,
                    method: NewerMethod::AddedInANewerApp { n: 1 },
                })
                .unwrap(),
            )
            .await
            .unwrap();
        let refusal = next_reply(
            tokio::time::timeout(Duration::from_secs(2), read_frame(&mut client_read))
                .await
                .expect("a reply, not a dropped connection")
                .expect("readable"),
        );
        assert_eq!(refusal.id, 41);
        match refusal.result {
            ResponseResult::Err(msg) => assert!(
                crate::sync::is_unsupported_method_refusal(&msg),
                "the typed refusal, got {msg:?}"
            ),
            other => panic!("expected the refusal, got {other:?}"),
        }

        client_write
            .write_all(
                &encode_frame(&Request {
                    id: 42,
                    method: RequestMethod::GetSyncStatus,
                })
                .unwrap(),
            )
            .await
            .unwrap();
        let resp = next_reply(
            tokio::time::timeout(Duration::from_secs(2), read_frame(&mut client_read))
                .await
                .expect("the connection still serves")
                .expect("readable"),
        );
        assert_eq!(resp.id, 42);
        assert!(matches!(
            resp.result,
            ResponseResult::Ok(ResponsePayload::SyncStatus(_))
        ));
    }

    /// tier_1: a frame with no readable `id` at all is still fatal — there is
    /// no call to answer.
    #[tokio::test]
    async fn a_frame_with_no_readable_id_ends_the_connection() {
        let (client, server) = duplex(8192);
        let (_event_tx, event_rx) = broadcast::channel::<Event>(4);
        let served = tokio::spawn(handle_conn(server, echo_get_sync_status, event_rx));
        let (_client_read, mut client_write) = tokio::io::split(client);
        client_write
            .write_all(&encode_frame(&"not a request").unwrap())
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(2), served)
            .await
            .expect("the connection ends")
            .expect("no panic");
    }

    /// tier_1: a value a handler holds for its connection
    /// (`conn_scope::hold_for_connection`) lives until the CLIENT closes the
    /// connection, and not a moment longer — the attachment the agent's push
    /// arm reads (`RequestMethod::AttachApp`).
    #[tokio::test]
    async fn a_value_held_for_the_connection_drops_when_the_client_closes() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};

        struct Attached(Arc<AtomicUsize>);
        impl Drop for Attached {
            fn drop(&mut self) {
                self.0.fetch_sub(1, Ordering::SeqCst);
            }
        }

        let count = Arc::new(AtomicUsize::new(0));
        let handler_count = Arc::clone(&count);
        let handler = move |req: Request| {
            let count = Arc::clone(&handler_count);
            async move {
                count.fetch_add(1, Ordering::SeqCst);
                let held = crate::conn_scope::hold_for_connection(Attached(count));
                Response {
                    id: req.id,
                    result: if held {
                        ResponseResult::Ok(ResponsePayload::Empty)
                    } else {
                        ResponseResult::Err("not held".into())
                    },
                }
            }
        };

        let (client, server) = duplex(8192);
        let (_event_tx, event_rx) = broadcast::channel::<Event>(4);
        let served = tokio::spawn(handle_conn(server, handler, event_rx));

        let (mut client_read, mut client_write) = tokio::io::split(client);
        let frame = encode_frame(&Request {
            id: 1,
            method: RequestMethod::AttachApp {
                app: Some("tui".into()),
            },
        })
        .unwrap();
        client_write.write_all(&frame).await.unwrap();
        let payload = tokio::time::timeout(Duration::from_secs(2), read_frame(&mut client_read))
            .await
            .expect("a reply")
            .expect("readable");
        let resp: Response = crate::decode_payload(&payload).unwrap();
        assert!(
            matches!(resp.result, ResponseResult::Ok(ResponsePayload::Empty)),
            "the handler ran inside the connection's scope"
        );
        assert_eq!(count.load(Ordering::SeqCst), 1, "attached while open");

        drop(client_write);
        drop(client_read);
        tokio::time::timeout(Duration::from_secs(2), served)
            .await
            .expect("the connection ends once the client closes")
            .expect("no panic");
        assert_eq!(
            count.load(Ordering::SeqCst),
            0,
            "the held value outlived its connection"
        );
    }
}
