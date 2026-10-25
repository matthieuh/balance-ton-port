import AppKit
import SwiftUI

/// The menu, decision first: header and usage bar, the servers that need
/// you (one card each), the others as compact rows behind a fold, then the
/// footer. A click on a card or a row slides its detail in, in the same
/// panel.
struct MenuView: View {
    @EnvironmentObject private var model: CoreModel
    @EnvironmentObject private var language: LanguageModel
    @Environment(\.openSettings) private var openSettings
    @Environment(\.colorScheme) private var scheme
    @StateObject private var nav = MenuNavigation()
    @State private var showAll = false
    /// The cards and rows' natural height, measured, so the scroll view is
    /// only as tall as its content up to `sectionsMaxHeight`.
    @State private var sectionsHeight: CGFloat = 0

    var body: some View {
        ZStack(alignment: .top) {
            if let id = nav.detail, let server = model.servers.first(where: { $0.id == id }) {
                DetailView(server: server)
                    .transition(.move(edge: .trailing))
            } else {
                list
                    .transition(.move(edge: .leading))
            }
        }
        .frame(width: panelWidth, alignment: .top)
        .clipped()
        .environmentObject(nav)
        .onChange(of: model.servers) { _, servers in
            let live = Set(servers.map(\.id))
            PreviewStore.shared.retain(live)
            // A server stopped (or replaced by its restart) closes its detail.
            if let id = nav.detail, !live.contains(id) { nav.back() }
        }
    }

    /// The servers' list: what the panel shows without a detail open.
    private var list: some View {
        let sections = menuSections(model.servers, visibleAllGood: showAll ? .max : allGoodVisibleRows)
        return VStack(alignment: .leading, spacing: 0) {
            VStack(alignment: .leading, spacing: 0) {
                HeaderView(count: model.servers.count, memory: model.memory, servers: model.servers)
                    .padding(.bottom, 12)
                notices
            }
            .padding(.horizontal, panelPadding)
            // Inside the scroll view, so the cards' shadows are not clipped.
            ScrollView(.vertical) {
                sectionsView(sections)
                    .padding(.horizontal, panelPadding)
                    .background(GeometryReader { geo in
                        Color.clear.preference(key: SectionsHeightKey.self, value: geo.size.height)
                    })
            }
            .scrollIndicators(.automatic)
            .frame(height: min(sectionsHeight, maxSectionsHeight))
            .onPreferenceChange(SectionsHeightKey.self) { sectionsHeight = $0 }
            FooterView(recentKills: model.recentKills)
                .padding(.top, 6)
                .padding(.horizontal, panelPadding)
        }
        .padding(.top, panelPadding)
        .padding(.bottom, 10)
        .frame(width: panelWidth, alignment: .leading)
    }

    private var maxSectionsHeight: CGFloat {
        sectionsMaxHeight(screenHeight: NSScreen.main?.visibleFrame.height ?? 800)
    }

    @ViewBuilder private func sectionsView(_ sections: MenuSections) -> some View {
        VStack(alignment: .leading, spacing: 0) {
            if sections.needsYou.isEmpty {
                Label(nothingToDecideText(bundle: language.bundle, locale: language.locale), systemImage: "checkmark.circle.fill")
                    .font(.system(size: 12, weight: .medium))
                    .foregroundStyle(.green)
                    .padding(.bottom, 12)
            } else {
                sectionHeader(
                    sectionTitle(language.tr("Needs you"), count: sections.needsYou.count, locale: language.locale),
                    color: needsYouColor(dark: scheme == .dark)
                )
                    .padding(.bottom, 6)
                VStack(spacing: 8) {
                    ForEach(sections.needsYou, id: \.id) { server in
                        if let card = server.card {
                            NeedsYouCard(
                                server: server, card: card, favicon: model.icons[server.id],
                                note: model.rowNotes[server.id]
                            )
                        }
                    }
                }
                .padding(.bottom, 12)
            }

            let allGoodCount = sections.allGood.count + sections.hiddenCount
            if allGoodCount > 0 {
                sectionHeader(
                    sectionTitle(language.tr("All good"), count: allGoodCount, locale: language.locale),
                    color: Color(nsColor: .tertiaryLabelColor)
                )
                    .padding(.bottom, 2)
                ForEach(sections.allGood, id: \.id) { server in
                    ServerRowView(
                        server: server, favicon: model.icons[server.id],
                        notificationsOff: model.unnotified.contains(server.id),
                        note: model.rowNotes[server.id]
                    )
                }
                if sections.hiddenCount > 0 {
                    Button {
                        showAll = true
                    } label: {
                        Text(verbatim: moreLabel(sections.hiddenCount, bundle: language.bundle, locale: language.locale))
                            .font(.system(size: 12))
                            .foregroundStyle(.secondary)
                            .padding(.vertical, 4)
                            .padding(.horizontal, 6)
                            .contentShape(Rectangle())
                    }
                    .buttonStyle(PressableStyle())
                }
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    private func sectionHeader(_ title: String, color: Color) -> some View {
        Text(verbatim: title)
            .font(.system(size: 11, weight: .semibold))
            .foregroundStyle(color)
    }

    /// Notifications off, another watcher, errors, the last manual kill.
    @ViewBuilder private var notices: some View {
        VStack(alignment: .leading, spacing: 4) {
            if let fix = menuNotificationFix(model.notificationPermission) {
                HStack(spacing: 4) {
                    Text(verbatim: language.tr("Notifications are off") + " \u{00B7}")
                        .foregroundStyle(.secondary)
                    Button(language.tr("Turn on")) {
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
                Text(verbatim: language.tr("Watching is done by another process"))
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

private struct SectionsHeightKey: PreferenceKey {
    static let defaultValue: CGFloat = 0
    static func reduce(value: inout CGFloat, nextValue: () -> CGFloat) {
        value = max(value, nextValue())
    }
}
