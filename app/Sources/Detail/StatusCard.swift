import SwiftUI

/// What is going on in one sentence, the countdown ring and its bold line
/// when a stop is scheduled, and the one fitting action (Postpone 1 h
/// before a stop, Restart for a leak). While a Restart runs, and for a
/// moment after a refused one, the sentence says so instead.
struct StatusCard: View {
    let server: ServerViewFfi
    let note: RowNote?
    /// The countdown's full length (`countdownTotalMs`).
    let totalMs: UInt64
    let restarting: Bool
    let perform: (DetailButton) -> Void
    @Environment(\.colorScheme) private var scheme
    @EnvironmentObject private var language: LanguageModel

    var body: some View {
        let tint = server.card.map(cardTint) ?? Color.secondary
        let text = server.card.map { cardTextColor($0, dark: scheme == .dark) }
        let fraction = countdownFraction(killInMs: server.killInMs, totalMs: totalMs)
        HStack(spacing: 10) {
            if let fraction {
                ZStack {
                    Circle().stroke(Color.primary.opacity(0.12), lineWidth: 3)
                    Circle()
                        .trim(from: 0, to: fraction)
                        .stroke(tint, style: StrokeStyle(lineWidth: 3, lineCap: .round))
                        .rotationEffect(.degrees(-90))
                }
                .frame(width: 30, height: 30)
                .padding(2)
            }
            VStack(alignment: .leading, spacing: 2) {
                if let title = statusTitle(server, bundle: language.bundle, locale: language.locale) {
                    Text(verbatim: title)
                        .font(.system(size: 13, weight: .bold))
                        .foregroundStyle(text ?? Color.primary)
                }
                Text(verbatim: note.map { rowNoteText($0, bundle: language.bundle, locale: language.locale) } ?? statusSentence(server, bundle: language.bundle, locale: language.locale))
                    .font(.system(size: 12))
                    .foregroundStyle(text.map { $0.opacity(0.9) } ?? Color.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            if let action = statusCardAction(server) {
                Button { perform(action) } label: {
                    Text(verbatim: action.title(bundle: language.bundle, locale: language.locale))
                        .font(.system(size: 12, weight: .semibold))
                        .padding(.horizontal, 14)
                        .frame(height: detailButtonHeight)
                        .background(Color.primary.opacity(0.14), in: Capsule())
                        .contentShape(Capsule())
                }
                .buttonStyle(PressableStyle())
                .disabled(!detailButtonEnabled(action, server, restarting: restarting))
            }
        }
        .padding(.vertical, 10)
        .padding(.horizontal, 12)
        .background {
            let shape = RoundedRectangle(cornerRadius: cardRadius, style: .continuous)
            shape.fill(server.card == nil ? Color.primary.opacity(0.05) : tint.opacity(scheme == .dark ? 0.1 : 0.07))
                .overlay(shape.strokeBorder(server.card == nil ? Color.primary.opacity(0.08) : tint.opacity(0.18)))
        }
    }
}
