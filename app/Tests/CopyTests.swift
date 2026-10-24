import Foundation
import XCTest

/// The English copy is what the D plan and today's tests fix; the catalog is
/// looked up through any bundle and locale a test hands in.

extension CardFfi {
    /// A card without numbers (the tests that only look at its kind and action).
    init(kind: String, reason: String, action: String) {
        self.init(kind: kind, reason: reason, action: action, rssBytes: 0, growthBytes: 0, inMs: 0, idleMs: 0)
    }
}

private func status(_ kind: String, _ minutes: UInt64 = 0, agent: String = "", reason: String? = nil) -> StatusFfi {
    StatusFfi(kind: kind, minutes: minutes, agentLabel: agent, reason: reason)
}

private func card(
    _ kind: String, action: String = "stop", rss: UInt64 = 0, growth: Int64 = 0, inMs: UInt64 = 0, idleMs: UInt64 = 0
) -> CardFfi {
    CardFfi(kind: kind, reason: "", action: action, rssBytes: rss, growthBytes: growth, inMs: inMs, idleMs: idleMs)
}

private func row(
    toolLabel: String = "Vite", port: UInt16 = 5173, idleMs: UInt64 = 0, status: StatusFfi = status("idle")
) -> ServerViewFfi {
    ServerViewFfi(
        pid: 1, startTime: 2, port: port, ports: [port], command: "vite", toolLabel: toolLabel,
        worktree: "/r/app", worktreeName: "app", locationLabel: "my-project \u{203A} web", sessionName: "web",
        agent: "claude_code", agentLabel: "Claude", startedMs: 0, startedLabel: "", startedAgeMs: 0,
        sessionAlive: true, idleMs: idleMs, killInMs: nil, state: "active", managed: true, excluded: false,
        framework: nil, title: toolLabel, hasIcon: false, branch: nil, rssBytes: 0, cpuPct: 0, growthBytes: 0,
        health: "green", inUse: false, statusText: "", status: status, card: nil, leakKind: nil,
        canRestart: false, postponed: false, uptimeMs: 0, processCount: 1
    )
}

/// A bundle with a `fr.lproj` holding `entries`, to stand for a translated catalog.
private func frenchBundle(_ entries: [String: String]) throws -> Bundle {
    let dir = FileManager.default.temporaryDirectory.appendingPathComponent("btp-copy-\(UUID().uuidString)")
    let lproj = dir.appendingPathComponent("fr.lproj")
    try FileManager.default.createDirectory(at: lproj, withIntermediateDirectories: true)
    let text = entries.map { "\"\($0.key)\" = \"\($0.value)\";" }.joined(separator: "\n")
    try text.write(to: lproj.appendingPathComponent("Localizable.strings"), atomically: true, encoding: .utf8)
    return try XCTUnwrap(Bundle(path: dir.path))
}

