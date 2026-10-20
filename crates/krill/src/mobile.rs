//! 모바일 페어링·조회·회수. 게이트웨이·터널은 krilld가 소유하므로 CLI가 끝나도 유지된다.
use krill_proto::mobile::{self, ConnectMode, MobileReply, MobileRequest};
use std::ffi::OsString;
use std::io;
use std::process::ExitCode;
use std::thread;
use std::time::Duration;

const HELP: &str = "usage: krill mobile pair [--tunnel|--tailscale] [--address <ip:port>] [--json]\n       krill mobile devices [--json]\n       krill mobile revoke <device_id|--all> [--json]\n       krill mobile serve [--tunnel]";
#[derive(Debug, PartialEq, Eq)]
enum Command {
    Pair {
        mode: ConnectMode,
        address: Option<String>,
        json: bool,
    },
    Devices {
        json: bool,
    },
    Revoke {
        id: Option<String>,
        all: bool,
        json: bool,
    },
    Serve {
        tunnel: bool,
    },
    Help,
}
fn parse(args: &[OsString]) -> Result<Command, &'static str> {
    let mut a = args
        .iter()
        .map(|v| v.to_str().ok_or("arguments must be UTF-8"));
    let command = a.next().transpose()?.unwrap_or("--help");
    let mut tunnel = false;
    let mut tailscale = false;
    let mut address = None;
    let mut json = false;
    let mut all = false;
    let mut id = None;
    while let Some(arg) = a.next() {
        match arg? {
            "--json" if !json => json = true,
            "--tunnel" if !tunnel && !tailscale => tunnel = true,
            "--tailscale" if !tailscale && !tunnel => tailscale = true,
            "--address" if address.is_none() => {
                address = Some(
                    a.next()
                        .transpose()?
                        .ok_or("--address requires IPv4:port")?
                        .to_owned(),
                );
            }
            "--all" if !all => all = true,
            arg if !arg.starts_with('-') && id.is_none() => id = Some(arg.to_owned()),
            _ => return Err(HELP),
        }
    }
    match command {
        "--help" | "-h"
            if !tailscale && !tunnel && !json && !all && id.is_none() && address.is_none() =>
        {
            Ok(Command::Help)
        }
        "pair" if !all && id.is_none() => {
            let loopback = address
                .as_deref()
                .and_then(|value| value.parse::<std::net::SocketAddrV4>().ok())
                .is_some_and(|address| address.ip().is_loopback());
            if !tailscale && !tunnel && address.is_some() && !loopback {
                return Err(
                    "LAN pairing has been retired; use --tailscale for a tailnet address or Relay server without --address",
                );
            }
            Ok(Command::Pair {
                mode: if tailscale {
                    ConnectMode::Tailscale
                } else if !tunnel && loopback {
                    ConnectMode::Lan
                } else {
                    ConnectMode::Tunnel
                },
                address,
                json,
            })
        }
        "devices" if !tailscale && !all && !tunnel && id.is_none() && address.is_none() => {
            Ok(Command::Devices { json })
        }
        "revoke" if !tailscale && !tunnel && address.is_none() && (all != id.is_some()) => {
            Ok(Command::Revoke { id, all, json })
        }
        "serve" if !tailscale && !all && !json && id.is_none() && address.is_none() => {
            Ok(Command::Serve { tunnel })
        }
        _ => Err(HELP),
    }
}
pub fn run(args: &[OsString]) -> ExitCode {
    match parse(args) {
        Ok(command) => {
            let tunnel = matches!(
                command,
                Command::Pair {
                    mode: ConnectMode::Tunnel,
                    ..
                } | Command::Serve { tunnel: true }
            );
            match execute(command) {
                Ok(()) => ExitCode::SUCCESS,
                // 데몬은 오류 종류만 넘기므로 cloudflared가 없을 때 "entity not found"만 보였다(설 실기, v0.1.8).
                Err(e) if tunnel && e.kind() == std::io::ErrorKind::NotFound => {
                    eprintln!(
                        "mobile: cloudflared not found; install it with `brew install cloudflared` and retry ({e})"
                    );
                    ExitCode::FAILURE
                }
                Err(e) => {
                    eprintln!("mobile: {e}");
                    ExitCode::FAILURE
                }
            }
        }
        Err(message) => {
            eprintln!("{message}");
            ExitCode::from(2)
        }
    }
}
fn call(request: &MobileRequest) -> io::Result<MobileReply> {
    mobile::client::request(
        &krill_proto::daemon::Paths::dev()?,
        request,
        krill_proto::ipc::ClientRole::Cli,
    )
}
fn execute(command: Command) -> io::Result<()> {
    match command {
        Command::Help => println!("{HELP}"),
        Command::Pair {
            mode,
            address,
            json,
        } => {
            let before = match call(&MobileRequest::ListDevices)? {
                MobileReply::Devices(devices) => devices
                    .into_iter()
                    .map(|d| d.device_id)
                    .collect::<std::collections::BTreeSet<_>>(),
                _ => std::collections::BTreeSet::new(),
            };
            let reply = call(&MobileRequest::MintPairingTicket {
                mode,
                custom_address: address,
                rotate: true,
            })?;
            if json {
                println!(
                    "{}",
                    serde_json::to_string(&reply).map_err(io::Error::other)?
                );
                return Ok(());
            }
            let MobileReply::PairingOffer {
                url, expires_at_ms, ..
            } = reply
            else {
                return Err(io::Error::other("expected pairing offer"));
            };
            let qr = mobile::qr::Qr::encode(&url).map_err(io::Error::other)?;
            println!(
                "\x1b[30;47m{}\x1b[0m\n{url}\nWaiting for pairing (5 minutes)…",
                qr.terminal()
            );
            loop {
                if mobile::client::now_ms() >= expires_at_ms {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "pairing offer expired",
                    ));
                }
                if let MobileReply::GatewayStatus {
                    devices,
                    active_offer,
                    ..
                } = call(&MobileRequest::Status)?
                {
                    if devices
                        .iter()
                        .any(|d| d.revoked_at_ms.is_none() && !before.contains(&d.device_id))
                    {
                        println!("Pairing completed");
                        break;
                    }
                    if !active_offer {
                        return Err(io::Error::new(
                            io::ErrorKind::Interrupted,
                            "pairing offer cancelled",
                        ));
                    }
                }
                if let MobileReply::PairingOffer { url: current, .. } =
                    call(&MobileRequest::CurrentPairingOffer)?
                    && current != url
                {
                    return Err(io::Error::new(
                        io::ErrorKind::Interrupted,
                        "pairing offer replaced",
                    ));
                }
                thread::sleep(Duration::from_millis(500));
            }
        }
        Command::Devices { json } => {
            let MobileReply::Devices(devices) = call(&MobileRequest::ListDevices)? else {
                return Err(io::Error::other("expected device list"));
            };
            if json {
                println!(
                    "{}",
                    serde_json::to_string(&devices).map_err(io::Error::other)?
                );
            } else {
                for device in devices {
                    println!(
                        "{}\t{}\t{}",
                        device.device_id,
                        device.label,
                        if device.revoked_at_ms.is_some() {
                            "revoked"
                        } else {
                            "paired"
                        }
                    );
                }
            }
        }
        Command::Revoke { id, all, json } => {
            let reply = call(&MobileRequest::RevokeDevice { device_id: id, all })?;
            if json {
                println!(
                    "{}",
                    serde_json::to_string(&reply).map_err(io::Error::other)?
                );
            } else if let MobileReply::Revoked { count } = reply {
                println!("Revoked {count} device(s)");
            }
        }
        Command::Serve { tunnel } => {
            if tunnel {
                let reply = call(&MobileRequest::MintPairingTicket {
                    mode: ConnectMode::Tunnel,
                    custom_address: None,
                    rotate: false,
                })?;
                if let MobileReply::PairingOffer { url, .. } = reply {
                    println!("{url}");
                }
                loop {
                    thread::sleep(Duration::from_secs(30));
                    call(&MobileRequest::MintPairingTicket {
                        mode: ConnectMode::Tunnel,
                        custom_address: None,
                        rotate: false,
                    })?;
                }
            } else {
                println!(
                    "{}",
                    serde_json::to_string(&call(&MobileRequest::Status)?)
                        .map_err(io::Error::other)?
                );
            }
        }
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    fn args(a: &[&str]) -> Vec<OsString> {
        a.iter().map(OsString::from).collect()
    }
    #[test]
    fn command_validation() {
        assert_eq!(
            parse(&args(&["devices", "--json"])).unwrap(),
            Command::Devices { json: true }
        );
        assert!(matches!(
            parse(&args(&["pair", "--tunnel", "--address", "127.0.0.1:1234"])),
            Ok(Command::Pair {
                mode: ConnectMode::Tunnel,
                ..
            })
        ));
        assert!(matches!(
            parse(&args(&["pair"])),
            Ok(Command::Pair {
                mode: ConnectMode::Tunnel,
                ..
            })
        ));
        assert!(matches!(
            parse(&args(&["pair", "--tailscale"])),
            Ok(Command::Pair {
                mode: ConnectMode::Tailscale,
                ..
            })
        ));
        assert!(matches!(
            parse(&args(&["pair", "--address", "127.0.0.1:0", "--json"])),
            Ok(Command::Pair {
                mode: ConnectMode::Lan,
                ..
            })
        ));
        for a in [
            vec!["pair", "--address", "192.168.1.2:0"],
            vec!["pair", "--tunnel", "--tailscale"],
            vec!["revoke"],
            vec!["revoke", "phone", "--all"],
            vec!["pair", "--address"],
            vec!["devices", "--tunnel"],
            vec!["serve", "--json"],
        ] {
            assert!(parse(&args(&a)).is_err());
        }
    }
}
