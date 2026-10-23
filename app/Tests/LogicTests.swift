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
    state: String = "active",
    agent: String = "claude_code",
    agentLabel: String = "Claude"
) -> ServerViewFfi {
    ServerViewFfi(
        pid: 4242, startTime: 1_700_000_000_123, port: port, ports: [port], command: command,
        toolLabel: toolLabel, worktree: "/Users/me/trees/\(worktree)", worktreeName: worktree,
        locationLabel: "revva \u{203A} \(worktree)", sessionName: sessionName,
        agent: agent, agentLabel: agentLabel,
        startedMs: 1_700_000_000, startedLabel: "started 4 min ago",
        sessionAlive: sessionAlive, idleMs: idleMs, killInMs: killInMs, state: state,
        managed: true, excluded: false, framework: nil, title: toolLabel, hasIcon: false,
        branch: nil, rssBytes: 0, cpuPct: 0, growthBytes: 0, health: "green", inUse: false,
        statusText: "idle 0 min", card: nil, canRestart: false, postponed: false,
        uptimeMs: 0, processCount: 1
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
        claudeConfigDirs: ["/Users/me/.claude"], excludeCommands: ["mcp"], excludePorts: [5432],
        leakMemoryMb: 4096, leakGrowthMb: 300
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

    func testSessionLineNamesTheAgent() {
        let s = server(sessionName: "web", agent: "codex", agentLabel: "Codex")
        XCTAssertEqual(
            sessionLine(s),
            "Codex session \"web\" \u{00B7} active \u{00B7} started 4 min ago"
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
        XCTAssertEqual(setup.message, "Get a notification for servers that sit idle and for sessions that end with servers still running, with buttons to stop or keep them.")
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
        let s = menuServer(port: 3010, ports: [3010, 9229], branch: "roadbook-explore")
        let menuCopy = [
            nothingToDecide, moreLabel(2), recentlyStoppedLabel(1), headerCount(7), headerCount(1),
            sectionTitle("Needs you", count: 2), sectionTitle("All good", count: 5), rowSubtitle(s),
            agentBadge(s).text, rowStatus(s, notificationsOff: true),
        ] + ["restart", "postpone", "stop"].map { cardActionLabel(CardFfi(kind: "leak", reason: "", action: $0)) }
        for text in menuCopy {
            XCTAssertFalse(text.contains("\u{2014}"), text)
        }
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
    private let changed = "Nothing to kill: this server changed since the notification"

    func testNotificationKillThatFindsNothingSaysTheServerChanged() {
        let same = ServerID(pid: 4242, startTime: 2)
        for tracked in [[], [same], [ServerID(pid: 7, startTime: 1)]] {
            XCTAssertEqual(missedKillNotice(id, tracked: tracked, fromNotification: true), changed)
        }
    }

    func testMenuKillSaysGoneOnlyWhenNoServerHasThePid() {
        let other = ServerID(pid: 7, startTime: 1)
        XCTAssertEqual(
            missedKillNotice(id, tracked: [other], fromNotification: false),
            "That server was already gone."
        )
        let same = ServerID(pid: 4242, startTime: 2)
        XCTAssertNil(missedKillNotice(id, tracked: [same], fromNotification: false))
    }
}

final class SessionEndedNotificationTests: XCTestCase {
    private let group = SessionEndedID(sessionPid: 50, sessionProcStart: "Thu Oct 22 10:00:00 2026", atMs: 1_800_000_000_123)

    private func info(_ dict: [String: Any]) -> [AnyHashable: Any] {
        dict.reduce(into: [:]) { $0[$1.key] = $1.value }
    }

    func testUserInfoRoundTrip() {
        XCTAssertEqual(sessionEndedID(fromUserInfo: info(sessionEndedUserInfo(group))), group)
    }

    func testUserInfoMissingFields() {
        XCTAssertNil(sessionEndedID(fromUserInfo: [:]))
        var partial = sessionEndedUserInfo(group)
        partial["atMs"] = "nope"
        XCTAssertNil(sessionEndedID(fromUserInfo: info(partial)))
        partial = sessionEndedUserInfo(group)
        partial["sessionProcStart"] = ""
        XCTAssertNil(sessionEndedID(fromUserInfo: info(partial)))
        // A server alert's userInfo is not a group.
        XCTAssertNil(sessionEndedID(fromUserInfo: info(notificationUserInfo(ServerID(pid: 1, startTime: 2)))))
    }

    func testRequestIDNamesTheProcessAndTheEnd() {
        XCTAssertEqual(sessionEndedRequestID(group), "session-ended-50-1800000000123")
    }

    func testFfiGroupID() {
        let ffi = SessionEndedFfi(
            sessionPid: 50, sessionProcStart: "Thu Oct 22 10:00:00 2026", atMs: 1_800_000_000_123,
            sessionName: "web", servers: [], body: "b"
        )
        XCTAssertEqual(ffi.id, group)
    }

    func testRoutesGroupButtons() {
        let i = info(sessionEndedUserInfo(group))
        XCTAssertEqual(notificationRoute(actionIdentifier: NotificationIDs.stopNow, userInfo: i), .sessionEnded(.stopNow, group))
        XCTAssertEqual(notificationRoute(actionIdentifier: NotificationIDs.keep, userInfo: i), .sessionEnded(.keep, group))
        // Kill is not a group button; a body tap opens nothing.
        XCTAssertNil(notificationRoute(actionIdentifier: NotificationIDs.kill, userInfo: i))
        XCTAssertNil(notificationRoute(actionIdentifier: UNNotificationDefaultActionIdentifier, userInfo: i))
    }

    func testRoutesServerButtons() {
        let id = ServerID(pid: 7, startTime: 9)
        let i = info(notificationUserInfo(id))
        XCTAssertEqual(notificationRoute(actionIdentifier: NotificationIDs.kill, userInfo: i), .server(.kill, id))
        XCTAssertEqual(notificationRoute(actionIdentifier: NotificationIDs.keep, userInfo: i), .server(.keep, id))
        XCTAssertNil(notificationRoute(actionIdentifier: NotificationIDs.stopNow, userInfo: i))
        XCTAssertNil(notificationRoute(actionIdentifier: UNNotificationDismissActionIdentifier, userInfo: i))
    }

    func testSampleAndUnknownRouteNowhere() {
        let sample = info(sampleNotificationUserInfo())
        XCTAssertNil(notificationRoute(actionIdentifier: NotificationIDs.kill, userInfo: sample))
        XCTAssertNil(notificationRoute(actionIdentifier: NotificationIDs.keep, userInfo: [:]))
    }
}

private func menuServer(
    port: UInt16 = 3000,
    ports: [UInt16]? = nil,
    managed: Bool = true,
    agent: String = "claude_code",
    branch: String? = nil,
    worktree: String = "web",
    card: String? = nil,
    action: String = "stop",
    rssMb: UInt64 = 100,
    health: String = "green",
    title: String = "Bun dev",
    framework: String? = nil
) -> ServerViewFfi {
    var s = server(port: port, worktree: worktree, agent: agent)
    s.pid = Int32(port)
    s.ports = ports ?? [port]
    s.managed = managed
    s.branch = branch
    s.card = card.map { CardFfi(kind: $0, reason: "why", action: action) }
    s.rssBytes = rssMb * 1_048_576
    s.health = health
    s.title = title
    s.framework = framework
    return s
}

final class MenuLayoutTests: XCTestCase {
    func testSectionsOrderCardsAndFoldAllGood() {
        let servers = [
            menuServer(port: 8081),
            menuServer(port: 3000, managed: false, agent: ""),
            menuServer(port: 3010, card: "about_to_stop", action: "postpone"),
            menuServer(port: 4400),
            menuServer(port: 5173, agent: "codex", card: "leak", action: "restart"),
            menuServer(port: 1234, managed: false, agent: ""),
            menuServer(port: 9000),
        ]
        let m = menuSections(servers)
        XCTAssertEqual(m.needsYou.map(\.port), [5173, 3010])
        XCTAssertEqual(m.allGood.map(\.port), [4400, 8081, 9000], "agent servers first, by port")
        XCTAssertEqual(m.hiddenCount, 2)
        XCTAssertEqual(menuSections(servers, visibleAllGood: 10).allGood.map(\.port), [4400, 8081, 9000, 1234, 3000])
    }

    func testCardsOrderByKindThenMemory() {
        let servers = [
            menuServer(port: 1, card: "idle_alert"),
            menuServer(port: 2, card: "about_to_stop", rssMb: 10),
            menuServer(port: 3, card: "about_to_stop", rssMb: 900),
            menuServer(port: 4, card: "session_ended_open"),
            menuServer(port: 5, card: "leak"),
        ]
        XCTAssertEqual(menuSections(servers).needsYou.map(\.port), [5, 4, 3, 2, 1])
    }

    func testNothingToDecideWhenNoCard() {
        let m = menuSections([menuServer(port: 1), menuServer(port: 2)])
        XCTAssertTrue(m.needsYou.isEmpty)
        XCTAssertEqual(m.allGood.count, 2)
        XCTAssertEqual(m.hiddenCount, 0)
        XCTAssertEqual(nothingToDecide, "Nothing to decide")
    }

    func testPortColorIsStable() {
        XCTAssertEqual(portHue(3010), portHue(3010))
        XCTAssertEqual(portHue(3010), (3010 * 0.618_033_988_75).truncatingRemainder(dividingBy: 1), accuracy: 1e-12)
        XCTAssertEqual(portColor(3010), portColor(3010))
        XCTAssertNotEqual(portColor(3000), portColor(3001))
        for port: UInt16 in [0, 1, 80, 3000, 65535] {
            XCTAssertTrue((0..<1).contains(portHue(port)), "\(port)")
        }
    }

    func testAgentBadges() {
        let claude = agentBadge(menuServer())
        XCTAssertEqual(claude.text, "\u{2733} Claude")
        XCTAssertEqual(claude.color, .orange)
        let codex = agentBadge(menuServer(agent: "codex"))
        XCTAssertEqual(codex.text, "\u{25CE} Codex")
        XCTAssertEqual(codex.color, .blue)
        let none = agentBadge(menuServer(managed: false, agent: ""))
        XCTAssertEqual(none.text, "no agent")
        XCTAssertNil(none.color)
    }

    func testRowSubtitle() {
        let s = menuServer(port: 3010, ports: [3010, 9229], branch: "roadbook-explore")
        XCTAssertEqual(rowSubtitle(s), ":3010 +9229 \u{00B7} roadbook-explore \u{00B7} \u{2733} Claude")
        XCTAssertEqual(
            rowSubtitle(menuServer(port: 3000, agent: "", worktree: "client-portal")),
            ":3000 \u{00B7} client-portal \u{00B7} no agent",
            "the project when the branch is unknown"
        )
        XCTAssertEqual(rowSubtitle(menuServer(port: 8081, worktree: "")), ":8081 \u{00B7} \u{2733} Claude")
    }

    func testTitlePartsDropARepeatedFramework() {
        XCTAssertEqual(titleParts(menuServer(title: "Revva", framework: "Expo")).framework, "Expo")
        XCTAssertNil(titleParts(menuServer(title: "Vite", framework: "Vite")).framework)
        XCTAssertNil(titleParts(menuServer(title: "serve")).framework)
    }

    func testSectionTitles() {
        XCTAssertEqual(sectionTitle("Needs you", count: 2), "NEEDS YOU \u{00B7} 2")
        XCTAssertEqual(sectionTitle("All good", count: 5), "ALL GOOD \u{00B7} 5")
        XCTAssertEqual(moreLabel(2), "+ 2 more \u{25B8}")
        XCTAssertEqual(recentlyStoppedLabel(1), "Recently stopped (1)")
    }

    func testHeaderCount() {
        XCTAssertEqual(headerCount(7), "7 servers")
        XCTAssertEqual(headerCount(1), "1 server")
        XCTAssertEqual(headerCount(0), "0 servers")
    }

    func testCardActionLabels() {
        let label = { (action: String) in cardActionLabel(CardFfi(kind: "x", reason: "", action: action)) }
        XCTAssertEqual(label("restart"), "Restart")
        XCTAssertEqual(label("postpone"), "Postpone 1 h")
        XCTAssertEqual(label("stop"), "Stop")
        let aboutToStop = CardFfi(kind: "about_to_stop", reason: "Session ended, unused", action: "postpone")
        XCTAssertEqual(cardActionLabel(aboutToStop), "Postpone 1 h")
        XCTAssertFalse(cardActionIsProminent(aboutToStop))
        XCTAssertTrue(cardActionIsProminent(CardFfi(kind: "leak", reason: "", action: "restart")))
    }

    func testConcentricRadius() {
        XCTAssertEqual(concentricRadius(outer: 16, padding: 4), 12)
        XCTAssertEqual(concentricRadius(outer: 16, padding: 6), 10)
        XCTAssertEqual(concentricRadius(outer: 6, padding: 4), 4, "never under 4")
        XCTAssertEqual(concentricRadius(outer: 4, padding: 10), 4)
        XCTAssertTrue((26...28).contains(actionButtonHeight))
        XCTAssertEqual(pressScale, 0.96)
    }

    func testHealthAndSizeColors() {
        XCTAssertEqual(healthColor("green"), .green)
        XCTAssertEqual(healthColor("amber"), .orange)
        XCTAssertEqual(healthColor("red"), .red)
        XCTAssertNil(sizeColor("green"))
        XCTAssertEqual(sizeColor("red"), .red)
    }

    func testMemoryLabelsAndGauge() {
        XCTAssertEqual(memoryLabel(90 * 1_048_576), "90 MB")
        XCTAssertEqual(memoryLabel(2355 * 1_048_576), "2.3 GB")
        XCTAssertEqual(gaugeFraction(0), 0.03)
        XCTAssertEqual(gaugeFraction(1024 * 1_048_576), 0.5)
        XCTAssertEqual(gaugeFraction(5 * 1024 * 1_048_576), 1)
    }

    func testUsageSegmentsSplitDevByHealth() {
        let gb: UInt64 = 1024 * 1_048_576
        let m = MemoryBarFfi(
            devBytes: 3 * gb, appsBytes: 8 * gb, freeBytes: 5 * gb, totalBytes: 16 * gb, line: "", freeLow: false
        )
        let servers = [
            menuServer(port: 1, rssMb: 1024, health: "green"),
            menuServer(port: 2, rssMb: 2048, health: "red"),
        ]
        XCTAssertEqual(usageSegments(m, servers: servers), UsageSegments(green: 1 / 16, amber: 0, red: 2 / 16, apps: 0.5))
        let empty = MemoryBarFfi(devBytes: 0, appsBytes: 0, freeBytes: 0, totalBytes: 0, line: "", freeLow: false)
        XCTAssertEqual(usageSegments(empty, servers: servers), UsageSegments(green: 0, amber: 0, red: 0, apps: 0))
        XCTAssertEqual(usageHelp(m), "Dev servers 3072 MB\nOther apps 8192 MB\nFree 5120 MB of 16384 MB")
    }

    func testUsageCaptionCutsTheCoreLine() {
        XCTAssertEqual(
            usageCaption("4.8 GB dev servers (13%) \u{00B7} apps 26.8 \u{00B7} free 4.4 of 36 GB"),
            UsageCaption(figure: "4.8 GB", dev: "dev servers (13%)", apps: "apps 26.8", free: "free 4.4 of 36 GB")
        )
        XCTAssertNil(usageCaption(""))
    }

    func testIconTiles() {
        XCTAssertEqual(iconTile(menuServer(framework: "Vite")).letter, "V")
        XCTAssertEqual(iconTile(menuServer(framework: "wrangler")).symbol, "cloud.fill")
        let plain = iconTile(menuServer(title: "serve"))
        XCTAssertEqual(plain.letter, "S")
        XCTAssertNil(plain.background)
        XCTAssertEqual(iconTile(menuServer(title: "@scope/app")).letter, "S")
    }
}
