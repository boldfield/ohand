//! C-compatible capture bindings for Rust-to-Swift boundary.

use super::{create_ffi_error, OhAndError};
use crate::store::captures::Capture;
use std::ffi::CStr;
use std::os::raw::c_char;
use std::ptr;

/// C-compatible capture structure mirroring the Rust Capture type.
/// All pointer fields are owned by the caller and must remain valid
/// for the duration of use. String ownership is transferred as needed.
#[repr(C)]
pub struct OhAndCapture {
    pub capture_id: *mut c_char,
    pub text: *mut c_char,            // NULL if not present
    pub audio_reference: *mut c_char, // NULL if not present
    pub capture_instant: *mut c_char,
    pub timezone_id: *mut c_char,
    pub utc_offset_minutes: i32,
    pub locale: *mut c_char,
    pub calendar: *mut c_char,
    pub item_scope: *mut c_char,
    pub route_id: *mut c_char,
    pub entry_locked: u8, // 0 = false, 1 = true
    pub created_at: *mut c_char,
    pub session_topic: *mut c_char, // NULL if not present
}

/// Allocate and initialize an OhAndCapture.
#[no_mangle]
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn ohand_capture_new(
    capture_id: *const c_char,
    text: *const c_char,
    audio_reference: *const c_char,
    capture_instant: *const c_char,
    timezone_id: *const c_char,
    utc_offset_minutes: i32,
    locale: *const c_char,
    calendar: *const c_char,
    item_scope: *const c_char,
    route_id: *const c_char,
    entry_locked: u8,
    created_at: *const c_char,
    session_topic: *const c_char,
    out_error: *mut *mut OhAndError,
) -> *mut OhAndCapture {
    if out_error.is_null() {
        return ptr::null_mut();
    }

    // Check required pointers early to prevent undefined behavior
    if capture_id.is_null()
        || capture_instant.is_null()
        || timezone_id.is_null()
        || locale.is_null()
        || calendar.is_null()
        || item_scope.is_null()
        || route_id.is_null()
        || created_at.is_null()
    {
        unsafe {
            *out_error = create_ffi_error(anyhow::anyhow!("Null pointer for required parameter"));
        }
        return ptr::null_mut();
    }

    let capture_id_str = match unsafe { CStr::from_ptr(capture_id) }.to_str() {
        Ok(s) => s.to_string(),
        Err(e) => {
            unsafe {
                *out_error = create_ffi_error(anyhow::anyhow!("Invalid capture_id UTF-8: {}", e));
            }
            return ptr::null_mut();
        }
    };

    let text_str = if text.is_null() {
        None
    } else {
        match unsafe { CStr::from_ptr(text) }.to_str() {
            Ok(s) => Some(s.to_string()),
            Err(e) => {
                unsafe {
                    *out_error = create_ffi_error(anyhow::anyhow!("Invalid text UTF-8: {}", e));
                }
                return ptr::null_mut();
            }
        }
    };

    let audio_ref_str = if audio_reference.is_null() {
        None
    } else {
        match unsafe { CStr::from_ptr(audio_reference) }.to_str() {
            Ok(s) => Some(s.to_string()),
            Err(e) => {
                unsafe {
                    *out_error =
                        create_ffi_error(anyhow::anyhow!("Invalid audio_reference UTF-8: {}", e));
                }
                return ptr::null_mut();
            }
        }
    };

    let capture_instant_str = match unsafe { CStr::from_ptr(capture_instant) }.to_str() {
        Ok(s) => s.to_string(),
        Err(e) => {
            unsafe {
                *out_error =
                    create_ffi_error(anyhow::anyhow!("Invalid capture_instant UTF-8: {}", e));
            }
            return ptr::null_mut();
        }
    };

    let timezone_id_str = match unsafe { CStr::from_ptr(timezone_id) }.to_str() {
        Ok(s) => s.to_string(),
        Err(e) => {
            unsafe {
                *out_error = create_ffi_error(anyhow::anyhow!("Invalid timezone_id UTF-8: {}", e));
            }
            return ptr::null_mut();
        }
    };

    let locale_str = match unsafe { CStr::from_ptr(locale) }.to_str() {
        Ok(s) => s.to_string(),
        Err(e) => {
            unsafe {
                *out_error = create_ffi_error(anyhow::anyhow!("Invalid locale UTF-8: {}", e));
            }
            return ptr::null_mut();
        }
    };

    let calendar_str = match unsafe { CStr::from_ptr(calendar) }.to_str() {
        Ok(s) => s.to_string(),
        Err(e) => {
            unsafe {
                *out_error = create_ffi_error(anyhow::anyhow!("Invalid calendar UTF-8: {}", e));
            }
            return ptr::null_mut();
        }
    };

    let item_scope_str = match unsafe { CStr::from_ptr(item_scope) }.to_str() {
        Ok(s) => s.to_string(),
        Err(e) => {
            unsafe {
                *out_error = create_ffi_error(anyhow::anyhow!("Invalid item_scope UTF-8: {}", e));
            }
            return ptr::null_mut();
        }
    };

    let route_id_str = match unsafe { CStr::from_ptr(route_id) }.to_str() {
        Ok(s) => s.to_string(),
        Err(e) => {
            unsafe {
                *out_error = create_ffi_error(anyhow::anyhow!("Invalid route_id UTF-8: {}", e));
            }
            return ptr::null_mut();
        }
    };

    let created_at_str = match unsafe { CStr::from_ptr(created_at) }.to_str() {
        Ok(s) => s.to_string(),
        Err(e) => {
            unsafe {
                *out_error = create_ffi_error(anyhow::anyhow!("Invalid created_at UTF-8: {}", e));
            }
            return ptr::null_mut();
        }
    };

    let session_topic_str = if session_topic.is_null() {
        None
    } else {
        match unsafe { CStr::from_ptr(session_topic) }.to_str() {
            Ok(s) => Some(s.to_string()),
            Err(e) => {
                unsafe {
                    *out_error =
                        create_ffi_error(anyhow::anyhow!("Invalid session_topic UTF-8: {}", e));
                }
                return ptr::null_mut();
            }
        }
    };

    match Capture::new(
        capture_id_str,
        text_str,
        audio_ref_str,
        capture_instant_str,
        timezone_id_str,
        utc_offset_minutes,
        locale_str,
        calendar_str,
        item_scope_str,
        route_id_str,
        entry_locked != 0,
        created_at_str,
        session_topic_str,
    ) {
        Ok(capture) => match capture_to_c(&capture) {
            Ok(c_capture) => Box::into_raw(Box::new(c_capture)),
            Err(e) => {
                unsafe {
                    *out_error = create_ffi_error(e);
                }
                ptr::null_mut()
            }
        },
        Err(e) => {
            unsafe {
                *out_error = create_ffi_error(e);
            }
            ptr::null_mut()
        }
    }
}

