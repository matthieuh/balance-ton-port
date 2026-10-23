import ImageIO
import SwiftUI

// The menu's colors and icon tiles: pure, compiled into both the app and
// BalanceTonPortTests.

/// Leak and idle cards are amber; the session ended ones rose.
func cardTint(_ card: CardFfi) -> Color {
    switch card.kind {
    case "leak", "idle_alert": return .orange
    default: return .pink
    }
}

/// The `NEEDS YOU` title: the mockup's amber on the dark panel, a deeper
/// amber on the light one.
func needsYouColor(dark: Bool) -> Color {
    Color(hex: dark ? 0xE9B764 : 0xA86A00)
}

/// `green`, `amber` or `red` memory health, as system colors.
func healthColor(_ health: String) -> Color {
    switch health {
    case "red": return .red
    case "amber": return .orange
    default: return .green
    }
}

/// A size label's color: plain (`nil`) while healthy.
func sizeColor(_ health: String) -> Color? {
    health == "green" ? nil : healthColor(health)
}

/// A port's color: pastel on the dark panel, deeper on the light one.
func portColor(_ port: UInt16, dark: Bool = true) -> Color {
    Color(hue: portHue(port), saturation: dark ? 0.45 : 0.75, brightness: dark ? 0.95 : 0.62)
}

/// `✳ Claude` orange, `◎ Codex` blue, `no agent` secondary (`nil`).
func agentBadge(_ s: ServerViewFfi) -> (text: String, color: Color?) {
    let text = agentBadgeText(s)
    switch s.agent {
    case "claude_code": return (text, .orange)
    case "codex": return (text, .blue)
    default: return (text, nil)
    }
}

/// What stands for a server without a favicon: its framework's tile, else
/// the first letter of its title in its port's color on the neutral tile.
struct IconTile: Equatable {
    let letter: String
    /// An SF Symbol drawn instead of the letter.
    let symbol: String?
    /// One color, or two for a diagonal gradient; `nil`: the neutral tile.
    let background: [Color]?
}

func iconTile(_ s: ServerViewFfi) -> IconTile {
    func tile(_ letter: String, _ hex: UInt32..., symbol: String? = nil) -> IconTile {
        IconTile(letter: letter, symbol: symbol, background: hex.map { Color(hex: $0) })
    }
    switch s.framework {
    case "Vite": return tile("V", 0x41D1FF, 0xBD34FE)
    case "Next.js": return tile("N", 0x000000)
    case "Astro": return tile("A", 0xFF5D01)
    case "Remix": return tile("R", 0x3992FF)
    case "Nuxt": return tile("N", 0x00C16A)
    case "SvelteKit": return tile("S", 0xFF3E00)
    case "Storybook": return tile("S", 0xFF4785)
    case "Expo": return tile("E", 0x4630EB)
    case "Django": return tile("D", 0x0C4B33)
    case "Rails": return tile("R", 0xCC0000)
    case "Flask": return tile("F", 0x3B3B3B)
    case "wrangler": return tile("W", 0xF38020, symbol: "cloud.fill")
    default:
        let title = titleParts(s).title
        let letter = title.first(where: { $0.isLetter || $0.isNumber }).map { String($0).uppercased() } ?? "?"
        return IconTile(letter: letter, symbol: nil, background: nil)
    }
}

extension Color {
    /// `0xRRGGBB`.
    init(hex: UInt32) {
        self.init(
            red: Double((hex >> 16) & 0xFF) / 255,
            green: Double((hex >> 8) & 0xFF) / 255,
            blue: Double(hex & 0xFF) / 255
        )
    }
}

// MARK: - Detail

/// The two most common colors of an image's opaque pixels, each channel
/// quantized to 16 levels, as `0xRRGGBB`; most common first. Empty when the
/// bytes are not an image or every pixel is transparent.
func dominantRGB(_ data: Data) -> [UInt32] {
    guard let source = CGImageSourceCreateWithData(data as CFData, nil),
          let image = CGImageSourceCreateImageAtIndex(source, 0, nil),
          let space = CGColorSpace(name: CGColorSpace.sRGB)
    else { return [] }
    // A favicon is small; a big image is sampled at 64 pt.
    let w = min(image.width, 64), h = min(image.height, 64)
    guard w > 0, h > 0 else { return [] }
    var pixels = [UInt8](repeating: 0, count: w * h * 4)
    let drawn = pixels.withUnsafeMutableBytes { buffer -> Bool in
        guard let ctx = CGContext(
            data: buffer.baseAddress, width: w, height: h, bitsPerComponent: 8, bytesPerRow: w * 4,
            space: space, bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue
        ) else { return false }
        ctx.interpolationQuality = .none
        ctx.draw(image, in: CGRect(x: 0, y: 0, width: w, height: h))
        return true
    }
    guard drawn else { return [] }
    var counts: [UInt32: Int] = [:]
    for i in stride(from: 0, to: pixels.count, by: 4) {
        let a = UInt32(pixels[i + 3])
        guard a >= 128 else { continue }
        // Un-premultiply, then keep the top 4 bits (x 17 gives 0x00...0xFF).
        func q(_ c: UInt8) -> UInt32 { (min(255, UInt32(c) * 255 / a) >> 4) * 17 }
        counts[q(pixels[i]) << 16 | q(pixels[i + 1]) << 8 | q(pixels[i + 2]), default: 0] += 1
    }
    return counts.sorted { $0.value != $1.value ? $0.value > $1.value : $0.key < $1.key }
        .prefix(2).map(\.key)
}

/// The hero's gradient colors from the favicon (`dominantRGB`), else
/// `fallback` (the port's color).
func dominantColors(_ png: Data, fallback: Color) -> [Color] {
    let rgb = dominantRGB(png)
    return rgb.isEmpty ? [fallback] : rgb.map { Color(hex: $0) }
}

/// A card's text on its tint: the mockup's pastel on the dark panel, a
/// deeper shade on the light one, so it stays readable.
func cardTextColor(_ card: CardFfi, dark: Bool) -> Color {
    switch card.kind {
    case "leak", "idle_alert": return Color(hex: dark ? 0xE9B764 : 0x9A5B00)
    default: return Color(hex: dark ? 0xFF9AA0 : 0xB4232F)
    }
}

/// The session chip's text: the agent's color, deeper on the light panel.
func agentChipColor(_ agent: String, dark: Bool) -> Color {
    switch agent {
    case "codex": return dark ? .blue : Color(hex: 0x1F5FBF)
    default: return dark ? .orange : Color(hex: 0xB8571A)
    }
}

/// The text of the detail's Stop button on its rose fill.
func stopTextColor(dark: Bool) -> Color {
    Color(hex: dark ? 0xFF9AA0 : 0xB4232F)
}

/// The memory chart's dotted leak line.
func leakLineColor(dark: Bool) -> Color {
    Color(hex: dark ? 0xE9B764 : 0xB07A10)
}

/// An event's rule and label on the memory chart, deeper on the light
/// panel.
func eventColor(_ kind: String, dark: Bool = true) -> Color {
    switch kind {
    case "started": return Color(hex: dark ? 0xF0A472 : 0xB8571A)
    case "session_ended": return Color(hex: dark ? 0xFF8A8F : 0xC0303C)
    case "alert": return Color(hex: dark ? 0xE9B764 : 0x9A5B00)
    case "restarted": return Color(hex: dark ? 0x7AB8FF : 0x1F5FBF)
    case "browser_connected": return Color(hex: dark ? 0x7FD6A0 : 0x1E7A45)
    default: return Color(hex: dark ? 0x9A9EAB : 0x5E6270)
    }
}
