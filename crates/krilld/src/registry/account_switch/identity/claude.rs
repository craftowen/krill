//! Claude closes JSONL between writes. Its process-keyed session record can supply identity.
//! This optional, version-dependent fallback requires PID and process birth to match exactly;
//! an absent/changed record simply leaves the original running.

use std::fs::File;
use std::io::{self, Read as _};
use std::path::{Path, PathBuf};
#[cfg(unix)]
use std::process::Command;

use krill_proto::usage::ProviderKind;

use super::{ExpectedId, Identity, descendant_pids, process_pids, transcript};

#[cfg(any(windows, test))]
const WINDOWS_TO_UNIX_MS: u64 = 11_644_473_600_000;
#[cfg(any(windows, test))]
const WINDOWS_DRIFT_TOLERANCE_MS: u64 = 60_000;

fn is_claude_process_name(name: &str) -> bool {
    let base = name.rsplit(['/', '\\']).next().unwrap_or(name);
    let stem = base
        .rsplit_once('.')
        .filter(|(_, ext)| ext.eq_ignore_ascii_case("exe"))
        .map_or(base, |(stem, _)| stem);
    stem.eq_ignore_ascii_case("claude")
}

pub(super) fn discover(
    homes: &[PathBuf],
    root: u32,
    foreground: Option<u32>,
    expected_id: ExpectedId<'_>,
) -> io::Result<Option<Identity>> {
    let mut found = CandidateState::default();
    let mut candidates = process_pids(root, foreground);
    for pid in descendant_pids(root, foreground) {
        if !candidates.contains(&pid) {
            candidates.push(pid);
        }
    }
    for pid in candidates {
        if !crate::sys::process_name(pid).is_ok_and(|name| is_claude_process_name(name.trim())) {
            continue;
        }
        discover_pid(homes, pid, expected_id, &mut found)?;
    }
    found.finish()
}

#[cfg(unix)]
fn discover_pid(
    homes: &[PathBuf],
    pid: u32,
    expected_id: ExpectedId<'_>,
    found: &mut CandidateState,
) -> io::Result<()> {
    let Ok(start) = crate::sys::start_us(pid) else {
        return Ok(());
    };
    let output = Command::new("/bin/ps")
        .args(["-p", &pid.to_string(), "-o", "lstart="])
        .env("LC_ALL", "C")
        .env("TZ", "UTC")
        .output()?;
    if !output.status.success() {
        return Ok(());
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
        let Some(bytes) = read_session_record(home, pid)? else {
            continue;
        };
        let mut identity = from_record(&bytes, pid, birth_utc.trim(), home)?;
        if identity.is_none()
            && let Some(local) = birth_local.as_deref().map(str::trim)
            && local != birth_utc.trim()
        {
            identity = from_record(&bytes, pid, local, home)?;
        }
        if let Some(identity) = identity {
            select_candidate(found, identity, expected_id)?;
        }
    }
    if crate::sys::start_us(pid).ok() != Some(start) {
        return Err(io::ErrorKind::WouldBlock.into());
    }
    Ok(())
}

#[cfg(windows)]
fn discover_pid(
    homes: &[PathBuf],
    pid: u32,
    expected_id: ExpectedId<'_>,
    found: &mut CandidateState,
) -> io::Result<()> {
    let Ok(start) = crate::sys::start_us(pid) else {
        return Ok(());
    };
    for home in homes {
        let Some(bytes) = read_session_record(home, pid)? else {
            continue;
        };
        if let Some(identity) = from_windows_record(&bytes, pid, start, home)? {
            select_candidate(found, identity, expected_id)?;
        }
    }
    if crate::sys::start_us(pid).ok() != Some(start) {
        return Err(io::ErrorKind::WouldBlock.into());
    }
    Ok(())
}

#[cfg(not(any(unix, windows)))]
fn discover_pid(
    _homes: &[PathBuf],
    _pid: u32,
    _expected_id: ExpectedId<'_>,
    _found: &mut CandidateState,
) -> io::Result<()> {
    Ok(())
}

fn read_session_record(home: &Path, pid: u32) -> io::Result<Option<Vec<u8>>> {
    let record = home.join("sessions").join(format!("{pid}.json"));
    let Ok(file) = File::open(record) else {
        return Ok(None);
    };
    let mut bytes = Vec::new();
    file.take(64 * 1024).read_to_end(&mut bytes)?;
    Ok(Some(bytes))
}

#[derive(Default)]
struct CandidateState {
    preferred: Option<Identity>,
    fallback: Option<Identity>,
    fallback_ambiguous: bool,
}

impl CandidateState {
    fn finish(self) -> io::Result<Option<Identity>> {
        if self.preferred.is_some() {
            return Ok(self.preferred);
        }
        if self.fallback_ambiguous {
            return Err(io::ErrorKind::InvalidData.into());
        }
        Ok(self.fallback)
    }
}

