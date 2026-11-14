// The App Store leg of a release, after the upload: it takes one uploaded build
// of one version through App Store Connect's processing, gives it the
// version's *What to Test*, attaches it to the internal and the external
// TestFlight groups and submits it to Beta App Review — each step reading App
// Store Connect first and acting only on what is not yet there, so a re-run
// after a red resumes and does nothing twice. The Beta App Review submission
// waits on the acceptance nest: while that nest does not answer from where the
// leg runs (its cloud firewall not yet lifted), the leg submits nothing,
// reports the external track `uploaded` with a note naming the wait, and ends
// green; the job's re-run after the lift submits.
//
// Owner: docs/goal/architecture/installers/store-review.md § The store legs
// (the App Store row of *How far each leg goes*; *Inside the approved job the
// store steps come last and each one converges*). It runs as a step of the
// public release workflow's `sign` job (Environment `release-macos`), after
// the upload by Apple's own tool, and nowhere else:
//
//     cat scripts/asc/common.swift scripts/asc/store-leg.swift | xcrun swift - \
//         --version <x.y.z> --what-to-test <file> --legs <file> \
//         [--build <n>] [--internal-group <name>] [--external-group <name>] \
//         [--furthest testing|public] [--processing-wait <s>] [--poll <s>] \
//         [--status] [--dry-run] [--fixture <file>]
//
// The key: `FAUNA_ASC_STORE_KEY_FILE` / `_KEY_ID` / `_ISSUER` — the App Manager
// key, from the environment only (common.swift's loader with no local store).
//
// `--what-to-test` is the text as `scripts/store-legs.py text app-store
// what-to-test <version>` printed it (that command refuses a text missing or
// approved for another version); this file sends it verbatim as the build's
// en-US *What to Test* and refuses an empty one.
//
// `--status` reads whether App Store Connect holds the build (exit 0) or not
// (exit 5) and writes nothing: the workflow's upload step asks it first, so a
// re-run never uploads twice.
//
// `--dry-run` checks the inputs and, with a key, reads; it never writes.
//
// `--legs` holds one JSON object, the furthest state reached (rewritten as
// each state is reached, so a red later in the run leaves the last state the
// store read), in the grammar of `scripts/_store_legs.py` (store, version,
// build, track, state, read, ids, note); `scripts/store-legs.py add` appends it
// to the run artifact `store-legs.json`, which the attempt refuses when it
// reports one store twice. It never writes `installer/store-review.toml`.
//
// `--furthest public` is refused: the App Review submission of the public
// version is not built yet, and a leg that silently stopped short would read
// as done.
//
// `--fixture` replays recorded replies instead of asking App Store Connect: a
// JSON object mapping `"<METHOD> <path>"` (or a bare path, meaning GET), the
// query string dropped, to a reply body, to `{"_status": <code>, "_body":
// "<text>"}` for a refusal, or to a list of those served in turn (the last
// repeated). Every write is also printed on stderr as `fixture-write: <METHOD>
// <path> <body>`. The acceptance nest's reading is the entry `"PROBE <url>"`
// (a reply is an answer, a `_status` refusal — 0 for no answer — is not, and
// no entry is never an open nest). No key is read in that mode; it is how the steps are tested
// (`tests/e2e-unified/tests/test_asc_store_leg.py`).
//
// One `ios-store-leg: <step> OK|FAILED — …` line per step on stderr. Exit
// codes: 0 done; 2, 3, 4 and 64 as common.swift says; 5 App Store Connect
// holds no such build; 6 processing failed, or still processing at the bound
// (the re-run resumes); 7 an input refused (the text, `--furthest`).

import Foundation

func say(_ line: String) { FileHandle.standardError.write(("ios-store-leg: " + line + "\n").data(using: .utf8)!) }
func red(_ code: Int32, _ step: String, _ why: String) -> Never { fail(code, "ios-store-leg: \(step) FAILED — \(why)") }

