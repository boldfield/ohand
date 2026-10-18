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
        "The secret is stored in this device's secure storage and is never shown again. The profile holds only an "
        + "opaque name for it."
    static let noCredentialBoundCode = "credential_not_bound"
    static let credentialMissingReference = "No saved credential is named yet."
    static let credentialEmptySecret = "Enter the secret first. Nothing was stored."
    static let earlierSaveStillFinishing = "An earlier save is still finishing. Try again in a moment."
    static let connectionCheckNote =
        "A connection check sends one fixed test message to this profile's own destination. It contains none of your "
        + "notes and runs only when you ask."

    static func rejectedSecret(_ rejection: ProviderSecretRejection) -> String {
        switch rejection {
        case .empty: return credentialEmptySecret
        case .surroundingWhitespace:
            return "The secret starts or ends with a space or line break. Remove it and try again. Nothing was stored."
        }
    }

    static func credentialState(_ state: ProviderSettingsCredentialState) -> String {
        switch state {
        case .working: return "Working…"
        case .stored: return "Secret stored on this device."
        case .updated: return "Secret replaced."
        case .removed: return "Secret removed. Profile versions that used it cannot send until you enter a new one."
        case .status(.present): return "A secret is saved for this name."
        case .status(.absent): return "No secret is saved for this name. Enter one to use it."
        case .status(.invalidated): return "The saved secret cannot be used. Enter it again."
        case .failed(let message): return message
        }
    }

    static func credentialFailure(_ error: Error) -> String {
        if let error = error as? ProviderConfigurationError, case .notAuthenticated = error { return lockedText }
        if let error = error as? ProviderConfigurationError, case .core(let failure) = error,
            failure.code == ProviderSettingsText.noCredentialBoundCode
        {
            return "This profile version has no secret bound to it. Nothing was changed."
        }
        guard let error = error as? CredentialError else { return "The credential could not be changed." }
        switch error {
        case .invalidReference: return "That credential name is not valid. Nothing was changed."
        case .emptySecret: return credentialEmptySecret
        case .notFound: return "No secret is saved for this name."
        case .invalidated: return "The saved secret cannot be used. Enter it again."
        case .deviceLocked: return "Unlock the device and try again. Nothing was changed."
        case .storage: return "Secure storage could not complete that. Nothing was changed."
        }
    }

    static func connectionState(_ state: ProviderSettingsConnectionState) -> String {
        switch state {
        case .checking: return "Checking…"
        case .finished(let outcome): return connectionOutcome(outcome)
        case .failed(let message): return message
        }
    }

    /// One fixed sentence per outcome. None echoes an address, a response or a secret.
    static func connectionOutcome(_ outcome: ProviderConnectionOutcome) -> String {
        switch outcome {
        case .ok: return "Connected. The provider accepted the test."
        case .credentialMissing: return "No usable secret is saved for this profile. Enter it again."
        case .credentialRejected: return "The provider rejected the saved secret. Replace it."
        case .unreachable: return "The provider could not be reached. Check the network or the server address."
        case .tlsFailure: return "A secure connection could not be made to the provider."
        case .timeout: return "The provider did not answer in time."
        case .capabilityMismatch: return "The provider does not offer this model or capability."
        case .protocolNotEnabled: return "This provider type is not enabled in this version of the app. Nothing was sent."
        case .rateLimited: return "The provider is limiting requests right now. Try again later."
        case .providerError: return "The provider reported a problem. Try again later."
        case .unexpectedResponse: return "The provider answered in a way the app does not understand."
        case .cancelled: return "The check was cancelled."
        }
    }

    static func protocolName(_ providerProtocol: String) -> String {
        switch providerProtocol {
        case "open_ai": return "OpenAI-compatible"
        case "anthropic": return "Anthropic"
        case "self_hosted": return "Private server"
        default: return "Unknown provider"
        }
    }

    static let unavailableDefaultNotice =
        "This default cannot process notes yet: it has no verification evidence. Captures are saved and wait on this "
        + "device; they are not sent to another provider."

    /// One capability of a stored version, with whether it can be used.
    static func capabilityStatus(_ capability: String, support: String) -> String {
        let name = capabilityName(capability)
        switch support {
        case "supported": return "\(name): available"
        case "unverified":
            return "\(name): unavailable, not verified. It will not process notes until verification evidence exists."
        default: return "\(name): unavailable. \(unsupportedReason(capability))"
        }
    }

    static func capabilityName(_ capability: String) -> String {
        switch capability {
        case "text_interpretation": return "Text interpretation"
        case "transcription": return "Transcription"
        case "embeddings": return "Embeddings"
        case "speech_generation": return "Speech generation"
        default: return "Other capability (\(capability))"
        }
    }

    /// Why a capability the core declared unsupported cannot be used, following the M1 provider capability contract.
    static func unsupportedReason(_ capability: String) -> String {
        switch capability {
        case "text_interpretation": return "This provider version does not offer it."
        case "transcription": return "Notes are transcribed on this device only; no provider transcribes them."
        case "embeddings": return "This version of the app does not use embeddings."
        case "speech_generation": return "This version of the app does not use speech generation."
        default: return "This version of the app does not use it."
        }
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
        case .credentialRequired: return "Store the secret this profile uses."
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