/// Convert a Rust Capture to a C-compatible OhAndCapture.
/// This function is fallible due to NUL byte checking in string conversion.
fn capture_to_c(capture: &Capture) -> Result<OhAndCapture, anyhow::Error> {
    Ok(OhAndCapture {
        capture_id: string_to_c(&capture.capture_id)?,
        text: string_to_c_option(&capture.text)?,
        audio_reference: string_to_c_option(&capture.audio_reference)?,
        capture_instant: string_to_c(&capture.capture_instant)?,
        timezone_id: string_to_c(&capture.timezone_id)?,
        utc_offset_minutes: capture.utc_offset_minutes,
        locale: string_to_c(&capture.locale)?,
        calendar: string_to_c(&capture.calendar)?,
        item_scope: string_to_c(&capture.item_scope)?,
        route_id: string_to_c(&capture.route_id)?,
        entry_locked: if capture.entry_locked { 1 } else { 0 },
        created_at: string_to_c(&capture.created_at)?,
        session_topic: string_to_c_option(&capture.session_topic)?,
    })
}

/// Convert a Rust Capture from C-compatible representation.
#[allow(dead_code)]
fn capture_from_c(c_capture: &OhAndCapture) -> Result<Capture, anyhow::Error> {
    let capture_id = c_str_to_string(c_capture.capture_id)?;
    let text = c_str_to_option(c_capture.text)?;
    let audio_reference = c_str_to_option(c_capture.audio_reference)?;
    let capture_instant = c_str_to_string(c_capture.capture_instant)?;
    let timezone_id = c_str_to_string(c_capture.timezone_id)?;
    let locale = c_str_to_string(c_capture.locale)?;
    let calendar = c_str_to_string(c_capture.calendar)?;
    let item_scope = c_str_to_string(c_capture.item_scope)?;
    let route_id = c_str_to_string(c_capture.route_id)?;
    let created_at = c_str_to_string(c_capture.created_at)?;
    let session_topic = c_str_to_option(c_capture.session_topic)?;

    Capture::new(
        capture_id,
        text,
        audio_reference,
        capture_instant,
        timezone_id,
        c_capture.utc_offset_minutes,
        locale,
        calendar,
        item_scope,
        route_id,
        c_capture.entry_locked != 0,
        created_at,
        session_topic,
    )
}

