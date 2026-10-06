import SwiftUI
import FaunaKit
#if os(iOS)
import VisionKit
#endif

struct IdentityImportView: View {
    @Environment(AppState.self) private var appState
    @Bindable var vm: OnboardingVM
    @State private var selectedTab = 1  // start on paste tab; QR tab is tab 0
    @State private var importSecret: String = ""

    var body: some View {
        VStack(spacing: 20) {
            automationText(Ids.pageHeading, L.onboarding.identityImport.title)
                .font(.title2.bold())

            Picker("", selection: $selectedTab) {
                Text(L.onboarding.identityImport.scanTab).tag(0)
                Text(L.onboarding.identityImport.pasteTab).tag(1)
            }
            .pickerStyle(.segmented)
            .padding(.horizontal)

            if selectedTab == 0 {
                qrScannerSection
            } else {
                pasteSection
            }

            if let error = vm.errorMessage {
                ErrorBanner(message: error)
            }
        }
        .padding()
        .toolbar {
            ToolbarItem(placement: .navigationBarLeading) {
                Button(L.common.back) {
                    vm.machine.back()
                }
                .accessibilityIdentifier(Ids.identityImportBackButton)
                .automationActivate(Ids.identityImportBackButton) { vm.machine.back() }
            }
        }
    }

    @ViewBuilder
    private var qrScannerSection: some View {
        VStack(spacing: 12) {
            Text(L.onboarding.identityImport.scanSubtitle)
                .font(.subheadline)
                .foregroundStyle(.secondary)
                .multilineTextAlignment(.center)

            #if os(iOS)
            if DataScannerViewController.isSupported && DataScannerViewController.isAvailable {
                QRScannerView { code in
                    handleScannedCode(code)
                }
                .frame(height: 300)
                .clipShape(RoundedRectangle(cornerRadius: 12))
                .accessibilityIdentifier(Ids.qrCameraView)
                // Non-interactive surface — register for presence so the driver's
                // is_visible("qr-camera-view") resolves (count>0) when shown.
                .automationValue(Ids.qrCameraView)
            } else {
                cameraUnavailable
            }
            #else
            cameraUnavailable
            #endif
        }
    }

    private var cameraUnavailable: some View {
        ContentUnavailableView(
            L.onboarding.identityImport.cameraUnavailable,
            systemImage: "camera.fill",
            description: Text(L.onboarding.identityImport.cameraUnavailableHint)
        )
        .frame(height: 200)
        .accessibilityIdentifier(Ids.qrCameraView)
        .automationValue(Ids.qrCameraView)
    }

    private var pasteSection: some View {
        VStack(spacing: 16) {
            Text(L.onboarding.identityImport.pasteSubtitle)
                .font(.subheadline)
                .foregroundStyle(.secondary)
                .multilineTextAlignment(.center)

            TextField(L.onboarding.identityImport.pastePlaceholder, text: $importSecret)
                .textFieldStyle(.roundedBorder)
                .autocorrectionDisabled()
                .textInputAutocapitalization(.never)
                .font(.system(.body, design: .monospaced))
                .accessibilityIdentifier(Ids.pasteSecretField)
                .automationField(Ids.pasteSecretField, text: $importSecret)

            Button(L.onboarding.identityImport.`import`) { submitImport() }
            .buttonStyle(.borderedProminent)
            .controlSize(.large)
            .disabled(importSecret.isEmpty)
            .accessibilityIdentifier(Ids.importSubmitButton)
            .automationActivate(Ids.importSubmitButton, isEnabled: { !importSecret.isEmpty }) {
                submitImport()
            }
        }
        .padding(.horizontal)
    }

    /// The real "Import" action, referenced by both the `Button` and its
    /// `automationActivate` registration so the two never diverge
    /// (apple-e2e-automation.md § Resolved design point). Both the paste tab and
    /// the QR scanner feed the raw field through the shared parser in
    /// `vm.importIdentity` (handle pre-fill + invalid-secret error included).
    private func submitImport() {
        vm.importIdentity(importSecret, append: appState.isAddingAccount)
    }

    private func handleScannedCode(_ value: String) {
        vm.importIdentity(value, append: appState.isAddingAccount)
    }
}

#if os(iOS)
struct QRScannerView: UIViewControllerRepresentable {
    let onScan: (String) -> Void

    func makeUIViewController(context: Context) -> DataScannerViewController {
        let scanner = DataScannerViewController(
            recognizedDataTypes: [.barcode(symbologies: [.qr])],
            isHighlightingEnabled: true
        )
        scanner.delegate = context.coordinator
        try? scanner.startScanning()
        return scanner
    }

    func updateUIViewController(_ controller: DataScannerViewController, context: Context) {}

    func makeCoordinator() -> Coordinator { Coordinator(onScan: onScan) }

    class Coordinator: NSObject, DataScannerViewControllerDelegate {
        let onScan: (String) -> Void
        private var handled = false

        init(onScan: @escaping (String) -> Void) { self.onScan = onScan }

        func dataScanner(_ dataScanner: DataScannerViewController, didAdd items: [RecognizedItem], allItems: [RecognizedItem]) {
            guard !handled else { return }
            for item in items {
                if case .barcode(let barcode) = item,
                   let value = barcode.payloadStringValue,
                   value.lowercased().hasPrefix("fauna://identity") {
                    handled = true
                    dataScanner.stopScanning()
                    onScan(value)
                    return
                }
            }
        }
    }
}
#endif
