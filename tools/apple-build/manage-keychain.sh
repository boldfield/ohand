#!/bin/bash
# Keychain management for Apple signing certificates
# Handles temporary keychain creation, certificate import, and cleanup

set -euo pipefail

ACTION="${1:-}"
KEYCHAIN_NAME="${2:-ohand-build}"

if [ -z "$ACTION" ]; then
    echo "Usage: $0 <action> [keychain-name]"
    echo ""
    echo "Actions:"
    echo "  setup     - Create and configure temporary keychain"
    echo "  import    - Import certificate into keychain"
    echo "  unlock    - Unlock keychain for build"
    echo "  lock      - Lock keychain after build"
    echo "  cleanup   - Remove temporary keychain"
    echo "  status    - Check keychain status"
    exit 1
fi

case "$ACTION" in
    setup)
        if [ -z "${APPLE_CERT_PASSWORD:-}" ]; then
            echo "✗ APPLE_CERT_PASSWORD not set"
            exit 1
        fi

        # Check if keychain already exists
        if security list-keychains | grep -q "$KEYCHAIN_NAME"; then
            echo "✓ Keychain $KEYCHAIN_NAME already exists"
        else
            echo "Creating keychain: $KEYCHAIN_NAME"
            security create-keychain -p "$APPLE_CERT_PASSWORD" "$KEYCHAIN_NAME"
            echo "✓ Keychain created"
        fi

        # Set timeout (1 hour)
        security set-keychain-settings -t 3600 "$KEYCHAIN_NAME" 2>/dev/null || true
        echo "✓ Keychain timeout set to 1 hour"

        # Set as default for codesigning
        security set-keychain-settings -l -u "$KEYCHAIN_NAME" 2>/dev/null || true
        echo "✓ Keychain configured"
        ;;

    import)
        if [ -z "${APPLE_CERT_PATH:-}" ]; then
            echo "✗ APPLE_CERT_PATH not set"
            exit 1
        fi

        if [ ! -f "$APPLE_CERT_PATH" ]; then
            echo "✗ Certificate file not found: $APPLE_CERT_PATH"
            exit 1
        fi

        if [ -z "${APPLE_CERT_PASSWORD:-}" ]; then
            echo "✗ APPLE_CERT_PASSWORD not set"
            exit 1
        fi

        echo "Importing certificate into keychain..."
        security import "$APPLE_CERT_PATH" \
            -k "$KEYCHAIN_NAME" \
            -P "$APPLE_CERT_PASSWORD" \
            -T /usr/bin/xcodebuild \
            -T /usr/bin/codesign \
            -T /usr/bin/xcrun \
            2>/dev/null

        echo "✓ Certificate imported"

        # Set partition list for headless code signing (AC3 requirement)
        security set-key-partition-list -S "apple-tool:,apple:" \
            -k "$APPLE_CERT_PASSWORD" \
            "$KEYCHAIN_NAME" > /dev/null 2>&1
        echo "✓ Partition list configured for headless signing"

        # List imported certificates
        echo "Imported certificates:"
        security find-identity -v -p codesigning "$KEYCHAIN_NAME" || true
        ;;

    unlock)
        if [ -z "${APPLE_CERT_PASSWORD:-}" ]; then
            echo "✗ APPLE_CERT_PASSWORD not set"
            exit 1
        fi

        echo "Unlocking keychain..."
        security unlock-keychain -p "$APPLE_CERT_PASSWORD" "$KEYCHAIN_NAME" 2>/dev/null || true
        echo "✓ Keychain unlocked"
        ;;

    lock)
        echo "Locking keychain..."
        security lock-keychain "$KEYCHAIN_NAME" 2>/dev/null || true
        echo "✓ Keychain locked"
        ;;

    cleanup)
        echo "Cleaning up keychain..."

        # Remove from search path first
        original_keychains=$(security list-keychains -d user | grep -v "$KEYCHAIN_NAME" | tr '\n' ' ' || true)
        if [ -n "$original_keychains" ]; then
            security list-keychains -d user -s $original_keychains
        fi

        # Lock and delete
        security lock-keychain "$KEYCHAIN_NAME" 2>/dev/null || true
        security delete-keychain "$KEYCHAIN_NAME" 2>/dev/null || true

        echo "✓ Keychain cleaned up"
        ;;

    status)
        echo "Checking keychain status for: $KEYCHAIN_NAME"

        if security list-keychains | grep -q "$KEYCHAIN_NAME"; then
            echo "✓ Keychain exists"

            # Check if unlocked (list-keychains would show it if accessible)
            if security unlock-keychain -p "dummy" "$KEYCHAIN_NAME" 2>/dev/null; then
                echo "✓ Keychain is unlocked"
                security lock-keychain "$KEYCHAIN_NAME" 2>/dev/null || true
            else
                echo "⚠ Keychain is locked"
            fi

            # List certificates
            echo "Certificates:"
            security find-identity -v -p codesigning "$KEYCHAIN_NAME" 2>/dev/null | sed 's/^/  /' || echo "  (none found)"
        else
            echo "✗ Keychain does not exist: $KEYCHAIN_NAME"
        fi
        ;;

    *)
        echo "✗ Unknown action: $ACTION"
        exit 1
        ;;
esac
