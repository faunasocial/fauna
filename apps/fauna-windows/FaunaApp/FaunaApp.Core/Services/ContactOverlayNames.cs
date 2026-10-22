using System;
using uniffi.fauna_conversations;
using uniffi.fauna_core;
using uniffi.fauna_ffi;

namespace FaunaApp.Core.Services;

/// <summary>
/// The one windows door onto the private contact overlay's UniFFI face
/// (<c>libs/fauna-ffi/src/contact_overlays.rs</c>; contacts.md § The private
/// overlay). Every surface keyed on the person the overlay is about — roster row,
/// knock sender, feed author, subscription author, Profile header — reads its names
/// through <see cref="Open"/>, so no windows surface resolves a person's name, joins
/// a label line or re-derives the roster filter itself.
///
/// <para><b>The face is rebuilt over the manager the app reads from NOW, on every
/// <see cref="Open"/>.</b> Construction is cheap, and it is also what registers the
/// overlay seam for a manager no conversations session serves. The app wires the
/// manager source once (<see cref="UseManager"/>, from <c>ConversationsManagerHost.Instance</c>);
/// <c>ConversationsManagerHost.ResetForActorChange</c> swaps that instance, which is
/// why a reader is never cached across a reload. Never compare readers or faces by
/// reference — C# mints a new wrapper per crossing.</para>
///
/// <para>Member chips and message senders never read here: their names come from the
/// conversations snapshot, which applies the paint gate.</para>
///
/// <para>With no manager wired (unit tests, a view model built before login) the
/// source answers one shared EMPTY manager, so every read degrades to the public
/// fallback the shared resolver itself gives (handle, else the canonical short id) —
/// never a windows-side fallback.</para>
/// </summary>
internal static class ContactOverlayNames
{
    private static readonly Lazy<ConversationsManager> EmptyManager = new(() => new ConversationsManager());
    private static Func<ConversationsManager> _source = () => EmptyManager.Value;

    /// <summary>Wire the manager the app reads from. Called once at startup with a
    /// thunk, so an actor-change swap is followed without re-wiring.</summary>
    internal static void UseManager(Func<ConversationsManager> source) => _source = source;

    /// <summary>Raised (on whatever thread the manager's observer ran) when the
    /// projection's revision moved. A page re-reads its names on it —
    /// <c>DispatcherQueue.TryEnqueue</c> first — and unsubscribes when it leaves.
    /// Raised by <c>ContactOverlayChangeObserver</c>, the process-lifetime observer
    /// registered on the manager.</summary>
    internal static event Action? Moved;

    internal static void RaiseMoved() => Moved?.Invoke();

    /// <summary>A reader over the current manager's overlay projection. Dispose it
    /// when the pass is done.</summary>
    internal static Reader Open() => new(new FfiContactOverlays(_source()));

    /// <summary>The projection's reads, one face for one pass.</summary>
    internal sealed class Reader : IDisposable
    {
        private readonly FfiContactOverlays _face;

        internal Reader(FfiContactOverlays face) => _face = face;

        /// <summary>Moves exactly when the projection's content does.</summary>
        internal ulong Revision() => _face.Revision();

        /// <summary>What the viewer calls <paramref name="actorId"/>: the nickname
        /// when one is set, else the public name; <c>Public</c> is the public name a
        /// nickname replaced, for the secondary line.</summary>
        internal PeerLabel PeerLabel(string? displayName, string? handle, string actorId) =>
            _face.PeerLabel(displayName, handle, actorId);

        /// <summary>The roster row's <c>contact-labels</c> line; null = no label.</summary>
        internal string? LabelsLine(string actorId) => _face.LabelsLine(actorId);

        /// <summary>Whether a roster row matches the <c>contacts-search-field</c>
        /// query (handle, domain, actor id, nickname and labels; never notes).</summary>
        internal bool MatchesFilter(string query, string? handle, string? domain, string actorId) =>
            _face.MatchesFilter(query, handle, domain, actorId);

        /// <summary>What a subscription row calls its creator.</summary>
        internal string SubscriptionAuthorLabel(string? handle, byte[] authorId) =>
            _face.SubscriptionAuthorLabel(handle, authorId);

        /// <summary>The completion under the label add field: the vocabulary labels
        /// <paramref name="typed"/> starts, minus the <paramref name="staged"/> ones
        /// (shared <c>label_suggestions</c>).</summary>
        internal System.Collections.Generic.IReadOnlyList<string> LabelSuggestions(
            string typed, System.Collections.Generic.IReadOnlyList<string> staged) =>
            _face.LabelSuggestions(typed, staged.ToArray());

        /// <summary>The private section's editor for <paramref name="actorId"/>.</summary>
        internal FfiOverlayEditor Editor(string actorId) => _face.Editor(actorId);

        public void Dispose() => _face.Dispose();
    }
}