var version: String? = nil
var buildNumber = "1"
var internalGroup = "Fauna internal"
var externalGroup = "Fauna testers"
var furthestSetting = "testing"
var textPath: String? = nil
var legsPath: String? = nil
var processingWait = 1800.0
var poll = 30.0
var statusOnly = false
var dryRun = false
var fixturePath: String? = nil
let usage = "usage: store-leg.swift --version <x.y.z> --what-to-test <file> --legs <file> [--build <n>] [--internal-group <name>] [--external-group <name>] [--furthest testing|public] [--processing-wait <s>] [--poll <s>] [--status] [--dry-run] [--fixture <file>]"
var args = CommandLine.arguments.dropFirst()
func value(_ flag: String) -> String {
    guard let v = args.popFirst(), !v.isEmpty else { fail(64, "\(flag) needs a value\n\(usage)") }
    return v
}
while let arg = args.popFirst() {
    switch arg {
    case "--version": version = value(arg)
    case "--build": buildNumber = value(arg)
    case "--internal-group": internalGroup = value(arg)
    case "--external-group": externalGroup = value(arg)
    case "--furthest": furthestSetting = value(arg)
    case "--what-to-test": textPath = value(arg)
    case "--legs": legsPath = value(arg)
    case "--processing-wait":
        guard let n = Double(value(arg)), n >= 0 else { fail(64, "--processing-wait needs seconds") }
        processingWait = n
    case "--poll":
        guard let n = Double(value(arg)), n >= 0 else { fail(64, "--poll needs seconds") }
        poll = n
    case "--status": statusOnly = true
    case "--dry-run": dryRun = true
    case "--fixture": fixturePath = value(arg)
    default: fail(64, usage)
    }
}
guard let version = version, version.range(of: #"^\d+\.\d+\.\d+$"#, options: .regularExpression) != nil else {
    fail(64, "--version needs MAJOR.MINOR.PATCH\n\(usage)")
}

// --- the inputs, checked before App Store Connect is asked anything ---------
var whatToTest = ""
if !statusOnly {
    guard furthestSetting == "testing" || furthestSetting == "public" else {
        red(7, "inputs", "--furthest is testing or public, got '\(furthestSetting)'")
    }
    if furthestSetting == "public" {
        red(7, "inputs", "furthest = \"public\" asks for the App Review submission of the public version, which this leg does not perform yet; nothing was sent")
    }
    guard let path = textPath, let legs = legsPath else { fail(64, usage) }
    guard let raw = try? String(contentsOfFile: path, encoding: .utf8) else {
        red(7, "inputs", "the What to Test text \(path) is not readable")
    }
    whatToTest = raw.trimmingCharacters(in: .whitespacesAndNewlines)
    guard !whatToTest.isEmpty else { red(7, "inputs", "the What to Test text \(path) is empty") }
    // App Store Connect caps whatsNew at 4000 characters.
    guard whatToTest.count <= 4000 else { red(7, "inputs", "the What to Test text is \(whatToTest.count) characters, over App Store Connect's 4000") }
    FileManager.default.createFile(atPath: legs, contents: Data())
    say("inputs OK — \(version) build \(buildNumber), What to Test \(whatToTest.count) characters, furthest \(furthestSetting)\(dryRun ? ", dry run" : "")")
}