final class CopyTests: XCTestCase {
    func testEnglishCopyUnchanged() {
        XCTAssertEqual(statusText(status("in_use")), "in use")
        XCTAssertEqual(statusText(status("kept_by_agent", agent: "Claude", reason: "watching the build")), "kept by Claude: \"watching the build\"")
        XCTAssertEqual(statusText(status("kept_by_agent", agent: "Codex")), "kept by Codex")
        XCTAssertEqual(statusText(status("kept_by_user")), "kept by you")
        XCTAssertEqual(statusText(status("stops_in", 8)), "stops in 8 min")
        XCTAssertEqual(statusText(status("stops_in", 0)), "stops in <1 min")
        XCTAssertEqual(statusText(status("stops_in", 120)), "stops in 2 h")
        XCTAssertEqual(statusText(status("excluded")), "excluded")
        XCTAssertEqual(statusText(status("idle", 34)), "idle 34 min")
        XCTAssertEqual(statusText(status("idle", 3 * 24 * 60)), "idle 3 d")

        let growth = card("leak", action: "restart", rss: 900 * 1_048_576, growth: 600 * 1_048_576)
        XCTAssertEqual(cardReason(growth, leakKind: "growth"), "Growing fast: +600 MB in 10 min")
        XCTAssertEqual(cardReason(growth, leakKind: "size"), "Using 900 MB")
        XCTAssertEqual(cardReason(card("about_to_stop", inMs: 8 * 60_000)), "Session ended, unused \u{00B7} stops in 8 min")
        XCTAssertEqual(cardReason(card("session_ended_open")), "Session ended, still open in a browser")
        XCTAssertEqual(cardReason(card("idle_alert", idleMs: 40 * 60_000), agentLabel: "Codex"), "Codex session idle 40 min")

        let blocked: [(RestartBlockedFfi, String)] = [
            (.gone, "Could not start it again"), (.spawnFailed, "Could not start it again"),
            (.cwdGone, "The folder no longer exists"), (.envUnreadable, "Cannot read its environment"),
            (.argvUnreadable, "Cannot read its environment"), (.portStillTaken, "The port is still taken"),
            (.excluded, "It matches an exclusion now"), (.busy, "Already restarting"),
        ]
        for (b, text) in blocked { XCTAssertEqual(restartBlockedText(b), text) }

        XCTAssertEqual(
            [("started", "x"), ("browser_connected", "x"), ("browser_closed", "x"), ("session_ended", "x"),
             ("alert", "x"), ("restarted", "x"), ("postponed", "x")].map { eventLabel(kind: $0.0) },
            ["started", "browser connected", "browser closed", "session ended", "alert", "restarted", "postponed 1 h"]
        )
        XCTAssertEqual(minutes(1), "1 min")
        XCTAssertEqual(minutes(12), "12 min")
        XCTAssertEqual(secondsText(30), "30 s")
        XCTAssertEqual(headerCount(1), "1 server")
        XCTAssertEqual(headerCount(7), "7 servers")
        XCTAssertEqual(processesLabel(1), "1 process \u{203A}")
        XCTAssertEqual(processesLabel(3), "3 processes \u{203A}")
        XCTAssertEqual(moreLabel(2), "+ 2 more \u{25B8}")
        XCTAssertEqual(recentlyStoppedLabel(1), "Recently stopped (1)")
        XCTAssertEqual(sectionTitle("Needs you", count: 2), "NEEDS YOU \u{00B7} 2")
        XCTAssertEqual(uptimeChip(ms: 5 * 3_600_000), "up 5 h")
        XCTAssertEqual(cpuLabel(3.6), "CPU 4%")
        XCTAssertEqual(startedText(ageMs: 30_000), "started just now")
        XCTAssertEqual(startedText(ageMs: 4 * 60_000), "started 4 min ago")
        XCTAssertEqual(
            notificationBody(row(toolLabel: "Astro", port: 4322, idleMs: 30 * 60_000)),
            "Astro on port 4322 (my-project \u{203A} web) hasn't been used for 30 min."
        )
        XCTAssertEqual(
            sampleNotificationBody, "Test: Vite on port 5173 (my-project \u{203A} web) hasn't been used for 30 min."
        )
        XCTAssertEqual(
            [NotificationButtonTitle.kill, .keep, .stopAll, .restart, .ignore].map { $0.text() },
            ["Stop", "Keep", "Stop all", "Restart", "Ignore"]
        )
        XCTAssertEqual(
            killReasonText(KillRecordFfi(port: 1, command: "", toolLabel: "", worktree: "", worktreeName: "", reason: "", reasonKind: "dead_session_idle", idleMs: 23 * 60_000, atMs: 0)),
            "dead session + idle 23 min"
        )
        XCTAssertEqual(
            killReasonText(KillRecordFfi(port: 1, command: "", toolLabel: "", worktree: "", worktreeName: "", reason: "", reasonKind: "manual", idleMs: 0, atMs: 0)),
            "manual"
        )
        XCTAssertEqual(
            usageLine(MemoryBarFfi(
                devBytes: 5_153_960_755, appsBytes: 28_779_460_000, freeBytes: 4_724_464_025,
                totalBytes: 38_654_705_664, line: "", freeLow: false
            )),
            "4.8 GB dev servers (13%) \u{00B7} apps 26.8 \u{00B7} free 4.4 of 36 GB"
        )
    }

