using uniffi.fauna_launch_machine;

namespace FaunaApp.Core.Services;

/// <summary>
/// A <see cref="LaunchObserver"/> that does nothing. The Windows launch flow
/// reads <c>LaunchMachine.Snapshot()</c> imperatively after <c>Start()</c> /
/// <c>RetrySilentChallenge()</c>; it does not drive UI off observer ticks.
/// Mirrors Linux's <c>NullObserver</c>.
///
/// Marked <c>internal</c> because <c>fauna-launch-machine</c>'s UniFFI-generated
/// <see cref="LaunchObserver"/> interface is emitted as <c>internal</c>. FaunaApp.Core
/// exposes its internals to the FaunaApp WinUI shell and the FaunaApp.Tests project
/// via <c>InternalsVisibleTo</c>.
/// </summary>
internal sealed class NullLaunchObserver : LaunchObserver
{
    public void OnChanged() { }
}
