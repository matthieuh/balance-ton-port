import SwiftUI

@main
struct BalanceTonPortApp: App {
    @StateObject private var model = CoreModel()

    var body: some Scene {
        // MenuIcon is an anchor template image (Assets.xcassets), so it
        // follows the menu bar's light and dark look.
        MenuBarExtra("Balance ton port", image: "MenuIcon") {
            MenuView()
                .environmentObject(model)
        }
        .menuBarExtraStyle(.window)

        // Filled in by Task 10.
        Settings {
            EmptyView()
        }
    }
}
