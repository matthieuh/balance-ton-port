import AppKit
import Foundation
import SwiftUI
import UserNotifications
import WebKit
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
        startedMs: 1_700_000_000, startedLabel: "started 4 min ago", startedAgeMs: 240_000,
        sessionAlive: sessionAlive, idleMs: idleMs, killInMs: killInMs, state: state,
        managed: true, excluded: false, framework: nil, title: toolLabel, hasIcon: false,
        branch: nil, rssBytes: 0, cpuPct: 0, growthBytes: 0, health: "green", inUse: false,
        statusText: "idle 0 min", status: StatusFfi(kind: "idle", minutes: 0, agentLabel: "", reason: nil), card: nil, leakKind: nil, canRestart: false, postponed: false,
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
            sessionName: "web", servers: [], body: "b", deadIdleS: 1800
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

    func testExtraPortsShowOneThenACount() {
        let many = menuServer(port: 3022, ports: [3022, 3030, 5433, 6379, 35432, 41000], worktree: "")
        XCTAssertEqual(subtitleParts(many).otherPorts, "+3030 +4")
        XCTAssertEqual(rowSubtitle(many), ":3022 +3030 +4 \u{00B7} \u{2733} Claude")
        XCTAssertEqual(subtitleParts(menuServer(port: 1, ports: [1, 2])).otherPorts, "+2")
        XCTAssertNil(subtitleParts(menuServer(port: 1)).otherPorts)
    }

    func testEqualCardsAndRowsKeepAStableOrder() {
        let servers = [
            menuServer(port: 3003, card: "idle_alert", rssMb: 50),
            menuServer(port: 3000, card: "idle_alert", rssMb: 50),
            menuServer(port: 3001, card: "idle_alert", rssMb: 50),
        ]
        XCTAssertEqual(menuSections(servers).needsYou.map(\.port), [3000, 3001, 3003])
        XCTAssertEqual(menuSections(servers.reversed()).needsYou.map(\.port), [3000, 3001, 3003])
    }

    func testSectionsMaxHeightLeavesRoomForHeaderAndFooter() {
        XCTAssertEqual(sectionsMaxHeight(screenHeight: 1000), 780)
        XCTAssertEqual(sectionsMaxHeight(screenHeight: 300), 200)
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
        XCTAssertEqual(needsYouColor(dark: true), Color(hex: 0xE9B764))
        XCTAssertNotEqual(needsYouColor(dark: false), needsYouColor(dark: true))
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
        XCTAssertEqual(iconTile(menuServer(framework: "Vite")).background?.count, 2, "the Vite gradient")
        let serve = iconTile(menuServer(title: "serve", framework: "static"))
        XCTAssertEqual(serve.letter, "S")
        XCTAssertNil(serve.background, "a letter on the neutral tile")
        XCTAssertNil(serve.symbol)
        let plain = iconTile(menuServer(title: "serve"))
        XCTAssertEqual(plain.letter, "S")
        XCTAssertNil(plain.background)
        XCTAssertEqual(iconTile(menuServer(title: "@scope/app")).letter, "S")
    }
}

final class MenuActionTests: XCTestCase {
    func testConfirmationOnlyForObserved() {
        XCTAssertFalse(needsConfirmation(menuServer()), "an agent server stops without asking")
        XCTAssertTrue(needsConfirmation(menuServer(managed: false, agent: "")))
    }

    func testConfirmationCopy() {
        let s = menuServer(port: 3000, managed: false, agent: "", title: "client-portal")
        let stop = confirmationText(s, action: .stop)
        XCTAssertEqual(stop.title, "Stop client-portal :3000?")
        XCTAssertEqual(stop.message, "No agent started it. Balance ton port never stops it on its own.")
        XCTAssertEqual(stop.button, "Stop")
        let restart = confirmationText(menuServer(port: 10_443, managed: false, agent: "", title: "api"), action: .restart)
        XCTAssertEqual(restart.title, "Restart api :10443?", "no digit grouping")
        XCTAssertEqual(restart.message, stop.message)
        XCTAssertEqual(restart.button, "Restart")
    }

