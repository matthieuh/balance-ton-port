import AppKit
import SwiftUI

/// The menu, decision first: header and usage bar, the servers that need
/// you (one card each), the others as compact rows behind a fold, then the
/// footer.
struct MenuView: View {
    @EnvironmentObject private var model: CoreModel
    @Environment(\.openSettings) private var openSettings
    @State private var showAll = false

    var body: some View {
        let sections = menuSections(model.servers, visibleAllGood: showAll ? .max : allGoodVisibleRows)
        VStack(alignment: .leading, spacing: 0) {
            HeaderView(count: model.servers.count, memory: model.memory, servers: model.servers)
                .padding(.bottom, 12)
            notices

            if sections.needsYou.isEmpty {
                Label(nothingToDecide, systemImage: "checkmark.circle.fill")
                    .font(.system(size: 12, weight: .medium))
                    .foregroundStyle(.green)
                    .padding(.bottom, 12)
            } else {
                sectionHeader(sectionTitle("Needs you", count: sections.needsYou.count), color: .orange)
                    .padding(.bottom, 6)
                VStack(spacing: 8) {
                    ForEach(sections.needsYou, id: \.id) { server in
                        if let card = server.card {
                            NeedsYouCard(
                                server: server, card: card, favicon: model.icons[server.id],
                                onKeep: keepAction(server), onStop: stopAction(server)
                            )
                        }
                    }
                }
                .padding(.bottom, 12)
            }

            let allGoodCount = sections.allGood.count + sections.hiddenCount
            if allGoodCount > 0 {
                sectionHeader(sectionTitle("All good", count: allGoodCount), color: Color(nsColor: .tertiaryLabelColor))
                    .padding(.bottom, 2)
                ForEach(sections.allGood, id: \.id) { server in
                    ServerRowView(
                        server: server, favicon: model.icons[server.id],
                        notificationsOff: model.unnotified.contains(server.id),
                        onKeep: keepAction(server), onStop: stopAction(server)
                    )
                }
                if sections.hiddenCount > 0 {
                    Button {
                        showAll = true
                    } label: {
                        Text(verbatim: moreLabel(sections.hiddenCount))
                            .font(.system(size: 12))
                            .foregroundStyle(Color(nsColor: .tertiaryLabelColor))
                            .padding(.vertical, 4)
                            .padding(.horizontal, 6)
                            .contentShape(Rectangle())
                    }
                    .buttonStyle(PressableStyle())
                }
            }

            FooterView(recentKills: model.recentKills)
                .padding(.top, 6)
        }
        .padding(.horizontal, panelPadding)
        .padding(.top, panelPadding)
        .padding(.bottom, 10)
        .frame(width: panelWidth, alignment: .leading)
    }

    private func sectionHeader(_ title: String, color: Color) -> some View {
        Text(verbatim: title)
            .font(.system(size: 11, weight: .semibold))
            .foregroundStyle(color)
    }

    /// Keep and Stop exist for agent servers only; observed ones get theirs
    /// (with a confirmation) later.
    private func keepAction(_ server: ServerViewFfi) -> (() -> Void)? {
        server.managed ? { model.keep(server.id) } : nil
    }

    private func stopAction(_ server: ServerViewFfi) -> (() -> Void)? {
        server.managed && !server.excluded ? { model.kill(server) } : nil
    }

    /// Notifications off, another watcher, errors, the last manual kill.
    @ViewBuilder private var notices: some View {
        VStack(alignment: .leading, spacing: 4) {
            if let fix = menuNotificationFix(model.notificationPermission) {
                HStack(spacing: 4) {
                    Text("Notifications are off \u{00B7}")
                        .foregroundStyle(.secondary)
                    Button("Turn on") {
                        switch fix {
                        case .request:
                            NSApp.activate(ignoringOtherApps: true)
                            Task { await model.requestNotifications() }
                        case .openSettings:
                            NSApp.activate(ignoringOtherApps: true)
                            openSettings()
                        }
                    }
                    .buttonStyle(.link)
                }
            }
            if model.watchedElsewhere {
                Text("Watching is done by another process")
                    .foregroundStyle(.secondary)
            }
            if let error = model.lastError {
                Text(verbatim: error)
                    .foregroundStyle(.red)
                    .lineLimit(3)
            }
            if let notice = model.killNotice {
                Text(verbatim: notice)
                    .foregroundStyle(.secondary)
            }
        }
        .font(.caption)
        .padding(.bottom, hasNotices ? 10 : 0)
    }

    private var hasNotices: Bool {
        menuNotificationFix(model.notificationPermission) != nil || model.watchedElsewhere
            || model.lastError != nil || model.killNotice != nil
    }
}
