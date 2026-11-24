#![forbid(unsafe_code)]
//! Workspace release version and shared dev-build identity (`<version>+dev.<hash>`).
//!
//! Keeping version and source-hash metadata in a tiny leaf crate ensures that bumping
//! `[workspace.package].version` (or editing daemon sources) never invalidates compile caches for
//! internal library crates (`krill-proto`, `krill-term`, `krill-hosts`, `krill-log`, `krill-ui`,
//! `krill-app-core`).

/// Workspace semantic version (`Cargo.toml` `[workspace.package].version`).
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Hello·업그레이드에서 공유하는 빌드 식별자. 버전이 같아도 소스가 바뀌면 데몬을 교체한다.
pub const BUILD: &str = env!("KRILL_DEV_BUILD");

#[cfg(test)]
mod tests {
    #[test]
    fn dev_build_identifier_contains_the_source_hash() {
        let (version, hash) = super::BUILD.split_once("+dev.").unwrap_or(("", ""));
        assert_eq!(version, super::VERSION);
        assert_eq!(hash.len(), 16);
        assert!(hash.bytes().all(|byte| byte.is_ascii_hexdigit()));
    }
}
