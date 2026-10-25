import Foundation
import UserNotifications

// Pure helpers: no UI, no notification center, no core queue. Compiled into
// both the app and BalanceTonPortTests.

/// A server's identity: a pid alone can be reused by a later process.
struct ServerID: Hashable, Sendable {
    let pid: Int32
    let startTime: UInt64
}

extension ServerViewFfi {
    var id: ServerID { ServerID(pid: pid, startTime: startTime) }
}

// MARK: - Menu labels

/// The menu panel's width and padding, and the indent of a card's second
/// and third lines (past the 22 pt icon and its 8 pt gap).
let panelWidth: CGFloat = 460
let panelPadding: CGFloat = 14
let cardIndent: CGFloat = 30

/// `:8081`. Always the bare digits: a port is not a quantity, so no
/// locale grouping (SwiftUI's `Text("\(port)")` would show `8 081`).
func portLabel(_ port: UInt16) -> String {
    ":" + String(port)
}

/// Session names longer than this are cut with an ellipsis in the menu.
let sessionNameMaxLength = 20

/// `name`, or its first characters plus "\u{2026}" when longer than
/// `sessionNameMaxLength` (the result is never longer than that).
func truncatedSessionName(_ name: String) -> String {
    guard name.count > sessionNameMaxLength else { return name }
    return String(name.prefix(sessionNameMaxLength - 1)) + "\u{2026}"
}

/// The row tooltip: full command line, pid and full worktree path.
func rowHelp(_ s: ServerViewFfi) -> String {
    "\(s.command)\npid \(s.pid)\n\(s.worktree)"
}

// MARK: - Menu (decision first)

/// "All good" rows shown before the `+ N more` fold.
let allGoodVisibleRows = 3

/// Card action pills and row capsule buttons: 26 to 28 pt high, pressed at
/// 96 %.
let actionButtonHeight: CGFloat = 27
let pressScale: CGFloat = 0.96

/// The radius of a shape inset by `padding` in one of radius `outer`, so
/// both curves share a centre; never under 4 pt.
func concentricRadius(outer: CGFloat, padding: CGFloat) -> CGFloat {
    max(4, outer - padding)
}

/// The menu panel's corner radius, and the radii that nest in it.
let panelRadius: CGFloat = 16
let cardRadius = concentricRadius(outer: panelRadius, padding: 4)
let rowRadius = concentricRadius(outer: panelRadius, padding: 6)
let iconRadius = concentricRadius(outer: rowRadius, padding: 4)

/// The tallest the scrolling middle of the menu (cards and rows) may be on
/// a screen whose visible height is `screenHeight`: the rest is left to the
/// header, the footer and a margin. Never under 200 pt.
func sectionsMaxHeight(screenHeight: CGFloat) -> CGFloat {
    max(200, screenHeight - 220)
}

/// The menu's lists: servers that need a decision (one card each), the
/// others, of which the first `visibleAllGood` show before the fold, and the
/// quiet listeners folded under "Other ports".
struct MenuSections {
    let needsYou: [ServerViewFfi]
    let allGood: [ServerViewFfi]
    let hiddenCount: Int
    let otherPorts: [ServerViewFfi]
}

/// Under this, a listener with no agent and no known tool (ssh, adb, a
/// tunnel) waits under "Other ports"; past it, it is back in the list.
let quietListenerMaxBytes: UInt64 = 100 * 1_048_576

func isQuietListener(_ s: ServerViewFfi) -> Bool {
    !s.managed && s.agent.isEmpty && (s.framework ?? "").isEmpty && s.rssBytes < quietListenerMaxBytes
}

/// Memory in 64 MB steps: rows sort by it, so two servers of about the same
/// size do not swap at every pass.
private func memoryStep(_ s: ServerViewFfi) -> UInt64 {
    s.rssBytes / (64 * 1_048_576)
}

/// Cards: leak, session ended but open, about to stop, idle alert, then the
/// biggest first. All good: the biggest first (in 64 MB steps); quiet
/// listeners go to Other ports, by port. The port (then the pid) breaks
/// every tie, so rows do not swap between passes.
func menuSections(_ servers: [ServerViewFfi], visibleAllGood: Int = allGoodVisibleRows) -> MenuSections {
    let cards = servers.filter { $0.card != nil }.sorted {
        let (a, b) = (cardRank($0.card?.kind), cardRank($1.card?.kind))
        if a != b { return a < b }
        if $0.rssBytes != $1.rssBytes { return $0.rssBytes > $1.rssBytes }
        return ($0.port, $0.pid) < ($1.port, $1.pid)
    }
    let byPort: (ServerViewFfi, ServerViewFfi) -> Bool = { ($0.port, $0.pid) < ($1.port, $1.pid) }
    let others = servers.filter { $0.card == nil && isQuietListener($0) }.sorted(by: byPort)
    let rest = servers.filter { $0.card == nil && !isQuietListener($0) }.sorted {
        let (a, b) = (memoryStep($0), memoryStep($1))
        return a != b ? a > b : byPort($0, $1)
    }
    let shown = Array(rest.prefix(max(0, visibleAllGood)))
    return MenuSections(needsYou: cards, allGood: shown, hiddenCount: rest.count - shown.count, otherPorts: others)
}

