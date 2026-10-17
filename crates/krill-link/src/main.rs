#![forbid(unsafe_code)]
//! `krill-link` CLI 진입점.

use std::env;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use krill_link::{
    ConnectOptions, Error, LinkConfig, RelaySetting, ServeOptions, create_invite_command,
    disable_launch_agent, enable_launch_agent, host_status, load_devices, pair_with_server,
    resolve_state_dir, revoke_device_command, start_server, start_tunnel, status_report,
};

#[tokio::main]
async fn main() -> ExitCode {
    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            writeln!(io::stderr(), "오류 (Error): {e}").ok();
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
        "enable" => cmd_enable(&state_dir).map_err(|e| e.to_string()),
        "disable" => cmd_disable(&state_dir).map_err(|e| e.to_string()),
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
        "사용법 (Usage):",
        "  krill-link serve [--socket <krilld.sock>] [--state-dir <dir>]",
        "  krill-link enable | disable",
        "  krill-link invite [--ttl-secs <secs>] [--no-qr] [--json]",
        "  krill-link pair <krill://pair?...> --name <기기 이름 / device name> [--json]",
        "  krill-link devices [--json]",
        "  krill-link revoke <기기 ID 또는 이름 / device id or name> [--json]",
        "  krill-link connect <호스트 ID / host id>",
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

    let mut opts = ServeOptions::new(state_dir.to_path_buf());
    opts.krilld_socket = socket_override;
    let handle = start_server(opts).await?;
    writeln!(
        io::stdout(),
        "원격 연결 서버 실행 중 (Remote link server listening): node_id={}",
        handle.node_id()
    )
    .ok();
    io::stdout().flush().ok();

    wait_for_shutdown_signal().await;
    handle.shutdown().await;
    Ok(())
}

fn cmd_enable(state_dir: &Path) -> Result<(), Error> {
    let exe = krill_proto::daemon::sibling_exe("krill-link").map_err(Error::Io)?;
    let plist_path = enable_launch_agent(&exe, state_dir)?;
    writeln!(
        io::stdout(),
        "원격 연결 백그라운드 서비스 켜짐 (Remote link LaunchAgent enabled): {}",
        plist_path.display()
    )
    .ok();
    Ok(())
}

fn cmd_disable(state_dir: &Path) -> Result<(), Error> {
    let plist_path = disable_launch_agent(state_dir)?;
    writeln!(
        io::stdout(),
        "원격 연결 백그라운드 서비스 꺼짐 (Remote link LaunchAgent disabled): {}",
        plist_path.display()
    )
    .ok();
    Ok(())
}

async fn cmd_invite(state_dir: &Path, args: &[String]) -> Result<(), Error> {
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
                ttl_secs =
                    Some(val.parse().map_err(|e| {
                        Error::InvalidConfig(format!("invalid --ttl-secs value: {e}"))
                    })?);
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

    let out = create_invite_command(state_dir, ttl_secs.map(Duration::from_secs)).await?;
    if json {
        let mut value =
            serde_json::to_value(&out).map_err(|e| Error::InvalidConfig(e.to_string()))?;
        if no_qr && let Some(fields) = value.as_object_mut() {
            fields.remove("qr_unicode");
        }
        let s = serde_json::to_string_pretty(&value)
            .map_err(|e| Error::InvalidConfig(e.to_string()))?;
        writeln!(io::stdout(), "{s}").ok();
    } else {
        writeln!(
            io::stdout(),
            "초대 링크 (10분간 1회 유효 — 다른 맥에서 붙여넣으세요 / Invite link, valid once for 10m):\n{}",
            out.link
        )
        .ok();
        if !no_qr {
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
            "짝짓기 완료 (Paired successfully): id={} name={} node_id={}",
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
        writeln!(
            io::stdout(),
            "등록된 허용 기기가 없습니다 (No paired devices)."
        )
        .ok();
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
            "기기 연결 권한 해제됨 (Device revoked): id={} name={}",
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
        "원격 호스트 터널 준비됨 (Remote host tunnel listening): {}",
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
                "호스트 (Host): {} ({})\n상태 (State): {:?}\n경로 (Route): {:?}\nRTT: {:?} ms\n소켓 (Socket): {}",
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
            "내 기기 ID (Node ID): {}\n중계 설정 (Relay mode): {}\n서버 실행 중 (Server running): {}\n허용 기기 수 (Allowed devices): {}",
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
                        Error::InvalidConfig(
                            "유예 시간은 밀리초 숫자로 입력하세요 (invalid grace period)"
                                .to_owned(),
                        )
                    })?;
                } else {
                    cfg.replay_buffer_bytes = value.parse().map_err(|_| {
                        Error::InvalidConfig(
                            "보관 용량은 바이트 숫자로 입력하세요 (invalid buffer limit)"
                                .to_owned(),
                        )
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
