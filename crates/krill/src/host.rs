//! `krill host status [<host>] [--json]` — 원격 SSH 호스트 연결·인증 상태 및 Linux systemd linger 점검.

use std::ffi::OsString;
use std::fmt::Write as _;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use krill_hosts::managed::{LingerStatus, RemoteInfo, RemoteVersion, query_remote_info};
use krill_hosts::ssh::{SshOptions, is_ssh_auth_failure, resolve_host_options};
use serde::{Deserialize, Serialize};

const USAGE: &str = "usage: krill host status [<host>] [--json] | krill host ls [--json]";
const HOSTS_FILE: &str = "hosts.toml";

#[derive(Debug, Default, Deserialize)]
struct HostsFile {
    #[serde(default)]
    hosts: Vec<HostEntry>,
}

#[derive(Debug, Clone, Deserialize)]
struct HostEntry {
    #[serde(default)]
    id: String,
    #[serde(default)]
    label: String,
    destination: String,
    #[serde(default)]
    port: Option<u16>,
    #[serde(default)]
    identity_file: Option<String>,
    #[serde(default)]
    remote_socket: Option<String>,
}

impl HostEntry {
    fn display_name(&self) -> &str {
        let label = self.label.trim();
        if label.is_empty() {
            &self.destination
        } else {
            label
        }
    }

