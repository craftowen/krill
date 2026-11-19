#![forbid(unsafe_code)]
//! `krill-link` CLI 진입점.

use std::env;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use krill_link::{
    ConnectOptions, DEFAULT_INVITE_TTL, Error, LinkConfig, RelaySetting, ServeOptions,
    create_invite_command, host_status, load_devices, pair_with_server, resolve_state_dir,
    revoke_device_command, start_server, start_tunnel, status_report,
};

#[tokio::main]
async fn main() -> ExitCode {
    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            writeln!(io::stderr(), "Error: {e}").ok();
            ExitCode::FAILURE
        }
    }
}

async fn run() -> Result<(), String> {
    let raw_args: Vec<String> = env::args().skip(1).collect();
    let (state_dir_override, args) = extract_state_dir(&raw_args)?;
    let Some(cmd) = args.first().map(String::as_str) else {
        return Err(usage());
    };
    let rest = &args[1..];
    let state_dir = resolve_state_dir(state_dir_override.as_deref()).map_err(|e| e.to_string())?;

    match cmd {
        "serve" => cmd_serve(&state_dir, rest).await.map_err(|e| e.to_string()),
        "enable" => tokio::task::spawn_blocking(move || cmd_enable(&state_dir))
            .await
            .map_err(|e| e.to_string())?
            .map_err(|e| e.to_string()),
        "disable" => tokio::task::spawn_blocking(move || cmd_disable(&state_dir))
            .await
            .map_err(|e| e.to_string())?
            .map_err(|e| e.to_string()),
        "invite" => cmd_invite(&state_dir, rest)
            .await
            .map_err(|e| e.to_string()),
        "pair" => cmd_pair(&state_dir, rest).await.map_err(|e| e.to_string()),
        "devices" => cmd_devices(&state_dir, rest).map_err(|e| e.to_string()),
        "revoke" => cmd_revoke(&state_dir, rest)
            .await
            .map_err(|e| e.to_string()),
        "connect" => cmd_connect(&state_dir, rest)
            .await
            .map_err(|e| e.to_string()),
        "status" => cmd_status(&state_dir, rest).map_err(|e| e.to_string()),
        "config" => cmd_config(&state_dir, rest).map_err(|e| e.to_string()),
        "-h" | "--help" | "help" => {
            writeln!(io::stdout(), "{}", usage()).ok();
            Ok(())
        }
        _ => Err(usage()),
    }
}

fn usage() -> String {
    [
        "Usage:",
        "  krill-link serve [--socket <krilld.sock>] [--state-dir <dir>]",
        "  krill-link enable | disable",
        "  krill-link invite [--ttl-secs <secs>] [--no-qr] [--json]",
        "  krill-link pair <krill://pair?...> --name <device name> [--json]",
        "  krill-link devices [--json]",
        "  krill-link revoke <device id or name> [--json]",
        "  krill-link connect <host id>",
        "  krill-link status [--host <id>] [--json]",
        "  krill-link config [--relay <n0|off|https://...>] [--server-name <name>] [--bind-port <port>] [--resume-grace-ms <ms>] [--replay-buffer-bytes <bytes>] [--json]",
    ]
    .join("\n")
}

fn extract_state_dir(args: &[String]) -> Result<(Option<PathBuf>, Vec<String>), String> {
    let mut state_dir = None;
    let mut out = Vec::with_capacity(args.len());
    let mut i = 0;
    while i < args.len() {
        if args[i] == "--state-dir" {
            i += 1;
            let val = args
                .get(i)
                .ok_or_else(|| "--state-dir requires a path".to_owned())?;
            state_dir = Some(PathBuf::from(val));
        } else {
            out.push(args[i].clone());
        }
        i += 1;
    }
    Ok((state_dir, out))
}

async fn wait_for_shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        if let Ok(mut sigterm) = signal(SignalKind::terminate()) {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {}
                _ = sigterm.recv() => {}
            }
            return;
        }
    }
    tokio::signal::ctrl_c().await.ok();
}

