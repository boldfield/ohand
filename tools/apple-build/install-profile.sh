#!/bin/bash
# Install and manage provisioning profiles for device builds

set -euo pipefail

ACTION="${1:-}"
PROFILE_PATH="${2:-}"

PROFILES_DIR="$HOME/Library/MobileDevice/Provisioning\ Profiles"

if [ -z "$ACTION" ]; then
    echo "Usage: $0 <action> [profile-path]"
    echo ""
    echo "Actions:"
    echo "  install   - Install provisioning profile"
    echo "  list      - List installed profiles"
    echo "  remove    - Remove profile"
    echo "  cleanup   - Remove all ohand profiles"
    exit 1
fi

case "$ACTION" in
    install)
        if [ -z "$PROFILE_PATH" ]; then
            echo "✗ Profile path not specified"
            exit 1
        fi

        if [ ! -f "$PROFILE_PATH" ]; then
            echo "✗ Profile file not found: $PROFILE_PATH"
            exit 1
        fi

        mkdir -p "$PROFILES_DIR"

        PROFILE_NAME="$(basename "$PROFILE_PATH")"
        PROFILE_UUID=$(security cms -D -i "$PROFILE_PATH" 2>/dev/null | grep -A 1 "UUID" | tail -1 | grep -oE '[A-F0-9-]{36}' || echo "$PROFILE_NAME")

        DEST="$PROFILES_DIR/$PROFILE_UUID.mobileprovision"

        echo "Installing provisioning profile: $PROFILE_NAME"
        echo "  UUID: $PROFILE_UUID"

        cp "$PROFILE_PATH" "$DEST"

        echo "✓ Profile installed to: $DEST"
        ;;

    list)
        echo "Installed provisioning profiles:"

        if [ -d "$PROFILES_DIR" ]; then
            ls -1 "$PROFILES_DIR" 2>/dev/null | while read -r profile; do
                PROFILE_PATH="$PROFILES_DIR/$profile"
                if [ -f "$PROFILE_PATH" ]; then
                    # Extract profile info (requires openssl or security command)
                    NAME=$(security cms -D -i "$PROFILE_PATH" 2>/dev/null | grep -o '<key>Name</key>.*<string>\([^<]*\)</string>' | sed 's/.*<string>\([^<]*\)<\/string>.*/\1/' || echo "Unknown")
                    echo "  $profile"
                    echo "    Name: $NAME"
                fi
            done
        else
            echo "  (no profiles directory)"
        fi
        ;;

    remove)
        if [ -z "$PROFILE_PATH" ]; then
            echo "✗ Profile path/UUID not specified"
            exit 1
        fi

        # Support both full path and UUID
        if [[ "$PROFILE_PATH" == *".mobileprovision"* ]]; then
            PROFILE_UUID=$(basename "$PROFILE_PATH" .mobileprovision)
        else
            PROFILE_UUID="$PROFILE_PATH"
        fi

        DEST="$PROFILES_DIR/$PROFILE_UUID.mobileprovision"

        if [ -f "$DEST" ]; then
            echo "Removing provisioning profile: $PROFILE_UUID"
            rm "$DEST"
            echo "✓ Profile removed"
        else
            echo "✗ Profile not found: $DEST"
            exit 1
        fi
        ;;

    cleanup)
        echo "Removing all ohand provisioning profiles..."

        if [ -d "$PROFILES_DIR" ]; then
            # This is a safety feature - only removes profiles we can identify as ohand-related
            # In practice, you should manually verify which profiles to remove
            echo "⚠ Manual cleanup recommended. Profiles in:"
            echo "  $PROFILES_DIR"
            echo ""
            echo "Remove specific profiles with: $0 remove <uuid>"
        else
            echo "✓ No profiles directory found"
        fi
        ;;

    *)
        echo "✗ Unknown action: $ACTION"
        exit 1
        ;;
esac
