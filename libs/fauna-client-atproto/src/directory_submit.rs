//! Shared PLC-directory submission mechanics for [`crate::tombstone`] and
//! [`crate::recovery_fork`] — both submit a signed op via the identical
//! `POST {base}/{did}` shape and non-success handling (`submit_fork`'s own
//! doc comment already called this out: "for the reason `submit_tombstone`
//! gives"). Each caller keeps its own error type, wrapping the detail string
//! this returns.

/// `POST {base}/{did}` with `body` as `application/json` — the PLC
/// directory's submit format (dag-cbor is only ever the signing encoding).
/// `Ok(())` on any successful status; `Err(detail)` carrying the response
/// body on failure — a rejected op's refusal reason is the only diagnostic
/// the user gets. Serialized by the caller rather than through reqwest's
/// `json` helper: this crate takes reqwest with default features off (it has
/// to compile to wasm), so the helper is absent.
pub(crate) async fn post_plc_op(
    directory_base_url: &str,
    did: &str,
    body: Vec<u8>,
) -> Result<(), String> {
    let url = format!("{}/{did}", directory_base_url.trim_end_matches('/'));
    let client = reqwest::Client::new();
    let req = client
        .post(&url)
        .header("content-type", "application/json")
        .body(body);
    #[cfg(not(target_arch = "wasm32"))]
    let req = req.timeout(std::time::Duration::from_secs(30));
    let resp = req.send().await.map_err(|e| e.to_string())?;
    let status = resp.status();
    if status.is_success() {
        return Ok(());
    }
    let detail = resp.text().await.unwrap_or_default();
    Err(format!("HTTP {} — {}", status.as_u16(), detail.trim()))
}