/// Free an OhAndCapture returned from FFI.
#[no_mangle]
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn ohand_capture_free(capture: *mut OhAndCapture) {
    if !capture.is_null() {
        unsafe {
            let c = &*capture;
            free_c_string(c.capture_id);
            free_c_string(c.text);
            free_c_string(c.audio_reference);
            free_c_string(c.capture_instant);
            free_c_string(c.timezone_id);
            free_c_string(c.locale);
            free_c_string(c.calendar);
            free_c_string(c.item_scope);
            free_c_string(c.route_id);
            free_c_string(c.created_at);
            free_c_string(c.session_topic);
            let _ = Box::from_raw(capture);
        }
    }
}

// Helper functions for string conversion

#[allow(dead_code)]
fn string_to_c(s: &str) -> Result<*mut c_char, anyhow::Error> {
    std::ffi::CString::new(s)
        .map(|c_string| c_string.into_raw())
        .map_err(|e| anyhow::anyhow!("String contains interior NUL byte: {}", e))
}

#[allow(dead_code)]
fn string_to_c_option(s: &Option<String>) -> Result<*mut c_char, anyhow::Error> {
    match s {
        Some(s) => string_to_c(s),
        None => Ok(ptr::null_mut()),
    }
}

#[allow(dead_code)]
fn c_str_to_string(ptr: *const c_char) -> Result<String, anyhow::Error> {
    if ptr.is_null() {
        return Err(anyhow::anyhow!("Null pointer for non-optional string"));
    }
    unsafe { CStr::from_ptr(ptr) }
        .to_str()
        .map(|s| s.to_string())
        .map_err(|e| anyhow::anyhow!("Invalid UTF-8: {}", e))
}

#[allow(dead_code)]
fn c_str_to_option(ptr: *const c_char) -> Result<Option<String>, anyhow::Error> {
    if ptr.is_null() {
        Ok(None)
    } else {
        c_str_to_string(ptr).map(Some)
    }
}

