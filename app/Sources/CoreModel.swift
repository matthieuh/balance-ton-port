import AppKit
import Foundation

/// Owns the Rust `Core` for the app's lifetime and publishes what the menu
/// shows. Every `Core` call can block (kills wait up to `kill_grace_s` while
/// holding the core's lock, a tick may run several kills), so all of them run
/// on one serial background queue, never on the main thread.
@MainActor
final class CoreModel: ObservableObject {
    /// Agent servers, then observed ones (`managed == false`, no agent).
    @Published private(set) var servers: [ServerViewFfi] = []
    /// The usage bar's figures from the last pass.
    @Published private(set) var memory: MemoryBarFfi?
    /// Favicons by server, loaded off the main thread once per server.
    @Published private(set) var icons: [ServerID: NSImage] = [:]
    private var iconsLoading: Set<ServerID> = []
    /// Servers whose favicon bytes NSImage could not read: not fetched again.
    private var iconsUndecodable: Set<ServerID> = []
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
    /// "restarting…" while a Restart runs, then a refused Restart's reason
    /// for `blockedNoteDuration`, in place of the row's status.
    @Published private(set) var rowNotes: [ServerID: RowNote] = [:]
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
    /// Rows changed at once by Postpone, Keep or Ignore this alert, applied
    /// over every pass enqueued before the action reached the core (`since`
    /// is `passCount` at the click), dropped by the first pass after it.
    private var optimistic: [ServerID: (since: Int, apply: (ServerViewFfi) -> ServerViewFfi)] = [:]
    /// Passes enqueued so far.
    private var passCount = 0
    private let killedHoldTime: TimeInterval = 30
    private let notifier = Notifier()

