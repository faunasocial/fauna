import SwiftUI

/// Shared (macOS + iOS) renderer for the folder creation wizard. A dumb view
/// over the embedded shared-Rust `FolderWizardMachine` (reached via
/// `DevicesMachineVM.wizardMachine`): it reads the rendered state from
/// `snapshot.wizard` (a `FolderWizardSnapshot`) and forwards every gesture to
/// the machine — no wizard logic client-side
/// (`docs/goal/ui/devices.md` § Where logic lives). Steps `Name → Devices
/// → Review → Done` (the scan-frequency step retired with folders re-model
/// phase 5, 2026-08-20 — the cadence is a constant, `file-sync.md` § Config,
/// the phase-5 block). Presented as a sheet on both platforms
/// (resolving the macOS nested `NavigationSplitView` crash — `devices.md`
/// § Errors).
///
/// **Folders re-model phase 2 slice e** (on tui, the lead app;
/// apple's leg): a folder has **no type**, so step 1 is
/// the name and nothing else, step 2's Source/Sync/Backup/Mirror picker gave way
/// to three place-flag checkboxes, and retention left step 3 for the nest
/// place's per-folder policy on the row (`folder-nest-*`, `FoldersContent`).
/// Eight ids retired here: `wizard-mode-sync`/`-backup`/`-web`/`-backup-warning`,
/// `wizard-device-role`, `wizard-retention-snapshots`/`-days`, and (phase 5)
/// `wizard-frequency-option` — do not reintroduce any of them
/// (`docs/goal/ui/folders.md` § Element IDs).
struct FolderWizardSheetView: View {
    let vm: DevicesMachineVM

    var body: some View {
        // `snapshot.wizard` is the source of truth; nil during the dismiss
        // animation after `closeWizard()`.
        if let wiz = vm.snapshot?.wizard {
            content(wiz)
        } else {
            Color.clear.frame(width: 1, height: 1)
        }
    }

