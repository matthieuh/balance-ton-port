import SwiftUI

@main
struct BalanceTonPortApp: App {
    @NSApplicationDelegateAdaptor(AppDelegate.self) private var appDelegate
    @StateObject private var model = CoreModel()
    @StateObject private var updates = UpdateController()

    var body: some Scene {
        // MenuIcon is a pig snout template image (Assets.xcassets), so it
        // follows the menu bar's light and dark look.
        MenuBarExtra("Balance ton port", image: "MenuIcon") {
            MenuView()
                .environmentObject(model)
        }
        .menuBarExtraStyle(.window)

        Settings {
            SettingsView()
                .environmentObject(model)
                .environmentObject(updates)
        }
    }
}

/// Sets up notifications before launch finishes, so a notification button
/// that launched the app reaches it.
final class AppDelegate: NSObject, NSApplicationDelegate {
    func applicationWillFinishLaunching(_ notification: Notification) {
        _ = Notifier.shared
    }
}