// --- the one source of replies ----------------------------------------------
typealias Call = (_ method: String, _ path: String, _ body: [String: Any]?) -> Reply
let call: Call
var haveKey = true
if let path = fixturePath {
    guard let data = FileManager.default.contents(atPath: path),
          let recorded = (try? JSONSerialization.jsonObject(with: data)) as? [String: Any]
    else { fail(64, "--fixture: \(path) is not a readable JSON object") }
    var served: [String: Int] = [:]
    call = { method, requestPath, body in
        let bare = String(requestPath.split(separator: "?", maxSplits: 1)[0])
        let key = "\(method) \(bare)"
        if method != "GET" {
            let text = body.flatMap { try? JSONSerialization.data(withJSONObject: $0, options: [.sortedKeys]) }
                .map { String(decoding: $0, as: UTF8.self) } ?? ""
            FileHandle.standardError.write("fixture-write: \(key) \(text)\n".data(using: .utf8)!)
        }
        var entry = recorded[key] ?? (method == "GET" ? recorded[bare] : nil)
        if let list = entry as? [Any] {
            let n = served[key, default: 0]
            served[key] = n + 1
            entry = list.isEmpty ? nil : list[min(n, list.count - 1)]
        }
        guard let reply = entry as? [String: Any] else {
            if method != "GET" { return .ok(["data": ["id": "fixture-created", "type": "fixture"]]) }
            return .refused(404, "the fixture has no reply for \(key)")
        }
        if let status = reply["_status"] as? Int { return .refused(status, (reply["_body"] as? String) ?? "") }
        return .ok(reply)
    }
} else if dryRun && ProcessInfo.processInfo.environment["FAUNA_ASC_STORE_KEY_FILE"].map({ $0.isEmpty }) ?? true {
    haveKey = false
    call = { _, _, _ in fail(2, "unreachable: a dry run without a key asks nothing") }
} else {
    let cred = loadCredential(prefix: "FAUNA_ASC_STORE", localStore: false)
    let bearer = token(cred)
    call = { method, path, body in send(method, path, body, bearer) }
}

/// One request of any verb: a JSON object on 2xx (an empty object on 204), else the status and body.
func send(_ method: String, _ path: String, _ body: [String: Any]?, _ bearer: String) -> Reply {
    if method == "GET" { return request(path, bearer) }
    guard let url = URL(string: apiBase + path) else { fail(2, "bad request path: \(path)") }
    var req = URLRequest(url: url)
    req.httpMethod = method
    req.setValue("Bearer \(bearer)", forHTTPHeaderField: "Authorization")
    req.setValue("application/json", forHTTPHeaderField: "Content-Type")
    req.timeoutInterval = 60
    if let body = body { req.httpBody = try! JSONSerialization.data(withJSONObject: body) }
    var result: (Data?, URLResponse?, Error?) = (nil, nil, nil)
    let done = DispatchSemaphore(value: 0)
    URLSession.shared.dataTask(with: req) { d, r, e in
        result = (d, r, e)
        done.signal()
    }.resume()
    done.wait()
    if let error = result.2 { fail(2, "\(method) \(path): request failed: \(error.localizedDescription)") }
    let status = (result.1 as? HTTPURLResponse)?.statusCode ?? 0
    let data = result.0 ?? Data()
    guard (200..<300).contains(status) else { return .refused(status, String(decoding: data, as: UTF8.self)) }
    if data.isEmpty { return .ok([:]) }
    return .ok(((try? JSONSerialization.jsonObject(with: data)) as? [String: Any]) ?? [:])
}

func must(_ method: String, _ path: String, _ body: [String: Any]? = nil) -> [String: Any] {
    switch call(method, path, body) {
    case .ok(let object): return object
    case .refused(let status, let text):
        // The store's own words, bounded: the answer a human may need to act on.
        fail(2, "ios-store-leg: \(method) \(path) FAILED — HTTP \(status)\n\(String(text.prefix(2000)))")
    }
}
func get(_ path: String) -> [String: Any] { must("GET", path) }
func rows(_ reply: [String: Any]) -> [[String: Any]] { (reply["data"] as? [[String: Any]]) ?? [] }
func attrs(_ row: [String: Any]) -> [String: Any] { (row["attributes"] as? [String: Any]) ?? [:] }

/// A write: performed, or in a dry run only announced.
func write(_ method: String, _ path: String, _ body: [String: Any], _ what: String) -> [String: Any] {
    if dryRun {
        say("dry run — would \(what) (\(method) \(path))")
        return [:]
    }
    return must(method, path, body)
}

func minute() -> String {
    let f = DateFormatter()
    f.locale = Locale(identifier: "en_US_POSIX")
    f.timeZone = TimeZone(identifier: "UTC")
    f.dateFormat = "yyyy-MM-dd'T'HH:mm'Z'"
    return f.string(from: Date())
}

