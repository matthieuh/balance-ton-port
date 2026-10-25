import SwiftUI

/// The machine's memory in one slim bar: dev servers split by health, other
/// apps grey, free memory left empty. Hovering shows the exact figures.
struct UsageBar: View {
    let memory: MemoryBarFfi
    let servers: [ServerViewFfi]
    @EnvironmentObject private var language: LanguageModel

    var body: some View {
        let seg = usageSegments(memory, servers: servers)
        VStack(alignment: .leading, spacing: 5) {
            GeometryReader { geo in
                let w = geo.size.width
                HStack(spacing: 0) {
                    healthColor("green").frame(width: w * seg.green)
                    healthColor("amber").frame(width: w * seg.amber)
                    healthColor("red").frame(width: w * seg.red)
                    Color(nsColor: .windowBackgroundColor).frame(width: 1)
                    Color.secondary.opacity(0.45).frame(width: max(0, w * seg.apps - 1))
                    Spacer(minLength: 0)
                }
            }
            .frame(height: 8)
            .background(Color.primary.opacity(0.1))
            .clipShape(Capsule())
            caption
        }
        .contentShape(Rectangle())
        .help(usageHelp(memory, bundle: language.bundle, locale: language.locale))
    }

    @ViewBuilder private var caption: some View {
        let c = usageCaption(memory, bundle: language.bundle, locale: language.locale)
        HStack(spacing: 4) {
            Text(verbatim: c.figure)
                .font(.system(size: 11, weight: .semibold).monospacedDigit())
                .foregroundStyle(.primary)
            Text(verbatim: c.dev)
            Spacer(minLength: 8)
            swatch(Color.secondary.opacity(0.45))
            Text(verbatim: c.apps)
            swatch(Color.primary.opacity(0.1)).padding(.leading, 6)
            Text(verbatim: c.free)
                .foregroundStyle(memory.freeLow ? Color.orange : Color.secondary)
        }
        .font(.system(size: 11))
        .foregroundStyle(.secondary)
        .lineLimit(1)
    }

    /// The legend's key: the bar segment's own color.
    private func swatch(_ color: Color) -> some View {
        Capsule().fill(color).frame(width: 8, height: 6)
    }
}
