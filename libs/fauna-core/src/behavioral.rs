//! Behavioral anomaly detection for DM spam (Layer 2).
//!
//! Computes a behavioural anomaly score for a sender based on observed
//! communication patterns and social-graph proximity. Scores are in [0.0, 1.0]
//! where higher values indicate more anomalous (potentially spam) behaviour.

use serde::{Deserialize, Serialize};

/// Observed behavioural signals for a single sender–recipient pair.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BehavioralProfile {
    /// Number of unique DM recipients in the last 1 hour.
    pub unique_dm_recipients_1h: u32,
    /// Number of unique DM recipients in the last 24 hours.
    pub unique_dm_recipients_24h: u32,
    /// Number of unique DM recipients in the last 7 days.
    pub unique_dm_recipients_7d: u32,
    /// Social-graph hop distance to recipient (None = no path found).
    pub social_distance: Option<u8>,
    /// Number of mutual contacts between sender and recipient.
    pub mutual_contacts: u32,
    /// Whether the recipient already follows the sender.
    pub is_followed_by_recipient: bool,
    /// Whether the sender already follows the recipient.
    pub is_following_recipient: bool,
    /// Age of the sender's account in days.
    pub account_age_days: u64,
    /// Total number of public posts the sender has made.
    pub total_public_posts: u64,
    /// Total number of replies the sender has received on public posts.
    pub total_received_replies: u64,
    /// Ratio of DMs sent to public posts made (DMs / posts).
    pub dm_to_post_ratio: f64,
    /// Fraction of the sender's 7-day correspondents who replied (0.0–1.0):
    /// *distinct correspondents who replied ÷ distinct correspondents messaged*.
    ///
    /// **This field must have a live writer feeding it.** It appears only in a
    /// conjunction (`unique_dm_recipients_7d > 10 && dm_response_rate < 0.05`),
    /// so a value that is structurally 0.0 does not disable the rule — it makes
    /// the second conjunct permanently true and silently degrades the rule to
    /// fanout-alone, which labels honest users at 11 correspondents a week.
    /// That is exactly what finding caught: the `dm_replied` event this
    /// is derived from had no production writer for the layer's whole life.
    /// The regression guard is
    /// `bins/fauna-nest/tests/conformance_conversations_channel.rs::a_reply_feeds_the_original_senders_dm_response_rate`,
    /// which drives the real send path rather than the DB helper.
    pub dm_response_rate: f64,
}