    func testLeakNotificationBody() {
        var big = menuServer(port: 3010, title: "Revva web")
        big.rssBytes = UInt64(2.3 * 1024 * 1_048_576)
        big.growthBytes = 600 * 1_048_576
        big.card = CardFfi(kind: "leak", reason: "Using 2.3 GB", action: "restart")
        big.leakKind = "size"
        XCTAssertEqual(leakNotificationBody(big), "Revva web :3010 is using 2.3 GB", "size, even when it grew")
        var growing = menuServer(port: 5173, title: "Vite")
        growing.growthBytes = 600 * 1_048_576
        growing.card = CardFfi(kind: "leak", reason: "Growing fast: +600 MB in 10 min", action: "restart")
        growing.leakKind = "growth"
        XCTAssertEqual(leakNotificationBody(growing), "Vite :5173 grew 600 MB in 10 min")
        var notALeak = growing
        notALeak.card = CardFfi(kind: "idle_alert", reason: "Growing fast", action: "stop")
        notALeak.leakKind = nil
        XCTAssertEqual(leakNotificationBody(notALeak), "Vite :5173 is using 100 MB", "the kind, never a reason")
    }

    func testMoreMenuItemsOrderAndDisabledStates() {
        var s = menuServer(card: "idle_alert")
        s.canRestart = true
        let items = moreMenuItems(s)
        XCTAssertEqual(
            items.map(\.title),
            ["\u{2197} Open in browser", "\u{27F3} Restart", "\u{23F8} Keep (no alerts)", "\u{2298} Ignore this alert",
             "\u{24D8} Details\u{2026}", "", "\u{25A0} Stop"]
        )
        XCTAssertEqual(items.map(\.kind), [.open, .restart, .keep, .ignoreAlert, .details, .separator, .stop])
        XCTAssertEqual(items.filter(\.enabled).map(\.kind), [.open, .restart, .keep, .ignoreAlert, .details, .stop])
        XCTAssertFalse(moreMenuItems(s, inDetail: true).first { $0.kind == .details }!.enabled, "not from the detail")

        func enabled(_ s: ServerViewFfi) -> [MenuItemKind] { moreMenuItems(s).filter(\.enabled).map(\.kind) }
        XCTAssertEqual(enabled(menuServer()), [.open, .keep, .details, .stop], "no Restart when it cannot, no Ignore without a card")
        XCTAssertEqual(enabled(menuServer(managed: false, agent: "")), [.open, .details, .stop], "no Keep for an observed server")
        var excluded = menuServer(managed: false, agent: "")
        excluded.excluded = true
        XCTAssertEqual(enabled(excluded), [.open, .details], "no Stop for an excluded server")
        XCTAssertEqual(enabled(menuServer(card: "leak", action: "restart")), [.open, .keep, .ignoreAlert, .details, .stop])
        XCTAssertEqual(
            enabled(menuServer(card: "about_to_stop", action: "postpone")), [.open, .keep, .details, .stop],
            "an about-to-stop card is not an alert"
        )
        XCTAssertEqual(
            moreMenuItems(s, restarting: true).filter(\.enabled).map(\.kind), [.open, .keep, .ignoreAlert, .details],
            "no Restart nor Stop while a Restart runs"
        )
    }

    func testCapsuleHasNoRestart() {
        XCTAssertEqual(capsuleButtons(menuServer()), [.open, .stop, .more])
        var excluded = menuServer(managed: false, agent: "")
        excluded.excluded = true
        XCTAssertEqual(capsuleButtons(excluded), [.open, .more])
    }

    func testCardPillEnabled() {
        var s = menuServer(card: "leak", action: "restart")
        XCTAssertFalse(cardActionEnabled(s.card!, s), "no Restart when it cannot")
        s.canRestart = true
        XCTAssertTrue(cardActionEnabled(s.card!, s))
        let observed = menuServer(managed: false, agent: "", card: "session_ended_open", action: "stop")
        XCTAssertTrue(cardActionEnabled(observed.card!, observed), "Stop with a confirmation")
        let postpone = menuServer(card: "about_to_stop", action: "postpone")
        XCTAssertTrue(cardActionEnabled(postpone.card!, postpone))
        XCTAssertFalse(cardActionEnabled(s.card!, s, restarting: true))
        XCTAssertFalse(cardActionEnabled(observed.card!, observed, restarting: true))
        XCTAssertTrue(canStopNow(observed, restarting: false))
        XCTAssertFalse(canStopNow(observed, restarting: true))
    }

