import AppKit
import SwiftUI

/// Which server's detail the panel shows, if any. A click on a row or a
/// card (or ⋯ Details…) opens it; ‹ and Esc go back.
@MainActor
final class MenuNavigation: ObservableObject {
    @Published private(set) var detail: ServerID?

    func open(_ id: ServerID) {
        withAnimation(.easeInOut(duration: 0.25)) { detail = id }
    }

    func back() {
        withAnimation(.easeInOut(duration: 0.25)) { detail = nil }
    }
}

/// One server's story inside the menu panel (mockup v2), with what it
/// fetches: the resume command, favicon colors and config once per opened
/// server, history and events on every pass, the processes only while
/// their list is open.
struct DetailView: View {
    let server: ServerViewFfi
    @EnvironmentObject private var model: CoreModel
    @EnvironmentObject private var nav: MenuNavigation
    @ObservedObject private var previews = PreviewStore.shared
    @State private var once = DetailOnce()
    @State private var live = DetailLive()
    @State private var showProcesses = false
    @State private var contentHeight: CGFloat = 0

    var body: some View {
        ScrollView(.vertical) {
            DetailContent(
                server: server, once: once, live: live, favicon: model.icons[server.id],
                preview: previews.image(for: server.id), note: model.rowNotes[server.id],
                restarting: model.isRestarting(server.id), showProcesses: $showProcesses,
                onBack: { nav.back() }, more: AnyView(MoreMenu(server: server, inDetail: true, onImage: false)),
                moreOnImage: AnyView(MoreMenu(server: server, inDetail: true, onImage: true)),
                perform: perform
            )
            .background(GeometryReader { geo in
                Color.clear.preference(key: DetailHeightKey.self, value: geo.size.height)
            })
        }
        .scrollIndicators(.automatic)
        // Opaque, as the mockup: the panel's translucent material would
        // veil the detail in light mode.
        .background(Color(nsColor: .windowBackgroundColor))
        .frame(height: min(contentHeight, detailMaxHeight(screenHeight: NSScreen.main?.visibleFrame.height ?? 800)))
        .onPreferenceChange(DetailHeightKey.self) { contentHeight = $0 }
        .frame(width: panelWidth)
        .background(EscapeKey { nav.back() })
        .task(id: server.id) {
            once = await model.detailOnce(server.id)
        }
        .task(id: LiveKey(id: server.id, pass: model.passesDone, processes: showProcesses)) {
            live = await model.detailLive(server.id, processes: showProcesses)
            previews.refresh(server)
        }
        .onDisappear { previews.drop(server.id) }
    }

    /// Every button goes through the menu's own actions (confirmations,
    /// restarting state, blocked copy).
    private func perform(_ b: DetailButton) {
        switch b {
        case .open: model.open(server)
        case .stop, .stopNow: model.stop(server)
        case .restart: model.restart(server)
        case .postpone: model.postpone(server)
        }
    }
}

