//! tier_1: **a search page's resolver does not keep a departed identity's
//! index launcher alive.**
//!
//! Proof obligation (`docs/goal/architecture/transport-connection.md`
//! § Connection lifecycle → *No dialer outlives its owner*): the launcher's
//! flush driver lives exactly as long as the launcher (its `Drop` cancels the
//! driver), and the driver publishes through the identity's `NestClient`. The
//! local-search resolver is handed to a search manager that page glue owns,
//! with a lifetime this crate cannot see. On windows, a manager a page never
//! disposed kept the launcher of an identity the app had switched away from,
//! and its flush driver retried as that identity once a minute, seven minutes
//! after the switch. So the resolver holds the launcher weakly: once the
//! session's own holders let go, the launcher drops (and its driver with it)
//! even though the resolver is still registered, and it resolves to no local
//! arm, which the page renders as no local rows.

use std::sync::Arc;

use fauna_client::{AuthClient, NestClient};
use fauna_client_conversations::{LauncherLocalIndex, MailKeyCache, NestMailIndexLauncher};
use fauna_client_index::{LocatedDraft, LocatedMessage, MailContentLookup};
use fauna_client_search::LocalIndexResolver;
use fauna_core::identity::ActorKeypair;

struct NoContent;

impl MailContentLookup for NoContent {
    fn locate(&self, _message_id: &str) -> Option<LocatedMessage> {
        None
    }

    fn locate_draft(&self, _content_id: &str) -> Option<LocatedDraft> {
        None
    }
}

#[tokio::test]
async fn a_registered_resolver_does_not_keep_the_launcher_alive() {
    // Never connected: nothing here dials, and nothing needs to.
    let nest = NestClient::with_auth(Arc::new(AuthClient::new(
        "http://127.0.0.1:0".into(),
        ActorKeypair::from_secret([7u8; 32]),
    )));
    let launcher = NestMailIndexLauncher::new(
        Arc::clone(&nest),
        MailKeyCache::new(
            Arc::clone(&nest),
            Arc::new(fauna_client_config::test_helpers::FakeMailStore::empty()),
        ),
        None,
    );
    let resolver = LauncherLocalIndex::new(Arc::clone(&launcher), Arc::new(NoContent));
    let watch = Arc::downgrade(&launcher);

    drop(launcher);

    assert!(
        watch.upgrade().is_none(),
        "the resolver a search page holds must not pin the launcher (and its flush driver)"
    );
    assert!(
        resolver.resolve().await.is_none(),
        "a departed launcher resolves to no local arm"
    );
}
