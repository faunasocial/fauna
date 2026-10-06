import SwiftUI

/// The invite-button action for the event detail page — factored out so the
/// automation sibling drives the exact same code path the Button does (each
/// caller's own doc comment already said so). `inviteEmail` is a `Binding`
/// because it is each caller's own View-local `@State` (macOS's
/// `MacEventDetailView`, iOS's `EventDetailView`) — SwiftUI state can't be
/// shared across two independent View structs, the same reason `selectedPost`
/// (`FeedPostCardOpen.swift`) takes its local echo by reference too; `Binding`
/// rather than `inout` because an actor-isolated `@State` property can't be
/// passed `inout` across an `await` at all (a property-wrapper-backed var
/// desugars to a computed accessor, not a plain stored var). Was a
/// byte-identical per-target twin until this harvest pass found it
/// .
@MainActor
public func submitInvite(vm: EventsVM, inviteEmail: Binding<String>) async {
    await vm.invite(email: inviteEmail.wrappedValue)
    inviteEmail.wrappedValue = ""
}
