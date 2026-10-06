using System.Threading.Tasks;

namespace FaunaApp.Views;

/// <summary>
/// A page whose <c>OnNavigatedTo</c> → <c>Loaded</c> does asynchronous work (nest
/// reads, machine builds) that must finish before the app is genuinely usable.
///
/// <para>The e2e state protocol reports navigation readiness (<c>set_state</c> waits
/// for <c>ready == true</c>, <c>drivers/http_bridge.py</c>) after the test agent runs
/// the nav post-action — which only KICKS OFF the frame navigation synchronously and
/// returns; a page's <c>async void Page_Loaded</c> completes later. Without this
/// barrier the agent signals <c>ready</c> while the target page is still loading, so an
/// immediately-following gesture (e.g. the shared <c>create_folder_via_wizard</c>
/// action clicking <c>folder-add-button</c> right after <c>navigate_folders</c>)
/// races the load and is silently dropped — the whole-file <c>test_folders.py</c>
/// wizard-render flake (the window widens with the session-accumulated folder count,
/// which is why it fails batched but passes in isolation).</para>
///
/// <para>A page that implements this exposes a <see cref="LoadComplete"/> task its shell
/// arms into <see cref="App.PendingNavLoad"/> right after the frame navigate; the test
/// agent awaits it (bounded) before signalling <c>ready</c>, so <c>ready == true</c>
/// means the page finished loading — not merely that the nav was kicked off. Pages that
/// do NOT implement it are unaffected (no barrier is armed, the agent waits on nothing).
/// Purely an e2e-readiness contract — no production behaviour depends on it.</para>
/// </summary>
internal interface IAsyncLoadedPage
{
    /// <summary>Completes when this page's initial async load has finished (rendered),
    /// success or handled error. Reset per navigation so a reused (cached) page instance
    /// re-arms; must ALWAYS complete (in a <c>finally</c>) so the agent's bounded await
    /// never hangs a navigation.</summary>
    Task LoadComplete { get; }
}
