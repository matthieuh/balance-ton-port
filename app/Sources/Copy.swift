import Foundation

// The app's words, pure: every user-facing sentence is looked up in the
// String Catalog (`Localizable.xcstrings`) through the `bundle` and `locale`
// it is given, so a test can ask for any language and the app passes the
// chosen one (`LanguageModel`). The defaults are English, from the bundle
// this code is compiled into. Compiled into both the app and
// BalanceTonPortTests.

private final class CopyBundleToken {}

/// The bundle holding the catalog: the English `.lproj` of the bundle this
/// file is compiled into, else that bundle itself.
let copyBundle: Bundle = {
    let base = Bundle(for: CopyBundleToken.self)
    return base.path(forResource: "en", ofType: "lproj").flatMap(Bundle.init(path:)) ?? base
}()

let copyLocale = Locale(identifier: "en")

private func L(_ key: String.LocalizationValue, _ bundle: Bundle, _ locale: Locale) -> String {
    String(localized: key, bundle: bundle, locale: locale)
}

/// `s` kept left to right inside a right-to-left sentence; unchanged in a
/// left-to-right language, so the English copy carries no control characters.
func isolated(_ s: String, _ locale: Locale) -> String {
    layoutDirection(for: locale.identifier) == .rightToLeft ? ltr(s) : s
}

// MARK: - Durations and sizes

/// `<1 min`, `12 min`, `2 h`, `3 d`: a duration as the menu words it.
func durationText(ms: UInt64, bundle: Bundle = copyBundle, locale: Locale = copyLocale) -> String {
    durationText(minutes: ms / 60_000, bundle: bundle, locale: locale)
}

func durationText(minutes min: UInt64, bundle: Bundle = copyBundle, locale: Locale = copyLocale) -> String {
    if min < 1 { return L("<1 min", bundle, locale) }
    if min < 60 { return L("\(Int(min)) min", bundle, locale) }
    if min < 60 * 24 { return L("\(Int(min / 60)) h", bundle, locale) }
    return L("\(Int(min / (60 * 24))) d", bundle, locale)
}

/// `12 min`, with the catalog's plural forms.
func minutes(_ n: Int, bundle: Bundle = copyBundle, locale: Locale = copyLocale) -> String {
    L("\(n) min", bundle, locale)
}

/// `30 s`.
func secondsText(_ n: Int, bundle: Bundle = copyBundle, locale: Locale = copyLocale) -> String {
    L("\(n) s", bundle, locale)
}

/// The `90 MB` / `2.3 GB` size label (same rounding as the core); the units
/// stay Latin and the decimal separator follows `locale`.
func memoryLabel(bytes: UInt64, locale: Locale = copyLocale) -> String {
    let mbUnit: UInt64 = 1_048_576
    let mb = (bytes + mbUnit / 2) / mbUnit
    if mb < 1024 { return "\(mb) MB" }
    return String(format: "%.1f GB", locale: locale, Double(bytes) / Double(mbUnit * 1024))
}

func memoryLabel(_ bytes: UInt64, locale: Locale = copyLocale) -> String {
    memoryLabel(bytes: bytes, locale: locale)
}

/// `2 GB`, `1.5 GB`: the leak line's label.
func leakLineLabel(_ bytes: UInt64, locale: Locale = copyLocale) -> String {
    let gb = Double(bytes) / Double(1024 * 1_048_576)
    return gb.rounded() == gb ? "\(Int(gb)) GB" : String(format: "%.1f GB", locale: locale, gb)
}

/// `CPU 4%`, rounded.
func cpuLabel(_ pct: Float, bundle: Bundle = copyBundle, locale: Locale = copyLocale) -> String {
    L("CPU \(Int(pct.rounded()))%", bundle, locale)
}

// MARK: - Rows and cards

/// `started just now`, `started 4 min ago`.
func startedText(ageMs: UInt64, bundle: Bundle = copyBundle, locale: Locale = copyLocale) -> String {
    if ageMs < 60_000 { return L("started just now", bundle, locale) }
    return L("started \(durationText(ms: ageMs, bundle: bundle, locale: locale)) ago", bundle, locale)
}

