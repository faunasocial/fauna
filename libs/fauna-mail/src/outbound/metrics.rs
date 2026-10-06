//! Outbound metrics label constants.
//!
//! Implements the metric-name/label registry from
//! `docs/goal/behavior/smtp-server.md` § Outbound metrics. The counter
//! values themselves live in the bridge's Prometheus registry; this
//! module is the contract that every emitter and every dashboard
//! consumer agrees on, so a label string typo can't drift the metric
//! out of band.
//!
//! Wiring: the bridge's `queue.rs` drain loop calls
//! `<registry>.smtp_outbound_attempts_total([(VERDICT, VERDICT_DELIVERED)])`
//! at each attempt point. The increment sites are marked with
//! `// TODO item 9` comments in this commit's earlier sibling
//! commits; lining them up alongside the registry registration is the
//! Task 9b sub-task (the bridge's metric registry now lives in the Go
//! MTA bridge, `bins/fauna-bridges`; the legacy Rust daemon that
//! once held it was deleted at the I6 cutover).

pub const ATTEMPTS_TOTAL: &str = "smtp_outbound_attempts_total";
pub const BOUNCES_TOTAL: &str = "smtp_outbound_bounces_total";
pub const QUEUE_DEPTH: &str = "smtp_outbound_queue_depth";
pub const ATTEMPT_SECONDS: &str = "smtp_outbound_attempt_seconds";
pub const TLSRPT_REPORTS_SENT_TOTAL: &str = "tlsrpt_reports_sent_total";

pub const VERDICT: &str = "verdict";
pub const STATE: &str = "state";
pub const TRANSPORT: &str = "transport";

// smtp_outbound_attempts_total{verdict=…}
pub const VERDICT_DELIVERED: &str = "delivered";
pub const VERDICT_TEMPFAIL_4XX: &str = "tempfail_4xx";
pub const VERDICT_TEMPFAIL_CONNECT: &str = "tempfail_connect";
pub const VERDICT_PERMFAIL_5XX: &str = "permfail_5xx";
pub const VERDICT_PERMFAIL_POLICY: &str = "permfail_policy";

// smtp_outbound_bounces_total{verdict=…}
pub const VERDICT_BOUNCE_SENT: &str = "sent";
pub const VERDICT_BOUNCE_SUPPRESSED_RATE: &str = "suppressed_rate";
pub const VERDICT_BOUNCE_SUPPRESSED_BACKSCATTER: &str = "suppressed_backscatter";

// smtp_outbound_queue_depth{state=…}
pub const STATE_PENDING: &str = "pending";
pub const STATE_RETRYING: &str = "retrying";
pub const STATE_FAILED: &str = "failed";

// tlsrpt_reports_sent_total{transport=…}
pub const TRANSPORT_MAILTO: &str = "mailto";
pub const TRANSPORT_HTTPS: &str = "https";
