import AppKit
import WebKit

/// Live page previews for the detail's hero: one offscreen snapshot per
/// server (pid and start time), taken only while a browser already uses it
/// (`shouldSnapshot`), kept `previewCacheTTL`. The web view lives only for
/// the snapshot. Its connections come from WebKit's own processes, not the
/// app's, so the core may count them as use while the page loads (a few
/// seconds, on a server that is in use anyway).
@MainActor
final class PreviewStore: ObservableObject {
    static let shared = PreviewStore()

    @Published private(set) var images: [ServerID: (image: NSImage, at: Date)] = [:]
    /// When the last attempt started, per server: the next waits a full TTL.
    private var triedAt: [ServerID: Date] = [:]
    /// The one snapshot in flight, if any (the panel shows one detail).
    private var running: (id: ServerID, preview: PagePreview)?
    /// The port the stored rule list was compiled for.
    private var rulesPort: UInt16?
    private var cleaned = false

    private init() {}

    /// The snapshot, while it is younger than `previewCacheTTL`.
    func image(for id: ServerID) -> NSImage? {
        guard let entry = images[id], previewCacheFresh(takenAt: entry.at) else { return nil }
        return entry.image
    }

    /// Starts a snapshot when the server is in use, none is running and the
    /// last attempt is older than the TTL; drops a stale image.
    func refresh(_ s: ServerViewFfi) {
        let id = s.id
        if let entry = images[id], !previewCacheFresh(takenAt: entry.at) { images[id] = nil }
        guard shouldSnapshot(s), running == nil else { return }
        if let at = triedAt[id], previewCacheFresh(takenAt: at) { return }
        triedAt[id] = Date()
        let preview = PagePreview(port: s.port, rules: { [weak self] port in await self?.rules(for: port) }) {
            [weak self] image in
            guard let self else { return }
            if self.running?.id == id { self.running = nil }
            if let image { self.images[id] = (image, Date()) }
        }
        running = (id, preview)
        preview.start()
    }

    /// The detail closed: its image goes.
    func drop(_ id: ServerID) {
        images[id] = nil
    }

    /// Forgets the previews of servers that are gone.
    func retain(_ live: Set<ServerID>) {
        images = images.filter { live.contains($0.key) }
        triedAt = triedAt.filter { live.contains($0.key) }
        if let r = running, !live.contains(r.id) { running = nil }
    }

    /// The rule list for `port`, stored under the one `previewRuleListID`:
    /// looked up when it was compiled for this port, compiled (replacing
    /// it) otherwise.
    private func rules(for port: UInt16) async -> WKContentRuleList? {
        let store = WKContentRuleListStore.default()
        if !cleaned {
            cleaned = true
            // Lists stored one per port by an earlier build.
            for old in await store?.availableIdentifiers() ?? [] where old.hasPrefix(previewRuleListID + "-") {
                try? await store?.removeContentRuleList(forIdentifier: old)
            }
        }
        if rulesPort == port, let list = try? await store?.contentRuleList(forIdentifier: previewRuleListID) {
            return list
        }
        let list = try? await store?.compileContentRuleList(
            forIdentifier: previewRuleListID, encodedContentRuleList: previewContentRules(port: port)
        )
        rulesPort = list == nil ? nil : port
        return list
    }
}

/// One snapshot of `http://127.0.0.1:<port>/` at 1280 x 800, then the web
/// view is released. Non-persistent data store. Every load, the page's
/// subresources included, is blocked by a content rule list unless it goes
/// to the server's own port on loopback; navigations elsewhere (links,
/// redirects, frames) are cancelled as well; a document-start script turns
/// DNS prefetch off, drops resource hints to other origins and takes
/// service workers away (`previewUserScript`). Without the rule list there
/// is no preview.
@MainActor
final class PagePreview: NSObject, WKNavigationDelegate {
    private let port: UInt16
    private let rules: (UInt16) async -> WKContentRuleList?
    private let done: (NSImage?) -> Void
    private var webView: WKWebView?
    private var finished = false
    /// How long the page may take to load, and to settle once loaded.
    private let loadTimeout: Duration = .seconds(10)
    private let settle: Duration = .milliseconds(1200)

    init(port: UInt16, rules: @escaping (UInt16) async -> WKContentRuleList?, done: @escaping (NSImage?) -> Void) {
        self.port = port
        self.rules = rules
        self.done = done
    }

    func start() {
        Task {
            guard let rules = await self.rules(port) else {
                finish(nil)
                return
            }
            let config = WKWebViewConfiguration()
            config.websiteDataStore = .nonPersistent()
            config.userContentController.add(rules)
            config.userContentController.addUserScript(WKUserScript(
                source: previewUserScript(port: port), injectionTime: .atDocumentStart, forMainFrameOnly: true
            ))
            config.defaultWebpagePreferences.allowsContentJavaScript = true
            config.mediaTypesRequiringUserActionForPlayback = .all
            let web = WKWebView(frame: CGRect(x: 0, y: 0, width: 1280, height: 800), configuration: config)
            web.navigationDelegate = self
            webView = web
            web.load(URLRequest(url: previewURL(port: port), timeoutInterval: 8))
            try? await Task.sleep(for: loadTimeout)
            finish(nil)
        }
    }

    func webView(_ webView: WKWebView, decidePolicyFor navigationAction: WKNavigationAction) async -> WKNavigationActionPolicy {
        guard let url = navigationAction.request.url else { return .cancel }
        if url.absoluteString == "about:blank" || previewAllows(url, port: port) { return .allow }
        return .cancel
    }

    func webView(_ webView: WKWebView, didReceiveServerRedirectForProvisionalNavigation navigation: WKNavigation!) {
        if let url = webView.url, !previewAllows(url, port: port) {
            finish(nil)
        }
    }

    func webView(_ webView: WKWebView, didFinish navigation: WKNavigation!) {
        Task {
            try? await Task.sleep(for: settle)
            guard !finished, let web = self.webView else { return }
            let config = WKSnapshotConfiguration()
            config.snapshotWidth = 840
            let image = try? await web.takeSnapshot(configuration: config)
            finish(image)
        }
    }

    func webView(_ webView: WKWebView, didFail navigation: WKNavigation!, withError error: Error) {
        finish(nil)
    }

    func webView(_ webView: WKWebView, didFailProvisionalNavigation navigation: WKNavigation!, withError error: Error) {
        finish(nil)
    }

    func webViewWebContentProcessDidTerminate(_ webView: WKWebView) {
        finish(nil)
    }

    /// Hands the image over once and releases the web view.
    private func finish(_ image: NSImage?) {
        guard !finished else { return }
        finished = true
        webView?.stopLoading()
        webView?.navigationDelegate = nil
        webView = nil
        done(image)
    }
}
