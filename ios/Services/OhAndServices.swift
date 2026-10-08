import Foundation

public struct OhAndServices {
    public init() {}

    public static let version = "0.1.0"
}

public extension OhAndServices {
    static func makeCredentialService(keychainService: String = "com.boldfield.ohand.credentials") -> CredentialService {
        CredentialService(keychainService: keychainService)
    }
}
