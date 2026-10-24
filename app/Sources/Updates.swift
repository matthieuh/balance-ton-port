import Foundation
import Sparkle

/// Automatic updates through Sparkle. The feed (SUFeedURL) is the app's only
/// network call besides localhost.
@MainActor
final class UpdateController: ObservableObject {
    private let controller = SPUStandardUpdaterController(
        startingUpdater: true, updaterDelegate: nil, userDriverDelegate: nil)

    /// Checks and downloads in the background; both Sparkle switches move together.
    @Published var automatic: Bool {
        didSet {
            controller.updater.automaticallyChecksForUpdates = automatic
            controller.updater.automaticallyDownloadsUpdates = automatic
        }
    }

    init() {
        automatic = controller.updater.automaticallyChecksForUpdates
            && controller.updater.automaticallyDownloadsUpdates
    }

    var canCheck: Bool { controller.updater.canCheckForUpdates }

    func checkNow() { controller.updater.checkForUpdates() }

    /// "0.5.0 (7)" from the bundle's Info.plist.
    nonisolated static func appVersion(_ info: [String: Any]) -> String {
        let short = info["CFBundleShortVersionString"] as? String ?? "?"
        let build = info["CFBundleVersion"] as? String ?? "?"
        return "\(short) (\(build))"
    }
}
