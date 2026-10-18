use super::*;
use krill_proto::control;
use krill_proto::daemon::client_hello;
use krill_proto::ipc::{ClientRole, FEATURES, PROTO_MAX, PROTO_MIN};
use krill_proto::screen::Msg;
use ring::digest;
use std::io::Read;

fn hello() -> ServerHello {
    ServerHello {
        proto_min: PROTO_MIN,
        proto_max: PROTO_MAX,
        features: FEATURES,
        build: "mobile-test".into(),
        instance_id: 1,
    }
}
fn start() -> (Arc<Registry>, Arc<Gateway>) {
    let r = Arc::new(Registry::new(None, None));
    r.start_mobile(hello()).unwrap();
    lock(&r.mobile_gateway().unwrap().shared.state).tunnel_start =
        super::super::tunnel::Tunnel::fixture;
    let gateway = Arc::clone(r.mobile_gateway().unwrap());
    (r, gateway)
}
fn offer(g: &Gateway) -> (u16, krill_proto::mobile::PairingTicket) {
    let reply = g.request(MobileRequest::MintPairingTicket {
        mode: ConnectMode::Tunnel,
        custom_address: None,
        rotate: true,
    });
    let MobileReply::PairingOffer { port, url, .. } = reply else {
        panic!("no offer");
    };
    assert!(url.contains("#key="));
    let t = lock(&g.shared.state)
        .devices
        .ticket(now_ms())
        .unwrap()
        .clone();
    (port, t)
}
fn send(stream: &mut TcpStream, opcode: u8, body: &[u8]) {
    let mut frame = Vec::new();
    ws::write(&mut frame, opcode, body).unwrap();
    let header = match frame.get(1).unwrap() {
        126 => 4,
        127 => 10,
        _ => 2,
    };
    *frame.get_mut(1).unwrap() |= 0x80;
    let mask = [7, 8, 9, 10];
    let masked: Vec<_> = body
        .iter()
        .zip(mask.into_iter().cycle())
        .map(|(b, k)| b ^ k)
        .collect();
    stream.write_all(frame.get(..header).unwrap()).unwrap();
    stream.write_all(&mask).unwrap();
    stream.write_all(&masked).unwrap();
}
fn receive(stream: &mut TcpStream) -> io::Result<(u8, Vec<u8>)> {
    let mut h = [0; 2];
    stream.read_exact(&mut h)?;
    let [opcode, length] = h;
    let len = match length {
        126 => {
            let mut b = [0; 2];
            stream.read_exact(&mut b)?;
            usize::from(u16::from_be_bytes(b))
        }
        127 => {
            let mut b = [0; 8];
            stream.read_exact(&mut b)?;
            usize::try_from(u64::from_be_bytes(b)).unwrap()
        }
        n => usize::from(n),
    };
    assert!(len < 2 * 1024 * 1024);
    let mut body = vec![0; len];
    stream.read_exact(&mut body)?;
    Ok((opcode & 0x0f, body))
}
fn websocket(port: u16) -> (TcpStream, [u8; 32]) {
    let mut s = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    s.set_write_timeout(Some(Duration::from_secs(5))).unwrap();
    s.write_all(b"GET /ws HTTP/1.1\r\nHost: localhost\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\r\n").unwrap();
    let mut header = Vec::new();
    while !header.ends_with(b"\r\n\r\n") {
        let mut b = [0];
        s.read_exact(&mut b).unwrap();
        header.extend(b);
    }
    assert!(
        String::from_utf8(header)
            .unwrap()
            .starts_with("HTTP/1.1 101")
    );
    let (op, body) = receive(&mut s).unwrap();
    assert_eq!(op, ws::TEXT);
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let challenge = decode_hex(json.get("challenge").unwrap().as_str().unwrap()).unwrap();
    (s, challenge)
}
struct Client {
    socket: TcpStream,
    channel: Channel,
    device: String,
    credential: [u8; 32],
}
impl Client {
    fn pair(port: u16, t: &krill_proto::mobile::PairingTicket) -> Self {
        Self::auth(port, Some(t.token), None, &t.secret)
    }
    fn reconnect(port: u16, id: &str, credential: &[u8; 32]) -> Self {
        let hash = digest::digest(&digest::SHA256, credential);
        Self::auth(
            port,
            None,
            Some(id.to_owned()),
            &hash.as_ref().try_into().unwrap(),
        )
    }
    fn auth(
        port: u16,
        token: Option<[u8; 32]>,
        device_id: Option<String>,
        secret: &[u8; 32],
    ) -> Self {
        let (mut s, challenge) = websocket(port);
        let nonce = random().unwrap();
        let mut salt = [0; 64];
        for (a, b) in salt.iter_mut().zip(challenge.into_iter().chain(nonce)) {
            *a = b;
        }
        let mut channel = Channel::new(secret, &salt, Endpoint::Client).unwrap();
        send(
            &mut s,
            ws::TEXT,
            &serde_json::to_vec(&AuthHello {
                token: token.map(|v| hex(&v)),
                device_id,
                client_nonce: hex(&nonce),
            })
            .unwrap(),
        );
        send(
            &mut s,
            ws::BINARY,
            &channel
                .seal(
                    &serde_json::to_vec(&AuthProof {
                        label: "phone".into(),
                    })
                    .unwrap(),
                )
                .unwrap(),
        );
        let (_, body) = receive(&mut s).unwrap();
        let a: Authenticated = serde_json::from_slice(&channel.open(&body).unwrap()).unwrap();
        let credential = a
            .credential
            .as_deref()
            .map(decode_hex::<32>)
            .transpose()
            .unwrap()
            .unwrap_or([0; 32]);
        Self {
            socket: s,
            channel,
            device: a.device_id,
            credential,
        }
    }
    fn read(&mut self) -> io::Result<(u8, Vec<u8>)> {
        let (op, body) = receive(&mut self.socket)?;
        assert_eq!(op, ws::BINARY);
        let plain = self.channel.open(&body)?;
        ipc::read_frame(&mut plain.as_slice())?.ok_or_else(invalid)
    }
    fn write(&mut self, frame: &[u8]) -> Vec<u8> {
        let sealed = self.channel.seal(frame).unwrap();
        send(&mut self.socket, ws::BINARY, &sealed);
        sealed
    }
    fn ipc(&mut self) {
        let (k, b) = self.read().unwrap();
        ServerHello::decode(k, &b).unwrap();
        self.write(
            &client_hello(ClientRole::App, "phone-test")
                .encode()
                .unwrap(),
        );
    }
}
impl Drop for Client {
    fn drop(&mut self) {
        self.socket.shutdown(Shutdown::Both).ok();
    }
}
fn until(mut f: impl FnMut() -> bool) {
    let end = std::time::Instant::now() + Duration::from_secs(5);
    while !f() {
        assert!(std::time::Instant::now() < end);
        thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn lazy_binding_expiry_and_mode_rotation() {
    let (_r, g) = start();
    assert!(lock(&g.shared.state).listeners.is_empty());
    let (port, first) = offer(&g);
    assert!(TcpStream::connect((Ipv4Addr::LOCALHOST, port)).is_ok());
    let s = lock(&g.shared.state);
    assert_eq!(s.listeners.len(), 1);
    assert!(
        s.listeners
            .iter()
            .all(|l| l.local_addr().unwrap().ip().is_loopback())
    );
    drop(s);
    // 설정 변경은 이전 QR을 즉시 폐기하고 새로운 티켓을 발급한다.
    g.request(MobileRequest::SetConnectMode {
        mode: ConnectMode::Tunnel,
        custom_address: Some("127.0.0.1:0".into()),
    });
    assert!(
        lock(&g.shared.state)
            .devices
            .pairing_secret(&first.token, now_ms())
            .is_err()
    );
    let mut s = lock(&g.shared.state);
    s.devices.clear_ticket();
    reconcile(&mut s, now_ms()).unwrap();
    assert!(s.listeners.is_empty());
    drop(s);
    assert!(TcpStream::connect((Ipv4Addr::LOCALHOST, port)).is_err());
    let (port, ticket) = offer(&g);
    let mut s = lock(&g.shared.state);
    reconcile(&mut s, ticket.expires_at_ms).unwrap();
    assert!(s.listeners.is_empty());
    drop(s);
    assert!(TcpStream::connect((Ipv4Addr::LOCALHOST, port)).is_err());
}
#[test]
fn authenticated_ws_pair_bridge_reconnect_replay_and_revoke() {
    let (_r, g) = start();
    let (port, t) = offer(&g);
    let mut phone = Client::pair(port, &t);
    phone.ipc();
    let sealed = phone.write(&control::Request::List.encode().unwrap());
    let (k, b) = phone.read().unwrap();
    assert_eq!(
        control::Reply::decode(k, &b).unwrap(),
        control::Reply::Sessions(Vec::new())
    );
    // 미수정 IPC가 여러 프레임에 걸쳐 재사용된다.
    phone.write(&control::Request::List.encode().unwrap());
    phone.read().unwrap();
    send(&mut phone.socket, ws::BINARY, &sealed);
    assert!(receive(&mut phone.socket).is_err());
    until(|| lock(&g.shared.state).connections.is_empty());
    let mut reconnect = Client::reconnect(port, &phone.device, &phone.credential);
    reconnect.ipc();
    reconnect.write(&control::Request::List.encode().unwrap());
    reconnect.read().unwrap();
    assert_eq!(
        g.request(MobileRequest::RevokeDevice {
            device_id: Some(phone.device.clone()),
            all: false
        }),
        MobileReply::Revoked { count: 1 }
    );
    assert!(receive(&mut reconnect.socket).is_err());
    assert!(lock(&g.shared.state).listeners.is_empty());
    assert!(TcpStream::connect((Ipv4Addr::LOCALHOST, port)).is_err());
}
#[test]
fn revoke_all_closes_pending_unauthenticated_connections() {
    let (_r, g) = start();
    let (port, _) = offer(&g);
    let (mut pending, _) = websocket(port);
    g.request(MobileRequest::RevokeDevice {
        device_id: None,
        all: true,
    });
    assert!(receive(&mut pending).is_err());
    assert!(lock(&g.shared.state).listeners.is_empty());
}
#[test]
fn local_control_feature_and_observer_gates() {
    let (r, _g) = start();
    let (mut client, server) = UnixStream::pair().unwrap();
    let worker = thread::spawn(move || r.serve(server, &hello()).unwrap());
    ipc::read_frame(&mut client).unwrap();
    client
        .write_all(
            &client_hello(ClientRole::App, "mobile-admin")
                .encode()
                .unwrap(),
        )
        .unwrap();
    client
        .write_all(
            &krill_proto::roles::Request::Set(krill_proto::roles::Role::Observer)
                .encode()
                .unwrap(),
        )
        .unwrap();
    ipc::read_frame(&mut client).unwrap();
    client
        .write_all(&MobileRequest::ListDevices.encode().unwrap())
        .unwrap();
    let (k, b) = ipc::read_frame(&mut client).unwrap().unwrap();
    assert_eq!(
        MobileReply::decode(k, &b).unwrap(),
        MobileReply::Devices(Vec::new())
    );
    client
        .write_all(
            &MobileRequest::MintPairingTicket {
                mode: ConnectMode::Tunnel,
                custom_address: None,
                rotate: true,
            }
            .encode()
            .unwrap(),
        )
        .unwrap();
    let (k, b) = ipc::read_frame(&mut client).unwrap().unwrap();
    assert!(matches!(
        MobileReply::decode(k, &b).unwrap(),
        MobileReply::Failed(_)
    ));
    client
        .write_all(
            &MobileRequest::Workflow(
                "{\"action\":\"prompt\",\"session\":1,\"text\":\"denied\"}".into(),
            )
            .encode()
            .unwrap(),
        )
        .unwrap();
    let (kind, body) = ipc::read_frame(&mut client).unwrap().unwrap();
    assert!(matches!(
        MobileReply::decode(kind, &body).unwrap(),
        MobileReply::Failed(_)
    ));
    drop(client);
    worker.join().unwrap();
}

#[test]
fn individual_revocation_preserves_other_device_and_listener() {
    let (_r, g) = start();
    let (port, first) = offer(&g);
    let mut a = Client::pair(port, &first);
    a.ipc();
    let (_, second) = offer(&g);
    let mut b = Client::pair(port, &second);
    b.ipc();
    assert_eq!(lock(&g.shared.state).devices.active_count(), 2);
    g.request(MobileRequest::RevokeDevice {
        device_id: Some(a.device.clone()),
        all: false,
    });
    assert!(receive(&mut a.socket).is_err());
    b.write(&control::Request::List.encode().unwrap());
    b.read().unwrap();
    assert!(!lock(&g.shared.state).listeners.is_empty());
    g.request(MobileRequest::RevokeDevice {
        device_id: None,
        all: true,
    });
    assert!(receive(&mut b.socket).is_err());
    assert!(lock(&g.shared.state).listeners.is_empty());
}

#[test]
fn wrong_e2ee_proof_never_consumes_ticket_and_is_rate_limited() {
    let (_r, g) = start();
    let (port, t) = offer(&g);
    for _ in 0..5 {
        let (mut socket, challenge) = websocket(port);
        let nonce = random().unwrap();
        let mut salt = [0; 64];
        for (a, b) in salt.iter_mut().zip(challenge.into_iter().chain(nonce)) {
            *a = b;
        }
        let mut channel = Channel::new(&[0; 32], &salt, Endpoint::Client).unwrap();
        send(
            &mut socket,
            ws::TEXT,
            &serde_json::to_vec(&AuthHello {
                token: Some(hex(&t.token)),
                device_id: None,
                client_nonce: hex(&nonce),
            })
            .unwrap(),
        );
        send(
            &mut socket,
            ws::BINARY,
            &channel.seal(b"{\"label\":\"attacker\"}").unwrap(),
        );
        assert_eq!(
            receive(&mut socket).unwrap(),
            (8, 1008_u16.to_be_bytes().to_vec())
        );
        assert!(receive(&mut socket).is_err());
    }
    let s = lock(&g.shared.state);
    assert_eq!(s.devices.active_count(), 0);
    assert!(s.devices.ticket(now_ms()).is_some());
    drop(s);
    let mut socket = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).unwrap();
    socket
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    socket.write_all(b"GET /ws HTTP/1.1\r\nHost: localhost\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\r\n").unwrap();
    let mut bytes = Vec::new();
    socket.read_to_end(&mut bytes).unwrap();
    assert!(String::from_utf8(bytes).unwrap().ends_with("\r\n\r\n"));
}

#[test]
fn unnegotiated_mobile_feature_is_rejected() {
    let (r, _g) = start();
    let (mut client, server) = UnixStream::pair().unwrap();
    let worker = thread::spawn(move || r.serve(server, &hello()).unwrap());
    ipc::read_frame(&mut client).unwrap();
    let mut h = client_hello(ClientRole::App, "legacy");
    h.features &= !krill_proto::mobile::FEAT_MOBILE;
    client.write_all(&h.encode().unwrap()).unwrap();
    client
        .write_all(&MobileRequest::Status.encode().unwrap())
        .unwrap();
    assert!(ipc::read_frame(&mut client).unwrap().is_none());
    drop(client);
    worker.join().unwrap();
}

#[test]
fn ws_attach_input_and_resize_preserve_real_desktop_pty() {
    use crate::holder::{Holder, Kept};
    use krill_proto::{
        holder::{FromHolder, VERSION},
        input::{Input, InputAck, InputId, InputResult},
        roles::{self, Role},
        screen::{Attach, Seq, Stream},
    };
    use std::os::fd::OwnedFd;
    use std::process::{Child, Command, Stdio};
    struct Running(Child);
    impl Drop for Running {
        fn drop(&mut self) {
            self.0.kill().ok();
            self.0.wait().ok();
        }
    }
    let pty = rustix_openpty::openpty(
        None,
        Some(&rustix::termios::Winsize {
            ws_row: 40,
            ws_col: 120,
            ws_xpixel: 0,
            ws_ypixel: 0,
        }),
    )
    .unwrap();
    let mut child = Running(
        Command::new("/bin/sh")
            .args(["-c", "stty -echo; printf 'READY\\n'; cat"])
            .stdin(Stdio::from(pty.user.try_clone().unwrap()))
            .stdout(Stdio::from(pty.user.try_clone().unwrap()))
            .stderr(Stdio::from(pty.user))
            .spawn()
            .unwrap(),
    );
    let master = pty.controller.try_clone().unwrap();
    let (daemon, mut held) = UnixStream::pair().unwrap();
    for m in [
        FromHolder::Hello {
            version: VERSION,
            holder_pid: 1,
        },
        FromHolder::Ready,
    ] {
        held.write_all(&m.encode().unwrap()).unwrap();
    }
    let (holder, _link, _, _) = Holder::connect(OwnedFd::from(daemon)).unwrap();
    let id = krill_proto::SessionId(122);
    let r = Arc::new(Registry::new(
        Some((
            holder,
            vec![Kept {
                id,
                pid: child.0.id(),
                status: None,
                master: pty.controller,
            }],
        )),
        None,
    ));
    r.start_mobile(hello()).unwrap();
    lock(&r.mobile_gateway().unwrap().shared.state).tunnel_start =
        super::super::tunnel::Tunnel::fixture;
    let g = Arc::clone(r.mobile_gateway().unwrap());
    let (mut desktop, server) = UnixStream::pair().unwrap();
    desktop
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let registry = Arc::clone(&r);
    let worker = thread::spawn(move || registry.serve(server, &hello()).ok());
    ipc::read_frame(&mut desktop).unwrap();
    desktop
        .write_all(&client_hello(ClientRole::App, "desktop").encode().unwrap())
        .unwrap();
    desktop
        .write_all(
            &Attach {
                session: id,
                stream: Stream::Raw { from: Seq(0) },
            }
            .encode()
            .unwrap(),
        )
        .unwrap();

    desktop_frame(&mut desktop, krill_proto::screen::kind::ATTACHED);
    desktop
        .write_all(&roles::Request::Focus.encode().unwrap())
        .unwrap();
    desktop_frame(&mut desktop, roles::kind::ROLE_ACK);
    let (port, t) = offer(&g);
    let mut phone = Client::pair(port, &t);
    phone.ipc();
    // 구 모바일 클라이언트가 Interactive를 명시해도 Resize 전에는 크기 소유권을 주지 않는다.
    phone.write(&roles::Request::Set(Role::Interactive).encode().unwrap());
    phone.read().unwrap();
    phone.write(
        &Attach {
            session: id,
            stream: Stream::Raw { from: Seq(0) },
        }
        .encode()
        .unwrap(),
    );

    phone_frame(&mut phone, krill_proto::screen::kind::ATTACHED);
    phone.write(
        &Input {
            id: InputId(1),
            bytes: b"approve\n".to_vec(),
        }
        .encode()
        .unwrap(),
    );
    let ack = phone_frame(&mut phone, krill_proto::input::kind::INPUT_ACK);
    assert_eq!(
        InputAck::decode(krill_proto::input::kind::INPUT_ACK, &ack)
            .unwrap()
            .result,
        InputResult::Applied
    );
    let size = rustix::termios::tcgetwinsize(&master).unwrap();
    assert_eq!((size.ws_col, size.ws_row), (120, 40));
    phone.write(
        &roles::Request::Resize { cols: 40, rows: 20 }
            .encode()
            .unwrap(),
    );
    phone_frame(&mut phone, roles::kind::ROLE_ACK);
    let size = rustix::termios::tcgetwinsize(&master).unwrap();
    assert_eq!((size.ws_col, size.ws_row), (40, 20));
    g.request(MobileRequest::RevokeDevice {
        device_id: None,
        all: true,
    });
    until(|| {
        let size = rustix::termios::tcgetwinsize(&master).unwrap();
        (size.ws_col, size.ws_row) == (120, 40)
    });
    assert!(child.0.try_wait().unwrap().is_none());
    desktop.shutdown(Shutdown::Both).ok();
    drop(desktop);
    worker.join().unwrap();
}

fn desktop_frame(s: &mut UnixStream, kind: u8) -> Vec<u8> {
    loop {
        let (k, b) = ipc::read_frame(s).unwrap().unwrap();
        if k == kind {
            return b;
        }
    }
}

fn phone_frame(c: &mut Client, kind: u8) -> Vec<u8> {
    loop {
        let (k, b) = c.read().unwrap();
        if k == kind {
            return b;
        }
    }
}

#[test]
fn notification_deep_link_exists_only_while_gateway_is_exposed_and_has_no_secret() {
    let (_registry, gateway) = start();
    let id = krill_proto::SessionId(42);
    assert!(gateway.deep_link(id).is_none());
    let (_port, _ticket) = offer(&gateway);
    let link = gateway.deep_link(id).unwrap();
    assert!(link.starts_with("https://"));
    assert!(link.ends_with("/?session=42"));
    assert!(!link.contains("pair="));
    assert!(!link.contains("key="));
    gateway.request(MobileRequest::RevokeDevice {
        device_id: None,
        all: true,
    });
    assert!(gateway.deep_link(id).is_none());
}
#[test]
fn connect_mode_survives_daemon_restart() {
    let dir = std::env::temp_dir().join(format!("mk-mobile-intent-{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    let g = Gateway::start(Weak::new(), Some(&dir), hello()).unwrap();
    assert_eq!(lock(&g.shared.state).mode, ConnectMode::Lan);
    g.request(MobileRequest::SetConnectMode {
        mode: ConnectMode::Tunnel,
        custom_address: None,
    });
    drop(g);
    let g = Gateway::start(Weak::new(), Some(&dir), hello()).unwrap();
    assert_eq!(lock(&g.shared.state).mode, ConnectMode::Tunnel);
    drop(g);
    // 손상된 파일은 모바일을 끄지 않고 LAN으로 시작한다.
    std::fs::write(dir.join(INTENT_FILE), b"{").unwrap();
    let g = Gateway::start(Weak::new(), Some(&dir), hello()).unwrap();
    assert_eq!(lock(&g.shared.state).mode, ConnectMode::Lan);
    drop(g);
    std::fs::remove_dir_all(dir).unwrap();
}
#[test]
fn manual_loopback_bind_survives_restart_with_multiple_interfaces() {
    let dir = std::env::temp_dir().join(format!("mk-mobile-loopback-{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    let r = Arc::new(Registry::new(None, None));
    let g = Gateway::start(Arc::downgrade(&r), Some(&dir), hello()).unwrap();
    let MobileReply::PairingOffer { port, .. } = g.request(MobileRequest::MintPairingTicket {
        mode: ConnectMode::Lan,
        custom_address: Some("127.0.0.1:0".into()),
        rotate: true,
    }) else {
        panic!("expected loopback offer");
    };
    let ticket = lock(&g.shared.state)
        .devices
        .ticket(now_ms())
        .unwrap()
        .clone();
    let mut phone = Client::pair(port, &ticket);
    phone.ipc();
    let (device, credential) = (phone.device.clone(), phone.credential);
    drop(phone);
    drop(g);

    // Status·SetConnectMode 호출 전 최초 reconcile에서도 수동 loopback만 바인딩해야 한다.
    let g = Gateway::start(Arc::downgrade(&r), Some(&dir), hello()).unwrap();
    until(|| !lock(&g.shared.state).listeners.is_empty());
    let mut s = lock(&g.shared.state);
    assert_eq!(s.mode, ConnectMode::Lan);
    assert_eq!(s.custom.as_deref(), Some("127.0.0.1:0"));
    assert_eq!(s.selected.unwrap().ip(), &Ipv4Addr::LOCALHOST);
    assert_eq!(s.listeners.len(), 1);
    assert_eq!(
        s.listeners[0].local_addr().unwrap().ip(),
        Ipv4Addr::LOCALHOST
    );
    s.interfaces = vec![
        NetworkInterface {
            name: "en0".into(),
            address: "10.1.1.2".into(),
            default_route: true,
        },
        NetworkInterface {
            name: "en1".into(),
            address: "192.168.1.2".into(),
            default_route: false,
        },
        NetworkInterface {
            name: "utun5".into(),
            address: "100.64.0.2".into(),
            default_route: false,
        },
    ];
    close(&mut s);
    s.selected = None;
    reconcile(&mut s, now_ms()).unwrap();
    assert_eq!(s.selected.unwrap().ip(), &Ipv4Addr::LOCALHOST);
    assert_eq!(s.listeners.len(), 1);
    assert_eq!(
        s.listeners[0].local_addr().unwrap().ip(),
        Ipv4Addr::LOCALHOST
    );
    let restarted_port = s.selected.unwrap().port();
    drop(s);
    let mut reconnected = Client::reconnect(restarted_port, &device, &credential);
    reconnected.ipc();
    drop(reconnected);
    drop(g);
    std::fs::remove_dir_all(dir).unwrap();
}
#[test]
fn supervisor_replaces_unreachable_tunnel_with_backoff() {
    let (_r, g) = start();
    offer(&g);
    let mut s = lock(&g.shared.state);
    let port = s.selected.unwrap().port();
    s.tunnel = Some(super::super::tunnel::Tunnel::stale(port).unwrap());
    reconcile(&mut s, now_ms()).unwrap();
    assert!(s.tunnel.is_none());
    assert_eq!(s.tunnel_failures, 1);
    assert!(s.tunnel_retry_at.is_some());
    // 백오프가 지나면 새 터널을 띄우고, 준비되면 실패 횟수를 되돌린다.
    s.tunnel_retry_at = None;
    reconcile(&mut s, now_ms()).unwrap();
    assert!(s.tunnel.as_ref().unwrap().ready_url().is_some());
    reconcile(&mut s, now_ms()).unwrap();
    assert_eq!(s.tunnel_failures, 0);
}
#[test]
fn idle_authenticated_socket_is_pinged_then_closed_without_pong() {
    let (_r, g) = start();
    lock(&g.shared.state).ping = Duration::from_millis(100);
    let (port, t) = offer(&g);
    let mut phone = Client::pair(port, &t);
    phone.ipc();
    let (op, body) = receive(&mut phone.socket).unwrap();
    assert_eq!((op, body.len()), (9, 0));
    // pong 없이 IDLE_PINGS 주기가 지나면 죽은 연결로 보고 닫는다.
    loop {
        match receive(&mut phone.socket) {
            Ok((9, _)) => {}
            Ok(other) => panic!("unexpected frame {other:?}"),
            Err(_) => break,
        }
    }
    until(|| lock(&g.shared.state).connections.is_empty());
}
