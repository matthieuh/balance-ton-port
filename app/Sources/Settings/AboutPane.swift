import SwiftUI

/// The About tab: version and updates.
struct AboutPane: View {
    @EnvironmentObject private var updates: UpdateController
    @EnvironmentObject private var language: LanguageModel

    var body: some View {
        Form {
            Section {
                LabeledContent(String("Balance ton port"), value: UpdateController.appVersion(Bundle.main.infoDictionary ?? [:]))
            }
            Section {
                Button(language.tr("Check for Updates…")) { updates.checkNow() }
                    .disabled(!UpdateController.enabled || !updates.canCheck)
                Toggle(language.tr("Update automatically"), isOn: $updates.automatic)
                    .disabled(!UpdateController.enabled)
            } footer: {
                Text(verbatim: language.tr("Updates are signed and downloaded from GitHub."))
            }
        }
        .formStyle(.grouped)
    }
}
