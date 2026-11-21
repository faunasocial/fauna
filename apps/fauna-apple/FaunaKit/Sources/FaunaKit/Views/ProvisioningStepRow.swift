import SwiftUI

/// Shared provisioning step row for both Apple targets (iOS + macOS).
///
/// Renders one `StepSnapshot` from the four-step provisioning pipeline: the
/// status glyph, the step label, an optional substep line, and an optional
/// per-step error. Row visibility is read straight off the snapshot's
/// display-projection fields — `showsSubstep` / `showsError` — which are the *single source* of the per-step
/// visibility rule (computed in `libs/fauna-provisioning`'s `recompute_display`
/// and filled by the machine's `provisioningSnapshot()` getter). The client
/// never re-derives that rule. Mirrors the Linux
/// `views/onboarding/nest_provisioning.rs` step refresher and the web block.
///
/// The status glyph, step label, and per-key sub-step text are themselves
/// single-sourced in shared Rust (`fauna_provisioning::progress::{status_glyph,
/// step_label,substep_label}`, exposed as the `provisioning{StatusGlyph,
/// StepLabel,SubstepLabel}` UniFFI free fns); this view only resolves the
/// returned `LocalizedText` through the apple i18n pipeline (`renderLocalizedText`
/// → `L.lookup`). The Skipped placeholder stays client-side text assembly. No
/// attempt count is ever shown (`onboarding-provisioning.md` § 6). See `docs/goal/behavior/value-formatting.md`
/// § Provisioning step display.
public struct ProvisioningStepRow: View {
    let snapshot: StepSnapshot

    public init(snapshot: StepSnapshot) {
        self.snapshot = snapshot
    }

    public var body: some View {
        HStack(alignment: .top, spacing: 8) {
            statusCheckbox
                .frame(width: 18, alignment: .center)

            VStack(alignment: .leading, spacing: 2) {
                automationText(
                    Ids.provisioningStepLabel,
                    renderLocalizedText(provisioningStepLabel(kind: snapshot.kind)))

                if snapshot.showsSubstep {
                    automationText(Ids.provisioningSubstep, provisioningSubstepText(snapshot))
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }

                if snapshot.showsError, let err = snapshot.lastError {
                    automationText(Ids.provisioningStepError, err)
                        .font(.caption)
                        .foregroundColor(.red)
                }
            }

            Spacer()
        }
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(Ids.provisioningStepRow)
        // Presence anchor for the indexed `provisioning-step-row` (one entry
        // per pipeline step, so `count("provisioning-step-row")` is the step
        // count) — a bare id is invisible to the in-process driver (the
        // apple-e2e-automation.md § Registration rules memory'd rule; mirrors
        // `folder-member-item`'s identical container + presence-anchor shape).
        .automationValue(
            Ids.provisioningStepRow,
            text: { renderLocalizedText(provisioningStepLabel(kind: snapshot.kind)) })
    }

    /// `provisioning-step-checkbox`'s paint. A `Running` step shows the
    /// platform's indeterminate activity indicator (`onboarding-provisioning.md`
    /// § 6, *A running step shows activity, never an attempt count*); every
    /// other status shows the shared glyph. The automation read stays the
    /// shared glyph in both cases, so a driver reads the same text whatever
    /// the paint is (the tui `spinner.rs` shape).
    @ViewBuilder
    private var statusCheckbox: some View {
        if snapshot.status == .running {
            ProgressView()
                .progressViewStyle(.circular)
                .controlSize(.small)
                .accessibilityIdentifier(Ids.provisioningStepCheckbox)
                .accessibilityValue(provisioningStatusGlyph(status: snapshot.status))
                .automationValue(
                    Ids.provisioningStepCheckbox,
                    text: { provisioningStatusGlyph(status: snapshot.status) })
        } else {
            automationText(Ids.provisioningStepCheckbox, provisioningStatusGlyph(status: snapshot.status))
                .font(.system(.body, design: .monospaced))
        }
    }
}

// MARK: - Sub-step line assembly (client-side concatenation; the per-key text
// itself comes from the shared `provisioningSubstepLabel` UniFFI fn)

/// Build the substep line. The caller gates rendering on `snapshot.showsSubstep`;
/// no attempt count is ever shown (onboarding-provisioning.md § 6). The
/// per-key text (and the `{cause}` substitution for `status_retrying`, filled
/// from `lastError`) comes from shared `provisioningSubstepLabel`.
private func provisioningSubstepText(_ snapshot: StepSnapshot) -> String {
    var text = ""
    if snapshot.status == .skipped {
        text = L.lookup("onboarding.provision.substep.status_skipped")
    } else if let key = snapshot.substep {
        text = renderLocalizedText(provisioningSubstepLabel(key: key, cause: snapshot.lastError))
    }
    return text
}
