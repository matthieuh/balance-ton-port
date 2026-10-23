import AppKit
import SwiftUI

/// The top of the detail: the page's live preview when there is one, else
/// the favicon's colors as a gradient; icon, name, framework and ports over
/// it, ‹ (back) top left and ⋯ top right.
struct HeroView: View {
    let server: ServerViewFfi
    let favicon: NSImage?
    let iconData: Data?
    let preview: NSImage?
    @EnvironmentObject private var nav: MenuNavigation
    @Environment(\.colorScheme) private var scheme

    var body: some View {
        ZStack(alignment: .bottomLeading) {
            background
            identity
                .padding(.leading, panelPadding)
                .padding(.bottom, 12)
        }
        .frame(height: heroHeight)
        .frame(maxWidth: .infinity)
        .clipped()
        .overlay(alignment: .topLeading) {
            Button { nav.back() } label: {
                Text(verbatim: "\u{2039}")
                    .font(.system(size: 18, weight: .medium))
                    .foregroundStyle(onImage ? Color.white.opacity(0.9) : Color.primary.opacity(0.75))
                    .frame(width: actionButtonHeight, height: actionButtonHeight)
                    .background(chrome, in: Circle())
                    .contentShape(Circle())
            }
            .buttonStyle(PressableStyle())
            .help("Back")
            .padding(10)
        }
        .overlay(alignment: .topTrailing) {
            MoreMenu(server: server, inDetail: true, onImage: onImage)
                .padding(10)
        }
    }

    /// Text and buttons sit on the preview (darkened): always light there.
    private var onImage: Bool { preview != nil }

    private var chrome: Color {
        onImage ? Color.black.opacity(0.35) : Color.primary.opacity(scheme == .dark ? 0.12 : 0.08)
    }

    @ViewBuilder private var background: some View {
        if let preview {
            Image(nsImage: preview)
                .resizable()
                .interpolation(.high)
                .scaledToFill()
                .frame(maxWidth: .infinity, maxHeight: heroHeight, alignment: .top)
                .clipped()
                .overlay(LinearGradient(
                    colors: [.black.opacity(0.15), .black.opacity(0.7)], startPoint: .top, endPoint: .bottom
                ))
        } else {
            let colors = dominantColors(iconData ?? Data(), fallback: portColor(server.port, dark: scheme == .dark))
            let first = colors[0], second = colors.count > 1 ? colors[1] : colors[0]
            let strength = scheme == .dark ? 0.32 : 0.22
            ZStack {
                LinearGradient(
                    stops: [
                        .init(color: first.opacity(strength), location: 0),
                        .init(color: second.opacity(strength * 1.2), location: 0.6),
                        .init(color: .clear, location: 1),
                    ],
                    startPoint: .topLeading, endPoint: .bottomTrailing
                )
                Stripes().opacity(scheme == .dark ? 0.04 : 0.05)
            }
        }
    }

    private var identity: some View {
        let sub = heroSubtitle(server)
        let title = titleParts(server).title
        return HStack(spacing: 10) {
            ServerIcon(server: server, favicon: favicon, size: 40)
                .shadow(color: .black.opacity(0.4), radius: 7, y: 4)
            VStack(alignment: .leading, spacing: 2) {
                Text(verbatim: title)
                    .font(.system(size: 16, weight: .semibold))
                    .foregroundStyle(onImage ? Color.white : Color.primary)
                    .lineLimit(1)
                HStack(spacing: 0) {
                    if let kind = sub.kind {
                        Text(verbatim: kind + " \u{00B7} ")
                    }
                    Text(verbatim: sub.port)
                        .font(.system(size: 12, design: .monospaced))
                        .foregroundStyle(portColor(server.port, dark: scheme == .dark || onImage))
                    if let other = sub.otherPorts {
                        Text(verbatim: " " + other)
                            .font(.system(size: 12, design: .monospaced))
                    }
                }
                .font(.system(size: 12))
                .foregroundStyle(onImage ? Color.white.opacity(0.75) : Color.secondary)
                .lineLimit(1)
            }
        }
        .padding(.trailing, panelPadding)
    }
}

let heroHeight: CGFloat = 118

/// The mockup's faint vertical lines over the gradient.
private struct Stripes: View {
    var body: some View {
        Canvas { ctx, size in
            var x: CGFloat = 22
            while x < size.width {
                ctx.fill(Path(CGRect(x: x, y: 0, width: 1, height: size.height)), with: .color(.primary))
                x += 23
            }
        }
    }
}
