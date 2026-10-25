//! Claude closes JSONL between writes. Its process-keyed session record can supply identity.
//! This optional, version-dependent fallback requires PID and process birth to match exactly;
//! an absent/changed record simply leaves the original running.

use std::fs::File;
use std::io::{self, Read as _};
use std::path::{Path, PathBuf};
use std::process::Command;

use krill_proto::usage::ProviderKind;

use super::{Identity, descendant_pids, process_pids, transcript};

pub(super) fn discover(
    homes: &[PathBuf],
    root: u32,
    foreground: Option<u32>,
) -> io::Result<Option<Identity>> {
    let mut found: Option<Identity> = None;
    let mut candidates = process_pids(root, foreground);
    for pid in descendant_pids(root, foreground) {
        if !candidates.contains(&pid) {
            candidates.push(pid);
        }
    }
    for pid in candidates {
        if crate::sys::process_name(pid).ok().as_deref() != Some("claude") {
            continue;
        }
        let Ok(start) = crate::sys::start_us(pid) else {
            continue;
        };
        let output = Command::new("/bin/ps")
            .args(["-p", &pid.to_string(), "-o", "lstart="])
            .env("LC_ALL", "C")
            .env("TZ", "UTC")
            .output()?;
        if !output.status.success() {
            continue;
        }
        let birth_utc = String::from_utf8_lossy(&output.stdout).into_owned();
        let birth_local = Command::new("/bin/ps")
            .args(["-p", &pid.to_string(), "-o", "lstart="])
            .env("LC_ALL", "C")
            .output()
            .ok()
            .filter(|out| out.status.success())
            .map(|out| String::from_utf8_lossy(&out.stdout).into_owned());
        for home in homes {
            let record = home.join("sessions").join(format!("{pid}.json"));
            let Ok(file) = File::open(record) else {
                continue;
            };
            let mut bytes = Vec::new();
            file.take(64 * 1024).read_to_end(&mut bytes)?;
            let mut identity = from_record(&bytes, pid, birth_utc.trim(), home)?;
            if identity.is_none()
                && let Some(local) = birth_local.as_deref().map(str::trim)
                && local != birth_utc.trim()
            {
                identity = from_record(&bytes, pid, local, home)?;
            }
            let Some(identity) = identity else {
                continue;
            };
            if let Some(previous) = &found {
                if previous.id != identity.id
                    || !same_file::is_same_file(&previous.path, &identity.path)?
                {
                    return Err(io::ErrorKind::InvalidData.into());
                }
            } else {
                found = Some(identity);
            }
        }
        if crate::sys::start_us(pid).ok() != Some(start) {
            return Err(io::ErrorKind::WouldBlock.into());
        }
    }
    Ok(found)
}

fn from_record(bytes: &[u8], pid: u32, birth: &str, home: &Path) -> io::Result<Option<Identity>> {
    let Ok(record) = serde_json::from_slice::<serde_json::Value>(bytes) else {
        return Ok(None);
    };
    if record.get("pid").and_then(serde_json::Value::as_u64) != Some(u64::from(pid))
        || record.get("procStart").and_then(serde_json::Value::as_str) != Some(birth)
    {
        return Ok(None);
    }
    let Some(id) = record
        .get("sessionId")
        .and_then(serde_json::Value::as_str)
        .filter(|id| crate::agent::control::valid_session_id(id))
    else {
        return Ok(None);
    };
    let path = transcript::find_transcript(&home.join("projects"), ProviderKind::Claude, id)?;
    Ok(path.map(|path| Identity {
        id: id.to_owned(),
        path,
        home: home.to_path_buf(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn process_record_requires_matching_pid_birth_and_verified_conversation() {
        let home = std::env::temp_dir().join(format!("kas-claude-process-{}", std::process::id()));
        std::fs::create_dir_all(home.join("projects/project")).unwrap();
        std::fs::write(
            home.join("projects/project/known.jsonl"),
            r#"{"sessionId":"known"}"#,
        )
        .unwrap();
        let record = br#"{"pid":41,"procStart":"Sat Oct 24 13:54:54 2026","sessionId":"known"}"#;
        assert_eq!(
            from_record(record, 41, "Sat Oct 24 13:54:54 2026", &home)
                .unwrap()
                .unwrap()
                .id,
            "known"
        );
        assert!(
            from_record(record, 42, "Sat Oct 24 13:54:54 2026", &home)
                .unwrap()
                .is_none()
        );
        assert!(
            from_record(record, 41, "different process birth", &home)
                .unwrap()
                .is_none()
        );
        std::fs::write(
            home.join("projects/project/known.jsonl"),
            r#"{"sessionId":"other"}"#,
        )
        .unwrap();
        assert!(from_record(record, 41, "Sat Oct 24 13:54:54 2026", &home).is_err());
        std::fs::remove_dir_all(home).unwrap();
    }
}
