import Foundation
import SwiftUI

/// Made-up servers for the README screenshots: no real project, path or
/// port of anyone's machine.
@MainActor
enum Demo {
    private static let mb: UInt64 = 1_048_576
    private static let now = UInt64(Date().timeIntervalSince1970 * 1000)

    static let storefront = server(
        pid: 41_001, port: 3000, ports: [3000, 9229], title: "storefront", framework: "Next.js",
        command: "next dev --port 3000", branch: "feat/checkout", agent: "claude_code",
        rss: 1_870 * mb, health: "red", status: idle(4),
        card: CardFfi(
            kind: "leak", reason: "", action: "restart", rssBytes: 1_870 * mb,
            growthBytes: Int64(640 * mb), inMs: 0, idleMs: 0
        ),
        leakKind: "growth", growth: 640 * mb, cpu: 38, processes: 6
    )

    static let servers: [ServerViewFfi] = [
        storefront,
        server(
            pid: 41_002, port: 5173, title: "admin", framework: "Vite", command: "vite",
            branch: "fix/login-redirect", agent: "codex", rss: 240 * mb, status: idle(42),
            card: CardFfi(
                kind: "idle_alert", reason: "", action: "stop", rssBytes: 240 * mb,
                growthBytes: 0, inMs: 0, idleMs: 42 * 60_000
            )
        ),
        server(
            pid: 41_003, port: 4321, title: "docs", framework: "Astro", command: "astro dev",
            branch: "docs/getting-started", agent: "claude_code", rss: 820 * mb, health: "amber",
            status: StatusFfi(kind: "in_use", minutes: 0, agentLabel: "", reason: nil), inUse: true
        ),
        server(
            pid: 41_004, port: 8081, title: "mobile", framework: "Expo", command: "expo start",
            branch: "feat/onboarding", agent: "claude_code", rss: 560 * mb, health: "amber",
            status: idle(6)
        ),
        server(
            pid: 41_005, port: 8787, title: "edge-worker", framework: "wrangler", command: "wrangler dev",
            branch: "main", agent: "", rss: 310 * mb,
            status: StatusFfi(kind: "in_use", minutes: 0, agentLabel: "", reason: nil), inUse: true
        ),
        server(
            pid: 41_006, port: 4000, title: "blog", framework: "Eleventy", command: "eleventy --serve",
            branch: "post/release-notes", agent: "codex", rss: 140 * mb,
            status: StatusFfi(kind: "kept_by_agent", minutes: 0, agentLabel: "Codex", reason: "demo at 3 pm")
        ),
        quiet(pid: 41_007, port: 5432, title: "postgres", rss: 48 * mb),
        quiet(pid: 41_008, port: 6379, title: "redis-server", rss: 12 * mb),
        quiet(pid: 41_009, port: 11_434, title: "ollama", rss: 64 * mb),
    ]

    static let memory = MemoryBarFfi(
        devBytes: servers.reduce(0) { $0 + $1.rssBytes }, appsBytes: 15_200 * mb,
        freeBytes: 12_100 * mb, totalBytes: 32_768 * mb, line: "", freeLow: false
    )

    static let kills: [KillRecordFfi] = [
        kill(port: 4322, tool: "Astro", worktree: "landing-v2", reason: "worktree_deleted", minutesAgo: 18),
        kill(port: 3001, tool: "Next.js", worktree: "spike-auth", reason: "dead_session_idle", minutesAgo: 95),
        kill(port: 5174, tool: "Vite", worktree: "admin", reason: "manual", minutesAgo: 240),
    ]

