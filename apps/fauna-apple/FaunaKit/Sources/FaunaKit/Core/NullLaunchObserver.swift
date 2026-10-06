import Foundation

/// No-op `LaunchObserver` for the shared `LaunchMachine`.
///
/// The machine requires an observer at construction, but Apple drives it in a
/// **pull** model, exactly as Windows does (`App.xaml.cs`'s `NullLaunchObserver`):
/// every entry point (`start()`, `trustNestIdentity()`, `retrySilentChallenge()`)
/// is `await`ed, and the caller then reads one fresh `snapshot()` and dispatches on
/// its `LaunchPhase`. There is no state to push between those awaits that the
/// snapshot read doesn't already carry, so a push observer would only duplicate the
/// dispatch.
///
/// (Android, by contrast, holds a live observer because its `AppLaunchVM` exposes the
/// phase as an observable flow the Compose tree recomposes off. Apple's launch is a
/// one-shot gate — `MacAppState.launchGate` / `AppState.identityChanged` — so it has
/// no such stream to feed.)
///
/// `@unchecked Sendable`: the protocol is `Sendable` and this type is stateless.
public final class NullLaunchObserver: LaunchObserver, @unchecked Sendable {
    public init() {}
    public func onChanged() {}
}
