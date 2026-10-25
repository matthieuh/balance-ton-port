import Dispatch
import Foundation

/// macOS memory pressure, the signal Activity Monitor graphs in green, yellow
/// and red.
enum MemoryPressure: Equatable, Sendable {
    case normal, warning, critical

    /// From `kern.memorystatus_vm_pressure_level`: 1 normal, 2 warning, 4 critical.
    init(sysctlLevel: Int32) {
        switch sysctlLevel {
        case 4: self = .critical
        case 2: self = .warning
        default: self = .normal
        }
    }

    /// The level now, `.normal` when the kernel does not answer.
    static func current() -> MemoryPressure {
        var level: Int32 = 0
        var size = MemoryLayout<Int32>.size
        guard sysctlbyname("kern.memorystatus_vm_pressure_level", &level, &size, nil, 0) == 0 else { return .normal }
        return MemoryPressure(sysctlLevel: level)
    }
}

/// Publishes the memory pressure level, read again each time macOS reports a
/// change.
@MainActor
final class MemoryPressureMonitor: ObservableObject {
    @Published private(set) var pressure = MemoryPressure.current()
    private let source = DispatchSource.makeMemoryPressureSource(eventMask: [.normal, .warning, .critical], queue: .main)

    init() {
        source.setEventHandler { [weak self] in
            MainActor.assumeIsolated { self?.pressure = MemoryPressure.current() }
        }
        source.activate()
    }

    deinit { source.cancel() }
}
