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
/// to the model. When notifications are not allowed, nothing is posted and
/// `post` reports nothing delivered: the menu row says so and stays the way
/// to act on the server.
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
    }

    /// Asks macOS for permission. The system only prompts the first time;
    /// later calls just report.
    func requestAuthorization() async {
        do {
            let granted = try await center.requestAuthorization(options: [.alert, .sound])
            if !granted { log.info("notifications denied; the menu shows idle servers") }
        } catch {
            log.error("authorization failed: \(error.localizedDescription, privacy: .public)")
        }
    }

    /// The current permission, read from Notification Center.
    func permission() async -> NotificationPermission {
        let settings = await center.notificationSettings()
        return NotificationPermission(status: settings.authorizationStatus, alertStyle: settings.alertStyle)
    }

    /// A made-up idle-server notification with the real Kill and Keep
    /// buttons. Its userInfo names no server, so both buttons do nothing.
    /// `true` once Notification Center accepted it.
    func postSample() async -> Bool {
        let content = UNMutableNotificationContent()
        content.title = "Balance ton port"
        content.body = sampleNotificationBody
        content.categoryIdentifier = IDs.category
        content.sound = .default
        content.userInfo = sampleNotificationUserInfo()
        let request = UNNotificationRequest(identifier: "sample", content: content, trigger: nil)
        do {
            try await center.add(request)
            return true
        } catch {
            log.error("could not post the test: \(error.localizedDescription, privacy: .public)")
            return false
        }
    }

    /// One notification per server; returns the servers whose notification
    /// was added. The identifier is the server's identity, so a repeat
    /// replaces the previous one instead of stacking.
    func post(_ servers: [ServerViewFfi]) async -> [ServerID] {
        let settings = await center.notificationSettings()
        switch settings.authorizationStatus {
        case .authorized, .provisional, .ephemeral:
            break
        default:
            return []
        }
        var delivered: [ServerID] = []
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
            do {
                try await center.add(request)
                delivered.append(server.id)
            } catch {
                log.error("could not post: \(error.localizedDescription, privacy: .public)")
            }
        }
        return delivered
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
        let info = response.notification.request.content.userInfo
        // The test notification: its buttons never reach a server.
        guard !isSampleNotification(info), let id = serverID(fromUserInfo: info) else { return }
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
