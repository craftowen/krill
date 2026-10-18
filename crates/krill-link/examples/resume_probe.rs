//! Unix-only resumable transport probe.
#![forbid(unsafe_code)]

#[cfg(unix)]
#[path = "support/resume_probe_unix.rs"]
mod unix;

#[cfg(unix)]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    unix::run()
}

#[cfg(not(unix))]
fn main() -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "resume probe requires Unix sockets",
    ))
}
