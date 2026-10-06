//! Shared mailing-list helpers (`docs/goal/behavior/mail-mass-mailing.md`).
//!
//! Pure, WASM-safe core (no tokio/DNS/parser) behind the deployment-managed,
//! RFC-8058-compliant mailing-list surface: the one-click-unsubscribe token
//! derivation ([`token`]), the RFC 2369 + RFC 8058 list-header set
//! ([`headers`]), and the per-recipient strip-and-prepend that stamps that set
//! onto a client-composed message ([`stamp`]). The nest derives + verifies
//! tokens and stamps headers in the `send_list_message` fan-out over this one
//! impl (priority #2); a future client preview could reuse the builder unchanged.
//!
//! The DB tables (`mail_lists` / `mail_list_members`), the list-mode
//! submission discriminator, the rate accounting, the HTTPS/mailto unsubscribe
//! handlers, and the `fauna.bridges.*` list RPCs are nest-side.

pub mod headers;
pub mod stamp;
pub mod token;

pub use headers::{ListHeaderInputs, list_headers};
pub use stamp::stamp_list_headers_on_message;
pub use token::{UNSUBSCRIBE_SECRET_BYTES, UNSUBSCRIBE_TOKEN_BYTES, UnsubscribeTokenGenerator};