/// `in use`, `kept by Claude: "<reason>"`, `kept by you`, `stops in 8 min`,
/// `excluded`, `idle 12 min`.
func statusText(_ s: StatusFfi, bundle: Bundle = copyBundle, locale: Locale = copyLocale) -> String {
    switch s.kind {
    case "in_use": return L("in use", bundle, locale)
    case "kept_by_agent":
        let agent = isolated(s.agentLabel, locale)
        guard let reason = s.reason, !reason.isEmpty else { return L("kept by \(agent)", bundle, locale) }
        return L("kept by \(agent): \"\(reason)\"", bundle, locale)
    case "kept_by_user": return L("kept by you", bundle, locale)
    case "stops_in":
        return L("stops in \(durationText(minutes: s.minutes, bundle: bundle, locale: locale))", bundle, locale)
    case "excluded": return L("excluded", bundle, locale)
    default:
        return L("idle \(durationText(minutes: s.minutes, bundle: bundle, locale: locale))", bundle, locale)
    }
}

/// A card's one-sentence reason. `leakKind` (`size` or `growth`) and
/// `agentLabel` come from the server the card is on.
func cardReason(
    _ card: CardFfi, leakKind: String? = nil, agentLabel: String = "",
    bundle: Bundle = copyBundle, locale: Locale = copyLocale
) -> String {
    switch card.kind {
    case "leak":
        if leakKind == "growth" {
            let grown = memoryLabel(UInt64(max(0, card.growthBytes)), locale: locale)
            return L("Growing fast: +\(grown) in 10 min", bundle, locale)
        }
        return L("Using \(memoryLabel(card.rssBytes, locale: locale))", bundle, locale)
    case "about_to_stop":
        return L(
            "Session ended, unused \u{00B7} stops in \(durationText(ms: card.inMs, bundle: bundle, locale: locale))",
            bundle, locale
        )
    case "session_ended_open":
        return L("Session ended, still open in a browser", bundle, locale)
    default:
        let idle = durationText(ms: card.idleMs, bundle: bundle, locale: locale)
        return L("\(isolated(agentLabel, locale)) session idle \(idle)", bundle, locale)
    }
}

/// The pill on a card: `Restart`, `Postpone 1 h` or `Stop`.
func cardActionLabel(_ card: CardFfi, bundle: Bundle = copyBundle, locale: Locale = copyLocale) -> String {
    switch card.action {
    case "restart": return L("Restart", bundle, locale)
    case "postpone": return L("Postpone 1 h", bundle, locale)
    default: return L("Stop", bundle, locale)
    }
}

/// `✳ Claude`, `◎ Codex`, or `no agent`.
func agentBadgeText(_ s: ServerViewFfi, bundle: Bundle = copyBundle, locale: Locale = copyLocale) -> String {
    switch s.agent {
    case "claude_code": return "\u{2733} Claude"
    case "codex": return "\u{25CE} Codex"
    default: return L("no agent", bundle, locale)
    }
}

/// `active`, `idle 12 min`, or `session dead · kill in 18 min`.
func stateLabel(_ s: ServerViewFfi, bundle: Bundle = copyBundle, locale: Locale = copyLocale) -> String {
    if let killIn = s.killInMs {
        let left = durationText(ms: killIn, bundle: bundle, locale: locale)
        return L("session dead \u{00B7} kill in \(left)", bundle, locale)
    }
    if s.state == "idle" {
        return L("idle \(durationText(ms: s.idleMs, bundle: bundle, locale: locale))", bundle, locale)
    }
    return s.state == "active" ? L("active", bundle, locale) : s.state
}

