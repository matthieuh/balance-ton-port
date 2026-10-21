import AppKit
import Foundation
import UserNotifications
import XCTest

private func server(
    command: String = "bun run dev",
    toolLabel: String = "Bun dev",
    port: UInt16 = 4322,
    worktree: String = "web",
    sessionName: String = "revva-b8",
    sessionAlive: Bool = true,
    idleMs: UInt64 = 0,
    killInMs: UInt64? = nil,
    state: String = "active"
) -> ServerViewFfi {
    ServerViewFfi(
        pid: 4242, startTime: 1_700_000_000_123, port: port, command: command,
        toolLabel: toolLabel, worktree: "/Users/me/trees/\(worktree)", worktreeName: worktree,
        locationLabel: "revva \u{203A} \(worktree)", sessionName: sessionName,
        startedMs: 1_700_000_000, startedLabel: "started 4 min ago",
        sessionAlive: sessionAlive, idleMs: idleMs, killInMs: killInMs, state: state
    )
}

final class NotificationTextTests: XCTestCase {
    func testBodyRoundsMinutesDown() {
        let s = server(command: "astro dev", toolLabel: "Astro", idleMs: 30 * 60_000 + 59_999)
        XCTAssertEqual(
            notificationBody(s),
            "Astro on port 4322 (revva \u{203A} web) hasn't been used for 30 min."
        )
    }

    func testBodyUnderOneMinute() {
        XCTAssertEqual(
            notificationBody(server(idleMs: 59_000)),
            "Bun dev on port 4322 (revva \u{203A} web) hasn't been used for 0 min."
        )
    }

    func testBodyNeverCarriesTheCommandLine() {
        let s = server(command: "node server.js --token=abc123", toolLabel: "Node server.js", port: 8081)
        let body = notificationBody(s)
        XCTAssertFalse(body.contains("abc123"), body)
        XCTAssertTrue(body.hasPrefix("Node server.js on port 8081 "), body)
    }

    func testUserInfoRoundTrip() {
        let id = ServerID(pid: 4242, startTime: UInt64.max - 1)
        XCTAssertEqual(serverID(fromUserInfo: notificationUserInfo(id)), id)
    }

    func testUserInfoMissingFields() {
        XCTAssertNil(serverID(fromUserInfo: [:]))
        XCTAssertNil(serverID(fromUserInfo: ["pid": 1, "startTime": "nope"]))
    }
}

final class SettingsDraftTests: XCTestCase {
    private let config = ConfigFfi(
        deadSessionIdleS: 1200, alertAfterS: 1800, killGraceS: 5, scanIntervalS: 5,
        claudeConfigDirs: ["/Users/me/.claude"], excludeCommands: ["mcp"], excludePorts: [5432]
    )

    func testConversions() {
        XCTAssertEqual(minutes(fromSeconds: 0), 0)
        XCTAssertEqual(minutes(fromSeconds: 119), 1)
        XCTAssertEqual(minutes(fromSeconds: 1800), 30)
        XCTAssertEqual(seconds(fromMinutes: 30), 1800)
        XCTAssertEqual(seconds(fromMinutes: -3), 0)
    }

    func testDraftShowsHumanUnits() {
        let d = SettingsDraft(config)
        XCTAssertEqual(d.deadSessionIdleMin, 20)
        XCTAssertEqual(d.alertAfterMin, 30)
        XCTAssertEqual(d.killGraceSec, 5)
        XCTAssertEqual(d.scanIntervalSec, 5)
        XCTAssertEqual(d.ffi, config)
    }

    func testEditingOneDelayKeepsTheOthersExact() {
        var c = config
        c.deadSessionIdleS = 90 // not a whole number of minutes
        var d = SettingsDraft(c)
        d.alertAfterMin = 1
        XCTAssertEqual(d.ffi.alertAfterS, 60)
        XCTAssertEqual(d.ffi.deadSessionIdleS, 90)
    }

