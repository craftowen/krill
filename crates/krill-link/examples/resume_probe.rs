//! Reproducible release probe: real krill-link serve + library connect, both buffer-saturated.
//! Run alongside footprint: phase records include both process IDs.
#![forbid(unsafe_code)]
use krill_link::{
    ConnectOptions, DeviceIdentity, LinkConfig, RelaySetting, RemoteHost, StreamPhase,
    start_tunnel, upsert_device, upsert_host,
};
use std::error::Error;
use std::fs;
use std::io::Write as _;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::{UnixListener, UnixStream};

const BLOCK: [u8; 8192] = [0x5a; 8192];

struct ServerChild(Child);
impl Drop for ServerChild {
    fn drop(&mut self) {
        self.0.kill().ok();
        self.0.wait().ok();
    }
}

fn phase(name: &str, server: u32, details: &serde_json::Value) -> Result<(), Box<dyn Error>> {
    let record = serde_json::json!({"phase":name,"connect_pid":std::process::id(),"serve_pid":server,"details":details});
    writeln!(std::io::stdout(), "{record}")?;
    std::io::stdout().flush()?;
    let mut acknowledgement = String::new();
    std::io::stdin().read_line(&mut acknowledgement)?;
    Ok(())
}

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() -> Result<(), Box<dyn Error>> {
    let base = PathBuf::from(format!("/tmp/m134-probe-{}", std::process::id()));
    fs::create_dir_all(base.join("s/run"))?;
    fs::create_dir_all(base.join("c"))?;
    let config = LinkConfig {
        relay: RelaySetting::Off,
        replay_buffer_bytes: 4 * 1024 * 1024,
        ..LinkConfig::default()
    };
    config.save(&base.join("s"))?;
    config.save(&base.join("c"))?;
    let identity = DeviceIdentity::load_or_create(&base.join("c"))?;
    upsert_device(&base.join("s"), identity.node_id(), "probe", 0)?;
    let listener = UnixListener::bind(base.join("s/run/krilld.sock"))?;
    let current = std::env::current_exe()?;
    let binary = current
        .parent()
        .and_then(std::path::Path::parent)
        .ok_or("missing binary directory")?
        .join("krill-link");
    let mut child = ServerChild(
        Command::new(binary)
            .args(["serve", "--state-dir"])
            .arg(base.join("s"))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?,
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    let addr = loop {
        if let Some(addr) = krill_link::read_live_server_addr(&base.join("s")) {
            break addr;
        }
        if let Some(status) = child.0.try_wait()? {
            return Err(format!("serve exited: {status}").into());
        }
        if Instant::now() >= deadline {
            return Err("serve startup timeout".into());
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    };
    upsert_host(
        &base.join("c"),
        RemoteHost::from_endpoint_addr("probe".to_owned(), "probe", &addr, 0),
    )?;
    let tunnel = start_tunnel(ConnectOptions::new(base.join("c"), "probe")).await?;
    let mut streams = Vec::new();
    let mut idle_holders = Vec::new();
    for index in 0..32 {
        let mut app = UnixStream::connect(tunnel.socket_path()).await?;
        let (mut upstream, _) = listener.accept().await?;
        app.write_all(b"x").await?;
        let mut byte = [0];
        upstream.read_exact(&mut byte).await?;
        upstream.write_all(b"y").await?;
        app.read_exact(&mut byte).await?;
        if index < 4 {
            streams.push((app, upstream));
        } else {
            idle_holders.push((app, upstream));
        }
    }
    tokio::time::sleep(Duration::from_millis(100)).await;
    phase("idle", child.0.id(), &serde_json::json!({"streams":32}))?;
    tunnel.set_transport_offline(true);
    let detected = loop {
        if tunnel
            .status()
            .streams
            .iter()
            .all(|s| s.state == StreamPhase::Suspended)
        {
            break Instant::now();
        }
        if Instant::now() >= deadline {
            return Err("outage detection timeout".into());
        }
        tokio::time::sleep(Duration::from_millis(2)).await;
    };
    for (app, upstream) in &mut streams {
        for _ in 0..120 {
            app.write_all(&BLOCK).await?;
            upstream.write_all(&BLOCK).await?;
        }
    }
    tokio::time::sleep(Duration::from_millis(200)).await;
    let buffered: u64 = tunnel
        .status()
        .streams
        .iter()
        .map(|s| s.buffered_bytes)
        .sum();
    phase(
        "saturated",
        child.0.id(),
        &serde_json::json!({"buffered_bytes":buffered,"buffered_streams":4,"total_streams":32}),
    )?;
    if let Some((app, _)) = streams.first_mut() {
        // Exceed the process budget while staying below the individual 4 MiB cap.
        for _ in 0..80 {
            if app.write_all(&BLOCK).await.is_err() {
                break;
            }
        }
    }
    tokio::time::sleep(Duration::from_millis(200)).await;
    phase(
        "overflow_isolated",
        child.0.id(),
        &serde_json::json!({"streams":tunnel.status().streams}),
    )?;
    let redial = Instant::now();
    tunnel.set_transport_offline(false);
    let mut readers = tokio::task::JoinSet::new();
    for (app, upstream) in streams.into_iter().skip(1) {
        for mut stream in [app, upstream] {
            readers.spawn(async move {
                let mut buffer = [0; 8192];
                for _ in 0..120 {
                    stream.read_exact(&mut buffer).await?;
                    if buffer != BLOCK {
                        return Err(std::io::Error::other("byte mismatch"));
                    }
                }
                Ok::<_, std::io::Error>(stream)
            });
        }
    }
    let mut retained = Vec::new();
    while let Some(result) =
        tokio::time::timeout(Duration::from_secs(10), readers.join_next()).await?
    {
        retained.push(result??);
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if tunnel
            .status()
            .streams
            .iter()
            .all(|s| s.state == StreamPhase::Normal && s.buffered_bytes == 0)
        {
            break;
        }
        if Instant::now() >= deadline {
            return Err("resume acknowledgement timeout".into());
        }
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    phase(
        "resumed",
        child.0.id(),
        &serde_json::json!({"detected_to_complete_ms":detected.elapsed().as_secs_f64()*1000.0,"redial_to_complete_ms":redial.elapsed().as_secs_f64()*1000.0,"streams":tunnel.status().streams}),
    )?;
    for _ in 0..5 {
        // Stay within the per-device resume admission rate while exercising reuse.
        tokio::time::sleep(Duration::from_millis(1100)).await;
        tunnel.set_transport_offline(true);
        let deadline = Instant::now() + Duration::from_secs(5);
        while !tunnel
            .status()
            .streams
            .iter()
            .all(|s| s.state == StreamPhase::Suspended)
        {
            if Instant::now() >= deadline {
                return Err("repeat outage timeout".into());
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        for stream in &mut retained {
            for _ in 0..120 {
                stream.write_all(&BLOCK).await?;
            }
        }
        tunnel.set_transport_offline(false);
        let mut tasks = tokio::task::JoinSet::new();
        for mut stream in std::mem::take(&mut retained) {
            tasks.spawn(async move {
                let mut buffer = [0; 8192];
                for _ in 0..120 {
                    stream.read_exact(&mut buffer).await?;
                    if buffer != BLOCK {
                        return Err(std::io::Error::other("repeat byte mismatch"));
                    }
                }
                Ok::<_, std::io::Error>(stream)
            });
        }
        while let Some(result) =
            tokio::time::timeout(Duration::from_secs(10), tasks.join_next()).await?
        {
            retained.push(result??);
        }
    }
    phase(
        "repeated",
        child.0.id(),
        &serde_json::json!({"outages":6,"streams":tunnel.status().streams}),
    )?;
    drop(retained);
    drop(idle_holders);
    tunnel.shutdown().await;
    krill_link::revoke_device_command(&base.join("s"), "probe").await?;
    tokio::time::sleep(Duration::from_millis(100)).await;
    phase("cleaned", child.0.id(), &serde_json::json!({}))?;
    drop(child);
    fs::remove_dir_all(base)?;
    Ok(())
}
