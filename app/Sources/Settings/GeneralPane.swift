import AppKit
import ServiceManagement
import SwiftUI

/// The General tab: language, notifications, the four delays, the two lists, launch at login.
/// Every valid user edit is saved at once through `CoreModel.saveConfig`;
/// loading the form never saves (`SettingsSaveGate`).
struct GeneralPane: View {
    @EnvironmentObject private var model: CoreModel
    @EnvironmentObject private var language: LanguageModel
    @State private var draft: SettingsDraft?
    @State private var discovered: [String] = []
    @State private var gate = SettingsSaveGate()
    /// Set when `config.toml` cannot be parsed: no form is shown, so nothing
    /// overwrites the user's file with defaults.
    @State private var loadError: String?

    private func iso(_ s: String) -> String { isolated(s, language.locale) }

    var body: some View {
        Form {
            Section {
                Picker(language.tr("Language"), selection: $language.choice) {
                    ForEach(AppLanguage.allCases) { Text(verbatim: $0.name(language)).tag($0) }
                }
            }
            NotificationsSection()
            if let binding = Binding($draft) {
                DelaysSection(draft: binding)
                Section {
                    if discovered.isEmpty {
                        Text(verbatim: language.tr("None")).foregroundStyle(.secondary)
                    }
                    ForEach(discovered, id: \.self) { dir in
                        Text(verbatim: iso(dir)).lineLimit(1).truncationMode(.middle)
                    }
                } header: {
                    Text(verbatim: language.tr("Claude config directories: found automatically"))
                } footer: {
                    Text(verbatim: language.tr(
                        "\(iso("~/.claude")), \(iso("CLAUDE_CONFIG_DIR")) and every \(iso("~/.claude-*")) with a \(iso("sessions/")) folder, re-checked every scan."
                    ))
                }
                Section {
                    ListEditor(
                        items: binding.claudeConfigDirs,
                        placeholder: "~/work/.claude",
                        emptyText: language.tr("None")
                    )
                } header: {
                    Text(verbatim: language.tr("Also watch"))
                } footer: {
                    Text(verbatim: language.tr("Other Claude config directories (each holding a \(iso("sessions/")) folder)."))
                }
                Section {
                    ListEditor(
                        items: binding.excludeCommands,
                        placeholder: "redis-server",
                        emptyText: language.tr("None")
                    )
                } header: {
                    Text(verbatim: language.tr("Never track or kill commands containing"))
                } footer: {
                    Text(verbatim: language.tr("Case-insensitive, anywhere in the command line, paths included."))
                }
                Section {
                    PortListEditor(ports: binding.excludePorts)
                } header: {
                    Text(verbatim: language.tr("Never track or kill ports"))
                } footer: {
                    Text(verbatim: language.tr("A server listening on one of these, itself or through a child process."))
                }
                ForEach(
                    binding.wrappedValue.validationMessages(bundle: language.bundle, locale: language.locale),
                    id: \.self
                ) { error in
                    Text(verbatim: error).foregroundStyle(.red)
                }
                if let error = model.settingsError {
                    Text(verbatim: language.tr("Could not save: \(error)")).foregroundStyle(.red)
                }
            } else if let loadError {
                Section(language.tr("Configuration")) {
                    Text(verbatim: language.tr("Could not read \(iso("config.toml")): \(iso(loadError))"))
                        .foregroundStyle(.red)
                    Text(verbatim: language.tr("Fix or delete the file; the watcher keeps its last valid settings meanwhile."))
                        .foregroundStyle(.secondary)
                }
            } else {
                ProgressView()
            }
            LaunchAtLoginSection()
        }
        .formStyle(.grouped)
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
        }
        .onChange(of: draft) { _, new in
            guard let new, gate.shouldSave(new) else { return }
            model.saveConfig(new.ffi)
        }
    }
}

/// Whether notifications reach the user, and the one step that fixes it.
private struct NotificationsSection: View {
    @EnvironmentObject private var model: CoreModel
    @EnvironmentObject private var language: LanguageModel
    @State private var testResult = TestNotificationResult.none
    @State private var testResetTask: Task<Void, Never>?

    var body: some View {
        Section(language.tr("Notifications")) {
            if let permission = model.notificationPermission {
                let setup = notificationSetup(permission, bundle: language.bundle, locale: language.locale)
                if setup.isOn {
                    Label {
                        Text(verbatim: setup.message)
                    } icon: {
                        Image(systemName: "checkmark.circle.fill").foregroundStyle(.green)
                    }
                } else {
                    Text(verbatim: setup.message)
                }
                if let hint = setup.hint {
                    Text(verbatim: hint).foregroundStyle(.secondary)
                }
                if setup.button != nil || setup.canSendTest {
                    HStack {
                        if let button = setup.button {
                            Button(button.title(bundle: language.bundle, locale: language.locale)) { perform(button) }
                        }
                        if setup.canSendTest {
                            Button(testNotificationButtonTitle(testResult, bundle: language.bundle, locale: language.locale)) { sendTest() }
                                .controlSize(.small)
                        }
                    }
                }
            } else {
                ProgressView().controlSize(.small)
            }
        }
        .task { await model.refreshNotificationPermission() }
        .onReceive(NotificationCenter.default.publisher(for: NSApplication.didBecomeActiveNotification)) { _ in
            Task { await model.refreshNotificationPermission() }
        }
    }

