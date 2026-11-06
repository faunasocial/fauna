import Foundation
@testable import FaunaKit

// The one test fixture for "a guardian policy that blocks or collapses a
// category": the guardian's rule document, authored through the shared rule
// editor (`FfiRuleEditor.forGuardian`) exactly as the guardian's mounting does,
// so no test hand-encodes the document's bytes (family-safety.md § Content
// policy; nest-rules.md § The shared rule editor).

/// A guardian `FfiContentPolicy` whose rule document holds one category rule
/// per `(category, verdict)` pair, each at the editor's default level.
func guardianRules(_ rules: [(category: String, verdict: RuleVerdict)]) -> FfiContentPolicy {
    let editor = FfiRuleEditor.forGuardian(policy: nil)
    let reasonKey = editor.view().reasonDefaultKey
    for rule in rules {
        let index = editor.addRule(kind: .category)!
        precondition(editor.setCategory(index: index, category: rule.category))
        precondition(editor.setVerdict(index: index, verdict: rule.verdict))
        precondition(editor.setReason(index: index, lang: reasonKey, text: "Guardian rule"))
    }
    return FfiContentPolicy(document: try! editor.documentBytes())
}

/// A guardian policy with one category rule — the common case.
func guardianRule(_ category: String, _ verdict: RuleVerdict = .block) -> FfiContentPolicy {
    guardianRules([(category: category, verdict: verdict)])
}

/// The guardian policy most supervision tests seed: block `nsfw`.
func nsfwBlockRule() -> FfiContentPolicy {
    guardianRule("nsfw", .block)
}
