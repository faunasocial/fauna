/// Extract actor_id from a Bearer token in the Authorization header.
///
/// Expected format: `Bearer {actor_id_hex}.{random}`
/// where actor_id_hex is a 64-character lowercase hex string.
pub fn actor_id_from_token(auth_header: &str) -> Option<[u8; 32]> {
    let token = auth_header.strip_prefix("Bearer ")?;
    let dot_pos = token.find('.')?;
    let hex_part = &token[..dot_pos];
    actor_id_from_hex(hex_part)
}

/// Decode a 64-character hex string into 32 bytes.
pub fn actor_id_from_hex(hex_str: &str) -> Option<[u8; 32]> {
    fauna_core::hex32::decode(hex_str).ok()
}

/// Extract actor_id from a JSON body.
///
/// Looks for a top-level `"actor_id"` field containing a 64-char hex string.
pub fn actor_id_from_json_body(body: &[u8]) -> Option<[u8; 32]> {
    let value: serde_json::Value = serde_json::from_slice(body).ok()?;
    let hex_str = value.get("actor_id")?.as_str()?;
    actor_id_from_hex(hex_str)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_from_bearer_token() {
        let actor_id_hex = "a".repeat(64);
        let token = format!("Bearer {}.randomsuffix123", actor_id_hex);
        let result = actor_id_from_token(&token);
        assert!(result.is_some());
        let expected = hex::decode(&actor_id_hex).unwrap();
        assert_eq!(result.unwrap(), expected.as_slice());
    }

    #[test]
    fn extract_from_bearer_rejects_old_format() {
        // No dot separator — should return None
        let actor_id_hex = "b".repeat(64);
        let token = format!("Bearer {}", actor_id_hex);
        assert!(actor_id_from_token(&token).is_none());
    }

    #[test]
    fn extract_from_hex_string() {
        // Valid 64-char hex
        let valid = "c".repeat(64);
        assert!(actor_id_from_hex(&valid).is_some());

        // Too short
        assert!(actor_id_from_hex("abcd").is_none());

        // Too long
        let too_long = "d".repeat(65);
        assert!(actor_id_from_hex(&too_long).is_none());

        // Invalid characters
        let invalid = "g".repeat(64);
        assert!(actor_id_from_hex(&invalid).is_none());
    }

    #[test]
    fn extract_from_json() {
        let actor_id_hex = "e".repeat(64);
        let body = format!(r#"{{"actor_id":"{}","other":"data"}}"#, actor_id_hex);
        let result = actor_id_from_json_body(body.as_bytes());
        assert!(result.is_some());
        let expected = hex::decode(&actor_id_hex).unwrap();
        assert_eq!(result.unwrap(), expected.as_slice());
    }

    #[test]
    fn extract_from_json_missing_field() {
        let body = br#"{"username":"alice","password":"secret"}"#;
        assert!(actor_id_from_json_body(body).is_none());
    }
}
