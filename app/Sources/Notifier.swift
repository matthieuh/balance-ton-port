import Foundation
import UserNotifications
import os

private typealias IDs = NotificationIDs

/// Posts the "idle server" alerts and the "session ended" groups, and routes
/// their buttons back to the model. When notifications are not allowed,
/// nothing is posted and `post` reports nothing delivered: the menu row says
/// so and stays the way to act on the server.
@MainActor
final class Notifier: NSObject, UNUserNotificationCenterDelegate {
    /// Called on the main actor when the user presses a button.
    var onAction: ((NotificationRoute) -> Void)?

    private let center = UNUserNotificationCenter.current()
    private let log = Logger(subsystem: "com.matthieuh.balancetonport", category: "notifications")

    override init() {
        super.init()
        center.delegate = self
        let kill = UNNotificationAction(identifier: IDs.kill, title: "Kill", options: [.destructive])
        let keep = UNNotificationAction(identifier: IDs.keep, title: "Keep", options: [])
        let stopNow = UNNotificationAction(identifier: IDs.stopNow, title: "Stop now", options: [.destructive])
        center.setNotificationCategories([
            UNNotificationCategory(
                identifier: IDs.idleCategory, actions: [kill, keep], intentIdentifiers: [], options: []
            ),
            UNNotificationCategory(
                identifier: IDs.sessionEndedCategory, actions: [stopNow, keep], intentIdentifiers: [], options: []
            ),
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
        content.categoryIdentifier = IDs.idleCategory
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
        guard await canPost() else { return [] }
        var delivered: [ServerID] = []
        for server in servers {
            let content = UNMutableNotificationContent()
            content.title = "Balance ton port"
            content.body = notificationBody(server)
            content.categoryIdentifier = IDs.idleCategory
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

    /// One notification per ended session ("Session ended: 2 servers kept
    /// running, ..."), with Stop now and Keep; returns the groups whose
    /// notification was added. Nothing is posted when notifications are not
    /// allowed.
    func postSessionEnded(_ groups: [SessionEndedFfi]) async -> [SessionEndedID] {
        guard await canPost() else { return [] }
        var delivered: [SessionEndedID] = []
        for group in groups {
            let content = UNMutableNotificationContent()
            content.title = "Balance ton port"
            content.subtitle = group.sessionName
            content.body = group.body
            content.categoryIdentifier = IDs.sessionEndedCategory
            content.sound = .default
            content.userInfo = sessionEndedUserInfo(group.id)
            let request = UNNotificationRequest(
                identifier: sessionEndedRequestID(group.id), content: content, trigger: nil
            )
            do {
                try await center.add(request)
                delivered.append(group.id)
            } catch {
                log.error("could not post: \(error.localizedDescription, privacy: .public)")
            }
        }
        return delivered
    }

    /// Notifications are allowed (nothing is posted otherwise).
    func canPost() async -> Bool {
        let settings = await center.notificationSettings()
        switch settings.authorizationStatus {
        case .authorized, .provisional, .ephemeral:
            return true
        default:
            return false
        }
    }

    nonisolated func userNotificationCenter(
        _ center: UNUserNotificationCenter,
        didReceive response: UNNotificationResponse
    ) async {
        // A tap on the body, a dismiss or the test notification: nothing.
        guard let route = notificationRoute(
            actionIdentifier: response.actionIdentifier,
            userInfo: response.notification.request.content.userInfo
        ) else { return }
        await MainActor.run { self.onAction?(route) }
    }

    /// Shows the banner even while the menu is open (the app is then active).
    nonisolated func userNotificationCenter(
        _ center: UNUserNotificationCenter,
        willPresent notification: UNNotification
    ) async -> UNNotificationPresentationOptions {
        [.banner, .list, .sound]
    }
}