async fn cmd_serve(state_dir: &Path, args: &[String]) -> Result<(), Error> {
    let mut socket_override = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--socket" => {
                i += 1;
                let s = args.get(i).ok_or_else(|| {
                    Error::InvalidConfig("serve --socket requires a path".to_owned())
                })?;
                socket_override = Some(PathBuf::from(s));
            }
            other => {
                return Err(Error::InvalidConfig(format!(
                    "unknown serve argument: {other}"
                )));
            }
        }
        i += 1;
    }

    #[cfg(target_os = "macos")]
    let agent = krill_link::launchd::lifecycle::ManagedAgent::from_env(state_dir);
    #[cfg(target_os = "macos")]
    if let Some(agent) = agent.as_ref()
        && !agent.prepare_start().await.map_err(Error::Io)?
    {
        return Ok(());
    }

    let mut opts = ServeOptions::new(state_dir.to_path_buf());
    opts.krilld_socket = socket_override;
    let mut handle = start_server(opts).await?;
    writeln!(
        io::stdout(),
        "Remote link server listening: node_id={}",
        handle.node_id()
    )
    .ok();
    io::stdout().flush().ok();

    let result: Result<(), io::Error> = tokio::select! {
        () = wait_for_shutdown_signal() => Ok(()),
        () = handle.stopped() => Ok(()),
        result = async {
            #[cfg(target_os = "macos")]
            if let Some(agent) = agent {
                return agent.wait_for_removal().await;
            }
            std::future::pending::<Result<(), io::Error>>().await
        } => result,
    };
    handle.shutdown().await;
    result.map_err(Error::Io)
}

fn cmd_enable(state_dir: &Path) -> Result<(), Error> {
    let exe = krill_proto::daemon::sibling_exe("krill-link").map_err(Error::Io)?;
    krill_link::service::enable_server(&exe, state_dir)?;
    writeln!(
        io::stdout(),
        "Remote link server enabled: {}",
        state_dir.display()
    )
    .ok();
    Ok(())
}

fn cmd_disable(state_dir: &Path) -> Result<(), Error> {
    krill_link::service::disable_server(state_dir)?;
    writeln!(
        io::stdout(),
        "Remote link server disabled: {}",
        state_dir.display()
    )
    .ok();
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct InviteArgs {
    ttl_secs: Option<u64>,
    no_qr: bool,
    json: bool,
}

fn parse_invite_args(args: &[String]) -> Result<InviteArgs, Error> {
    let mut ttl_secs: Option<u64> = None;
    let mut no_qr = false;
    let mut json = false;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--ttl-secs" => {
                i += 1;
                let val = args.get(i).ok_or_else(|| {
                    Error::InvalidConfig("--ttl-secs requires a value".to_owned())
                })?;
                let secs: u64 = val
                    .parse()
                    .map_err(|e| Error::InvalidConfig(format!("invalid --ttl-secs value: {e}")))?;
                if secs == 0 {
                    return Err(Error::InvalidConfig(
                        "--ttl-secs must be greater than 0".to_owned(),
                    ));
                }
                ttl_secs = Some(secs);
            }
            "--no-qr" => no_qr = true,
            "--json" => json = true,
            other => {
                return Err(Error::InvalidConfig(format!(
                    "unknown invite argument: {other}"
                )));
            }
        }
        i += 1;
    }

    Ok(InviteArgs {
        ttl_secs,
        no_qr,
        json,
    })
}

fn format_invite_ttl(ttl: Duration) -> String {
    let secs = ttl.as_secs();
    let h = secs / 3600;
    let m = (secs % 3600) / 60;
    let s = secs % 60;
    match (h, m, s) {
        (0, 0, s) => format!("{s}s"),
        (0, m, 0) => format!("{m}m"),
        (0, m, s) => format!("{m}m{s}s"),
        (h, 0, 0) => format!("{h}h"),
        (h, m, 0) => format!("{h}h{m}m"),
        (h, 0, s) => format!("{h}h{s}s"),
        (h, m, s) => format!("{h}h{m}m{s}s"),
    }
}

