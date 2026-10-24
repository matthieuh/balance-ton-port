import SwiftUI

/// The languages the app can show; `system` follows the macOS preferred languages.
enum AppLanguage: String, CaseIterable, Identifiable {
    case system, en, de, fr, es, zhHans = "zh-Hans", he, ja, uk

    var id: String { rawValue }

    /// Each language in its own tongue, never translated; only `system` is localized.
    var nativeName: String {
        switch self {
        case .system: String(localized: "System default")
        case .en: "English"
        case .de: "Deutsch"
        case .fr: "Français"
        case .es: "Español"
        case .zhHans: "简体中文"
        case .he: "עברית"
        case .ja: "日本語"
        case .uk: "Українська"
        }
    }
}

/// The language code to show: an explicit choice gives its own code; `system`
/// gives the first preferred language that is supported, else English.
func resolveLanguage(_ choice: AppLanguage, preferred: [String]) -> String {
    if choice != .system { return choice.rawValue }
    let supported = AppLanguage.allCases.filter { $0 != .system }
    for tag in preferred {
        let parts = tag.split(whereSeparator: { $0 == "-" || $0 == "_" }).map { $0.lowercased() }
        guard let lang = parts.first else { continue }
        if lang == "zh" {
            // Only Simplified Chinese is shipped: an explicit Hans script, or a
            // region that writes it, matches; Hant, TW, HK and MO do not.
            if parts.contains("hans") || (!parts.contains("hant") && parts.contains(where: { ["cn", "sg"].contains($0) })) {
                return AppLanguage.zhHans.rawValue
            }
            continue
        }
        if let match = supported.first(where: { $0.rawValue == lang }) { return match.rawValue }
    }
    return AppLanguage.en.rawValue
}

func layoutDirection(for code: String) -> LayoutDirection {
    code == AppLanguage.he.rawValue ? .rightToLeft : .leftToRight
}

/// Keeps ports, paths, commands, tool labels and URLs reading left to right
/// inside a right-to-left sentence.
func ltr(_ s: String) -> String {
    s.isEmpty ? s : "\u{2066}" + s + "\u{2069}"
}

/// The user's language choice, saved in the app's defaults (not in `config.toml`).
@MainActor
final class LanguageModel: ObservableObject {
    static let defaultsKey = "appLanguage"

    @Published var choice: AppLanguage {
        didSet { defaults.set(choice.rawValue, forKey: Self.defaultsKey) }
    }

    private let defaults: UserDefaults
    private let base: Bundle

    init(defaults: UserDefaults = .standard, base: Bundle = .main) {
        self.defaults = defaults
        self.base = base
        choice = defaults.string(forKey: Self.defaultsKey).flatMap(AppLanguage.init(rawValue:)) ?? .system
    }

    var code: String { resolveLanguage(choice, preferred: Locale.preferredLanguages) }

    var locale: Locale { Locale(identifier: code) }

    /// The `<code>.lproj` sub-bundle, else the base bundle.
    var bundle: Bundle {
        base.path(forResource: code, ofType: "lproj").flatMap(Bundle.init(path:)) ?? base
    }

    /// Resolves a key in the chosen language, for text outside SwiftUI views
    /// (notifications, alerts, `NSMenu`).
    func tr(_ key: String.LocalizationValue) -> String {
        String(localized: key, bundle: bundle, locale: locale)
    }
}