    func testOpenURL() {
        XCTAssertEqual(openURL(menuServer(port: 10_443)).absoluteString, "http://localhost:10443/")
    }

    func testOptimisticViews() {
        let soon = menuServer(card: "about_to_stop", action: "postpone")
        let p = postponedView(soon)
        XCTAssertNil(p.card)
        XCTAssertEqual(p.statusText, "stops in 60 min")
        XCTAssertEqual(p.killInMs, 3_600_000)
        XCTAssertTrue(p.postponed)
        XCTAssertNotNil(postponedView(menuServer(card: "leak", action: "restart")).card, "a leak stays")

        let k = keptView(menuServer(card: "idle_alert"))
        XCTAssertNil(k.card)
        XCTAssertEqual(k.statusText, "kept by you")
        XCTAssertNil(k.killInMs)
        XCTAssertNotNil(keptView(menuServer(card: "session_ended_open")).card)
        XCTAssertNotNil(keptView(menuServer(card: "leak")).card)

        XCTAssertNil(alertIgnoredView(menuServer(card: "leak")).card)
        XCTAssertNotNil(alertIgnoredView(menuServer(card: "about_to_stop")).card, "not an alert: unchanged")
    }

    func testRowNoteText() {
        XCTAssertEqual(rowNoteText(.restarting), "restarting\u{2026}")
        XCTAssertEqual(rowNoteText(.blocked("The port is still taken")), "The port is still taken")
        XCTAssertEqual(blockedNoteDuration, 4)
    }

    func testLeakRoutes() {
        let id = ServerID(pid: 7, startTime: 99)
        let i = notificationUserInfo(id) as [AnyHashable: Any]
        XCTAssertEqual(notificationRoute(actionIdentifier: NotificationIDs.restart, userInfo: i), .server(.restart, id))
        XCTAssertEqual(notificationRoute(actionIdentifier: NotificationIDs.ignore, userInfo: i), .server(.ignoreAlert, id))
        XCTAssertEqual(leakRequestID(id), "leak-7-99")
    }

    func testActionCopyHasNoEmDash() {
        let s = menuServer(managed: false, agent: "", card: "leak")
        var copy = moreMenuItems(s).map(\.title) + [leakNotificationBody(s), rowNoteText(.restarting)]
        for a in [ConfirmAction.stop, .restart] {
            let t = confirmationText(s, action: a)
            copy += [t.title, t.message, t.button]
        }
        for text in copy {
            XCTAssertFalse(text.contains("\u{2014}"), text)
        }
    }
}

private func detailServer(
    managed: Bool = true,
    agent: String = "claude_code",
    sessionAlive: Bool = false,
    idleMs: UInt64 = 12 * 60_000,
    killInMs: UInt64? = nil,
    inUse: Bool = false,
    card: String? = nil,
    action: String = "stop",
    reason: String = "why",
    statusText: String = "idle 12 min",
    state: String = "idle"
) -> ServerViewFfi {
    var s = menuServer(managed: managed, agent: agent, card: card, action: action)
    s.card = card.map { CardFfi(kind: $0, reason: reason, action: action) }
    s.sessionAlive = sessionAlive
    s.idleMs = idleMs
    s.killInMs = killInMs
    s.inUse = inUse
    s.statusText = statusText
    s.status = statusOf(statusText)
    s.state = state
    s.agentLabel = managed ? (agent == "codex" ? "Codex" : "Claude") : "no agent"
    return s
}

