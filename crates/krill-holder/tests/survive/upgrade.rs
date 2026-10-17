//! MK-004·MK-065 일반 프로세스 exec 핸드오프 검증.

use super::*;

/// 로그 출력이 막혀도 복원 ACK는 파일 IO보다 먼저 도착한다.
#[test]
fn exec_handoff_ack_precedes_log_io() {
    let rig = Rig::start("log-io", &[("MKH_MODE", "log-io".into())]);
    let result = wait_for("로그 IO 전 복원 ACK", 10, || {
        get(&rig.dir.join("result"))
    });
    assert_eq!(result, "ok");
}

pub(super) fn blocked_log(link: &mut Link, dir: &Path) {
    use rustix::fs::{Mode, OFlags, open};
    use rustix::io::{Errno, read, write};

    // 첫 시작 로그가 끝난 뒤 FIFO를 채워 다음 exec의 시작 로그만 막는다.
    let path = dir.join("logs/krill-holder.log");
    fs::remove_file(&path).unwrap();
    assert!(
        Command::new("/usr/bin/mkfifo")
            .args(["-m", "600"])
            .arg(&path)
            .status()
            .unwrap()
            .success()
    );
    let fifo = open(&path, OFlags::RDWR | OFlags::NONBLOCK, Mode::empty()).unwrap();
    let fill = [b'x'; 1024];
    let mut filled = 0;
    let mut chunk = fill.as_slice();
    loop {
        match write(&fifo, chunk) {
            Ok(count) => filled += count,
            Err(Errno::AGAIN) if chunk.len() > 1 => chunk = &fill[..1],
            Err(Errno::AGAIN) => break,
            Err(error) => panic!("FIFO 채우기 실패: {error}"),
        }
    }
    send(
        link,
        &ToHolder::UpgradeHolder {
            req: 1,
            path: HOLDER.as_bytes().to_vec(),
        },
        &[],
    );
    assert!(matches!(reply(link, dir).0, FromHolder::Hello { .. }));
    assert_eq!(reply(link, dir).0, FromHolder::Done { req: 1 });

    // ACK 뒤에만 로그를 풀어 준다. 기존 순서는 여기까지 오지 못해 부모 테스트가 시간 초과된다.
    let mut buf = [0; 4096];
    while filled > 0 {
        let len = filled.min(buf.len());
        filled -= read(&fifo, &mut buf[..len]).unwrap();
    }
    send(
        link,
        &ToHolder::Release {
            req: 2,
            id: SessionId(99),
        },
        &[],
    );
    assert_eq!(reply(link, dir).0, FromHolder::Failed { req: 2, errno: 3 });
    let count = read(&fifo, &mut buf).unwrap();
    assert!(String::from_utf8_lossy(&buf[..count]).contains("holder starting"));
    fs::remove_file(path).unwrap();
    put(&dir.join("result"), "ok");
    while next(link, dir).is_some() {}
}

/// MK-004: 세션 10개 × 1MB/s 출력 중 holder exec 핸드오프를 거듭해도 줄 손실 0,
/// 종료 코드 수신, 핸드오프 ≤ 50ms, holder PID 유지.
#[test]
fn exec_handoff() {
    let upgrades = env_u64("MKH_UPGRADES", 10);
    let rig = Rig::start(
        "exec",
        &[
            ("MKH_GENS", "10".into()),
            ("MKH_RATE", "1000000".into()),
            ("MKH_VERIFY", "1".into()),
            ("MKH_UPGRADES_RUN", upgrades.to_string()),
            ("MKH_EVERY_MS", "300".into()),
        ],
    );
    let holder_pid = rig.holder.id();
    let preflight_ns: u64 = wait_for("사전 검증", 30, || get(&rig.dir.join("preflight")))
        .parse()
        .unwrap();
    let log = wait_for("핸드오프 완료", 60, || get(&rig.dir.join("handoffs")));
    let mut ns = Vec::new();
    for l in log.lines() {
        let (t, p) = l.split_once(' ').unwrap();
        assert_eq!(p.parse::<u32>().unwrap(), holder_pid, "exec 뒤 PID가 바뀜");
        ns.push(t.parse::<u64>().unwrap());
    }
    assert_eq!(ns.len(), usize::try_from(upgrades).unwrap());
    println!(
        "holder exec {upgrades}회: 사전 검증 {:.2}ms, 핸드오프 p50 {:.2}ms 최대 {:.2}ms",
        Duration::from_nanos(preflight_ns).as_secs_f64() * 1e3,
        pct(&mut ns, 50),
        pct(&mut ns, 100)
    );
    assert!(pct(&mut ns, 100) <= 50.0);
    rig.stop_and_check_exits();
    for (id, _) in rig.gens() {
        let v = wait_for("검증 결과", 30, || {
            get(&rig.dir.join(format!("verify-{id}")))
        });
        assert!(v.starts_with("ok "), "세션 {id}: {v}");
    }
}
