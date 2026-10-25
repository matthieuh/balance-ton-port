import Combine
import Foundation
import Sparkle

/// Automatic updates through Sparkle. The feed (SUFeedURL) is the app's only
/// network call besides localhost.
@MainActor
final class UpdateController: ObservableObject {
    /// Debug builds never start the updater: an unsigned local build must not check
    /// the real feed, nor install a release over itself.
    #if DEBUG
    nonisolated static let enabled = false
    #else
    nonisolated static let enabled = true
    #endif

    private let controller = SPUStandardUpdaterController(
        startingUpdater: UpdateController.enabled, updaterDelegate: nil, userDriverDelegate: nil)

    /// Checks and downloads in the background; both Sparkle switches move together.
    @Published var automatic: Bool {
        didSet {
            controller.updater.automaticallyChecksForUpdates = automatic
            controller.updater.automaticallyDownloadsUpdates = automatic
        }
    }

    init() {
        let updater = controller.updater
        let both = Self.reconciled(
            checks: updater.automaticallyChecksForUpdates, downloads: updater.automaticallyDownloadsUpdates)
        automatic = both
        // didSet does not run inside init: when the switches disagreed, align them.
        updater.automaticallyChecksForUpdates = both
        updater.automaticallyDownloadsUpdates = both
        // Sparkle publishes canCheckForUpdates through KVO; the pane follows it.
        updater.publisher(for: \.canCheckForUpdates).assign(to: &$canCheck)
    }

    /// Automatic updates are on only when Sparkle both checks and downloads.
    nonisolated static func reconciled(checks: Bool, downloads: Bool) -> Bool { checks && downloads }

    @Published private(set) var canCheck = false

    func checkNow() { controller.updater.checkForUpdates() }

    /// "0.5.0 (7)" from the bundle's Info.plist.
    nonisolated static func appVersion(_ info: [String: Any]) -> String {
        let short = info["CFBundleShortVersionString"] as? String ?? "?"
        let build = info["CFBundleVersion"] as? String ?? "?"
        return "\(short) (\(build))"
    }
}
