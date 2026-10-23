import AppKit
import SwiftUI

/// The top of the detail: the page's live preview when there is one, else
/// the favicon's colors as a gradient; icon, name, framework and ports over
/// it, ‹ (back) top left and ⋯ top right.
struct HeroView: View {
    let server: ServerViewFfi
    let favicon: NSImage?
    /// The favicon's dominant colors, worked out once per opened server.
    let iconRGB: [UInt32]
    let preview: NSImage?
    let onBack: () -> Void
    let more: AnyView
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
            Button(action: onBack) {
                Text(verbatim: "\u{2039}")
                    .font(.system(size: 20, weight: .regular))
                    .foregroundStyle(onImage ? Color.white.opacity(0.9) : Color.primary.opacity(0.7))
                    .frame(width: actionButtonHeight, height: actionButtonHeight)
                    .contentShape(Rectangle())
            }
            .buttonStyle(PressableStyle())
            .help("Back")
            .padding(.leading, 6)
            .padding(.top, 6)
        }
        .overlay(alignment: .topTrailing) {
            more.padding(10)
        }
    }

    /// Text and buttons sit on the preview (darkened): always light there.
    private var onImage: Bool { preview != nil }

    @ViewBuilder private var background: some View {
        if let preview {
            Image(nsImage: preview)
                .resizable()
                .interpolation(.high)
                .scaledToFill()
                .frame(maxWidth: .infinity, maxHeight: heroHeight, alignment: .top)
                .clipped()
                // A scrim at the top for ‹ and ⋯, a deeper one at the bottom
                // for the name.
                .overlay(LinearGradient(
                    stops: [
                        .init(color: .black.opacity(0.55), location: 0),
                        .init(color: .black.opacity(0.1), location: 0.35),
                        .init(color: .black.opacity(0.25), location: 0.55),
                        .init(color: .black.opacity(0.75), location: 1),
                    ],
                    startPoint: .top, endPoint: .bottom
                ))
        } else {
            // The favicon's colors, else the framework tile's, else the port's.
            let colors = iconRGB.isEmpty
                ? (iconTile(server).background ?? [portColor(server.port, dark: scheme == .dark)])
                : iconRGB.map { Color(hex: $0) }
            let first = colors[0], second = colors.count > 1 ? colors[1] : colors[0]
            let strength = scheme == .dark ? 0.32 : 0.28
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
                .shadow(color: .black.opacity(scheme == .dark || onImage ? 0.4 : 0.15), radius: 7, y: 4)
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
                .foregroundStyle(onImage ? Color.white.opacity(0.8) : Color.primary.opacity(0.7))
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