private func cardRank(_ kind: String?) -> Int {
    switch kind {
    case "leak": return 0
    case "session_ended_open": return 1
    case "about_to_stop": return 2
    case "idle_alert": return 3
    default: return 4
    }
}

/// A card's pill is filled (amber) only for Restart, the fix for a leak.
func cardActionIsProminent(_ card: CardFfi) -> Bool {
    card.action == "restart"
}

/// A card's pill can act: Restart and Stop as in the ⋯ menu, Postpone only
/// for an agent server.
func cardActionEnabled(_ card: CardFfi, _ s: ServerViewFfi, restarting: Bool = false) -> Bool {
    switch card.action {
    case "restart": return canRestartNow(s, restarting: restarting)
    case "postpone": return s.managed
    default: return canStopNow(s, restarting: restarting)
    }
}

/// A port's hue in 0..<1: the golden-ratio step from the port number, so
/// neighbours differ and a port keeps its color across launches.
func portHue(_ port: UInt16) -> Double {
    (Double(port) * 0.618_033_988_75).truncatingRemainder(dividingBy: 1)
}

/// The parts of a row's second line, so each can take its own color and
/// its own truncation.
struct SubtitleParts: Equatable {
    /// `:3010`
    let port: String
    /// `+9229`, or `+9229 +4` past one extra port; `nil` with a single port.
    let otherPorts: String?
    /// The branch, else the project folder; `nil` when neither is known.
    let place: String?
    let badge: String
}

func subtitleParts(_ s: ServerViewFfi, bundle: Bundle = copyBundle, locale: Locale = copyLocale) -> SubtitleParts {
    let others = s.ports.filter { $0 != s.port }
    var extra: String?
    if let first = others.first {
        extra = "+" + String(first) + (others.count > 1 ? " +\(others.count - 1)" : "")
    }
    let place = [s.branch ?? "", s.worktreeName].first { !$0.isEmpty }
    return SubtitleParts(port: portLabel(s.port), otherPorts: extra, place: place,
                         badge: agentBadgeText(s, bundle: bundle, locale: locale))
}

/// `:3010 +9229 · roadbook-explore · ✳ Claude`.
func rowSubtitle(_ s: ServerViewFfi, bundle: Bundle = copyBundle, locale: Locale = copyLocale) -> String {
    let p = subtitleParts(s, bundle: bundle, locale: locale)
    let port = [p.port, p.otherPorts].compactMap { $0 }.joined(separator: " ")
    return [port, p.place, p.badge].compactMap { $0 }.joined(separator: " \u{00B7} ")
}

/// `chore/node-sec…oct-2026`: `s` cut in the middle to at most `max`
/// characters, the ellipsis included.
func middleTruncated(_ s: String, max: Int) -> String {
    guard s.count > max, max > 1 else { return s }
    let keep = max - 1
    return String(s.prefix((keep + 1) / 2)) + "\u{2026}" + String(s.suffix(keep / 2))
}

/// The row's title and its framework, the framework left out when it only
/// repeats the title (`Vite · Vite`).
func titleParts(_ s: ServerViewFfi) -> (title: String, framework: String?) {
    let title = s.title.isEmpty ? s.toolLabel : s.title
    guard let f = s.framework, !f.isEmpty, f.lowercased() != title.lowercased() else {
        return (title, nil)
    }
    return (title, f)
}

/// How full a row's memory gauge is: the share of 2 GB (the red line),
/// at least 3 % so a small server still shows a tick.
func gaugeFraction(_ bytes: UInt64) -> Double {
    min(1, max(0.03, Double(bytes) / Double(2 * 1024 * 1_048_576)))
}

/// The usage bar's segments, as shares of the machine's memory: dev servers
/// split by health, then every other app. Free memory is what is left.
struct UsageSegments: Equatable {
    let green: Double
    let amber: Double
    let red: Double
    let apps: Double
}

