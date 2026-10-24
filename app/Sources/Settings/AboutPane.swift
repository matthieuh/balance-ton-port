import SwiftUI

/// The About tab: version and updates.
struct AboutPane: View {
    @EnvironmentObject private var updates: UpdateController

    var body: some View {
        Form {
            Section {
                LabeledContent("Balance ton port", value: UpdateController.appVersion(Bundle.main.infoDictionary ?? [:]))
            }
            Section {
                Button("Check for Updates…") { updates.checkNow() }
                    .disabled(!updates.canCheck)
                Toggle("Update automatically", isOn: $updates.automatic)
            } footer: {
                Text("Updates are signed and downloaded from GitHub.")
            }
        }
        .formStyle(.grouped)
    }
}
