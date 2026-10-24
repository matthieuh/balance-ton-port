import SwiftUI

/// The Settings window: General (language and the watcher's settings) and About (updates).
struct SettingsView: View {
    @EnvironmentObject private var language: LanguageModel

    var body: some View {
        TabView {
            GeneralPane()
                .tabItem { Label(language.tr("General"), systemImage: "gearshape") }
            AboutPane()
                .tabItem { Label(language.tr("About"), systemImage: "info.circle") }
        }
        .frame(width: 460)
        .frame(minHeight: 520)
    }
}