fn select_candidate(
    found: &mut CandidateState,
    identity: Identity,
    expected_id: ExpectedId<'_>,
) -> io::Result<()> {
    if Some(identity.id.as_str()) == expected_id.expected() {
        if let Some(previous) = &found.preferred {
            if previous.id != identity.id
                || !same_file::is_same_file(&previous.path, &identity.path)?
            {
                return Err(io::ErrorKind::InvalidData.into());
            }
        } else {
            found.preferred = Some(identity);
        }
        return Ok(());
    }
    if matches!(expected_id, ExpectedId::Exact(_)) {
        return Ok(());
    }
    if let Some(previous) = &found.fallback {
        if previous.id != identity.id || !same_file::is_same_file(&previous.path, &identity.path)? {
            if matches!(expected_id, ExpectedId::Any) {
                return Err(io::ErrorKind::InvalidData.into());
            }
            found.fallback_ambiguous = true;
        }
    } else {
        found.fallback = Some(identity);
    }
    Ok(())
}

#[cfg(any(unix, test))]
fn from_record(bytes: &[u8], pid: u32, birth: &str, home: &Path) -> io::Result<Option<Identity>> {
    let Ok(record) = serde_json::from_slice::<serde_json::Value>(bytes) else {
        return Ok(None);
    };
    if record.get("pid").and_then(serde_json::Value::as_u64) != Some(u64::from(pid))
        || record.get("procStart").and_then(serde_json::Value::as_str) != Some(birth)
    {
        return Ok(None);
    }
    verified_transcript_identity(&record, home)
}

#[cfg(any(windows, test))]
fn parse_windows_timestamp_ms(val: &serde_json::Value) -> Option<u64> {
    if let Some(n) = val.as_u64() {
        if n >= 1_000_000_000_000_000_000 {
            return Some(n / 1_000_000);
        }
        if n >= 100_000_000_000_000_000 {
            return (n / 10_000).checked_sub(WINDOWS_TO_UNIX_MS);
        }
        if n >= 10_000_000_000_000_000 {
            return (n / 1_000).checked_sub(WINDOWS_TO_UNIX_MS);
        }
        if n >= 1_000_000_000_000_000 {
            return Some(n / 1_000);
        }
        if n >= 1_000_000_000_000 {
            return Some(n);
        }
        if n >= 1_000_000_000 {
            return n.checked_mul(1000);
        }
        return None;
    }
    let s = val.as_str()?;
    let secs = krill_proto::time::parse_rfc3339_epoch(s)?;
    u64::try_from(secs).ok()?.checked_mul(1000)
}

#[cfg(any(windows, test))]
fn from_windows_record(
    bytes: &[u8],
    pid: u32,
    proc_start_us: u64,
    home: &Path,
) -> io::Result<Option<Identity>> {
    let Ok(record) = serde_json::from_slice::<serde_json::Value>(bytes) else {
        return Ok(None);
    };
    if record.get("pid").and_then(serde_json::Value::as_u64) != Some(u64::from(pid)) {
        return Ok(None);
    }
    let proc_unix_ms = (proc_start_us / 1_000).checked_sub(WINDOWS_TO_UNIX_MS);
    if let Some(proc_ms) = proc_unix_ms {
        if let Some(proc_start) = record.get("procStart").and_then(parse_windows_timestamp_ms)
            && proc_start.abs_diff(proc_ms) > WINDOWS_DRIFT_TOLERANCE_MS
        {
            return Ok(None);
        }
        if let Some(started_at) = record.get("startedAt").and_then(parse_windows_timestamp_ms)
            && started_at.saturating_add(WINDOWS_DRIFT_TOLERANCE_MS) < proc_ms
        {
            return Ok(None);
        }
    }
    verified_transcript_identity(&record, home)
}

