#include <cstdarg>
#include <cstdint>
#include <cstdlib>
#include <ostream>
#include <new>

/// Opaque handle to a Rust error for FFI boundary.
/// Created by FFI functions that can fail and must be freed with
/// `ohand_error_free`.
struct OhAndError;

/// C-compatible capture structure mirroring the Rust Capture type.
/// All pointer fields are owned by the caller and must remain valid
/// for the duration of use. String ownership is transferred as needed.
struct OhAndCapture {
  char *capture_id;
  char *text;
  char *audio_reference;
  char *capture_instant;
  char *timezone_id;
  int32_t utc_offset_minutes;
  char *locale;
  char *calendar;
  char *item_scope;
  char *route_id;
  uint8_t entry_locked;
  char *created_at;
  char *session_topic;
};

extern "C" {

/// Free an error returned from FFI.
void ohand_error_free(OhAndError *err);

/// Get the error message from an OhAndError.
/// The returned pointer is valid only for the lifetime of the error.
const char *ohand_error_message(const OhAndError *err);

/// Allocate and initialize an OhAndCapture.
OhAndCapture *ohand_capture_new(const char *capture_id,
                                const char *text,
                                const char *audio_reference,
                                const char *capture_instant,
                                const char *timezone_id,
                                int32_t utc_offset_minutes,
                                const char *locale,
                                const char *calendar,
                                const char *item_scope,
                                const char *route_id,
                                uint8_t entry_locked,
                                const char *created_at,
                                const char *session_topic,
                                OhAndError **out_error);

/// Free an OhAndCapture returned from FFI.
void ohand_capture_free(OhAndCapture *capture);

} // extern "C"
