import AppKit
import SwiftUI

/// The 22 pt tile in front of a card or row (40 pt in the detail's hero):
/// the page's favicon when the core has one, else the framework's tile,
/// else the title's first letter.
struct ServerIcon: View {
    let server: ServerViewFfi
    let favicon: NSImage?
    var size: CGFloat = 22
    @Environment(\.colorScheme) private var scheme

    var body: some View {
        let shape = RoundedRectangle(cornerRadius: iconRadius * size / 22, style: .continuous)
        let glyph = 11 * size / 22
        Group {
            if let favicon {
                Image(nsImage: favicon)
                    .resizable()
                    .interpolation(.high)
                    .scaledToFit()
                    .padding(size / 11)
                    .background(Color.primary.opacity(0.06))
            } else {
                let tile = iconTile(server)
                ZStack {
                    if let colors = tile.background {
                        LinearGradient(colors: colors, startPoint: .topLeading, endPoint: .bottomTrailing)
                    } else {
                        Color.primary.opacity(scheme == .dark ? 0.1 : 0.07)
                    }
                    if let symbol = tile.symbol {
                        Image(systemName: symbol).font(.system(size: glyph, weight: .semibold))
                    } else {
                        Text(verbatim: tile.letter).font(.system(size: glyph, weight: .bold))
                    }
                }
                .foregroundStyle(tile.background == nil ? portColor(server.port, dark: scheme == .dark) : .white)
            }
        }
        .frame(width: size, height: size)
        .clipShape(shape)
        .overlay(shape.strokeBorder(Color.primary.opacity(0.12), lineWidth: 0.5))
    }
}
