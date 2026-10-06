// Re-export FaunaFFISwift so all FaunaKit files can use FFI functions
// without explicit imports.
@_exported import FaunaFFISwift
// Re-export the FFI-free extension slice (`AppleIdentifiers`, the generated
// `L` strings, the widget snapshot types) so FaunaKit and every app that
// imports it keep reading them unqualified, exactly as before they moved out
// for the widget appex's sake (Package.swift § FaunaExtensionKit).
@_exported import FaunaExtensionKit