func usageSegments(_ memory: MemoryBarFfi, servers: [ServerViewFfi]) -> UsageSegments {
    guard memory.totalBytes > 0 else { return UsageSegments(green: 0, amber: 0, red: 0, apps: 0) }
    let total = Double(memory.totalBytes)
    func share(_ health: String) -> Double {
        Double(servers.filter { $0.health == health }.reduce(UInt64(0)) { $0 + $1.rssBytes }) / total
    }
    return UsageSegments(
        green: share("green"), amber: share("amber"), red: share("red"),
        apps: Double(memory.appsBytes) / total
    )
}

/// The core's usage line cut for the bar's caption: the bold figure, the
/// rest of the left half, then the apps and free halves on the right.
struct UsageCaption: Equatable {
    /// `4.8 GB`
    let figure: String
    /// `dev servers (13%)`
    let dev: String
    /// `apps 26.8`
    let apps: String
    /// `free 4.4 of 36 GB`
    let free: String
}

/// `nil` when the line is not in the core's shape.
func usageCaption(_ line: String) -> UsageCaption? {
    let halves = line.components(separatedBy: " \u{00B7} ")
    guard halves.count == 3 else { return nil }
    let words = halves[0].split(separator: " ", maxSplits: 2).map(String.init)
    guard words.count == 3 else { return nil }
    return UsageCaption(figure: "\(words[0]) \(words[1])", dev: words[2], apps: halves[1], free: halves[2])
}

// MARK: - Menu actions

/// What a ⋯ menu item does; `separator` is the line before Stop.
enum MenuItemKind: Hashable, Sendable {
    case open, restart, keep, ignoreAlert, details, separator, stop
}

struct MenuItem: Equatable, Sendable {
    let kind: MenuItemKind
    let title: String
    let enabled: Bool
}

/// A card that is an alert, which "Ignore this alert" dismisses: a leak or
/// an idle alert (the core refuses the others).
func hasAlertCard(_ s: ServerViewFfi) -> Bool {
    s.card?.kind == "leak" || s.card?.kind == "idle_alert"
}

/// Stop is offered: not excluded, and no Restart of it running.
func canStopNow(_ s: ServerViewFfi, restarting: Bool) -> Bool {
    !s.excluded && !restarting
}

/// Restart is offered: the core can restart it, and none is running.
func canRestartNow(_ s: ServerViewFfi, restarting: Bool) -> Bool {
    s.canRestart && !restarting
}

/// The hover capsule's buttons. Restart is never one of them.
enum CapsuleButton: Hashable, Sendable {
    case open, stop, more
}

func capsuleButtons(_ s: ServerViewFfi) -> [CapsuleButton] {
    s.excluded ? [.open, .more] : [.open, .stop, .more]
}

/// `http://localhost:3000/`.
func openURL(_ s: ServerViewFfi) -> URL {
    URL(string: "http://localhost:\(String(s.port))/")!
}

enum ConfirmAction: Equatable, Sendable {
    case stop, restart
}

/// Stop and Restart ask first only for a server no agent started.
func needsConfirmation(_ s: ServerViewFfi) -> Bool {
    !s.managed
}

/// What the row shows in place of its status while a Restart runs, and
/// after one that was refused (for `blockedNoteDuration`).
enum RowNote: Equatable, Sendable {
    case restarting
    case blocked(String)
}

let blockedNoteDuration: TimeInterval = 4

/// A Restart running or just done, with the row as it was. The core
/// returns once the new process is started, not once it listens, so the row
/// is held, saying "restarting…", until a pass lists its replacement.
struct HeldRestart: Sendable {
    let row: ServerViewFfi
    /// The pass count and time when the core returned; `nil` while it runs.
    var done: (pass: Int, at: Date)?
}

/// How long a done Restart's row waits for its replacement (a new process
/// that never listens, or one the core no longer links).
let restartHoldLimit: TimeInterval = 60

/// Whether `server` is the process a Restart of `row` started: another
/// process on the same port, or with the same command in the same folder.
func isReplacement(_ server: ServerViewFfi, of row: ServerViewFfi) -> Bool {
    server.id != row.id
        && (server.port == row.port || (server.command == row.command && server.worktree == row.worktree))
}

/// Whether the pass `generation`, landing at `now` with `servers`, lets a
/// held Restart row go: only a pass enqueued after the core returned, once
/// it lists the replacement or `restartHoldLimit` passed.
func releasesRestart(_ held: HeldRestart, generation: Int, now: Date, servers: [ServerViewFfi]) -> Bool {
    guard let done = held.done, generation > done.pass else { return false }
    return servers.contains { isReplacement($0, of: held.row) }
        || now.timeIntervalSince(done.at) >= restartHoldLimit
}