    func testValidation() {
        XCTAssertTrue(SettingsDraft(config).isValid)
        var d = SettingsDraft(config)
        d.deadSessionIdleS = 59
        d.alertAfterS = 0
        d.killGraceS = 0
        d.scanIntervalS = 0
        XCTAssertEqual(d.validationErrors.count, 4)
        d.deadSessionIdleMin = 1
        d.alertAfterMin = 1
        d.killGraceSec = 1
        d.scanIntervalSec = 1
        XCTAssertTrue(d.isValid)
    }

    func testSaveGateIgnoresLoadsAndReloads() {
        var gate = SettingsSaveGate()
        let loaded = SettingsDraft(config)
        XCTAssertFalse(gate.shouldSave(loaded), "nothing loaded yet")
        gate.loaded(loaded)
        XCTAssertFalse(gate.shouldSave(loaded), "the load itself is not an edit")

        var edited = loaded
        edited.alertAfterMin = 1
        XCTAssertTrue(gate.shouldSave(edited))
        XCTAssertFalse(gate.shouldSave(edited), "already saved")

        // Reopen: the form reloads what is on disk; that is not a save.
        gate.loaded(edited)
        XCTAssertFalse(gate.shouldSave(edited))

        var invalid = edited
        invalid.scanIntervalS = 0
        XCTAssertFalse(gate.shouldSave(invalid), "invalid drafts never save")
    }

    func testAddingPort() {
        XCTAssertEqual(addingPort(" 6379 ", to: [5432]), [5432, 6379])
        XCTAssertNil(addingPort("5432", to: [5432]))
        XCTAssertNil(addingPort("0", to: []))
        XCTAssertNil(addingPort("70000", to: []))
        XCTAssertNil(addingPort("redis", to: []))
        XCTAssertEqual(SettingsDraft(config).ffi.excludePorts, [5432])
    }

    func testAddingEntry() {
        XCTAssertEqual(addingEntry("  redis-server ", to: ["mcp"]), ["mcp", "redis-server"])
        XCTAssertNil(addingEntry("   ", to: ["mcp"]))
        XCTAssertNil(addingEntry("mcp", to: ["mcp"]))
    }
}

final class MenuLabelTests: XCTestCase {
    func testStateLabel() {
        XCTAssertEqual(stateLabel(server()), "active")
        XCTAssertEqual(stateLabel(server(idleMs: 12 * 60_000, state: "idle")), "idle 12 min")
        XCTAssertEqual(
            stateLabel(server(killInMs: 18 * 60_000, state: "session dead")),
            "session dead \u{00B7} kill in 18 min"
        )
    }

    func testSessionNameTruncation() {
        XCTAssertEqual(truncatedSessionName("revva-b8"), "revva-b8")
        let exact = String(repeating: "a", count: 20)
        XCTAssertEqual(truncatedSessionName(exact), exact)
        let long = "tec-6435-us58-endpoints-publics-create-u-a1"
        let cut = truncatedSessionName(long)
        XCTAssertEqual(cut, "tec-6435-us58-endpo\u{2026}")
        XCTAssertEqual(cut.count, 20)
    }

    func testSessionLineAliveActive() {
        XCTAssertEqual(
            sessionLine(server()),
            "Claude session \"revva-b8\" \u{00B7} active \u{00B7} started 4 min ago"
        )
    }

    func testSessionLineAliveIdleTruncatesTheName() {
        let s = server(
            sessionName: "web-itinerairer-public-cb4fa7a5-ff",
            idleMs: 12 * 60_000, state: "idle"
        )
        XCTAssertEqual(
            sessionLine(s),
            "Claude session \"web-itinerairer-pub\u{2026}\" \u{00B7} idle 12 min \u{00B7} started 4 min ago"
        )
    }

    func testSessionLineWhenTheAlertCouldNotBeShown() {
        let s = server(idleMs: 31 * 60_000, state: "idle")
        XCTAssertEqual(
            sessionLine(s, notificationsOff: true),
            "Claude session \"revva-b8\" \u{00B7} idle 31 min, notifications off"
        )
    }

