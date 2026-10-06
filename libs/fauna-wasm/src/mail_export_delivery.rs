//! The web half of `mail-export.md` § Download flow — the browser twin of
//! `rpc_glue::NativeExportArchiveDelivery`.
//!
//! Only the two platform ends live here: the authenticated streaming GET of the
//! session's blob, and the place the recovered archive is written. Everything
//! between them — unwrapping the session key, opening the frames in order,
//! refusing an over-long frame or a missing terminator — is
//! `MailExportMachine::download` in shared Rust and must not be re-implemented
//! on either side of this file (priority #2).
//!
//! The browser APIs themselves (`fetch`, the origin-private file system, the
//! anchor that hands the finished file to the browser's own download) are the
//! SPA's, reached through the [`MailExportSavePort`] object the page passes in
//! (`apps/fauna-web/src/lib/mail-export-save.ts`). This side adds the two
//! things that port cannot know: the session bearer, and the refusal contract
//! — a sink dropped before `finish()` aborts, so a refused archive leaves
//! nothing behind, exactly as the native sink's `.part` file is removed on drop.
//!
//! **Nothing is buffered whole on either half** (§ Quota composition caps a
//! blob at 10 GiB): the body is read one slice at a time and every opened frame
//! is written out before the next slice is pulled.

use async_trait::async_trait;
use fauna_client_mail_settings::DispatchError;
use fauna_client_mail_settings::export::{
    ArchiveFileSink, ExportArchiveDelivery, SealedBlobStream,
};
use fauna_rpc_wasm::WsRpcClient as InnerClient;
use wasm_bindgen::prelude::*;

#[wasm_bindgen]
extern "C" {
    /// The SPA's save port: `{ openDownload(downloadUrl, bearer),
    /// createArchive(fileName) }`.
    pub type MailExportSavePort;

    /// Begin the GET. Resolves a [`BlobReader`]; rejects on a non-2xx answer.
    #[wasm_bindgen(method, catch, js_name = openDownload)]
    async fn open_download(
        this: &MailExportSavePort,
        download_url: String,
        bearer: String,
    ) -> Result<JsValue, JsValue>;

    /// Create the temporary destination. Resolves an [`ArchiveSink`].
    #[wasm_bindgen(method, catch, js_name = createArchive)]
    async fn create_archive(
        this: &MailExportSavePort,
        file_name: String,
    ) -> Result<JsValue, JsValue>;

    /// `{ next(): Promise<Uint8Array | null>, cancel(): void }`.
    type BlobReader;

    #[wasm_bindgen(method, catch)]
    async fn next(this: &BlobReader) -> Result<JsValue, JsValue>;

    #[wasm_bindgen(method)]
    fn cancel(this: &BlobReader);

    /// `{ write(bytes): Promise<void>, finish(): Promise<string>, abort(): void }`.
    type ArchiveSink;

    #[wasm_bindgen(method, catch)]
    async fn write(this: &ArchiveSink, bytes: js_sys::Uint8Array) -> Result<JsValue, JsValue>;

    #[wasm_bindgen(method, catch)]
    async fn finish(this: &ArchiveSink) -> Result<JsValue, JsValue>;

    #[wasm_bindgen(method)]
    fn abort(this: &ArchiveSink);
}

/// A rejected port promise as the machine's error. The message is what the
/// user reads on `error-message`, so it carries the step and the browser's own
/// reason.
fn port_error(step: &str, e: JsValue) -> DispatchError {
    let reason = e
        .dyn_ref::<js_sys::Error>()
        .map(|err| String::from(err.message()))
        .or_else(|| e.as_string())
        .unwrap_or_else(|| format!("{e:?}"));
    DispatchError::InvalidState(format!("{step}: {reason}"))
}

pub(crate) struct WebExportArchiveDelivery {
    nest: InnerClient,
    port: MailExportSavePort,
}

impl WebExportArchiveDelivery {
    pub(crate) fn new(nest: InnerClient, port: MailExportSavePort) -> Self {
        Self { nest, port }
    }
}

#[async_trait(?Send)]
impl ExportArchiveDelivery for WebExportArchiveDelivery {
    async fn open_download(
        &self,
        download_url: String,
    ) -> Result<Box<dyn SealedBlobStream>, DispatchError> {
        // The session's own bearer, from the same cache the WS-RPC connection
        // mints through — the route answers 404 for a foreign actor, a
        // still-running session and a missing blob alike (§ Cross-actor
        // isolation), so the message carries the status and invents no
        // distinction the nest refused to make.
        let bearer = self.nest.bearer(false).await.map_err(|e| {
            DispatchError::InvalidState(format!("download the export archive: {e}"))
        })?;
        let reader = self
            .port
            .open_download(download_url, bearer)
            .await
            .map_err(|e| port_error("download the export archive", e))?;
        Ok(Box::new(WebBlobStream {
            reader: reader.unchecked_into(),
            ended: false,
        }))
    }

    async fn create_archive(
        &self,
        file_name: String,
    ) -> Result<Box<dyn ArchiveFileSink>, DispatchError> {
        let sink = self
            .port
            .create_archive(file_name)
            .await
            .map_err(|e| port_error("prepare the export archive", e))?;
        Ok(Box::new(WebArchiveFile {
            sink: sink.unchecked_into(),
            finished: false,
        }))
    }
}

struct WebBlobStream {
    reader: BlobReader,
    ended: bool,
}

#[async_trait(?Send)]
impl SealedBlobStream for WebBlobStream {
    async fn next_slice(&mut self) -> Result<Option<Vec<u8>>, DispatchError> {
        let slice = self
            .reader
            .next()
            .await
            .map_err(|e| port_error("download the export archive", e))?;
        if slice.is_null() || slice.is_undefined() {
            self.ended = true;
            return Ok(None);
        }
        Ok(Some(js_sys::Uint8Array::new(&slice).to_vec()))
    }
}

impl Drop for WebBlobStream {
    fn drop(&mut self) {
        // A download refused mid-body (a bad frame, a failed write) must not
        // keep pulling up to 10 GiB the opener will never read.
        if !self.ended {
            self.reader.cancel();
        }
    }
}

/// The temporary file the recovered archive accumulates in.
///
/// The web twin of `NativeArchiveFile`'s `.part` + rename: the bytes land in an
/// origin-private temporary file the user cannot see, the browser download is
/// started only by `finish()`, and a sink dropped unfinished aborts — so a
/// truncated or still-running archive (§ Download flow step 4) never becomes a
/// file that reads as the user's whole mailbox.
struct WebArchiveFile {
    sink: ArchiveSink,
    finished: bool,
}

#[async_trait(?Send)]
impl ArchiveFileSink for WebArchiveFile {
    async fn write(&mut self, bytes: &[u8]) -> Result<(), DispatchError> {
        // A copy into a JS-owned buffer: the port's write is asynchronous, and
        // a view into wasm memory would dangle the moment that memory grows.
        self.sink
            .write(js_sys::Uint8Array::from(bytes))
            .await
            .map_err(|e| port_error("write the export archive", e))?;
        Ok(())
    }

    async fn finish(mut self: Box<Self>) -> Result<String, DispatchError> {
        let saved = self
            .sink
            .finish()
            .await
            .map_err(|e| port_error("save the export archive", e))?;
        // Handed to the browser: nothing is left for `Drop` to abort.
        self.finished = true;
        Ok(saved.as_string().unwrap_or_default())
    }
}

impl Drop for WebArchiveFile {
    fn drop(&mut self) {
        if !self.finished {
            self.sink.abort();
        }
    }
}