fn free_c_string(ptr: *mut c_char) {
    if !ptr.is_null() {
        unsafe {
            let _ = std::ffi::CString::from_raw(ptr);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ffi::{ohand_error_free, ohand_error_message};
    use std::ffi::CString;

    #[test]
    fn test_capture_round_trip_ascii() {
        let capture_id = CString::new("test-123").unwrap();
        let text = CString::new("Hello, World!").unwrap();
        let capture_instant = CString::new("2024-01-01T12:00:00Z").unwrap();
        let timezone_id = CString::new("UTC").unwrap();
        let locale = CString::new("en-US").unwrap();
        let calendar = CString::new("gregorian").unwrap();
        let item_scope = CString::new("personal").unwrap();
        let route_id = CString::new("local").unwrap();
        let created_at = CString::new("2024-01-01T12:00:00Z").unwrap();

        let mut out_error: *mut OhAndError = ptr::null_mut();
        let result = ohand_capture_new(
            capture_id.as_ptr(),
            text.as_ptr(),
            ptr::null(),
            capture_instant.as_ptr(),
            timezone_id.as_ptr(),
            -300,
            locale.as_ptr(),
            calendar.as_ptr(),
            item_scope.as_ptr(),
            route_id.as_ptr(),
            0,
            created_at.as_ptr(),
            ptr::null(),
            &mut out_error,
        );

        assert!(!result.is_null(), "Capture creation failed");
        assert!(out_error.is_null(), "Error should be null on success");

        unsafe {
            let c_capture = &*result;
            assert_eq!(
                CStr::from_ptr(c_capture.capture_id).to_str().unwrap(),
                "test-123"
            );
            assert_eq!(
                CStr::from_ptr(c_capture.text).to_str().unwrap(),
                "Hello, World!"
            );
            assert_eq!(c_capture.utc_offset_minutes, -300);
            assert_eq!(c_capture.entry_locked, 0);
            ohand_capture_free(result);
        }
    }

    #[test]
    fn test_capture_unicode() {
        let capture_id = CString::new("test-unicode").unwrap();
        let text = CString::new("Héllo, 世界! 🌍").unwrap();
        let capture_instant = CString::new("2024-01-01T12:00:00Z").unwrap();
        let timezone_id = CString::new("UTC").unwrap();
        let locale = CString::new("en-US").unwrap();
        let calendar = CString::new("gregorian").unwrap();
        let item_scope = CString::new("personal").unwrap();
        let route_id = CString::new("local").unwrap();
        let created_at = CString::new("2024-01-01T12:00:00Z").unwrap();

        let mut out_error: *mut OhAndError = ptr::null_mut();
        let result = ohand_capture_new(
            capture_id.as_ptr(),
            text.as_ptr(),
            ptr::null(),
            capture_instant.as_ptr(),
            timezone_id.as_ptr(),
            0,
            locale.as_ptr(),
            calendar.as_ptr(),
            item_scope.as_ptr(),
            route_id.as_ptr(),
            0,
            created_at.as_ptr(),
            ptr::null(),
            &mut out_error,
        );

        assert!(!result.is_null(), "Unicode capture creation failed");
        assert!(out_error.is_null(), "Error should be null on success");

        unsafe {
            let c_capture = &*result;
            assert_eq!(
                CStr::from_ptr(c_capture.text).to_str().unwrap(),
                "Héllo, 世界! 🌍"
            );
            ohand_capture_free(result);
        }
    }

    #[test]
    fn test_capture_optional_fields() {
        let capture_id = CString::new("test-optional").unwrap();
        let capture_instant = CString::new("2024-01-01T12:00:00Z").unwrap();
        let timezone_id = CString::new("UTC").unwrap();
        let locale = CString::new("en-US").unwrap();
        let calendar = CString::new("gregorian").unwrap();
        let item_scope = CString::new("personal").unwrap();
        let route_id = CString::new("local").unwrap();
        let created_at = CString::new("2024-01-01T12:00:00Z").unwrap();

        let mut out_error: *mut OhAndError = ptr::null_mut();
        let result = ohand_capture_new(
            capture_id.as_ptr(),
            ptr::null(), // No text
            ptr::null(), // No audio_reference
            capture_instant.as_ptr(),
            timezone_id.as_ptr(),
            0,
            locale.as_ptr(),
            calendar.as_ptr(),
            item_scope.as_ptr(),
            route_id.as_ptr(),
            0,
            created_at.as_ptr(),
            ptr::null(), // No session_topic
            &mut out_error,
        );

        // This should fail because Capture requires either text or audio_reference
        assert!(result.is_null(), "Should fail without text or audio");
        assert!(!out_error.is_null(), "Error should be set");
        ohand_error_free(out_error);
    }

    #[test]
    fn test_capture_locked_entry() {
        let capture_id = CString::new("test-locked").unwrap();
        let text = CString::new("Secret message").unwrap();
        let capture_instant = CString::new("2024-01-01T12:00:00Z").unwrap();
        let timezone_id = CString::new("UTC").unwrap();
        let locale = CString::new("en-US").unwrap();
        let calendar = CString::new("gregorian").unwrap();
        let item_scope = CString::new("private").unwrap();
        let route_id = CString::new("local").unwrap();
        let created_at = CString::new("2024-01-01T12:00:00Z").unwrap();

        let mut out_error: *mut OhAndError = ptr::null_mut();
        let result = ohand_capture_new(
            capture_id.as_ptr(),
            text.as_ptr(),
            ptr::null(),
            capture_instant.as_ptr(),
            timezone_id.as_ptr(),
            0,
            locale.as_ptr(),
            calendar.as_ptr(),
            item_scope.as_ptr(),
            route_id.as_ptr(),
            1, // entry_locked = true
            created_at.as_ptr(),
            ptr::null(),
            &mut out_error,
        );

        assert!(!result.is_null(), "Capture creation failed");
        unsafe {
            let c_capture = &*result;
            assert_eq!(c_capture.entry_locked, 1);
            ohand_capture_free(result);
        }
    }

    #[test]
    fn test_capture_large_input() {
        let capture_id = CString::new("test-large").unwrap();
        let large_text = "x".repeat(1_000_000); // 1 MB
        let text = CString::new(large_text).unwrap();
        let capture_instant = CString::new("2024-01-01T12:00:00Z").unwrap();
        let timezone_id = CString::new("UTC").unwrap();
        let locale = CString::new("en-US").unwrap();
        let calendar = CString::new("gregorian").unwrap();
        let item_scope = CString::new("personal").unwrap();
        let route_id = CString::new("local").unwrap();
        let created_at = CString::new("2024-01-01T12:00:00Z").unwrap();

        let mut out_error: *mut OhAndError = ptr::null_mut();
        let result = ohand_capture_new(
            capture_id.as_ptr(),
            text.as_ptr(),
            ptr::null(),
            capture_instant.as_ptr(),
            timezone_id.as_ptr(),
            0,
            locale.as_ptr(),
            calendar.as_ptr(),
            item_scope.as_ptr(),
            route_id.as_ptr(),
            0,
            created_at.as_ptr(),
            ptr::null(),
            &mut out_error,
        );

        assert!(!result.is_null(), "Large capture creation failed");
        assert!(out_error.is_null(), "Error should be null on success");
        unsafe {
            let c_capture = &*result;
            let retrieved_text = CStr::from_ptr(c_capture.text).to_str().unwrap();
            assert_eq!(retrieved_text.len(), 1_000_000);
            ohand_capture_free(result);
        }
    }

    #[test]
    fn test_capture_error_memory_cleanup() {
        let capture_id = CString::new("test-error").unwrap();
        let text = CString::new("Test error").unwrap();
        let capture_instant = CString::new("2024-01-01T12:00:00Z").unwrap();
        let timezone_id = CString::new("UTC").unwrap();
        let locale = CString::new("en-US").unwrap();
        let calendar = CString::new("gregorian").unwrap();
        let item_scope = CString::new("personal").unwrap();
        let route_id = CString::new("local").unwrap();
        let created_at = CString::new("2024-01-01T12:00:00Z").unwrap();

        // Simulate error by passing null error output (invalid)
        let result = ohand_capture_new(
            capture_id.as_ptr(),
            text.as_ptr(),
            ptr::null(),
            capture_instant.as_ptr(),
            timezone_id.as_ptr(),
            0,
            locale.as_ptr(),
            calendar.as_ptr(),
            item_scope.as_ptr(),
            route_id.as_ptr(),
            0,
            created_at.as_ptr(),
            ptr::null(),
            ptr::null_mut(), // Invalid: null error output
        );

        assert!(result.is_null(), "Should fail with null error output");
    }

    #[test]
    fn test_error_message_lifetime() {
        let capture_id = CString::new("test-error-msg").unwrap();
        let capture_instant = CString::new("2024-01-01T12:00:00Z").unwrap();
        let timezone_id = CString::new("UTC").unwrap();
        let locale = CString::new("en-US").unwrap();
        let calendar = CString::new("gregorian").unwrap();
        let item_scope = CString::new("personal").unwrap();
        let route_id = CString::new("local").unwrap();
        let created_at = CString::new("2024-01-01T12:00:00Z").unwrap();

        let mut out_error: *mut OhAndError = ptr::null_mut();
        let result = ohand_capture_new(
            capture_id.as_ptr(),
            ptr::null(), // No text - should fail
            ptr::null(),
            capture_instant.as_ptr(),
            timezone_id.as_ptr(),
            0,
            locale.as_ptr(),
            calendar.as_ptr(),
            item_scope.as_ptr(),
            route_id.as_ptr(),
            0,
            created_at.as_ptr(),
            ptr::null(),
            &mut out_error,
        );

        assert!(result.is_null(), "Should fail without text");
        assert!(!out_error.is_null(), "Error should be set");

        unsafe {
            let error_msg = ohand_error_message(out_error);
            assert!(!error_msg.is_null(), "Error message should not be null");
            let msg = CStr::from_ptr(error_msg).to_str().unwrap();
            assert!(!msg.is_empty(), "Error message should not be empty");
            ohand_error_free(out_error);
        }
    }

    #[test]
    fn test_null_required_parameter() {
        let text = CString::new("Test").unwrap();
        let capture_instant = CString::new("2024-01-01T12:00:00Z").unwrap();
        let timezone_id = CString::new("UTC").unwrap();
        let locale = CString::new("en-US").unwrap();
        let calendar = CString::new("gregorian").unwrap();
        let item_scope = CString::new("personal").unwrap();
        let route_id = CString::new("local").unwrap();
        let created_at = CString::new("2024-01-01T12:00:00Z").unwrap();

        let mut out_error: *mut OhAndError = ptr::null_mut();
        let result = ohand_capture_new(
            ptr::null(), // Null capture_id - required field
            text.as_ptr(),
            ptr::null(),
            capture_instant.as_ptr(),
            timezone_id.as_ptr(),
            0,
            locale.as_ptr(),
            calendar.as_ptr(),
            item_scope.as_ptr(),
            route_id.as_ptr(),
            0,
            created_at.as_ptr(),
            ptr::null(),
            &mut out_error,
        );

        assert!(result.is_null(), "Should fail with null required parameter");
        assert!(!out_error.is_null(), "Error should be set");
        unsafe {
            let msg = CStr::from_ptr(ohand_error_message(out_error))
                .to_str()
                .unwrap();
            assert!(
                msg.contains("Null pointer"),
                "Error should mention null pointer"
            );
            ohand_error_free(out_error);
        }
    }

    #[test]
    fn test_invalid_utf8_in_required_field() {
        // Invalid UTF-8 sequence: 0xFF followed by NUL terminator
        let invalid_bytes = [0xFF_u8, 0x00];
        let capture_id = unsafe { CStr::from_bytes_with_nul_unchecked(&invalid_bytes) };
        let text = CString::new("Test").unwrap();
        let capture_instant = CString::new("2024-01-01T12:00:00Z").unwrap();
        let timezone_id = CString::new("UTC").unwrap();
        let locale = CString::new("en-US").unwrap();
        let calendar = CString::new("gregorian").unwrap();
        let item_scope = CString::new("personal").unwrap();
        let route_id = CString::new("local").unwrap();
        let created_at = CString::new("2024-01-01T12:00:00Z").unwrap();

        let mut out_error: *mut OhAndError = ptr::null_mut();
        let result = ohand_capture_new(
            capture_id.as_ptr(),
            text.as_ptr(),
            ptr::null(),
            capture_instant.as_ptr(),
            timezone_id.as_ptr(),
            0,
            locale.as_ptr(),
            calendar.as_ptr(),
            item_scope.as_ptr(),
            route_id.as_ptr(),
            0,
            created_at.as_ptr(),
            ptr::null(),
            &mut out_error,
        );

        assert!(result.is_null(), "Should fail with invalid UTF-8");
        assert!(!out_error.is_null(), "Error should be set");
        let msg = unsafe {
            CStr::from_ptr(ohand_error_message(out_error))
                .to_str()
                .unwrap()
        };
        assert!(msg.contains("UTF-8"), "Error should mention UTF-8 failure");
        ohand_error_free(out_error);
    }

    #[test]
    fn test_ownership_and_cleanup_multiple_calls() {
        // Multiple successful calls to verify memory is properly managed
        for i in 0..10 {
            let capture_id = format!("test-cleanup-{}", i);
            let capture_id_c = CString::new(capture_id).unwrap();
            let text = CString::new("Test cleanup").unwrap();
            let capture_instant = CString::new("2024-01-01T12:00:00Z").unwrap();
            let timezone_id = CString::new("UTC").unwrap();
            let locale = CString::new("en-US").unwrap();
            let calendar = CString::new("gregorian").unwrap();
            let item_scope = CString::new("personal").unwrap();
            let route_id = CString::new("local").unwrap();
            let created_at = CString::new("2024-01-01T12:00:00Z").unwrap();

            let mut out_error: *mut OhAndError = ptr::null_mut();
            let result = ohand_capture_new(
                capture_id_c.as_ptr(),
                text.as_ptr(),
                ptr::null(),
                capture_instant.as_ptr(),
                timezone_id.as_ptr(),
                0,
                locale.as_ptr(),
                calendar.as_ptr(),
                item_scope.as_ptr(),
                route_id.as_ptr(),
                0,
                created_at.as_ptr(),
                ptr::null(),
                &mut out_error,
            );

            assert!(!result.is_null(), "Iteration {} should succeed", i);
            assert!(out_error.is_null(), "No error on successful creation");
            ohand_capture_free(result);
        }
    }

    #[test]
    fn test_capture_very_large_input() {
        let capture_id = CString::new("test-very-large").unwrap();
        // Test with 10 MB text to verify handling of large inputs
        let large_text = "x".repeat(10_000_000);
        let text = CString::new(large_text).unwrap();
        let capture_instant = CString::new("2024-01-01T12:00:00Z").unwrap();
        let timezone_id = CString::new("UTC").unwrap();
        let locale = CString::new("en-US").unwrap();
        let calendar = CString::new("gregorian").unwrap();
        let item_scope = CString::new("personal").unwrap();
        let route_id = CString::new("local").unwrap();
        let created_at = CString::new("2024-01-01T12:00:00Z").unwrap();

        let mut out_error: *mut OhAndError = ptr::null_mut();
        let result = ohand_capture_new(
            capture_id.as_ptr(),
            text.as_ptr(),
            ptr::null(),
            capture_instant.as_ptr(),
            timezone_id.as_ptr(),
            0,
            locale.as_ptr(),
            calendar.as_ptr(),
            item_scope.as_ptr(),
            route_id.as_ptr(),
            0,
            created_at.as_ptr(),
            ptr::null(),
            &mut out_error,
        );

        assert!(
            !result.is_null(),
            "Very large capture creation should succeed"
        );
        assert!(out_error.is_null(), "No error on valid large input");
        unsafe {
            let c_capture = &*result;
            let retrieved_text = CStr::from_ptr(c_capture.text).to_str().unwrap();
            assert_eq!(retrieved_text.len(), 10_000_000);
            ohand_capture_free(result);
        }
    }
}
