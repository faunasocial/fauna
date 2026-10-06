use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ThreadId(pub String); // opaque stable id; UUID-shaped in practice

// UniFFI 0.31 does not support derive(uniffi::Record) on tuple-struct newtypes;
// expose ThreadId as a String custom type instead.
#[cfg(feature = "uniffi")]
uniffi::custom_type!(ThreadId, String, {
    lower: |id| id.0.clone(),
    try_lift: |s| Ok(ThreadId(s)),
});

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum ThreadFlavor {
    OneToOne,
    MlsGroup,
    SubjectKeyed,
    /// A flavor a newer build writes and this one does not name, read out of
    /// a history slice another device wrote — carried whole as its canonical
    /// bytes so the slice this build merges and re-uploads keeps it
    /// (`transport.md` § Schema and forward-compat discipline → *Rule 3 in
    /// full*; the shape: [`fauna_core::carried::canonical_bytes`]). The thread
    /// is shown, and behaves as the most restrictive known flavor: every
    /// flavor-gated affordance (group roster edits, subject threading) is
    /// withheld. No build writes one except by passing a carried value
    /// through.
    #[serde(untagged, with = "fauna_core::carried::canonical_bytes")]
    Unknown {
        canonical: Vec<u8>,
    },
}
