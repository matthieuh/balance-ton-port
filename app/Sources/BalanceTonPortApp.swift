import SwiftUI

@main
struct BalanceTonPortApp: App {
    @StateObject private var model = CoreModel()

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
        }
    }
}
