import SwiftUI

/// The shared "Task delegation" Settings sub-page (macOS + iOS, one FaunaKit
/// view) — the per-task-kind runner + assignment surface
/// (`docs/goal/behavior/participants.md` § Task delegation; rail placement
/// `docs/goal/ui/settings.md` § Navigation model, after Nests). A dumb
/// renderer of `FfiTaskDelegationRow` + dispatcher of a pin write over the
/// shared `FfiTaskDelegationView` (via `TaskDelegationVM`); no delegation
/// policy here (priority #2). Element IDs match
/// `tests/e2e-unified/ui.yaml` `task-delegation` exactly. Reference
/// implementation: linux `apps/fauna-linux/src/settings/task_delegation.rs`.
///
/// **macOS passes `.indexOnly`; iOS passes `.viewerOnly`** (participants.md § The
/// assignment picker) — a phone is always battery-mobile and never runs a
/// heavy task kind, so its picker legitimately offers only "Automatic" plus a
/// pin made elsewhere, rendered so it stays escapable. Never add a "This
/// device" option on iOS; `setAssignment` would reject it anyway
/// (`resolve_pin` → `NotPinnable`).
///
/// **macOS moved `.runner` → `.indexOnly` on 2026-08-15**, when its in-app
/// segment-backup upload driver was deleted at the slice-5 flip
/// (`backup-restore.md` § Background Tasks → *Flip status (slice 5)*): it still
/// builds the content index, so it stays a legal `index` self-pin target, but it
/// no longer drives `backup-upload` and must not be offered as one — the source
/// nest is the writer. Retracting the *declaration* alongside the driver is the
/// load-bearing half; leaving it would hand the user a self-pin that can only
/// ever wait (the exact stranding linux hit in 2026-07). windows keeps `.runner`
/// until its own arm lands.
public struct TaskDelegationView: View {
    @Environment(FaunaClient.self) private var client: FaunaClient?
    @State private var vm = TaskDelegationVM()
    /// Re-load on every visit, not just first mount — the runner column is
    /// **live** advisory-lease state. macOS/iOS pass `appState.navGeneration`
    /// so a *re*-navigation to the same page refetches (a first-mount-only
    /// `.task` would never satisfy the e2e's re-`navigate()` polls — mirrors
    /// `FamilyView`/`DevicesView`).
    var reloadToken: Int = 0

    public init(reloadToken: Int = 0) {
        self.reloadToken = reloadToken
    }

