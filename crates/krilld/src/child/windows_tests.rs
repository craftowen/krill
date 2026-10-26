use super::*;
use std::io::Read as _;
use std::process::Stdio;

#[expect(
    clippy::unwrap_used,
    reason = "Windows regression fixture must start its command"
)]
fn powershell(script: &str) -> Process {
    spawn(
        Command::new("powershell.exe")
            .args(["-NoProfile", "-NonInteractive", "-Command", script])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped()),
    )
    .unwrap()
}

#[test]
fn pipe_reader_yields_on_empty_open_pipe_and_observes_eof() {
    let gate = std::env::temp_dir().join(format!("krill-command-pipe-gate-{}", std::process::id()));
    std::fs::remove_file(&gate).ok();
    let quoted = gate.to_string_lossy().replace('\'', "''");
    let mut process = powershell(&format!(
        "while (-not [IO.File]::Exists('{quoted}')) {{ Start-Sleep -Milliseconds 10 }}; [Console]::Out.Write('done')"
    ));
    let mut reader = PipeReader::new(process.stdout.take().unwrap()).unwrap();
    let mut buf = [0; 256];
    assert_eq!(
        reader.read(&mut buf).unwrap_err().kind(),
        io::ErrorKind::WouldBlock
    );
    std::fs::write(&gate, b"ready").unwrap();
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut received = Vec::new();
    loop {
        match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => received.extend_from_slice(&buf[..n]),
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
            Err(e) => panic!("pipe read: {e}"),
        }
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(received, b"done");
    assert!(process.wait().unwrap().success());
    std::fs::remove_file(gate).unwrap();
}

#[test]
fn dropping_command_job_kills_its_descendant_and_preserves_unrelated_process() {
    let mut unrelated = powershell("Start-Sleep -Seconds 60");
    let mut process = powershell(
        "$p = Start-Process cmd.exe -ArgumentList '/d','/c','ping -t 127.0.0.1 >NUL' -NoNewWindow -PassThru; [Console]::Out.WriteLine($p.Id); Start-Sleep -Seconds 60",
    );
    let mut reader = PipeReader::new(process.stdout.take().unwrap()).unwrap();
    let mut bytes = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut buf = [0; 256];
    while !bytes.contains(&b'\n') {
        match reader.read(&mut buf) {
            Ok(n) => bytes.extend_from_slice(&buf[..n]),
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
            other => panic!("missing child pid: {other:?}"),
        }
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(5));
    }
    let pid: u32 = String::from_utf8_lossy(&bytes)
        .lines()
        .next()
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    drop(process);
    let deadline = Instant::now() + Duration::from_secs(5);
    while crate::sys::start_us(pid).is_ok() {
        assert!(
            Instant::now() < deadline,
            "command descendant survived job drop"
        );
        thread::sleep(Duration::from_millis(10));
    }
    assert!(unrelated.try_wait().unwrap().is_none());
}
