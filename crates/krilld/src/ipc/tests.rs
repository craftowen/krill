use std::io::Read;
use std::path::PathBuf;
use std::thread;

use krill_proto::ipc::{ClientRole, MAX_FRAME, PROTO_MAX, PROTO_MIN};

use super::*;

/// 테스트마다 짧은 임시 디렉터리(소켓 경로 104바이트 제한 안).
struct Dir(PathBuf);

impl Dir {
    fn new(tag: &str) -> Self {
        let d = std::env::temp_dir().join(format!("mkipc-{}-{tag}", std::process::id()));
        fs::remove_dir_all(&d).ok();
        Self(d)
    }
    fn sock(&self) -> PathBuf {
        self.0.join("k.sock")
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).ok();
    }
}

fn hello() -> ServerHello {
    ServerHello {
        proto_min: PROTO_MIN,
        proto_max: PROTO_MAX,
        features: ipc::FEATURES,
        build: "test".to_owned(),
        instance_id: 1,
    }
}

fn unhex(s: &str) -> Vec<u8> {
    s.as_bytes()
        .chunks(2)
        .map(|p| u8::from_str_radix(std::str::from_utf8(p).unwrap(), 16).unwrap())
        .collect()
}

/// 클라이언트 스레드: `ServerHello`를 읽어 확인하고 `reply`를 그대로 보낸다.
fn client(path: PathBuf, reply: Vec<u8>) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let mut c = UnixStream::connect(path).unwrap();
        let (kind, body) = ipc::read_frame(&mut c).unwrap().unwrap();
        assert_eq!(ServerHello::decode(kind, &body).unwrap(), hello());
        c.write_all(&reply).unwrap();
        // 서버가 끝낼 때까지 연결을 연다.
        c.read_to_end(&mut Vec::new()).ok();
    })
}

#[test]
fn rejects_other_uid_before_sending_anything() {
    let dir = Dir::new("uid");
    let mut lis = Listener::bind(&dir.sock()).unwrap();
    let me = lis.uid;
    // 실제로 다른 UID 프로세스를 띄우려면 root가 필요하다. 대신 기대 UID를 바꿔 실제 getpeereid 결과와 어긋나게 한다.
    lis.uid = me.wrapping_add(1);
    let mut c = UnixStream::connect(dir.sock()).unwrap();
    let e = lis.accept().unwrap_err();
    assert_eq!(e.kind(), io::ErrorKind::PermissionDenied);
    assert_eq!(
        c.read(&mut [0u8; 1]).unwrap(),
        0,
        "closed without ServerHello"
    );

    // 같은 UID는 받는다. getpeereid가 이 프로세스의 euid를 돌려준다.
    lis.uid = me;
    let c = UnixStream::connect(dir.sock()).unwrap();
    assert_eq!(sys::peer_uid(c.as_fd()).unwrap(), me);
    lis.accept().unwrap();
}

/// N-1 호환: v1 클라이언트의 고정 바이트(krill-proto `ipc/tests.rs`의 `CLIENT_HELLO_V1`)를 실제 소켓으로 받는다.
#[test]
fn accepts_frozen_v1_client_over_socket() {
    let dir = Dir::new("v1");
    let lis = Listener::bind(&dir.sock()).unwrap();
    let peer = client(dir.sock(), unhex("0f0000000201010001096d6b20ed959ceab880"));
    let mut conn = lis.accept().unwrap();
    let (hello_c, neg) = handshake(&mut conn, &hello()).unwrap();
    // 고정 바이트가 담은 이름 그대로다(Krill 개명과 무관하게 v1 바이트는 동결 — 바꾸면 N-1 회귀망이 무력화된다).
    assert_eq!(
        (hello_c.role, hello_c.name.as_str()),
        (ClientRole::Cli, "mk 한글")
    );
    assert_eq!(neg.proto, 1);
    drop(conn);
    peer.join().unwrap();
}

#[test]
fn rejects_oversize_frame() {
    let dir = Dir::new("big");
    let lis = Listener::bind(&dir.sock()).unwrap();
    let mut big = u32::try_from(MAX_FRAME + 1).unwrap().to_le_bytes().to_vec();
    big.push(0x02);
    let peer = client(dir.sock(), big);
    let mut conn = lis.accept().unwrap();
    let e = handshake(&mut conn, &hello()).unwrap_err();
    assert_eq!(e.kind(), io::ErrorKind::InvalidData);
    drop(conn);
    peer.join().unwrap();
}