/// The row's third line. Alive session:
/// `Claude session "revva-b8" · active · started 4 min ago`; due an alert
/// that could not be shown (`notificationsOff`):
/// `Claude session "revva-b8" · idle 31 min, notifications off`; dead
/// session: `Session ended · idle 12 min · kill in 8 min` (`kill soon` under
/// a minute).
func sessionLine(
    _ s: ServerViewFfi, notificationsOff: Bool = false,
    bundle: Bundle = copyBundle, locale: Locale = copyLocale
) -> String {
    let idleText = L(
        "idle \(durationText(ms: s.idleMs, bundle: bundle, locale: locale))", bundle, locale
    )
    var parts: [String]
    if s.sessionAlive {
        let agent = isolated(s.agentLabel, locale)
        let session = isolated("\"\(truncatedSessionName(s.sessionName))\"", locale)
        let name = L("\(agent) session \(session)", bundle, locale)
        if notificationsOff {
            parts = [name, L("\(idleText), notifications off", bundle, locale)]
        } else {
            let state = s.state == "idle" ? idleText : L("active", bundle, locale)
            parts = [name, state, startedText(ageMs: s.startedAgeMs, bundle: bundle, locale: locale)]
        }
    } else {
        parts = [L("Session ended", bundle, locale), idleText]
    }
    if let killIn = s.killInMs {
        if killIn < 60_000 {
            parts.append(L("kill soon", bundle, locale))
        } else {
            parts.append(L("kill in \(durationText(ms: killIn, bundle: bundle, locale: locale))", bundle, locale))
        }
    }
    return parts.joined(separator: " \u{00B7} ")
}

/// A row's status after its badge: the status, plus `notifications off`
/// when its alert is due but could not be shown.
func rowStatus(
    _ s: ServerViewFfi, notificationsOff: Bool = false,
    bundle: Bundle = copyBundle, locale: Locale = copyLocale
) -> String {
    let text = statusText(s.status, bundle: bundle, locale: locale)
    return notificationsOff ? L("\(text), notifications off", bundle, locale) : text
}

/// `just now`, `12 min ago`, `2 h ago`, `yesterday`, `3 days ago`.
func relativeTime(
    atMs: UInt64, now: Date = Date(), bundle: Bundle = copyBundle, locale: Locale = copyLocale
) -> String {
    let nowMs = UInt64(max(0, now.timeIntervalSince1970 * 1000))
    let min = nowMs > atMs ? (nowMs - atMs) / 60_000 : 0
    switch min {
    case 0: return L("just now", bundle, locale)
    case 1..<60: return L("\(Int(min)) min ago", bundle, locale)
    case 60..<(24 * 60): return L("\(Int(min / 60)) h ago", bundle, locale)
    case (24 * 60)..<(48 * 60): return L("yesterday", bundle, locale)
    default: return L("\(Int(min / (24 * 60))) days ago", bundle, locale)
    }
}

/// Why a server was stopped, in the footer's list.
func killReasonText(
    _ r: KillRecordFfi, bundle: Bundle = copyBundle, locale: Locale = copyLocale
) -> String {
    switch r.reasonKind {
    case "worktree_deleted": return L("worktree deleted", bundle, locale)
    case "dead_session_idle":
        return L("dead session + idle \(durationText(ms: r.idleMs, bundle: bundle, locale: locale))", bundle, locale)
    case "manual": return L("manual", bundle, locale)
    case "session_ended": return L("session ended", bundle, locale)
    default: return L("other", bundle, locale)
    }
}

/// An event's label under the chart (`browser connected`, `postponed 1 h`).
func eventLabel(kind: String, bundle: Bundle = copyBundle, locale: Locale = copyLocale) -> String {
    switch kind {
    case "started": return L("started", bundle, locale)
    case "browser_connected": return L("browser connected", bundle, locale)
    case "browser_closed": return L("browser closed", bundle, locale)
    case "session_ended": return L("session ended", bundle, locale)
    case "alert": return L("alert", bundle, locale)
    case "restarted": return L("restarted", bundle, locale)
    case "postponed": return L("postponed 1 h", bundle, locale)
    default: return kind
    }
}

/// An event's label under the chart: `✳ started` for an agent start, else
/// the event's name.
func eventChartLabel(_ e: EventFfi, bundle: Bundle = copyBundle, locale: Locale = copyLocale) -> String {
    let started = eventLabel(kind: "started", bundle: bundle, locale: locale)
    switch (e.kind, e.label) {
    case ("started", "started by Claude"): return "\u{2733} " + started
    case ("started", "started by Codex"): return "\u{25CE} " + started
    default: return eventLabel(kind: e.kind, bundle: bundle, locale: locale)
    }
}

