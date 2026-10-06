//! The page-level "nest call failed" error enum every `fauna-*-machine`
//! crate's `nest_api` layer hand-copies: a fixed vocabulary of variants
//! (`Conflict` / `BadRequest` / `NotFound` / `Unavailable` / `Transient`),
//! each carrying one `detail: String`, plus a `detail()` accessor that
//! OR-matches every variant and a `Display` impl delegating to it. Six
//! crates had this same shape byte-for-byte, differing only in the enum
//! name and which subset of variants they declare —
//! [`declare_api_error`] is the one definition they now all expand.
//!
//! [`map_rpc_error`] is this enum's other half: the same six crates also
//! hand-copied the `map_err` classifier that turns a transport `R::Error`
//! into one of `declare_api_error`'s variants, keyed on the WS-RPC error's
//! code suffix.

/// Declare a page-level nest-call error enum: each named variant gets a
/// `{ detail: String }` field, plus the shared `detail()` accessor and
/// `Display` impl every `fauna-*-machine` crate's `nest_api` layer wants.
///
/// Per-variant (and per-enum) doc comments are threaded through verbatim, so
/// each call site keeps its own domain-specific explanation of what each
/// variant means.
///
/// The `@uniffi_flat_error` form additionally derives `uniffi::Error` with
/// `flat_error` (gated on the `uniffi` feature, as every other FFI-exported
/// type in this tree is) and provides the empty `std::error::Error` impl
/// `flat_error` requires — for a crate that hands this error across the FFI
/// boundary represented by its `Display` string.
///
/// ```ignore
/// fauna_core::declare_api_error!(
///     /// Failure of a page-level nest call.
///     DevicesApiError {
///         /// Name taken / concurrent destructive op / sole-source device.
///         Conflict,
///         /// Invalid request (bad mode / role / hex / candidate).
///         BadRequest,
///         /// Folder, device, or conflict not found / not owned.
///         NotFound,
///         /// Transport fault / 5xx — retryable.
///         Transient,
///     }
/// );
///
/// fauna_core::declare_api_error!(
///     @uniffi_flat_error
///     /// Failure of a Media page nest interaction.
///     MediaApiError { BadRequest, NotFound, Transient }
/// );
/// ```
#[macro_export]
macro_rules! declare_api_error {
    (
        $(#[$enum_meta:meta])*
        $name:ident {
            $( $(#[$variant_meta:meta])* $variant:ident ),+ $(,)?
        }
    ) => {
        $(#[$enum_meta])*
        #[derive(Debug, Clone, PartialEq, Eq)]
        pub enum $name {
            $(
                $(#[$variant_meta])*
                $variant { detail: String }
            ),+
        }

        impl $name {
            /// The human-readable detail every variant carries.
            pub fn detail(&self) -> &str {
                match self {
                    $( $name::$variant { detail } => detail ),+
                }
            }
        }

        impl ::std::fmt::Display for $name {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                f.write_str(self.detail())
            }
        }
    };
    (
        @uniffi_flat_error
        $(#[$enum_meta:meta])*
        $name:ident {
            $( $(#[$variant_meta:meta])* $variant:ident ),+ $(,)?
        }
    ) => {
        $(#[$enum_meta])*
        #[derive(Debug, Clone, PartialEq, Eq)]
        #[cfg_attr(feature = "uniffi", derive(uniffi::Error))]
        #[cfg_attr(feature = "uniffi", uniffi(flat_error))]
        pub enum $name {
            $(
                $(#[$variant_meta])*
                $variant { detail: String }
            ),+
        }

        impl $name {
            /// The human-readable detail every variant carries.
            pub fn detail(&self) -> &str {
                match self {
                    $( $name::$variant { detail } => detail ),+
                }
            }
        }

        impl ::std::fmt::Display for $name {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                f.write_str(self.detail())
            }
        }

        // `flat_error` requires `std::error::Error`; `Debug` + `Display` are
        // enough for the empty impl.
        impl ::std::error::Error for $name {}
    };
}

/// Declare the `map_err<E: RpcErrorClass + Display>(e: E) -> $out` classifier
/// every `fauna-*-machine` crate's `nest_api::ws_rpc` module hand-writes
/// alongside its [`declare_api_error`] enum: a server rejection dispatches on
/// the WS-RPC `RpcError`'s code suffix into a caller-named subset of `$out`'s
/// own variants; a transport fault, or any unrecognized suffix (forward
/// compat with a newer nest), falls through to `$out::Transient`. Six crates
/// hand-copied this scaffolding identically, keying only on their own error
/// type and arm list — each one's own doc comment already called it a
/// "mirror" of a sibling's.
///
/// ```ignore
/// fauna_core::map_rpc_error!(
///     /// Map a transport `R::Error` onto [`DevicesApiError`], keyed on the
///     /// WS-RPC `RpcError.code` suffix.
///     fn map_err(e) -> DevicesApiError {
///         "conflict" => Conflict,
///         "not_found" => NotFound,
///         "invalid_request" | "malformed" | "bad_candidate" => BadRequest,
///     }
/// );
/// ```
#[macro_export]
macro_rules! map_rpc_error {
    (
        $(#[$meta:meta])*
        fn $name:ident($e:ident) -> $out:ident {
            $($suffix:pat => $variant:ident),+ $(,)?
        }
    ) => {
        $(#[$meta])*
        fn $name<E: ::fauna_protocol::RpcErrorClass + ::core::fmt::Display>($e: E) -> $out {
            match $e.as_rpc_error() {
                Some(rpc) => {
                    let detail = rpc.detail_or_code();
                    match rpc.code_suffix() {
                        $($suffix => $out::$variant { detail },)+
                        _ => $out::Transient { detail },
                    }
                }
                None => $out::Transient { detail: $e.to_string() },
            }
        }
    };
}