async fn cmd_invite(state_dir: &Path, args: &[String]) -> Result<(), Error> {
    let parsed = parse_invite_args(args)?;
    let effective_ttl = parsed
        .ttl_secs
        .map_or(DEFAULT_INVITE_TTL, Duration::from_secs);
    let out = create_invite_command(state_dir, parsed.ttl_secs.map(Duration::from_secs)).await?;
    if parsed.json {
        let mut value =
            serde_json::to_value(&out).map_err(|e| Error::InvalidConfig(e.to_string()))?;
        if parsed.no_qr
            && let Some(fields) = value.as_object_mut()
        {
            fields.remove("qr_unicode");
        }
        let s = serde_json::to_string_pretty(&value)
            .map_err(|e| Error::InvalidConfig(e.to_string()))?;
        writeln!(io::stdout(), "{s}").ok();
    } else {
        let ttl_label = format_invite_ttl(effective_ttl);
        writeln!(
            io::stdout(),
            "Invite link, valid once for {ttl_label}. Paste it on the other computer:\n{}",
            out.link
        )
        .ok();
        if !parsed.no_qr {
            writeln!(io::stdout(), "\n{}", out.qr_unicode).ok();
        }
    }
    Ok(())
}

async fn cmd_pair(state_dir: &Path, args: &[String]) -> Result<(), Error> {
    let mut link: Option<String> = None;
    let mut device_name: Option<String> = None;
    let mut json = false;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--name" => {
                i += 1;
                let val = args.get(i).ok_or_else(|| {
                    Error::InvalidConfig("--name requires a device name".to_owned())
                })?;
                device_name = Some(val.clone());
            }
            "--json" => json = true,
            other if !other.starts_with("--") && link.is_none() => {
                link = Some(other.to_owned());
            }
            other => {
                return Err(Error::InvalidConfig(format!(
                    "unknown pair argument: {other}"
                )));
            }
        }
        i += 1;
    }

    let link_str =
        link.ok_or_else(|| Error::InvalidConfig("pair requires <krill://pair?...>".to_owned()))?;
    let name_str = device_name
        .ok_or_else(|| Error::InvalidConfig("pair requires --name <device name>".to_owned()))?;

    let host = pair_with_server(state_dir, &link_str, &name_str).await?;
    if json {
        let s =
            serde_json::to_string_pretty(&host).map_err(|e| Error::InvalidConfig(e.to_string()))?;
        writeln!(io::stdout(), "{s}").ok();
    } else {
        writeln!(
            io::stdout(),
            "Paired successfully: id={} name={} node_id={}",
            host.id,
            host.name,
            host.node_id
        )
        .ok();
    }
    Ok(())
}

fn cmd_devices(state_dir: &Path, args: &[String]) -> Result<(), Error> {
    let json = args.iter().any(|a| a == "--json");
    let devices = load_devices(state_dir)?;
    if json {
        let s = serde_json::to_string_pretty(&devices)
            .map_err(|e| Error::InvalidConfig(e.to_string()))?;
        writeln!(io::stdout(), "{s}").ok();
    } else if devices.is_empty() {
        writeln!(io::stdout(), "No paired devices.").ok();
    } else {
        writeln!(io::stdout(), "ID\tNAME\tADDED_AT\tNODE_ID").ok();
        for d in devices {
            writeln!(
                io::stdout(),
                "{}\t{}\t{}\t{}",
                d.id,
                d.name,
                d.added_at,
                d.node_id
            )
            .ok();
        }
    }
    Ok(())
}