    func testSessionEndedBodyMatchesTheHook() {
        func group(_ n: Int) -> SessionEndedFfi {
            SessionEndedFfi(
                sessionPid: 1, sessionProcStart: "s", atMs: 2, sessionName: "web",
                servers: (0..<n).map { GroupServerFfi(pid: Int32($0), startTime: 0, port: UInt16(5173 + $0), toolLabel: "Vite", inUse: false) },
                body: "", deadIdleS: 1800
            )
        }
        XCTAssertEqual(
            sessionEndedBody(group(1)),
            "Session ended: 1 server kept running: Vite :5173. Stopped after 30 min unused, once no session works in that folder."
        )
        XCTAssertEqual(
            sessionEndedBody(group(6)),
            "Session ended: 6 servers kept running: Vite :5173, Vite :5174, Vite :5175, Vite :5176 and 2 more. Stopped after 30 min unused, once no session works in that folder."
        )
    }

    func testFrenchCopyUsesCatalog() throws {
        let fr = try frenchBundle(["idle %@": "inactif depuis %@", "%lld min": "%lld minutes"])
        let locale = Locale(identifier: "fr")
        let english = statusText(status("idle", 34))
        let french = statusText(status("idle", 34), bundle: fr, locale: locale)
        XCTAssertNotEqual(french, english)
        XCTAssertTrue(french.contains("34"))
        XCTAssertEqual(french, "inactif depuis 34 minutes")
        XCTAssertEqual(statusText(status("in_use"), bundle: fr, locale: locale), "in use", "a missing key falls back to its English key")
    }

    func testMemoryLabelUsesLocaleDecimal() {
        let bytes = UInt64(2.3 * 1024 * 1_048_576)
        XCTAssertEqual(memoryLabel(bytes: bytes, locale: Locale(identifier: "en")), "2.3 GB")
        XCTAssertEqual(memoryLabel(bytes: bytes, locale: Locale(identifier: "fr")), "2,3 GB")
        XCTAssertEqual(memoryLabel(bytes: 90 * 1_048_576, locale: Locale(identifier: "fr")), "90 MB")
        XCTAssertEqual(leakLineLabel(1_610_612_736, locale: Locale(identifier: "fr")), "1,5 GB")
    }

    func testRightToLeftIsolatesTechnicalParts() {
        let he = Locale(identifier: "he")
        XCTAssertEqual(isolated(":5173", Locale(identifier: "en")), ":5173")
        XCTAssertEqual(isolated(":5173", he), "\u{2066}:5173\u{2069}")
        let body = notificationBody(row(), locale: he)
        XCTAssertTrue(body.contains("\u{2066}Vite\u{2069}"), "the tool label reads left to right")
        XCTAssertTrue(body.contains("\u{2066}5173\u{2069}"))
    }

    @MainActor
    func testNotificationBodyUsesChosenLanguage() throws {
        let fr = try frenchBundle([
            "%@ on port %@ (%@) hasn't been used for %@.": "%1$@ sur le port %2$@ (%3$@) inutilisé depuis %4$@.",
        ])
        let defaults = try XCTUnwrap(UserDefaults(suiteName: "btp-copy-\(UUID().uuidString)"))
        let model = LanguageModel(defaults: defaults, base: fr)
        model.choice = .fr
        let body = notificationBody(row(toolLabel: "Astro", port: 4322, idleMs: 30 * 60_000), bundle: model.bundle, locale: model.locale)
        XCTAssertEqual(body, "Astro sur le port 4322 (my-project \u{203A} web) inutilisé depuis 30 min.")
    }

    func testCatalogHasEveryEnglishKeyUsed() throws {
        // Each key resolves to itself (formatted) in English, so none is missing
        // from the catalog: a missing key would also look like itself, so read
        // the compiled table to be sure.
        let table = try XCTUnwrap(copyBundle.path(forResource: "Localizable", ofType: "strings"))
        let strings = try XCTUnwrap(NSDictionary(contentsOfFile: table) as? [String: String])
        for key in ["idle %@", "stops in %@", "kept by %@", "Needs you", "All good", "Stop all", "restarting\u{2026}"] {
            XCTAssertNotNil(strings[key], key)
        }
    }
}
