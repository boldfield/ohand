// Non-private item preview eligibility (route-based, not classifier).
//
// This module handles the determination of preview-safe route permissions
// and whether an item is eligible for display in notification previews.
// It enforces the invariant that preview eligibility can only be narrowed
// by classification or user action, never broadened by derived data.
