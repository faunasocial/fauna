import Foundation

/// Resolves a `LocalizedText`'s template, before argument substitution.
///
/// Rust emits fully qualified catalog keys (`onboarding.handle_check.phase.parsing`),
/// so the key is looked up as-is — no per-app namespace guessing (pinned on the
/// Rust side by `fauna-onboarding-machine`'s `i18n_keys_resolve` test).
/// `nil` for an empty key (machine's default) — shared by
/// `renderLocalizedText`/`renderLocalizedTextNested`, which differ only in
/// how they substitute the resolved template's arguments.
private func resolveLocalizedTemplate(_ lt: LocalizedText) -> String? {
    if lt.key.isEmpty { return nil }
    return L.lookup(lt.key)
}

/// Renders a `LocalizedText` (key + args dictionary) emitted by the
/// `OnboardingMachine` snapshots. Substitutes `{name}` placeholders with the
/// args verbatim, in the one shared single pass (`fillPlaceholders`): an arg
/// holding `{other}` is data and is never expanded.
public func renderLocalizedText(_ lt: LocalizedText) -> String {
    guard let template = resolveLocalizedTemplate(lt) else { return "" }
    return fillPlaceholders(template: template, args: lt.args)
}

/// `renderLocalizedText`, but each **argument** is first put through `L.lookup`
/// as well — for templates whose substitution is itself a translatable term
/// rather than data. Some machines deliberately pass a *key* as an argument
/// value, because the substituted word has to be translated too: the feature
/// plane's exhaustion/restriction sentence carries `window = "features.window_day"`
/// so *"you've used up the limit for this {window}"* reads "…for this day"
/// rather than the raw key. Mirrors
/// `fauna_core::localized::LocalizedText::resolve_nested`. An argument that is
/// not a key simply misses the lookup (falls back to the raw value via
/// `L.lookup`'s own miss behavior) and substitutes verbatim.
public func renderLocalizedTextNested(_ lt: LocalizedText) -> String {
    guard let template = resolveLocalizedTemplate(lt) else { return "" }
    return fillPlaceholders(template: template, args: lt.args.mapValues { L.lookup($0) })
}

/// The first non-nil source, in priority order — the shared `errorMessage`
/// composition rule across every machine-backed VM whose page has its own
/// client-side error(s) that must outrank the machine snapshot's own (e.g.
/// `connectError`, then a VM-local glue error, then the machine snapshot's
/// `renderLocalizedText`-rendered error). A VM whose precedence genuinely
/// inverts this (`FeedVM.errorMessage`: "the snapshot's own error always
/// wins") is not this shape and stays hand-rolled.
func firstNonNil(_ sources: String?...) -> String? {
    for s in sources {
        if let s { return s }
    }
    return nil
}

/// A compose form's own error banner text: the compose's own send failure
/// outranks the page-level client error, mirroring `ConversationsVM.pageError`'s
/// same precedence but scoped to one compose (not the whole page's `snap.error`
/// fallback, which `pageError` alone owns — see its own doc comment for why that
/// precedence must not be reversed).
func composeSendOrClientError(_ sendState: SendState, clientError: String?) -> String {
    if case let .failed(reason) = sendState { return renderLocalizedText(reason) }
    return clientError ?? ""
}
