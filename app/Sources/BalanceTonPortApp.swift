import SwiftUI

@main
struct BalanceTonPortApp: App {
    @StateObject private var model = CoreModel()

    var body: some Scene {
        MenuBarExtra {
            MenuView()
                .environmentObject(model)
        } label: {
            // SF Symbol template image: follows the menu bar's light/dark look.
            Image(systemName: "ferry")
                .accessibilityLabel("Balance ton port")
        }
        .menuBarExtraStyle(.window)

        // Filled in by Task 10.
        Settings {
            EmptyView()
        }
    }
}
