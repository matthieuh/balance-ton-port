# Localization

The macOS app speaks eight languages. Settings, General, Language picks one of
them, or "System default", which follows the first macOS preferred language the
app supports and falls back to English.

- English (`en`), the source language
- German (`de`)
- French (`fr`)
- Spanish (`es`)
- Simplified Chinese (`zh-Hans`)
- Hebrew (`he`), shown right to left
- Japanese (`ja`)
- Ukrainian (`uk`)

All app copy lives in one String Catalog, `app/Sources/Localizable.xcstrings`.
The `btp` command line tool and the Claude Code plugin stay in English.

## Status

The translations were written with an AI model and wait for a native speaker's
review. A language moves from `pending` to the reviewer's name once someone has
read every string in it.

| Language | Code | Translated by | Native review |
|---|---|---|---|
| German | `de` | AI draft | pending |
| French | `fr` | AI draft | pending |
| Spanish | `es` | AI draft | pending |
| Simplified Chinese | `zh-Hans` | AI draft | pending |
| Hebrew | `he` | AI draft | pending |
| Japanese | `ja` | AI draft | pending |
| Ukrainian | `uk` | AI draft | pending |

`CatalogTests` reads this table: every string of a language still `pending`
must keep the state `needs_review` in the catalog.

## Rules

- Never translate `Balance ton port`, project names, paths (`~/.claude`),
  commands, ports (`:5173`), tool labels (`Vite`, `Bun dev`), `btp`, `Claude`,
  `Codex`, `MB` and `GB`.
- Keep every placeholder (`%@`, `%lld`, `%%`) with the same count and type.
  When the word order needs it, number them (`%2$@ … %1$@`).
- Fill every plural form the language has: Ukrainian `one`, `few`, `many`,
  `other`; Hebrew `one`, `two`, `other`; Japanese and Chinese `other`; German,
  French and Spanish `one`, `other`.
- No em dash (U+2014) in any language. Use the language's own punctuation.
- Short, plain macOS wording: button labels are a verb or two, and the names
  of macOS settings (System Settings, Login Items, Allow notifications) are the
  ones macOS shows in that language.

`CatalogTests` (in `app/Tests`) checks the placeholders, the technical parts,
the plural forms and the em dash.

### To verify on a Mac in that language

These name macOS controls and were written without a Mac set to the language:

- Ukrainian: `Елементи входу` (Login Items), `Попередження` (the Alerts
  notification style), `Системні параметри` (System Settings).
- Hebrew: `פריטי התחברות` (Login Items), `אפשר עדכונים` (the Allow
  notifications switch), `באנרים` and `התראות` (the Banners and Alerts
  notification styles; macOS may name the Alerts style differently, since it
  calls notifications `עדכונים`).
- Spanish: `Ítems de inicio de sesión`. French: `Ouverture`. German: `Hinweise`
  (Alerts style). Japanese: `通知パネル` (Alerts style).

## How to review a language

1. Open `app/Sources/Localizable.xcstrings` in Xcode and pick the language in
   the catalog's sidebar.
2. Read each string next to its English source. Fix what reads wrong, keeping
   the rules above. Running the app in that language (Settings, General,
   Language) shows each string in place: the menu, a card, the detail view,
   Settings and a test notification.
3. Mark each string you have checked as reviewed (its state changes from
   `needs_review` to `translated`).
4. Once every string of the language is reviewed, replace `pending` with your
   name in the status table above. A partial review keeps `pending`, and its
   unchecked strings keep `needs_review`. Then open a pull request.

## How to add a language

1. Add the language to the catalog (Xcode: the + under the catalog's language
   list) and translate every string, following the rules above. Leave each one
   `needs_review`.
2. Add a case to `AppLanguage` in `app/Sources/Language.swift`, with the
   language's name in its own tongue, and teach `resolveLanguage` any script or
   region rule it needs. Right-to-left languages also go in
   `layoutDirection(for:)`.
3. `knownRegions` in the generated Xcode project comes from the catalog
   (xcodegen reads its languages), so `xcodegen generate` picks it up.
4. Add the language, with its plural forms, to `pluralForms` in
   `app/Tests/CatalogTests.swift`, and a row to the lists and the status table
   in this file.
