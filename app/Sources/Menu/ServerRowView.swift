import AppKit
import SwiftUI

/// The second line of a card or row: port in its color, other ports, branch
/// or project, agent badge, and (rows only) the status. The place gives way
/// first (cut in the middle); the status stays whole.
struct ServerSubtitle: View {
    let server: ServerViewFfi
    var status: String?
    @Environment(\.colorScheme) private var scheme

    var body: some View {
        let p = subtitleParts(server)
        let badge = agentBadge(server)
        let sep = " \u{00B7} "
        let port = Text(verbatim: p.port)
            .font(.system(size: 12, design: .monospaced))
            .foregroundColor(portColor(server.port, dark: scheme == .dark))
        HStack(spacing: 0) {
            (p.otherPorts.map { port + Text(verbatim: " " + $0) } ?? port)
                .fixedSize()
            if let place = p.place {
                Text(verbatim: sep + place)
                    .truncationMode(.middle)
                    .layoutPriority(0)
            }
            (Text(verbatim: sep) + Text(verbatim: badge.text)
                .foregroundColor(badge.color ?? Color(nsColor: .tertiaryLabelColor)))
                .fixedSize()
            if let status {
                Text(verbatim: sep + status)
                    .truncationMode(.tail)
                    .layoutPriority(2)
            }
        }
        .font(.system(size: 12))
        .foregroundStyle(.secondary)
        .lineLimit(1)
    }
}

/// The title line: bold title, then ` · framework` in secondary.
struct ServerTitle: View {
    let server: ServerViewFfi

    var body: some View {
        let t = titleParts(server)
        var line = Text(verbatim: t.title).fontWeight(.semibold)
        if let f = t.framework {
            line = line + Text(verbatim: " \u{00B7} \(f)").font(.system(size: 12)).foregroundColor(.secondary)
        }
        return line.font(.system(size: 13)).lineLimit(1).truncationMode(.tail)
    }
}

/// An "All good" row: icon, title and subtitle, then the memory gauge and
/// size, which the hover capsule replaces.
struct ServerRowView: View {
    let server: ServerViewFfi
    let favicon: NSImage?
    let notificationsOff: Bool
    var note: RowNote?
    @State private var hovering = false

    var body: some View {
        HStack(spacing: 8) {
            ServerIcon(server: server, favicon: favicon)
            VStack(alignment: .leading, spacing: 1) {
                ServerTitle(server: server)
                ServerSubtitle(
                    server: server,
                    status: note.map(rowNoteText) ?? rowStatus(server, notificationsOff: notificationsOff)
                )
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            if hovering {
                RowCapsule(server: server)
            } else {
                gauge
                Text(verbatim: memoryLabel(server.rssBytes))
                    .font(.system(size: 12, design: .monospaced))
                    .foregroundStyle(sizeColor(server.health) ?? Color.secondary)
                    .frame(width: 54, alignment: .trailing)
            }
        }
        .padding(.vertical, 7)
        .padding(.horizontal, 6)
        .frame(minHeight: 46)
        .background(
            Color.primary.opacity(hovering ? 0.06 : 0),
            in: RoundedRectangle(cornerRadius: rowRadius, style: .continuous)
        )
        .contentShape(Rectangle())
        .onHover { hovering = $0 }
        .help(rowHelp(server))
    }

    private var gauge: some View {
        Capsule()
            .fill(Color.primary.opacity(0.14))
            .frame(width: 40, height: 4)
            .overlay(alignment: .leading) {
                Capsule()
                    .fill(healthColor(server.health))
                    .frame(width: 40 * gaugeFraction(server.rssBytes), height: 4)
            }
    }
}
