import XCTest

final class LanguageTests: XCTestCase {
    func testResolveExplicitChoice() {
        XCTAssertEqual(resolveLanguage(.fr, preferred: ["de-DE"]), "fr")
        XCTAssertEqual(resolveLanguage(.zhHans, preferred: []), "zh-Hans")
    }

    func testResolveSystemPicksFirstSupported() {
        XCTAssertEqual(resolveLanguage(.system, preferred: ["he-IL", "en-US"]), "he")
        XCTAssertEqual(resolveLanguage(.system, preferred: ["pt-BR", "de-AT"]), "de")
        XCTAssertEqual(resolveLanguage(.system, preferred: ["pt-BR"]), "en")
    }

    func testChineseScripts() {
        XCTAssertEqual(resolveLanguage(.system, preferred: ["zh-Hans-CN"]), "zh-Hans")
        XCTAssertEqual(resolveLanguage(.system, preferred: ["zh-Hant-TW"]), "en")
        XCTAssertEqual(resolveLanguage(.system, preferred: ["zh-HK"]), "en")
    }

    func testHebrewIsRightToLeft() {
        XCTAssertEqual(layoutDirection(for: "he"), .rightToLeft)
        XCTAssertEqual(layoutDirection(for: "fr"), .leftToRight)
    }

    func testLtrIsolatesTechnicalParts() {
        let wrapped = ltr(":5173")
        XCTAssertTrue(wrapped.hasPrefix("\u{2066}"))
        XCTAssertTrue(wrapped.hasSuffix("\u{2069}"))
        XCTAssertEqual(ltr(""), "")
    }

    @MainActor
    func testNativeNamesAreNotTranslated() throws {
        let model = try testModel()
        let names: [AppLanguage: String] = [
            .en: "English", .de: "Deutsch", .fr: "Français", .es: "Español",
            .zhHans: "简体中文", .he: "עברית", .ja: "日本語", .uk: "Українська",
        ]
        for choice in [AppLanguage.en, .fr, .he] {
            model.choice = choice
            for (language, name) in names { XCTAssertEqual(language.name(model), name) }
        }
    }

    @MainActor
    func testSystemNameFollowsChosenLanguage() throws {
        let model = try testModel()
        model.choice = .fr
        XCTAssertEqual(AppLanguage.system.name(model), "Langue du système")
        model.choice = .he
        XCTAssertEqual(AppLanguage.system.name(model), "ברירת מחדל של המערכת")
        model.choice = .en
        XCTAssertEqual(AppLanguage.system.name(model), "System default")
    }

    /// Every language is in the catalog now, so the missing one is missing
    /// from a bundle holding only `en.lproj` and `fr.lproj`.
    @MainActor
    func testMissingLanguageFallsBackToEnglish() throws {
        let dir = FileManager.default.temporaryDirectory.appendingPathComponent("btp-lang-\(UUID().uuidString)")
        let tables = ["en": ["Language": "Language", "About": "About"], "fr": ["Language": "Langue", "About": "À propos"]]
        for (code, table) in tables {
            let lproj = dir.appendingPathComponent("\(code).lproj")
            try FileManager.default.createDirectory(at: lproj, withIntermediateDirectories: true)
            try (table as NSDictionary).write(to: lproj.appendingPathComponent("Localizable.strings"))
        }
        let defaults = try XCTUnwrap(UserDefaults(suiteName: "btp-lang-\(UUID().uuidString)"))
        let model = LanguageModel(defaults: defaults, base: try XCTUnwrap(Bundle(path: dir.path)))
        model.choice = .ja
        XCTAssertEqual(model.tr("Language"), "Language")
        XCTAssertEqual(model.tr("About"), "About")
    }

    @MainActor
    private func testModel() throws -> LanguageModel {
        let defaults = try XCTUnwrap(UserDefaults(suiteName: "btp-lang-\(UUID().uuidString)"))
        return LanguageModel(defaults: defaults, base: Bundle(for: LanguageTests.self))
    }

    @MainActor
    func testChoiceIsPersisted() throws {
        let defaults = try XCTUnwrap(UserDefaults(suiteName: "btp-lang-\(UUID().uuidString)"))
        let model = LanguageModel(defaults: defaults)
        XCTAssertEqual(model.choice, .system)
        model.choice = .he
        XCTAssertEqual(LanguageModel(defaults: defaults).choice, .he)
    }

    @MainActor
    func testBundleResolvesCatalogKeyInChosenLanguage() throws {
        let defaults = try XCTUnwrap(UserDefaults(suiteName: "btp-lang-\(UUID().uuidString)"))
        let model = LanguageModel(defaults: defaults, base: Bundle(for: LanguageTests.self))
        model.choice = .fr
        XCTAssertEqual(model.tr("Language"), "Langue")
        model.choice = .he
        XCTAssertEqual(model.tr("Language"), "שפה")
        model.choice = .en
        XCTAssertEqual(model.tr("Language"), "Language")
    }
}
