import SwiftUI

/// The Settings window: General (language and the watcher's settings) and About (updates).
struct SettingsView: View {
    var body: some View {
        TabView {
            GeneralPane()
                .tabItem { Label("General", systemImage: "gearshape") }
            AboutPane()
                .tabItem { Label("About", systemImage: "info.circle") }
        }
        .frame(width: 460)
        .frame(minHeight: 520)
    }
}
