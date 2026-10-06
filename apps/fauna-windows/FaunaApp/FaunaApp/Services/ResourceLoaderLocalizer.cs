using Microsoft.Windows.ApplicationModel.Resources;
using FaunaApp.Core.Services;

namespace FaunaApp.Services;

/// <summary>
/// IStringLocalizer implementation backed by WinUI 3 ResourceLoader.
/// Loads strings from Strings/en-US/Resources.resw using slash-notation keys.
/// </summary>
public sealed class ResourceLoaderLocalizer : IStringLocalizer
{
    private readonly ResourceLoader _loader;

    public ResourceLoaderLocalizer()
    {
        _loader = new ResourceLoader();
    }

    public string Get(string key)
    {
        try
        {
            var value = _loader.GetString(key);
            return string.IsNullOrEmpty(value) ? key : value;
        }
        catch
        {
            return key;
        }
    }
}