    fn to_ssh_options(&self) -> SshOptions {
        SshOptions {
            destination: self.destination.clone(),
            port: self.port,
            identity_file: self.identity_file.clone().filter(|s| !s.trim().is_empty()),
            remote_socket: self.remote_socket.clone().filter(|s| !s.trim().is_empty()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct HostStatusReport {
    pub id: String,
    pub label: String,
    pub destination: String,
    pub status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub platform: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    pub linger: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub linger_user: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub linger_warning: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub linger_fix_command: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

pub(crate) fn report_from_probe(
    id: String,
    label: String,
    destination: String,
    result: io::Result<RemoteInfo>,
) -> HostStatusReport {
    match result {
        Ok(info) => {
            let version = match &info.version {
                RemoteVersion::Known(v) => Some(v.clone()),
                RemoteVersion::Unmanaged => Some("unmanaged".to_owned()),
                RemoteVersion::Missing => Some("missing".to_owned()),
            };
            let (linger, linger_user, linger_warning, linger_fix_command) = match &info.linger {
                LingerStatus::Enabled => ("enabled", None, None, None),
                LingerStatus::Disabled { user } => {
                    let fix = info.linger.fix_command();
                    let warn = Some(
                        "Linux systemd linger is disabled; background sessions may stop when the last SSH login closes."
                            .to_owned(),
                    );
                    ("disabled", Some(user.clone()), warn, fix)
                }
                LingerStatus::NotApplicable => ("not_applicable", None, None, None),
            };
            HostStatusReport {
                id,
                label,
                destination,
                status: "connected",
                platform: Some(info.platform.describe()),
                version,
                linger,
                linger_user,
                linger_warning,
                linger_fix_command,
                error: None,
            }
        }
        Err(err) => {
            let msg = err.to_string();
            let status = if is_ssh_auth_failure(&msg) {
                "auth_failed"
            } else {
                "disconnected"
            };
            HostStatusReport {
                id,
                label,
                destination,
                status,
                platform: None,
                version: None,
                linger: "unknown",
                linger_user: None,
                linger_warning: None,
                linger_fix_command: None,
                error: Some(msg),
            }
        }
    }
}

pub(crate) fn format_text_report(reports: &[HostStatusReport]) -> String {
    if reports.is_empty() {
        return "No SSH hosts configured.\n".to_owned();
    }
    let mut out = String::new();
    for r in reports {
        let state = match r.status {
            "connected" => "connected",
            "auth_failed" => "needs login (auth failed)",
            _ => "disconnected",
        };
        let mut line = format!("{} ({})  {state}", r.label, r.destination);
        if let Some(platform) = &r.platform {
            write!(line, "  [{platform}]").ok();
        }
        if let Some(version) = &r.version {
            write!(line, "  krill={version}").ok();
        }
        out.push_str(&line);
        out.push('\n');
        if let Some(err) = &r.error {
            writeln!(out, "  error: {err}").ok();
        }
        if let Some(warn) = &r.linger_warning {
            writeln!(out, "  warning: {warn}").ok();
            if let Some(cmd) = &r.linger_fix_command {
                writeln!(out, "  fix: {cmd}").ok();
            }
        }
    }
    out
}

fn hosts_config_path(state_dir: Option<&Path>) -> PathBuf {
    if let Some(dir) = state_dir {
        dir.join(HOSTS_FILE)
    } else if let Some(dir) = krill_proto::daemon::state_dir() {
        dir.join(HOSTS_FILE)
    } else {
        let home = krill_proto::os::home_dir().unwrap_or_else(std::env::temp_dir);
        krill_proto::user_dirs::config_dir(&home).join(HOSTS_FILE)
    }
}

fn load_configured_hosts(state_dir: Option<&Path>) -> Vec<HostEntry> {
    let path = hosts_config_path(state_dir);
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    toml::from_str::<HostsFile>(&text)
        .map(|f| f.hosts)
        .unwrap_or_default()
}

pub(crate) fn run(args: &[OsString]) -> ExitCode {
    if matches!(
        args.first().and_then(|s| s.to_str()),
        Some("-h" | "--help" | "help")
    ) {
        writeln!(io::stdout(), "{USAGE}").ok();
        return ExitCode::SUCCESS;
    }
    let mut json = false;
    let mut target: Option<String> = None;
    let rest = match args.first().and_then(|s| s.to_str()) {
        Some("status" | "ls" | "list") => &args[1..],
        None => &[],
        Some(other) if other.starts_with('-') => args,
        Some(other) => {
            writeln!(io::stderr(), "unknown host subcommand: {other}\n{USAGE}").ok();
            return ExitCode::from(2);
        }
    };
    for arg in rest {
        match arg.to_str() {
            Some("-h" | "--help") => {
                writeln!(io::stdout(), "{USAGE}").ok();
                return ExitCode::SUCCESS;
            }
            Some("--json") => json = true,
            Some(flag) if flag.starts_with('-') => {
                writeln!(io::stderr(), "unknown flag: {flag}\n{USAGE}").ok();
                return ExitCode::from(2);
            }
            Some(val) if target.is_none() => target = Some(val.to_owned()),
            _ => {
                writeln!(io::stderr(), "{USAGE}").ok();
                return ExitCode::from(2);
            }
        }
    }

    let state = krill_proto::daemon::state_dir();
    let configured = load_configured_hosts(state.as_deref());
    let targets: Vec<(String, String, SshOptions)> = if let Some(name) = target {
        if let Some(found) = configured
            .iter()
            .find(|h| h.id == name || h.label == name || h.destination == name)
        {
            vec![(
                found.id.clone(),
                found.display_name().to_owned(),
                found.to_ssh_options(),
            )]
        } else {
            match resolve_host_options(&name, state.as_deref()) {
                Ok(opts) => vec![(name.clone(), name, opts)],
                Err(err) => {
                    writeln!(io::stderr(), "krill host status: {err}").ok();
                    return ExitCode::from(2);
                }
            }
        }
    } else {
        configured
            .into_iter()
            .map(|h| {
                let id = h.id.clone();
                let label = h.display_name().to_owned();
                let opts = h.to_ssh_options();
                (id, label, opts)
            })
            .collect()
    };

    let reports: Vec<HostStatusReport> = targets
        .into_iter()
        .map(|(id, label, opts)| {
            let dest = opts.destination.clone();
            let result = query_remote_info(&opts);
            report_from_probe(id, label, dest, result)
        })
        .collect();

    if json {
        match serde_json::to_string_pretty(&reports) {
            Ok(rendered) => {
                writeln!(io::stdout(), "{rendered}").ok();
            }
            Err(err) => {
                writeln!(io::stderr(), "krill host status: {err}").ok();
                return ExitCode::FAILURE;
            }
        }
    } else {
        write!(io::stdout(), "{}", format_text_report(&reports)).ok();
    }

    if reports.iter().any(|r| r.status != "connected") {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use krill_hosts::managed::{RemoteArch, RemoteOs, RemotePlatform};

    #[test]
    fn host_status_reports_linux_linger_warning_with_exact_fix_command() {
        let info = RemoteInfo {
            platform: RemotePlatform {
                os: RemoteOs::Linux,
                arch: RemoteArch::X86_64,
                kernel: "Linux".into(),
            },
            home: "/home/ubuntu".into(),
            version: RemoteVersion::Unmanaged,
            socket_bundle: None,
            linger: LingerStatus::Disabled {
                user: "ubuntu".into(),
            },
        };
        let report = report_from_probe(
            "ssh-devbox".into(),
            "devbox".into(),
            "ubuntu@devbox".into(),
            Ok(info),
        );
        assert_eq!(report.status, "connected");
        assert_eq!(report.linger, "disabled");
        assert_eq!(
            report.linger_fix_command.as_deref(),
            Some("sudo loginctl enable-linger ubuntu")
        );

        let text = format_text_report(std::slice::from_ref(&report));
        assert!(text.contains("warning:"), "{text}");
        assert!(
            text.contains("sudo loginctl enable-linger ubuntu"),
            "{text}"
        );

        let json = serde_json::to_string(&report).unwrap();
        assert!(json.contains("\"linger\":\"disabled\""), "{json}");
        assert!(
            json.contains("sudo loginctl enable-linger ubuntu"),
            "{json}"
        );
    }

    #[test]
    fn host_status_ignores_linger_when_enabled_or_macos_and_classifies_auth_failure() {
        let mac = report_from_probe(
            "ssh-mac".into(),
            "studio".into(),
            "owen@studio".into(),
            Ok(RemoteInfo {
                platform: RemotePlatform {
                    os: RemoteOs::Macos,
                    arch: RemoteArch::Arm64,
                    kernel: "Darwin".into(),
                },
                home: "/Users/owen".into(),
                version: RemoteVersion::Known("0.3.24".into()),
                socket_bundle: None,
                linger: LingerStatus::NotApplicable,
            }),
        );
        assert_eq!(mac.linger, "not_applicable");
        assert!(mac.linger_warning.is_none());
        assert!(mac.linger_fix_command.is_none());

        let auth_err = report_from_probe(
            "ssh-locked".into(),
            "locked".into(),
            "alice@locked".into(),
            Err(io::Error::other(
                "alice@locked: Permission denied (publickey).",
            )),
        );
        assert_eq!(auth_err.status, "auth_failed");
        let text = format_text_report(&[mac, auth_err]);
        assert!(!text.contains("warning:"), "{text}");
        assert!(text.contains("needs login (auth failed)"), "{text}");
    }

    #[test]
    fn host_help_exits_zero() {
        assert_eq!(run(&[OsString::from("--help")]), ExitCode::SUCCESS);
        assert_eq!(
            run(&[OsString::from("status"), OsString::from("--help")]),
            ExitCode::SUCCESS
        );
    }
}