final class DetailTests: XCTestCase {
    func testStatusSentences() {
        let mb: UInt64 = 1_048_576
        XCTAssertEqual(
            statusSentence(detailServer(killInMs: 8 * 60_000, card: "about_to_stop", action: "postpone")),
            "Its Claude session ended and nothing has connected for 12 min."
        )
        XCTAssertEqual(statusSentence(detailServer(inUse: true, statusText: "in use")), "In use: a browser is connected.")
        var growth = detailServer(inUse: true, card: "leak", action: "restart", reason: "ignored")
        growth.leakKind = "growth"
        growth.growthBytes = Int64(600 * mb)
        XCTAssertEqual(statusSentence(growth), "Growing fast: +600 MB in 10 min.")
        var size = detailServer(card: "leak", action: "restart", reason: "Using 2.3 GB.")
        size.leakKind = "size"
        size.rssBytes = 2355 * mb
        XCTAssertEqual(statusSentence(size), "Using 2.3 GB.", "built from the size, never a double period")
        XCTAssertEqual(
            statusSentence(detailServer(sessionAlive: true, statusText: "kept by Claude: \"watching the build\"")),
            "Kept by Claude: \"watching the build\". No alerts."
        )
        XCTAssertEqual(statusSentence(detailServer(statusText: "kept by you")), "Kept by you. No alerts.")
        XCTAssertEqual(statusSentence(detailServer(statusText: "kept by Codex")), "Kept by Codex. No alerts.")
        XCTAssertEqual(
            statusSentence(detailServer(managed: false, agent: "")),
            "Not started by an agent. Balance ton port never stops it on its own."
        )
        XCTAssertEqual(
            statusSentence(detailServer(managed: false, agent: "", inUse: true, statusText: "in use")),
            "In use: a browser is connected."
        )
        XCTAssertEqual(
            statusSentence(detailServer(agent: "codex", sessionAlive: false)),
            "Its Codex session ended and nothing has connected for 12 min."
        )
        XCTAssertEqual(
            statusSentence(detailServer(sessionAlive: true, idleMs: 23 * 3_600_000, card: "idle_alert")),
            "Its Claude session has been idle 23 h. Nothing stops it while the session is open."
        )
        XCTAssertEqual(
            statusSentence(detailServer(sessionAlive: true, idleMs: 34 * 60_000, statusText: "idle 34 min")),
            "Its Claude session is open.", "no alert: no idle story"
        )
        XCTAssertEqual(
            statusSentence(detailServer(sessionAlive: true, idleMs: 0, statusText: "active", state: "active")),
            "Its Claude session is open."
        )
    }

    func testDownsampleKeepsSpikes() {
        let flat = (0..<1000).map { SampleFfi(atMs: UInt64($0) * 1000, rssBytes: 100, cpuPct: 0) }
        var spiky = flat
        spiky[537].rssBytes = 9000
        spiky[538].rssBytes = 1
        let d = downsample(spiky)
        XCTAssertLessThanOrEqual(d.count, 200)
        XCTAssertTrue(d.contains { $0.rssBytes == 9000 }, "the spike stays")
        XCTAssertTrue(d.contains { $0.rssBytes == 1 }, "the dip stays")
        XCTAssertEqual(d.map(\.atMs), d.map(\.atMs).sorted(), "time order")
        XCTAssertEqual(d.first?.atMs, 0)
        XCTAssertEqual(downsample(Array(flat.prefix(150))).count, 150, "short histories untouched")
    }

    func testPreviewUserScriptRemovesHintsAndServiceWorkers() {
        let js = previewUserScript(port: 3010)
        XCTAssertTrue(js.contains("x-dns-prefetch-control"))
        XCTAssertTrue(js.contains("preconnect|dns-prefetch|prefetch|preload|prerender|modulepreload"))
        XCTAssertTrue(js.contains("\"http://localhost:3010\", \"http://127.0.0.1:3010\""))
        XCTAssertTrue(js.contains("MutationObserver"))
        XCTAssertTrue(js.contains("serviceWorker"))
    }

