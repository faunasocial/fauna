import Foundation
import Testing
@testable import FaunaKit

#if !FAUNA_KIDS

// The moderation queue's `train-correction-button` on a SERVER row runs the one
// shared flow `MailSettingsMachine.trainModerationCorrection` (sealed model write,
// then the nest half always — moderation.md § State & data shape → *Training
// correction*). The VM's job is only to route into it exactly once; the
// nest-half-after-sealed-write proof is the shared Rust witness
// `spam_model_write.rs::correction_trains_sealed_then_reports_to_the_nest`.

private final class RecordingTrainer: ModerationTraining, @unchecked Sendable {
    private(set) var corrections: [(contentId: String, isSpam: Bool)] = []
    private(set) var standaloneTrains = 0
    var failure: String?

    struct Failure: Error, LocalizedError {
        let message: String
        var errorDescription: String? { message }
    }

    func trainModerationCorrection(contentId: String, isSpam: Bool) async throws -> SpamModelClientWrite {
        corrections.append((contentId, isSpam))
        if let failure { throw Failure(message: failure) }
        return SpamModelClientWrite(sealed: true, sampleCount: 1)
    }

    func trainSpamModelClient(text: String, isSpam: Bool) async throws -> SpamModelClientWrite {
        standaloneTrains += 1
        return SpamModelClientWrite(sealed: true, sampleCount: 1)
    }
}

private let serverRow = QueueRow(contentId: "content-1", contentType: "post",
                                 category: "spam", confidencePerMille: 900, action: 1,
                                 timestamp: 0, source: .server)

@Suite("ModerationQueueVM server-row correction")
@MainActor
struct ModerationQueueCorrectionTests {
    @Test("a server-row correction runs the shared flow exactly once, as a ham verdict")
    func serverRowRunsTheSharedFlowOnce() async {
        let trainer = RecordingTrainer()
        let vm = ModerationQueueVM()
        vm.seedMailSettingsForTest(trainer)

        await vm.correct(row: serverRow)

        #expect(trainer.corrections.count == 1)
        #expect(trainer.corrections.first?.contentId == "content-1")
        #expect(trainer.corrections.first?.isSpam == false)
        #expect(trainer.standaloneTrains == 0, "the VM must not re-run the model half itself")
        #expect(vm.errorMessage == nil)
    }

    @Test("a failing shared flow surfaces on errorMessage")
    func failureSurfaces() async {
        let trainer = RecordingTrainer()
        trainer.failure = "train failed"
        let vm = ModerationQueueVM()
        vm.seedMailSettingsForTest(trainer)

        await vm.correct(row: serverRow)

        #expect(vm.errorMessage?.contains("train failed") == true)
    }

    @Test("no mail-settings machine is an error, not a silent skip")
    func noMachineIsAnError() async {
        let vm = ModerationQueueVM()

        await vm.correct(row: serverRow)

        #expect(vm.errorMessage?.contains("no mail settings machine") == true)
    }
}

#endif
