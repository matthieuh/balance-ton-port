import SwiftUI

/// The capsule a hovered row shows in place of its gauge: Open, Stop (not
/// for an excluded server) and ⋯ (`capsuleButtons`). Restart lives in ⋯
/// only.
struct RowCapsule: View {
    let server: ServerViewFfi
    @EnvironmentObject private var model: CoreModel
    @EnvironmentObject private var language: LanguageModel

    var body: some View {
        HStack(spacing: 2) {
            ForEach(capsuleButtons(server), id: \.self) { button in
                switch button {
                case .open:
                    pill("\u{2197} " + language.tr("Open"), color: .primary.opacity(0.8)) { model.open(server) }
                case .stop:
                    pill("\u{25A0} " + language.tr("Stop"), color: .red) { model.stop(server) }
                        .disabled(!canStopNow(server, restarting: model.isRestarting(server.id)))
                case .more:
                    MoreMenu(server: server, bare: true)
                }
            }
        }
        .padding(3)
        .background(Color.primary.opacity(0.08), in: Capsule())
    }

    private func pill(_ title: String, color: Color, action: @escaping () -> Void) -> some View {
        Button(action: action) {
            Text(verbatim: title)
                .font(.system(size: 12))
                .foregroundStyle(color)
                .padding(.horizontal, 10)
                .frame(height: actionButtonHeight - 1)
                .contentShape(Capsule())
        }
        .buttonStyle(PressableStyle())
    }
}
