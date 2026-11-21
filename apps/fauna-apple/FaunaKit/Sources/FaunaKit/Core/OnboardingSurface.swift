import Foundation

/// A wizard gesture's refusal is never silent (`docs/goal/ui/README.md`
/// § Offer only what is possible, rule 7): the apple twin of tui's
/// `surface` (`apps/fauna-tui/src/wizard/mod.rs`). The shared machine puts its
/// own refusals on `error_message` already; any other `Err` the gesture throws
/// lands there too — but never over the machine's own, more specific reason.
/// Every view renders `errorMessage()` as the page's `error-message`, so a
/// `try?` at a gesture site is the defect this replaces.
public extension OnboardingMachine {
    /// Run one throwing wizard gesture; `true` when it succeeded.
    @discardableResult
    func surface(_ gesture: (OnboardingMachine) throws -> Void) -> Bool {
        do {
            try gesture(self)
            return true
        } catch {
            if errorMessage() == nil {
                setErrorMessage(message: error.localizedDescription)
            }
            return false
        }
    }
}
