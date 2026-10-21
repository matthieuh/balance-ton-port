import AppKit
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
    /// Servers due an alert that could not be shown (notifications not
    /// allowed): their row says so, and the alert is not marked as sent.
    @Published private(set) var unnotified: Set<ServerID> = []
    /// A short note about the last manual kill ("already gone"), shown in the
    /// menu for `killedHoldTime`.
    @Published private(set) var killNotice: String?
    private var killNoticeAt: Date?
    /// The last Settings save error, cleared by the next successful save.
    @Published private(set) var settingsError: String?
    /// What macOS allows for notifications; `nil` until first read.
    @Published private(set) var notificationPermission: NotificationPermission?

    private let core = Core()
    private let queue = DispatchQueue(label: "com.matthieuh.balancetonport.core", qos: .utility)
    private var timer: Timer?
    /// Servers killed by hand, with when. A pass already queued when the
    /// kill was requested can still report them; they stay hidden until
    /// `killedHoldTime` has passed.
    private var killedByHand: [ServerID: Date] = [:]
    /// Every server the last pass reported, hidden ones included.
    private var lastPassServers: [ServerViewFfi] = []
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
        // Permission changes happen in System Settings; pick them up when
        // the user comes back.
        NotificationCenter.default.addObserver(
            forName: NSApplication.didBecomeActiveNotification, object: nil, queue: .main
        ) { [weak self] _ in
            MainActor.assumeIsolated {
                guard let self else { return }
                Task { await self.refreshNotificationPermission() }
            }
        }
        Task {
            await notifier.requestAuthorization()
            await refreshNotificationPermission()
        }
    }

    func refreshNotificationPermission() async {
        notificationPermission = await notifier.permission()
    }

    /// Shows the system prompt (first time only), then the new status.
    func requestNotifications() async {
        await notifier.requestAuthorization()
        await refreshNotificationPermission()
    }

    /// `true` once Notification Center accepted the test.
    func sendTestNotification() async -> Bool {
        await notifier.postSample()
    }

    /// Kills one server by hand. The row leaves the list at once; the next
    /// pass confirms it.
    func kill(_ server: ServerViewFfi) {
        kill(server.id)
    }

    /// Kills by identity (pid and start time), from the menu or a
    /// notification's Kill button. Asks the core directly, not the last pass:
    /// right after a relaunch the list is still empty. The core kills only
    /// that exact process; when it is not tracked under that identity
    /// (already gone, its pid now another process, or merged into the server
    /// that started it), nothing is signalled, the row is no longer held
    /// hidden, and the menu says "already gone" only when no tracked server
    /// has that pid.
    func kill(_ id: ServerID) {
        killedByHand[id] = Date()
        servers.removeAll { $0.id == id }
        Task {
            do {
                let (record, kills) = try await onCore { core in
                    let record = try core.killServer(pid: id.pid, startTime: id.startTime)
                    return (record, try core.recentKills())
                }
                recentKills = kills
                if record == nil {
                    killedByHand[id] = nil
                    killNotice = missedKillNotice(id, tracked: lastPassServers.map(\.id))
                    killNoticeAt = killNotice == nil ? nil : Date()
                } else {
                    killNotice = nil
                }
            } catch {
                killedByHand[id] = nil
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
    /// Throws when `config.toml` cannot be parsed: the form must not show (and
    /// then save) defaults over what the user wrote.
    func loadConfig() async throws -> (config: ConfigFfi, discovered: [String]) {
        try await onCore { core in (try core.config(), core.discoveredClaudeDirs()) }
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
                // The file may be broken (reported in `configError`): fall
                // back to the default interval, the core keeps its last
                // valid config anyway.
                let scan = (try? core.config())?.scanIntervalS ?? 5
                return PassResult(
                    owner: owner, servers: report.servers, alerts: report.alerts,
                    kills: kills, scanIntervalS: scan, configError: report.configError
                )
            }
            watchedElsewhere = !result.owner
            let now = Date()
            killedByHand = killedByHand.filter { now.timeIntervalSince($0.value) < killedHoldTime }
            if let at = killNoticeAt, now.timeIntervalSince(at) >= killedHoldTime {
                killNotice = nil
                killNoticeAt = nil
            }
            lastPassServers = result.servers
            servers = result.servers.filter { killedByHand[$0.id] == nil }
            recentKills = result.kills
            // Only the watching app alerts; a read-only pass leaves them to
            // the other watcher. An alert is marked only once it was added
            // to Notification Center, so a refused one is retried and its
            // row says notifications are off.
            if result.owner && !result.alerts.isEmpty {
                let delivered = await notifier.post(result.alerts)
                if !delivered.isEmpty {
                    try await onCore { core in
                        for id in delivered {
                            try core.markAlerted(pid: id.pid, startTime: id.startTime)
                        }
                    }
                }
                unnotified = Set(result.alerts.map(\.id)).subtracting(delivered)
            } else {
                unnotified = []
            }
            await refreshNotificationPermission()
            interval = TimeInterval(max(1, result.scanIntervalS))
            lastError = result.configError
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
    let configError: String?
}
