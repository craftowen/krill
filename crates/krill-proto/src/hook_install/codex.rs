//! MK-012: ask Codex for hashes, persist trust through its atomic writer with expectedVersion.
//! No hooks.json edits, auth copies, overlay homes, or bypass-hook-trust flags.

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::io::{self, BufRead as _, BufReader, Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::Duration;

use crate::agent::hooks;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::CodexTrust;
use super::store::{self, error};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Record {
    pub path: PathBuf,
    pub command: String,
    pub added: BTreeMap<String, Value>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub replaced: BTreeMap<String, Value>,
}

pub struct Plan {
    rpc: Rpc,
    pub record: Record,
    pub original: Option<String>,
    pub state: Value,
    version: String,
}

impl Plan {
    pub fn install(
        home: &Path,
        command: &str,
        search: Option<&OsStr>,
        trust: CodexTrust,
    ) -> io::Result<Self> {
        let path = home.join("config.toml");
        let original = store::read(&path)?;
        let mut rpc = Rpc::start(home, command, search)?;
        let version = rpc.version(&path)?;
        // Match the bytes to the versioned snapshot before computing the patch.
        if store::read(&path)? != original {
            return Err(error("Codex configuration changed during discovery"));
        }
        let mut state = state(original.as_deref())?;
        let list = rpc.call("hooks/list", &json!({"cwds": [std::env::current_dir()?]}))?;
        let mut found = BTreeMap::new();
        for layer in list
            .get("data")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            for hook in layer
                .get("hooks")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                if hook.get("command").and_then(Value::as_str) != Some(command) {
                    continue;
                }
                let key = hook
                    .get("key")
                    .and_then(Value::as_str)
                    .ok_or_else(|| error("missing Codex hook key"))?;
                if !key.starts_with("/<session-flags>/config.toml:") {
                    continue;
                }
                let hash = hook
                    .get("currentHash")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                    .ok_or_else(|| error("missing Codex currentHash"))?;
                found.insert(key.to_owned(), json!({"trusted_hash": hash}));
            }
        }
        if found.len() != hooks::Agent::Codex.events().len() {
            return Err(error("Codex does not support the requested session hooks"));
        }
        let mut added = BTreeMap::new();
        let mut replaced = BTreeMap::new();
        let object = state
            .as_object_mut()
            .ok_or_else(|| error("hooks.state must be a table"))?;
        for (key, value) in found {
            match object.get(&key) {
                Some(old) if old != &value => {
                    if trust == CodexTrust::Preserve {
                        return Err(io::Error::new(
                            io::ErrorKind::AlreadyExists,
                            format!("existing Codex trust conflicts: {key}"),
                        ));
                    }
                    replaced.insert(key.clone(), old.clone());
                    object.insert(key.clone(), value.clone());
                    added.insert(key, value);
                }
                Some(_) => {} // Existing trust belongs to the user, not this receipt.
                None => {
                    object.insert(key.clone(), value.clone());
                    added.insert(key, value);
                }
            }
        }
        Ok(Self {
            rpc,
            record: Record {
                path,
                command: command.to_owned(),
                added,
                replaced,
            },
            original,
            state,
            version,
        })
    }

    pub fn uninstall(record: &Record, search: Option<&OsStr>) -> io::Result<Self> {
        let home = record
            .path
            .parent()
            .ok_or_else(|| error("missing Codex home"))?;
        let original = store::read(&record.path)?;
        let mut rpc = Rpc::start(home, &record.command, search)?;
        let version = rpc.version(&record.path)?;
        if store::read(&record.path)? != original {
            return Err(error("Codex configuration changed during discovery"));
        }
        let mut state = state(original.as_deref())?;
        let object = state
            .as_object_mut()
            .ok_or_else(|| error("hooks.state must be a table"))?;
        for (key, owned) in &record.added {
            if object.get(key) == Some(owned) {
                if let Some(previous) = record.replaced.get(key) {
                    object.insert(key.clone(), previous.clone());
                } else {
                    object.remove(key);
                }
            } else if object.contains_key(key) {
                eprintln!("preserved modified Codex trust: {key}");
            }
        }
        Ok(Self {
            rpc,
            record: record.clone(),
            original,
            state,
            version,
        })
    }

    pub fn apply(&mut self) -> io::Result<()> {
        if self.state == state(self.original.as_deref())? {
            return Ok(());
        }
        if store::read(&self.record.path)? != self.original {
            return Err(error("Codex configuration changed; rerun to review"));
        }
        store::directory(
            self.record
                .path
                .parent()
                .ok_or_else(|| error("missing config parent"))?,
        )?;
        if let Some(text) = &self.original {
            store::backup(&self.record.path, text)?;
        }
        self.rpc.call("config/batchWrite", &json!({
            "edits": [{"keyPath": "hooks.state", "value": self.state, "mergeStrategy": "replace"}],
            "filePath": self.record.path, "expectedVersion": self.version, "reloadUserConfig": true
        }))?;
        Ok(())
    }

    pub fn diff(&self, writer: &mut impl io::Write) -> io::Result<()> {
        writeln!(
            writer,
            "--- {}: hooks.state\n+++ {}: hooks.state",
            self.record.path.display(),
            self.record.path.display()
        )?;
        writeln!(
            writer,
            "-{}\n+{}",
            state(self.original.as_deref())?,
            self.state
        )?;
        writeln!(
            writer,
            "session args: {}",
            hooks::codex_args(&self.record.command)
                .iter()
                .map(|s| hooks::shell_quote(s))
                .collect::<Vec<_>>()
                .join(" ")
        )?;
        Ok(())
    }
}

