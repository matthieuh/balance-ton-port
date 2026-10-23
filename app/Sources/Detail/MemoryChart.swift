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

    var body: some View {
        let end = Date()
        let start = end.addingTimeInterval(-historyWindow)
        let shown = events.filter { date($0.atMs) >= start }
        let scale = chartScale(maxBytes: max(rssBytes, samples.map(\.rssBytes).max() ?? 0), lineBytes: lineBytes)
        let lineY = scale.lineOnScale ? Double(lineBytes) : scale.upper
        let color = healthColor(health)
        VStack(alignment: .leading, spacing: 4) {
            HStack {
                Text("Last 2 hours")
                Spacer()
                Text(verbatim: memoryLabel(rssBytes))
                    .font(.system(size: 11, design: .monospaced))
                    .foregroundStyle(.primary)
                Text(verbatim: "\u{00B7} " + cpuLabel(cpuPct))
            }
            .font(.system(size: 11))
            .foregroundStyle(.secondary)
            Chart {
                ForEach(Array(samples.enumerated()), id: \.offset) { _, s in
                    AreaMark(x: .value("Time", date(s.atMs)), y: .value("Memory", Double(s.rssBytes)))
                        .foregroundStyle(color.opacity(0.1))
                        .interpolationMethod(.monotone)
                    LineMark(x: .value("Time", date(s.atMs)), y: .value("Memory", Double(s.rssBytes)))
                        .foregroundStyle(color)
                        .lineStyle(StrokeStyle(lineWidth: 2))
                        .interpolationMethod(.monotone)
                }
                RuleMark(y: .value("Leak line", lineY))
                    .foregroundStyle(Color(hex: 0xE9B764).opacity(0.6))
                    .lineStyle(StrokeStyle(lineWidth: 1, dash: [4, 4]))
                    .annotation(position: .bottom, alignment: .trailing, spacing: 1) {
                        Text(verbatim: (scale.lineOnScale ? "" : "\u{2191} ") + leakLineLabel(lineBytes))
                            .font(.system(size: 9))
                            .foregroundStyle(Color(hex: 0xE9B764).opacity(0.8))
                    }
                ForEach(Array(shown.enumerated()), id: \.offset) { _, e in
                    RuleMark(x: .value("Event", date(e.atMs)))
                        .foregroundStyle(eventColor(e.kind).opacity(0.65))
                        .lineStyle(StrokeStyle(lineWidth: 1))
                }
            }
            .chartXScale(domain: start...end)
            .chartYScale(domain: 0...scale.upper)
            .chartXAxis(.hidden)
            .chartYAxis(.hidden)
            .chartLegend(.hidden)
            .frame(height: 86)
            labels(shown, start: start)
        }
    }

    /// The events' labels under the chart, the latest kept when they crowd.
    private func labels(_ shown: [EventFfi], start: Date) -> some View {
        let positions = shown.map { date($0.atMs).timeIntervalSince(start) / historyWindow }
        let kept = labelledEvents(at: positions, minGap: 0.2)
        return GeometryReader { geo in
            ForEach(kept, id: \.self) { i in
                Text(verbatim: eventChartLabel(shown[i]))
                    .font(.system(size: 10))
                    .foregroundStyle(eventColor(shown[i].kind))
                    .lineLimit(1)
                    .fixedSize()
                    .offset(x: min(max(0, positions[i] * geo.size.width - 4), geo.size.width - 84))
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