    /// The longest third lines fit the menu at the callout font (12 pt), so
    /// "started 2 d ago" is never cut.
    func testLongestSessionLinesFitTheMenu() {
        let font = NSFont.preferredFont(forTextStyle: .callout)
        let name = "web-itinerairer-public-cb4fa7a5"
        let lines = [
            sessionLine(server(sessionName: name, idleMs: 59 * 60_000, state: "idle")),
            sessionLine(server(sessionName: name, idleMs: 59 * 60_000, state: "idle"), notificationsOff: true),
            sessionLine(server(sessionName: name, killInMs: 0)),
        ].map { $0.replacingOccurrences(of: "started 4 min ago", with: "started 59 min ago") }
        for line in lines {
            let width = (line as NSString).size(withAttributes: [.font: font]).width
            XCTAssertLessThanOrEqual(width, rowTextWidth, line)
        }
    }

    func testPortLabelIsVerbatim() {
        XCTAssertEqual(portLabel(8081), ":8081")
        XCTAssertEqual(portLabel(65535), ":65535")
    }

    func testSessionLineAliveWithWorktreeGone() {
        let s = server(killInMs: 0)
        XCTAssertEqual(
            sessionLine(s),
            "Claude session \"revva-b8\" \u{00B7} active \u{00B7} started 4 min ago \u{00B7} kill soon"
        )
    }

    func testSessionLineDead() {
        let dead = server(sessionAlive: false, idleMs: 12 * 60_000, killInMs: 8 * 60_000, state: "session dead")
        XCTAssertEqual(sessionLine(dead), "Session ended \u{00B7} idle 12 min \u{00B7} kill in 8 min")
        let soon = server(sessionAlive: false, idleMs: 20 * 60_000, killInMs: 59_999, state: "session dead")
        XCTAssertEqual(sessionLine(soon), "Session ended \u{00B7} idle 20 min \u{00B7} kill soon")
        // Shared with a live session: no kill is scheduled.
        let shared = server(sessionAlive: false, idleMs: 30_000, state: "session dead")
        XCTAssertEqual(sessionLine(shared), "Session ended \u{00B7} idle <1 min")
    }

    func testRowHelp() {
        XCTAssertEqual(
            rowHelp(server(command: "bun run dev")),
            "bun run dev\npid 4242\n/Users/me/trees/web"
        )
    }

    func testRelativeTime() {
        let now = Date(timeIntervalSince1970: 1_000_000)
        let nowMs: UInt64 = 1_000_000_000
        XCTAssertEqual(relativeTime(atMs: nowMs, now: now), "just now")
        XCTAssertEqual(relativeTime(atMs: nowMs + 5_000, now: now), "just now")
        XCTAssertEqual(relativeTime(atMs: nowMs - 12 * 60_000, now: now), "12 min ago")
        XCTAssertEqual(relativeTime(atMs: nowMs - 2 * 3_600_000, now: now), "2 h ago")
        XCTAssertEqual(relativeTime(atMs: nowMs - 25 * 3_600_000, now: now), "yesterday")
        XCTAssertEqual(relativeTime(atMs: nowMs - 3 * 86_400_000, now: now), "3 days ago")
    }
}

/// The Settings save path (draft, `ffi`, `Core.setConfig`) against a temp
/// data dir, read back by a fresh `Core` and by `btp config get`.
final class ConfigSaveTests: XCTestCase {
    private var dir: URL!
    private var previous: String?

