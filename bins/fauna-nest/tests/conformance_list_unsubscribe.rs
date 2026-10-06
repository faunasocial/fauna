//! **mail-mass-mailing — full-route acceptance for the RFC 8058 one-click
//! List-Unsubscribe HTTPS endpoint** (`mail-mass-mailing.md` § The HTTPS
//! endpoint; #4).
//!
//! Proves `GET/POST /list/unsubscribe?t=<token>` is mounted on the real
//! `build_router` — the same axum app `main.rs` serves — and behaves per RFC
//! 8058 §3.3 against a live in-memory `CacheDb` seeded with a list + a member
//! carrying a known token:
//!
//! - **POST** with the `List-Unsubscribe=One-Click` body + a matching token →
//!   200 and the member's `unsubscribed_at` is set; a second POST is idempotent
//!   ("already unsubscribed"); an unknown token → 404; a missing One-Click body
//!   → 400 (and never unsubscribes); a malformed-shape token → 404.
//! - **GET** is read-only: it renders the confirm page (never unsubscribes),
//!   and a malformed token renders the "not recognized" page rather than
//!   reflecting attacker markup.
//!
//! - **GET `/list/<list-id>/help`** — the default RFC 2369 `List-Help` target
//!   (§ RFC 2369 list headers) is mounted, renders the static subscribe /
//!   unsubscribe explanation, and never echoes the path id or member data.
//!
//! The token derivation + the DB flip are unit-tested in `fauna-mail` /
//! `db::mail_lists`; this test owns the **full-route** integration (route
//! mounted, query + body extracted, status/body mapped, db mutated).

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use fauna_nest::db::CacheDb;
use fauna_nest::routes::AppState;
use tower::ServiceExt; // oneshot

const OWNER: [u8; 32] = [0x33u8; 32];
const TOKEN: &str = "abcdEFGH1234_-zyXW9876543210ABCD"; // 32-char base64url shape

/// A fresh in-memory nest with one list owning one member whose cached
/// `one_click_unsubscribe_token` is [`TOKEN`].
async fn nest_with_member() -> Arc<AppState> {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let (list_id, _alias) = db
        .create_list(
            &OWNER,
            "fauna.test",
            "news",
            Some("News"),
            None,
            None,
            None,
            None,
        )
        .await
        .unwrap();
    db.add_member(&list_id, "alice@example.com", TOKEN)
        .await
        .unwrap();
    Arc::new(AppState::for_test(db))
}

async fn get(state: &Arc<AppState>, token: &str) -> (StatusCode, String) {
    let app = fauna_nest::build_router(state.clone());
    let req = Request::builder()
        .uri(format!("/list/unsubscribe?t={token}"))
        .method("GET")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024)
        .await
        .unwrap();
    (status, String::from_utf8(bytes.to_vec()).unwrap())
}

async fn post(state: &Arc<AppState>, token: &str, body: &str) -> (StatusCode, String) {
    let app = fauna_nest::build_router(state.clone());
    let req = Request::builder()
        .uri(format!("/list/unsubscribe?t={token}"))
        .method("POST")
        .header("content-type", "application/x-www-form-urlencoded")
        .body(Body::from(body.to_string()))
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024)
        .await
        .unwrap();
    (status, String::from_utf8(bytes.to_vec()).unwrap())
}

/// Whether `alice@example.com` is currently subscribed (unsubscribed_at NULL).
async fn alice_subscribed(state: &Arc<AppState>) -> bool {
    let (sub, _unsub) = {
        // The list is the only one owned by OWNER.
        let lists = state.db.list_lists_for_actor(&OWNER).await.unwrap();
        state.db.count_members(&lists[0].list_id).await.unwrap()
    };
    sub == 1
}

#[tokio::test]
async fn one_click_post_unsubscribes_then_is_idempotent() {
    let state = nest_with_member().await;
    assert!(alice_subscribed(&state).await, "seeded subscribed");

    let (status, body) = post(&state, TOKEN, "List-Unsubscribe=One-Click").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.to_lowercase().contains("unsubscribed"), "{body}");
    assert!(
        !alice_subscribed(&state).await,
        "POST flips unsubscribed_at"
    );

    // A second click is idempotent.
    let (status2, body2) = post(&state, TOKEN, "List-Unsubscribe=One-Click").await;
    assert_eq!(status2, StatusCode::OK, "{body2}");
    assert!(body2.to_lowercase().contains("already"), "{body2}");
}

#[tokio::test]
async fn post_without_one_click_body_is_400_and_does_not_unsubscribe() {
    let state = nest_with_member().await;
    let (status, _body) = post(&state, TOKEN, "something-else").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
        alice_subscribed(&state).await,
        "a non-One-Click POST must never unsubscribe"
    );
}

#[tokio::test]
async fn post_unknown_token_is_404() {
    let state = nest_with_member().await;
    // Well-shaped but unseeded token.
    let (status, _) = post(
        &state,
        "ZZZZ0000zzzz1111-_ABCDEFGHIJKLmno",
        "List-Unsubscribe=One-Click",
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(alice_subscribed(&state).await);
}

#[tokio::test]
async fn post_malformed_token_shape_is_404() {
    let state = nest_with_member().await;
    // `.` is URI-safe but not in the base64url token charset → rejected before
    // any db lookup.
    let (status, _) = post(&state, "bad.token", "List-Unsubscribe=One-Click").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(alice_subscribed(&state).await);
}

#[tokio::test]
async fn get_is_read_only_and_renders_confirm_form() {
    let state = nest_with_member().await;
    let (status, body) = get(&state, TOKEN).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        body.contains("Confirm unsubscribe"),
        "renders the confirm button: {body}"
    );
    assert!(body.contains("method=\"post\""), "the button POSTs: {body}");
    assert!(
        alice_subscribed(&state).await,
        "GET is read-only — it must not unsubscribe"
    );
}

#[tokio::test]
async fn get_malformed_token_does_not_reflect_markup() {
    let state = nest_with_member().await;
    // Percent-encoded `<script>alert(1)</script>` — the Query extractor decodes
    // it, the handler sees the raw markup and (shape-invalid) refuses to echo it.
    let (status, body) = get(&state, "%3Cscript%3Ealert%281%29%3C%2Fscript%3E").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        !body.contains("<script>"),
        "must not reflect attacker markup: {body}"
    );
    assert!(body.to_lowercase().contains("not recognized"), "{body}");
}

async fn get_path(state: &Arc<AppState>, path: &str) -> (StatusCode, String) {
    let app = fauna_nest::build_router(state.clone());
    let req = Request::builder()
        .uri(path)
        .method("GET")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024)
        .await
        .unwrap();
    (status, String::from_utf8(bytes.to_vec()).unwrap())
}

#[tokio::test]
async fn list_help_page_explains_subscribe_and_unsubscribe() {
    let state = nest_with_member().await;
    let lists = state.db.list_lists_for_actor(&OWNER).await.unwrap();
    let list_id = uuid::Uuid::from_bytes(lists[0].list_id).to_string();
    let (status, body) = get_path(&state, &format!("/list/{list_id}/help")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let lower = body.to_lowercase();
    assert!(lower.contains("to unsubscribe"), "{body}");
    assert!(lower.contains("to subscribe"), "{body}");
    assert!(
        !body.contains("alice@example.com"),
        "no member data: {body}"
    );
}

#[tokio::test]
async fn list_help_page_does_not_reflect_the_path_id() {
    let state = nest_with_member().await;
    let (status, body) = get_path(&state, "/list/%3Cscript%3Ex%3C%2Fscript%3E/help").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(!body.contains("<script>"), "{body}");
}
