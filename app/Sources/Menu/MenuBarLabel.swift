import AppKit
import SwiftUI

/// The menu bar item: the pig, tinted with the count beside it when something
/// needs a look (`menuBarStatus`), the plain template pig otherwise.
struct MenuBarLabel: View {
    @EnvironmentObject private var model: CoreModel
    @EnvironmentObject private var memory: MemoryPressureMonitor
    @EnvironmentObject private var language: LanguageModel

    var body: some View {
        let needsYou = menuSections(model.servers).needsYou.count
        let label = menuBarAccessibilityLabel(
            needsYou: needsYou, pressure: memory.pressure, bundle: language.bundle, locale: language.locale
        )
        Image(nsImage: menuBarImage(menuBarStatus(needsYou: needsYou, pressure: memory.pressure)))
            .accessibilityLabel(Text(verbatim: label))
            .help(label)
    }
}

/// A tinted image is not a template, so the menu bar keeps its color.
@MainActor
func menuBarImage(_ status: MenuBarStatus) -> NSImage {
    let pig = appBundle.image(forResource: "MenuIcon") ?? NSImage()
    guard let tint = status.tint else { return pig }
    let color: NSColor = tint == .critical ? .systemRed : .systemOrange
    let side: CGFloat = 18
    let gap: CGFloat = 3
    let count = status.count.map {
        NSAttributedString(string: $0, attributes: [
            .font: NSFont.monospacedDigitSystemFont(ofSize: 13, weight: .semibold),
            .foregroundColor: color,
        ])
    }
    let countWidth = count.map { gap + ceil($0.size().width) } ?? 0
    let image = NSImage(size: NSSize(width: side + countWidth, height: side), flipped: false) { _ in
        let pigRect = NSRect(x: 0, y: 0, width: side, height: side)
        pig.draw(in: pigRect)
        color.set()
        pigRect.fill(using: .sourceAtop)
        if let count {
            count.draw(at: NSPoint(x: side + gap, y: (side - count.size().height) / 2))
        }
        return true
    }
    image.isTemplate = false
    return image
}

/// The bundle holding the app's assets: the app itself, or the screenshots
/// test bundle that compiles the same sources.
let appBundle = Bundle(for: CoreModel.self)
