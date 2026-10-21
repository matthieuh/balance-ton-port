import Foundation
import UserNotifications

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

/// The menu panel's width, its padding, and the indent of a row's lines 2
/// and 3 (past the status dot). A row's third line must fit in
/// `rowTextWidth` at the callout font (tested).
let menuWidth: CGFloat = 480
let menuPadding: CGFloat = 12
let rowIndent: CGFloat = 16
var rowTextWidth: CGFloat { menuWidth - 2 * menuPadding - rowIndent }

/// `:8081`. Always the bare digits: a port is not a quantity, so no
/// locale grouping (SwiftUI's `Text("\(port)")` would show `8 081`).
func portLabel(_ port: UInt16) -> String {
    ":" + String(port)
}

/// Session names longer than this are cut with an ellipsis in the menu.
let sessionNameMaxLength = 20

/// `name`, or its first characters plus "\u{2026}" when longer than
/// `sessionNameMaxLength` (the result is never longer than that).
func truncatedSessionName(_ name: String) -> String {
    guard name.count > sessionNameMaxLength else { return name }
    return String(name.prefix(sessionNameMaxLength - 1)) + "\u{2026}"
}

/// The row's third line. Alive session:
/// `Claude session "revva-b8" · active · started 4 min ago`; due an alert
/// that could not be shown (`notificationsOff`):
/// `Claude session "revva-b8" · idle 31 min, notifications off`; dead
/// session: `Session ended · idle 12 min · kill in 8 min` (`kill soon` under
/// a minute).
func sessionLine(_ s: ServerViewFfi, notificationsOff: Bool = false) -> String {
    var parts: [String]
    if s.sessionAlive {
        let name = "Claude session \"\(truncatedSessionName(s.sessionName))\""
        let idle = "idle \(durationLabel(ms: s.idleMs))"
        if notificationsOff {
            parts = [name, "\(idle), notifications off"]
        } else {
            parts = [name, s.state == "idle" ? idle : "active", s.startedLabel]
        }
    } else {
        parts = ["Session ended", "idle \(durationLabel(ms: s.idleMs))"]
    }
    if let killIn = s.killInMs {
        parts.append(killIn < 60_000 ? "kill soon" : "kill in \(durationLabel(ms: killIn))")
    }
    return parts.joined(separator: " \u{00B7} ")
}

