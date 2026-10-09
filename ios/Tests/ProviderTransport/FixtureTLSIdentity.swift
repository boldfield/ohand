import Foundation
import Security

/// A throwaway self-signed P-256 server certificate for `localhost`, generated at test time so no key material is
/// committed. The certificate doubles as the trust anchor handed to the transport under test. Hostname `127.0.0.1`
/// is deliberately not in the subject alternative names.
final class FixtureTLSIdentity {
    enum Failure: Error {
        case keyGeneration
        case signing
        case certificate
        case keychain(OSStatus)
    }

    let certificate: SecCertificate
    let identity: SecIdentity
    private let privateKey: SecKey
    private let label: String

    init() throws {
        let label = "ohand.fixture.\(UUID().uuidString)"
        var keyError: Unmanaged<CFError>?
        let keyAttributes: [String: Any] = [
            kSecAttrKeyType as String: kSecAttrKeyTypeECSECPrimeRandom,
            kSecAttrKeySizeInBits as String: 256,
            kSecPrivateKeyAttrs as String: [
                kSecAttrIsPermanent as String: true,
                kSecAttrLabel as String: label,
                kSecAttrApplicationTag as String: Data(label.utf8),
            ] as [String: Any],
        ]
        guard let privateKey = SecKeyCreateRandomKey(keyAttributes as CFDictionary, &keyError),
            let publicKey = SecKeyCopyPublicKey(privateKey),
            let publicKeyBytes = SecKeyCopyExternalRepresentation(publicKey, &keyError) as Data?
        else { throw Failure.keyGeneration }

        let toBeSigned = Self.toBeSignedCertificate(publicKeyPoint: [UInt8](publicKeyBytes))
        var signError: Unmanaged<CFError>?
        guard
            let signature = SecKeyCreateSignature(
                privateKey, .ecdsaSignatureMessageX962SHA256, Data(toBeSigned) as CFData, &signError) as Data?
        else { throw Failure.signing }
        let certificateBytes = DER.sequence([
            toBeSigned,
            DER.sequence([DER.objectIdentifier(DER.ecdsaWithSHA256)]),
            DER.bitString([UInt8](signature)),
        ])
        guard let certificate = SecCertificateCreateWithData(nil, Data(certificateBytes) as CFData) else {
            throw Failure.certificate
        }

        // On iOS an identity only exists once its key and certificate are in the Keychain.
        let certificateStatus = SecItemAdd(
            [
                kSecClass as String: kSecClassCertificate,
                kSecValueRef as String: certificate,
                kSecAttrLabel as String: label,
            ] as CFDictionary, nil)
        guard certificateStatus == errSecSuccess || certificateStatus == errSecDuplicateItem else {
            throw Failure.keychain(certificateStatus)
        }
        var identityRef: CFTypeRef?
        let identityStatus = SecItemCopyMatching(
            [
                kSecClass as String: kSecClassIdentity,
                kSecAttrLabel as String: label,
                kSecReturnRef as String: true,
                kSecMatchLimit as String: kSecMatchLimitOne,
            ] as CFDictionary, &identityRef)
        guard identityStatus == errSecSuccess, let identityRef else { throw Failure.keychain(identityStatus) }

        self.privateKey = privateKey
        self.label = label
        self.certificate = certificate
        self.identity = identityRef as! SecIdentity
    }

    func remove() {
        SecItemDelete([kSecClass as String: kSecClassCertificate, kSecAttrLabel as String: label] as CFDictionary)
        SecItemDelete([kSecClass as String: kSecClassKey, kSecAttrLabel as String: label] as CFDictionary)
    }