async fn cmd_revoke(state_dir: &Path, args: &[String]) -> Result<(), Error> {
    let mut target: Option<String> = None;
    let mut json = false;
    for arg in args {
        if arg == "--json" {
            json = true;
        } else if !arg.starts_with("--") && target.is_none() {
            target = Some(arg.clone());
        } else {
            return Err(Error::InvalidConfig(format!(
                "unknown revoke argument: {arg}"
            )));
        }
    }
    let id =
        target.ok_or_else(|| Error::InvalidConfig("revoke requires <device id>".to_owned()))?;
    let removed = revoke_device_command(state_dir, &id).await?;
    if json {
        let s = serde_json::to_string_pretty(&removed)
            .map_err(|e| Error::InvalidConfig(e.to_string()))?;
        writeln!(io::stdout(), "{s}").ok();
    } else {
        writeln!(
            io::stdout(),
            "Device revoked: id={} name={}",
            removed.id,
            removed.name
        )
        .ok();
    }
    Ok(())
}

async fn cmd_connect(state_dir: &Path, args: &[String]) -> Result<(), Error> {
    let Some(host_id) = args.first().filter(|a| !a.starts_with("--")) else {
        return Err(Error::InvalidConfig(
            "connect requires <host_id>".to_owned(),
        ));
    };
    let handle = start_tunnel(ConnectOptions::new(
        state_dir.to_path_buf(),
        host_id.clone(),
    ))
    .await?;
    writeln!(
        io::stdout(),
        "Remote host tunnel listening: {}",
        handle.socket_path().display()
    )
    .ok();
    io::stdout().flush().ok();

    wait_for_shutdown_signal().await;
    handle.shutdown().await;
    Ok(())
}

fn cmd_status(state_dir: &Path, args: &[String]) -> Result<(), Error> {
    let mut json = false;
    let mut host_filter: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--json" => json = true,
            "--host" => {
                i += 1;
                let h = args
                    .get(i)
                    .ok_or_else(|| Error::InvalidConfig("--host requires a value".to_owned()))?;
                host_filter = Some(h.clone());
            }
            other => {
                return Err(Error::InvalidConfig(format!(
                    "unknown status argument: {other}"
                )));
            }
        }
        i += 1;
    }

    if let Some(hid) = host_filter {
        let st = host_status(state_dir, &hid)?;
        if json {
            let s = serde_json::to_string_pretty(&st)
                .map_err(|e| Error::InvalidConfig(e.to_string()))?;
            writeln!(io::stdout(), "{s}").ok();
        } else {
            writeln!(
                io::stdout(),
                "Host: {} ({})\nState: {:?}\nRoute: {:?}\nRTT: {:?} ms\nSocket: {}",
                st.host_name,
                st.host_id,
                st.state,
                st.route,
                st.rtt_ms,
                st.socket_path.display()
            )
            .ok();
        }
        return Ok(());
    }

    let report = status_report(state_dir)?;
    if json {
        let s = serde_json::to_string_pretty(&report)
            .map_err(|e| Error::InvalidConfig(e.to_string()))?;
        writeln!(io::stdout(), "{s}").ok();
    } else {
        writeln!(
            io::stdout(),
            "Node ID: {}\nRelay mode: {}\nServer running: {}\nAllowed devices: {}",
            report.node_id,
            report.relay_mode,
            report.serve_running,
            report.allowed_devices_count
        )
        .ok();
        for h in report.hosts {
            writeln!(
                io::stdout(),
                "  - {} ({}): state={:?} route={:?} rtt_ms={:?} socket={}",
                h.host_id,
                h.host_name,
                h.state,
                h.route,
                h.rtt_ms,
                h.socket_path.display()
            )
            .ok();
        }
    }
    Ok(())
}