/// The pass's rows plus the held Restart rows it no longer lists.
func withHeldRestarts(_ servers: [ServerViewFfi], held: [HeldRestart]) -> [ServerViewFfi] {
    let listed = Set(servers.map(\.id))
    return servers + held.map(\.row).filter { !listed.contains($0.id) }
}

/// The row right after "Postpone 1 h", until the next pass: one hour left,
/// and no "about to stop" card any more.
func postponedView(_ s: ServerViewFfi) -> ServerViewFfi {
    var v = s
    v.killInMs = 3_600_000
    v.postponed = true
    v.statusText = "stops in 60 min"
    v.status = StatusFfi(kind: "stops_in_min", minutes: 60, agentLabel: "", reason: nil)
    if v.card?.kind == "about_to_stop" { v.card = nil }
    return v
}

/// The row right after Keep: no countdown, and only a leak or a session
/// ended but open still shows a card.
func keptView(_ s: ServerViewFfi) -> ServerViewFfi {
    var v = s
    v.killInMs = nil
    v.statusText = "kept by you"
    v.status = StatusFfi(kind: "kept_by_user", minutes: 0, agentLabel: "", reason: nil)
    if v.card?.kind != "leak" && v.card?.kind != "session_ended_open" { v.card = nil }
    return v
}

/// The row right after "Ignore this alert": its alert card goes.
func alertIgnoredView(_ s: ServerViewFfi) -> ServerViewFfi {
    var v = s
    if hasAlertCard(v) { v.card = nil }
    return v
}

// MARK: - Detail

/// A button of the detail view: the two at the bottom and the status card's.
enum DetailButton: Hashable, Sendable {
    case open, stop, restart, postpone, stopNow
}

/// How much of the countdown is left, 0 to 1; `nil` when no stop is
/// scheduled.
func countdownFraction(killInMs: UInt64?, totalMs: UInt64) -> Double? {
    guard let killInMs, totalMs > 0 else { return nil }
    return min(1, Double(killInMs) / Double(totalMs))
}

/// The countdown's full length: one hour after Postpone 1 h, else the dead
/// session delay.
func countdownTotalMs(_ s: ServerViewFfi, deadSessionIdleS: UInt64) -> UInt64 {
    s.postponed ? 3_600_000 : deadSessionIdleS * 1000
}

/// About to stop: Postpone 1 h, Stop now. Leak: Restart, Stop. Otherwise
/// (in use included): Open, Stop.
func bottomButtons(_ s: ServerViewFfi) -> [DetailButton] {
    switch s.card?.kind {
    case "leak": return [.restart, .stop]
    case "about_to_stop": return [.postpone, .stopNow]
    default: return [.open, .stop]
    }
}

/// The status card's own button: Postpone 1 h when about to stop, Restart
/// for a leak.
func statusCardAction(_ s: ServerViewFfi) -> DetailButton? {
    switch s.card?.kind {
    case "about_to_stop": return .postpone
    case "leak": return .restart
    default: return nil
    }
}

/// The menu's rules: Stop unless excluded, Restart when the core can,
/// neither while a Restart runs, Postpone for an agent server.
func detailButtonEnabled(_ b: DetailButton, _ s: ServerViewFfi, restarting: Bool) -> Bool {
    switch b {
    case .open: return true
    case .stop, .stopNow: return canStopNow(s, restarting: restarting)
    case .restart: return canRestartNow(s, restarting: restarting)
    case .postpone: return s.managed
    }
}

/// The session chip: `✳ "revva-b8" ↗`. A click copies the resume command.
func sessionChipText(_ s: ServerViewFfi) -> String {
    let glyph = s.agent == "codex" ? "\u{25CE}" : "\u{2733}"
    return "\(glyph) \"\(truncatedSessionName(s.sessionName))\" \u{2197}"
}

/// How long the session chip says "Copied".
let copiedDuration: TimeInterval = 1.5

/// The hero's second line: the framework (else the tool, when it is not
/// the title), then the ports.
func heroSubtitle(
    _ s: ServerViewFfi, bundle: Bundle = copyBundle, locale: Locale = copyLocale
) -> (kind: String?, port: String, otherPorts: String?) {
    let t = titleParts(s)
    let kind = t.framework ?? (s.toolLabel.lowercased() == t.title.lowercased() ? nil : s.toolLabel)
    let p = subtitleParts(s, bundle: bundle, locale: locale)
    return (kind, p.port, p.otherPorts)
}

