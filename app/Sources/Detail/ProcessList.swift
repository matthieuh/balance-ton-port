import SwiftUI

/// The server's process tree, server first, each with its own memory.
struct ProcessList: View {
    let processes: [ProcRowFfi]
    @EnvironmentObject private var language: LanguageModel

    var body: some View {
        VStack(alignment: .leading, spacing: 3) {
            ForEach(processes, id: \.pid) { p in
                HStack(spacing: 8) {
                    Text(verbatim: String(p.pid))
                        .font(.system(size: 11, design: .monospaced))
                        .foregroundStyle(Color(nsColor: .tertiaryLabelColor))
                        .frame(width: 52, alignment: .leading)
                    Text(verbatim: p.command)
                        .lineLimit(1)
                        .truncationMode(.middle)
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .help(p.command)
                    Text(verbatim: memoryLabel(p.rssBytes, locale: language.locale))
                        .font(.system(size: 11, design: .monospaced))
                }
                .font(.system(size: 11))
                .foregroundStyle(.secondary)
            }
        }
        .padding(.vertical, 4)
    }
}
