import Foundation

// Pure helpers: no UI, no notification center, no core queue. Compiled into
// both the app and BalanceTonPortTests.

/// A server's identity: a pid alone can be reused by a later process.
struct ServerID: Hashable, Sendable {
    let pid: Int32
    let startTime: UInt64
}

extension ServerViewFfi {
    var id: ServerID { ServerID(pid: pid, startTime: startTime) }
}

// MARK: - Menu labels

/// `active`, `idle 12 min`, or `session dead · kill in 18 min`.
func stateLabel(_ s: ServerViewFfi) -> String {
    if let killIn = s.killInMs {
        return "session dead \u{00B7} kill in \(durationLabel(ms: killIn))"
    }
    if s.state == "idle" {
        return "idle \(durationLabel(ms: s.idleMs))"
    }
    return s.state
}

/// `just now`, `12 min ago`, `2 h ago`, `yesterday`, `3 days ago`.
func relativeTime(atMs: UInt64, now: Date = Date()) -> String {
    let nowMs = UInt64(max(0, now.timeIntervalSince1970 * 1000))
    let min = nowMs > atMs ? (nowMs - atMs) / 60_000 : 0
    switch min {
    case 0: return "just now"
    case 1..<60: return "\(min) min ago"
    case 60..<(24 * 60): return "\(min / 60) h ago"
    case (24 * 60)..<(48 * 60): return "yesterday"
    default: return "\(min / (24 * 60)) days ago"
    }
}

// MARK: - Notifications

/// Commands longer than this are shortened in notifications.
let shortCommandMaxLength = 40

/// A command line short enough for a notification. Kept as is up to
/// `shortCommandMaxLength` characters; otherwise every path-like word is cut
/// to its basename (`/x/.bun/bin/bun /repo/node_modules/.bin/astro dev`
/// becomes `bun astro dev`), and if that is still too long, only the first
/// word plus the next one remain (`bun astro`).
func shortCommand(_ command: String) -> String {
    let trimmed = command.trimmingCharacters(in: .whitespaces)
    if trimmed.count <= shortCommandMaxLength { return trimmed }
    let words = trimmed.split(separator: " ", omittingEmptySubsequences: true).map { word -> String in
        let w = String(word)
        guard w.contains("/") else { return w }
        let base = (w as NSString).lastPathComponent
        return base.isEmpty ? w : base
    }
    let basenamed = words.joined(separator: " ")
    if basenamed.count <= shortCommandMaxLength { return basenamed }
    return words.prefix(2).joined(separator: " ")
}

/// "`astro dev` on port 4322 (worktree web) hasn't been used for 30 min."
/// Minutes are rounded down.
func notificationBody(_ s: ServerViewFfi) -> String {
    let minutes = s.idleMs / 60_000
    return "`\(shortCommand(s.command))` on port \(s.port) (worktree \(s.worktreeName)) hasn't been used for \(minutes) min."
}

/// What a notification carries to find its server again. The start time is a
/// string: property lists have no unsigned 64-bit integer.
func notificationUserInfo(_ id: ServerID) -> [String: Any] {
    ["pid": Int(id.pid), "startTime": String(id.startTime)]
}

func serverID(fromUserInfo info: [AnyHashable: Any]) -> ServerID? {
    guard let pid = (info["pid"] as? NSNumber)?.int32Value,
          let startText = info["startTime"] as? String,
          let start = UInt64(startText)
    else { return nil }
    return ServerID(pid: pid, startTime: start)
}

// MARK: - Settings

/// Whole minutes in `seconds`, rounded down.
func minutes(fromSeconds seconds: UInt64) -> Int {
    Int(clamping: seconds / 60)
}

func seconds(fromMinutes minutes: Int) -> UInt64 {
    UInt64(max(0, minutes)) * 60
}

/// The Settings form's working copy of the config. Delays are kept in
/// seconds, so a delay the form does not touch is saved back unchanged even
/// when it is not a whole number of minutes.
struct SettingsDraft: Equatable, Sendable {
    var deadSessionIdleS: UInt64
    var alertAfterS: UInt64
    var killGraceS: UInt64
    var scanIntervalS: UInt64
    var claudeConfigDirs: [String]
    var excludeCommands: [String]

    init(_ c: ConfigFfi) {
        deadSessionIdleS = c.deadSessionIdleS
        alertAfterS = c.alertAfterS
        killGraceS = c.killGraceS
        scanIntervalS = c.scanIntervalS
        claudeConfigDirs = c.claudeConfigDirs
        excludeCommands = c.excludeCommands
    }

    var ffi: ConfigFfi {
        ConfigFfi(
            deadSessionIdleS: deadSessionIdleS,
            alertAfterS: alertAfterS,
            killGraceS: killGraceS,
            scanIntervalS: scanIntervalS,
            claudeConfigDirs: claudeConfigDirs,
            excludeCommands: excludeCommands
        )
    }

    var deadSessionIdleMin: Int {
        get { minutes(fromSeconds: deadSessionIdleS) }
        set { deadSessionIdleS = seconds(fromMinutes: newValue) }
    }

    var alertAfterMin: Int {
        get { minutes(fromSeconds: alertAfterS) }
        set { alertAfterS = seconds(fromMinutes: newValue) }
    }

    var killGraceSec: Int {
        get { Int(clamping: killGraceS) }
        set { killGraceS = UInt64(max(0, newValue)) }
    }

    var scanIntervalSec: Int {
        get { Int(clamping: scanIntervalS) }
        set { scanIntervalS = UInt64(max(0, newValue)) }
    }

    /// Empty when the draft can be saved: minute delays at least 1 min,
    /// second delays at least 1 s.
    var validationErrors: [String] {
        var errors: [String] = []
        if deadSessionIdleS < 60 { errors.append("Dead session delay must be at least 1 min.") }
        if alertAfterS < 60 { errors.append("Alert delay must be at least 1 min.") }
        if killGraceS < 1 { errors.append("Kill grace must be at least 1 s.") }
        if scanIntervalS < 1 { errors.append("Scan interval must be at least 1 s.") }
        return errors
    }

    var isValid: Bool { validationErrors.isEmpty }
}

/// `list` with `entry` appended, trimmed. `nil` when the entry is empty or
/// already in the list.
func addingEntry(_ entry: String, to list: [String]) -> [String]? {
    let value = entry.trimmingCharacters(in: .whitespacesAndNewlines)
    guard !value.isEmpty, !list.contains(value) else { return nil }
    return list + [value]
}
