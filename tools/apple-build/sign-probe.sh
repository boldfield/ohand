#!/bin/bash
# Reproducible signed build automation for Oh And probes
# Requires externally supplied Apple identity and credentials via environment variables.
# Simulator builds are unsigned; device/adhoc builds require signing inputs.

set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# Configuration
PROBE_NAME="${1:-}"
BUILD_TYPE="${2:-simulator}"  # simulator, adhoc, or appstore
EVIDENCE_DIR_ARG="${3:-.evidence}"
KEYCHAIN_NAME="ohand-build"
KEYCHAIN_TIMEOUT=3600  # 1 hour

# Resolve evidence directory to absolute path
if [[ "$EVIDENCE_DIR_ARG" = /* ]]; then
    EVIDENCE_DIR="$EVIDENCE_DIR_ARG"
else
    EVIDENCE_DIR="$(cd "$(pwd)" && mkdir -p "$EVIDENCE_DIR_ARG" && cd "$EVIDENCE_DIR_ARG" && pwd)"
fi

if [ -z "$PROBE_NAME" ]; then
    echo "Usage: $0 <probe-name> [build-type] [evidence-dir]"
    echo ""
    echo "Probe names: BridgeProbe, NotificationProbe, AudioProbe, TranscriptionProbe, CredentialProbe, CaptureProbe"
    echo "Build types: simulator (default), adhoc, appstore"
    echo ""
    echo "Environment variables for signed builds:"
    echo "  APPLE_TEAM_ID:        Apple Developer Team ID (e.g., 'ABC123DEF4')"
    echo "  APPLE_CERT_PATH:      Path to .p12 certificate file"
    echo "  APPLE_CERT_PASSWORD:  Certificate password"
    echo "  APPLE_PROFILE_PATH:   Path to .mobileprovision file"
    exit 1
fi

if [ "$BUILD_TYPE" = "simulator" ]; then
    # Simulator builds are unsigned; no credentials needed
    echo "Building unsigned simulator build for $PROBE_NAME"

    mkdir -p "$EVIDENCE_DIR"

    cd "$(dirname "$script_dir")"/../ios

    # Generate project
    ./scripts/generate.sh > "$EVIDENCE_DIR/${PROBE_NAME}-generate.log" 2>&1

    # Build
    xcodebuild build \
        -project OhAnd.xcodeproj \
        -scheme "$PROBE_NAME" \
        -configuration Debug \
        -sdk iphonesimulator \
        -derivedDataPath .derived \
        CODE_SIGNING_ALLOWED=NO \
        2>&1 | tee "$EVIDENCE_DIR/${PROBE_NAME}-simulator-build.log"

    # Record build metadata (content-free)
    BUILD_DIR=".derived/Build/Products/Debug-iphonesimulator"
    if [ -d "$BUILD_DIR/${PROBE_NAME}.app" ]; then
        cat > "$EVIDENCE_DIR/${PROBE_NAME}-build-metadata.txt" <<EOF
Build Type: Simulator (Unsigned)
Probe: $PROBE_NAME
Build Time: $(date -u +"%Y-%m-%dT%H:%M:%SZ")
Build Configuration: Debug
SDK: iphonesimulator
Product Location: $BUILD_DIR/${PROBE_NAME}.app
Signing: None (simulator)
EOF
        echo "✓ Simulator build complete. Evidence recorded in $EVIDENCE_DIR/${PROBE_NAME}-build-metadata.txt"
    else
        echo "✗ Build failed: product not found at $BUILD_DIR/${PROBE_NAME}.app"
        exit 1
    fi

elif [ "$BUILD_TYPE" = "adhoc" ] || [ "$BUILD_TYPE" = "appstore" ]; then
    # Device/distributed builds require signing

    if [ -z "${APPLE_TEAM_ID:-}" ]; then
        echo "✗ Error: APPLE_TEAM_ID not set. Required for $BUILD_TYPE builds."
        exit 1
    fi

    if [ -z "${APPLE_CERT_PATH:-}" ] || [ ! -f "$APPLE_CERT_PATH" ]; then
        echo "✗ Error: APPLE_CERT_PATH not set or certificate file not found."
        echo "   Signed builds require a .p12 certificate."
        exit 1
    fi

    if [ -z "${APPLE_CERT_PASSWORD:-}" ]; then
        echo "✗ Error: APPLE_CERT_PASSWORD not set. Required to unlock certificate."
        exit 1
    fi

    if [ -z "${APPLE_PROFILE_PATH:-}" ] || [ ! -f "$APPLE_PROFILE_PATH" ]; then
        echo "✗ Error: APPLE_PROFILE_PATH not set or provisioning profile not found."
        exit 1
    fi

    echo "Building $BUILD_TYPE-signed $PROBE_NAME"

    mkdir -p "$EVIDENCE_DIR"

    cd "$(dirname "$script_dir")"/../ios

    # Set up temporary keychain for signing
    echo "Setting up temporary keychain for signing..."

    # Create a temporary keychain (or use existing if already set up)
    if ! security list-keychains | grep -q "$KEYCHAIN_NAME"; then
        security create-keychain -p "temp-$$" "$KEYCHAIN_NAME"
    fi

    # Lock timeout
    security set-keychain-settings -t "$KEYCHAIN_TIMEOUT" "$KEYCHAIN_NAME" 2>/dev/null || true

    # Import certificate to keychain
    security import "$APPLE_CERT_PATH" \
        -k "$KEYCHAIN_NAME" \
        -P "$APPLE_CERT_PASSWORD" \
        -T /usr/bin/xcodebuild \
        -T /usr/bin/codesign \
        > /dev/null 2>&1

    # Add keychain to search path temporarily
    original_keychains=$(security list-keychains -d user | tr '\n' ' ')
    security list-keychains -d user -s "$KEYCHAIN_NAME" $original_keychains

    # Unlock keychain
    security unlock-keychain -p "temp-$$" "$KEYCHAIN_NAME" 2>/dev/null || true

    # Import provisioning profile
    PROFILES_DIR="$HOME/Library/MobileDevice/Provisioning\ Profiles"
    mkdir -p "$PROFILES_DIR"
    cp "$APPLE_PROFILE_PATH" "$PROFILES_DIR/$(basename "$APPLE_PROFILE_PATH")"

    # Generate project
    ./scripts/generate.sh > "$EVIDENCE_DIR/${PROBE_NAME}-generate.log" 2>&1

    # Determine export method
    case "$BUILD_TYPE" in
        adhoc)
            EXPORT_METHOD="ad-hoc"
            BUILD_CONFIG="Release"
            SDK="iphoneos"
            ;;
        appstore)
            EXPORT_METHOD="app-store"
            BUILD_CONFIG="Release"
            SDK="iphoneos"
            ;;
    esac

    # Build and sign
    xcodebuild build \
        -project OhAnd.xcodeproj \
        -scheme "$PROBE_NAME" \
        -configuration "$BUILD_CONFIG" \
        -sdk "$SDK" \
        -derivedDataPath .derived \
        DEVELOPMENT_TEAM="$APPLE_TEAM_ID" \
        CODE_SIGNING_ALLOWED=YES \
        CODE_SIGN_STYLE=Automatic \
        2>&1 | tee "$EVIDENCE_DIR/${PROBE_NAME}-${BUILD_TYPE}-build.log"

    # Record signed build metadata (content-free)
    BUILD_DIR=".derived/Build/Products/${BUILD_CONFIG}-iphoneos"
    if [ -d "$BUILD_DIR/${PROBE_NAME}.app" ]; then
        # Extract code signing identity (sanitized)
        IDENTITY_HASH=$(codesign -dv "$BUILD_DIR/${PROBE_NAME}.app" 2>&1 | grep "^Authority=" | md5sum | cut -d' ' -f1)
        BUNDLE_ID=$(plutil -extract CFBundleIdentifier raw "$BUILD_DIR/${PROBE_NAME}.app/Info.plist")

        cat > "$EVIDENCE_DIR/${PROBE_NAME}-build-metadata.txt" <<EOF
Build Type: $BUILD_TYPE
Probe: $PROBE_NAME
Bundle Identifier: $BUNDLE_ID
Build Time: $(date -u +"%Y-%m-%dT%H:%M:%SZ")
Build Configuration: $BUILD_CONFIG
SDK: $SDK
Product Location: $BUILD_DIR/${PROBE_NAME}.app
Code Signing Identity Hash: $IDENTITY_HASH
Team ID: (sanitized)
Provisioning Profile: (sanitized)
Export Method: $EXPORT_METHOD
EOF
        echo "✓ $BUILD_TYPE build complete. Evidence recorded in $EVIDENCE_DIR/${PROBE_NAME}-build-metadata.txt"
    else
        echo "✗ Build failed: product not found at $BUILD_DIR/${PROBE_NAME}.app"
        exit 1
    fi

    # Cleanup: restore original keychain search path and lock temporary keychain
    security list-keychains -d user -s $original_keychains
    security lock-keychain "$KEYCHAIN_NAME" 2>/dev/null || true

else
    echo "✗ Unknown build type: $BUILD_TYPE"
    exit 1
fi
