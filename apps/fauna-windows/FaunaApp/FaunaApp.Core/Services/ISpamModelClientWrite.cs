using System.Threading.Tasks;
using uniffi.fauna_client_mail_settings;

namespace FaunaApp.Core.Services;

/// <summary>
/// The outcome of a client-side spam-model write — the FFI
/// <see cref="SpamModelClientWrite"/> flattened to a plain result so the <c>sealed</c>
/// C# keyword stays confined to the one production mapping. <see cref="Sealed"/>
/// <c>true</c> ⇒ the model was mutated, re-sealed and written back via
/// <c>put_spam_model</c> (<c>SpamModelWriteOutcome::Sealed</c>); <c>false</c> ⇒ the
/// machine took the server path (the nest does not advertise
/// <c>spam-model-sealed-at-rest</c>), so the caller runs the server-side train it uses
/// today.
/// </summary>
internal readonly record struct SpamWriteResult(bool Sealed, uint SampleCount);

/// <summary>
/// The <b>sealed spam-model client-write</b> seam (<c>mail-spam.md</c> § Encrypted-mode
/// interaction; the co-design 1d surface switch, tracked internally): the
/// moderation-queue <c>train-correction-button</c> writes the caller's tier-1 Bayesian
/// model through the shared <c>MailSettingsMachine::apply_spam_model_write</c> (fetch →
/// unwrap → mutate → re-seal → <c>put_spam_model</c>) when the nest advertises the
/// <c>spam-model-sealed-at-rest</c> capability, else degrades to
/// <c>fauna.moderation.train</c>.
///
/// <para>A thin hand-written seam over the shared-Rust machine — the windows twin of
/// linux's <c>client.rs::train_moderation_flow</c> — so <c>ModerationViewModel</c>'s
/// train dispatch is unit-testable: the real
/// <see cref="MailSettingsMachine"/> is a sealed UniFFI object that cannot be faked. A
/// <c>null</c> seam on the VM means "no sealed-write path" → the correction always takes
/// the server-side train (a queue with no mail machine, e.g. mail not enabled).</para>
/// </summary>
internal interface ISpamModelClientWrite
{
    /// <summary>Cheap pre-check: would a write take the sealed client path? True iff the
    /// nest advertises <c>spam-model-sealed-at-rest</c> and the actor has an MSEK seal
    /// key (<c>MailSettingsMachine::sealed_spam_write_available</c>). A server row runs
    /// this <b>before</b> paying for the post-body fetch. Any failure ⇒ <c>false</c>
    /// (degrade — never a blind sealed write, which would double-seal a v-older nest's
    /// model → the user's model becomes unreadable, <c>version-compatibility.md</c>
    /// I1/I2).</summary>
    Task<bool> SealedSpamWriteAvailableAsync();

    /// <summary>Fetch → unwrap → apply the training delta on <paramref name="text"/>
    /// (<paramref name="isSpam"/> ⇒ spam, else ham) → re-seal → <c>put_spam_model</c>
    /// (<c>MailSettingsMachine::train_spam_model_client</c>). A
    /// <see cref="SpamWriteResult.Sealed"/> of <c>false</c> ⇒ the machine took the server
    /// path, so the caller degrades to the server-side train.</summary>
    Task<SpamWriteResult> TrainSpamModelClientAsync(string text, bool isSpam);

    /// <summary>The <b>live <c>Insert</c> consumer</b> (mail-spam.md § Wire shapes —
    /// the follow-on to the moderation-queue 1d switch,
    /// which left <c>history_op: None</c>). Fetch → unwrap → apply the training delta
    /// on <paramref name="text"/> → re-seal → <c>put_spam_model</c>, AND atomically seal
    /// a <c>spam_training_history</c> audit row to the actor's own key
    /// (<c>MailSettingsMachine::train_spam_model_client_mail</c>) — the row the
    /// <c>mail-spam</c> page renders (<c>{subject} · {mailbox}</c>) and undoes.
    /// <paramref name="messageId"/> is stored <b>opaque</b> (never decoded nest-side);
    /// <paramref name="mailbox"/> is display metadata (<c>INBOX</c> for a received
    /// conversation message, which has no IMAP mailbox); <paramref name="subject"/> is
    /// sealed to the actor's own key. A <see cref="SpamWriteResult.Sealed"/> of
    /// <c>false</c> ⇒ ServerPath — unlike <see cref="TrainSpamModelClientAsync"/>, there
    /// is NO server-train fallback for a conversation message (client-only encrypted
    /// content the nest can't read), so the caller degrades to a silent no-op.</summary>
    Task<SpamWriteResult> TrainSpamModelClientMailAsync(
        string text, bool isSpam, byte[] messageId, string mailbox, string subject);
}

/// <summary>
/// Production <see cref="ISpamModelClientWrite"/> over the shared-Rust
/// <see cref="MailSettingsMachine"/>, built lazily from the session's WS-RPC seam (the
/// same machine <c>MailSettingsPanel</c> builds). Tolerates a failed build / call → the
/// ServerPath result, so a social-only or mail-disabled actor (no MSEK) degrades to the
/// server-side train rather than surfacing an error.
/// </summary>
internal sealed class MachineSpamModelClientWrite : ISpamModelClientWrite
{
    private readonly INestRpcClient _rpc;
    private MailSettingsMachine? _machine;

    public MachineSpamModelClientWrite(INestRpcClient rpc) => _rpc = rpc;

    private async Task<MailSettingsMachine?> MachineAsync()
    {
        if (_machine is not null) return _machine;
        try { _machine = await _rpc.BuildMailSettingsMachineAsync(); }
        catch { _machine = null; }
        return _machine;
    }

    public async Task<bool> SealedSpamWriteAvailableAsync()
    {
        try
        {
            var m = await MachineAsync();
            return m is not null && await m.SealedSpamWriteAvailable();
        }
        catch { return false; }
    }

    public async Task<SpamWriteResult> TrainSpamModelClientAsync(string text, bool isSpam)
    {
        try
        {
            var m = await MachineAsync();
            if (m is null) return new SpamWriteResult(false, 0);
            var w = await m.TrainSpamModelClient(text, isSpam);
            return new SpamWriteResult(w.@sealed, w.@sampleCount);
        }
        catch { return new SpamWriteResult(false, 0); }
    }

    public async Task<SpamWriteResult> TrainSpamModelClientMailAsync(
        string text, bool isSpam, byte[] messageId, string mailbox, string subject)
    {
        try
        {
            var m = await MachineAsync();
            if (m is null) return new SpamWriteResult(false, 0);
            var w = await m.TrainSpamModelClientMail(text, isSpam, messageId, mailbox, subject);
            return new SpamWriteResult(w.@sealed, w.@sampleCount);
        }
        catch { return new SpamWriteResult(false, 0); }
    }
}
