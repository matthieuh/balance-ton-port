import SwiftUI

/// The ⋯ button and its menu, the same for every server. Keep and Stop call
/// through for agent servers; items without an action yet are disabled.
struct MoreMenu: View {
    let server: ServerViewFfi
    var onKeep: (() -> Void)?
    var onStop: (() -> Void)?
    /// Drawn without its own circle (inside the row capsule).
    var bare = false

    var body: some View {
        Menu {
            Button("\u{2197} Open in browser") {}.disabled(true)
            Button("\u{27F3} Restart") {}.disabled(true)
            Button("\u{23F8} Keep (no alerts)") { onKeep?() }.disabled(onKeep == nil)
            Button("\u{2298} Ignore this alert") {}.disabled(true)
            Divider()
            Button("\u{24D8} Details\u{2026}") {}.disabled(true)
            if !server.excluded {
                Button("\u{25A0} Stop", role: .destructive) { onStop?() }.disabled(onStop == nil)
            }
        } label: {
            Text(verbatim: "\u{22EF}")
                .font(.system(size: 13, weight: .semibold))
                .frame(width: actionButtonHeight, height: actionButtonHeight)
                .background(bare ? Color.clear : Color.primary.opacity(0.1), in: Circle())
                .contentShape(Circle())
        }
        .menuStyle(.button)
        .menuIndicator(.hidden)
        .buttonStyle(PressableStyle())
        .fixedSize()
    }
}

/// Every pill and capsule button: plain, pressed at `pressScale`, faded
/// while disabled.
struct PressableStyle: ButtonStyle {
    @Environment(\.isEnabled) private var isEnabled

    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .opacity(isEnabled ? 1 : 0.45)
            .scaleEffect(configuration.isPressed ? pressScale : 1)
            .animation(.easeOut(duration: 0.12), value: configuration.isPressed)
    }
}
