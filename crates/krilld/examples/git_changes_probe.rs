//! Reproducible MK-053 latency/footprint probe in an isolated temporary repository.
//! `cargo run -p krilld --example git_changes_probe --locked`

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use krilld::git::{
    GitQueue,
    watch::{Visibility, Watch},
};

#[cfg(not(windows))]
#[global_allocator]
static ALLOCATOR: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

struct Repo(PathBuf);
impl Drop for Repo {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).ok();
    }
}

fn git(repo: &Path, args: &[&str]) -> Result<(), Box<dyn std::error::Error>> {
    let out = Command::new("git")
        .args(["-c", "user.name=probe", "-c", "user.email=probe@localhost"])
        .args(args)
        .current_dir(repo)
        .output()?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).into_owned().into());
    }
    Ok(())
}

fn footprint() -> Result<u64, Box<dyn std::error::Error>> {
    let out = Command::new("/usr/bin/footprint")
        .args(["-f", "bytes", "-p"])
        .arg(std::process::id().to_string())
        .output()?;
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    text.lines()
        .find_map(|line| {
            line.trim()
                .strip_prefix("phys_footprint:")
                .and_then(|s| s.split_whitespace().next())
                .and_then(|s| s.parse().ok())
        })
        .ok_or_else(|| format!("footprint missing: {text}").into())
}

fn wait(watch: &mut Watch, count: usize) -> Result<Duration, Box<dyn std::error::Error>> {
    let start = Instant::now();
    loop {
        if watch.poll()
            && let Some(result) = watch.snapshot()
        {
            let c = result.as_ref().map_err(Clone::clone)?;
            if c.files.len() == count {
                return Ok(start.elapsed());
            }
        }
        if start.elapsed() > Duration::from_secs(4) {
            return Err("update exceeded four seconds".into());
        }
        thread::sleep(Duration::from_millis(20));
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    krilld::alloc::return_freed_pages().map_err(|e| format!("allocator: {e}"))?;
    let repo = Repo(std::env::temp_dir().join(format!("mk053-probe-{}", std::process::id())));
    fs::create_dir_all(repo.0.join("files"))?;
    git(&repo.0, &["init", "-q"])?;
    for n in 0..36_000 {
        fs::write(repo.0.join(format!("files/f{n:05}")), "one\n")?;
    }
    git(&repo.0, &["add", "."])?;
    git(&repo.0, &["commit", "-qm", "initial"])?;
    let q = Arc::new(GitQueue::new()?);
    let baseline = footprint()?;
    let mut watch = Watch::new(q, repo.0.clone());
    watch.set_visibility(Visibility::Visible)?;
    wait(&mut watch, 0)?;
    fs::write(repo.0.join("files/f00000"), "one\ntwo\n")?;
    let saved = wait(&mut watch, 1)?;
    for n in 0..1_100 {
        fs::write(repo.0.join(format!("files/f{n:05}")), "one\ntwo\nthree\n")?;
    }
    let capped = wait(&mut watch, 1_000)?;
    let limited = watch
        .snapshot()
        .and_then(|r| r.as_ref().ok())
        .is_some_and(|c| c.limited);
    let resident = footprint()?;
    watch.set_visibility(Visibility::Hidden)?;
    let hidden = footprint()?;
    println!("tracked_files=36000 changed_files=1100 returned_files=1000 limited={limited}");
    println!(
        "save_ms={} cap_ms={}",
        saved.as_millis(),
        capped.as_millis()
    );
    println!(
        "baseline_bytes={baseline} visible_bytes={resident} delta_bytes={} hidden_bytes={hidden}",
        resident.saturating_sub(baseline)
    );
    if !limited || resident.saturating_sub(baseline) >= 10_000_000 {
        return Err("probe exceeded its bound".into());
    }
    Ok(())
}