/// Which event labels fit under the chart: `positions` are 0 to 1 across
/// it, oldest first. The latest are kept; one closer than `minGap` to a
/// kept one is drawn without its label. Indices, oldest first.
func labelledEvents(at positions: [Double], minGap: Double) -> [Int] {
    var kept: [Int] = []
    for i in positions.indices.reversed() where kept.allSatisfy({ abs(positions[$0] - positions[i]) >= minGap }) {
        kept.append(i)
    }
    return kept.reversed()
}

/// The chart's top, in bytes, and whether the leak line is drawn to scale.
/// A small server is not flattened under a line far above it: the line is
/// then pinned to the top and labelled as above.
func chartScale(maxBytes: UInt64, lineBytes: UInt64) -> (upper: Double, lineOnScale: Bool) {
    let upper = max(Double(maxBytes) * 1.25, 64 * 1_048_576)
    let line = Double(lineBytes)
    guard line <= upper * 2 else { return (upper, false) }
    return (max(upper, line * 1.1), true)
}

/// The tallest the detail may be on a screen whose visible height is
/// `screenHeight`; it scrolls past that. Never under 300 pt.
func detailMaxHeight(screenHeight: CGFloat) -> CGFloat {
    max(300, screenHeight - 40)
}

/// At most about `maxPoints` samples for the chart: the samples are cut
/// into equal buckets of time order, and each bucket keeps its lowest and
/// its highest sample (in time order), so a spike stays visible.
func downsample(_ samples: [SampleFfi], maxPoints: Int = 200) -> [SampleFfi] {
    guard maxPoints >= 2, samples.count > maxPoints else { return samples }
    let buckets = maxPoints / 2
    var out: [SampleFfi] = []
    out.reserveCapacity(buckets * 2)
    for b in 0..<buckets {
        let lo = samples.count * b / buckets, hi = samples.count * (b + 1) / buckets
        guard lo < hi else { continue }
        let slice = samples[lo..<hi]
        let minI = slice.indices.min { slice[$0].rssBytes < slice[$1].rssBytes }!
        let maxI = slice.indices.max { slice[$0].rssBytes < slice[$1].rssBytes }!
        for i in Set([minI, maxI]).sorted() { out.append(samples[i]) }
    }
    return out
}

// MARK: - Page preview

/// A live preview is taken only for a server a browser already uses: the
/// preview never wakes an idle one.
func shouldSnapshot(_ s: ServerViewFfi) -> Bool {
    s.inUse
}

/// How long a preview is kept for one server (pid and start time).
let previewCacheTTL: TimeInterval = 180

func previewCacheFresh(takenAt: Date, now: Date = Date()) -> Bool {
    now.timeIntervalSince(takenAt) < previewCacheTTL
}

/// The page the preview loads.
func previewURL(port: UInt16) -> URL {
    URL(string: "http://127.0.0.1:\(String(port))/")!
}

/// The preview may only go to the server itself: plain http on loopback
/// (`localhost` or `127.0.0.1`), on its own port.
func previewAllows(_ url: URL, port: UInt16) -> Bool {
    guard url.scheme?.lowercased() == "http", let host = url.host?.lowercased(), url.port == Int(port) else {
        return false
    }
    return host == "localhost" || host == "127.0.0.1"
}

/// The one identifier the preview's rule list is stored under: compiling
/// for another port replaces it, so the store holds a single list.
let previewRuleListID = "btp-preview"

/// Run in the preview's main frame at document start, before the page's own
/// scripts: turns DNS prefetching off, removes resource hints (preconnect,
/// dns-prefetch, prefetch, preload, prerender, modulepreload) that point
/// anywhere but the server's own origin, now and as they are added, and
/// takes service workers away (WKWebView has no switch for them). Content
/// rules block requests but not a hint's bare connection or lookup.
func previewUserScript(port: UInt16) -> String {
    let p = String(port)
    return """
    (function () {
      var allowed = ["http://localhost:\(p)", "http://127.0.0.1:\(p)"];
      var hints = /(^|\\s)(preconnect|dns-prefetch|prefetch|preload|prerender|modulepreload)(\\s|$)/i;
      function ok(href) {
        try { return allowed.indexOf(new URL(href, location.href).origin) >= 0; } catch (e) { return false; }
      }
      function clean(node) {
        if (node.nodeType !== 1) return;
        var links = node.matches && node.matches("link") ? [node] : [];
        if (node.querySelectorAll) links = links.concat(Array.prototype.slice.call(node.querySelectorAll("link")));
        links.forEach(function (l) {
          if (hints.test(l.getAttribute("rel") || "") && !ok(l.getAttribute("href") || "")) l.remove();
        });
      }
      var meta = document.createElement("meta");
      meta.httpEquiv = "x-dns-prefetch-control";
      meta.content = "off";
      (document.head || document.documentElement).appendChild(meta);
      new MutationObserver(function (records) {
        records.forEach(function (r) { r.addedNodes.forEach(clean); });
      }).observe(document.documentElement, { childList: true, subtree: true });
      try {
        Object.defineProperty(navigator, "serviceWorker", { get: function () { return undefined; } });
      } catch (e) {}
    })();
    """
}

