import Foundation

/// `ThreadFlavor` glue for the conversations TestAgent commands
/// (mac/iOS, priority #2 — was duplicated verbatim in both App shells before
/// this lift; mirrors linux/windows, which use the Rust `Debug`-format
/// spellings the cross-app action layer
/// (`tests/e2e-unified/actions/conversations.py`) expects — Swift has no
/// built-in `Debug`, so the variant names are mirrored explicitly). `Rail`'s
/// own string↔enum mapping now rides the shared `railAsStr`/`railParse` FFI
/// functions (`libs/fauna-conversations/src/address.rs`) directly at each
/// call site.
public extension ThreadFlavor {
    /// Rust `Debug`-format name (see `Rail.debugString`).
    var debugString: String {
        switch self {
        case .oneToOne: return "OneToOne"
        case .mlsGroup: return "MlsGroup"
        case .subjectKeyed: return "SubjectKeyed"
        // A flavor a newer device wrote, carried from its history slice.
        case .unknown: return "Unknown"
        }
    }
}