var ids: [String: String] = [:]
func leg(_ track: String, _ state: String, _ note: String) {
    guard let path = legsPath, !dryRun else { return }
    let object: [String: Any] = [
        "store": "app-store", "version": version, "build": buildNumber, "track": track,
        "state": state, "read": minute(), "ids": ids, "note": note,
    ]
    let line = String(decoding: try! JSONSerialization.data(withJSONObject: object, options: [.sortedKeys]), as: UTF8.self)
    // One leg per store per run: the furthest state replaces the one before it.
    FileManager.default.createFile(atPath: path, contents: (line + "\n").data(using: .utf8)!)
}

if !haveKey {
    say("done — dry run with no key: the inputs hold, App Store Connect was not asked")
    exit(0)
}

// --- the build --------------------------------------------------------------
let app = appID(bundleID: defaultBundleID, keyID: fixturePath == nil ? "the store key" : "fixture", fetch: get)
func findBuild() -> [String: Any]? {
    rows(get(
        "/v1/builds?filter%5Bapp%5D=\(query(app))&filter%5BpreReleaseVersion.version%5D=\(query(version))"
            + "&filter%5Bversion%5D=\(query(buildNumber))&fields%5Bbuilds%5D=version,processingState,uploadedDate,expired"
    )).first
}
guard var build = findBuild(), let buildID = build["id"] as? String else {
    if statusOnly { say("status — App Store Connect holds no \(version) build \(buildNumber): upload it"); exit(5) }
    red(5, "build", "App Store Connect holds no \(version) build \(buildNumber); the upload step comes first")
}
ids["build"] = buildID
if statusOnly {
    say("status — App Store Connect holds \(version) build \(buildNumber) (\(attrs(build)["processingState"] as? String ?? "?")): no upload")
    exit(0)
}

// --- processing, bounded ----------------------------------------------------
let deadline = Date().addingTimeInterval(processingWait)
while true {
    let state = attrs(build)["processingState"] as? String ?? "?"
    if state == "VALID" { break }
    if state == "FAILED" || state == "INVALID" {
        red(6, "processing", "App Store Connect reads \(version) build \(buildNumber) as \(state); the version string is spent, a fix ships as the next patch")
    }
    if Date() >= deadline {
        red(6, "processing", "\(version) build \(buildNumber) is still \(state) after \(Int(processingWait)) s; re-run the job and the leg resumes here")
    }
    if poll > 0 { Thread.sleep(forTimeInterval: poll) }
    guard let again = findBuild() else { red(5, "processing", "the build vanished from App Store Connect while processing") }
    build = again
}
say("processing OK — \(version) build \(buildNumber) is VALID")
leg("testflight-internal", "uploaded", "Processed by App Store Connect.")

// --- What to Test -------------------------------------------------------------
let locale = "en-US"
let localizations = rows(get("/v1/builds/\(query(buildID))/betaBuildLocalizations?fields%5BbetaBuildLocalizations%5D=locale,whatsNew"))
if let existing = localizations.first(where: { (attrs($0)["locale"] as? String) == locale }), let lid = existing["id"] as? String {
    if (attrs(existing)["whatsNew"] as? String) == whatToTest {
        say("what-to-test OK — already set")
    } else {
        _ = write("PATCH", "/v1/betaBuildLocalizations/\(query(lid))", [
            "data": ["type": "betaBuildLocalizations", "id": lid, "attributes": ["whatsNew": whatToTest]],
        ], "replace the build's \(locale) What to Test")
        say("what-to-test OK — replaced")
    }
} else {
    _ = write("POST", "/v1/betaBuildLocalizations", [
        "data": [
            "type": "betaBuildLocalizations",
            "attributes": ["locale": locale, "whatsNew": whatToTest],
            "relationships": ["build": ["data": ["type": "builds", "id": buildID]]],
        ],
    ], "set the build's \(locale) What to Test")
    say("what-to-test OK — set")
}

