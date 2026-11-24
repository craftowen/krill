//! Process-wide runtime build identity supplied by shipped executables at startup.
//!
//! `krill-proto` pins an internal package version (`0.0.0`) so release version bumps do not
//! invalidate compile caches for every downstream library crate. Each shipped binary (`krill`,
//! `krilld`, `krill-link`, `krill-app`) registers `(krill_version::VERSION, krill_version::BUILD)`
//! once via [`init_build_info`] at process entry.

use std::sync::OnceLock;

// In isolated library unit tests (where no binary `main()` calls `init_build_info`), use a
// fixed semantic version between legacy test fixtures (`0.0.1`, `0.1.0`) and future fixtures
// (`99.0.0`, `999.0.0`). Shipped binaries always override this at startup with `krill-version`.
const FALLBACK_VERSION: &str = "0.2.0";
const FALLBACK_BUILD: &str = "0.2.0+dev.0000000000000000";

#[derive(Debug, Clone, Copy)]
struct BuildInfo {
    version: &'static str,
    build: &'static str,
}

static BUILD_INFO: OnceLock<BuildInfo> = OnceLock::new();

/// Registers the running executable's workspace version and dev-build identifier.
/// Safe to call more than once; only the first registration takes effect.
pub fn init_build_info(version: &'static str, build: &'static str) {
    BUILD_INFO.get_or_init(|| BuildInfo { version, build });
}

/// Returns the registered release version, or `"0.0.0"` in isolated library unit tests.
#[must_use]
pub fn release_version() -> &'static str {
    BUILD_INFO
        .get()
        .map_or(FALLBACK_VERSION, |info| info.version)
}

/// Returns the registered build identifier (`<version>+dev.<hash>`), or a deterministic fallback
/// in isolated library unit tests.
#[must_use]
pub fn build_id() -> &'static str {
    BUILD_INFO.get().map_or(FALLBACK_BUILD, |info| info.build)
}
