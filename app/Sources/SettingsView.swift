import AppKit
import ServiceManagement
import SwiftUI
import UserNotifications

/// The Settings window: the four delays, the two lists, launch at login.
/// Every valid user edit is saved at once through `CoreModel.saveConfig`;
/// loading the form never saves (`SettingsSaveGate`).
struct SettingsView: View {
    @EnvironmentObject private var model: CoreModel
    @State private var draft: SettingsDraft?
    @State private var discovered: [String] = []
    @State private var gate = SettingsSaveGate()
    @State private var notificationsDenied = false
    /// Set when `config.toml` cannot be parsed: no form is shown, so nothing
    /// overwrites the user's file with defaults.
    @State private var loadError: String?

    var body: some View {
        Form {
            if let binding = Binding($draft) {
                DelaysSection(draft: binding)
                Section {
                    if discovered.isEmpty {
                        Text("None").foregroundStyle(.secondary)
                    }
                    ForEach(discovered, id: \.self) { dir in
                        Text(dir).lineLimit(1).truncationMode(.middle)
                    }
                } header: {
                    Text("Claude config directories: found automatically")
                } footer: {
                    Text("~/.claude, CLAUDE_CONFIG_DIR and every ~/.claude-* with a sessions/ folder, re-checked every scan.")
                }
                Section {
                    ListEditor(
                        items: binding.claudeConfigDirs,
                        placeholder: "~/work/.claude",
                        emptyText: "None"
                    )
                } header: {
                    Text("Also watch")
                } footer: {
                    Text("Other Claude config directories (each holding a sessions/ folder).")
                }
                Section {
                    ListEditor(
                        items: binding.excludeCommands,
                        placeholder: "redis-server",
                        emptyText: "None"
                    )
                } header: {
                    Text("Never track or kill commands containing")
                } footer: {
                    Text("Case-insensitive, anywhere in the command line, paths included.")
                }
                Section {
                    PortListEditor(ports: binding.excludePorts)
                } header: {
                    Text("Never track or kill ports")
                } footer: {
                    Text("A server listening on one of these, itself or through a child process.")
                }
                ForEach(binding.wrappedValue.validationErrors, id: \.self) { error in
                    Text(error).foregroundStyle(.red)
                }
                if let error = model.settingsError {
                    Text("Could not save: \(error)").foregroundStyle(.red)
                }
            } else if let loadError {
                Section("Configuration") {
                    Text(verbatim: "Could not read config.toml: \(loadError)")
                        .foregroundStyle(.red)
                    Text("Fix or delete the file; the watcher keeps its last valid settings meanwhile.")
                        .foregroundStyle(.secondary)
                }
            } else {
                ProgressView()
            }
            LaunchAtLoginSection()
            if notificationsDenied {
                Section("Notifications") {
                    Text("Notifications are off for Balance ton port in System Settings. Idle servers still show in the menu.")
                        .foregroundStyle(.secondary)
                    Button("Open Notification Settings") {
                        if let url = URL(string: "x-apple.systempreferences:com.apple.Notifications-Settings.extension?id=com.matthieuh.balancetonport") {
                            NSWorkspace.shared.open(url)
                        }
                    }
                }
            }
        }
        .formStyle(.grouped)
        .frame(width: 460)
        .frame(minHeight: 520)
        .task {
            do {
                let loaded = try await model.loadConfig()
                let fresh = SettingsDraft(loaded.config)
                discovered = loaded.discovered
                gate.loaded(fresh)
                draft = fresh
                loadError = nil
            } catch {
                loadError = error.localizedDescription
            }
            let settings = await UNUserNotificationCenter.current().notificationSettings()
            notificationsDenied = settings.authorizationStatus == .denied
        }
        .onChange(of: draft) { _, new in
            guard let new, gate.shouldSave(new) else { return }
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

/// The excluded ports, with a remove button per row and a field to add one.
private struct PortListEditor: View {
    @Binding var ports: [UInt16]
    @State private var newPort = ""

    var body: some View {
        if ports.isEmpty {
            Text("None").foregroundStyle(.secondary)
        }
        ForEach(ports, id: \.self) { port in
            HStack {
                Text(verbatim: String(port)).monospacedDigit()
                Spacer()
                Button {
                    ports.removeAll { $0 == port }
                } label: {
                    Image(systemName: "minus.circle.fill")
                }
                .buttonStyle(.borderless)
                .foregroundStyle(.secondary)
                .help("Remove")
            }
        }
        HStack {
            TextField("Add", text: $newPort, prompt: Text(verbatim: "5432"))
                .labelsHidden()
                .onSubmit(add)
            Button("Add", action: add)
                .disabled(addingPort(newPort, to: ports) == nil)
        }
    }

    private func add() {
        guard let next = addingPort(newPort, to: ports) else { return }
        ports = next
        newPort = ""
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
        // Approving in System Settings happens outside the app; pick it up
        // when the user comes back.
        .onReceive(NotificationCenter.default.publisher(for: NSApplication.didBecomeActiveNotification)) { _ in
            status = SMAppService.mainApp.status
        }
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