    @MainActor
    func testPreviewRulesCompile() async throws {
        let dir = FileManager.default.temporaryDirectory.appendingPathComponent("btp-rules-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: dir) }
        let store = try XCTUnwrap(WKContentRuleListStore(url: dir))
        let list = try await store.compileContentRuleList(
            forIdentifier: previewRuleListID, encodedContentRuleList: previewContentRules(port: 3010)
        )
        XCTAssertEqual(list?.identifier, previewRuleListID)
    }

    func testStatusTitle() {
        XCTAssertEqual(statusTitle(detailServer(killInMs: 8 * 60_000)), "Stops in 8 min")
        XCTAssertEqual(statusTitle(detailServer(killInMs: 30_000)), "Stops soon")
        XCTAssertNil(statusTitle(detailServer()))
    }

    func testBottomButtonsPerCase() {
        func titles(_ s: ServerViewFfi) -> [String] { bottomButtons(s).map(\.title) }
        XCTAssertEqual(
            titles(detailServer(killInMs: 8 * 60_000, card: "about_to_stop", action: "postpone")),
            ["Postpone 1 h", "Stop now"]
        )
        XCTAssertEqual(titles(detailServer(inUse: true)), ["Open", "Stop"])
        XCTAssertEqual(titles(detailServer(inUse: true, card: "leak", action: "restart")), ["Restart", "Stop"])
        XCTAssertEqual(titles(detailServer()), ["Open", "Stop"])
        XCTAssertEqual(titles(detailServer(managed: false, agent: "")), ["Open", "Stop"])
    }

    func testDetailButtonsFollowTheMenuRules() {
        var leak = detailServer(card: "leak", action: "restart")
        XCTAssertFalse(detailButtonEnabled(.restart, leak, restarting: false), "no Restart when it cannot")
        leak.canRestart = true
        XCTAssertTrue(detailButtonEnabled(.restart, leak, restarting: false))
        XCTAssertFalse(detailButtonEnabled(.restart, leak, restarting: true))
        XCTAssertFalse(detailButtonEnabled(.stop, leak, restarting: true))
        var excluded = detailServer(managed: false, agent: "")
        excluded.excluded = true
        XCTAssertFalse(detailButtonEnabled(.stop, excluded, restarting: false))
        XCTAssertTrue(detailButtonEnabled(.open, excluded, restarting: true))
        XCTAssertTrue(detailButtonEnabled(.postpone, detailServer(), restarting: false))
        XCTAssertFalse(detailButtonEnabled(.postpone, detailServer(managed: false, agent: ""), restarting: false))
    }

    func testStatusCardAction() {
        XCTAssertEqual(statusCardAction(detailServer(card: "about_to_stop", action: "postpone")), .postpone)
        XCTAssertEqual(statusCardAction(detailServer(card: "leak", action: "restart")), .restart)
        XCTAssertNil(statusCardAction(detailServer(card: "idle_alert")))
        XCTAssertNil(statusCardAction(detailServer(card: "session_ended_open")))
        XCTAssertNil(statusCardAction(detailServer()))
        XCTAssertEqual(DetailButton.postpone.title, "Postpone 1 h")
    }

    func testCountdownFraction() {
        XCTAssertEqual(countdownFraction(killInMs: 8 * 60_000, totalMs: 60 * 60_000)!, 8.0 / 60.0, accuracy: 1e-9)
        XCTAssertNil(countdownFraction(killInMs: nil, totalMs: 60 * 60_000))
        XCTAssertNil(countdownFraction(killInMs: 1000, totalMs: 0))
        XCTAssertEqual(countdownFraction(killInMs: 2 * 3_600_000, totalMs: 3_600_000), 1)
        var s = detailServer(killInMs: 1000)
        XCTAssertEqual(countdownTotalMs(s, deadSessionIdleS: 1800), 1_800_000)
        s.postponed = true
        XCTAssertEqual(countdownTotalMs(s, deadSessionIdleS: 1800), 3_600_000, "Postpone 1 h restarts an hour")
    }

    func testUptimeChip() {
        XCTAssertEqual(uptimeChip(ms: 5 * 3_600_000 + 20 * 60_000), "up 5 h")
        XCTAssertEqual(uptimeChip(ms: 12 * 60_000), "up 12 min")
        XCTAssertEqual(uptimeChip(ms: 3 * 86_400_000), "up 3 d")
    }

    func testDetailLabels() {
        XCTAssertEqual(processesLabel(3), "3 processes \u{203A}")
        XCTAssertEqual(processesLabel(1), "1 process \u{203A}")
        XCTAssertEqual(cpuLabel(0.4), "CPU 0%")
        XCTAssertEqual(cpuLabel(12.6), "CPU 13%")
        var s = detailServer()
        s.sessionName = "web-itinerairer-public-cb4fa7a5"
        XCTAssertEqual(sessionChipText(s), "\u{2733} \"web-itinerairer-pub\u{2026}\" \u{2197}")
        s.agent = "codex"
        s.sessionName = "fix"
        XCTAssertEqual(sessionChipText(s), "\u{25CE} \"fix\" \u{2197}")
        var hero = menuServer(port: 3010, ports: [3010, 9229], title: "revva-server", framework: "wrangler")
        XCTAssertEqual(heroSubtitle(hero).kind, "wrangler")
        XCTAssertEqual(heroSubtitle(hero).port, ":3010")
        XCTAssertEqual(heroSubtitle(hero).otherPorts, "+9229")
        hero.framework = nil
        hero.toolLabel = "Bun dev"
        XCTAssertEqual(heroSubtitle(hero).kind, "Bun dev")
        hero.title = "Bun dev"
        XCTAssertNil(heroSubtitle(hero).kind, "not the title twice")
    }

    func testEventChartLabels() {
        XCTAssertEqual(eventChartLabel(EventFfi(atMs: 0, kind: "started", label: "started by Claude")), "\u{2733} started")
        XCTAssertEqual(eventChartLabel(EventFfi(atMs: 0, kind: "started", label: "started by Codex")), "\u{25CE} started")
        XCTAssertEqual(eventChartLabel(EventFfi(atMs: 0, kind: "started", label: "started")), "started")
        XCTAssertEqual(
            eventChartLabel(EventFfi(atMs: 0, kind: "browser_closed", label: "browser closed")), "browser closed"
        )
    }

    func testCrowdedEventLabelsKeepTheLatest() {
        XCTAssertEqual(labelledEvents(at: [0.06, 0.52, 0.73], minGap: 0.15), [0, 1, 2])
        XCTAssertEqual(labelledEvents(at: [0.50, 0.55, 0.60, 0.90], minGap: 0.15), [2, 3])
        XCTAssertEqual(labelledEvents(at: [0.30, 0.55, 0.60, 0.90], minGap: 0.15), [0, 2, 3])
        XCTAssertEqual(labelledEvents(at: [], minGap: 0.15), [])
    }

    func testChartScale() {
        let mb: UInt64 = 1_048_576
        let small = chartScale(maxBytes: 90 * mb, lineBytes: 2048 * mb)
        XCTAssertFalse(small.lineOnScale, "a 90 MB server is not flattened under a 2 GB line")
        XCTAssertEqual(small.upper, Double(90 * mb) * 1.25, accuracy: 1)
        let big = chartScale(maxBytes: 1500 * mb, lineBytes: 2048 * mb)
        XCTAssertTrue(big.lineOnScale)
        XCTAssertEqual(big.upper, Double(2048 * mb) * 1.1, accuracy: 1)
        let leak = chartScale(maxBytes: 3000 * mb, lineBytes: 2048 * mb)
        XCTAssertTrue(leak.lineOnScale)
        XCTAssertEqual(leak.upper, Double(3000 * mb) * 1.25, accuracy: 1)
        XCTAssertEqual(chartScale(maxBytes: 0, lineBytes: 2048 * mb).upper, Double(64 * mb), accuracy: 1)
        XCTAssertEqual(leakLineLabel(2048 * mb), "2 GB")
        XCTAssertEqual(leakLineLabel(3072 * mb), "3 GB")
        XCTAssertEqual(leakLineLabel(1536 * mb), "1.5 GB")
    }

    func testDetailHeightStaysOnScreen() {
        XCTAssertEqual(detailMaxHeight(screenHeight: 1000), 960)
        XCTAssertEqual(detailMaxHeight(screenHeight: 200), 300)
    }

    func testDominantColorsOfATwoColorImage() throws {
        // 10 x 10: 5 red columns, 3 blue, 2 transparent.
        let ctx = try XCTUnwrap(CGContext(
            data: nil, width: 10, height: 10, bitsPerComponent: 8, bytesPerRow: 0,
            space: CGColorSpace(name: CGColorSpace.sRGB)!, bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue
        ))
        ctx.setFillColor(red: 1, green: 0, blue: 0, alpha: 1)
        ctx.fill(CGRect(x: 0, y: 0, width: 5, height: 10))
        ctx.setFillColor(red: 0, green: 0, blue: 1, alpha: 1)
        ctx.fill(CGRect(x: 5, y: 0, width: 3, height: 10))
        let rep = NSBitmapImageRep(cgImage: try XCTUnwrap(ctx.makeImage()))
        let png = try XCTUnwrap(rep.representation(using: .png, properties: [:]))
        XCTAssertEqual(dominantRGB(png), [0xFF0000, 0x0000FF])
        XCTAssertEqual(dominantColors(png, fallback: .gray), [Color(hex: 0xFF0000), Color(hex: 0x0000FF)])
        XCTAssertEqual(dominantColors(Data([1, 2, 3]), fallback: .gray), [.gray], "unreadable: the port color")
    }

    func testPreviewOnlyWhenInUse() {
        XCTAssertTrue(shouldSnapshot(detailServer(inUse: true)))
        XCTAssertFalse(shouldSnapshot(detailServer(inUse: false)))
        XCTAssertTrue(previewCacheFresh(takenAt: Date(timeIntervalSince1970: 0), now: Date(timeIntervalSince1970: 179)))
        XCTAssertFalse(previewCacheFresh(takenAt: Date(timeIntervalSince1970: 0), now: Date(timeIntervalSince1970: 180)))
    }

    func testPreviewStaysOnTheServersOwnPort() {
        func allows(_ u: String) -> Bool { previewAllows(URL(string: u)!, port: 3010) }
        XCTAssertTrue(allows("http://localhost:3010/"))
        XCTAssertTrue(allows("http://127.0.0.1:3010/app?x=1"))
        XCTAssertFalse(allows("http://localhost:3011/"))
        XCTAssertFalse(allows("http://localhost/"), "no port is port 80")
        XCTAssertFalse(allows("https://localhost:3010/"))
        XCTAssertFalse(allows("http://example.com:3010/"))
        XCTAssertFalse(allows("http://localhost.example.com:3010/"))
        XCTAssertFalse(allows("http://[::1]:3010/"))
        XCTAssertEqual(previewURL(port: 3010).absoluteString, "http://127.0.0.1:3010/")
        let rules = previewContentRules(port: 3010)
        let parsed = try? JSONSerialization.jsonObject(with: Data(rules.utf8)) as? [[String: Any]]
        XCTAssertEqual(parsed?.count, 3, "block all, then let the two loopback origins through")
        XCTAssertTrue(rules.contains("^http://127\\\\.0\\\\.0\\\\.1:3010/"))
        XCTAssertTrue(rules.contains("^http://localhost:3010/"))
    }

    func testDetailCopyHasNoEmDash() {
        let s = detailServer()
        let copy = [statusSentence(s), uptimeChip(ms: 1), processesLabel(2), copiedLabel, sessionChipText(s)]
            + [DetailButton.open, .stop, .restart, .postpone, .stopNow].map(\.title)
        for text in copy {
            XCTAssertFalse(text.contains("\u{2014}"), text)
        }
    }
}

final class RestartHoldTests: XCTestCase {
    private let t0 = Date(timeIntervalSince1970: 1_700_000_000)

