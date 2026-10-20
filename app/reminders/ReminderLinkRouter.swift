import Foundation
import OhAndCoreBridge
import OhAndServices

/// Reads enough of an item to tell whether a notification's target still names a readable record.
protocol ReminderLinkChecking: AnyObject, Sendable {
    func reminder(itemID: String) async throws -> ReminderReadAnswer
}

extension ReminderControlService: ReminderLinkChecking {}

/// Opens an item's protected screen. `ManagementNavigationModel.openItem` is the production implementation: it asks the
/// device owner first when reads are locked and drops the request if they do not succeed.
protocol ReminderLinkNavigating: AnyObject {
    func openItem(itemID: String)
}

/// What became of a tapped notification's link.
enum ReminderLinkOutcome: Equatable {
    /// The item was handed to protected navigation.
    case opened(itemID: String)
    /// The link carried no usable identifier. Nothing was read or opened.
    case invalid
    /// No readable item has that identifier any more (deleted, replaced or never existed). Nothing is shown about it.
    case unavailable

    var notice: String? {
        switch self {
        case .opened: return nil
        case .invalid: return "That notification could not be opened."
        case .unavailable: return "The item for that notification is no longer available."
        }
    }
}

/// Routes the generic opaque target of a reminder notification to the protected record it names.
///
/// * The link is an identifier only. It is checked for shape before anything is read, and its content is never shown.
/// * While reads are locked the core is not asked and the item is handed to navigation, which authenticates first. A
///   failed or cancelled authentication therefore opens nothing and reveals nothing.
/// * Once reads are allowed, a target that is deleted, unknown or answered for a different item is `unavailable`.
/// * One tap opens at most one item. Older notifications are never gathered into a summary.
final class ReminderLinkRouter {
    private let checker: ReminderLinkChecking
    private let navigator: ReminderLinkNavigating

    init(checker: ReminderLinkChecking, navigator: ReminderLinkNavigating) {
        self.checker = checker
        self.navigator = navigator
    }

    @MainActor @discardableResult
    func route(targetID: String?) async -> ReminderLinkOutcome {
        guard let targetID, let identifier = OpaqueIdentifier(targetID) else { return .invalid }
        let itemID = identifier.rawValue
        do {
            let answer = try await checker.reminder(itemID: itemID)
            guard answer.itemID == itemID, answer.lifecycleState != "deleted" else { return .unavailable }
        } catch let error as ReminderControlError {
            switch error {
            case .notAuthenticated, .itemCannotCarryReminder:
                // Locked: navigation authenticates before it shows anything. Not an action: the item exists.
                break
            default:
                return .unavailable
            }
        } catch {
            return .unavailable
        }
        navigator.openItem(itemID: itemID)
        return .opened(itemID: itemID)
    }
}
