import Foundation

/// Hop to `@MainActor`, weak-read `target`, and forward if it's still alive —
/// the body every UniFFI observer trampoline's callback runs (`BackupsObserverBox
/// .onChanged`, `DevicesObserverBox.onChanged`, …). Each box conforms to its
/// own distinct UniFFI-generated protocol, so the classes themselves can't share
/// a common superclass — but the boxes hand-rolling this identical
/// hop-and-forward shape cross-reference each other in a doc-comment "mirrors"
/// chain.
func notifyOnMainActor<Target: AnyObject>(_ target: Target?, _ action: @escaping @MainActor (Target) -> Void) {
    Task { @MainActor [weak target] in
        if let target { action(target) }
    }
}
