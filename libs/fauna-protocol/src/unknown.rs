//! Unknown-variant catch-all for forward compatibility. Per spec § 2.3 rule 3.
//!
//! Every CDDL-defined sum type has an Unknown variant that captures
//! `(kind, payload)` opaquely. Decoders try known variants in order
//! and fall through to Unknown on no match. The Rust shape is shared
//! across PushEvent, RpcKind, and any other tagged union in the
//! protocol crate or feature crates.

use serde::{Deserialize, Serialize};

use fauna_cbor::Value;

/// Opaque catch-all preserving an unknown kind + its payload.
///
/// Used everywhere a tagged union appears. Application code matches
/// against known variants first; Unknown is the residual.
///
/// **Exempt from transport.md rule 4's own `extra` catch-all**
/// (`tools/check-additive-evolution/catch_all_baseline.txt`): this struct
/// IS rule 3's tagged-union fallback mechanism, not a growable request/reply
/// payload — its two fields (`kind`, opaque `payload`) are the closed,
/// fixed shape a rule-4 catch-all exists to protect other structs' *own*
/// forward growth, not something itself needing that protection.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Unknown {
    pub kind: String,
    pub payload: Value,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::{decode_strict as decode, encode_canonical};
    use std::collections::BTreeMap;

    #[test]
    fn unknown_round_trip() {
        let u = Unknown {
            kind: "com.acme.experimental.foo".into(),
            payload: Value::Map(BTreeMap::from([(
                "custom_field".to_string(),
                Value::Integer(42),
            )])),
        };
        let bytes = encode_canonical(&u).unwrap();
        let decoded: Unknown = decode(&bytes).unwrap();
        assert_eq!(decoded, u);
    }
}