    /// The card being restarted, and the process its Restart started.
    private func pair() -> (old: ServerViewFfi, new: ServerViewFfi) {
        let old = menuServer(port: 3000, card: "idle_alert")
        var new = menuServer(port: 3000)
        new.pid = 9001
        new.startTime = old.startTime + 1
        return (old, new)
    }

    /// The menu's rows after a pass, the way `CoreModel.pass` applies it.
    private func land(
        _ held: inout [ServerID: HeldRestart], generation: Int, at now: Date, servers: [ServerViewFfi]
    ) -> [ServerViewFfi] {
        for (id, h) in held where releasesRestart(h, generation: generation, now: now, servers: servers) {
            held[id] = nil
        }
        return withHeldRestarts(servers, held: Array(held.values))
    }

    /// The manual check: the core returns as soon as the new process is
    /// started, and the next pass does not list it yet (it is not listening),
    /// so the row must stay, card and all, until a pass lists the new one.
    func testTheRowStaysUntilThePassThatListsTheNewProcess() {
        let (old, new) = pair()
        var held: [ServerID: HeldRestart] = [old.id: HeldRestart(row: old)]
        // A pass while the core call blocks: the old process is gone.
        var rows = land(&held, generation: 2, at: t0, servers: [])
        XCTAssertEqual(rows.map(\.id), [old.id])
        XCTAssertNotNil(menuSections(rows).needsYou.first?.card, "still a card")
        // The core returned during pass 2.
        held[old.id]?.done = (2, t0.addingTimeInterval(1))
        rows = land(&held, generation: 3, at: t0.addingTimeInterval(5), servers: [])
        XCTAssertEqual(rows.map(\.id), [old.id], "the new process does not listen yet")
        rows = land(&held, generation: 4, at: t0.addingTimeInterval(10), servers: [new])
        XCTAssertEqual(rows.map(\.id), [new.id], "replaced, not doubled")
        XCTAssertTrue(held.isEmpty)
    }

