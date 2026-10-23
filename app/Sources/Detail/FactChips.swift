import AppKit
import SwiftUI

/// Small facts as chips: branch, project › worktree, the session (a click
/// copies the command that resumes it, never opens a terminal), uptime.
struct FactChips: View {
    let server: ServerViewFfi
    let resumeCommand: String?
    @State private var copied = false
    @Environment(\.colorScheme) private var scheme

    var body: some View {
        FlowLayout(spacing: 6) {
            if let branch = server.branch, !branch.isEmpty {
                chip("\u{2387} " + branch)
            }
            if !server.locationLabel.isEmpty {
                chip("\u{25A3} " + server.locationLabel)
            }
            if let resumeCommand {
                sessionChip(resumeCommand)
            }
            chip("\u{25F7} " + uptimeChip(ms: server.uptimeMs))
        }
    }

    private func chip(_ text: String) -> some View {
        Text(verbatim: text)
            .font(.system(size: 12))
            .lineLimit(1)
            .truncationMode(.middle)
            .padding(.vertical, 4)
            .padding(.horizontal, 9)
            .background(Color.primary.opacity(0.06), in: Capsule())
    }

    private func sessionChip(_ command: String) -> some View {
        let color = agentChipColor(server.agent, dark: scheme == .dark)
        return Button {
            NSPasteboard.general.clearContents()
            NSPasteboard.general.setString(command, forType: .string)
            copied = true
            Task {
                try? await Task.sleep(for: .seconds(copiedDuration))
                copied = false
            }
        } label: {
            Text(verbatim: copied ? copiedLabel : sessionChipText(server))
                .font(.system(size: 12))
                .foregroundStyle(color)
                .lineLimit(1)
                .padding(.vertical, 4)
                .padding(.horizontal, 9)
                .background(color.opacity(0.16), in: Capsule())
                .frame(minHeight: minHitHeight)
                .contentShape(Rectangle())
        }
        .buttonStyle(PressableStyle())
        .help("Copy: \(command)")
    }
}

/// Lays its children out left to right, wrapping to a new line when the
/// width runs out.
struct FlowLayout: Layout {
    var spacing: CGFloat = 6

    func sizeThatFits(proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) -> CGSize {
        let rows = arrange(subviews, width: proposal.width ?? .infinity)
        let height = rows.last.map { $0.y + $0.height } ?? 0
        let width = rows.map(\.width).max() ?? 0
        return CGSize(width: proposal.width ?? width, height: height)
    }

    func placeSubviews(in bounds: CGRect, proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) {
        for row in arrange(subviews, width: bounds.width) {
            var x = bounds.minX
            for i in row.items {
                let size = subviews[i].sizeThatFits(ProposedViewSize(width: bounds.width, height: nil))
                subviews[i].place(
                    at: CGPoint(x: x, y: bounds.minY + row.y), proposal: ProposedViewSize(width: min(size.width, bounds.width), height: size.height)
                )
                x += min(size.width, bounds.width) + spacing
            }
        }
    }

    private struct Row {
        var items: [Int] = []
        var y: CGFloat = 0
        var width: CGFloat = 0
        var height: CGFloat = 0
    }

    private func arrange(_ subviews: Subviews, width: CGFloat) -> [Row] {
        var rows: [Row] = []
        var row = Row()
        for i in subviews.indices {
            let size = subviews[i].sizeThatFits(ProposedViewSize(width: width, height: nil))
            let w = min(size.width, width)
            if !row.items.isEmpty && row.width + spacing + w > width {
                let next = row.y + row.height + spacing
                rows.append(row)
                row = Row(y: next)
            }
            row.width += (row.items.isEmpty ? 0 : spacing) + w
            row.height = max(row.height, size.height)
            row.items.append(i)
        }
        if !row.items.isEmpty { rows.append(row) }
        return rows
    }
}
