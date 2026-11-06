import SwiftUI

/// The port storm alert when no listed server holds the flooded port: the
/// machine is running out of outgoing ports. Its title, the reason line
/// (`cardReason`), no action pill (nothing to stop), and a ⋯ menu with only
/// Ignore this alert.
struct PortStormCard: View {
    let card: CardFfi
    @EnvironmentObject private var model: CoreModel
    @EnvironmentObject private var language: LanguageModel
    @Environment(\.colorScheme) private var scheme

    var body: some View {
        let tint = cardTint(card)
        VStack(alignment: .leading, spacing: 0) {
            HStack(spacing: 8) {
                Image(systemName: "network")
                    .font(.system(size: 12, weight: .semibold))
                    .foregroundStyle(tint)
                    .frame(width: 22, height: 22)
                    .background(tint.opacity(0.15), in: RoundedRectangle(cornerRadius: iconRadius, style: .continuous))
                Text(verbatim: portStormTitle(level: card.portLevel, bundle: language.bundle, locale: language.locale))
                    .font(.system(size: 13, weight: .semibold))
                    .frame(maxWidth: .infinity, alignment: .leading)
            }
            HStack(spacing: 8) {
                Text(verbatim: cardReason(card, bundle: language.bundle, locale: language.locale))
                    .font(.system(size: 12))
                    .foregroundStyle(cardTextColor(card, dark: scheme == .dark))
                    .lineLimit(2)
                    .frame(maxWidth: .infinity, alignment: .leading)
                more
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
    }

    private var more: some View {
        Menu {
            Button("\u{2298} " + language.tr("Ignore this alert")) { model.ignorePortAlert() }
        } label: {
            Text(verbatim: "\u{22EF}")
                .font(.system(size: 13, weight: .semibold))
                .foregroundStyle(Color.primary)
                .frame(width: actionButtonHeight, height: actionButtonHeight)
                .background(Color.primary.opacity(0.1), in: Circle())
                .contentShape(Circle())
        }
        .menuStyle(.button)
        .menuIndicator(.hidden)
        .buttonStyle(PressableStyle())
        .fixedSize()
    }
}
