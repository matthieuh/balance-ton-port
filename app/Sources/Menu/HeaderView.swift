import SwiftUI

/// Snout, name and server count, then the usage bar.
struct HeaderView: View {
    let count: Int
    let memory: MemoryBarFfi?
    let servers: [ServerViewFfi]
    @EnvironmentObject private var language: LanguageModel

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack(spacing: 5) {
                Image("MenuIcon", bundle: appBundle)
                    .renderingMode(.template)
                    .resizable()
                    .scaledToFit()
                    .frame(width: 16, height: 16)
                Text(verbatim: language.tr("Balance ton port")).fontWeight(.semibold)
                Spacer()
                Text(verbatim: headerCount(count, bundle: language.bundle, locale: language.locale))
                    .font(.system(size: 12))
                    .foregroundStyle(.secondary)
            }
            .font(.system(size: 13))
            if let memory, memory.totalBytes > 0 {
                UsageBar(memory: memory, servers: servers)
            }
        }
    }
}
