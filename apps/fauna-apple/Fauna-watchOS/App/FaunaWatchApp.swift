import SwiftUI
import FaunaKit

@main
struct FaunaWatchApp: App {
    @State private var appState = WatchAppState()
    @State private var apiClient: APIClient?
    @Environment(\.scenePhase) private var scenePhase

    var body: some Scene {
        WindowGroup {
            if appState.isBootstrapped {
                WatchTabView(appState: appState, apiClient: apiClient)
            } else {
                NotBootstrappedView()
            }
        }
        .onChange(of: scenePhase) { _, newPhase in
            switch newPhase {
            case .active:
                Task { await activate() }
            default:
                break
            }
        }
        .task {
            WatchSessionManager.shared.activate(appState: appState)
        }
    }

    @MainActor
    private func activate() async {
        appState.loadFromKeychain()

        guard appState.isBootstrapped,
              let secret = appState.secretHex,
              let nodeUrlStr = appState.nodeUrl,
              let nodeUrl = URL(string: nodeUrlStr) else { return }

        if apiClient == nil {
            apiClient = APIClient(nodeUrl: nodeUrl)
        }

        guard let api = apiClient else { return }

        do {
            try await api.authenticate(secret: secret)
            appState.isConnected = true
        } catch {
            appState.isConnected = false
        }
    }
}

struct NotBootstrappedView: View {
    var body: some View {
        VStack(spacing: 12) {
            Image(systemName: "iphone.and.arrow.forward")
                .font(.largeTitle)
                .foregroundStyle(.secondary)
            Text("Open Fauna on iPhone")
                .font(.headline)
            Text("to set up your Watch")
                .font(.caption)
                .foregroundStyle(.secondary)
        }
        .padding()
    }
}

struct WatchTabView: View {
    let appState: WatchAppState
    let apiClient: APIClient?

    var body: some View {
        TabView {
            InboxView(appState: appState, apiClient: apiClient)
            WatchStatusView(appState: appState, apiClient: apiClient)
        }
        .tabViewStyle(.verticalPage)
    }
}
