import Foundation

/// Owns the Rust `Core` for the app's lifetime and publishes what the menu
/// shows. Every `Core` call can block (kills wait up to `kill_grace_s` while
/// holding the core's lock, a tick may run several kills), so all of them run
/// on one serial background queue, never on the main thread.
@MainActor
final class CoreModel: ObservableObject {
    @Published private(set) var servers: [ServerViewFfi] = []
    @Published private(set) var recentKills: [KillRecordFfi] = []
    /// True while another process (for example `btp watch`) holds the watch
    /// lock: the app then only observes and never runs a killing tick.
    @Published private(set) var watchedElsewhere = false
    @Published private(set) var lastError: String?

    private let core = Core()
    private let queue = DispatchQueue(label: "com.matthieuh.balancetonport.core", qos: .utility)
    private var timer: Timer?

    init() {
        scheduleNextPass(after: 0)
    }

    /// Kills one server by hand. The row leaves the list at once; the next
    /// pass confirms it.
    func kill(_ server: ServerViewFfi) {
        servers.removeAll { $0.pid == server.pid && $0.startTime == server.startTime }
        let pid = server.pid
        Task {
            do {
                let kills = try await onCore { core in
                    _ = try core.killPid(pid: pid)
                    return try core.recentKills()
                }
                recentKills = kills
                lastError = nil
            } catch {
                lastError = error.localizedDescription
            }
        }
    }

    /// One pass, then the next one `scan_interval_s` later (re-read every
    /// pass, at least 1 s). Passes never overlap: the next timer is only
    /// scheduled once this one has finished.
    private func pass() async {
        var interval: TimeInterval = 5
        do {
            let result = try await onCore { core in
                // Retried every pass, so the app takes over when `btp watch` stops.
                let owner = try core.acquireWatch()
                let report = owner ? try core.tick() : try core.observe()
                let kills = try core.recentKills()
                let scan = core.config().scanIntervalS
                return PassResult(owner: owner, servers: report.servers, kills: kills, scanIntervalS: scan)
            }
            watchedElsewhere = !result.owner
            servers = result.servers
            recentKills = result.kills
            interval = TimeInterval(max(1, result.scanIntervalS))
            lastError = nil
        } catch {
            lastError = error.localizedDescription
        }
        scheduleNextPass(after: interval)
    }

    private func scheduleNextPass(after interval: TimeInterval) {
        timer?.invalidate()
        timer = Timer.scheduledTimer(withTimeInterval: interval, repeats: false) { [weak self] _ in
            // Scheduled on the main run loop, so the block runs on the main thread.
            MainActor.assumeIsolated {
                guard let self else { return }
                Task { await self.pass() }
            }
        }
    }

    /// Runs `work` on the core's serial queue.
    private func onCore<T: Sendable>(_ work: @escaping @Sendable (Core) throws -> T) async throws -> T {
        let core = self.core
        return try await withCheckedThrowingContinuation { continuation in
            queue.async {
                continuation.resume(with: Result { try work(core) })
            }
        }
    }
}

private struct PassResult: Sendable {
    let owner: Bool
    let servers: [ServerViewFfi]
    let kills: [KillRecordFfi]
    let scanIntervalS: UInt64
}