    override func setUpWithError() throws {
        dir = FileManager.default.temporaryDirectory
            .appendingPathComponent("btp-settings-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        previous = ProcessInfo.processInfo.environment["BTP_DATA_DIR"]
        setenv("BTP_DATA_DIR", dir.path, 1)
    }

    override func tearDownWithError() throws {
        if let previous { setenv("BTP_DATA_DIR", previous, 1) } else { unsetenv("BTP_DATA_DIR") }
        try? FileManager.default.removeItem(at: dir)
    }

    func testDelayChangeIsSavedAndVisibleToCli() throws {
        let core = Core()
        let discovered = core.discoveredClaudeDirs()
        var draft = SettingsDraft(try core.config())
        XCTAssertTrue(draft.claudeConfigDirs.isEmpty, "extras only, discovered dirs stay out")
        draft.alertAfterMin = 1
        draft.excludeCommands = addingEntry("vite", to: draft.excludeCommands) ?? draft.excludeCommands
        try core.setConfig(c: draft.ffi)

        let reread = try Core().config()
        XCTAssertEqual(reread.alertAfterS, 60)
        XCTAssertTrue(reread.excludeCommands.contains("vite"))

        // app/Tests/LogicTests.swift -> repo root
        let root = URL(fileURLWithPath: #filePath)
            .deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent()
        let candidates = ["target/debug/btp", "target/release/btp"].map { root.appendingPathComponent($0) }
        guard let btp = candidates.first(where: { FileManager.default.isExecutableFile(atPath: $0.path) }) else {
            throw XCTSkip("btp not built (cargo build -p btp-cli)")
        }
        let process = Process()
        process.executableURL = btp
        process.arguments = ["config", "get"]
        var env = ProcessInfo.processInfo.environment
        env["BTP_DATA_DIR"] = dir.path
        process.environment = env
        let pipe = Pipe()
        process.standardOutput = pipe
        try process.run()
        let output = String(decoding: pipe.fileHandleForReading.readDataToEndOfFile(), as: UTF8.self)
        process.waitUntilExit()
        XCTAssertEqual(process.terminationStatus, 0)
        XCTAssertTrue(output.contains("alert_after_s = 60"), output)
        XCTAssertTrue(output.contains("\"vite\""), output)
        XCTAssertTrue(output.contains("claude_config_dirs = []"), output)
        let saved = try String(contentsOf: dir.appendingPathComponent("config.toml"), encoding: .utf8)
        for d in discovered {
            XCTAssertFalse(saved.contains(d), "discovered dir \(d) written to config.toml")
        }
    }
}

final class NotificationSetupTests: XCTestCase {
    func testPermissionFromStatus() {
        XCTAssertEqual(NotificationPermission(status: .notDetermined, alertStyle: .none), .notAsked)
        XCTAssertEqual(NotificationPermission(status: .denied, alertStyle: .banner), .denied)
        XCTAssertEqual(NotificationPermission(status: .authorized, alertStyle: .banner), .allowed(bannersOff: false))
        XCTAssertEqual(NotificationPermission(status: .authorized, alertStyle: .alert), .allowed(bannersOff: false))
        XCTAssertEqual(NotificationPermission(status: .provisional, alertStyle: .none), .allowed(bannersOff: true))
        XCTAssertTrue(NotificationPermission.allowed(bannersOff: true).isAllowed)
        XCTAssertFalse(NotificationPermission.denied.isAllowed)
        XCTAssertFalse(NotificationPermission.notAsked.isAllowed)
    }

    func testNotAskedOffersToTurnOn() {
        let setup = notificationSetup(.notAsked)
        XCTAssertEqual(setup.message, "Get a notification when a server sits idle, with Kill and Keep buttons.")
        XCTAssertFalse(setup.isOn)
        XCTAssertNil(setup.hint)
        XCTAssertEqual(setup.button, .turnOn)
        XCTAssertEqual(setup.button?.title, "Turn on notifications")
        XCTAssertFalse(setup.canSendTest)
    }

    func testAllowedShowsCheckAndTest() {
        let setup = notificationSetup(.allowed(bannersOff: false))
        XCTAssertEqual(setup.message, "Notifications are on")
        XCTAssertTrue(setup.isOn)
        XCTAssertNil(setup.hint)
        XCTAssertNil(setup.button)
        XCTAssertTrue(setup.canSendTest)
    }

    func testAllowedWithBannersOffPointsToSettings() {
        let setup = notificationSetup(.allowed(bannersOff: true))
        XCTAssertTrue(setup.isOn)
        XCTAssertEqual(setup.hint, "Banners are off: choose Banners or Alerts in System Settings")
        XCTAssertEqual(setup.button, .openSettings)
        XCTAssertTrue(setup.canSendTest)
    }

    func testDeniedPointsToSettings() {
        let setup = notificationSetup(.denied)
        XCTAssertEqual(setup.message, "Notifications are off for Balance ton port.")
        XCTAssertFalse(setup.isOn)
        XCTAssertEqual(setup.hint, "Turn on Allow notifications, then come back here.")
        XCTAssertEqual(setup.button, .openSettings)
        XCTAssertEqual(setup.button?.title, "Open Notification Settings")
        XCTAssertFalse(setup.canSendTest)
    }

    func testMenuFix() {
        XCTAssertEqual(menuNotificationFix(.notAsked), .request)
        XCTAssertEqual(menuNotificationFix(.denied), .openSettings)
        XCTAssertNil(menuNotificationFix(.allowed(bannersOff: false)))
        XCTAssertNil(menuNotificationFix(.allowed(bannersOff: true)))
        XCTAssertNil(menuNotificationFix(nil))
    }

    func testSettingsURLs() {
        let urls = notificationSettingsURLs(bundleID: "com.matthieuh.balancetonport").map(\.absoluteString)
        XCTAssertEqual(urls, [
            "x-apple.systempreferences:com.apple.Notifications-Settings.extension?id=com.matthieuh.balancetonport",
            "x-apple.systempreferences:com.apple.Notifications-Settings.extension",
        ])
    }

    func testSampleNotificationNamesNoServer() {
        let info: [AnyHashable: Any] = sampleNotificationUserInfo().reduce(into: [:]) { $0[$1.key] = $1.value }
        XCTAssertTrue(isSampleNotification(info))
        XCTAssertNil(serverID(fromUserInfo: info))
        let real: [AnyHashable: Any] = notificationUserInfo(ServerID(pid: 1, startTime: 2))
            .reduce(into: [:]) { $0[$1.key] = $1.value }
        XCTAssertFalse(isSampleNotification(real))
    }

    func testCopyHasNoEmDash() {
        let permissions: [NotificationPermission] = [.notAsked, .allowed(bannersOff: false), .allowed(bannersOff: true), .denied]
        for p in permissions {
            let setup = notificationSetup(p)
            for text in [setup.message, setup.hint ?? "", setup.button?.title ?? ""] {
                XCTAssertFalse(text.contains("\u{2014}"), text)
            }
        }
        XCTAssertFalse(sampleNotificationBody.contains("\u{2014}"))
    }
}

final class TestNotificationButtonTests: XCTestCase {
    func testTitleSaysSentOnlyOnSuccess() {
        XCTAssertEqual(testNotificationButtonTitle(.none), "Send a test notification")
        XCTAssertEqual(testNotificationButtonTitle(.sent), "Test sent")
        XCTAssertEqual(testNotificationButtonTitle(.failed), "Could not send the test")
    }

    func testResultFollowsTheAdd() {
        XCTAssertEqual(TestNotificationResult(added: true), .sent)
        XCTAssertEqual(TestNotificationResult(added: false), .failed)
    }

    func testLabelResetsAfterAFewSeconds() {
        XCTAssertGreaterThanOrEqual(testNotificationResetDelay, 2)
        XCTAssertLessThanOrEqual(testNotificationResetDelay, 10)
    }
}

final class MissedKillTests: XCTestCase {
    private let id = ServerID(pid: 4242, startTime: 1)

    func testGoneWhenNoServerHasThePid() {
        let other = ServerID(pid: 7, startTime: 1)
        XCTAssertEqual(missedKillNotice(id, tracked: [other]), "That server was already gone.")
        XCTAssertEqual(missedKillNotice(id, tracked: []), "That server was already gone.")
    }

    func testNoNoticeWhileAServerWithThePidIsTracked() {
        // Same pid, another identity (for example the key changed since the
        // notification): the row is still there, nothing is "gone".
        let same = ServerID(pid: 4242, startTime: 2)
        XCTAssertNil(missedKillNotice(id, tracked: [same]))
    }
}