fn cmd_config(state_dir: &Path, args: &[String]) -> Result<(), Error> {
    let mut cfg = LinkConfig::load(state_dir)?;
    let mut json = false;
    let mut modified = false;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--relay" => {
                i += 1;
                let val = args
                    .get(i)
                    .ok_or_else(|| Error::InvalidConfig("--relay requires a value".to_owned()))?;
                cfg.relay = match val.trim() {
                    "n0" | "default" => RelaySetting::N0,
                    "off" | "disabled" | "none" => RelaySetting::Off,
                    url => RelaySetting::Custom(url.to_owned()),
                };
                modified = true;
            }
            "--server-name" => {
                i += 1;
                let val = args.get(i).ok_or_else(|| {
                    Error::InvalidConfig("--server-name requires a value".to_owned())
                })?;
                cfg.server_name = Some(val.clone());
                modified = true;
            }
            "--bind-port" => {
                i += 1;
                let val = args.get(i).ok_or_else(|| {
                    Error::InvalidConfig("--bind-port requires a value".to_owned())
                })?;
                let port: u16 = val
                    .parse()
                    .map_err(|e| Error::InvalidConfig(format!("invalid port: {e}")))?;
                cfg.bind_port = if port == 0 { None } else { Some(port) };
                modified = true;
            }
            "--resume-grace-ms" | "--replay-buffer-bytes" => {
                let field = &args[i];
                i += 1;
                let value = args.get(i).ok_or_else(|| {
                    Error::InvalidConfig(format!(
                        "{field}: {}",
                        krill_link::messages::VALUE_REQUIRED
                    ))
                })?;
                if field == "--resume-grace-ms" {
                    cfg.resume_grace_ms = value.parse().map_err(|_| {
                        Error::InvalidConfig("invalid grace period: enter milliseconds".to_owned())
                    })?;
                } else {
                    cfg.replay_buffer_bytes = value.parse().map_err(|_| {
                        Error::InvalidConfig("invalid buffer limit: enter bytes".to_owned())
                    })?;
                }
                modified = true;
            }
            "--json" => json = true,
            other => {
                return Err(Error::InvalidConfig(format!(
                    "unknown config argument: {other}"
                )));
            }
        }
        i += 1;
    }

    if modified {
        cfg.save(state_dir)?;
    }

    if json {
        let out = serde_json::json!({
            "relay_mode": cfg.relay.as_str(),
            "server_name": cfg.server_name,
            "bind_port": cfg.bind_port,
            "lan_discovery": cfg.lan_discovery,
            "resume_grace_ms": cfg.resume_grace_ms,
            "replay_buffer_bytes": cfg.replay_buffer_bytes,
        });
        let s =
            serde_json::to_string_pretty(&out).map_err(|e| Error::InvalidConfig(e.to_string()))?;
        writeln!(io::stdout(), "{s}").ok();
    } else {
        writeln!(io::stdout(), "{}", cfg.to_toml_string()?).ok();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_invite_args_validates_ttl_and_flags() {
        let defaults = parse_invite_args(&[]).unwrap();
        assert_eq!(
            defaults,
            InviteArgs {
                ttl_secs: None,
                no_qr: false,
                json: false,
            }
        );

        let custom = parse_invite_args(&[
            "--ttl-secs".to_owned(),
            "90".to_owned(),
            "--no-qr".to_owned(),
            "--json".to_owned(),
        ])
        .unwrap();
        assert_eq!(
            custom,
            InviteArgs {
                ttl_secs: Some(90),
                no_qr: true,
                json: true,
            }
        );

        assert!(parse_invite_args(&["--ttl-secs".to_owned(), "0".to_owned()]).is_err());
        assert!(parse_invite_args(&["--ttl-secs".to_owned()]).is_err());
        assert!(parse_invite_args(&["--bogus".to_owned()]).is_err());
    }

    #[test]
    fn format_invite_ttl_matches_duration_units() {
        assert_eq!(format_invite_ttl(DEFAULT_INVITE_TTL), "10m");
        assert_eq!(format_invite_ttl(Duration::from_secs(45)), "45s");
        assert_eq!(format_invite_ttl(Duration::from_secs(60)), "1m");
        assert_eq!(format_invite_ttl(Duration::from_secs(90)), "1m30s");
        assert_eq!(format_invite_ttl(Duration::from_secs(3600)), "1h");
        assert_eq!(format_invite_ttl(Duration::from_secs(3665)), "1h1m5s");
    }
}
