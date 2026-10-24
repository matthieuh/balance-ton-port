import XCTest

/// The String Catalog itself (`Sources/Localizable.xcstrings`, read as JSON):
/// every key in every shipped language, placeholders and technical parts kept,
/// and every translation waiting for a native review (`LOCALIZATION.md`).
final class CatalogTests: XCTestCase {
    /// The shipped languages besides English, with the plural forms each must carry.
    private let pluralForms: [String: Set<String>] = [
        "de": ["one", "other"], "fr": ["one", "other"], "es": ["one", "other"],
        "zh-Hans": ["other"], "he": ["one", "two", "other"], "ja": ["other"],
        "uk": ["one", "few", "many", "other"],
    ]

    private var repo: URL {
        URL(fileURLWithPath: #filePath).deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent()
    }

    private func strings() throws -> [String: [String: Any]] {
        let url = repo.appendingPathComponent("app/Sources/Localizable.xcstrings")
        let catalog = try JSONSerialization.jsonObject(with: Data(contentsOf: url)) as? [String: Any]
        return try XCTUnwrap(catalog?["strings"] as? [String: [String: Any]])
    }

    private func localization(_ entry: [String: Any], _ language: String) -> [String: Any]? {
        (entry["localizations"] as? [String: Any])?[language] as? [String: Any]
    }

    /// A localization's units by plural form (`""` for a plain string unit).
    private func units(_ loc: [String: Any]) -> [String: [String: Any]] {
        if let unit = loc["stringUnit"] as? [String: Any] { return ["": unit] }
        let plural = ((loc["variations"] as? [String: Any])?["plural"] as? [String: Any]) ?? [:]
        var out: [String: [String: Any]] = [:]
        for (form, value) in plural {
            if let unit = (value as? [String: Any])?["stringUnit"] as? [String: Any] { out[form] = unit }
        }
        return out
    }

    /// Every (key, language, form, value, state) of the translations.
    private func translations() throws -> [(key: String, language: String, form: String, value: String, state: String)] {
        var out: [(String, String, String, String, String)] = []
        for (key, entry) in try strings() {
            for language in pluralForms.keys.sorted() {
                guard let loc = localization(entry, language) else { continue }
                for (form, unit) in units(loc) {
                    out.append((key, language, form, unit["value"] as? String ?? "", unit["state"] as? String ?? ""))
                }
            }
        }
        return out
    }

    /// The English value for a translation's form: its own form when English
    /// has it, else `other`.
    private func english(_ entry: [String: Any], form: String) -> String? {
        guard let en = localization(entry, "en") else { return nil }
        let u = units(en)
        return (u[form] ?? u["other"] ?? u[""])?["value"] as? String
    }

    func testEveryKeyHasEveryLanguage() throws {
        let all = try strings()
        XCTAssertGreaterThan(all.count, 100)
        for (key, entry) in all {
            let en = try XCTUnwrap(localization(entry, "en"), key)
            let englishIsPlural = en["variations"] != nil
            for (language, forms) in pluralForms {
                guard let loc = localization(entry, language) else {
                    XCTFail("\(key.debugDescription): no \(language)")
                    continue
                }
                let u = units(loc)
                if englishIsPlural {
                    XCTAssertEqual(Set(u.keys), forms, "\(key.debugDescription) \(language): plural forms")
                } else {
                    XCTAssertEqual(Set(u.keys), [""], "\(key.debugDescription) \(language): a plain string")
                }
                for (form, unit) in u {
                    XCTAssertFalse((unit["value"] as? String ?? "").isEmpty, "\(key.debugDescription) \(language) \(form): empty")
                }
            }
        }
    }

    /// The specifiers of a format string in argument order: `%@`, `%lld`, and
    /// positional `%2$@`, as (position or nil, type); `%%` as a literal.
    private func specifiers(_ s: String) -> (args: [(Int?, String)], percents: Int) {
        let regex = try! NSRegularExpression(pattern: "%(?:(\\d+)\\$)?(@|lld|%)")
        var args: [(Int?, String)] = []
        var percents = 0
        for m in regex.matches(in: s, range: NSRange(s.startIndex..., in: s)) {
            let type = String(s[Range(m.range(at: 2), in: s)!])
            if type == "%" { percents += 1; continue }
            let position = Range(m.range(at: 1), in: s).flatMap { Int(s[$0]) }
            args.append((position, type))
        }
        return (args, percents)
    }

    func testPlaceholdersMatchEnglish() throws {
        let all = try strings()
        for t in try translations() {
            let source = try XCTUnwrap(english(all[t.key]!, form: t.form), t.key)
            let expected = specifiers(source)
            let found = specifiers(t.value)
            let label = "\(t.key.debugDescription) \(t.language) \(t.form)"
            XCTAssertEqual(found.percents, expected.percents, "\(label): %%")
            let types = expected.args.map(\.1)
            if found.args.contains(where: { $0.0 != nil }) {
                // Positional: every argument once, each with English's type.
                XCTAssertTrue(found.args.allSatisfy { $0.0 != nil }, "\(label): mixed positional")
                XCTAssertEqual(found.args.compactMap(\.0).sorted(), Array(1..<(types.count + 1)), label)
                for (position, type) in found.args {
                    if let p = position, p >= 1, p <= types.count { XCTAssertEqual(type, types[p - 1], label) }
                }
            } else {
                XCTAssertEqual(found.args.map(\.1), types, label)
            }
        }
    }

    /// Ports, paths, commands, product names and units that stay as they are.
    private func technicalTokens(_ english: String) -> [String] {
        let patterns = ["`[^`]+`", "\\bbtp\\b", ":\\d+", "~/\\S*", "(?<!\\d)\\d{2,}(?!\\d)"]
        var out: [String] = []
        for p in patterns {
            let regex = try! NSRegularExpression(pattern: p)
            for m in regex.matches(in: english, range: NSRange(english.startIndex..., in: english)) {
                out.append(String(english[Range(m.range, in: english)!]))
            }
        }
        let names = ["Balance ton port", "GitHub", "macOS", "CPU", "MB", "GB", "Claude", "Codex", "Vite", "TERM", "KILL", "my-project \u{203A} web"]
        return out + names.filter { english.contains($0) }
    }

    func testTechnicalTokensUntranslated() throws {
        let all = try strings()
        for t in try translations() {
            let source = try XCTUnwrap(english(all[t.key]!, form: t.form), t.key)
            for token in technicalTokens(source) {
                XCTAssertTrue(t.value.contains(token), "\(t.key.debugDescription) \(t.language) \(t.form): \(token) changed in \(t.value.debugDescription)")
            }
        }
    }

    func testNoEmDash() throws {
        for (key, entry) in try strings() {
            XCTAssertFalse(key.contains("\u{2014}"), key)
            for language in ["en"] + pluralForms.keys {
                guard let loc = localization(entry, language) else { continue }
                for (form, unit) in units(loc) {
                    XCTAssertFalse((unit["value"] as? String ?? "").contains("\u{2014}"), "\(key.debugDescription) \(language) \(form)")
                }
            }
        }
    }

    /// The languages whose row in LOCALIZATION.md's status table no longer
    /// says the native review is pending.
    private func reviewedLanguages() throws -> Set<String> {
        let doc = try String(contentsOf: repo.appendingPathComponent("LOCALIZATION.md"), encoding: .utf8)
        var reviewed: Set<String> = []
        for line in doc.split(separator: "\n") where line.hasPrefix("|") {
            let cells = line.split(separator: "|").map { $0.trimmingCharacters(in: .whitespaces) }
            guard cells.count >= 4 else { continue }
            let code = cells[1].trimmingCharacters(in: CharacterSet(charactersIn: "`"))
            if pluralForms[code] != nil, cells[3].lowercased() != "pending" { reviewed.insert(code) }
        }
        return reviewed
    }

    func testTranslationsMarkedForReview() throws {
        let reviewed = try reviewedLanguages()
        for t in try translations() where !reviewed.contains(t.language) {
            XCTAssertEqual(t.state, "needs_review", "\(t.key.debugDescription) \(t.language) \(t.form)")
        }
    }
}