/// Compute a behavioural anomaly score for a sender based on the given profile.
///
/// Returns a value clamped to [0.0, 1.0]. Higher scores indicate more
/// anomalous behaviour consistent with unsolicited bulk DM patterns.
///
/// Scoring rules applied:
/// - `unique_dm_recipients_1h > 20` → +0.3 (else `> 10` → +0.15)
/// - `social_distance: None` → +0.25, `> 3` → +0.15, `> 1` → +0.05
/// - `account_age_days < 7` and effective `social_distance > 1` → +0.2
/// - `total_public_posts < 3` and `unique_dm_recipients_24h > 5` → +0.2
/// - `unique_dm_recipients_7d > 10` and `dm_response_rate < 0.05` → +0.3
/// - `mutual_contacts == 0` and `!is_followed_by_recipient` → +0.1
pub fn compute_behavioral_anomaly(profile: &BehavioralProfile) -> f64 {
    let mut score = 0.0_f64;

    // High-fanout DM rate signal.
    if profile.unique_dm_recipients_1h > 20 {
        score += 0.3;
    } else if profile.unique_dm_recipients_1h > 10 {
        score += 0.15;
    }

    // Social-graph distance signal.
    // "distance > 1" for the account_age check means either None or > 1.
    let dist_gt_1 = match profile.social_distance {
        None => true,
        Some(d) => d > 1,
    };

    match profile.social_distance {
        None => score += 0.25,
        Some(d) if d > 3 => score += 0.15,
        Some(d) if d > 1 => score += 0.05,
        _ => {}
    }

    // New account with distant/no social path.
    if profile.account_age_days < 7 && dist_gt_1 {
        score += 0.2;
    }

    // Ghost account (no public presence) sending many DMs.
    if profile.total_public_posts < 3 && profile.unique_dm_recipients_24h > 5 {
        score += 0.2;
    }

    // High-7d fanout with very low response rate.
    if profile.unique_dm_recipients_7d > 10 && profile.dm_response_rate < 0.05 {
        score += 0.3;
    }

    // No mutual contacts and recipient does not follow sender.
    if profile.mutual_contacts == 0 && !profile.is_followed_by_recipient {
        score += 0.1;
    }

    score.clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A baseline "normal user" profile: well-established, mutually connected,
    /// low DM volume, good response rate.
    fn normal_profile() -> BehavioralProfile {
        BehavioralProfile {
            unique_dm_recipients_1h: 1,
            unique_dm_recipients_24h: 3,
            unique_dm_recipients_7d: 5,
            social_distance: Some(1),
            mutual_contacts: 10,
            is_followed_by_recipient: true,
            is_following_recipient: true,
            account_age_days: 365,
            total_public_posts: 200,
            total_received_replies: 50,
            dm_to_post_ratio: 0.05,
            dm_response_rate: 0.6,
        }
    }

    #[test]
    fn normal_user_scores_zero() {
        let profile = normal_profile();
        let score = compute_behavioral_anomaly(&profile);
        assert!(
            score < 0.01,
            "normal user should score near 0.0, got {score}"
        );
    }

    #[test]
    fn high_fanout_scores_high() {
        let profile = BehavioralProfile {
            unique_dm_recipients_1h: 25,
            ..normal_profile()
        };
        let score = compute_behavioral_anomaly(&profile);
        assert!(
            score >= 0.3,
            "25 DM recipients/hour should score >= 0.3, got {score}"
        );
    }

    #[test]
    fn no_social_path_increases_score() {
        let profile = BehavioralProfile {
            social_distance: None,
            mutual_contacts: 0,
            is_followed_by_recipient: false,
            ..normal_profile()
        };
        let score = compute_behavioral_anomaly(&profile);
        assert!(
            score >= 0.3,
            "no social path + no mutuals + not followed should score >= 0.3, got {score}"
        );
    }

    #[test]
    fn new_account_spammer_pattern() {
        // New account, high fanout, no public presence, no social proximity,
        // no mutual contacts, not followed, very low response rate.
        let profile = BehavioralProfile {
            unique_dm_recipients_1h: 25, // +0.3
            unique_dm_recipients_24h: 30,
            unique_dm_recipients_7d: 50, // with dm_response_rate < 0.05 → +0.3
            social_distance: None,       // +0.25
            mutual_contacts: 0,          // with !is_followed → +0.1
            is_followed_by_recipient: false,
            is_following_recipient: false,
            account_age_days: 2,   // < 7, dist > 1 → +0.2
            total_public_posts: 0, // < 3, recipients_24h > 5 → +0.2
            total_received_replies: 0,
            dm_to_post_ratio: 999.0,
            dm_response_rate: 0.01, // < 0.05
        };
        let score = compute_behavioral_anomaly(&profile);
        assert!(
            score >= 0.9,
            "extreme spammer pattern should score >= 0.9, got {score}"
        );
    }

    /// Asserts `score` is within `1e-9` of `expected` (the rule bonuses are
    /// fixed decimal literals summed in a known order, but exact `f64`
    /// equality is still fragile across unrelated rule additions).
    fn assert_score_near(score: f64, expected: f64) {
        assert!(
            (score - expected).abs() < 1e-9,
            "expected score ~{expected}, got {score}"
        );
    }

    #[test]
    fn fanout_1h_boundary_at_10_no_bonus() {
        let profile = BehavioralProfile {
            unique_dm_recipients_1h: 10,
            ..normal_profile()
        };
        assert_score_near(compute_behavioral_anomaly(&profile), 0.0);
    }

    #[test]
    fn fanout_1h_boundary_at_11_low_bonus() {
        let profile = BehavioralProfile {
            unique_dm_recipients_1h: 11,
            ..normal_profile()
        };
        assert_score_near(compute_behavioral_anomaly(&profile), 0.15);
    }

    #[test]
    fn fanout_1h_boundary_at_20_still_low_bonus() {
        let profile = BehavioralProfile {
            unique_dm_recipients_1h: 20,
            ..normal_profile()
        };
        assert_score_near(compute_behavioral_anomaly(&profile), 0.15);
    }

    #[test]
    fn fanout_1h_boundary_at_21_high_bonus() {
        let profile = BehavioralProfile {
            unique_dm_recipients_1h: 21,
            ..normal_profile()
        };
        assert_score_near(compute_behavioral_anomaly(&profile), 0.3);
    }

    #[test]
    fn social_distance_boundary_at_2_low_bonus() {
        let profile = BehavioralProfile {
            social_distance: Some(2),
            ..normal_profile()
        };
        assert_score_near(compute_behavioral_anomaly(&profile), 0.05);
    }

    #[test]
    fn social_distance_boundary_at_3_still_low_bonus() {
        let profile = BehavioralProfile {
            social_distance: Some(3),
            ..normal_profile()
        };
        assert_score_near(compute_behavioral_anomaly(&profile), 0.05);
    }

    #[test]
    fn social_distance_boundary_at_4_mid_bonus() {
        let profile = BehavioralProfile {
            social_distance: Some(4),
            ..normal_profile()
        };
        assert_score_near(compute_behavioral_anomaly(&profile), 0.15);
    }

    #[test]
    fn new_account_boundary_at_7_days_no_extra_bonus() {
        // social_distance: None alone contributes +0.25; account_age_days==7
        // is NOT "< 7" so the new-account rule must not add its own +0.2.
        let profile = BehavioralProfile {
            social_distance: None,
            account_age_days: 7,
            ..normal_profile()
        };
        assert_score_near(compute_behavioral_anomaly(&profile), 0.25);
    }

    #[test]
    fn new_account_below_7_days_adds_bonus() {
        let profile = BehavioralProfile {
            social_distance: None,
            account_age_days: 6,
            ..normal_profile()
        };
        assert_score_near(compute_behavioral_anomaly(&profile), 0.45);
    }

    #[test]
    fn ghost_account_boundary_posts_at_3_no_bonus() {
        let profile = BehavioralProfile {
            total_public_posts: 3,
            unique_dm_recipients_24h: 6,
            ..normal_profile()
        };
        assert_score_near(compute_behavioral_anomaly(&profile), 0.0);
    }

    #[test]
    fn ghost_account_boundary_recipients_24h_at_5_no_bonus() {
        let profile = BehavioralProfile {
            total_public_posts: 2,
            unique_dm_recipients_24h: 5,
            ..normal_profile()
        };
        assert_score_near(compute_behavioral_anomaly(&profile), 0.0);
    }

    #[test]
    fn ghost_account_over_both_thresholds_adds_bonus() {
        let profile = BehavioralProfile {
            total_public_posts: 2,
            unique_dm_recipients_24h: 6,
            ..normal_profile()
        };
        assert_score_near(compute_behavioral_anomaly(&profile), 0.2);
    }

    #[test]
    fn low_response_boundary_recipients_7d_at_10_no_bonus() {
        let profile = BehavioralProfile {
            unique_dm_recipients_7d: 10,
            dm_response_rate: 0.01,
            ..normal_profile()
        };
        assert_score_near(compute_behavioral_anomaly(&profile), 0.0);
    }

    #[test]
    fn low_response_boundary_rate_at_point_zero_five_no_bonus() {
        let profile = BehavioralProfile {
            unique_dm_recipients_7d: 20,
            dm_response_rate: 0.05,
            ..normal_profile()
        };
        assert_score_near(compute_behavioral_anomaly(&profile), 0.0);
    }

    /// The arithmetic, pinned so it stops being a surprise: an
    /// otherwise-model citizen — 365-day account, 200 public posts, mutually
    /// connected, one hop away — lands on *exactly* the 0.3 label threshold the
    /// moment `dm_response_rate` reads 0.0 with 11 correspondents in 7 days.
    ///
    /// 0.0 is what a missing writer produces, so this is the cost of an unfed
    /// conjunct: not a disabled rule, but a fanout-alone rule that fires on
    /// honest users. The writer itself is guarded by a flow test in
    /// `fauna-nest`; this pins the consequence.
    #[test]
    fn an_unfed_response_rate_puts_an_honest_high_fanout_user_on_the_threshold() {
        let profile = BehavioralProfile {
            unique_dm_recipients_7d: 11,
            dm_response_rate: 0.0,
            ..normal_profile()
        };
        assert_score_near(compute_behavioral_anomaly(&profile), 0.3);
    }

    /// The same citizen, with the feed alive: their correspondents replied, so
    /// the rule correctly stays quiet.
    #[test]
    fn a_fed_response_rate_clears_the_same_honest_user() {
        let profile = BehavioralProfile {
            unique_dm_recipients_7d: 11,
            dm_response_rate: 0.6,
            ..normal_profile()
        };
        assert_score_near(compute_behavioral_anomaly(&profile), 0.0);
    }

    #[test]
    fn low_response_over_both_thresholds_adds_bonus() {
        let profile = BehavioralProfile {
            unique_dm_recipients_7d: 11,
            dm_response_rate: 0.049,
            ..normal_profile()
        };
        assert_score_near(compute_behavioral_anomaly(&profile), 0.3);
    }

    #[test]
    fn zero_mutuals_but_followed_no_bonus() {
        let profile = BehavioralProfile {
            mutual_contacts: 0,
            is_followed_by_recipient: true,
            ..normal_profile()
        };
        assert_score_near(compute_behavioral_anomaly(&profile), 0.0);
    }

    #[test]
    fn one_mutual_not_followed_no_bonus() {
        let profile = BehavioralProfile {
            mutual_contacts: 1,
            is_followed_by_recipient: false,
            ..normal_profile()
        };
        assert_score_near(compute_behavioral_anomaly(&profile), 0.0);
    }

    #[test]
    fn zero_mutuals_and_not_followed_adds_bonus() {
        let profile = BehavioralProfile {
            mutual_contacts: 0,
            is_followed_by_recipient: false,
            ..normal_profile()
        };
        assert_score_near(compute_behavioral_anomaly(&profile), 0.1);
    }

    #[test]
    fn score_clamps_to_one() {
        // Trigger every penalty rule simultaneously to exceed 1.0 before clamping.
        let profile = BehavioralProfile {
            unique_dm_recipients_1h: 100, // +0.3
            unique_dm_recipients_24h: 100,
            unique_dm_recipients_7d: 100, // +0.3
            social_distance: None,        // +0.25
            mutual_contacts: 0,           // +0.1
            is_followed_by_recipient: false,
            is_following_recipient: false,
            account_age_days: 1,   // +0.2
            total_public_posts: 0, // +0.2
            total_received_replies: 0,
            dm_to_post_ratio: 9999.0,
            dm_response_rate: 0.0, // +0.3
        };
        let score = compute_behavioral_anomaly(&profile);
        assert_eq!(
            score, 1.0,
            "extreme values should clamp to exactly 1.0, got {score}"
        );
    }
}