/// Why a restart did not happen, as the row words it.
func restartBlockedText(
    _ b: RestartBlockedFfi, bundle: Bundle = copyBundle, locale: Locale = copyLocale
) -> String {
    switch b {
    case .gone, .spawnFailed: return L("Could not start it again", bundle, locale)
    case .cwdGone: return L("The folder no longer exists", bundle, locale)
    case .envUnreadable, .argvUnreadable: return L("Cannot read its environment", bundle, locale)
    case .portStillTaken: return L("The port is still taken", bundle, locale)
    case .excluded: return L("It matches an exclusion now", bundle, locale)
    case .busy: return L("Already restarting", bundle, locale)
    }
}

func rowNoteText(_ note: RowNote, bundle: Bundle = copyBundle, locale: Locale = copyLocale) -> String {
    switch note {
    case .restarting: return L("restarting\u{2026}", bundle, locale)
    case .blocked(let copy): return copy
    }
}

// MARK: - Menu

/// `NEEDS YOU · 2`, `ALL GOOD · 5` (`kind` is already translated).
func sectionTitle(_ kind: String, count: Int, locale: Locale = copyLocale) -> String {
    "\(kind.uppercased(with: locale)) \u{00B7} \(count)"
}

func nothingToDecideText(bundle: Bundle = copyBundle, locale: Locale = copyLocale) -> String {
    L("Nothing to decide", bundle, locale)
}

var nothingToDecide: String { nothingToDecideText() }

/// The fold under "All good": `+ 2 more ▸`.
func moreLabel(_ n: Int, bundle: Bundle = copyBundle, locale: Locale = copyLocale) -> String {
    L("+ \(n) more \u{25B8}", bundle, locale)
}

/// `Recently stopped (1)`.
func recentlyStoppedLabel(_ n: Int, bundle: Bundle = copyBundle, locale: Locale = copyLocale) -> String {
    L("Recently stopped (\(n))", bundle, locale)
}

/// `7 servers`, `1 server`.
func headerCount(_ n: Int, bundle: Bundle = copyBundle, locale: Locale = copyLocale) -> String {
    L("\(n) servers", bundle, locale)
}

/// The usage bar's caption from the machine's memory.
func usageCaption(_ m: MemoryBarFfi, bundle: Bundle = copyBundle, locale: Locale = copyLocale) -> UsageCaption {
    let gb = { (b: UInt64) in Double(b) / Double(1024 * 1_048_576) }
    let pct = m.totalBytes == 0 ? 0 : Int((Double(m.devBytes) * 100 / Double(m.totalBytes)).rounded())
    let apps = String(format: "%.1f", locale: locale, gb(m.appsBytes))
    let free = String(format: "%.1f", locale: locale, gb(m.freeBytes))
    let total = Int(gb(m.totalBytes).rounded())
    return UsageCaption(
        figure: memoryLabel(m.devBytes, locale: locale),
        dev: L("dev servers (\(pct)%)", bundle, locale),
        apps: L("apps \(apps)", bundle, locale),
        free: L("free \(free) of \(total) GB", bundle, locale)
    )
}

/// `4.8 GB dev servers (13%) · apps 26.8 · free 4.4 of 36 GB`.
func usageLine(_ m: MemoryBarFfi, bundle: Bundle = copyBundle, locale: Locale = copyLocale) -> String {
    let c = usageCaption(m, bundle: bundle, locale: locale)
    return "\(c.figure) \(c.dev) \u{00B7} \(c.apps) \u{00B7} \(c.free)"
}

/// The usage bar's tooltip: exact figures in MB.
func usageHelp(_ m: MemoryBarFfi, bundle: Bundle = copyBundle, locale: Locale = copyLocale) -> String {
    let mb = { (b: UInt64) in "\(b / 1_048_576) MB" }
    return L(
        "Dev servers \(mb(m.devBytes))\nOther apps \(mb(m.appsBytes))\nFree \(mb(m.freeBytes)) of \(mb(m.totalBytes))",
        bundle, locale
    )
}