    /// Shows whether the test was added, then the plain title again after
    /// a few seconds.
    private func sendTest() {
        testResetTask?.cancel()
        testResetTask = Task {
            testResult = TestNotificationResult(added: await model.sendTestNotification())
            try? await Task.sleep(for: .seconds(testNotificationResetDelay))
            if !Task.isCancelled { testResult = .none }
        }
    }

    private func perform(_ button: NotificationSetupButton) {
        switch button {
        case .turnOn:
            Task { await model.requestNotifications() }
        case .openSettings:
            openNotificationSettings()
        }
    }
}

/// Opens this app's page in System Settings, Notifications (or the
/// Notifications pane when that page cannot be opened).
@MainActor
func openNotificationSettings() {
    let bundleID = Bundle.main.bundleIdentifier ?? "com.matthieuh.balancetonport"
    for url in notificationSettingsURLs(bundleID: bundleID) where NSWorkspace.shared.open(url) {
        return
    }
}

private struct DelaysSection: View {
    @EnvironmentObject private var language: LanguageModel
    @Binding var draft: SettingsDraft

    var body: some View {
        Section(language.tr("Delays")) {
            Stepper(value: $draft.deadSessionIdleMin, in: 1...1440) {
                LabeledContent(
                    language.tr("Kill after a dead session idles"),
                    value: minutes(draft.deadSessionIdleMin, bundle: language.bundle, locale: language.locale)
                )
            }
            Stepper(value: $draft.alertAfterMin, in: 1...1440) {
                LabeledContent(
                    language.tr("Alert after idle"),
                    value: minutes(draft.alertAfterMin, bundle: language.bundle, locale: language.locale)
                )
            }
            Stepper(value: $draft.killGraceSec, in: 1...300) {
                LabeledContent(
                    language.tr("Kill grace (TERM to KILL)"),
                    value: secondsText(draft.killGraceSec, bundle: language.bundle, locale: language.locale)
                )
            }
            Stepper(value: $draft.scanIntervalSec, in: 1...600) {
                LabeledContent(
                    language.tr("Scan every"),
                    value: secondsText(draft.scanIntervalSec, bundle: language.bundle, locale: language.locale)
                )
            }
        }
    }
}

/// A list with a remove button per row and a field to add one.
private struct ListEditor: View {
    @EnvironmentObject private var language: LanguageModel
    @Binding var items: [String]
    let placeholder: String
    let emptyText: String
    @State private var newItem = ""

    var body: some View {
        if items.isEmpty {
            Text(verbatim: emptyText).foregroundStyle(.secondary)
        }
        ForEach(items, id: \.self) { item in
            HStack {
                Text(verbatim: isolated(item, language.locale)).lineLimit(1).truncationMode(.middle)
                Spacer()
                Button {
                    items.removeAll { $0 == item }
                } label: {
                    Image(systemName: "minus.circle.fill")
                }
                .buttonStyle(.borderless)
                .foregroundStyle(.secondary)
                .help(language.tr("Remove"))
            }
        }
        HStack {
            TextField(language.tr("Add"), text: $newItem, prompt: Text(verbatim: placeholder))
                .labelsHidden()
                .onSubmit(add)
            Button(language.tr("Add"), action: add)
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
    @EnvironmentObject private var language: LanguageModel
    @Binding var ports: [UInt16]
    @State private var newPort = ""

    var body: some View {
        if ports.isEmpty {
            Text(verbatim: language.tr("None")).foregroundStyle(.secondary)
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
                .help(language.tr("Remove"))
            }
        }
        HStack {
            TextField(language.tr("Add"), text: $newPort, prompt: Text(verbatim: "5432"))
                .labelsHidden()
                .onSubmit(add)
            Button(language.tr("Add"), action: add)
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
    @EnvironmentObject private var language: LanguageModel
    @State private var status = SMAppService.mainApp.status
    @State private var error: String?

    var body: some View {
        Section(language.tr("Launch at login")) {
            Toggle(language.tr("Open Balance ton port at login"), isOn: Binding(
                get: { status == .enabled || status == .requiresApproval },
                set: { setEnabled($0) }
            ))
            switch status {
            case .requiresApproval:
                VStack(alignment: .leading, spacing: 6) {
                    Text(verbatim: language.tr("macOS needs your approval: turn Balance ton port on in System Settings, General, Login Items."))
                        .foregroundStyle(.secondary)
                    Button(language.tr("Open Login Items")) { SMAppService.openSystemSettingsLoginItems() }
                }
            case .notFound:
                Text(verbatim: language.tr("Not available for this copy of the app (move it to Applications)."))
                    .foregroundStyle(.secondary)
            default:
                EmptyView()
            }
            if let error {
                Text(verbatim: error).foregroundStyle(.red)
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
