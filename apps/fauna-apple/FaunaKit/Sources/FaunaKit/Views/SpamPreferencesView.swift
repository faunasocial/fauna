import SwiftUI

/// Shared spam-preferences settings section, used by both the iOS and macOS
/// Privacy/Moderation pages so the spam UI can't diverge across the two targets
/// (#1/#3). The threshold bands come from shared Rust
/// via `SpamOptions` (the `fauna_protocol::spam` contract) — no Swift-side
/// buckets. The sliders present the per-mille `u16` wire
/// value as a 0.0–1.0 step-0.1 slider; CRUD rides `fauna.spam.{get,set}_preferences`
/// through `PrivacySettingsVM` → `APIClient` → `FfiSpamClient`.
/// See `docs/goal/ui/settings.md` § Spam threshold slider labels.
public struct SpamPreferencesView: View {
    @Bindable var vm: PrivacySettingsVM

    public init(vm: PrivacySettingsVM) {
        self.vm = vm
    }

    public var body: some View {
        // A plain `VStack`, not `Section` — `PrivacySettingsView` (this view's
        // only caller) is an eager `ScrollView { VStack }`, not a `Form`/`List`
        // (rule 6 — apple-e2e-automation.md § Registration rules), so `Section`
        // has no lazy-container parent to group inside.
        VStack(alignment: .leading, spacing: 8) {
            Text(L.status.spam.title).font(.headline)
            VStack(alignment: .leading, spacing: 12) {
                HStack {
                    Text(L.status.spam.spamThreshold)
                    Slider(value: $vm.spamThreshold, in: 0...1, step: 0.1)
                        .accessibilityIdentifier(Ids.spamThreshold)
                        // Slider: read the current value; `select`/`type` parse a
                        // numeric string and set the same bound value a drag would
                        // (clear → "" keeps the current value).
                        .automationSelect(Ids.spamThreshold,
                                          value: { String(format: "%.1f", vm.spamThreshold) }) {
                            vm.spamThreshold = Double($0) ?? vm.spamThreshold
                        }
                    Text(String(format: "%.1f", vm.spamThreshold))
                        .font(.caption.monospaced())
                        .frame(width: 30)
                    Text(SpamOptions.bandLabel(threshold: vm.spamThreshold))
                        .font(.caption2)
                        .foregroundStyle(.secondary)
                }
                HStack {
                    Text(L.status.spam.phishingThreshold)
                    Slider(value: $vm.phishingThreshold, in: 0...1, step: 0.1)
                        .accessibilityIdentifier(Ids.phishingThreshold)
                        // Slider: read + numeric set (see spam-threshold).
                        .automationSelect(Ids.phishingThreshold,
                                          value: { String(format: "%.1f", vm.phishingThreshold) }) {
                            vm.phishingThreshold = Double($0) ?? vm.phishingThreshold
                        }
                    Text(String(format: "%.1f", vm.phishingThreshold))
                        .font(.caption.monospaced())
                        .frame(width: 30)
                }

                HStack {
                    Button(L.status.spam.save) {
                        saveSpam()
                    }
                    .disabled(vm.spamPrefsLoading)
                    .buttonStyle(.borderedProminent)
                    .accessibilityIdentifier(Ids.saveSpamPrefs)
                    .automationActivate(Ids.saveSpamPrefs,
                                        isEnabled: { !vm.spamPrefsLoading }) {
                        saveSpam()
                    }
                    if vm.spamPrefsSaved {
                        Text(L.common.saved)
                            .foregroundStyle(.green)
                            .font(.caption)
                    }
                }
            }
            // `.contain` keeps the `spam-preferences` container id from clobbering
            // the child control ids (`spam-threshold`, `phishing-threshold`,
            // `save-spam-prefs`) — a bare container
            // accessibilityIdentifier overwrites every direct child's id, so the
            // e2e found 0 `spam-threshold` (memory
            // `apple-section-accessibilityid-clobbers-children`; same fix as the
            // `device-card` / `admin-stat-card` / `filter-item` rows).
            .accessibilityElement(children: .contain)
            .accessibilityIdentifier(Ids.spamPreferences)
            // Registry presence sentinel for the in-process driver (no a11y tree).
            .automationValue(Ids.spamPreferences, text: { "" })
            if let error = vm.spamPrefsError {
                ErrorBanner(message: error)
            }
        }
    }

    /// Save spam preferences. Extracted so the `save-spam-prefs` Button and its
    /// `automationActivate` sibling invoke the exact same path (no drift).
    private func saveSpam() {
        Task { await vm.saveSpamPreferences() }
    }
}
