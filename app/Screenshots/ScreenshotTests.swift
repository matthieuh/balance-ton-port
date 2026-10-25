import AppKit
import SwiftUI
import XCTest

/// Renders the README screenshots from made-up servers, so no real machine,
/// project or open port shows. Skipped unless `BTP_SCREENSHOTS_DIR` is set
/// (`scripts/screenshots.sh` sets it).
@MainActor
final class ScreenshotTests: XCTestCase {
    func testRenderReadmeScreenshots() throws {
        guard let dir = ProcessInfo.processInfo.environment["BTP_SCREENSHOTS_DIR"], !dir.isEmpty else {
            throw XCTSkip("BTP_SCREENSHOTS_DIR is not set")
        }
        let out = URL(fileURLWithPath: dir, isDirectory: true)
        try FileManager.default.createDirectory(at: out, withIntermediateDirectories: true)
        // The calm pig must stay a template, or the menu bar cannot tint it.
        XCTAssertEqual(menuBarImage(menuBarStatus(needsYou: 0, pressure: .normal)).isTemplate, true)
        let model = CoreModel(preview: Demo.servers, memory: Demo.memory, recentKills: Demo.kills)

        for language in [AppLanguage.en] {
            let lang = languageModel(language)
            for dark in [false, true] {
                let suffix = "\(language.rawValue)-\(dark ? "dark" : "light")"
                try render(
                    panel(MenuView(), dark: dark), model: model, language: lang, dark: dark,
                    to: out.appendingPathComponent("menu-\(suffix).png")
                )
                try render(
                    panel(Demo.detail(model: model), dark: dark), model: model, language: lang, dark: dark,
                    to: out.appendingPathComponent("detail-\(suffix).png")
                )
            }
        }
        for dark in [false, true] {
            try render(
                MenuBarStrip(dark: dark), model: model, language: languageModel(.en), dark: dark,
                to: out.appendingPathComponent("menubar-\(dark ? "dark" : "light").png")
            )
        }
    }

    private func languageModel(_ language: AppLanguage) -> LanguageModel {
        let defaults = UserDefaults(suiteName: "btp-screenshots-\(language.rawValue)")!
        let model = LanguageModel(defaults: defaults, base: Bundle(for: ScreenshotTests.self))
        model.choice = language
        return model
    }

    /// The menu panel's look without the menu bar's window: an opaque
    /// rounded panel on a transparent margin.
    private func panel<V: View>(_ content: V, dark: Bool) -> some View {
        let shape = RoundedRectangle(cornerRadius: 12, style: .continuous)
        return content
            .background(Color(nsColor: .windowBackgroundColor))
            .clipShape(shape)
            .overlay(shape.strokeBorder(Color.primary.opacity(dark ? 0.18 : 0.12), lineWidth: 1))
            .shadow(color: .black.opacity(0.25), radius: 14, y: 6)
            .padding(28)
    }

    /// Lays `view` out in an offscreen window, lets its measured heights
    /// settle, then writes it at 2x as a PNG.
    private func render<V: View>(
        _ view: V, model: CoreModel, language: LanguageModel, dark: Bool, to url: URL
    ) throws {
        let root = view
            .environmentObject(model)
            .environmentObject(language)
            .environment(\.locale, language.locale)
            .environment(\.layoutDirection, .leftToRight)
        let host = NSHostingView(rootView: root)
        let window = NSWindow(
            contentRect: NSRect(x: -10_000, y: -10_000, width: 600, height: 600),
            styleMask: .borderless, backing: .buffered, defer: false
        )
        window.appearance = NSAppearance(named: dark ? .darkAqua : .aqua)
        window.isOpaque = false
        window.backgroundColor = .clear
        window.contentView = host
        for _ in 0..<4 {
            window.setContentSize(host.fittingSize)
            host.layoutSubtreeIfNeeded()
            RunLoop.main.run(until: Date().addingTimeInterval(0.15))
        }
        let size = host.bounds.size
        let rep = try XCTUnwrap(NSBitmapImageRep(
            bitmapDataPlanes: nil, pixelsWide: Int(size.width * 2), pixelsHigh: Int(size.height * 2),
            bitsPerSample: 8, samplesPerPixel: 4, hasAlpha: true, isPlanar: false,
            colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0
        ))
        rep.size = size
        host.cacheDisplay(in: host.bounds, to: rep)
        try XCTUnwrap(rep.representation(using: .png, properties: [:])).write(to: url)
    }
}

/// The pig as the menu bar shows it: calm, two servers need you, memory
/// pressure critical.
private struct MenuBarStrip: View {
    let dark: Bool

    var body: some View {
        HStack(spacing: 18) {
            Image("MenuIcon", bundle: appBundle).renderingMode(.template).foregroundStyle(.primary)
            Image(nsImage: menuBarImage(menuBarStatus(needsYou: 2, pressure: .normal)))
            Image(nsImage: menuBarImage(menuBarStatus(needsYou: 3, pressure: .critical)))
        }
        .padding(.horizontal, 14)
        .frame(height: 30)
        .background(
            (dark ? Color(white: 0.16) : Color(white: 0.93)),
            in: RoundedRectangle(cornerRadius: 8, style: .continuous)
        )
        .padding(12)
    }
}
