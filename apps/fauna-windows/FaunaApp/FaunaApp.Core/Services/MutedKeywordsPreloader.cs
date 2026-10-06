using System;
using System.Threading.Tasks;
using FaunaApp.Core.Logs;

namespace FaunaApp.Core.Services;

/// <summary>
/// Populate <see cref="MutedKeywordsCache"/> once at login (moderation.md §
/// Muted keywords) — the native twin of linux's
/// <c>FaunaClient::load_muted_keywords</c> auth-time cache seed, so the
/// conversation bubble collapse works on a fresh launch without the user
/// visiting the Settings "Muted words" page first. Every subsequent
/// load/save of that page also repopulates the cache (<c>MutedWordsViewModel</c>);
/// this is only the one-time seed. Fire-and-forget, best-effort — a fault must
/// never disrupt login (the HostAddressReporter / reconnect-pump stance); the
/// cache simply stays empty (no mutes) until the next successful read.
/// </summary>
internal sealed class MutedKeywordsPreloader
{
    private readonly INestRpcClient _rpc;

    public MutedKeywordsPreloader(INestRpcClient rpc) => _rpc = rpc;

    public async Task RunAsync()
    {
        try
        {
            MutedKeywordsCache.SetKeywords((await _rpc.MutedKeywordsListAsync().ConfigureAwait(false)).keywords);
        }
        catch (Exception ex)
        {
            ShellLog.Warn("MutedKeywordsPreloader", $"preload skipped: {ex.GetType().Name}: {ex.Message}");
        }
    }
}
