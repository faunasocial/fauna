import Foundation

/// The one door to the app's own `UserDefaults` — every Fauna-owned preference
/// (start at login, the dock icon, photo backup on/off, the store-consent record)
/// reads and writes through `AppDefaults.store`, never `UserDefaults.standard`.
///
/// Production: `.standard`, the app's own domain, exactly as before.
///
/// Under e2e (`FaunaE2E.isActive`, a constant `false` in a Release build —
/// convention 15): a plist FILE inside the launch's own Library
/// (`<Library>/Preferences/social.fauna.e2e.plist`), read once and rewritten
/// atomically on every write, with no CFPreferences in the path. The macOS
/// driver isolates a launch by relocating `CFFIXED_USER_HOME`, which moves
/// Application Support but not preferences: `cfprefsd` keeps the bare binary's
/// `.standard` domain in the real profile's `~/Library/Preferences/FaunaMacOS.plist`
/// (measured 2026-11-04 — one run's start-at-login OFF read back in every later
/// "fresh" launch on the box, convention 10, `e2e-launch-isolation.md`), and a
/// path-based suite under `CFFIXED_USER_HOME` is no better (measured the same
/// day: one write never reached disk, another read back stale in the next
/// launch). `.libraryDirectory` does follow `CFFIXED_USER_HOME`, so the file is
/// fresh per launch, kept across a `preserve_state_across_relaunch()` relaunch
/// (same home), and never shared with a concurrent run. On iOS the library is
/// the app container, which the driver already isolates; the same rule holds.
///
/// AppKit's own autosaves (window frames, split-view positions) still go to
/// `.standard`; they are cosmetic and outside this door.
public enum AppDefaults {
    public static let store: UserDefaults = {
        guard FaunaE2E.isActive, let url = e2eFileURL() else { return .standard }
        return FileDefaults(url: url)
    }()

    /// The e2e store's plist under the launch's Library, creating its
    /// `Preferences` directory; `nil` when the Library cannot be resolved.
    static func e2eFileURL(fileManager: FileManager = .default) -> URL? {
        guard let library = fileManager.urls(for: .libraryDirectory, in: .userDomainMask).first
        else { return nil }
        let prefs = library.appendingPathComponent("Preferences", isDirectory: true)
        try? fileManager.createDirectory(at: prefs, withIntermediateDirectories: true)
        return prefs.appendingPathComponent("social.fauna.e2e.plist")
    }
}

/// A `UserDefaults` whose whole state is one plist file this process owns:
/// loaded at init, rewritten atomically after every write, so a force-quit right
/// after a write (the e2e relaunch path) never loses it. Overrides every
/// accessor the app uses, so nothing falls through to the CFPreferences
/// storage behind `super`.
final class FileDefaults: UserDefaults, @unchecked Sendable {
    private let url: URL
    private let lock = NSLock()
    private var values: [String: Any]

    init(url: URL) {
        self.url = url
        if let data = try? Data(contentsOf: url),
           let plist = try? PropertyListSerialization.propertyList(from: data, format: nil),
           let dict = plist as? [String: Any] {
            values = dict
        } else {
            values = [:]
        }
        super.init(suiteName: nil)!
    }

    private func write(_ mutate: (inout [String: Any]) -> Void) {
        lock.lock(); defer { lock.unlock() }
        mutate(&values)
        if let data = try? PropertyListSerialization.data(
            fromPropertyList: values, format: .binary, options: 0) {
            try? data.write(to: url, options: .atomic)
        }
    }

    override func object(forKey defaultName: String) -> Any? {
        lock.lock(); defer { lock.unlock() }
        return values[defaultName]
    }
    override func string(forKey defaultName: String) -> String? {
        object(forKey: defaultName) as? String
    }
    override func array(forKey defaultName: String) -> [Any]? {
        object(forKey: defaultName) as? [Any]
    }
    override func dictionary(forKey defaultName: String) -> [String: Any]? {
        object(forKey: defaultName) as? [String: Any]
    }
    override func data(forKey defaultName: String) -> Data? {
        object(forKey: defaultName) as? Data
    }
    override func bool(forKey defaultName: String) -> Bool {
        (object(forKey: defaultName) as? NSNumber)?.boolValue ?? false
    }
    override func integer(forKey defaultName: String) -> Int {
        (object(forKey: defaultName) as? NSNumber)?.intValue ?? 0
    }
    override func double(forKey defaultName: String) -> Double {
        (object(forKey: defaultName) as? NSNumber)?.doubleValue ?? 0
    }
    override func float(forKey defaultName: String) -> Float {
        (object(forKey: defaultName) as? NSNumber)?.floatValue ?? 0
    }
    override func url(forKey defaultName: String) -> URL? {
        (object(forKey: defaultName) as? String).flatMap(URL.init(string:))
    }
    override func dictionaryRepresentation() -> [String: Any] {
        lock.lock(); defer { lock.unlock() }
        return values
    }

    override func set(_ value: Any?, forKey defaultName: String) {
        write { $0[defaultName] = value }
    }
    override func set(_ value: Bool, forKey defaultName: String) {
        write { $0[defaultName] = value }
    }
    override func set(_ value: Int, forKey defaultName: String) {
        write { $0[defaultName] = value }
    }
    override func set(_ value: Double, forKey defaultName: String) {
        write { $0[defaultName] = value }
    }
    override func set(_ value: Float, forKey defaultName: String) {
        write { $0[defaultName] = value }
    }
    override func set(_ url: URL?, forKey defaultName: String) {
        write { $0[defaultName] = url?.absoluteString }
    }
    override func removeObject(forKey defaultName: String) {
        write { $0[defaultName] = nil }
    }
    override func synchronize() -> Bool { true }
}
