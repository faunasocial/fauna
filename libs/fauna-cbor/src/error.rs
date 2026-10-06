use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum EncodeError {
    #[error("schema invalid for canonical dag-cbor: {0}")]
    SchemaInvalid(String),
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum DecodeError {
    #[error("input is not valid CBOR")]
    NotValidCbor,
    #[error("input is not canonical dag-cbor: {reason}")]
    NotCanonical { reason: String },
    #[error("schema mismatch: {0}")]
    SchemaMismatch(String),
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum VerifyError {
    #[error("CID multihash does not match bytes")]
    CidMismatch,
    #[error("Ed25519 signature invalid")]
    SignatureInvalid,
    #[error("public key has wrong shape")]
    KeyShape,
    #[error("bytes too short to verify")]
    BytesTooShort,
}
