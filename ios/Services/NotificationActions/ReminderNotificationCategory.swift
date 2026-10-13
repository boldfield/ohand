import UserNotifications

/// The notification category that makes the acknowledge and complete actions available on a reminder.
/// The OS only reports an action identifier for a category registered with it, so `SystemNotificationCenter`
/// registers this category and `NotificationContent.generic` names it.
enum ReminderNotificationCategory {
    static let acknowledgeTitle = "Acknowledge"
    static let completeTitle = "Done"

    static func make() -> UNNotificationCategory {
        let acknowledge = UNNotificationAction(
            identifier: NotificationActionIdentifier.acknowledge, title: acknowledgeTitle, options: [])
        // Completing changes private content, so the OS asks for the device to be unlocked first.
        let complete = UNNotificationAction(
            identifier: NotificationActionIdentifier.complete, title: completeTitle,
            options: [.authenticationRequired])
        return UNNotificationCategory(
            identifier: NotificationActionIdentifier.reminderCategory,
            actions: [acknowledge, complete],
            intentIdentifiers: [],
            options: [])
    }
}