/// A content rule list that blocks every load (subresources included)
/// except the server's own origin on loopback.
func previewContentRules(port: UInt16) -> String {
    let p = String(port)
    return """
    [{"trigger":{"url-filter":".*"},"action":{"type":"block"}},\
    {"trigger":{"url-filter":"^http://localhost:\(p)/"},"action":{"type":"ignore-previous-rules"}},\
    {"trigger":{"url-filter":"^http://127\\\\.0\\\\.0\\\\.1:\(p)/"},"action":{"type":"ignore-previous-rules"}}]
    """
}

// MARK: - Notifications

/// The leak notification's identifier: a repeat replaces the earlier one.
func leakRequestID(_ id: ServerID) -> String {
    "leak-\(id.pid)-\(id.startTime)"
}

/// What a notification carries to find its server again. The start time is a
/// string: property lists have no unsigned 64-bit integer.
func notificationUserInfo(_ id: ServerID) -> [String: Any] {
    ["pid": Int(id.pid), "startTime": String(id.startTime)]
}

func serverID(fromUserInfo info: [AnyHashable: Any]) -> ServerID? {
    guard let pid = (info["pid"] as? NSNumber)?.int32Value,
          let startText = info["startTime"] as? String,
          let start = UInt64(startText)
    else { return nil }
    return ServerID(pid: pid, startTime: start)
}

// MARK: - Session ended

/// An ended Claude Code session's group: the process (pid and start time)
/// and when it ended.
struct SessionEndedID: Hashable, Sendable {
    let sessionPid: Int32
    let sessionProcStart: String
    let atMs: UInt64
}

extension SessionEndedFfi {
    var id: SessionEndedID {
        SessionEndedID(sessionPid: sessionPid, sessionProcStart: sessionProcStart, atMs: atMs)
    }
}

/// The notification identifier: a repeat replaces the earlier one.
func sessionEndedRequestID(_ id: SessionEndedID) -> String {
    "session-ended-\(id.sessionPid)-\(id.atMs)"
}

/// What a session-ended notification carries to find its group again. Times
/// are strings: property lists have no unsigned 64-bit integer.
func sessionEndedUserInfo(_ id: SessionEndedID) -> [String: Any] {
    [
        "sessionEnded": true,
        "sessionPid": Int(id.sessionPid),
        "sessionProcStart": id.sessionProcStart,
        "atMs": String(id.atMs),
    ]
}

func sessionEndedID(fromUserInfo info: [AnyHashable: Any]) -> SessionEndedID? {
    guard info["sessionEnded"] as? Bool == true,
          let pid = (info["sessionPid"] as? NSNumber)?.int32Value,
          let start = info["sessionProcStart"] as? String, !start.isEmpty,
          let atText = info["atMs"] as? String,
          let at = UInt64(atText)
    else { return nil }
    return SessionEndedID(sessionPid: pid, sessionProcStart: start, atMs: at)
}

// MARK: - Notification buttons

/// The idle-server (Kill, Keep) and leak (Restart, Ignore) notifications'
/// buttons.
enum NotificationAction: Sendable, Equatable {
    case kill
    case keep
    case restart
    case ignoreAlert
}

/// The session-ended notification's buttons.
enum SessionEndedAction: Sendable, Equatable {
    /// Stop every kept server of the session now.
    case stopNow
    /// Keep on each of them.
    case keep
}

/// Category and action identifiers registered with Notification Center.
enum NotificationIDs {
    static let idleCategory = "IDLE_SERVER"
    static let sessionEndedCategory = "SESSION_ENDED"
    static let leakCategory = "LEAK"
    static let kill = "KILL"
    static let keep = "KEEP"
    static let stopNow = "STOP_NOW"
    static let restart = "RESTART"
    static let ignore = "IGNORE"
}