#[test]
fn rejects_incompatible_client() {
    let dir = Dir::new("ver");
    let lis = Listener::bind(&dir.sock()).unwrap();
    let future = ClientHello {
        proto_min: PROTO_MAX + 1,
        proto_max: PROTO_MAX + 1,
        features: 0,
        role: ClientRole::App,
        name: String::new(),
    };
    let peer = client(dir.sock(), future.encode().unwrap());
    let mut conn = lis.accept().unwrap();
    let e = handshake(&mut conn, &hello()).unwrap_err();
    assert_eq!(e.kind(), io::ErrorKind::InvalidData);
    drop(conn);
    peer.join().unwrap();
}

#[test]
fn bind_permissions_and_path_limit() {
    let dir = Dir::new("perm");
    let _lis = Listener::bind(&dir.sock()).unwrap();
    assert_eq!(fs::metadata(&dir.0).unwrap().mode() & 0o777, 0o700);
    assert_eq!(fs::metadata(dir.sock()).unwrap().mode() & 0o777, 0o600);

    let loose = Dir::new("loose");
    fs::create_dir(&loose.0).unwrap();
    fs::set_permissions(&loose.0, Permissions::from_mode(0o755)).unwrap();
    let e = Listener::bind(&loose.sock()).unwrap_err();
    assert_eq!(e.kind(), io::ErrorKind::PermissionDenied);

    let long = dir.0.join("x".repeat(SUN_PATH_MAX));
    let e = Listener::bind(&long).unwrap_err();
    assert_eq!(e.kind(), io::ErrorKind::InvalidInput);
}

/// MK-022: 락을 쥔 동안 두 번째 bind는 `AddrInUse`이고 게시된 소켓을 건드리지 않는다. 락이 풀리면 죽은 소켓과
/// 남은 `.tmp`를 새 소켓으로 대체한다.
#[test]
fn single_instance_and_stale_publish() {
    let dir = Dir::new("one");
    let first = Listener::bind(&dir.sock()).unwrap();
    let e = Listener::bind(&dir.sock()).unwrap_err();
    assert_eq!(e.kind(), io::ErrorKind::AddrInUse);
    let _c = UnixStream::connect(dir.sock()).unwrap();
    first.accept().unwrap();

    // 이전 소유자가 rename 직전에 죽은 상황: 죽은 소켓 + 남은 .tmp.
    // 일반 `cargo test --lib`는 한 프로세스에서 다른 단위 테스트가 동시에 `fork`+`exec`으로 자식을 띄우므로,
    // `first`가 열려 있던 순간 `fork`된 자식이 `exec`에서 `FD_CLOEXEC` fd를 닫을 때까지 잠시 기다린다.
    drop(first);
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while UnixStream::connect(dir.sock()).is_ok() {
        assert!(std::time::Instant::now() < deadline, "stale socket");
        thread::sleep(Duration::from_millis(1));
    }
    fs::write(dir.0.join("k.sock.tmp"), b"").unwrap();
    let second = loop {
        match Listener::bind(&dir.sock()) {
            Err(e)
                if e.kind() == io::ErrorKind::AddrInUse && std::time::Instant::now() < deadline =>
            {
                thread::sleep(Duration::from_millis(1));
            }
            r => break r.unwrap(),
        }
    };
    let _c = UnixStream::connect(dir.sock()).unwrap();
    second.accept().unwrap();
    let mut names: Vec<_> = fs::read_dir(&dir.0)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    assert_eq!(names, ["k.sock", "k.sock.lock"]);
    assert_eq!(
        fs::metadata(dir.0.join("k.sock.lock")).unwrap().mode() & 0o777,
        0o600
    );
}

/// 클라이언트가 Hello를 보내자마자 닫아도 협상은 성공한다. macOS는 피어가 닫힌 소켓의 `setsockopt`를 EINVAL로
/// 거부해 타임아웃 해제에서 실패했다(OWE-487). 닫힘 시점이 경합이라 여러 번 돌린다.
#[test]
fn handshake_survives_client_closing_after_hello() {
    const N: usize = 20_000;
    let dir = Dir::new("close");
    let lis = Listener::bind(&dir.sock()).unwrap();
    let path = dir.sock();
    let reply = krill_proto::daemon::client_hello(ClientRole::Cli, "test")
        .encode()
        .unwrap();
    let peer = thread::spawn(move || {
        for _ in 0..N {
            let mut c = UnixStream::connect(&path).unwrap();
            ipc::read_frame(&mut c).unwrap();
            c.write_all(&reply).unwrap();
        }
    });
    for _ in 0..N {
        let mut conn = lis.accept().unwrap();
        handshake(&mut conn, &hello()).unwrap();
    }
    peer.join().unwrap();
}
