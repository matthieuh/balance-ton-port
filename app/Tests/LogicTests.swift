import Foundation
import XCTest

private func server(
    command: String = "bun run dev",
    port: UInt16 = 4322,
    worktree: String = "web",
    idleMs: UInt64 = 0,
    killInMs: UInt64? = nil,
    state: String = "active"
) -> ServerViewFfi {
    ServerViewFfi(
        pid: 4242, startTime: 1_700_000_000_123, port: port, command: command,
        worktreeName: worktree, sessionAlive: true, idleMs: idleMs,
        killInMs: killInMs, state: state
    )
}

final class NotificationTextTests: XCTestCase {
    func testBodyRoundsMinutesDown() {
        let s = server(command: "astro dev", idleMs: 30 * 60_000 + 59_999)
        XCTAssertEqual(
            notificationBody(s),
            "astro dev on port 4322 (worktree web) hasn't been used for 30 min."
        )
    }

    func testBodyUnderOneMinute() {
        XCTAssertEqual(
            notificationBody(server(idleMs: 59_000)),
            "bun run dev on port 4322 (worktree web) hasn't been used for 0 min."
        )
    }

    func testShortCommandKeepsShortOnes() {
        XCTAssertEqual(shortCommand("bun run dev"), "bun run dev")
        XCTAssertEqual(shortCommand("  /usr/bin/node x.js "), "/usr/bin/node x.js")
    }

    func testShortCommandBasenamesPaths() {
        XCTAssertEqual(
            shortCommand("/Users/me/.bun/bin/bun /Users/me/repo/node_modules/.bin/astro dev"),
            "bun astro dev"
        )
    }

    func testShortCommandFallsBackToFirstTwoWords() {
        let long = "/opt/homebrew/bin/node /repo/node_modules/next/dist/bin/next dev --turbopack --port 3000 --hostname 0.0.0.0"
        XCTAssertEqual(shortCommand(long), "node next")
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
        claudeConfigDirs: ["/Users/me/.claude"], excludeCommands: ["mcp"]
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
        var draft = SettingsDraft(core.config())
        XCTAssertTrue(draft.claudeConfigDirs.isEmpty, "extras only, discovered dirs stay out")
        draft.alertAfterMin = 1
        draft.excludeCommands = addingEntry("vite", to: draft.excludeCommands) ?? draft.excludeCommands
        try core.setConfig(c: draft.ffi)

        let reread = Core().config()
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