/// Where a pressed notification button goes.
enum NotificationRoute: Equatable, Sendable {
    case server(NotificationAction, ServerID)
    case sessionEnded(SessionEndedAction, SessionEndedID)
}

/// The route for a button press; `nil` for a tap on the body, a dismiss, the
/// test notification, or a userInfo that names nothing.
func notificationRoute(actionIdentifier: String, userInfo info: [AnyHashable: Any]) -> NotificationRoute? {
    if isSampleNotification(info) { return nil }
    if let group = sessionEndedID(fromUserInfo: info) {
        switch actionIdentifier {
        case NotificationIDs.stopNow: return .sessionEnded(.stopNow, group)
        case NotificationIDs.keep: return .sessionEnded(.keep, group)
        default: return nil
        }
    }
    guard let id = serverID(fromUserInfo: info) else { return nil }
    switch actionIdentifier {
    case NotificationIDs.kill: return .server(.kill, id)
    case NotificationIDs.keep: return .server(.keep, id)
    case NotificationIDs.restart: return .server(.restart, id)
    case NotificationIDs.ignore: return .server(.ignoreAlert, id)
    default: return nil
    }
}

// MARK: - Notification permission

/// What macOS lets the app do with notifications, as the Settings section
/// and the menu show it.
enum NotificationPermission: Equatable, Sendable {
    /// The system has not asked the user yet.
    case notAsked
    /// Allowed; `bannersOff` when the alert style is None, so nothing pops up.
    case allowed(bannersOff: Bool)
    /// Refused (or the prompt was closed): only System Settings can fix it.
    case denied

    init(status: UNAuthorizationStatus, alertStyle: UNAlertStyle) {
        switch status {
        case .notDetermined:
            self = .notAsked
        case .authorized, .provisional, .ephemeral:
            self = .allowed(bannersOff: alertStyle == .none)
        case .denied:
            self = .denied
        @unknown default:
            self = .denied
        }
    }

    var isAllowed: Bool {
        if case .allowed = self { return true }
        return false
    }
}

/// The one button the Notifications section shows under its text.
enum NotificationSetupButton: Equatable, Sendable {
    case turnOn
    case openSettings
}

/// The Notifications section's content for a permission.
struct NotificationSetup: Equatable, Sendable {
    /// Main line; shown with a green check when `isOn`.
    let message: String
    let isOn: Bool
    /// A second, secondary line (what to do in System Settings).
    let hint: String?
    let button: NotificationSetupButton?
    /// "Send a test notification" is offered.
    let canSendTest: Bool
}

/// What "Turn on" in the menu does; `nil` when the menu shows no line.
enum MenuNotificationFix: Equatable, Sendable {
    /// Ask macOS right away (it has never asked).
    case request
    /// Open the Settings window, whose Notifications section is on top.
    case openSettings
}

func menuNotificationFix(_ permission: NotificationPermission?) -> MenuNotificationFix? {
    switch permission {
    case .notAsked: return .request
    case .denied: return .openSettings
    case .allowed, nil: return nil
    }
}

/// The app's pane in System Settings, Notifications, then the Notifications
/// pane itself as a fallback.
func notificationSettingsURLs(bundleID: String) -> [URL] {
    let base = "x-apple.systempreferences:com.apple.Notifications-Settings.extension"
    return [URL(string: "\(base)?id=\(bundleID)"), URL(string: base)].compactMap { $0 }
}

/// The test notification's userInfo: no pid nor start time, so
/// `serverID(fromUserInfo:)` finds no server and its Stop and Keep do nothing.
func sampleNotificationUserInfo() -> [String: Any] {
    ["sample": true]
}

func isSampleNotification(_ info: [AnyHashable: Any]) -> Bool {
    info["sample"] as? Bool == true
}

/// What the last "Send a test notification" did.
enum TestNotificationResult: Equatable, Sendable {
    case none
    /// Notification Center accepted it.
    case sent
    /// Notification Center refused it.
    case failed

    init(added: Bool) {
        self = added ? .sent : .failed
    }
}

/// Seconds before the test button's title goes back to "Send a test notification".
let testNotificationResetDelay: TimeInterval = 4

// MARK: - Settings

/// Whole minutes in `seconds`, rounded down.
func minutes(fromSeconds seconds: UInt64) -> Int {
    Int(clamping: seconds / 60)
}

func seconds(fromMinutes minutes: Int) -> UInt64 {
    UInt64(max(0, minutes)) * 60
}

