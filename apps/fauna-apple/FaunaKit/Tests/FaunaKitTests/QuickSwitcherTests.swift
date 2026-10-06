import Testing
import Foundation
@testable import FaunaKit

// Quick Switcher performance tests: verify search doesn't degrade with data.
// These test the search/scoring logic, not the SwiftUI view.

@Test func matchScoreExactIsBest() {
    // Simulate the scoring logic from QuickSwitcherView
    let query = "hello"
    let exactScore = score(query, in: "hello")
    let prefixScore = score(query, in: "helloworld")
    let substringScore = score(query, in: "say hello there")
    let noMatch = score(query, in: "goodbye")

    #expect(exactScore > prefixScore)
    #expect(prefixScore > substringScore)
    #expect(substringScore > 0)
    #expect(noMatch == 0)
}

@Test func matchScoreIsCaseInsensitive() {
    #expect(score("hello", in: "HELLO") > 0)
    #expect(score("HELLO", in: "hello world") > 0)
}

@Test func searchPerformanceWith1000Items() {
    // Simulate searching 1000 conversations
    let items = (0..<1000).map { i in
        ("subject-\(i)-\(UUID().uuidString)", "actor-\(i)")
    }
    let query = "subject-500"

    let start = Date()
    var matches = 0
    for (subject, actor) in items {
        if score(query, in: subject) > 0 || score(query, in: actor) > 0 {
            matches += 1
        }
    }
    let elapsed = Date().timeIntervalSince(start)

    #expect(matches > 0)
    #expect(elapsed < 0.1, "Search over 1000 items should take < 100ms, took \(elapsed * 1000)ms")
}

@Test func searchPerformanceWith10000Items() {
    let items = (0..<10000).map { i in
        ("conversation about topic \(i % 100)", "user-\(i)")
    }
    let query = "topic 42"

    let start = Date()
    var results: [(Int, Double)] = []
    for (i, (subject, _)) in items.enumerated() {
        let s = score(query, in: subject)
        if s > 0 { results.append((i, s)) }
    }
    results.sort { $0.1 > $1.1 }
    let top15 = results.prefix(15)
    let elapsed = Date().timeIntervalSince(start)

    #expect(!top15.isEmpty)
    #expect(elapsed < 0.5, "Search over 10000 items should take < 500ms, took \(elapsed * 1000)ms")
}

// Mirror the scoring logic from QuickSwitcherView
private func score(_ query: String, in field: String) -> Double {
    let q = query.lowercased()
    let lower = field.lowercased()
    if lower == q { return 3.0 }
    if lower.hasPrefix(q) { return 2.0 }
    if lower.contains(q) { return 1.0 }
    return 0.0
}
