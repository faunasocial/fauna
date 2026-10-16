using System.Collections.Generic;
using System.Threading.Tasks;
using FaunaApp.Core.Services;

namespace FaunaApp.Tests;

/// <summary>A minimal <see cref="ISpamModelClientWrite"/> that only records the shared
/// server-row correction flow's calls (<c>MailSettingsMachine::train_moderation_correction</c>
/// is an unfakeable sealed UniFFI object); every other member is out of scope for the
/// tests that use it.</summary>
internal sealed class RecordingCorrectionSeam : ISpamModelClientWrite
{
    private readonly List<(string ContentId, bool IsSpam)> _calls;
    public RecordingCorrectionSeam(List<(string ContentId, bool IsSpam)> calls) => _calls = calls;

    public Task TrainModerationCorrectionAsync(string contentId, bool isSpam)
    {
        _calls.Add((contentId, isSpam));
        return Task.CompletedTask;
    }

    public Task<bool> SealedSpamWriteAvailableAsync() => throw new System.NotImplementedException();
    public Task<SpamWriteResult> TrainSpamModelClientAsync(string text, bool isSpam) =>
        throw new System.NotImplementedException();
    public Task<SpamWriteResult> TrainSpamModelClientMailAsync(
        string text, bool isSpam, byte[] messageId, string mailbox, string subject) =>
        throw new System.NotImplementedException();
}