// --- the groups ---------------------------------------------------------------
let groups = rows(get("/v1/betaGroups?filter%5Bapp%5D=\(query(app))&limit=200&fields%5BbetaGroups%5D=name,isInternalGroup"))
var createdGroups: Set<String> = []
func group(_ name: String, isInternal: Bool) -> String {
    if let found = groups.first(where: { (attrs($0)["name"] as? String) == name }), let gid = found["id"] as? String {
        guard (attrs(found)["isInternalGroup"] as? Bool ?? false) == isInternal else {
            red(2, "groups", "the group '\(name)' exists but is \(isInternal ? "external" : "internal"); rename one of them in App Store Connect")
        }
        return gid
    }
    // A group with no testers is harmless: who the testers are stays the owner's
    // choice, made in the console (store-review.md § The store legs, what stays a human's).
    let created = write("POST", "/v1/betaGroups", [
        "data": [
            "type": "betaGroups",
            "attributes": ["name": name, "isInternalGroup": isInternal, "hasAccessToAllBuilds": false],
            "relationships": ["app": ["data": ["type": "apps", "id": app]]],
        ],
    ], "create the \(isInternal ? "internal" : "external") group '\(name)'")
    say("groups — created the \(isInternal ? "internal" : "external") group '\(name)' (no testers: the owner adds them)")
    let gid = ((created["data"] as? [String: Any])?["id"] as? String) ?? "dry-run"
    createdGroups.insert(gid)
    return gid
}
func attach(_ gid: String, _ name: String) {
    if !createdGroups.contains(gid) {  // a group made this run holds nothing yet
        let held = rows(get("/v1/betaGroups/\(query(gid))/relationships/builds?limit=200")).compactMap { $0["id"] as? String }
        if held.contains(buildID) { say("groups OK — '\(name)' already holds the build"); return }
    }
    _ = write("POST", "/v1/betaGroups/\(query(gid))/relationships/builds", [
        "data": [["type": "builds", "id": buildID]],
    ], "attach the build to '\(name)'")
    say("groups OK — attached the build to '\(name)'")
}
let internalID = group(internalGroup, isInternal: true)
attach(internalID, internalGroup)
ids["internalGroup"] = internalID
func betaDetail() -> [String: Any] {
    switch call("GET", "/v1/builds/\(query(buildID))/buildBetaDetail?fields%5BbuildBetaDetails%5D=internalBuildState,externalBuildState", nil) {
    case .ok(let object): return attrs((object["data"] as? [String: Any]) ?? [:])
    case .refused(let status, let text): fail(2, "ios-store-leg: buildBetaDetail FAILED — HTTP \(status)\n\(String(text.prefix(2000)))")
    }
}
let internalState = betaDetail()["internalBuildState"] as? String ?? "?"
if internalState == "IN_BETA_TESTING" {
    leg("testflight-internal", "serving", "App Store Connect reads the build in beta testing for the internal group.")
    say("internal OK — the internal group is served (IN_BETA_TESTING)")
} else {
    say("internal — App Store Connect reads the build \(internalState) for the internal group")
}

let externalID = group(externalGroup, isInternal: false)
attach(externalID, externalGroup)
ids["externalGroup"] = externalID

// --- the acceptance nest ----------------------------------------------------------
// A reviewer signs in to the acceptance nest; while it sits behind its cloud
// firewall (org-nests.md § fauna.build — the acceptance nest, *Exposure*: the
// owner alone lifts it) a submitted review is rejected and the version string
// is spent. So the submission waits until the nest answers from where this leg
// runs — a hosted runner, outside the firewall's allowlist, as a reviewer is.
// The leg only READS: one unauthenticated request, bounded; only whether a 2xx
// came back is used, the reply itself is untrusted data and never read. The
// address is the cloud-project map's zone apex (`scripts/_cloud_project.py`'s
// ORG_ZONE), spelled once here and pinned equal by test_asc_store_leg.py.
let acceptanceNest = "fauna.build"
let acceptanceProbe = "https://\(acceptanceNest)/api/v1/health"
func acceptanceNestAnswers() -> (Bool, String) {
    if let path = fixturePath {
        // The fixture's reading, keyed "PROBE <url>"; no reading is never an open nest.
        let recorded = FileManager.default.contents(atPath: path)
            .flatMap { (try? JSONSerialization.jsonObject(with: $0)) as? [String: Any] } ?? [:]
        guard let reply = recorded["PROBE \(acceptanceProbe)"] as? [String: Any] else { return (false, "no reading") }
        if let status = reply["_status"] as? Int {
            return ((200..<300).contains(status), status == 0 ? "no answer" : "HTTP \(status)")
        }
        return (true, "HTTP 200")
    }
    guard let url = URL(string: acceptanceProbe) else { return (false, "bad address") }
    var req = URLRequest(url: url)
    req.timeoutInterval = 15
    let session = URLSession(configuration: .ephemeral)
    var status = 0
    var why = "no answer"
    let done = DispatchSemaphore(value: 0)
    session.dataTask(with: req) { _, r, e in
        status = (r as? HTTPURLResponse)?.statusCode ?? 0
        if e != nil { why = "no answer within 15 s" } else { why = "HTTP \(status)" }
        done.signal()
    }.resume()
    done.wait()
    return ((200..<300).contains(status), why)
}

