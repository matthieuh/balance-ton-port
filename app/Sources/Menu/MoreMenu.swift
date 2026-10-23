import SwiftUI

/// The ⋯ button and its menu, the same for every server
/// (`moreMenuItems`). Details… waits for the detail view (Task 11).
struct MoreMenu: View {
    let server: ServerViewFfi
    /// Drawn without its own circle (inside the row capsule).
    var bare = false
    @EnvironmentObject private var model: CoreModel

    var body: some View {
        Menu {
            ForEach(moreMenuItems(server), id: \.kind) { item in
                if item.kind == .separator {
                    Divider()
                } else {
                    Button(item.title, role: item.kind == .stop ? .destructive : nil) { perform(item.kind) }
                        .disabled(!item.enabled)
                }
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

    private func perform(_ kind: MenuItemKind) {
        switch kind {
        case .open: model.open(server)
        case .restart: model.restart(server)
        case .keep: model.keep(server)
        case .ignoreAlert: model.ignoreAlert(server)
        case .stop: model.stop(server)
        // The detail view comes with Task 11; the item is disabled until then.
        case .details, .separator: break
        }
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
