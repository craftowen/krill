//! Orca's data-account registry is separate from profile settings.
//! Reference: Orca src/main/managed-data-accounts/service.ts,
//! Copyright (c) 2026 Lovecast Inc., MIT License.

use super::{ImportOptions, Json, PlannedOrcaAccount, ProviderKind, fs, slugify};

pub(super) fn opencode(opts: &ImportOptions) -> Vec<PlannedOrcaAccount> {
    let root = opts.orca_dir.join("managed-data-accounts/opencode");
    if fs::metadata(root.join("accounts.json")).is_ok_and(|m| m.len() > 512 * 1024) {
        return Vec::new();
    }
    let Ok(raw) = fs::read_to_string(root.join("accounts.json")) else {
        return Vec::new();
    };
    parse(&root, &raw)
}

fn parse(root: &std::path::Path, raw: &str) -> Vec<PlannedOrcaAccount> {
    if raw.len() > 512 * 1024 {
        return Vec::new();
    }
    let Ok(value) = serde_json::from_str::<Json>(raw) else {
        return Vec::new();
    };
    let active = value.get("activeAccountId").and_then(Json::as_str);
    value
        .get("accounts")
        .and_then(Json::as_array)
        .into_iter()
        .flatten()
        .take(64)
        .filter_map(|row| {
            let id = row.get("id")?.as_str()?;
            if id.is_empty()
                || id.len() > 128
                || !id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
            {
                return None;
            }
            let label = row.get("label")?.as_str()?.trim();
            if label.is_empty() || label.len() > 512 {
                return None;
            }
            let profile_dir = root.join(id.to_ascii_lowercase());
            // Never create an empty replacement for an absent Orca credential directory.
            if !profile_dir.join("data/opencode/auth.json").is_file() {
                return None;
            }
            Some(PlannedOrcaAccount {
                provider: ProviderKind::OpenCode,
                planned_id: format!("opencode-{}", slugify(label)),
                raw_id: id.into(),
                label: label.into(),
                email: None,
                org_or_workspace: None,
                org_uuid: None,
                codex_account_id: None,
                dir_str: profile_dir.display().to_string(),
                profile_dir,
                active: active == Some(id),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    proptest::proptest! {
        #[test]
        fn arbitrary_data_registry_documents_are_bounded(raw in ".{0,4096}") {
            let accounts = parse(std::path::Path::new("/nonexistent/krill-provider-parser-fixture"), &raw);
            proptest::prop_assert!(accounts.len() <= 64);
        }
    }
    #[test]
    fn data_accounts_are_discovered_outside_profile_settings() {
        let root = std::env::temp_dir().join(format!("krill-orca-data-{}", std::process::id()));
        fs::remove_dir_all(&root).ok();
        let base = root.join("managed-data-accounts/opencode");
        fs::create_dir_all(base.join("fixture-id/data/opencode")).unwrap();
        fs::write(base.join("fixture-id/data/opencode/auth.json"), "{}").unwrap();
        fs::write(base.join("accounts.json"), r#"{"accounts":[{"id":"fixture-id","label":"Go account"},{"id":"missing","label":"Missing"},{"id":"../escape","label":"Unsafe"}],"activeAccountId":"fixture-id"}"#).unwrap();
        let opts = ImportOptions {
            home: root.clone(),
            state_dir: root.join("state"),
            orca_dir: root.clone(),
            dry_run: true,
            domains: super::super::ImportDomain::ALL.to_vec(),
        };
        let found = opencode(&opts);
        assert_eq!(found.len(), 1);
        assert!(found[0].active);
        assert_eq!(found[0].label, "Go account");
        fs::remove_dir_all(root).unwrap();
    }
}
