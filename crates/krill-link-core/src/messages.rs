//! CLI messages introduced by resumable link settings.
pub const VALUE_REQUIRED: &str = "value required";
pub const INVALID_GRACE: &str = "invalid grace period: enter milliseconds";
pub const INVALID_BUFFER: &str = "invalid buffer limit: enter bytes";
pub const INVALID_RESUME_SETTINGS: &str =
    "resume grace must be 1..=600000 ms and buffer 1..=4194304 bytes";
