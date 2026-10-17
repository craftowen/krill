//! `krill history clear`: krilld를 통해 디스크 세그먼트만 삭제한다.

use krill_proto::daemon::{self, Paths};
use krill_proto::history::{FEAT_HISTORY, HistoryReply, HistoryRequest};
use krill_proto::ipc::{self, ClientRole};
use krill_proto::screen::Msg as _;
use std::ffi::OsString;
use std::io::Write as _;
use std::process::ExitCode;
use std::time::Duration;

pub fn run(args: &[OsString]) -> ExitCode {
    if !matches!(args, [command] if command == "clear") {
        eprintln!("usage: krill history clear");
        return ExitCode::from(2);
    }
    match clear() {
        Ok(()) => {
            println!("history cleared");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("history: {e}");
            ExitCode::FAILURE
        }
    }
}

fn clear() -> Result<(), String> {
    let paths = Paths::dev().map_err(|e| e.to_string())?;
    let mut hello = daemon::client_hello(ClientRole::Cli, "krill history clear");
    hello.features = FEAT_HISTORY;
    let mut conn = daemon::connect(&paths, &hello).map_err(|e| e.to_string())?;
    if conn.negotiated.features & FEAT_HISTORY == 0 {
        return Err("krilld does not support history clear; update the daemon".to_owned());
    }
    let timeout = Some(Duration::from_secs(30));
    conn.stream
        .set_read_timeout(timeout)
        .map_err(|e| e.to_string())?;
    conn.stream
        .set_write_timeout(timeout)
        .map_err(|e| e.to_string())?;
    conn.stream
        .write_all(&HistoryRequest::Clear.encode().map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    let (kind, body) = ipc::read_frame(&mut conn.stream)
        .map_err(|e| e.to_string())?
        .ok_or("krilld disconnected")?;
    match HistoryReply::decode(kind, &body).map_err(|e| e.to_string())? {
        HistoryReply::Cleared => Ok(()),
        HistoryReply::Paused { .. } => Err("unexpected history pause response".into()),
        HistoryReply::Failed(e) => Err(e.to_string()),
    }
}
