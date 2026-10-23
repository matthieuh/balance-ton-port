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