    /// The leaking server's detail: two hours climbing, then the alert.
    static func detail(model: CoreModel) -> some View {
        let s = storefront
        let start = now - 120 * 60_000
        let history = (0...120).map { i -> SampleFfi in
            let t = Double(i) / 120
            let rss = 380.0 + 1_490.0 * pow(t, 2.2) + 18 * sin(Double(i) / 3)
            return SampleFfi(
                atMs: start + UInt64(i) * 60_000, rssBytes: UInt64(rss) * mb,
                cpuPct: Float(6 + 30 * t + 4 * sin(Double(i) / 2))
            )
        }
        let events = [
            EventFfi(atMs: start + 2 * 60_000, kind: "browser_connected", label: ""),
            EventFfi(atMs: now - 9 * 60_000, kind: "alert", label: ""),
        ]
        let processes = [
            ProcRowFfi(pid: 41_001, command: "node next dev --port 3000", rssBytes: 210 * mb),
            ProcRowFfi(pid: 41_011, command: "next-server (v15)", rssBytes: 1_420 * mb),
            ProcRowFfi(pid: 41_012, command: "node jest-worker/processChild.js", rssBytes: 160 * mb),
        ]
        return DetailContent(
            server: s,
            once: DetailOnce(
                resumeCommand: "cd '~/code/shop/feat-checkout' && claude --resume 7f3c\u{2026}",
                iconRGB: [0x1F_2A_44, 0x3B_82_F6], leakLineBytes: 2_048 * mb, deadSessionIdleS: 1_200
            ),
            live: DetailLive(history: history, events: events, processes: processes),
            favicon: nil, preview: nil, note: nil, restarting: false, showProcesses: .constant(true),
            onBack: {}, more: AnyView(MoreMenu(server: s, inDetail: true, onImage: false)),
            moreOnImage: AnyView(MoreMenu(server: s, inDetail: true, onImage: true)),
            perform: { _ in }
        )
        .environmentObject(MenuNavigation())
    }

    private static func idle(_ minutes: UInt64) -> StatusFfi {
        StatusFfi(kind: "idle", minutes: minutes, agentLabel: "", reason: nil)
    }

    private static func server(
        pid: Int32, port: UInt16, ports: [UInt16]? = nil, title: String, framework: String?,
        command: String, branch: String?, agent: String, rss: UInt64, health: String = "green",
        status: StatusFfi, card: CardFfi? = nil, leakKind: String? = nil, growth: UInt64 = 0,
        cpu: Float? = nil, inUse: Bool = false, processes: UInt32 = 2
    ) -> ServerViewFfi {
        let managed = !agent.isEmpty
        let agentLabel = agent == "claude_code" ? "Claude" : agent == "codex" ? "Codex" : "no agent"
        let worktree = branch.map { $0.replacingOccurrences(of: "/", with: "-") } ?? title
        let idleMs = status.kind == "idle" ? status.minutes * 60_000 : 0
        let age: UInt64 = 3 * 3_600_000
        return ServerViewFfi(
            pid: pid, startTime: UInt64(pid) * 1_000, port: port, ports: ports ?? [port], command: command,
            toolLabel: framework ?? title, worktree: "/Users/dev/code/\(title)/\(worktree)",
            worktreeName: worktree, locationLabel: "\(title) \u{203A} \(worktree)",
            sessionName: managed ? title : "", agent: agent, agentLabel: agentLabel,
            startedMs: now - age, startedLabel: "", startedAgeMs: age,
            sessionAlive: managed, idleMs: idleMs, killInMs: nil, state: status.kind,
            managed: managed, excluded: false, framework: framework, title: title, hasIcon: false,
            branch: branch, rssBytes: rss, cpuPct: cpu ?? (inUse ? 12 : 1), growthBytes: Int64(growth), health: health,
            inUse: inUse, statusText: "", status: status, card: card, leakKind: leakKind,
            canRestart: true, postponed: false, uptimeMs: age, processCount: processes
        )
    }

    /// A listener no agent started, no framework, small: "Other ports".
    private static func quiet(pid: Int32, port: UInt16, title: String, rss: UInt64) -> ServerViewFfi {
        var s = server(
            pid: pid, port: port, title: title, framework: nil, command: title, branch: nil, agent: "",
            rss: rss, status: idle(90)
        )
        s.worktree = ""
        s.worktreeName = ""
        s.locationLabel = ""
        s.canRestart = false
        return s
    }

    private static func kill(
        port: UInt16, tool: String, worktree: String, reason: String, minutesAgo: UInt64
    ) -> KillRecordFfi {
        KillRecordFfi(
            port: port, command: tool, toolLabel: tool, worktree: "/Users/dev/code/\(worktree)",
            worktreeName: worktree, reason: reason, reasonKind: reason, idleMs: 25 * 60_000,
            atMs: now - minutesAgo * 60_000
        )
    }
}
