import SwiftUI
import FaunaKit
import WidgetKit

struct InboxView: View {
    let appState: WatchAppState
    let apiClient: APIClient?

    @State private var conversations: [WatchMessage] = []
    @State private var isLoading = false

    var body: some View {
        NavigationStack {
            if isLoading && conversations.isEmpty {
                ProgressView()
            } else if conversations.isEmpty {
                Text(L.common.noMessagesYet)
                    .foregroundStyle(.secondary)
            } else {
                List(conversations) { msg in
                    NavigationLink {
                        MessageDetailView(
                            message: msg,
                            appState: appState,
                            apiClient: apiClient
                        )
                    } label: {
                        VStack(alignment: .leading, spacing: 2) {
                            HStack {
                                Text(msg.senderDisplay)
                                    .font(.caption2)
                                    .lineLimit(1)
                                Spacer()
                                Text(msg.timestamp.relativeFormatted)
                                    .font(.caption2)
                                    .foregroundStyle(.secondary)
                            }
                            Text(msg.subject)
                                .font(.headline)
                                .lineLimit(1)
                            Text(msg.bodyPreview)
                                .font(.caption)
                                .foregroundStyle(.secondary)
                                .lineLimit(1)
                        }
                    }
                }
            }
        }
        .task { await fetchInbox() }
    }

    private func fetchInbox() async {
        guard let api = apiClient, appState.actorId != nil,
              appState.secretHex != nil else { return }
        isLoading = true
        defer { isLoading = false }
        do {
            let payloads = try await api.fetchInbox()
            var msgs: [WatchMessage] = []
            for payload in payloads.prefix(50) {
                if let jsonStr = try? decode_email(Array(payload)),
                   let data = jsonStr.data(using: .utf8),
                   let v = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
                   v["valid"] as? Bool == true {
                    let ts = (v["timestamp"] as? Double).map {
                        Date(timeIntervalSince1970: $0 / 1_000_000)
                    } ?? .now
                    msgs.append(WatchMessage(
                        id: v["post_id"] as? String ?? UUID().uuidString,
                        fromActorId: v["from"] as? String ?? "",
                        subject: v["subject"] as? String ?? "(no subject)",
                        bodyPreview: String((v["body"] as? String ?? "").prefix(200)),
                        timestamp: ts
                    ))
                }
            }
            conversations = msgs.sorted { $0.timestamp > $1.timestamp }

            // Update complication data
            let defaults = UserDefaults(suiteName: AppleIdentifiers.watchAppGroup)
            defaults?.set(conversations.count, forKey: "unread_count")
            if let first = conversations.first {
                defaults?.set(first.senderDisplay, forKey: "last_sender")
                defaults?.set(first.subject, forKey: "last_subject")
            }
            WidgetCenter.shared.reloadAllTimelines()
        } catch {
            // Silently fail — Watch is a glance device
        }
    }
}

/// Lightweight message struct for Watch display (not SwiftData)
struct WatchMessage: Identifiable {
    let id: String
    let fromActorId: String
    let subject: String
    let bodyPreview: String
    let timestamp: Date

    var senderDisplay: String {
        shortId(hex: fromActorId)
    }
}