/// The ⋯ menu, the same everywhere. Restart only when the core can restart
/// it, Keep for agent servers, Ignore this alert with an alert card, Stop
/// unless excluded; neither Restart nor Stop while a Restart runs.
/// Details… opens the detail view (not offered from the detail itself).
func moreMenuItems(
    _ s: ServerViewFfi, restarting: Bool = false, inDetail: Bool = false,
    bundle: Bundle = copyBundle, locale: Locale = copyLocale
) -> [MenuItem] {
    [
        MenuItem(kind: .open, title: "\u{2197} " + L("Open in browser", bundle, locale), enabled: true),
        MenuItem(
            kind: .restart, title: "\u{27F3} " + L("Restart", bundle, locale),
            enabled: canRestartNow(s, restarting: restarting)
        ),
        MenuItem(kind: .keep, title: "\u{23F8} " + L("Keep (no alerts)", bundle, locale), enabled: s.managed),
        MenuItem(
            kind: .ignoreAlert, title: "\u{2298} " + L("Ignore this alert", bundle, locale),
            enabled: hasAlertCard(s)
        ),
        MenuItem(kind: .details, title: "\u{24D8} " + L("Details\u{2026}", bundle, locale), enabled: !inDetail),
        MenuItem(kind: .separator, title: "", enabled: false),
        MenuItem(
            kind: .stop, title: "\u{25A0} " + L("Stop", bundle, locale),
            enabled: canStopNow(s, restarting: restarting)
        ),
    ]
}

/// `Stop client-portal :3000?`, why it asks, and the button.
func confirmationText(
    _ s: ServerViewFfi, action: ConfirmAction, bundle: Bundle = copyBundle, locale: Locale = copyLocale
) -> (title: String, message: String, button: String) {
    let name = isolated(titleParts(s).title, locale)
    let port = isolated(portLabel(s.port), locale)
    let title = action == .stop ? L("Stop \(name) \(port)?", bundle, locale) : L("Restart \(name) \(port)?", bundle, locale)
    return (
        title,
        L("No agent started it. Balance ton port never stops it on its own.", bundle, locale),
        action == .stop ? L("Stop", bundle, locale) : L("Restart", bundle, locale)
    )
}

/// The menu's note after a manual kill that found nothing for `id`. From a
/// notification: the server changed since it was posted (merged into the
/// server that started it, restarted, or gone). From the menu: "already
/// gone" only when no tracked server has that pid; `nil` while one does (its
/// row stays).
func missedKillNotice(
    _ id: ServerID, tracked: [ServerID], fromNotification: Bool,
    bundle: Bundle = copyBundle, locale: Locale = copyLocale
) -> String? {
    if fromNotification {
        return L("Nothing to kill: this server changed since the notification", bundle, locale)
    }
    return tracked.contains { $0.pid == id.pid } ? nil : L("That server was already gone.", bundle, locale)
}

/// The menu's note when a leak notification's Restart finds its server
/// gone from the last pass.
func missedRestartNoticeText(bundle: Bundle = copyBundle, locale: Locale = copyLocale) -> String {
    L("Nothing to restart: this server changed since the notification", bundle, locale)
}

var missedRestartNotice: String { missedRestartNoticeText() }

// MARK: - Detail

extension DetailButton {
    func title(bundle: Bundle = copyBundle, locale: Locale = copyLocale) -> String {
        switch self {
        case .open: return L("Open", bundle, locale)
        case .stop: return L("Stop", bundle, locale)
        case .restart: return L("Restart", bundle, locale)
        case .postpone: return L("Postpone 1 h", bundle, locale)
        case .stopNow: return L("Stop now", bundle, locale)
        }
    }

    var title: String { title() }
}

