import ServiceManagement
import SwiftUI
import UserNotifications

/// The Settings window: the four delays, the two lists, launch at login.
/// Every valid change is saved at once through `CoreModel.saveConfig`.
struct SettingsView: View {
    @EnvironmentObject private var model: CoreModel
    @State private var draft: SettingsDraft?
    @State private var notificationsDenied = false

    var body: some View {
        Form {
            if let binding = Binding($draft) {
                DelaysSection(draft: binding)
                Section {
                    ListEditor(
                        items: binding.claudeConfigDirs,
                        placeholder: "~/.claude",
                        emptyText: "None: no session is read"
                    )
                } header: {
                    Text("Claude config directories")
                } footer: {
                    Text("Where Claude Code keeps its sessions/ folder.")
                }
                Section {
                    ListEditor(
                        items: binding.excludeCommands,
                        placeholder: "redis-server",
                        emptyText: "None"
                    )
                } header: {
                    Text("Never kill commands containing")
                }
                ForEach(binding.wrappedValue.validationErrors, id: \.self) { error in
                    Text(error).foregroundStyle(.red)
                }
                if let error = model.settingsError {
                    Text("Could not save: \(error)").foregroundStyle(.red)
                }
            } else {
                ProgressView()
            }
            LaunchAtLoginSection()
            if notificationsDenied {
                Section("Notifications") {
                    Text("Notifications are off for Balance ton port in System Settings. Idle servers still show in the menu.")
                        .foregroundStyle(.secondary)
                }
            }
        }
        .formStyle(.grouped)
        .frame(width: 460)
        .frame(minHeight: 520)
        .task {
            draft = SettingsDraft(await model.loadConfig())
            let settings = await UNUserNotificationCenter.current().notificationSettings()
            notificationsDenied = settings.authorizationStatus == .denied
        }
        .onChange(of: draft) { old, new in
            // The first load is not a change; saving it would write defaults.
            guard let old, let new, old != new, new.isValid else { return }
            model.saveConfig(new.ffi)
        }
    }
}

private struct DelaysSection: View {
    @Binding var draft: SettingsDraft

    var body: some View {
        Section("Delays") {
            Stepper(value: $draft.deadSessionIdleMin, in: 1...1440) {
                LabeledContent("Kill after a dead session idles", value: "\(draft.deadSessionIdleMin) min")
            }
            Stepper(value: $draft.alertAfterMin, in: 1...1440) {
                LabeledContent("Alert after idle", value: "\(draft.alertAfterMin) min")
            }
            Stepper(value: $draft.killGraceSec, in: 1...300) {
                LabeledContent("Kill grace (TERM to KILL)", value: "\(draft.killGraceSec) s")
            }
            Stepper(value: $draft.scanIntervalSec, in: 1...600) {
                LabeledContent("Scan every", value: "\(draft.scanIntervalSec) s")
            }
        }
    }
}

/// A list with a remove button per row and a field to add one.
private struct ListEditor: View {
    @Binding var items: [String]
    let placeholder: String
    let emptyText: String
    @State private var newItem = ""

    var body: some View {
        if items.isEmpty {
            Text(emptyText).foregroundStyle(.secondary)
        }
        ForEach(items, id: \.self) { item in
            HStack {
                Text(item).lineLimit(1).truncationMode(.middle)
                Spacer()
                Button {
                    items.removeAll { $0 == item }
                } label: {
                    Image(systemName: "minus.circle.fill")
                }
                .buttonStyle(.borderless)
                .foregroundStyle(.secondary)
                .help("Remove")
            }
        }
        HStack {
            TextField("Add", text: $newItem, prompt: Text(placeholder))
                .labelsHidden()
                .onSubmit(add)
            Button("Add", action: add)
                .disabled(addingEntry(newItem, to: items) == nil)
        }
    }

    private func add() {
        guard let next = addingEntry(newItem, to: items) else { return }
        items = next
        newItem = ""
    }
}

private struct LaunchAtLoginSection: View {
    @State private var status = SMAppService.mainApp.status
    @State private var error: String?

    var body: some View {
        Section("Launch at login") {
            Toggle("Open Balance ton port at login", isOn: Binding(
                get: { status == .enabled || status == .requiresApproval },
                set: { setEnabled($0) }
            ))
            switch status {
            case .requiresApproval:
                VStack(alignment: .leading, spacing: 6) {
                    Text("macOS needs your approval: turn Balance ton port on in System Settings, General, Login Items.")
                        .foregroundStyle(.secondary)
                    Button("Open Login Items") { SMAppService.openSystemSettingsLoginItems() }
                }
            case .notFound:
                Text("Not available for this copy of the app (move it to Applications).")
                    .foregroundStyle(.secondary)
            default:
                EmptyView()
            }
            if let error {
                Text(error).foregroundStyle(.red)
            }
        }
        .onAppear { status = SMAppService.mainApp.status }
    }

    private func setEnabled(_ on: Bool) {
        do {
            if on {
                try SMAppService.mainApp.register()
            } else {
                try SMAppService.mainApp.unregister()
            }
            error = nil
        } catch {
            self.error = error.localizedDescription
        }
        status = SMAppService.mainApp.status
    }
}
