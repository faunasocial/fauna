import SwiftUI

/// The **store-consent lock** (`account-age-band.md` § The account age band →
/// the store-law developer duties bullet, item (3); § App surface →
/// *Store-consent surfaces*; ui.yaml `global.platform_elements.ios`
/// `store-consent-lock` / `store-consent-lock-message` /
/// `store-consent-lock-ask-button`, user-approved 2026-10-19) — the
/// `ScreenTimeLockOverlay` twin and the android `StoreConsentLockHost` twin,
/// but **whole-app**: the iOS shell mounts it over everything the window
/// renders, onboarding and the launch surfaces included, so while the shared
/// round says `Locked` no page can be reached, read or touched.
///
/// Presence, wording and the button all come from `StoreConsentStore` — the
/// one decision point. The message is the round's own lines, rendered in order
/// (the first as the heading) as one text. No sign-out: the lock is about the
/// person the store knows on this device, not the Fauna account.
public struct StoreConsentLockView: View {
    public let lines: [String]
    public let canAsk: Bool
    public let onAsk: () -> Void

    public init(lines: [String], canAsk: Bool, onAsk: @escaping () -> Void) {
        self.lines = lines
        self.canAsk = canAsk
        self.onAsk = onAsk
    }

    private var message: String { lines.joined(separator: "\n\n") }

    private var styled: AttributedString {
        var out = AttributedString()
        for (i, line) in lines.enumerated() {
            if i > 0 { out += AttributedString("\n\n") }
            var part = AttributedString(line)
            if i == 0 { part.font = .title2.bold() }
            out += part
        }
        return out
    }

    public var body: some View {
        VStack(spacing: 20) {
            Text(styled)
                .font(.body)
                .multilineTextAlignment(.center)
                .fixedSize(horizontal: false, vertical: true)
                .accessibilityIdentifier(Ids.storeConsentLockMessage)
                .automationValue(Ids.storeConsentLockMessage, text: { message })
            if canAsk {
                Button(action: onAsk) {
                    Text(L.family.storeConsentLock.askButton)
                }
                .buttonStyle(.borderedProminent)
                .accessibilityIdentifier(Ids.storeConsentLockAskButton)
                .automationActivate(Ids.storeConsentLockAskButton,
                                    text: { L.family.storeConsentLock.askButton },
                                    perform: onAsk)
            }
        }
        .padding(32)
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        // Opaque and hit-blocking, like the screen-time lock: nothing behind
        // it can be read or used.
        .background(.background)
        .contentShape(Rectangle())
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(Ids.storeConsentLock)
        .automationValue(Ids.storeConsentLock, text: { "locked" })
    }
}