/// The row tooltip: full command line, pid and full worktree path.
func rowHelp(_ s: ServerViewFfi) -> String {
    "\(s.command)\npid \(s.pid)\n\(s.worktree)"
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

/// "Astro on port 4322 (revva \u{203A} web) hasn't been used for 30 min."
/// Only the tool label, the port and the location: the raw command line
/// (flags, tokens) never reaches the lock screen. Minutes are rounded down.
func notificationBody(_ s: ServerViewFfi) -> String {
    let minutes = s.idleMs / 60_000
    return "\(s.toolLabel) on port \(String(s.port)) (\(s.locationLabel)) hasn't been used for \(minutes) min."
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

// MARK: - Notification permission

/// What macOS lets the app do with notifications, as the Settings section
/// and the menu show it.
enum NotificationPermission: Equatable, Sendable {
    /// The system has not asked the user yet.
    case notAsked
    /// Allowed; `bannersOff` when the alert style is None, so nothing pops up.
    case allowed(bannersOff: Bool)
    /// Refused (or the prompt was closed): only System Settings can fix it.
    case denied

    init(status: UNAuthorizationStatus, alertStyle: UNAlertStyle) {
        switch status {
        case .notDetermined:
            self = .notAsked
        case .authorized, .provisional, .ephemeral:
            self = .allowed(bannersOff: alertStyle == .none)
        case .denied:
            self = .denied
        @unknown default:
            self = .denied
        }
    }

    var isAllowed: Bool {
        if case .allowed = self { return true }
        return false
    }
}

/// The one button the Notifications section shows under its text.
enum NotificationSetupButton: Equatable, Sendable {
    case turnOn
    case openSettings

    var title: String {
        switch self {
        case .turnOn: return "Turn on notifications"
        case .openSettings: return "Open Notification Settings"
        }
    }
}

/// The Notifications section's content for a permission.
struct NotificationSetup: Equatable, Sendable {
    /// Main line; shown with a green check when `isOn`.
    let message: String
    let isOn: Bool
    /// A second, secondary line (what to do in System Settings).
    let hint: String?
    let button: NotificationSetupButton?
    /// "Send a test notification" is offered.
    let canSendTest: Bool
}

func notificationSetup(_ permission: NotificationPermission) -> NotificationSetup {
    switch permission {
    case .notAsked:
        return NotificationSetup(
            message: "Get a notification when a server sits idle, with Kill and Keep buttons.",
            isOn: false, hint: nil, button: .turnOn, canSendTest: false
        )
    case .allowed(let bannersOff):
        return NotificationSetup(
            message: "Notifications are on",
            isOn: true,
            hint: bannersOff ? "Banners are off: choose Banners or Alerts in System Settings" : nil,
            button: bannersOff ? .openSettings : nil,
            canSendTest: true
        )
    case .denied:
        return NotificationSetup(
            message: "Notifications are off for Balance ton port.",
            isOn: false,
            hint: "Turn on Allow notifications, then come back here.",
            button: .openSettings,
            canSendTest: false
        )
    }
}

/// What "Turn on" in the menu does; `nil` when the menu shows no line.
enum MenuNotificationFix: Equatable, Sendable {
    /// Ask macOS right away (it has never asked).
    case request
    /// Open the Settings window, whose Notifications section is on top.
    case openSettings
}

func menuNotificationFix(_ permission: NotificationPermission?) -> MenuNotificationFix? {
    switch permission {
    case .notAsked: return .request
    case .denied: return .openSettings
    case .allowed, nil: return nil
    }
}

/// The app's pane in System Settings, Notifications, then the Notifications
/// pane itself as a fallback.
func notificationSettingsURLs(bundleID: String) -> [URL] {
    let base = "x-apple.systempreferences:com.apple.Notifications-Settings.extension"
    return [URL(string: "\(base)?id=\(bundleID)"), URL(string: base)].compactMap { $0 }
}

/// The test notification's body: a made-up idle server.
let sampleNotificationBody =
    "Test: Vite on port 5173 (my-project \u{203A} web) hasn't been used for 30 min."

/// The test notification's userInfo: no pid nor start time, so
/// `serverID(fromUserInfo:)` finds no server and its Kill and Keep do nothing.
func sampleNotificationUserInfo() -> [String: Any] {
    ["sample": true]
}

func isSampleNotification(_ info: [AnyHashable: Any]) -> Bool {
    info["sample"] as? Bool == true
}

/// What the last "Send a test notification" did.
enum TestNotificationResult: Equatable, Sendable {
    case none
    /// Notification Center accepted it.
    case sent
    /// Notification Center refused it.
    case failed

    init(added: Bool) {
        self = added ? .sent : .failed
    }
}

/// The test button's title: "Test sent" only once the notification was added.
func testNotificationButtonTitle(_ result: TestNotificationResult) -> String {
    switch result {
    case .none: return "Send a test notification"
    case .sent: return "Test sent"
    case .failed: return "Could not send the test"
    }
}

/// Seconds before the test button's title goes back to "Send a test notification".
let testNotificationResetDelay: TimeInterval = 4

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
    var excludePorts: [UInt16]

    init(_ c: ConfigFfi) {
        deadSessionIdleS = c.deadSessionIdleS
        alertAfterS = c.alertAfterS
        killGraceS = c.killGraceS
        scanIntervalS = c.scanIntervalS
        claudeConfigDirs = c.claudeConfigDirs
        excludeCommands = c.excludeCommands
        excludePorts = c.excludePorts
    }

    var ffi: ConfigFfi {
        ConfigFfi(
            deadSessionIdleS: deadSessionIdleS,
            alertAfterS: alertAfterS,
            killGraceS: killGraceS,
            scanIntervalS: scanIntervalS,
            claudeConfigDirs: claudeConfigDirs,
            excludeCommands: excludeCommands,
            excludePorts: excludePorts
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

/// Decides when the Settings form saves: only when the draft differs from
/// what was last loaded or saved, so loading (or reloading on reopen) never
/// writes the file; only a user edit does.
struct SettingsSaveGate: Sendable {
    private var synced: SettingsDraft?

    /// The draft just read from disk.
    mutating func loaded(_ draft: SettingsDraft) {
        synced = draft
    }

    /// True when `draft` is a valid edit to save; it then counts as synced.
    mutating func shouldSave(_ draft: SettingsDraft) -> Bool {
        guard let synced, draft != synced, draft.isValid else { return false }
        self.synced = draft
        return true
    }
}

/// `list` with `entry` appended, trimmed. `nil` when the entry is empty or
/// already in the list.
func addingEntry(_ entry: String, to list: [String]) -> [String]? {
    let value = entry.trimmingCharacters(in: .whitespacesAndNewlines)
    guard !value.isEmpty, !list.contains(value) else { return nil }
    return list + [value]
}

/// `ports` with the port typed in `entry` appended. `nil` when the entry is
/// not a port number (1 to 65535) or is already in the list.
func addingPort(_ entry: String, to ports: [UInt16]) -> [UInt16]? {
    let text = entry.trimmingCharacters(in: .whitespacesAndNewlines)
    guard let port = UInt16(text), port > 0, !ports.contains(port) else { return nil }
    return ports + [port]
}
