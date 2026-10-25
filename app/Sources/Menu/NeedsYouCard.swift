import AppKit
import SwiftUI

/// A server that needs a decision: what it is, why, and the one suggested
/// action (plus ⋯); a click elsewhere opens the detail. While a Restart runs, and for a moment after a refused
/// one, the reason line says so instead.
struct NeedsYouCard: View {
    let server: ServerViewFfi
    let card: CardFfi
    let favicon: NSImage?
    var note: RowNote?
    @EnvironmentObject private var model: CoreModel
    @EnvironmentObject private var nav: MenuNavigation
    @EnvironmentObject private var language: LanguageModel
    @Environment(\.colorScheme) private var scheme

    var body: some View {
        let tint = cardTint(card)
        VStack(alignment: .leading, spacing: 0) {
            HStack(spacing: 8) {
                ServerIcon(server: server, favicon: favicon)
                ServerTitle(server: server).frame(maxWidth: .infinity, alignment: .leading)
                Text(verbatim: memoryLabel(server.rssBytes, locale: language.locale))
                    .font(.system(size: 12).monospacedDigit())
                    .foregroundStyle(sizeColor(server.health) ?? Color.secondary)
            }
            ServerSubtitle(server: server)
                .padding(.leading, cardIndent)
                .padding(.top, 3)
            HStack(spacing: 8) {
                Text(verbatim: note.map { rowNoteText($0, bundle: language.bundle, locale: language.locale) } ?? reason)
                    .font(.system(size: 12))
                    .foregroundStyle(tint.opacity(scheme == .dark ? 0.85 : 1))
                    .lineLimit(2)
                    .frame(maxWidth: .infinity, alignment: .leading)
                actionPill(tint: tint)
                MoreMenu(server: server)
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
        .contentShape(RoundedRectangle(cornerRadius: cardRadius, style: .continuous))
        .onTapGesture { nav.open(server.id) }
        .accessibilityAction(named: Text(verbatim: language.tr("Details"))) { nav.open(server.id) }
        .help(rowHelp(server))
    }

    private var reason: String {
        cardReason(
            card, leakKind: server.leakKind, agentLabel: server.agentLabel,
            bundle: language.bundle, locale: language.locale
        )
    }

    private func actionPill(tint: Color) -> some View {
        let prominent = cardActionIsProminent(card)
        return Button(action: perform) {
            Text(verbatim: cardActionLabel(card, bundle: language.bundle, locale: language.locale))
                .font(.system(size: 12, weight: .semibold))
                .foregroundStyle(prominent ? Color.black.opacity(0.85) : Color.primary)
                .padding(.horizontal, 14)
                .frame(height: actionButtonHeight)
                .background(prominent ? tint : Color.primary.opacity(0.12), in: Capsule())
                .contentShape(Capsule())
        }
        .buttonStyle(PressableStyle())
        .disabled(!cardActionEnabled(card, server, restarting: note == .restarting))
    }

    private func perform() {
        switch card.action {
        case "restart": model.restart(server)
        case "postpone": model.postpone(server)
        default: model.stop(server)
        }
    }
}
