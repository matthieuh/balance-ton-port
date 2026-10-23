import AppKit
import SwiftUI

/// A server that needs a decision: what it is, why, and the one suggested
/// action (plus ⋯). The pill is drawn here; its action comes later.
struct NeedsYouCard: View {
    let server: ServerViewFfi
    let card: CardFfi
    let favicon: NSImage?
    var onKeep: (() -> Void)?
    var onStop: (() -> Void)?
    @Environment(\.colorScheme) private var scheme

    var body: some View {
        let tint = cardTint(card)
        VStack(alignment: .leading, spacing: 0) {
            HStack(spacing: 8) {
                ServerIcon(server: server, favicon: favicon)
                ServerTitle(server: server).frame(maxWidth: .infinity, alignment: .leading)
                Text(verbatim: memoryLabel(server.rssBytes))
                    .font(.system(size: 12, design: .monospaced))
                    .foregroundStyle(sizeColor(server.health) ?? Color.secondary)
            }
            ServerSubtitle(server: server)
                .padding(.leading, cardIndent)
                .padding(.top, 3)
            HStack(spacing: 8) {
                Text(verbatim: card.reason)
                    .font(.system(size: 12))
                    .foregroundStyle(tint.opacity(scheme == .dark ? 0.85 : 1))
                    .lineLimit(2)
                    .frame(maxWidth: .infinity, alignment: .leading)
                actionPill(tint: tint)
                MoreMenu(server: server, onKeep: onKeep, onStop: onStop)
            }
            .padding(.leading, cardIndent)
            .padding(.top, 9)
        }
        .padding(10)
        .background {
            let shape = RoundedRectangle(cornerRadius: cardRadius, style: .continuous)
            shape.fill(tint.opacity(scheme == .dark ? 0.09 : 0.07))
                .background(shape.fill(Color(nsColor: .controlBackgroundColor).opacity(scheme == .dark ? 0.35 : 0.8)))
                .overlay(shape.strokeBorder(tint.opacity(0.22), lineWidth: 1))
                .shadow(color: .black.opacity(scheme == .dark ? 0.3 : 0.08), radius: 7, y: 4)
        }
        .help(rowHelp(server))
    }

    private func actionPill(tint: Color) -> some View {
        let prominent = cardActionIsProminent(card)
        return Button {} label: {
            Text(verbatim: cardActionLabel(card))
                .font(.system(size: 12, weight: .semibold))
                .foregroundStyle(prominent ? Color.black.opacity(0.85) : Color.primary)
                .padding(.horizontal, 14)
                .frame(height: actionButtonHeight)
                .background(prominent ? tint : Color.primary.opacity(0.12), in: Capsule())
                .contentShape(Capsule())
        }
        .buttonStyle(PressableStyle())
    }
}
