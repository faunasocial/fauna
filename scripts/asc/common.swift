// The shared half of the App Store Connect readers in this directory: the
// credential, the ES256 token and the GET. Each reader is run as this file
// followed by the reader's own file on the interpreter's standard input —
//
//     cat scripts/asc/common.swift scripts/asc/<reader>.swift | xcrun swift - [args]
//
// which is what the `just` recipes do (`ios-export-compliance`,
// `ios-store-builds`). Owner: docs/goal/architecture/installers/ios.md
// § Upload mechanics → *One App Store Connect API key serves notarization and
// upload*.
//
// No third-party code: the token is signed with CryptoKit, the requests go
// through Foundation's URLSession. The one request verb here is GET; the one
// file that writes to App Store Connect is `store-leg.swift`, the App Store
// leg of a release (installers/store-review.md § The store legs), which runs
// only in the release workflow's approved job, with its own key.
//
// The credential is the one App Store Connect API key the macOS release path
// already uses (installers/macos.md § Build Pipeline → *Required Environment
// Variables*): `FAUNA_NOTARY_KEY_FILE` / `FAUNA_NOTARY_KEY_ID` /
// `FAUNA_NOTARY_ISSUER` from the environment when all three are set, else the
// maintainer's local store `~/.config/fauna/asc/` — exactly one
// `AuthKey_<key id>.p8` (the key id read off its name) and a one-line `issuer`
// file.
//
// Shared exit codes: 2 App Store Connect refused or failed (its status and body
// printed verbatim on stderr); 3 no credential on this box; 4 no app with that
// bundle id is visible to the key; 64 usage.

import CryptoKit
import Foundation

let apiBase = "https://api.appstoreconnect.apple.com"
let defaultBundleID = "social.fauna.fauna"

func fail(_ code: Int32, _ message: String) -> Never {
    FileHandle.standardError.write((message + "\n").data(using: .utf8)!)
    exit(code)
}

struct Credential {
    let pem: String
    let keyID: String
    let issuer: String
    let source: String
}

/// The key from `<prefix>_KEY_FILE` / `<prefix>_KEY_ID` / `<prefix>_ISSUER`
/// when the file variable is set; else, when `localStore` allows it, the
/// maintainer's local store. The readers take the notarization key
/// (`FAUNA_NOTARY`); the store leg takes `FAUNA_ASC_STORE` with no local store,
/// since its key never rests on a dev machine.
func loadCredential(prefix: String = "FAUNA_NOTARY", localStore: Bool = true) -> Credential {
    let env = ProcessInfo.processInfo.environment
    let fileVar = "\(prefix)_KEY_FILE", idVar = "\(prefix)_KEY_ID", issuerVar = "\(prefix)_ISSUER"
    if let file = env[fileVar], !file.isEmpty {
        guard let keyID = env[idVar], !keyID.isEmpty,
              let issuer = env[issuerVar], !issuer.isEmpty
        else { fail(3, "no credential: \(fileVar) is set without \(idVar) and \(issuerVar)") }
        guard let pem = try? String(contentsOfFile: file, encoding: .utf8)
        else { fail(3, "no credential: \(fileVar) not readable: \(file)") }
        return Credential(pem: pem, keyID: keyID, issuer: issuer, source: "environment")
    }
    guard localStore else { fail(3, "no credential: \(fileVar) is unset (this key is read from the environment only)") }
    let store = FileManager.default.homeDirectoryForCurrentUser
        .appendingPathComponent(".config/fauna/asc")
    let names = (try? FileManager.default.contentsOfDirectory(atPath: store.path)) ?? []
    let keys = names.filter { $0.hasPrefix("AuthKey_") && $0.hasSuffix(".p8") }.sorted()
    guard keys.count == 1 else {
        fail(3, "no credential: the local store \(store.path) holds \(keys.count) AuthKey_<id>.p8 files (needs exactly one), and FAUNA_NOTARY_KEY_FILE is unset")
    }
    let keyFile = keys[0]
    let keyID = String(keyFile.dropFirst("AuthKey_".count).dropLast(".p8".count))
    let issuerPath = store.appendingPathComponent("issuer").path
    guard let issuerRaw = try? String(contentsOfFile: issuerPath, encoding: .utf8) else {
        fail(3, "no credential: \(issuerPath) is missing (the key's issuer id, one line — App Store Connect → Users and Access → Integrations)")
    }
    let issuer = issuerRaw.trimmingCharacters(in: .whitespacesAndNewlines)
    guard let pem = try? String(contentsOfFile: store.appendingPathComponent(keyFile).path, encoding: .utf8) else {
        fail(3, "no credential: \(keyFile) in \(store.path) is not readable")
    }
    return Credential(pem: pem, keyID: keyID, issuer: issuer, source: "local store")
}

