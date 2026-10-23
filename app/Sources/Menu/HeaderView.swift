import SwiftUI

/// Snout, name and server count, then the usage bar.
struct HeaderView: View {
    let count: Int
    let memory: MemoryBarFfi?
    let servers: [ServerViewFfi]

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack(spacing: 5) {
                Image("MenuIcon")
                    .renderingMode(.template)
                    .resizable()
                    .scaledToFit()
                    .frame(width: 16, height: 16)
                Text("Balance ton port").fontWeight(.semibold)
                Spacer()
                Text(verbatim: headerCount(count))
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
