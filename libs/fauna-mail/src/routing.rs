//! Recipient partitioning — split a submission's addresses into local and
//! remote. Pure std, no I/O: the nest `fauna.email.send` handler uses it to
//! decide which recipients are sealed in-domain vs. enqueued for outbound MX
//! delivery. Lifted from the retired `fauna-bridge-smtp` crate (its permanent
//! home is here, alongside the rest of the shared mail logic).

/// Partition a list of email addresses into local parts and remote addresses.
///
/// An address is "local" when its domain matches `local_domain` (case-insensitive).
/// Returns `(local_parts, remote_addrs)`. Local parts are lowercased.
///
/// Returns `Err(addr)` if any address lacks an `@` sign.
pub fn partition_recipients(
    recipients: &[String],
    local_domain: Option<&str>,
) -> Result<(Vec<String>, Vec<String>), String> {
    let mut local_parts = Vec::new();
    let mut remote_addrs = Vec::new();

    for addr in recipients {
        match addr.rsplit_once('@') {
            Some((local, domain)) => {
                if local_domain.is_some_and(|d| d.eq_ignore_ascii_case(domain)) {
                    local_parts.push(local.to_lowercase());
                } else {
                    remote_addrs.push(addr.clone());
                }
            }
            None => return Err(addr.clone()),
        }
    }

    Ok((local_parts, remote_addrs))
}

/// Merge `to` and `recipients` fields into a single deduplicated list.
pub fn merge_recipients(to: &str, recipients: &[String]) -> Vec<String> {
    let mut all = recipients.to_vec();
    if !to.is_empty() && !all.contains(&to.to_string()) {
        all.push(to.to_string());
    }
    all
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partition_all_local() {
        let addrs = vec!["alice@example.com".into(), "bob@example.com".into()];
        let (local, remote) = partition_recipients(&addrs, Some("example.com")).unwrap();
        assert_eq!(local, vec!["alice", "bob"]);
        assert!(remote.is_empty());
    }

    #[test]
    fn partition_all_remote() {
        let addrs = vec!["alice@example.net".into()];
        let (local, remote) = partition_recipients(&addrs, Some("example.com")).unwrap();
        assert!(local.is_empty());
        assert_eq!(remote, vec!["alice@example.net"]);
    }

    #[test]
    fn partition_mixed() {
        let addrs = vec!["alice@example.com".into(), "bob@example.net".into()];
        let (local, remote) = partition_recipients(&addrs, Some("example.com")).unwrap();
        assert_eq!(local, vec!["alice"]);
        assert_eq!(remote, vec!["bob@example.net"]);
    }

    #[test]
    fn partition_no_domain_all_remote() {
        let addrs = vec!["alice@example.com".into()];
        let (local, remote) = partition_recipients(&addrs, None).unwrap();
        assert!(local.is_empty());
        assert_eq!(remote, vec!["alice@example.com"]);
    }

    #[test]
    fn partition_case_insensitive_domain() {
        let addrs = vec!["Alice@EXAMPLE.COM".into()];
        let (local, remote) = partition_recipients(&addrs, Some("example.com")).unwrap();
        assert_eq!(local, vec!["alice"]);
        assert!(remote.is_empty());
    }

    #[test]
    fn partition_invalid_address() {
        let addrs = vec!["noatsign".into()];
        let err = partition_recipients(&addrs, Some("example.com")).unwrap_err();
        assert_eq!(err, "noatsign");
    }

    #[test]
    fn merge_deduplicates() {
        let recipients = vec!["bob@x.test".into()];
        let all = merge_recipients("bob@x.test", &recipients);
        assert_eq!(all, vec!["bob@x.test"]);
    }

    #[test]
    fn merge_adds_to_field() {
        let recipients = vec!["bob@x.test".into()];
        let all = merge_recipients("alice@x.test", &recipients);
        assert_eq!(all, vec!["bob@x.test", "alice@x.test"]);
    }

    #[test]
    fn merge_empty_to() {
        let recipients = vec!["bob@x.test".into()];
        let all = merge_recipients("", &recipients);
        assert_eq!(all, vec!["bob@x.test"]);
    }
}
