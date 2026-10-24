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

    func testNativeNamesAreNotTranslated() {
        XCTAssertEqual(AppLanguage.fr.nativeName, "Français")
        XCTAssertEqual(AppLanguage.zhHans.nativeName, "简体中文")
        XCTAssertEqual(AppLanguage.he.nativeName, "עברית")
        XCTAssertEqual(AppLanguage.uk.nativeName, "Українська")
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
