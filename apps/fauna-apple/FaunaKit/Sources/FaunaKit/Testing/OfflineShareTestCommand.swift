import Foundation

// Compiled out of release artifacts (testing.md convention 15), like the app
// shells' whole `handleTestCommand` surface that calls into it — the runtime
// `FAUNA_E2E_*` gates are the inner switch WITHIN a test-capable build, never
// the boundary. Also EXCISED BY `FAUNA_EXCISE_P2P_SHARE`, with the ceremony it
// drives (the reason is written once, at the top of `SharePlaneModel.swift`).
#if DEBUG && !FAUNA_EXCISE_P2P_SHARE

/// Shared handler for the co-present ceremony's two cross-app TestAgent
/// commands — `offline_share_advance_clock` and `offline_share_drop_connections`
/// (`docs/goal/behavior/p2p.md` § Offline share initiation, outcomes 5 and 6:
/// a dropped connection picks the same share up again; a lapsed receive
/// window refuses the initiator like a stranger).
///
/// Lives in FaunaKit so the macOS + iOS shells share ONE implementation,
/// mirroring `E2eLoudSurfaces`. Drives the SAME shared Rust seam tui's and
/// linux's `automation.rs`/`main.rs` arms do, reached here through the
/// `test-helpers` UniFFI exports — so every app moves the ceremony's fake
/// clock and cuts its connections through one code path.
public enum OfflineShareTestCommand {
    /// `offline_share_drop_connections`'s result: the shell stashes `.report`
    /// in `machineMethodResultBox`, and must surface `.refused` as a LOUD
    /// TestAgent failure (convention 11) with the slot left empty — never a
    /// stale neighbour's count.
    public enum Outcome {
        case report(String)
        case refused(String)
    }

    /// `offline_share_advance_clock` — move THIS PROCESS's co-present
    /// ceremony ADMISSION clock (`fauna_sync_engine::ceremony_clock`), the
    /// `now` a receive-act expectation is minted and judged against. The
    /// window is a 15-minute Rust constant, so a journey reaches "someone
    /// arriving after it has lapsed is refused like a stranger" only by
    /// moving this clock (convention 14's fake clock, never a sleep).
    /// `now_offset_secs: 0` resets it.
    ///
    /// ⚠ Process-wide, and nothing auto-resets it — a leftover offset lapses
    /// the next expectation this process mints.
    ///
    /// Nothing to re-render: the offset is read at admission time, by the
    /// seat's own listener, so unlike the delegation clock this command owes
    /// no refresh, and — unlike every other command in this file — it cannot
    /// refuse: the setter takes effect immediately, before or after a seat
    /// binds.
    public static func applyAdvanceClock(_ command: [String: Any]) {
        let offset = TestCommandArgs.nowOffsetSecs(command)
        offlineShareAdvanceClock(nowOffsetSecs: offset)
    }

    /// `offline_share_drop_connections` — drop every connection a
    /// counterpart has open to THIS SESSION's bound ceremony listener,
    /// keeping the listener up, and report how many were dropped
    /// (`actions/backups.py::drop_offline_share_connections` reads the count
    /// off `state.machine_method_result`). It is the link between two
    /// devices failing part-way through a ceremony, which a journey cannot
    /// otherwise cause — the witness that "the share picks up again without
    /// either person entering the code a second time" gets a witness. With
    /// no seat bound it refuses loudly rather than reporting a drop that
    /// never happened, mirroring tui's arm.
    public static func applyDropConnections(api: APIClient?) -> Outcome {
        guard let api else {
            return .refused(
                "offline_share_drop_connections: no fauna client, so no ceremony "
                    + "seat to drop connections from")
        }
        do {
            let dropped = try api.dropOfflineShareConnectionsForTest()
            return .report(String(dropped))
        } catch {
            return .refused("offline_share_drop_connections: \(error)")
        }
    }

    /// `offline_share_hold_serves` — turn THIS PROCESS's share-plane SERVE hold
    /// on or off (`fauna_sync_engine::share_serve_tally`), so the next manifest
    /// a peer asks this seat for parks unanswered and `share_serve_tally`'s
    /// `parked` count says so (convention 14: "part-way through a transfer" is a
    /// state a journey waits for, never a race). Process-wide, nothing
    /// auto-resets it; a missing `hold` reads as `false`, the lift, like tui's
    /// arm. Cannot refuse.
    public static func applyHoldServes(_ command: [String: Any]) {
        offlineShareHoldServes(on: (command["hold"] as? Bool) ?? false)
    }

    /// The `share_serve_tally` state key's body — `{manifests: {path: n},
    /// chunks: {path: n}, parked: n, held: b}`, the FFI mirror of the shared
    /// recorder's snapshot (`fauna_e2e_agent::SHARE_SERVE_TALLY_KEY`). A plain
    /// lock-and-clone, legal on the state-serialisation path.
    public static func serveTallyState() -> [String: Any] {
        let tally = shareServeTally()
        return [
            "manifests": tally.manifests,
            "chunks": tally.chunks,
            "parked": tally.parked,
            "held": tally.held,
        ]
    }

    /// `offline_share_probe_set` — dial the peer named by `peer_code` as this
    /// seat's own identity, claim the set in the admission exchange and report
    /// what came back (`fauna_sync_engine::share_probe`), asking on after a
    /// refused admission as a hostile client would. The report rides the
    /// machine-result slot as a JSON string, which `probe_shared_set` decodes.
    /// Fails loudly (convention 11) with no signed-in session; the FFI itself
    /// refuses when no ceremony seat is bound.
    public static func applyProbeSet(_ command: [String: Any], api: APIClient?) async -> Outcome {
        guard let api else {
            return .refused("offline_share_probe_set: no fauna client to probe from")
        }
        let hashes = (command["manifest_hashes"] as? [Any])?.compactMap { $0 as? String } ?? []
        let peerCode = command["peer_code"] as? String ?? ""
        let groupIdHex = command["group_id_hex"] as? String ?? ""
        do {
            let report = try await api.offlineShareProbeSetForTest(
                peerCode: peerCode, groupIdHex: groupIdHex, manifestHashes: hashes)
            let body: [String: Any] = [
                "dialed": report.dialed,
                "dial_error": report.dialError ?? NSNull(),
                "admitted": report.admitted,
                "admit_error": report.admitError ?? NSNull(),
                "rows": report.rows,
                "paths": report.paths,
                "rows_error": report.rowsError ?? NSNull(),
                "manifest_hashes": report.manifestHashes,
                "manifests": report.manifests,
                "manifest_errors": report.manifestErrors,
            ]
            let data = try JSONSerialization.data(withJSONObject: body, options: [.sortedKeys])
            return .report(String(decoding: data, as: UTF8.self))
        } catch {
            return .refused("offline_share_probe_set: \(error)")
        }
    }
}

#endif