// --- Beta App Review ------------------------------------------------------------
let submissions = rows(get("/v1/betaAppReviewSubmissions?filter%5Bbuild%5D=\(query(buildID))&fields%5BbetaAppReviewSubmissions%5D=betaReviewState"))
var reviewState: String
if let existing = submissions.first {
    ids["betaAppReviewSubmission"] = existing["id"] as? String ?? ""
    reviewState = attrs(existing)["betaReviewState"] as? String ?? "?"
    say("review OK — already submitted (\(reviewState))")
} else if case let (open, why) = acceptanceNestAnswers(), !open {
    // `uploaded`: on the external track, not submitted — not an open review
    // (store-review.md § The states), so the attempt's `stores` step and
    // `just store-review` read it as not submitted; the re-run submits.
    if dryRun {
        say("dry run — would wait: the acceptance nest does not answer from here (\(why)); nothing would be submitted")
    } else {
        leg("testflight-external", "uploaded", "Attached to the external group; not submitted to Beta App Review: the acceptance nest does not answer from outside its firewall, so a reviewer could not sign in. The job's re-run after the owner lifts the firewall submits.")
        say("review WAITING — the acceptance nest does not answer from here (\(why)): nothing was submitted to Beta App Review; re-run this job after the owner lifts its firewall")
    }
    say("done — \(version) build \(buildNumber): internal \(internalState), Beta App Review waiting on the acceptance nest\(dryRun ? " (dry run: nothing written)" : "")")
    exit(0)
} else {
    let created = write("POST", "/v1/betaAppReviewSubmissions", [
        "data": [
            "type": "betaAppReviewSubmissions",
            "relationships": ["build": ["data": ["type": "builds", "id": buildID]]],
        ],
    ], "submit the build to Beta App Review")
    let row = (created["data"] as? [String: Any]) ?? [:]
    ids["betaAppReviewSubmission"] = row["id"] as? String ?? ""
    reviewState = attrs(row)["betaReviewState"] as? String ?? "WAITING_FOR_REVIEW"
    say("review OK — submitted to Beta App Review (\(reviewState))")
}
let externalState = betaDetail()["externalBuildState"] as? String ?? "?"
switch reviewState {
case "APPROVED" where externalState == "IN_BETA_TESTING":
    leg("testflight-external", "serving", "Beta App Review approved; App Store Connect reads the build in beta testing for the external group.")
case "APPROVED":
    leg("testflight-external", "approved", "Beta App Review approved; the external group is not served yet (\(externalState)).")
case "REJECTED":
    leg("testflight-external", "rejected", "Beta App Review rejected the build; the store's reason is in App Store Connect.")
    red(6, "review", "Beta App Review rejected \(version) build \(buildNumber); the version string is spent")
default:
    leg("testflight-external", "in-review", "Submitted to Beta App Review for the external group.")
}
say("done — \(version) build \(buildNumber): internal \(internalState), Beta App Review \(reviewState), external \(externalState)\(dryRun ? " (dry run: nothing written)" : "")")