    public var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 16) {
                automationText(Ids.pageHeading, L.taskDelegation.title)
                    .font(.title2)
                Text(L.taskDelegation.description)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)

                // Absent from the tree when nil — a registered-but-empty
                // element would read as present.
                if let error = vm.errorMessage {
                    ErrorBanner(message: error)
                }

                VStack(alignment: .leading, spacing: 12) {
                    ForEach(Array(vm.rows.enumerated()), id: \.element.taskKind) { index, row in
                        kindRow(row, index: index)
                    }
                }
                .accessibilityIdentifier(Ids.taskDelegationList)
                // A bare `.accessibilityIdentifier` is invisible to the
                // in-process driver — only `automation*` modifiers register.
                // The container has no meaningful "value"; register presence
                // (row count, useful for debugging) so `wait_for(
                // "task-delegation-list")` resolves once the page has mounted.
                .automationValue(Ids.taskDelegationList, text: { "\(vm.rows.count)" })
            }
            .padding(24)
            .frame(maxWidth: .infinity, alignment: .leading)
        }
        .pageTitle(L.taskDelegation.title)
        .task(id: reloadToken) {
            guard let client else { return }
            await vm.configure(
                api: client.api, deviceId: client.deviceId,
                capability: TaskDelegationVM.forThisBuild)
        }
    }

    // MARK: - One `task-delegation-kind-item` row

    /// Every row carries the **bare** id `task-delegation-kind-item` (rows are
    /// addressed positionally in `LIVE_TASK_KINDS` order — `.automationScope`
    /// gives it real subtree containment for its children).
    private func kindRow(_ row: FfiTaskDelegationRow, index: Int) -> some View {
        let runnerText = runnerLabel(row.runner, labels: vm.deviceLabels)
        return VStack(alignment: .leading, spacing: 4) {
            automationText(Ids.taskDelegationKindName, renderLocalizedText(row.name))
                .font(.headline)
            automationText(Ids.taskDelegationKindRunner, runnerText)
                .font(.caption)
                .foregroundStyle(.secondary)
                // `.id` keyed on the rendered text so a post-`.onAppear` change
                // (the runner column is *live* advisory-lease state; a reload
                // can change it with no row re-identity) forces a fresh mount →
                // the in-process `_AutomationRegister` re-fires `.onAppear` and
                // re-registers the CURRENT closure — otherwise the registry
                // keeps serving the closure captured at first mount.
                // apple-e2e-automation.md § Stale-capture discipline.
                .id("task-delegation-kind-runner:\(runnerText)")
            assignmentPicker(row)
        }
        .padding(.vertical, 8)
        // Container id + `.contain` so the row's children stay queryable
        // alongside the row's own id (apple container-a11y rule).
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(Ids.taskDelegationKindItem)
        // Per-row presence entry so the in-process registry can `count`
        // `task-delegation-kind-item` rows (mirrors `linked-nests-item`).
        .automationValue(Ids.taskDelegationKindItem, text: { row.taskKind })
        .automationScope(Ids.taskDelegationKindItem, index: index)
    }

    /// The assignment picker — Automatic / This device (macOS/linux/windows
    /// only) / a foreign pin, rendered from `row.pinOptions` **verbatim**
    /// (participants.md § The assignment picker: the shared layer already
    /// decided the legal option set — never construct, filter, or extend it
    /// here). Selecting an option writes the pin immediately, mirroring the
    /// linux/windows renders (no local draft state).
    private func assignmentPicker(_ row: FfiTaskDelegationRow) -> some View {
        let labels = vm.deviceLabels
        let selectedKey = optionKey(row.assignment)
        let selection = Binding<String>(
            get: { optionKey(row.assignment) },
            set: { newKey in
                guard let option = row.pinOptions.first(where: { optionKey($0) == newKey })
                else { return }
                Task { await vm.setAssignment(taskKind: row.taskKind, option: option) }
            }
        )
        return Picker("", selection: selection) {
            ForEach(row.pinOptions, id: \.self) { option in
                Text(optionLabel(option, labels: labels)).tag(optionKey(option))
            }
        }
        .pickerStyle(.menu)
        .labelsHidden()
        .accessibilityIdentifier(Ids.taskDelegationAssignmentPicker)
        // `options` in the same key vocabulary `value` reports and `select`
        // receives: the legal-set journeys assert what the picker does NOT
        // offer, and convention 11's twin rule refuses an unpainted key.
        .automationSelect(
            Ids.taskDelegationAssignmentPicker,
            value: { optionKey(row.assignment) },
            options: { row.pinOptions.map(optionKey) }
        ) { picked in
            selection.wrappedValue = picked
        }
        // `.id` keyed on the selected option so a pin write (which reloads
        // `row.assignment` with no row re-identity) forces a fresh mount →
        // re-`.onAppear` → re-register the CURRENT `value`/`set` closures.
        // Same stale-capture remedy as the runner text above.
        .id("task-delegation-assignment-picker:\(selectedKey)")
    }
}

// MARK: - Label + key resolution (free functions — no VM state needed)

/// Label a runner status via the shared `fauna_core::delegation::runner_label`
/// decision over UniFFI (priority #2 — mirrors android's/web's
/// `taskDelegationRunnerLabel` FFI/wasm twin; participants.md § Task
/// delegation), resolved through the same `renderLocalizedText` pipeline
/// `row.name` uses. The throw is unreachable for a well-formed row (a
/// malformed 32-byte nest pubkey); the empty-string fallback matches that.
private func runnerLabel(_ runner: FfiRunnerStatus, labels: [String: String]) -> String {
    (try? taskDelegationRunnerLabel(runner: runner, labels: labels)).map(renderLocalizedText) ?? ""
}

/// Label one picker option via the shared `fauna_core::delegation::option_label`
/// decision over UniFFI (mirrors `runnerLabel` above).
private func optionLabel(_ option: FfiPinOption, labels: [String: String]) -> String {
    (try? taskDelegationOptionLabel(option: option, labels: labels)).map(renderLocalizedText) ?? ""
}

/// The stable cross-app picker key for one option — what the driver's
/// `select()` sends and `/element/select` receives verbatim on apple (the
/// in-process bridge does no label normalization; see
/// `tests/e2e-unified/actions/task_delegation.py`).
private func optionKey(_ option: FfiPinOption) -> String {
    switch option {
    case .automatic: "automatic"
    case .thisDevice: "this-device"
    case .other(let who): participantKey(who)
    }
}

private func participantKey(_ who: FfiParticipantRef) -> String {
    switch who {
    case .device(let deviceId): deviceId
    case .nest(let actorPubkey): data_to_hex(actorPubkey)
    }
}