    private static func toBeSignedCertificate(publicKeyPoint: [UInt8]) -> [UInt8] {
        var serial = (0..<8).map { _ in UInt8.random(in: 0...255) }
        serial[0] = (serial[0] & 0x7f) | 0x01
        let name = DER.sequence([
            DER.set([DER.sequence([DER.objectIdentifier(DER.commonName), DER.utf8String("localhost")])])
        ])
        let now = Date()
        let validity = DER.sequence([
            DER.utcTime(now.addingTimeInterval(-3600)), DER.utcTime(now.addingTimeInterval(86400)),
        ])
        let subjectPublicKeyInfo = DER.sequence([
            DER.sequence([DER.objectIdentifier(DER.ecPublicKey), DER.objectIdentifier(DER.prime256v1)]),
            DER.bitString(publicKeyPoint),
        ])
        let extensions = DER.tagged(
            0xA3,
            DER.sequence([
                DER.extensionEntry(
                    DER.keyUsage, critical: true, value: DER.bitString([0x80], unusedBits: 7)),
                DER.extensionEntry(
                    DER.extendedKeyUsage, critical: false,
                    value: DER.sequence([DER.objectIdentifier(DER.serverAuth)])),
                DER.extensionEntry(
                    DER.subjectAltName, critical: false,
                    value: DER.sequence([DER.tagged(0x82, Array("localhost".utf8))])),
            ]))
        return DER.sequence([
            DER.tagged(0xA0, DER.integer([2])),
            DER.integer(serial),
            DER.sequence([DER.objectIdentifier(DER.ecdsaWithSHA256)]),
            name,
            validity,
            name,
            subjectPublicKeyInfo,
            extensions,
        ])
    }
}

/// Minimal DER writer for the handful of structures a self-signed certificate needs.
enum DER {
    static let ecdsaWithSHA256: [UInt8] = [0x2A, 0x86, 0x48, 0xCE, 0x3D, 0x04, 0x03, 0x02]
    static let ecPublicKey: [UInt8] = [0x2A, 0x86, 0x48, 0xCE, 0x3D, 0x02, 0x01]
    static let prime256v1: [UInt8] = [0x2A, 0x86, 0x48, 0xCE, 0x3D, 0x03, 0x01, 0x07]
    static let commonName: [UInt8] = [0x55, 0x04, 0x03]
    static let keyUsage: [UInt8] = [0x55, 0x1D, 0x0F]
    static let subjectAltName: [UInt8] = [0x55, 0x1D, 0x11]
    static let extendedKeyUsage: [UInt8] = [0x55, 0x1D, 0x25]
    static let serverAuth: [UInt8] = [0x2B, 0x06, 0x01, 0x05, 0x05, 0x07, 0x03, 0x01]

    static func length(_ count: Int) -> [UInt8] {
        if count < 0x80 { return [UInt8(count)] }
        var bytes: [UInt8] = []
        var remaining = count
        while remaining > 0 {
            bytes.insert(UInt8(remaining & 0xff), at: 0)
            remaining >>= 8
        }
        return [0x80 | UInt8(bytes.count)] + bytes
    }

    static func tagged(_ tag: UInt8, _ content: [UInt8]) -> [UInt8] {
        [tag] + length(content.count) + content
    }

    static func sequence(_ items: [[UInt8]]) -> [UInt8] { tagged(0x30, items.flatMap { $0 }) }
    static func set(_ items: [[UInt8]]) -> [UInt8] { tagged(0x31, items.flatMap { $0 }) }
    static func objectIdentifier(_ encoded: [UInt8]) -> [UInt8] { tagged(0x06, encoded) }
    static func utf8String(_ text: String) -> [UInt8] { tagged(0x0C, Array(text.utf8)) }
    static func octetString(_ content: [UInt8]) -> [UInt8] { tagged(0x04, content) }

    static func integer(_ magnitude: [UInt8]) -> [UInt8] {
        let needsPadding = (magnitude.first ?? 0) & 0x80 != 0
        return tagged(0x02, (needsPadding ? [0x00] : []) + magnitude)
    }

    static func bitString(_ content: [UInt8], unusedBits: UInt8 = 0) -> [UInt8] {
        tagged(0x03, [unusedBits] + content)
    }

    static func utcTime(_ date: Date) -> [UInt8] {
        let formatter = DateFormatter()
        formatter.locale = Locale(identifier: "en_US_POSIX")
        formatter.timeZone = TimeZone(secondsFromGMT: 0)
        formatter.dateFormat = "yyMMddHHmmss'Z'"
        return tagged(0x17, Array(formatter.string(from: date).utf8))
    }

    static func extensionEntry(_ identifier: [UInt8], critical: Bool, value: [UInt8]) -> [UInt8] {
        var items = [objectIdentifier(identifier)]
        if critical { items.append(tagged(0x01, [0xFF])) }
        items.append(octetString(value))
        return sequence(items)
    }
}
