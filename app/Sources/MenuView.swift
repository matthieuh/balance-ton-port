import AppKit
import SwiftUI

/// The menu as the spec draws it: tracked servers, "Recently killed" (last 5),
/// then Settings and Quit.
struct MenuView: View {
    @EnvironmentObject private var model: CoreModel
    @Environment(\.openSettings) private var openSettings

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            Text("\u{2693} Balance ton port")
                .font(.headline)

            if model.watchedElsewhere {
                Text("Watching is done by another process")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
            if let error = model.lastError {
                Text(error)
                    .font(.caption)
                    .foregroundStyle(.red)
                    .lineLimit(2)
            }

            Divider()

            if model.servers.isEmpty {
                Text("No dev servers from Claude Code sessions")
                    .foregroundStyle(.secondary)
            } else {
                Grid(alignment: .leading, horizontalSpacing: 10, verticalSpacing: 4) {
                    ForEach(model.servers, id: \.id) { server in
                        ServerRow(server: server) { model.kill(server) }
                    }
                }
            }

            Divider()

            Text("Recently killed")
                .font(.subheadline.weight(.semibold))
            if model.recentKills.isEmpty {
                Text("Nothing yet")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            } else {
                Grid(alignment: .leading, horizontalSpacing: 10, verticalSpacing: 2) {
                    ForEach(Array(model.recentKills.prefix(5).enumerated()), id: \.offset) { _, kill in
                        GridRow {
                            Text(String(kill.port)).monospacedDigit()
                            Text(kill.command).lineLimit(1)
                            Text(kill.reason).lineLimit(1)
                            Text(relativeTime(atMs: kill.atMs))
                                .foregroundStyle(.secondary)
                        }
                        .font(.callout)
                    }
                }
            }

            Divider()

            HStack {
                Button("Settings\u{2026}") {
                    NSApp.activate(ignoringOtherApps: true)
                    openSettings()
                }
                Spacer()
                Button("Quit") { NSApp.terminate(nil) }
                    .keyboardShortcut("q")
            }
            .buttonStyle(.borderless)
        }
        .padding(12)
        .frame(minWidth: 380, alignment: .leading)
    }
}

private struct ServerRow: View {
    let server: ServerViewFfi
    let onKill: () -> Void

    var body: some View {
        GridRow {
            Text(server.sessionAlive ? "\u{25CF}" : "\u{25CB}")
                .foregroundStyle(server.sessionAlive ? Color.green : Color.secondary)
            Text(String(server.port)).monospacedDigit()
            Text(server.command).lineLimit(1)
            Text(server.worktreeName).lineLimit(1).foregroundStyle(.secondary)
            Text(stateLabel(server)).lineLimit(1)
            Button("Kill", action: onKill)
                .controlSize(.small)
        }
    }
}