/// The detail's one sentence: what is going on with this server and why.
/// A leak first, then in use, a server no agent started, kept, its
/// session's state.
func statusSentence(_ s: ServerViewFfi, bundle: Bundle = copyBundle, locale: Locale = copyLocale) -> String {
    let agent = isolated(s.agentLabel, locale)
    if s.card?.kind == "leak" {
        if s.leakKind == "growth" {
            let grown = memoryLabel(UInt64(max(0, s.growthBytes)), locale: locale)
            return L("Growing fast: +\(grown) in 10 min.", bundle, locale)
        }
        return L("Using \(memoryLabel(s.rssBytes, locale: locale)).", bundle, locale)
    }
    if s.inUse { return L("In use: a browser is connected.", bundle, locale) }
    if !s.managed {
        return L("Not started by an agent. Balance ton port never stops it on its own.", bundle, locale)
    }
    switch s.status.kind {
    case "kept_by_user":
        return L("Kept by you. No alerts.", bundle, locale)
    case "kept_by_agent":
        let by = isolated(s.status.agentLabel, locale)
        guard let reason = s.status.reason, !reason.isEmpty else {
            return L("Kept by \(by). No alerts.", bundle, locale)
        }
        return L("Kept by \(by): \"\(reason)\". No alerts.", bundle, locale)
    default: break
    }
    let idle = durationText(ms: s.idleMs, bundle: bundle, locale: locale)
    if !s.sessionAlive {
        return L("Its \(agent) session ended and nothing has connected for \(idle).", bundle, locale)
    }
    if s.card?.kind == "idle_alert" {
        return L("Its \(agent) session has been idle \(idle). Nothing stops it while the session is open.", bundle, locale)
    }
    return L("Its \(agent) session is open.", bundle, locale)
}

/// The status card's bold line when a stop is scheduled: `Stops in 8 min`.
func statusTitle(_ s: ServerViewFfi, bundle: Bundle = copyBundle, locale: Locale = copyLocale) -> String? {
    guard let killIn = s.killInMs else { return nil }
    if killIn < 60_000 { return L("Stops soon", bundle, locale) }
    return L("Stops in \(durationText(ms: killIn, bundle: bundle, locale: locale))", bundle, locale)
}

/// `up 5 h`.
func uptimeChip(ms: UInt64, bundle: Bundle = copyBundle, locale: Locale = copyLocale) -> String {
    L("up \(durationText(ms: ms, bundle: bundle, locale: locale))", bundle, locale)
}

/// `3 processes ›`.
func processesLabel(_ n: Int, bundle: Bundle = copyBundle, locale: Locale = copyLocale) -> String {
    L("\(n) processes \u{203A}", bundle, locale)
}

/// What the session chip says for a moment after the copy.
func copiedText(bundle: Bundle = copyBundle, locale: Locale = copyLocale) -> String {
    L("Copied", bundle, locale)
}

var copiedLabel: String { copiedText() }

// MARK: - Notifications

/// "Astro on port 4322 (revva › web) hasn't been used for 30 min."
/// Only the tool label, the port and the location: the raw command line
/// (flags, tokens) never reaches the lock screen. Minutes are rounded down.
func notificationBody(_ s: ServerViewFfi, bundle: Bundle = copyBundle, locale: Locale = copyLocale) -> String {
    let tool = isolated(s.toolLabel, locale)
    let port = isolated(String(s.port), locale)
    let place = isolated(s.locationLabel, locale)
    let idle = minutes(Int(clamping: s.idleMs / 60_000), bundle: bundle, locale: locale)
    return L("\(tool) on port \(port) (\(place)) hasn't been used for \(idle).", bundle, locale)
}

/// `Revva web :3010 is using 2.3 GB`, or `Vite :5173 grew 600 MB in 10 min`
/// when the core says the leak is the growth (`leakKind`).
func leakNotificationBody(_ s: ServerViewFfi, bundle: Bundle = copyBundle, locale: Locale = copyLocale) -> String {
    let name = isolated("\(titleParts(s).title) \(portLabel(s.port))", locale)
    if s.leakKind == "growth" {
        let grown = memoryLabel(UInt64(max(0, s.growthBytes)), locale: locale)
        return L("\(name) grew \(grown) in 10 min", bundle, locale)
    }
    return L("\(name) is using \(memoryLabel(s.rssBytes, locale: locale))", bundle, locale)
}

