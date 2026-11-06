import Foundation
import Testing
import FaunaExtensionKit

// The macOS backup stats popover's Encryption & Compression rows: each row shows a short
// label and a distinct value, never one string as both. The value strings are public
// sentences (`backups.repo_stats.*`, the app-strings-public region); the labels are the
// apps' own, outside it.
@Suite struct BackupStatsRowTextsTests {
    @Test func compressionRowLabelIsApartFromValue() {
        #expect(L.backups.repoStats.compressionLabel != L.backups.repoStats.compression)
        #expect(!L.backups.repoStats.compressionLabel.isEmpty)
    }

    @Test func chunkSizesRowLabelIsApartFromValue() {
        #expect(L.backups.repoStats.chunkSizesLabel != L.backups.repoStats.chunkSizes)
        #expect(!L.backups.repoStats.chunkSizesLabel.isEmpty)
    }
}
