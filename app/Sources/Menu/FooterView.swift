import AppKit
import SwiftUI

/// `Recently stopped (n)`, which unfolds the last stops, and the gear
/// (Settings, Quit).
struct FooterView: View {
    let recentKills: [KillRecordFfi]
    @Environment(\.openSettings) private var openSettings
    @EnvironmentObject private var language: LanguageModel
    @State private var showStopped = false

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            Divider().opacity(0.6)
            HStack {
                Button {
                    showStopped.toggle()
                } label: {
                    Text(verbatim: recentlyStoppedLabel(recentKills.count, bundle: language.bundle, locale: language.locale))
                        .contentShape(Rectangle())
                }
                .buttonStyle(PressableStyle())
                .disabled(recentKills.isEmpty)
                Spacer()
                Menu {
                    Button(language.tr("Settings\u{2026}")) {
                        NSApp.activate(ignoringOtherApps: true)
                        openSettings()
                    }
                    Divider()
                    Button(language.tr("Quit")) { NSApp.terminate(nil) }
                        .keyboardShortcut("q")
                } label: {
                    Image(systemName: "gearshape")
                }
                .menuStyle(.button)
                .menuIndicator(.hidden)
                .buttonStyle(PressableStyle())
                .fixedSize()
            }
            .font(.system(size: 12))
            .foregroundStyle(.secondary)
            if showStopped && !recentKills.isEmpty {
                Grid(alignment: .leading, horizontalSpacing: 10, verticalSpacing: 2) {
                    ForEach(Array(recentKills.prefix(5).enumerated()), id: \.offset) { _, kill in
                        GridRow {
                            Text(verbatim: portLabel(kill.port)).monospacedDigit()
                            Text(verbatim: kill.toolLabel).lineLimit(1)
                            Text(verbatim: killReasonText(kill, bundle: language.bundle, locale: language.locale)).lineLimit(1)
                            Text(verbatim: relativeTime(atMs: kill.atMs, bundle: language.bundle, locale: language.locale))
                        }
                    }
                }
                .font(.system(size: 11))
                .foregroundStyle(.secondary)
            }
        }
    }
}
