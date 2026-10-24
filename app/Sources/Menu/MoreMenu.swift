import SwiftUI

/// The ⋯ button and its menu, the same for every server
/// (`moreMenuItems`). Details… opens the detail view.
struct MoreMenu: View {
    let server: ServerViewFfi
    /// Drawn without its own circle (inside the row capsule).
    var bare = false
    /// In the detail's hero: no Details…, and a dark circle over a preview.
    var inDetail = false
    var onImage = false
    @EnvironmentObject private var model: CoreModel
    @EnvironmentObject private var nav: MenuNavigation
    @EnvironmentObject private var language: LanguageModel

    var body: some View {
        Menu {
            ForEach(
                moreMenuItems(
                    server, restarting: model.isRestarting(server.id), inDetail: inDetail,
                    bundle: language.bundle, locale: language.locale
                ),
                id: \.kind
            ) { item in
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
                .foregroundStyle(onImage ? Color.white.opacity(0.9) : Color.primary)
                .frame(width: actionButtonHeight, height: actionButtonHeight)
                .background(circle, in: Circle())
                .contentShape(Circle())
        }
        .menuStyle(.button)
        .menuIndicator(.hidden)
        .buttonStyle(PressableStyle())
        .fixedSize()
    }

    private var circle: Color {
        if bare { return .clear }
        return onImage ? Color.black.opacity(0.35) : Color.primary.opacity(0.1)
    }

    private func perform(_ kind: MenuItemKind) {
        switch kind {
        case .open: model.open(server)
        case .restart: model.restart(server)
        case .keep: model.keep(server)
        case .ignoreAlert: model.ignoreAlert(server)
        case .stop: model.stop(server)
        case .details: nav.open(server.id)
        case .separator: break
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
