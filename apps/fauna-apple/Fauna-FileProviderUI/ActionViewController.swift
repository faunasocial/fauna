#if canImport(FileProviderUI)

import FaunaDeepLink
import FileProvider
import FileProviderUI

/// The File Provider **UI action extension** (`com.apple.fileprovider-actionsui`):
/// Finder / Files invoke it for the custom actions the FP appex declares
/// (`Fauna-FileProviderUI/Info.plist` `NSExtensionFileProviderActions` — the
/// windows shell-submenu leaf set, `FileProviderAction`), and each action is a
/// pure trampoline: build the `fauna://` deep link and open the app — no UI of
/// its own beyond the system sheet the OS shows while `prepare` runs
/// (`file-sync.md` § On-Demand Files → Apple File Provider binding, *context
/// actions*). Links only the FFI-free `FaunaDeepLink` module by design: a
/// FaunaKit dependency would ship the Rust xcframework inside a menu action.
final class ActionViewController: FPUIActionExtensionViewController {
    override func prepare(
        forAction actionIdentifier: String, itemIdentifiers: [NSFileProviderItemIdentifier]
    ) {
        guard
            let action = FileProviderAction(rawValue: actionIdentifier),
            let set = extensionContext.domainIdentifier?.rawValue
        else {
            cancel()
            return
        }
        // Item identifier = the folder-relative rel path; the root container
        // maps to "" (set-level), which Share treats as the whole set.
        let rels = itemIdentifiers.map {
            $0 == .rootContainer ? "" : $0.rawValue
        }
        guard let url = action.deepLink(set: set, rels: rels)?.url else {
            cancel()
            return
        }
        extensionContext.open(url) { [weak self] _ in
            self?.extensionContext.completeRequest()
        }
    }

    override func prepare(forError error: Error) {
        // No auth UI of our own — the app owns sign-in; just report back.
        extensionContext.cancelRequest(withError: error)
    }

    private func cancel() {
        extensionContext.cancelRequest(
            withError: NSError(
                domain: FPUIErrorDomain,
                code: Int(FPUIExtensionErrorCode.failed.rawValue)
            ))
    }
}

#endif