fn verified_transcript_identity(
    record: &serde_json::Value,
    home: &Path,
) -> io::Result<Option<Identity>> {
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
    fn claude_process_name_matches_exe_and_case_insensitively() {
        for matched in [
            "claude",
            "Claude",
            "claude.exe",
            "CLAUDE.EXE",
            r"C:\Users\alice\.local\bin\Claude.Exe",
            "/usr/local/bin/claude",
        ] {
            assert!(is_claude_process_name(matched), "{matched}");
        }
        for rejected in ["claude-helper", "clauded", "node.exe", "codex.exe", ""] {
            assert!(!is_claude_process_name(rejected), "{rejected}");
        }
    }

    #[test]
    fn windows_process_record_validates_pid_optional_timestamps_and_transcript() {
        let home = std::env::temp_dir().join(format!("kas-claude-win-{}", std::process::id()));
        std::fs::remove_dir_all(&home).ok();
        std::fs::create_dir_all(home.join("projects/project")).unwrap();
        std::fs::write(
            home.join("projects/project/known.jsonl"),
            r#"{"sessionId":"known"}"#,
        )
        .unwrap();

        let proc_unix_ms = 1_792_038_480_000_u64;
        let proc_start_us = (proc_unix_ms + WINDOWS_TO_UNIX_MS) * 1_000;

        // Valid without procStart or startedAt, or with unrecognized/different procStart format.
        for record in [
            serde_json::json!({"pid": 41, "sessionId": "known"}),
            serde_json::json!({"pid": 41, "procStart": "custom windows format", "sessionId": "known"}),
            serde_json::json!({"pid": 41, "startedAt": proc_unix_ms + 500, "sessionId": "known"}),
            // `/clear` inside the same Claude process can update startedAt hours later.
            serde_json::json!({"pid": 41, "startedAt": proc_unix_ms + 3_600_000, "sessionId": "known"}),
            serde_json::json!({"pid": 41, "procStart": proc_start_us * 10, "sessionId": "known"}),
            serde_json::json!({"pid": 41, "procStart": proc_unix_ms * 1_000 + 250_000, "sessionId": "known"}),
            serde_json::json!({"pid": 41, "procStart": proc_unix_ms * 1_000_000, "sessionId": "known"}),
        ] {
            let bytes = serde_json::to_vec(&record).unwrap();
            assert_eq!(
                from_windows_record(&bytes, 41, proc_start_us, &home)
                    .unwrap()
                    .unwrap()
                    .id,
                "known",
                "{record}"
            );
        }

        // Rejects mismatched PID, stale startedAt (PID reuse), or mismatched procStart timestamp.
        for stale in [
            serde_json::json!({"pid": 42, "sessionId": "known"}),
            serde_json::json!({"pid": 41, "startedAt": proc_unix_ms - 120_000, "sessionId": "known"}),
            serde_json::json!({"pid": 41, "procStart": proc_unix_ms - 120_000, "sessionId": "known"}),
            serde_json::json!({"pid": 41, "procStart": (proc_unix_ms - 120_000) * 1_000, "sessionId": "known"}),
        ] {
            let bytes = serde_json::to_vec(&stale).unwrap();
            assert!(
                from_windows_record(&bytes, 41, proc_start_us, &home)
                    .unwrap()
                    .is_none(),
                "{stale}"
            );
        }

        // Still rejects corrupt/mismatched transcript content.
        std::fs::write(
            home.join("projects/project/known.jsonl"),
            r#"{"sessionId":"other"}"#,
        )
        .unwrap();
        let valid = br#"{"pid":41,"sessionId":"known"}"#;
        assert!(from_windows_record(valid, 41, proc_start_us, &home).is_err());

        std::fs::remove_dir_all(home).unwrap();
    }

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

    #[test]
    fn select_candidate_filters_by_expected_id_before_rejecting_siblings() {
        let home = std::env::temp_dir().join(format!("kas-claude-select-{}", std::process::id()));
        std::fs::remove_dir_all(&home).ok();
        std::fs::create_dir_all(&home).unwrap();
        let a_path = home.join("a.jsonl");
        let b_path = home.join("b.jsonl");
        std::fs::write(&a_path, r#"{"sessionId":"a"}"#).unwrap();
        std::fs::write(&b_path, r#"{"sessionId":"b"}"#).unwrap();

        let mk = |s: &str, p: &PathBuf| Identity {
            id: s.into(),
            path: p.clone(),
            home: home.clone(),
        };
        let mut selected = CandidateState::default();
        select_candidate(&mut selected, mk("a", &a_path), ExpectedId::Exact("b")).unwrap();
        assert!(selected.preferred.is_none() && selected.fallback.is_none());
        select_candidate(&mut selected, mk("b", &b_path), ExpectedId::Exact("b")).unwrap();
        assert_eq!(
            selected.finish().unwrap().as_ref().map(|i| i.id.as_str()),
            Some("b")
        );

        let mut preferred = CandidateState::default();
        select_candidate(
            &mut preferred,
            mk("a", &a_path),
            ExpectedId::Prefer("missing"),
        )
        .unwrap();
        assert_eq!(
            preferred.finish().unwrap().as_ref().map(|i| i.id.as_str()),
            Some("a")
        );

        let mut ambiguous = CandidateState::default();
        select_candidate(&mut ambiguous, mk("a", &a_path), ExpectedId::Any).unwrap();
        assert_eq!(
            select_candidate(&mut ambiguous, mk("b", &b_path), ExpectedId::Any)
                .err()
                .unwrap()
                .kind(),
            io::ErrorKind::InvalidData
        );
        std::fs::remove_dir_all(home).unwrap();
    }
}
