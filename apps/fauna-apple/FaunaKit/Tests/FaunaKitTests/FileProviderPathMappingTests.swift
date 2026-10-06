import XCTest

@testable import FaunaKit

/// The appex's item→parent derivation (`FileProviderItem.parentIdentifier(forRel:)`
/// delegates here). The `fetchContents` fallback-parent bug (2026-07-19) was exactly
/// a wrong answer for the nested case — keep these pinned.
final class FileProviderPathMappingTests: XCTestCase {
    func testTopLevelItemHasNoParentRel() {
        XCTAssertNil(FileProviderPathMapping.parentRel(forRel: "a.txt"))
    }

    func testSingleNestingParentIsTheDirectory() {
        XCTAssertEqual(FileProviderPathMapping.parentRel(forRel: "sub/b.txt"), "sub")
    }

    func testDeepNestingParentIsTheFullPrefix() {
        XCTAssertEqual(FileProviderPathMapping.parentRel(forRel: "a/b/c.txt"), "a/b")
    }

    func testDirectoryRelParentsLikeAFile() {
        // A synthesized subdirectory's rel carries no trailing slash (engine rows
        // are file rels; containers are synthesized from prefixes).
        XCTAssertEqual(FileProviderPathMapping.parentRel(forRel: "a/b"), "a")
        XCTAssertNil(FileProviderPathMapping.parentRel(forRel: "a"))
    }

    func testFilenamesWithDotsAndSpacesDoNotConfuseParenting() {
        XCTAssertEqual(
            FileProviderPathMapping.parentRel(forRel: "my dir/report v2.final.pdf"), "my dir")
    }
}