/// The detail itself, from plain values: hero, status card, memory chart
/// with its events, fact chips, command and processes, then the two
/// buttons for the situation.
struct DetailContent: View {
    let server: ServerViewFfi
    let once: DetailOnce
    let live: DetailLive
    let favicon: NSImage?
    let preview: NSImage?
    let note: RowNote?
    let restarting: Bool
    @Binding var showProcesses: Bool
    let onBack: () -> Void
    /// The ⋯ menu, drawn on the gradient or over a preview.
    let more: AnyView
    let moreOnImage: AnyView
    let perform: (DetailButton) -> Void
    @Environment(\.colorScheme) private var scheme
    @EnvironmentObject private var language: LanguageModel

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            HeroView(
                server: server, favicon: favicon, iconRGB: once.iconRGB, preview: preview,
                onBack: onBack, more: preview == nil ? more : moreOnImage
            )
            VStack(alignment: .leading, spacing: 0) {
                StatusCard(
                    server: server, note: note,
                    totalMs: countdownTotalMs(server, deadSessionIdleS: once.deadSessionIdleS),
                    restarting: restarting, perform: perform
                )
                MemoryChart(
                    samples: live.history, events: live.events, lineBytes: once.leakLineBytes,
                    rssBytes: server.rssBytes, cpuPct: server.cpuPct, health: server.health
                )
                .padding(.top, 14)
                FactChips(server: server, resumeCommand: once.resumeCommand)
                    .padding(.vertical, 12)
                commandRow
                if showProcesses {
                    ProcessList(processes: live.processes)
                        .padding(.bottom, 6)
                }
                buttons.padding(.top, 6)
            }
            .padding(.horizontal, panelPadding)
            .padding(.top, 12)
            .padding(.bottom, panelPadding)
        }
        .frame(width: panelWidth)
    }

    private var commandRow: some View {
        VStack(spacing: 0) {
            Divider().opacity(0.6)
            HStack(alignment: .firstTextBaseline, spacing: 10) {
                Text(verbatim: server.command)
                    .lineLimit(3)
                    .truncationMode(.middle)
                    .textSelection(.enabled)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .help(server.command)
                Button {
                    withAnimation(.easeOut(duration: 0.15)) { showProcesses.toggle() }
                } label: {
                    Text(verbatim: processesLabel(Int(server.processCount), bundle: language.bundle, locale: language.locale))
                        .frame(minHeight: minHitHeight)
                        .contentShape(Rectangle())
                }
                .buttonStyle(PressableStyle())
                .fixedSize()
            }
            .font(.system(size: 12))
            .foregroundStyle(.secondary)
            .padding(.vertical, 4)
        }
    }

    private var buttons: some View {
        let all = bottomButtons(server)
        return HStack(spacing: 8) {
            ForEach(Array(all.enumerated()), id: \.element) { index, b in
                let main = index == 0
                Button { perform(b) } label: {
                    Text(verbatim: b.title(bundle: language.bundle, locale: language.locale))
                        .font(.system(size: 13, weight: .semibold))
                        .foregroundStyle(main ? Color(nsColor: .windowBackgroundColor) : stopTextColor(dark: scheme == .dark))
                        .padding(.horizontal, 16)
                        .frame(maxWidth: main ? .infinity : nil)
                        .frame(height: detailButtonHeight)
                        .background(
                            main ? Color.primary.opacity(0.9) : Color.pink.opacity(0.16),
                            in: RoundedRectangle(cornerRadius: detailButtonRadius, style: .continuous)
                        )
                        .contentShape(RoundedRectangle(cornerRadius: detailButtonRadius, style: .continuous))
                }
                .buttonStyle(PressableStyle())
                .disabled(!detailButtonEnabled(b, server, restarting: restarting))
            }
        }
    }
}

/// The smallest hit area of a chip or a text button.
let minHitHeight: CGFloat = 26

/// The bottom buttons: the 28 pt top of the app's button range.
let detailButtonHeight: CGFloat = 28
let detailButtonRadius: CGFloat = 8

private struct LiveKey: Equatable {
    let id: ServerID
    let pass: Int
    let processes: Bool
}

private struct DetailHeightKey: PreferenceKey {
    static let defaultValue: CGFloat = 0
    static func reduce(value: inout CGFloat, nextValue: () -> CGFloat) {
        value = max(value, nextValue())
    }
}

/// Esc in the panel's own window calls `action` (and is swallowed there);
/// installed only while the view is in a window.
struct EscapeKey: NSViewRepresentable {
    let action: () -> Void

    func makeNSView(context: Context) -> EscapeKeyView {
        let view = EscapeKeyView()
        view.action = action
        return view
    }

    func updateNSView(_ view: EscapeKeyView, context: Context) {
        view.action = action
    }
}

final class EscapeKeyView: NSView {
    var action: (() -> Void)?
    private var monitor: Any?

    override func viewDidMoveToWindow() {
        super.viewDidMoveToWindow()
        if let monitor { NSEvent.removeMonitor(monitor) }
        monitor = nil
        guard window != nil else { return }
        monitor = NSEvent.addLocalMonitorForEvents(matching: .keyDown) { [weak self] event in
            // Only the panel's Esc: a confirmation alert's Esc is its own.
            guard let self, event.keyCode == 53, event.window === self.window else { return event }
            self.action?()
            return nil
        }
    }

    override func removeFromSuperview() {
        if let monitor { NSEvent.removeMonitor(monitor) }
        monitor = nil
        super.removeFromSuperview()
    }
}
