import AppKit
import SwiftUI

/// The menu as the spec draws it: tracked servers, "Recently killed" (last 5),
/// then Settings and Quit.
struct MenuView: View {
    @EnvironmentObject private var model: CoreModel
    @Environment(\.openSettings) private var openSettings

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack(spacing: 4) {
                Image("MenuIcon")
                    .renderingMode(.template)
                    .resizable()
                    .scaledToFit()
                    .frame(width: 16, height: 16)
                Text("Balance ton port")
            }
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
                VStack(alignment: .leading, spacing: 6) {
                    ForEach(Array(model.servers.enumerated()), id: \.element.id) { index, server in
                        if index > 0 { Divider() }
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
                            Text(kill.toolLabel).lineLimit(1)
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

/// Three lines: what the server is and its port, where it runs, and the
/// Claude Code session behind it. Hovering shows the full command line, pid
/// and worktree path.
private struct ServerRow: View {
    let server: ServerViewFfi
    let onKill: () -> Void

    /// Lines 2 and 3 start under the tool label, past the status dot.
    private let indent: CGFloat = 16

    var body: some View {
        VStack(alignment: .leading, spacing: 2) {
            HStack(spacing: 0) {
                Text(server.sessionAlive ? "\u{25CF}" : "\u{25CB}")
                    .foregroundStyle(server.sessionAlive ? Color.green : Color.secondary)
                    .frame(width: indent, alignment: .leading)
                Text(server.toolLabel)
                    .fontWeight(.semibold)
                    .lineLimit(1)
                Spacer(minLength: 12)
                Text(":\(server.port)")
                    .monospacedDigit()
                    .padding(.trailing, 10)
                Button("Kill", action: onKill)
                    .controlSize(.small)
            }
            .font(.body)
            Text(server.locationLabel)
                .lineLimit(1)
                .truncationMode(.middle)
                .foregroundStyle(.secondary)
                .padding(.leading, indent)
            Text(sessionLine(server))
                .lineLimit(1)
                .truncationMode(.tail)
                .foregroundStyle(.secondary)
                .padding(.leading, indent)
        }
        .font(.callout)
        .frame(maxWidth: .infinity, alignment: .leading)
        .contentShape(Rectangle())
        .help(rowHelp(server))
    }
}
