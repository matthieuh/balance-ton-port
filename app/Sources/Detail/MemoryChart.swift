import Charts
import SwiftUI

/// The last 2 hours of the tree's memory, with the events drawn on it
/// (agent start, browser connected or closed, session ended, alert,
/// restart, postpone) and the leak line dotted; the current figure and CPU
/// above it.
struct MemoryChart: View {
    let samples: [SampleFfi]
    let events: [EventFfi]
    /// The leak line (the configured leak size).
    let lineBytes: UInt64
    let rssBytes: UInt64
    let cpuPct: Float
    let health: String
    /// The chart's right edge: now.
    var end = Date()
    @Environment(\.colorScheme) private var scheme
    @EnvironmentObject private var language: LanguageModel

    var body: some View {
        let dark = scheme == .dark
        let start = end.addingTimeInterval(-historyWindow)
        let shown = events.filter { date($0.atMs) >= start }
        let points = downsample(samples)
        let scale = chartScale(maxBytes: max(rssBytes, samples.map(\.rssBytes).max() ?? 0), lineBytes: lineBytes)
        let color = healthColor(health)
        VStack(alignment: .leading, spacing: 4) {
            HStack {
                Text(verbatim: language.tr("Last 2 hours"))
                Spacer()
                Text(verbatim: memoryLabel(rssBytes, locale: language.locale))
                    .font(.system(size: 11, design: .monospaced))
                    .foregroundStyle(.primary)
                Text(verbatim: "\u{00B7} " + cpuLabel(cpuPct, bundle: language.bundle, locale: language.locale))
            }
            .font(.system(size: 11))
            .foregroundStyle(.secondary)
            Chart {
                ForEach(Array(points.enumerated()), id: \.offset) { _, s in
                    AreaMark(x: .value("Time", date(s.atMs)), y: .value("Memory", Double(s.rssBytes)))
                        .foregroundStyle(color.opacity(0.12))
                        .interpolationMethod(.monotone)
                    LineMark(x: .value("Time", date(s.atMs)), y: .value("Memory", Double(s.rssBytes)))
                        .foregroundStyle(color)
                        .lineStyle(StrokeStyle(lineWidth: 2))
                        .interpolationMethod(.monotone)
                }
                // Pinned to the top when the line is far above a small
                // server (`chartScale`), as in the mockup.
                RuleMark(y: .value("Leak line", scale.lineOnScale ? Double(lineBytes) : scale.upper))
                    .foregroundStyle(leakLineColor(dark: dark).opacity(dark ? 0.6 : 0.8))
                    .lineStyle(StrokeStyle(lineWidth: 1, dash: [4, 4]))
                ForEach(Array(shown.enumerated()), id: \.offset) { _, e in
                    RuleMark(x: .value("Event", date(e.atMs)))
                        .foregroundStyle(eventColor(e.kind, dark: dark).opacity(dark ? 0.65 : 0.75))
                        .lineStyle(StrokeStyle(lineWidth: 1))
                }
            }
            .chartXScale(domain: start...end)
            .chartYScale(domain: 0...scale.upper)
            .chartXAxis(.hidden)
            .chartYAxis(.hidden)
            .chartLegend(.hidden)
            .frame(height: 86)
            labels(shown, start: start, dark: dark)
        }
    }

    /// The events' labels under the chart, each starting at its rule (ending
    /// at it in the right part of the chart); the latest kept when they
    /// crowd.
    private func labels(_ shown: [EventFfi], start: Date, dark: Bool) -> some View {
        let positions = shown.map { date($0.atMs).timeIntervalSince(start) / historyWindow }
        let kept = labelledEvents(at: positions, minGap: 0.2)
        return GeometryReader { geo in
            ForEach(kept, id: \.self) { i in
                let x = positions[i] * geo.size.width
                let label = Text(verbatim: eventChartLabel(shown[i], bundle: language.bundle, locale: language.locale))
                    .font(.system(size: 10))
                    .foregroundStyle(eventColor(shown[i].kind, dark: dark))
                    .lineLimit(1)
                    .fixedSize()
                if positions[i] > 0.7 {
                    label.frame(width: max(0, x), alignment: .trailing)
                } else {
                    label.frame(width: geo.size.width, alignment: .leading).offset(x: x)
                }
            }
        }
        .frame(height: shown.isEmpty ? 0 : 16)
    }

    private func date(_ ms: UInt64) -> Date {
        Date(timeIntervalSince1970: Double(ms) / 1000)
    }
}

/// The chart's span: the 2 hours the core keeps.
let historyWindow: TimeInterval = 2 * 3600
