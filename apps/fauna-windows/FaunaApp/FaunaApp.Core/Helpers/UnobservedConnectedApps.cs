namespace FaunaApp.Core.Helpers;

/// <summary>
/// The observer of a connected-apps machine built for one verb outside the Connected apps
/// page — the bridges page's plugin cards (<c>bind_plugin</c> / <c>revoke_principal</c>)
/// and the Media page's version author line (<c>version_author</c>, whose roster read the
/// page awaits and then repaints). Each paints from its own reads, so the machine's change
/// notifications have no listener.
/// </summary>
internal sealed class UnobservedConnectedApps : uniffi.fauna_client_connected_apps.ConnectedAppsObserver
{
    public void OnChanged()
    {
    }
}