/// The Settings form's working copy of the config. Delays are kept in
/// seconds, so a delay the form does not touch is saved back unchanged even
/// when it is not a whole number of minutes.
struct SettingsDraft: Equatable, Sendable {
    var deadSessionIdleS: UInt64
    var alertAfterS: UInt64
    var killGraceS: UInt64
    var scanIntervalS: UInt64
    var claudeConfigDirs: [String]
    var excludeCommands: [String]
    var excludePorts: [UInt16]
    /// Leak thresholds (MB): no field in the form yet, saved back unchanged.
    var leakMemoryMb: UInt64
    var leakGrowthMb: UInt64

    init(_ c: ConfigFfi) {
        deadSessionIdleS = c.deadSessionIdleS
        alertAfterS = c.alertAfterS
        killGraceS = c.killGraceS
        scanIntervalS = c.scanIntervalS
        claudeConfigDirs = c.claudeConfigDirs
        excludeCommands = c.excludeCommands
        excludePorts = c.excludePorts
        leakMemoryMb = c.leakMemoryMb
        leakGrowthMb = c.leakGrowthMb
    }

    var ffi: ConfigFfi {
        ConfigFfi(
            deadSessionIdleS: deadSessionIdleS,
            alertAfterS: alertAfterS,
            killGraceS: killGraceS,
            scanIntervalS: scanIntervalS,
            claudeConfigDirs: claudeConfigDirs,
            excludeCommands: excludeCommands,
            excludePorts: excludePorts,
            leakMemoryMb: leakMemoryMb,
            leakGrowthMb: leakGrowthMb
        )
    }

    var deadSessionIdleMin: Int {
        get { minutes(fromSeconds: deadSessionIdleS) }
        set { deadSessionIdleS = seconds(fromMinutes: newValue) }
    }

    var alertAfterMin: Int {
        get { minutes(fromSeconds: alertAfterS) }
        set { alertAfterS = seconds(fromMinutes: newValue) }
    }

    var killGraceSec: Int {
        get { Int(clamping: killGraceS) }
        set { killGraceS = UInt64(max(0, newValue)) }
    }

    var scanIntervalSec: Int {
        get { Int(clamping: scanIntervalS) }
        set { scanIntervalS = UInt64(max(0, newValue)) }
    }

    /// Empty when the draft can be saved: minute delays at least 1 min,
    /// second delays at least 1 s.
    var validationErrors: [String] { validationMessages() }

    var isValid: Bool { validationErrors.isEmpty }
}

/// Decides when the Settings form saves: only when the draft differs from
/// what was last loaded or saved, so loading (or reloading on reopen) never
/// writes the file; only a user edit does.
struct SettingsSaveGate: Sendable {
    private var synced: SettingsDraft?

    /// The draft just read from disk.
    mutating func loaded(_ draft: SettingsDraft) {
        synced = draft
    }

    /// True when `draft` is a valid edit to save; it then counts as synced.
    mutating func shouldSave(_ draft: SettingsDraft) -> Bool {
        guard let synced, draft != synced, draft.isValid else { return false }
        self.synced = draft
        return true
    }
}

/// `list` with `entry` appended, trimmed. `nil` when the entry is empty or
/// already in the list.
func addingEntry(_ entry: String, to list: [String]) -> [String]? {
    let value = entry.trimmingCharacters(in: .whitespacesAndNewlines)
    guard !value.isEmpty, !list.contains(value) else { return nil }
    return list + [value]
}

/// `ports` with the port typed in `entry` appended. `nil` when the entry is
/// not a port number (1 to 65535) or is already in the list.
func addingPort(_ entry: String, to ports: [UInt16]) -> [UInt16]? {
    let text = entry.trimmingCharacters(in: .whitespacesAndNewlines)
    guard let port = UInt16(text), port > 0, !ports.contains(port) else { return nil }
    return ports + [port]
}

/// What the menu bar pig shows: red when memory pressure is critical, orange
/// when it is at warning or servers need you, plain otherwise (`tint` nil).
/// The count is the "Needs you" cards, capped at `9+`.
enum StatusTint: Equatable, Sendable {
    case warning, critical
}

struct MenuBarStatus: Equatable, Sendable {
    let tint: StatusTint?
    let count: String?
}

func menuBarStatus(needsYou: Int, pressure: MemoryPressure) -> MenuBarStatus {
    let count = needsYou <= 0 ? nil : needsYou > 9 ? "9+" : String(needsYou)
    let tint: StatusTint?
    switch pressure {
    case .critical: tint = .critical
    case .warning: tint = .warning
    case .normal: tint = needsYou > 0 ? .warning : nil
    }
    return MenuBarStatus(tint: tint, count: count)
}