/// The session-ended notification: `Session ended: 2 servers kept running:
/// Vite :5173, Bun dev :3000. Stopped after 20 min unused, once no session
/// works in that folder.` It names the group's tracked servers, at most four.
func sessionEndedBody(
    _ g: SessionEndedFfi, bundle: Bundle = copyBundle, locale: Locale = copyLocale
) -> String {
    let shown = g.servers.prefix(4).map { isolated("\($0.toolLabel) :\($0.port)", locale) }
    var names = shown.joined(separator: ", ")
    if g.servers.count > 4 {
        names = L("\(names) and \(g.servers.count - 4) more", bundle, locale)
    }
    let servers = headerCount(g.servers.count, bundle: bundle, locale: locale)
    let delay = durationText(ms: g.deadIdleS.multipliedReportingOverflow(by: 1000).partialValue, bundle: bundle, locale: locale)
    return L(
        "Session ended: \(servers) kept running: \(names). Stopped after \(delay) unused, once no session works in that folder.",
        bundle, locale
    )
}

/// The test notification's body: a made-up idle server.
func sampleNotificationText(bundle: Bundle = copyBundle, locale: Locale = copyLocale) -> String {
    let idle = minutes(30, bundle: bundle, locale: locale)
    return L("Test: Vite on port 5173 (my-project \u{203A} web) hasn't been used for \(idle).", bundle, locale)
}

var sampleNotificationBody: String { sampleNotificationText() }

/// The notification buttons' titles (the idle and leak categories, and the
/// session-ended one).
enum NotificationButtonTitle {
    case kill, keep, stopAll, restart, ignore

    func text(bundle: Bundle = copyBundle, locale: Locale = copyLocale) -> String {
        switch self {
        case .kill: return L("Stop", bundle, locale)
        case .keep: return L("Keep", bundle, locale)
        case .stopAll: return L("Stop all", bundle, locale)
        case .restart: return L("Restart", bundle, locale)
        case .ignore: return L("Ignore", bundle, locale)
        }
    }
}

// MARK: - Settings

extension NotificationSetupButton {
    func title(bundle: Bundle = copyBundle, locale: Locale = copyLocale) -> String {
        switch self {
        case .turnOn: return L("Turn on notifications", bundle, locale)
        case .openSettings: return L("Open Notification Settings", bundle, locale)
        }
    }

    var title: String { title() }
}

/// The Notifications section's content for a permission.
func notificationSetup(
    _ permission: NotificationPermission, bundle: Bundle = copyBundle, locale: Locale = copyLocale
) -> NotificationSetup {
    switch permission {
    case .notAsked:
        return NotificationSetup(
            message: L(
                "Get a notification for servers that sit idle and for sessions that end with servers still running, with buttons to stop or keep them.",
                bundle, locale
            ),
            isOn: false, hint: nil, button: .turnOn, canSendTest: false
        )
    case .allowed(let bannersOff):
        return NotificationSetup(
            message: L("Notifications are on", bundle, locale),
            isOn: true,
            hint: bannersOff
                ? L("Banners are off: choose Banners or Alerts in System Settings", bundle, locale) : nil,
            button: bannersOff ? .openSettings : nil,
            canSendTest: true
        )
    case .denied:
        return NotificationSetup(
            message: L("Notifications are off for Balance ton port.", bundle, locale),
            isOn: false,
            hint: L("Turn on Allow notifications, then come back here.", bundle, locale),
            button: .openSettings,
            canSendTest: false
        )
    }
}

/// The test button's title: "Test sent" only once the notification was added.
func testNotificationButtonTitle(
    _ result: TestNotificationResult, bundle: Bundle = copyBundle, locale: Locale = copyLocale
) -> String {
    switch result {
    case .none: return L("Send a test notification", bundle, locale)
    case .sent: return L("Test sent", bundle, locale)
    case .failed: return L("Could not send the test", bundle, locale)
    }
}

extension SettingsDraft {
    /// What stops the draft being saved: minute delays at least 1 min,
    /// second delays at least 1 s.
    func validationMessages(bundle: Bundle = copyBundle, locale: Locale = copyLocale) -> [String] {
        var errors: [String] = []
        if deadSessionIdleS < 60 { errors.append(L("Dead session delay must be at least 1 min.", bundle, locale)) }
        if alertAfterS < 60 { errors.append(L("Alert delay must be at least 1 min.", bundle, locale)) }
        if killGraceS < 1 { errors.append(L("Kill grace must be at least 1 s.", bundle, locale)) }
        if scanIntervalS < 1 { errors.append(L("Scan interval must be at least 1 s.", bundle, locale)) }
        return errors
    }
}
