import AppKit
import SwiftUI

/// The 22 pt tile in front of a card or row: the page's favicon when the
/// core has one, else the framework's tile, else the title's first letter.
struct ServerIcon: View {
    let server: ServerViewFfi
    let favicon: NSImage?
    @Environment(\.colorScheme) private var scheme

    var body: some View {
        let shape = RoundedRectangle(cornerRadius: iconRadius, style: .continuous)
        Group {
            if let favicon {
                Image(nsImage: favicon)
                    .resizable()
                    .interpolation(.high)
                    .scaledToFit()
                    .padding(2)
                    .background(Color.primary.opacity(0.06))
            } else {
                let tile = iconTile(server)
                ZStack {
                    tile.background ?? Color.primary.opacity(scheme == .dark ? 0.1 : 0.07)
                    if let symbol = tile.symbol {
                        Image(systemName: symbol).font(.system(size: 11, weight: .semibold))
                    } else {
                        Text(verbatim: tile.letter).font(.system(size: 11, weight: .bold))
                    }
                }
                .foregroundStyle(
                    tile.foreground ?? (tile.background == nil
                        ? portColor(server.port, dark: scheme == .dark) : .white)
                )
            }
        }
        .frame(width: 22, height: 22)
        .clipShape(shape)
        .overlay(shape.strokeBorder(Color.primary.opacity(0.12), lineWidth: 0.5))
    }
}
