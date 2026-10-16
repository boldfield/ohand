import Foundation
import OhAndCoreBridge
import OhAndServices

/// The words the provider settings screen uses. Kept apart from the model so each statement is checked against the
/// case that produces it.
enum ProviderSettingsText {
    static let lockedText = "Your saved items are locked. Go back and open Settings again to unlock them."
    static let localOnlyNotice =
        "No provider is selected, so notes stay on this device and nothing is sent anywhere. Everything you capture is "
        + "saved either way."
    static let privateServerNotice =
        "This default is a private server you named. If it cannot be reached, captures are still saved and wait on "
        + "this device; they are not sent to another provider."
    static let hostedNotice =
        "This default is a hosted provider. Choosing it does not allow any note to be sent; sending is a separate "
        + "permission."
    static let defaultChangeNote =
        "Changing the default only affects work created afterwards. Work already waiting keeps the provider version "
        + "it was created with, and saved captures are never changed."
    static let capabilityNote = "Each profile serves text interpretation. No other capability can be selected."
    static let credentialNote =
        "Enter only the name of a credential saved on this device. The secret itself is never typed here."

    static func protocolName(_ providerProtocol: String) -> String {
        switch providerProtocol {
        case "open_ai": return "OpenAI-compatible"
        case "anthropic": return "Anthropic"
        case "self_hosted": return "Private server"
        default: return "Unknown provider"
        }
    }

    static func capabilityName(_ capability: String) -> String {
        capability == "text_interpretation" ? "Text interpretation" : "Unsupported capability"
    }

    static func issue(_ issue: ProviderSettingsIssue) -> String {
        switch issue {
        case .profileNameRequired: return "Give the profile a name."
        case .profileNameTooLong: return "The profile name is too long."
        case .modelRequired: return "Enter the model name."
        case .modelTooLong: return "The model name is too long."
        case .endpointRequired: return "A private server needs its https address."
        case .endpointNotHTTPS: return "The server address must be an https address."
        case .endpointHasQueryOrFragment: return "The server address cannot include a query or fragment."
        case .endpointNotAllowed: return "Only a private server takes an address."
        case .credentialRequired: return "Name the saved credential this profile uses."
        case .credentialTooLong: return "The credential name is too long."
        case .timeoutOutOfBounds:
            return "The timeout must be between \(ProviderSettingsModel.minimumTimeoutSeconds) and "
                + "\(ProviderSettingsModel.maximumTimeoutSeconds) seconds."
        }
    }

    static func failure(_ error: Error) -> String {
        guard let error = error as? ProviderConfigurationError else {
            return "Something went wrong. Nothing was changed."
        }
        switch error {
        case .notAuthenticated: return lockedText
        case .invalidRequest: return "That request is not valid. Nothing was changed."
        case .unreadable: return "The app could not read the answer. Nothing was changed."
        case .core(let failure): return coreFailure(code: failure.code)
        }
    }

    private static func coreFailure(code: String) -> String {
        switch code {
        case "unknown_protocol": return "That provider type is not supported. Nothing was changed."
        case "endpoint_required": return "A private server needs its https address. Nothing was changed."
        case "endpoint_invalid": return "That server address is not acceptable. Nothing was changed."
        case "endpoint_not_allowed": return "Only a private server takes an address. Nothing was changed."
        case "model_required", "model_invalid": return "The model name is not acceptable. Nothing was changed."
        case "profile_id_invalid": return "The profile name is not acceptable. Nothing was changed."
        case "capability_unsupported", "capability_not_configurable":
            return "That provider cannot do what was asked. Nothing was changed."
        case "credential_required", "credential_invalid", "credential_pending_removal":
            return "The credential name is missing or not usable. Nothing was changed."
        case "timeout_out_of_bounds", "retry_out_of_bounds":
            return "A limit is outside what is allowed. Nothing was changed."
        case "profile_not_found": return "That profile version no longer exists. Nothing was changed."
        case "profile_revoked": return "That profile version was revoked and cannot be the default."
        default: return "The app could not complete that. Nothing was changed."
        }
    }
}
