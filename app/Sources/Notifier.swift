import Foundation
import UserNotifications
import os

enum NotificationAction: Sendable {
    case kill
    case keep
}

private enum IDs {
    static let category = "IDLE_SERVER"
    static let kill = "KILL"
    static let keep = "KEEP"
}

/// Posts the "idle server" alerts and routes their Kill and Keep buttons back
/// to the model. If notifications are denied, posting fails silently and the
/// menu row stays the way to act on the server.
@MainActor
final class Notifier: NSObject, UNUserNotificationCenterDelegate {
    /// Called on the main actor when the user presses Kill or Keep.
    var onAction: ((NotificationAction, ServerID) -> Void)?

    private let center = UNUserNotificationCenter.current()
    private let log = Logger(subsystem: "com.matthieuh.balancetonport", category: "notifications")

    override init() {
        super.init()
        center.delegate = self
        let kill = UNNotificationAction(identifier: IDs.kill, title: "Kill", options: [.destructive])
        let keep = UNNotificationAction(identifier: IDs.keep, title: "Keep", options: [])
        center.setNotificationCategories([
            UNNotificationCategory(identifier: IDs.category, actions: [kill, keep], intentIdentifiers: [], options: [])
        ])
        // The system only prompts the first time; later calls just report.
        let log = self.log
        center.requestAuthorization(options: [.alert, .sound]) { granted, error in
            if let error {
                log.error("authorization failed: \(error.localizedDescription, privacy: .public)")
            } else if !granted {
                log.info("notifications denied; the menu shows idle servers")
            }
        }
    }

    /// One notification per server. The identifier is the server's identity,
    /// so a repeat replaces the previous one instead of stacking.
    func post(_ servers: [ServerViewFfi]) {
        for server in servers {
            let content = UNMutableNotificationContent()
            content.title = "Balance ton port"
            content.body = notificationBody(server)
            content.categoryIdentifier = IDs.category
            content.sound = .default
            content.userInfo = notificationUserInfo(server.id)
            let request = UNNotificationRequest(
                identifier: "idle-\(server.pid)-\(server.startTime)",
                content: content,
                trigger: nil
            )
            let log = self.log
            center.add(request) { error in
                if let error {
                    log.error("could not post: \(error.localizedDescription, privacy: .public)")
                }
            }
        }
    }

    nonisolated func userNotificationCenter(
        _ center: UNUserNotificationCenter,
        didReceive response: UNNotificationResponse
    ) async {
        let action: NotificationAction
        switch response.actionIdentifier {
        case IDs.kill: action = .kill
        case IDs.keep: action = .keep
        // A tap on the body (or a dismiss) opens nothing.
        default: return
        }
        guard let id = serverID(fromUserInfo: response.notification.request.content.userInfo) else { return }
        await MainActor.run { self.onAction?(action, id) }
    }

    /// Shows the banner even while the menu is open (the app is then active).
    nonisolated func userNotificationCenter(
        _ center: UNUserNotificationCenter,
        willPresent notification: UNNotification
    ) async -> UNNotificationPresentationOptions {
        [.banner, .list, .sound]
    }
}
