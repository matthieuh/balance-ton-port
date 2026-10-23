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

    @Published private(set) var images: [ServerID: NSImage] = [:]
    private var takenAt: [ServerID: Date] = [:]
    private var running: [ServerID: PagePreview] = [:]

    private init() {}

    /// The last snapshot, kept while the detail is open even once stale.
    func image(for id: ServerID) -> NSImage? {
        images[id]
    }

    /// Starts a snapshot when the server is in use and none is fresh or
    /// running. An attempt (failed or not) waits a full TTL before the next.
    func refresh(_ s: ServerViewFfi) {
        let id = s.id
        guard shouldSnapshot(s), running[id] == nil else { return }
        if let at = takenAt[id], previewCacheFresh(takenAt: at) { return }
        takenAt[id] = Date()
        let preview = PagePreview(port: s.port) { [weak self] image in
            guard let self else { return }
            self.running[id] = nil
            if let image { self.images[id] = image }
        }
        running[id] = preview
        preview.start()
    }

    /// Forgets the previews of servers that are gone.
    func retain(_ live: Set<ServerID>) {
        images = images.filter { live.contains($0.key) }
        takenAt = takenAt.filter { live.contains($0.key) }
    }
}

/// One snapshot of `http://127.0.0.1:<port>/` at 1280 x 800, then the web
/// view is released. Non-persistent data store. Every load, the page's
/// subresources included, is blocked by a content rule list unless it goes
/// to the server's own port on loopback; navigations elsewhere (links,
/// redirects, frames) are cancelled as well. Without the rule list there is
/// no preview.
@MainActor
final class PagePreview: NSObject, WKNavigationDelegate {
    private let port: UInt16
    private let done: (NSImage?) -> Void
    private var webView: WKWebView?
    private var finished = false
    /// How long the page may take to load, and to settle once loaded.
    private let loadTimeout: Duration = .seconds(10)
    private let settle: Duration = .milliseconds(1200)

    init(port: UInt16, done: @escaping (NSImage?) -> Void) {
        self.port = port
        self.done = done
    }

    func start() {
        Task {
            let rules = try? await WKContentRuleListStore.default().compileContentRuleList(
                forIdentifier: "btp-preview-\(port)", encodedContentRuleList: previewContentRules(port: port)
            )
            guard let rules else {
                finish(nil)
                return
            }
            let config = WKWebViewConfiguration()
            config.websiteDataStore = .nonPersistent()
            config.userContentController.add(rules)
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