    init() {
        notifier.onAction = { [weak self] route in
            guard let self else { return }
            switch route {
            case .server(.kill, let id): self.kill(id, fromNotification: true)
            case .server(.keep, let id): self.keep(id)
            case .server(.restart, let id): self.restartFromNotification(id)
            case .server(.ignoreAlert, let id): self.ignoreAlert(id)
            case .sessionEnded(.stopNow, let id): self.stopSessionEnded(id)
            case .sessionEnded(.keep, let id): self.keepSessionEnded(id)
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
    /// hidden, and the menu says why (`missedKillNotice`).
    func kill(_ id: ServerID, fromNotification: Bool = false) {
        killedByHand[id] = Date()
        publish()
        Task {
            do {
                let (record, kills) = try await onCore { core in
                    let record = try core.killServer(pid: id.pid, startTime: id.startTime)
                    return (record, try core.recentKills())
                }
                recentKills = kills
                if record == nil {
                    killedByHand[id] = nil
                    killNotice = missedKillNotice(
                        id, tracked: lastPassServers.map(\.id), fromNotification: fromNotification
                    )
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

    /// Opens the server in the default browser.
    func open(_ server: ServerViewFfi) {
        NSWorkspace.shared.open(openURL(server))
    }

    /// Stop from the menu: an agent server at once (`kill`), a server no
    /// agent started after the confirmation. Never an excluded one.
    func stop(_ server: ServerViewFfi) {
        guard !server.excluded else { return }
        guard needsConfirmation(server) else {
            kill(server.id)
            return
        }
        confirm(server, .stop) { [weak self] in self?.stopObserved(server.id) }
    }

    /// Stops a server no agent started (already confirmed). The row leaves
    /// at once, like a kill.
    private func stopObserved(_ id: ServerID) {
        killedByHand[id] = Date()
        publish()
        Task {
            do {
                let (record, kills) = try await onCore { core in
                    let record = try core.stopObserved(pid: id.pid, startTime: id.startTime)
                    return (record, try core.recentKills())
                }
                recentKills = kills
                if record == nil {
                    killedByHand[id] = nil
                    killNotice = missedKillNotice(id, tracked: [], fromNotification: false)
                    killNoticeAt = Date()
                    publish()
                } else {
                    killNotice = nil
                }
            } catch {
                killedByHand[id] = nil
                publish()
                lastError = error.localizedDescription
            }
        }
    }

    /// Restart: stops the server and starts it again the same way (a server
    /// no agent started after the confirmation). The row says
    /// "restarting…" at once; the core call blocks for the kill grace plus
    /// up to 5 s, on the core queue. A refused Restart shows its reason in
    /// the row for `blockedNoteDuration`; a done one leaves the note until
    /// the next pass, which lists the new process instead.
    func restart(_ server: ServerViewFfi) {
        guard server.canRestart, rowNotes[server.id] != .restarting else { return }
        guard needsConfirmation(server) else {
            runRestart(server.id)
            return
        }
        confirm(server, .restart) { [weak self] in self?.runRestart(server.id) }
    }

    /// A leak notification's Restart: the app is brought forward, so the
    /// confirmation of a server no agent started shows on top.
    private func restartFromNotification(_ id: ServerID) {
        guard let server = lastPassServers.first(where: { $0.id == id }) else {
            killNotice = missedRestartNotice
            killNoticeAt = Date()
            return
        }
        NSApp.activate(ignoringOtherApps: true)
        restart(server)
    }

    private func runRestart(_ id: ServerID) {
        rowNotes[id] = .restarting
        Task {
            do {
                let blocked = try await onCore { core in try core.restart(pid: id.pid, startTime: id.startTime) }
                if let blocked { showBlocked(blocked, for: id) }
            } catch {
                rowNotes[id] = nil
                lastError = error.localizedDescription
            }
        }
    }

    private func showBlocked(_ copy: String, for id: ServerID) {
        let note = RowNote.blocked(copy)
        rowNotes[id] = note
        Task {
            try? await Task.sleep(for: .seconds(blockedNoteDuration))
            if rowNotes[id] == note { rowNotes[id] = nil }
        }
    }

    /// Postpone 1 h: the row says `stops in 60 min` at once.
    func postpone(_ server: ServerViewFfi) {
        let id = server.id
        applyOptimistic(id, postponedView)
        Task {
            do {
                let found = try await onCore { core in try core.postpone(pid: id.pid, startTime: id.startTime) }
                if !found { dropOptimistic(id) }
            } catch {
                dropOptimistic(id)
                lastError = error.localizedDescription
            }
        }
    }

    func keep(_ server: ServerViewFfi) {
        keep(server.id)
    }

    /// Keep, from the menu or a notification: no more alerts for this
    /// server until it restarts.
    func keep(_ id: ServerID) {
        applyOptimistic(id, keptView)
        Task {
            do {
                try await onCore { core in try core.keep(pid: id.pid, startTime: id.startTime) }
            } catch {
                dropOptimistic(id)
                lastError = error.localizedDescription
            }
        }
    }

    func ignoreAlert(_ server: ServerViewFfi) {
        ignoreAlert(server.id)
    }

    /// "Ignore this alert", from the menu or a leak notification: the alert
    /// card goes at once. The core refuses a card that is not an alert.
    func ignoreAlert(_ id: ServerID) {
        applyOptimistic(id, alertIgnoredView)
        Task {
            let ignored = try? await onCore { core in core.ignoreAlert(pid: id.pid, startTime: id.startTime) }
            if ignored != true { dropOptimistic(id) }
        }
    }

    /// Shows the confirmation for a server no agent started, then runs
    /// `action` when the user agrees. Run from the main run loop, outside
    /// any main-queue job, so passes keep landing while the alert is up.
    private func confirm(_ server: ServerViewFfi, _ action: ConfirmAction, then run: @escaping @MainActor () -> Void) {
        let text = confirmationText(server, action: action)
        RunLoop.main.perform(inModes: [.common]) {
            MainActor.assumeIsolated {
                NSApp.activate(ignoringOtherApps: true)
                let alert = NSAlert()
                alert.alertStyle = .warning
                alert.messageText = text.title
                alert.informativeText = text.message
                let button = alert.addButton(withTitle: text.button)
                button.hasDestructiveAction = action == .stop
                alert.addButton(withTitle: "Cancel")
                if alert.runModal() == .alertFirstButtonReturn { run() }
            }
        }
    }

    private func applyOptimistic(_ id: ServerID, _ change: @escaping (ServerViewFfi) -> ServerViewFfi) {
        let earlier = optimistic[id]?.apply
        optimistic[id] = (passCount, { change(earlier?($0) ?? $0) })
        publish()
    }

    private func dropOptimistic(_ id: ServerID) {
        optimistic[id] = nil
        publish()
    }

    /// The rows the menu shows: the last pass, without the servers stopped
    /// by hand, with the changes the core has not confirmed yet.
    private func publish() {
        servers = lastPassServers
            .filter { killedByHand[$0.id] == nil }
            .map { optimistic[$0.id]?.apply($0) ?? $0 }
    }

    /// A session-ended notification's Stop now: stops each kept server of
    /// that session (exact pid and start time; one gone or restarted is
    /// skipped).
    func stopSessionEnded(_ id: SessionEndedID) {
        Task {
            do {
                recentKills = try await onCore { core in
                    _ = try core.stopSessionEnded(
                        sessionPid: id.sessionPid, sessionProcStart: id.sessionProcStart, atMs: id.atMs
                    )
                    return try core.recentKills()
                }
            } catch {
                lastError = error.localizedDescription
            }
        }
    }

    /// A session-ended notification's Keep: Keep on each kept server of that
    /// session.
    func keepSessionEnded(_ id: SessionEndedID) {
        Task {
            do {
                try await onCore { core in
                    try core.keepSessionEnded(
                        sessionPid: id.sessionPid, sessionProcStart: id.sessionProcStart, atMs: id.atMs
                    )
                }
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
        passCount += 1
        let generation = passCount
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
                    kills: kills, scanIntervalS: scan, configError: report.configError,
                    memory: report.memory, leaks: report.leaks
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
            // This pass was enqueued after those actions: it shows them.
            optimistic = optimistic.filter { $0.value.since >= generation }
            let live = Set(result.servers.map(\.id))
            rowNotes = rowNotes.filter { live.contains($0.key) }
            publish()
            memory = result.memory
            recentKills = result.kills
            loadIcons(for: result.servers)
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
            // Leaks are this app's own (no other watcher notifies them), so
            // they are posted read-only too; marked once added, like alerts.
            if !result.leaks.isEmpty {
                let delivered = await notifier.postLeaks(result.leaks)
                if !delivered.isEmpty {
                    try await onCore { core in
                        for id in delivered {
                            core.markLeakNotified(pid: id.pid, startTime: id.startTime)
                        }
                    }
                }
            }
            await refreshNotificationPermission()
            interval = TimeInterval(max(1, result.scanIntervalS))
            lastError = result.configError
            await deliverSessionEnded()
        } catch {
            lastError = error.localizedDescription
        }
        scheduleNextPass(after: interval)
    }

    /// One notification per ended session that kept servers running. The
    /// hook leaves a group to the app only when the app holds the watch lock,
    /// and the app posts it whether or not it still does. Claim, then post:
    /// claiming marks a group delivered under the state lock, so nobody
    /// posts it twice; one whose `center.add` failed is handed back for the
    /// next pass. With notifications off nothing is claimed (the menu's
    /// notifications line says so).
    private func deliverSessionEnded() async {
        guard await notifier.canPost() else { return }
        let claimed: [SessionEndedFfi]
        do {
            claimed = try await onCore { core in try core.claimSessionEnded() }
        } catch {
            lastError = error.localizedDescription
            return
        }
        guard !claimed.isEmpty else { return }
        let delivered = Set(await notifier.postSessionEnded(claimed))
        let failed = claimed.map(\.id).filter { !delivered.contains($0) }
        guard !failed.isEmpty else { return }
        // Its own catch: a failed hand-back leaves the group marked, so it
        // is not posted again rather than posted on every pass.
        do {
            try await onCore { core in
                for id in failed {
                    try core.unclaimSessionEnded(
                        sessionPid: id.sessionPid, sessionProcStart: id.sessionProcStart, atMs: id.atMs
                    )
                }
            }
        } catch {
            lastError = error.localizedDescription
        }
    }

    /// Fetches the favicon of each server that has one and is not cached
    /// yet, on the core queue; forgets the icons of servers gone. A fetch
    /// that finds nothing is tried again on a later pass; bytes that do not
    /// decode are not.
    private func loadIcons(for servers: [ServerViewFfi]) {
        let live = Set(servers.map(\.id))
        icons = icons.filter { live.contains($0.key) }
        iconsUndecodable.formIntersection(live)
        for server in servers where server.hasIcon {
            let id = server.id
            guard icons[id] == nil, !iconsLoading.contains(id), !iconsUndecodable.contains(id) else { continue }
            iconsLoading.insert(id)
            Task {
                let data = try? await onCore { core in core.icon(pid: id.pid, startTime: id.startTime) }
                iconsLoading.remove(id)
                guard let data, lastPassServers.contains(where: { $0.id == id }) else { return }
                if let image = NSImage(data: data) {
                    icons[id] = image
                } else {
                    iconsUndecodable.insert(id)
                }
            }
        }
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
    let memory: MemoryBarFfi
    let leaks: [ServerViewFfi]
}