func base64url(_ data: Data) -> String {
    data.base64EncodedString()
        .replacingOccurrences(of: "+", with: "-")
        .replacingOccurrences(of: "/", with: "_")
        .replacingOccurrences(of: "=", with: "")
}

func token(_ cred: Credential) -> String {
    let signer: P256.Signing.PrivateKey
    do {
        signer = try P256.Signing.PrivateKey(pemRepresentation: cred.pem)
    } catch {
        fail(3, "no credential: the key (\(cred.source)) is not a P-256 PKCS#8 PEM: \(error)")
    }
    let now = Int(Date().timeIntervalSince1970)
    let header: [String: Any] = ["alg": "ES256", "kid": cred.keyID, "typ": "JWT"]
    // App Store Connect accepts at most 20 minutes of validity.
    let claims: [String: Any] = ["iss": cred.issuer, "iat": now, "exp": now + 1200, "aud": "appstoreconnect-v1"]
    let h = base64url(try! JSONSerialization.data(withJSONObject: header, options: [.sortedKeys]))
    let c = base64url(try! JSONSerialization.data(withJSONObject: claims, options: [.sortedKeys]))
    let input = "\(h).\(c)"
    let signature = try! signer.signature(for: Data(input.utf8))
    // JWS ES256 is the raw r||s pair, which is CryptoKit's rawRepresentation.
    return "\(input).\(base64url(signature.rawRepresentation))"
}

/// One GET's outcome: a JSON object on HTTP 200, else the status and the body
/// verbatim. A transport failure (no answer at all) exits 2 at once.
enum Reply {
    case ok([String: Any])
    case refused(Int, String)
}

func request(_ path: String, _ bearer: String) -> Reply {
    guard let url = URL(string: apiBase + path) else { fail(2, "bad request path: \(path)") }
    var req = URLRequest(url: url)
    req.httpMethod = "GET"
    req.setValue("Bearer \(bearer)", forHTTPHeaderField: "Authorization")
    req.timeoutInterval = 60
    var result: (Data?, URLResponse?, Error?) = (nil, nil, nil)
    let done = DispatchSemaphore(value: 0)
    URLSession.shared.dataTask(with: req) { d, r, e in
        result = (d, r, e)
        done.signal()
    }.resume()
    done.wait()
    if let error = result.2 { fail(2, "GET \(path): request failed: \(error.localizedDescription)") }
    let status = (result.1 as? HTTPURLResponse)?.statusCode ?? 0
    let body = result.0 ?? Data()
    guard status == 200 else { return .refused(status, String(decoding: body, as: UTF8.self)) }
    guard let object = (try? JSONSerialization.jsonObject(with: body)) as? [String: Any] else {
        fail(2, "GET \(path): HTTP 200 with a body that is not a JSON object")
    }
    return .ok(object)
}

/// A GET whose refusal ends the read (exit 2, the reply verbatim).
func get(_ path: String, _ bearer: String) -> [String: Any] {
    switch request(path, bearer) {
    case .ok(let object): return object
    case .refused(let status, let body): fail(2, "GET \(path): HTTP \(status)\n\(body)")
    }
}

func query(_ value: String) -> String {
    value.addingPercentEncoding(withAllowedCharacters: .alphanumerics.union(CharacterSet(charactersIn: "-._~"))) ?? value
}

/// The app id App Store Connect gives `bundleID`, or exit 4.
func appID(bundleID: String, keyID: String, fetch: (String) -> [String: Any]) -> String {
    let apps = fetch("/v1/apps?filter%5BbundleId%5D=\(query(bundleID))&fields%5Bapps%5D=bundleId,name")
    let rows = (apps["data"] as? [[String: Any]]) ?? []
    guard let app = rows.first(where: { (($0["attributes"] as? [String: Any])?["bundleId"] as? String) == bundleID }),
          let id = app["id"] as? String
    else { fail(4, "no app with bundle id \(bundleID) is visible to key \(keyID)") }
    return id
}

func printJSON(_ object: [String: Any]) {
    let data = try! JSONSerialization.data(withJSONObject: object, options: [.prettyPrinted, .sortedKeys])
    print(String(decoding: data, as: UTF8.self))
}
