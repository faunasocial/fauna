import SwiftUI
import FaunaKit

struct MessageDetailView: View {
    let message: WatchMessage
    let appState: WatchAppState
    let apiClient: APIClient?

    @State private var showReplySheet = false
    @State private var replyText = ""
    @State private var isSending = false

    private let cannedResponses = ["OK", "Thanks!", "On my way", "Talk later", "Sounds good"]

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 8) {
                Text(message.subject)
                    .font(.headline)
                Text(message.senderDisplay)
                    .font(.caption2)
                    .foregroundStyle(.secondary)
                Text(message.timestamp.relativeFormatted)
                    .font(.caption2)
                    .foregroundStyle(.secondary)

                Divider()

                Text(message.bodyPreview)
                    .font(.body)
            }
            .padding()
        }
        .toolbar {
            ToolbarItem(placement: .bottomBar) {
                Button { showReplySheet = true } label: {
                    Label("Reply", systemImage: "arrowshape.turn.up.left")
                }
                .disabled(isSending)
            }
        }
        .sheet(isPresented: $showReplySheet) {
            NavigationStack {
                List {
                    Section("Quick Reply") {
                        ForEach(cannedResponses, id: \.self) { response in
                            Button(response) {
                                Task { await sendReply(response) }
                            }
                        }
                    }

                    Section("Custom") {
                        TextField("Type reply...", text: $replyText)
                        Button("Send") {
                            Task { await sendReply(replyText) }
                        }
                        .disabled(replyText.isEmpty)
                    }
                }
                .navigationTitle("Reply")
            }
        }
    }

    private func sendReply(_ text: String) async {
        guard let api = apiClient, !text.isEmpty else { return }
        isSending = true
        defer { isSending = false }
        do {
            // `fauna.inbox.send` — APIClient composes the signed `(CR, Post)` tuple
            // (shared `buildSignedEmail`) and delivers it over the bearer WS-RPC
            // connection; it owns the actor secret + node URL post-`authenticate`.
            try await api.sendToInbox(
                recipientActorId: message.fromActorId,
                subject: message.subject,
                body: text
            )
            showReplySheet = false
        } catch {
            // Silently fail on Watch
        }
    }
}
