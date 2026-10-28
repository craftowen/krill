//! Store destinations in the existing project record, including paths imported from Orca.

use super::*;

fn project_key(path: &Path) -> String {
    let root = PathBuf::from(krill_proto::orchestration::worktree_key(path));
    normalize_project_path(main_worktree(&root).as_deref().unwrap_or(&root))
}

/// Local Git projects, including those that still use the default destination.
pub fn project_worktree_locations(
    state_dir: &Path,
    current: Option<&str>,
) -> BTreeMap<String, String> {
    let mut locations: BTreeMap<_, _> = load_imported_projects(state_dir)
        .unwrap_or_default()
        .projects
        .into_iter()
        .filter(|p| !p.archived && p.host_id.is_none() && p.kind.as_deref() != Some("folder"))
        .map(|p| {
            (
                project_key(Path::new(&p.path)),
                p.worktree_base_path.unwrap_or_default(),
            )
        })
        .collect();
    if let Some(current) = current {
        let value = locations
            .get(&project_key(Path::new(current)))
            .cloned()
            .unwrap_or_default();
        locations.insert(
            krill_proto::orchestration::worktree_key(Path::new(current)),
            value,
        );
    }
    locations
}

pub fn save_project_worktree_location(state_dir: &Path, repo: &str, raw: &str) -> io::Result<()> {
    let value = krill_proto::worktree_location::normalize(raw)?;
    let key = project_key(Path::new(repo));
    register_project(state_dir, Path::new(&key))?;
    let path = state_dir.join(PROJECTS_FILE);
    let _lock = krill_proto::config_document::lock_toml(&path)?;
    let mut original = String::new();
    fs::File::open(&path)?
        .take(MAX_PROJECTS_BYTES + 1)
        .read_to_string(&mut original)?;
    if u64::try_from(original.len()).unwrap_or(u64::MAX) > MAX_PROJECTS_BYTES {
        return Err(io::Error::other("projects.toml exceeds size limit"));
    }
    let mut document: toml::Value = toml::from_str(&original).map_err(io::Error::other)?;
    let project = document
        .get_mut("projects")
        .and_then(toml::Value::as_array_mut)
        .and_then(|projects| {
            projects.iter_mut().find(|project| {
                project.get("host_id").is_none()
                    && project
                        .get("path")
                        .and_then(toml::Value::as_str)
                        .is_some_and(|p| project_key(Path::new(p)) == key)
            })
        })
        .and_then(toml::Value::as_table_mut)
        .ok_or_else(|| io::Error::other("project is not registered"))?;
    if let Some(value) = value {
        project.insert("worktree_base_path".into(), toml::Value::String(value));
    } else {
        project.remove("worktree_base_path");
    }
    let body = krill_proto::config_document::edit_toml(&original, &document)?;
    write_projects_body(&path, &body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_location_preserves_metadata_and_clears_to_default() {
        let dir =
            std::env::temp_dir().join(format!("krill-project-location-{}", std::process::id()));
        fs::remove_dir_all(&dir).ok();
        let repo = dir.join("repo");
        let state = dir.join("state");
        fs::create_dir_all(repo.join(".git")).unwrap();
        register_project(&state, &repo).unwrap();
        let path = state.join(PROJECTS_FILE);
        let raw = fs::read_to_string(&path).unwrap();
        fs::write(
            &path,
            format!("# keep this comment\n{raw}\nfuture_setting = 'preserve me'\n"),
        )
        .unwrap();
        let key = project_key(&repo);
        save_project_worktree_location(&state, &key, " .worktrees ").unwrap();
        assert_eq!(
            project_worktree_locations(&state, None)
                .get(&key)
                .map(String::as_str),
            Some(".worktrees")
        );
        let saved = fs::read_to_string(&path).unwrap();
        assert!(saved.contains("# keep this comment"));
        assert!(saved.contains("future_setting = 'preserve me'"));
        assert!(save_project_worktree_location(&state, &key, ".git/tasks").is_err());
        assert_eq!(saved, fs::read_to_string(&path).unwrap());
        save_project_worktree_location(&state, &key, "").unwrap();
        assert_eq!(
            project_worktree_locations(&state, None)
                .get(&key)
                .map(String::as_str),
            Some("")
        );
        assert!(
            !fs::read_to_string(&path)
                .unwrap()
                .contains("worktree_base_path")
        );
        fs::remove_dir_all(dir).unwrap();
    }
}
