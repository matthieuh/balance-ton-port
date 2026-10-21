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
    /// The last Settings save error, cleared by the next successful save.
    @Published private(set) var settingsError: String?

    private let core = Core()
    private let queue = DispatchQueue(label: "com.matthieuh.balancetonport.core", qos: .utility)
    private var timer: Timer?
    /// Servers killed by hand, with when. A pass already queued when the
    /// kill was requested can still report them; they stay hidden until
    /// `killedHoldTime` has passed.
    private var killedByHand: [ServerID: Date] = [:]
    private let killedHoldTime: TimeInterval = 30
    private let notifier = Notifier()

    init() {
        notifier.onAction = { [weak self] action, id in
            guard let self else { return }
            switch action {
            case .kill: self.kill(id)
            case .keep: self.keep(id)
            }
        }
        scheduleNextPass(after: 0)
    }

    /// Kills one server by hand. The row leaves the list at once; the next
    /// pass confirms it.
    func kill(_ server: ServerViewFfi) {
        kill(server.id)
    }

    /// Kills by identity, from the menu or a notification's Kill button.
    /// Does nothing when the last pass no longer lists this exact server
    /// (gone already, or its pid now belongs to another process).
    func kill(_ id: ServerID) {
        guard servers.contains(where: { $0.id == id }) else { return }
        killedByHand[id] = Date()
        servers.removeAll { $0.id == id }
        let pid = id.pid
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

    /// A notification's Keep button: no more alerts for this server until it
    /// restarts.
    func keep(_ id: ServerID) {
        Task {
            do {
                try await onCore { core in try core.keep(pid: id.pid, startTime: id.startTime) }
            } catch {
                lastError = error.localizedDescription
            }
        }
    }

    /// The config as saved on disk (defaults when there is no file), whose
    /// `claudeConfigDirs` are the extras, plus the dirs found automatically.
    func loadConfig() async -> (config: ConfigFfi, discovered: [String]) {
        let core = self.core
        return await withCheckedContinuation { continuation in
            queue.async {
                continuation.resume(returning: (core.config(), core.discoveredClaudeDirs()))
            }
        }
    }

    /// Saves the Settings form. Enqueued on the core queue right away, so
    /// saves land in the order the form made them. The watcher re-reads the
    /// config every pass.
    func saveConfig(_ config: ConfigFfi) {
        let core = self.core
        queue.async { [weak self] in
            let message: String?
            do {
                try core.setConfig(c: config)
                message = nil
            } catch {
                message = error.localizedDescription
            }
            Task { @MainActor in self?.settingsError = message }
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
                return PassResult(
                    owner: owner, servers: report.servers, alerts: report.alerts,
                    kills: kills, scanIntervalS: scan
                )
            }
            watchedElsewhere = !result.owner
            let now = Date()
            killedByHand = killedByHand.filter { now.timeIntervalSince($0.value) < killedHoldTime }
            servers = result.servers.filter { killedByHand[$0.id] == nil }
            recentKills = result.kills
            // Only the watching app alerts; a read-only pass marks no alert.
            if result.owner && !result.alerts.isEmpty {
                notifier.post(result.alerts)
            }
            interval = TimeInterval(max(1, result.scanIntervalS))
            lastError = nil
        } catch {
            lastError = error.localizedDescription
        }
        scheduleNextPass(after: interval)
    }

    private func scheduleNextPass(after interval: TimeInterval) {
        timer?.invalidate()
        let next = Timer(timeInterval: interval, repeats: false) { [weak self] _ in
            // Added to the main run loop, so the block runs on the main thread.
            MainActor.assumeIsolated {
                guard let self else { return }
                Task { await self.pass() }
            }
        }
        // `.common` keeps passes running while the menu is open (event
        // tracking mode), not only in the default mode.
        RunLoop.main.add(next, forMode: .common)
        timer = next
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
    let alerts: [ServerViewFfi]
    let kills: [KillRecordFfi]
    let scanIntervalS: UInt64
}