    @ViewBuilder
    private func content(_ wiz: FolderWizardSnapshot) -> some View {
        VStack(spacing: 0) {
            header(wiz)
            Divider()

            Group {
                switch wiz.step {
                case .name: nameStep(wiz.name)
                case .devices: devicePlacesStep(wiz.devicePlaces)
                case .review, .done: reviewStep(wiz.review)
                }
            }
            .padding()
            .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)

            Divider()
            navBar(wiz)
        }
        .frame(minWidth: 460, minHeight: 400)
    }

    // MARK: - Header

    private func header(_ wiz: FolderWizardSnapshot) -> some View {
        HStack {
            Text(L.fileSync.newFolder)
                .font(.headline)
            Spacer()
            Text("Step \(stepNumber(wiz.step)) of 3")
                .font(.caption)
                .foregroundStyle(.secondary)
        }
        .padding()
    }

    // MARK: - Step 1: Name
    //
    // A folder has NO TYPE since slice e, so `wizard-mode-*` and
    // `wizard-mode-backup-warning` are retired and this step is the name and
    // nothing else.
    // ⚠ Retiring `wizard-mode-web` leaves website-folder creation unreachable
    // until phase 4 mints the website toggle — surfaced to the user and accepted
    // 2026-08-15. Do NOT re-add a mode control to close it.

    private func nameStep(_ snap: NameSnapshot) -> some View {
        Form {
            TextField(L.devices.wizard.namePlaceholder, text: Binding(
                get: { snap.name },
                set: { vm.wizardMachine?.setName(name: $0) }
            ))
            .textFieldStyle(.roundedBorder)
            .accessibilityIdentifier(Ids.wizardNameInput)
            // Writes/reads the wizard machine's name the same way a keystroke
            // does (live re-read of `snap.name`). Env-gated no-op in production.
            .automationField(Ids.wizardNameInput, text: Binding(
                get: { snap.name },
                set: { vm.wizardMachine?.setName(name: $0) }
            ))
        }
    }

    // MARK: - Step 2: Enrollment + each seat's three place flags
    //
    // The `wizard-device-role` Source/Sync/Backup/Mirror picker is RETIRED
    // (slice e): a live user called those labels "a completely incomprehensible
    // list of things" (2026-08-05), and the design's answer is not better role
    // nouns but three checkboxes that each say what they do.
    //
    // ⚠ There is NO refusal line to build: a place is only its three flags, so
    // every point the boxes span is a valid place and `continueEnabled` is
    // always true (`docs/goal/behavior/folders.md`).

    /// The three place-flag checkboxes, in canonical order: which flag of
    /// `WizardDevice` each owns, plus its element id, label and one-line
    /// explainer. The flag *meanings* live once, in
    /// `fauna_protocol::folders::PlaceFlags` — this only picks the field
    /// (linux's `PLACE_FLAG_BOXES` / android's `PlaceFlagBox`, same shape).
    private enum PlaceFlagBox: CaseIterable, Identifiable {
        case originates, accepts, appliesDeletes

        var id: String {
            switch self {
            case .originates: Ids.wizardDeviceOriginates
            case .accepts: Ids.wizardDeviceAccepts
            case .appliesDeletes: Ids.wizardDeviceAppliesDeletes
            }
        }

        /// The label/desc identity half — shared with `FoldersContent`'s twin
        /// via `PlaceFlagKind`.
        private var kind: PlaceFlagKind {
            switch self {
            case .originates: .originates
            case .accepts: .accepts
            case .appliesDeletes: .appliesDeletes
            }
        }

        var label: String { kind.label }
        var desc: String { kind.desc }

        func read(_ d: WizardDevice) -> Bool {
            switch self {
            case .originates: d.originates
            case .accepts: d.accepts
            case .appliesDeletes: d.appliesDeletes
            }
        }

        /// The seat's flag triple with this one replaced — the shape
        /// `setDeviceFlags` takes, which is whole-value like the nest write.
        func with(_ d: WizardDevice, _ on: Bool) -> (Bool, Bool, Bool) {
            switch self {
            case .originates: (on, d.accepts, d.appliesDeletes)
            case .accepts: (d.originates, on, d.appliesDeletes)
            case .appliesDeletes: (d.originates, d.accepts, on)
            }
        }
    }

    private func devicePlacesStep(_ snap: DevicePlacesSnapshot) -> some View {
        VStack(alignment: .leading, spacing: 12) {
            Text(L.devices.wizard.selectDevicesRoles)
                .font(.subheadline)

            if snap.devices.isEmpty {
                Text(L.devices.noDevices)
                    .font(.caption)
                    .foregroundStyle(.secondary)
            } else {
                List {
                    ForEach(Array(snap.devices.enumerated()), id: \.offset) { index, device in
                        // The flag boxes render for EVERY seat, not only an
                        // enrolled one — the shape tui (lead), linux and web all
                        // carry, so the three checkboxes are the step regardless
                        // of which seats are ticked.
                        VStack(alignment: .leading, spacing: 4) {
                            Button {
                                vm.wizardMachine?.toggleDeviceMember(index: UInt32(index))
                            } label: {
                                HStack {
                                    Image(systemName: device.selected
                                          ? "checkmark.square.fill" : "square")
                                    Text(device.label)
                                }
                            }
                            .buttonStyle(.plain)
                            .accessibilityIdentifier(Ids.wizardDeviceCheck)
                            // Actuate + read the live selected state (one indexed
                            // Entry per device row). Same toggle the Button runs.
                            // The text read answers the device's label — what
                            // every other app's checkbox text reads, and how a
                            // test finds a device's row — and the state rides
                            // `value`. Env-gated no-op in production.
                            .automationActivate(Ids.wizardDeviceCheck,
                                text: { device.label },
                                value: { device.selected ? "on" : "off" }) {
                                vm.wizardMachine?.toggleDeviceMember(index: UInt32(index))
                            }

                            ForEach(PlaceFlagBox.allCases) { box in
                                placeFlagBox(box, device: device, index: index)
                            }
                        }
                    }
                }
            }
        }
    }

    @ViewBuilder
    private func placeFlagBox(
        _ box: PlaceFlagBox,
        device: WizardDevice,
        index: Int
    ) -> some View {
        Button {
            setFlag(box, device: device, index: index, on: !box.read(device))
        } label: {
            HStack {
                Image(systemName: box.read(device)
                      ? "checkmark.square.fill" : "square")
                Text(box.label)
            }
        }
        .buttonStyle(.plain)
        .padding(.leading, 24)
        .accessibilityIdentifier(box.id)
        // The shared `wizard_set_device_flag` action reads the tick through
        // `get_attr(id, "state")` before clicking, so driving it twice cannot
        // undo the caller's intent — the same contract `folder-webdav-toggle`
        // carries. Env-gated no-op in production.
        .automationActivate(box.id,
            text: { box.label },
            value: { box.read(device) ? "on" : "off" }) {
            setFlag(box, device: device, index: index, on: !box.read(device))
        }

        // Each box carries its own one-line explainer — the pattern the retired
        // role picker used per seat, now per box, because the flags are the
        // thing being explained.
        Text(box.desc)
            .font(.caption)
            .foregroundStyle(.secondary)
            .padding(.leading, 48)
    }

    private func setFlag(_ box: PlaceFlagBox, device: WizardDevice, index: Int, on: Bool) {
        let (originates, accepts, appliesDeletes) = box.with(device, on)
        vm.wizardMachine?.setDeviceFlags(
            index: UInt32(index),
            originates: originates,
            accepts: accepts,
            appliesDeletes: appliesDeletes
        )
    }

    // MARK: - Step 3: Review
    //
    // The scan-frequency step that used to precede it retired with phase 5 (the
    // cadence is a constant); retention left in slice e — it is one of the nest
    // place's three snapshot knobs, editable on ANY folder's expanded row
    // (`folder-nest-*`, `FoldersContent`), not a create-time question.

    private func reviewStep(_ snap: ReviewSnapshot) -> some View {
        VStack(alignment: .leading, spacing: 16) {
            // No mode line, no retention line, no cadence line: a folder has no
            // type (slice e), retention is the nest place's policy edited on the
            // row, and the scan cadence is a constant (phase 5). The enrolled
            // list names devices, not roles — a seat's place is three flags,
            // which one review noun cannot summarize honestly.
            GroupBox(L.devices.wizard.review) {
                VStack(alignment: .leading, spacing: 8) {
                    LabeledContent(L.common.name) { Text(snap.name) }
                    if !snap.enrolled.isEmpty {
                        LabeledContent(L.devices.enrolledDevices) { Text("\(snap.enrolled.count)") }
                    }
                }
                .padding(8)
            }

            if snap.phase == .submitting {
                ProgressView(L.devices.wizard.creatingFolder)
            }
            if let error = snap.error {
                ErrorBanner(message: renderLocalizedText(error))
            }
        }
    }

    // MARK: - Navigation bar

    private func navBar(_ wiz: FolderWizardSnapshot) -> some View {
        HStack {
            Button(L.common.cancel) { vm.closeWizard() }
                .keyboardShortcut(.cancelAction)
            Spacer()
            if wiz.step != .name {
                Button(L.common.back) { vm.wizardMachine?.back() }
                    .accessibilityIdentifier(Ids.wizardBackButton)
                    // Same `back()` the Button action runs. Env-gated no-op.
                    .automationActivate(Ids.wizardBackButton) { vm.wizardMachine?.back() }
            }
            switch wiz.step {
            case .review, .done:
                Button(L.common.create) { Task { await create() } }
                    .buttonStyle(.borderedProminent)
                    .disabled(!wiz.review.createEnabled)
                    .accessibilityIdentifier(Ids.wizardCreateButton)
                    // Same async `create()` the Button runs; `isEnabled` live
                    // re-reads the machine's `createEnabled` (don't capture).
                    // Env-gated no-op in production.
                    .automationActivate(Ids.wizardCreateButton,
                        isEnabled: { vm.snapshot?.wizard?.review.createEnabled ?? false }) {
                        Task { await create() }
                    }
            default:
                Button(L.common.next) { vm.wizardMachine?.next() }
                    .buttonStyle(.borderedProminent)
                    .disabled(!continueEnabled(wiz))
                    .accessibilityIdentifier(Ids.wizardNextButton)
                    // Same `next()` the Button runs; `isEnabled` live re-reads
                    // the machine's per-step continue predicate (don't capture).
                    // Env-gated no-op in production.
                    .automationActivate(Ids.wizardNextButton,
                        isEnabled: { vm.snapshot?.wizard.map(continueEnabled) ?? false }) {
                        vm.wizardMachine?.next()
                    }
            }
        }
        .padding()
    }

    // MARK: - Helpers

    private func create() async {
        guard let machine = vm.wizardMachine else { return }
        let step = await machine.submit()
        if step == .done {
            vm.closeWizard()
            await vm.refresh()
        }
    }

    private func stepNumber(_ step: FolderWizardStep) -> Int {
        switch step {
        case .name: 1
        case .devices: 2
        case .review, .done: 3
        }
    }

    private func continueEnabled(_ wiz: FolderWizardSnapshot) -> Bool {
        switch wiz.step {
        case .name: wiz.name.continueEnabled
        case .devices: wiz.devicePlaces.continueEnabled
        case .review, .done: wiz.review.createEnabled
        }
    }
}
