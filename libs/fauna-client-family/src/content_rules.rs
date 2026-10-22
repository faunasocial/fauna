//! The guardian's **content rules** write — the shared rule editor's save at
//! its guardian mounting (`docs/goal/behavior/nest-rules.md` § The shared rule
//! editor (b); `family-safety.md` § App surface → *Content rules*).
//!
//! The rule document rides the ward's policy as `ContentPolicy.document`
//! through `fauna.family.policy.update`, like the feature tier's sub-document
//! (`fauna_client_features::ward_policy_with`): the ward's policy is re-read
//! and only the content pillar is written. Every other optional pillar is sent
//! absent — absent means unchanged — so saving the rules never rewrites a
//! screen-time limit, the Notify knob, the bridge-DM gate or a feature limit
//! from a stale copy; the four reach knobs, which the wire carries
//! non-optional, go back as the nest just reported them.

use fauna_core::content_policy::ContentPolicyDocument;
use fauna_core::obligation::ContentPolicy;
use fauna_protocol::RpcRequester;
use fauna_protocol::family::{FamilyWardInfo, ReachPolicy};

use crate::FamilyClient;

/// The `fauna.family.policy.update` document that stores `document` as the
/// ward's content rules and changes nothing else. The floors go out at
/// `inherit`; the nest clears them anyway when it stores a document, because
/// the editor showed every floor still set as a rule.
pub fn ward_policy_with_content_document(
    ward: &FamilyWardInfo,
    document: ContentPolicyDocument,
) -> ReachPolicy {
    ReachPolicy {
        contact_approval: ward.policy.contact_approval,
        unknown_sender_mail: ward.policy.unknown_sender_mail.clone(),
        federation_contact: ward.policy.federation_contact,
        feed_sources: ward.policy.feed_sources.clone(),
        content_policy: Some(ContentPolicy {
            document: Some(document),
            ..Default::default()
        }),
        ..Default::default()
    }
}

impl<R: RpcRequester> FamilyClient<R> {
    /// Save a ward's content rules: re-read the ward's policy, write the
    /// document ([`ward_policy_with_content_document`]), re-read. The ward's
    /// fresh entry, or `None` when the caller no longer guards `ward` — then
    /// nothing is written, since there is no policy to carry the knobs from.
    pub async fn save_content_rules(
        &self,
        ward: &[u8],
        document: ContentPolicyDocument,
    ) -> Result<Option<FamilyWardInfo>, R::Error> {
        let Some(entry) = self.ward_entry(ward).await? else {
            return Ok(None);
        };
        self.policy_update(
            ward.to_vec(),
            ward_policy_with_content_document(&entry, document),
        )
        .await?;
        self.ward_entry(ward).await
    }

    async fn ward_entry(&self, ward: &[u8]) -> Result<Option<FamilyWardInfo>, R::Error> {
        Ok(self
            .status()
            .await?
            .wards
            .into_iter()
            .find(|entry| entry.actor_id.as_slice() == ward))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_core::obligation::ContentFloor;
    use fauna_core::screen_time::ScreenTimePolicy;

    #[test]
    fn the_write_carries_the_reach_knobs_and_the_document_and_leaves_every_other_pillar_absent() {
        let ward = FamilyWardInfo {
            policy: ReachPolicy {
                contact_approval: true,
                unknown_sender_mail: "hold".into(),
                federation_contact: false,
                feed_sources: "block".into(),
                content_policy: Some(ContentPolicy {
                    nsfw: ContentFloor::Block,
                    ..Default::default()
                }),
                content_notify: Some(true),
                screen_time: Some(ScreenTimePolicy {
                    daily_minutes: Some(60),
                    ..Default::default()
                }),
                unknown_peer_dm: Some("hold".into()),
                ..Default::default()
            },
            ..Default::default()
        };
        let document = ContentPolicyDocument::empty();
        let policy = ward_policy_with_content_document(&ward, document.clone());
        assert!(policy.contact_approval);
        assert_eq!(policy.unknown_sender_mail, "hold");
        assert!(!policy.federation_contact);
        assert_eq!(policy.feed_sources, "block");
        let content = policy
            .content_policy
            .expect("the content pillar is written");
        assert_eq!(content.document, Some(document));
        assert_eq!(
            content.nsfw,
            ContentFloor::Inherit,
            "the floors are the document's now"
        );
        assert_eq!(policy.content_notify, None);
        assert_eq!(policy.screen_time, None);
        assert_eq!(policy.unknown_peer_dm, None);
        assert_eq!(policy.features, None);
    }
}
