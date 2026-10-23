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

/// One server's story inside the menu panel (mockup v2): hero, status
/// card, memory chart with its events, fact chips, command and processes,
/// then the two buttons for the situation. Its data is fetched on the core
/// queue on every pass.
struct DetailView: View {
    let server: ServerViewFfi
    @EnvironmentObject private var model: CoreModel
    @EnvironmentObject private var nav: MenuNavigation
    @ObservedObject private var previews = PreviewStore.shared
    @Environment(\.colorScheme) private var scheme
    @State private var data = DetailData()
    @State private var showProcesses = false
    @State private var contentHeight: CGFloat = 0

    var body: some View {
        ScrollView(.vertical) {
            content
                .background(GeometryReader { geo in
                    Color.clear.preference(key: DetailHeightKey.self, value: geo.size.height)
                })
        }
        .scrollIndicators(.automatic)
        .frame(height: min(contentHeight, detailMaxHeight(screenHeight: NSScreen.main?.visibleFrame.height ?? 800)))
        .onPreferenceChange(DetailHeightKey.self) { contentHeight = $0 }
        .frame(width: panelWidth)
        .background(EscapeKey { nav.back() })
        .task(id: TaskKey(id: server.id, pass: model.passesDone)) {
            data = await model.detail(server.id)
            previews.refresh(server)
        }
    }

    private var content: some View {
        VStack(alignment: .leading, spacing: 0) {
            HeroView(
                server: server, favicon: model.icons[server.id], iconData: data.icon,
                preview: previews.image(for: server.id)
            )
            VStack(alignment: .leading, spacing: 0) {
                StatusCard(
                    server: server, note: model.rowNotes[server.id],
                    totalMs: countdownTotalMs(server, deadSessionIdleS: data.deadSessionIdleS),
                    perform: perform
                )
                MemoryChart(
                    samples: data.history, events: data.events, lineBytes: data.leakLineBytes,
                    rssBytes: server.rssBytes, cpuPct: server.cpuPct, health: server.health
                )
                .padding(.top, 14)
                FactChips(server: server, resumeCommand: data.resumeCommand)
                    .padding(.vertical, 12)
                commandRow
                if showProcesses {
                    ProcessList(processes: data.processes)
                        .padding(.bottom, 6)
                }
                buttons.padding(.top, 6)
            }
            .padding(.horizontal, panelPadding)
            .padding(.top, 12)
            .padding(.bottom, panelPadding)
        }
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
                    Text(verbatim: processesLabel(Int(server.processCount)))
                        .contentShape(Rectangle())
                }
                .buttonStyle(PressableStyle())
                .fixedSize()
            }
            .font(.system(size: 12))
            .foregroundStyle(.secondary)
            .padding(.vertical, 8)
        }
    }

    private var buttons: some View {
        let restarting = model.isRestarting(server.id)
        let all = bottomButtons(server)
        return HStack(spacing: 8) {
            ForEach(Array(all.enumerated()), id: \.element) { index, b in
                let main = index == 0
                Button { perform(b) } label: {
                    Text(verbatim: b.title)
                        .font(.system(size: 13, weight: .semibold))
                        .foregroundStyle(main ? Color(nsColor: .windowBackgroundColor) : Color.pink)
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

/// The bottom buttons: the 28 pt top of the app's button range.
let detailButtonHeight: CGFloat = 28
let detailButtonRadius: CGFloat = 8

private struct TaskKey: Equatable {
    let id: ServerID
    let pass: Int
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
