import SwiftUI
import FaunaKit

struct MacContactDetailView: View {
    let vm: ContactsVM
    let peerId: String

    private var contact: Contact? {
        vm.contacts.first { $0.peerId == peerId }
    }

    private var knock: Knock? {
        vm.knocks.first { $0.sender == peerId }
    }

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 24) {
                // Identity
                GroupBox(L.common.identity) {
                    VStack(alignment: .leading, spacing: 8) {
                        LabeledContent("\(L.common.actorId):") {
                            Text(peerId)
                                .textSelection(.enabled)
                                .lineLimit(1)
                                .truncationMode(.middle)
                                .font(.caption.monospaced())
                        }
                        if let contact {
                            LabeledContent(L.common.status) {
                                ContactStatusBadge(status: contact.status)
                            }
                        }
                        if let knock {
                            if let node = knock.senderNode {
                                LabeledContent("\(L.status.node.title):") {
                                    Text(node)
                                        .textSelection(.enabled)
                                }
                            }
                            if let summary = knock.summary {
                                LabeledContent(L.events.summary) {
                                    Text(summary)
                                }
                            }
                        }
                    }
                    .padding(8)
                }

                // Actions
                if let contact {
                    GroupBox(L.common.actions) {
                        VStack(alignment: .leading, spacing: 8) {
                            if contact.status == "accepted" {
                                Button(L.common.confirm) {
                                    Task { await vm.confirmContact(peerId: peerId) }
                                }
                                .buttonStyle(.borderedProminent)
                            }
                            if contact.status != "blocked" {
                                Button(L.common.block, role: .destructive) {
                                    Task { await vm.blockKnock(peerId: peerId) }
                                }
                            }
                        }
                        .padding(8)
                    }
                }

                if let knock, contact == nil {
                    GroupBox(L.contacts.messageRequests.title) {
                        HStack(spacing: 12) {
                            Button(L.common.accept) {
                                Task { await vm.acceptKnock(peerId: knock.sender) }
                            }
                            .buttonStyle(.borderedProminent)

                            Button(L.common.dismiss) {
                                Task { await vm.dismissKnock(peerId: knock.sender) }
                            }

                            Button(L.common.block, role: .destructive) {
                                Task { await vm.blockKnock(peerId: knock.sender) }
                            }
                        }
                        .padding(8)
                    }
                }

                if let error = vm.errorMessage {
                    ErrorBanner(message: error)
                }
            }
            .padding()
        }
        .navigationTitle(L.common.contacts)
    }
}

// `ContactStatusBadge` is the shared apple-family capsule in FaunaKit (consumes
// `contactStatusLabel`); see FaunaKit/Sources/FaunaKit/Views/ContactStatusBadge.swift.
