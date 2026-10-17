using uniffi.fauna_conversations;
using uniffi.fauna_ffi;
using FaunaApp.Core.Services;

namespace FaunaApp.Conversations;

/// <summary>
/// Turns the conversations manager's own snapshot observer into the one "the private
/// overlay moved" signal the name surfaces re-read on
/// (<see cref="ContactOverlayNames.Moved"/>; contacts.md § The private overlay → the
/// projection re-emits when its content moves). Registered once per manager inside
/// <see cref="ConversationsManagerHost"/>'s factory — process lifetime, like
/// <see cref="RoomsRefreshObserver"/> — so a page needs no manager observer of its own
/// and a relaunch's late projection load still reaches an open page.
///
/// <para>Compares the face's <c>revision()</c> against the last one seen, so a manager
/// wake for a message re-paints nothing. The face is rebuilt over this observer's own
/// manager every time (construction is cheap); that wrapper is never compared by
/// reference.</para>
/// </summary>
internal sealed class ContactOverlayChangeObserver : SnapshotObserver
{
    private readonly ConversationsManager _manager;
    private ulong _seen = ulong.MaxValue;

    public ContactOverlayChangeObserver(ConversationsManager manager) => _manager = manager;

    public void OnChanged()
    {
        ulong now;
        try
        {
            using var face = new FfiContactOverlays(_manager);
            now = face.Revision();
        }
        catch
        {
            // Teardown race (the manager was disposed at an actor change): nothing to read.
            return;
        }
        if (now == _seen) return;
        _seen = now;
        ContactOverlayNames.RaiseMoved();
    }
}