fn state(text: Option<&str>) -> io::Result<Value> {
    let value: toml::Value = toml::from_str(text.unwrap_or("")).map_err(io::Error::other)?;
    let value = serde_json::to_value(value)?;
    let state = value.pointer("/hooks/state").cloned().unwrap_or(json!({}));
    if !state.is_object() {
        return Err(error("hooks.state must be a table"));
    }
    Ok(state)
}

struct Rpc {
    child: Child,
    send: Sender<Value>,
    receive: Receiver<io::Result<Value>>,
    id: u64,
}

impl Drop for Rpc {
    fn drop(&mut self) {
        self.child.kill().ok();
        self.child.wait().ok();
    }
}

impl Rpc {
    fn start(home: &Path, command: &str, search: Option<&OsStr>) -> io::Result<Self> {
        let mut codex = Command::new("codex");
        if let Some(search) = search {
            codex.env("PATH", search);
        }
        let mut child = codex
            .arg("app-server")
            .args(hooks::codex_args(command))
            .env("CODEX_HOME", home)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| {
                if e.kind() == io::ErrorKind::NotFound {
                    io::Error::new(e.kind(), "codex executable not found in PATH")
                } else {
                    e
                }
            })?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| error("Codex stdin unavailable"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| error("Codex stdout unavailable"))?;
        let (send, requests) = mpsc::channel::<Value>();
        let (reply, receive) = mpsc::channel();
        std::thread::spawn(move || {
            let mut stdin = stdin;
            for request in requests {
                if writeln!(stdin, "{request}").is_err() {
                    break;
                }
            }
        });
        std::thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            loop {
                let mut line = Vec::new();
                let result = reader
                    .by_ref()
                    .take(4 * 1024 * 1024)
                    .read_until(b'\n', &mut line);
                let value = match result {
                    Ok(0) => break,
                    Ok(_) if !line.ends_with(b"\n") => {
                        Err(error("Codex RPC response exceeds limit"))
                    }
                    Ok(_) => serde_json::from_slice(&line).map_err(io::Error::other),
                    Err(e) => Err(e),
                };
                let failed = value.is_err();
                if reply.send(value).is_err() || failed {
                    break;
                }
            }
        });
        let mut rpc = Self {
            child,
            send,
            receive,
            id: 0,
        };
        rpc.call("initialize", &json!({"clientInfo": {"name": "krill", "version": env!("CARGO_PKG_VERSION")}, "capabilities": {"experimentalApi": true}}))?;
        rpc.send
            .send(json!({"method": "initialized"}))
            .map_err(io::Error::other)?;
        Ok(rpc)
    }

    fn call(&mut self, method: &str, params: &Value) -> io::Result<Value> {
        let id = self.id;
        self.id = self.id.saturating_add(1);
        self.send
            .send(json!({"id": id, "method": method, "params": params}))
            .map_err(io::Error::other)?;
        // Correctness wait, not a latency budget: the Codex app-server is a fresh
        // process (Python startup under load in tests), so allow 20s before giving up.
        let deadline = std::time::Instant::now()
            .checked_add(Duration::from_secs(20))
            .ok_or_else(|| error("Codex RPC deadline overflow"))?;
        loop {
            let message = self
                .receive
                .recv_timeout(deadline.saturating_duration_since(std::time::Instant::now()))
                .map_err(io::Error::other)??;
            if message.get("id") != Some(&json!(id)) {
                continue;
            }
            if let Some(e) = message.get("error") {
                return Err(error(format!("Codex {method}: {e}")));
            }
            return message
                .get("result")
                .cloned()
                .ok_or_else(|| error("missing Codex RPC result"));
        }
    }

    fn version(&mut self, path: &Path) -> io::Result<String> {
        let read = self.call("config/read", &json!({"includeLayers": true}))?;
        for layer in read
            .get("layers")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            if layer.pointer("/name/type").and_then(Value::as_str) == Some("user")
                && layer
                    .pointer("/name/file")
                    .and_then(Value::as_str)
                    .is_some_and(|s| Path::new(s) == path)
            {
                return layer
                    .get("version")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                    .ok_or_else(|| error("missing Codex config version"));
            }
        }
        Err(error(
            "Codex did not return a version for the user config layer",
        ))
    }
}
