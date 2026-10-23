import SwiftUI

/// The capsule a hovered row shows in place of its gauge: Open, Stop (not
/// for an excluded server) and ⋯. Restart lives in ⋯ only.
struct RowCapsule: View {
    let server: ServerViewFfi
    var onKeep: (() -> Void)?
    var onStop: (() -> Void)?

    var body: some View {
        HStack(spacing: 2) {
            pill("\u{2197} Open", color: .primary.opacity(0.8)) {}
            if !server.excluded {
                pill("\u{25A0} Stop", color: .red) { onStop?() }
            }
            MoreMenu(server: server, onKeep: onKeep, onStop: onStop, bare: true)
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
