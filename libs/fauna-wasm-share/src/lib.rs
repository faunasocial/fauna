//! The private share link **viewer page's** wasm chunk
//! (`docs/goal/behavior/share-links.md` § The private-file extension → *The
//! viewer is a browser page and needs no account*): a thin binding over
//! [`fauna_client_share::viewer`], so the page decrypts with the same shared
//! Rust every app links — no second implementation of the crypto in
//! TypeScript. The page (`apps/fauna-web/src/lib/share-viewer/`) loads this
//! chunk alone: no account runtime, no identity store, no socket.
#![cfg(target_arch = "wasm32")]

use fauna_client_share::viewer::{self, OpenedShare, ViewerStart};
use wasm_bindgen::prelude::*;

// Each wasm chunk is its own module with its own Rust runtime, so a hook
// installed in one chunk covers none of the others (see the
// `fauna-wasm-panic-hook` crate doc comment).
#[wasm_bindgen(start)]
fn panic_hook_start() {
    fauna_wasm_panic_hook::install("fauna-wasm-share");
}

/// Test-only: deliberately panics, so an e2e can assert the hook above really
/// names this chunk in the browser console. Compiled out of every
/// non-`test-helpers` build.
#[cfg(feature = "test-helpers")]
#[wasm_bindgen(js_name = panicForTestOnly)]
pub fn panic_for_test_only() {
    panic!("deliberate test panic");
}

/// A link to open, read off the address bar.
#[wasm_bindgen]
pub struct ShareLink {
    token: String,
    fragment: String,
}

#[wasm_bindgen]
impl ShareLink {
    #[wasm_bindgen(getter)]
    pub fn token(&self) -> String {
        self.token.clone()
    }
    #[wasm_bindgen(getter)]
    pub fn fragment(&self) -> String {
        self.fragment.clone()
    }
}

/// `location.pathname` + `location.hash` → the link to open, or `undefined`
/// for the generic page (no fragment, or not a share path).
#[wasm_bindgen(js_name = viewerStart)]
pub fn viewer_start(pathname: &str, hash: &str) -> Option<ShareLink> {
    match viewer::viewer_start(pathname, hash) {
        ViewerStart::Generic => None,
        ViewerStart::Open { token, fragment } => Some(ShareLink { token, fragment }),
    }
}

#[wasm_bindgen(js_name = manifestPath)]
pub fn manifest_path(token: &str) -> String {
    viewer::manifest_path(token)
}

#[wasm_bindgen(js_name = chunkPath)]
pub fn chunk_path(token: &str, index: u32) -> String {
    viewer::chunk_path(token, index as usize)
}

/// The sentence for a refused fetch (410 / 451 / 404 / anything else).
#[wasm_bindgen(js_name = statusText)]
pub fn status_text(status: u16) -> String {
    viewer::status_text(status).to_string()
}

/// A private link whose envelope is open.
#[wasm_bindgen]
pub struct OpenedLink {
    inner: OpenedShare,
}

/// Open a link over the manifest path's answer. Throws the sentence to show
/// (a string) when the link is not one, or does not open.
#[wasm_bindgen(js_name = openShare)]
pub fn open_share(token: &str, fragment: &str, manifest: &[u8]) -> Result<OpenedLink, JsValue> {
    viewer::open_share(token, fragment, manifest)
        .map(|inner| OpenedLink { inner })
        .map_err(|e| JsValue::from_str(e.text()))
}

#[wasm_bindgen]
impl OpenedLink {
    #[wasm_bindgen(getter)]
    pub fn filename(&self) -> String {
        self.inner.filename().to_string()
    }
    #[wasm_bindgen(getter, js_name = sizeText)]
    pub fn size_text(&self) -> String {
        self.inner.size_text()
    }
    #[wasm_bindgen(getter, js_name = chunkCount)]
    pub fn chunk_count(&self) -> u32 {
        self.inner.chunk_count() as u32
    }
    #[wasm_bindgen(getter, js_name = contentType)]
    pub fn content_type(&self) -> String {
        self.inner.content_type().to_string()
    }
    /// `"image"`, `"audio"`, `"video"`, `"text"` or `"none"` (download only).
    #[wasm_bindgen(getter)]
    pub fn preview(&self) -> String {
        self.inner.preview().as_str().to_string()
    }
    /// The verified file from its ciphertext chunks (an array of
    /// `Uint8Array`, in index order). Throws the sentence to show on any
    /// mismatch — no unverified byte is ever returned.
    pub fn assemble(&self, chunks: js_sys::Array) -> Result<Vec<u8>, JsValue> {
        let chunks: Vec<Vec<u8>> = chunks
            .iter()
            .map(|c| js_sys::Uint8Array::new(&c).to_vec())
            .collect();
        self.inner
            .assemble(&chunks)
            .map_err(|e| JsValue::from_str(e.text()))
    }
}

/// The page's fixed text.
#[wasm_bindgen]
pub struct ViewerText {
    inner: viewer::ViewerText,
}

#[wasm_bindgen]
impl ViewerText {
    #[wasm_bindgen(getter)]
    pub fn title(&self) -> String {
        self.inner.title.to_string()
    }
    #[wasm_bindgen(getter, js_name = genericBody)]
    pub fn generic_body(&self) -> String {
        self.inner.generic_body.to_string()
    }
    #[wasm_bindgen(getter)]
    pub fn loading(&self) -> String {
        self.inner.loading.to_string()
    }
    #[wasm_bindgen(getter)]
    pub fn download(&self) -> String {
        self.inner.download.to_string()
    }
    #[wasm_bindgen(getter, js_name = keepNote)]
    pub fn keep_note(&self) -> String {
        self.inner.keep_note.to_string()
    }
}

#[wasm_bindgen(js_name = viewerText)]
pub fn viewer_text() -> ViewerText {
    ViewerText {
        inner: viewer::viewer_text(),
    }
}