    func testAPassEnqueuedBeforeTheCoreReturnedNeverReleases() {
        let (old, new) = pair()
        let h = HeldRestart(row: old, done: (5, t0))
        XCTAssertFalse(releasesRestart(h, generation: 5, now: t0.addingTimeInterval(999), servers: [new]))
        XCTAssertFalse(releasesRestart(HeldRestart(row: old), generation: 9, now: t0, servers: [new]))
        XCTAssertTrue(releasesRestart(h, generation: 6, now: t0, servers: [new]))
    }

    func testANewProcessThatNeverListensLetsGoAfterTheLimit() {
        let (old, _) = pair()
        let h = HeldRestart(row: old, done: (1, t0))
        XCTAssertFalse(releasesRestart(h, generation: 2, now: t0.addingTimeInterval(restartHoldLimit - 1), servers: []))
        XCTAssertTrue(releasesRestart(h, generation: 2, now: t0.addingTimeInterval(restartHoldLimit), servers: []))
    }

    func testReplacementIsAnotherProcessOnThePortOrTheSameCommandInTheFolder() {
        let (old, new) = pair()
        XCTAssertTrue(isReplacement(new, of: old))
        XCTAssertFalse(isReplacement(old, of: old), "the old process itself")
        var moved = new
        moved.port = 3001
        XCTAssertTrue(isReplacement(moved, of: old), "same command, same folder")
        var other = menuServer(port: 4000, worktree: "api")
        other.command = "node api.js"
        XCTAssertFalse(isReplacement(other, of: old))
    }

    func testAListedRowIsNotDoubled() {
        let (old, _) = pair()
        let rows = withHeldRestarts([old], held: [HeldRestart(row: old)])
        XCTAssertEqual(rows.map(\.id), [old.id])
    }
}

/// The structured status for the English sentence a test sets (`kept by
/// you`, `kept by Claude: "reason"`, `in use`, `idle 34 min`).
private func statusOf(_ text: String) -> StatusFfi {
    func status(_ kind: String, _ minutes: UInt64 = 0, _ agent: String = "", _ reason: String? = nil) -> StatusFfi {
        StatusFfi(kind: kind, minutes: minutes, agentLabel: agent, reason: reason)
    }
    if text == "in use" { return status("in_use") }
    if text == "kept by you" { return status("kept_by_user") }
    if text.hasPrefix("kept by ") {
        let rest = text.dropFirst("kept by ".count)
        guard let colon = rest.range(of: ": \"") else { return status("kept_by_agent", 0, String(rest)) }
        let reason = rest[colon.upperBound...].dropLast()
        return status("kept_by_agent", 0, String(rest[..<colon.lowerBound]), String(reason))
    }
    return status("idle", UInt64(text.split(separator: " ").dropFirst().first.flatMap { Int($0) } ?? 0))
}
