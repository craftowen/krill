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
        Some(super::super::tunnel::Tunnel::fixture);
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
    assert!(url.contains("#pair="));
    assert!(url.contains("&key="));
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

#[cfg(unix)]
#[test]
fn ws_attach_input_and_resize_preserve_real_desktop_pty() {
    use crate::holder::{Holder, Kept};
    use crate::os::OwnedFd;
    use krill_proto::{
        holder::{FromHolder, VERSION},
        input::{Input, InputAck, InputId, InputResult},
        roles::{self, Role},
        screen::{Attach, Seq, Stream},
    };
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
            inbox: false,
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
        Some(super::super::tunnel::Tunnel::fixture);
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

#[cfg(unix)]
fn desktop_frame(s: &mut UnixStream, kind: u8) -> Vec<u8> {
    loop {
        let (k, b) = ipc::read_frame(s).unwrap().unwrap();
        if k == kind {
            return b;
        }
    }
}

#[cfg(unix)]
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

#[test]
fn ws_closes_when_registry_ends_before_first_request() {
    let (_r, g) = start();
    let (port, t) = offer(&g);
    let mut phone = Client::pair(port, &t);
    phone.ipc();
    // 첫 요청 없이 ATTACH_TIMEOUT이 지나면 레지스트리가 끝낸다. WS도 닫혀야 하며 응답 없이 멈추면 안 된다.
    phone
        .socket
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let started = Instant::now();
    loop {
        match receive(&mut phone.socket) {
            Ok((9, _)) => {}
            Ok((8, _)) => break,
            Ok(other) => panic!("unexpected frame {other:?}"),
            Err(e) => {
                assert!(
                    !matches!(
                        e.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                    ),
                    "WS가 닫히지 않고 멈춤: {e}"
                );
                break;
            }
        }
    }
    assert!(started.elapsed() < Duration::from_secs(9));
    until(|| lock(&g.shared.state).connections.is_empty());
}

#[test]
fn fixed_offer_deep_link_and_fallback_follow_shared_contract() {
    let (_registry, gateway) = start();
    let (port, ticket) = offer(&gateway);
    {
        let mut state = lock(&gateway.shared.state);
        state.tunnel = Some(super::super::tunnel::Tunnel::fixed_fixture(port).unwrap());
        state.publication.origin = "https://stage.test".into();
    }
    let state = lock(&gateway.shared.state);
    let MobileReply::PairingOffer { url, .. } = pairing_offer(&state, now_ms()).unwrap() else {
        panic!("페어링 응답 없음");
    };
    assert_eq!(
        url,
        format!(
            "https://stage.test/m/{}/#pair={}&key={}&t=fixture-mobile.trycloudflare.com&r={}",
            super::super::pwa::asset_hash(),
            hex(&ticket.token),
            ws::base64(&ticket.secret, true),
            state.publication.rid
        )
    );
    drop(state);
    assert_eq!(
        gateway.deep_link(krill_proto::SessionId(7)).unwrap(),
        format!(
            "https://stage.test/m/{}/?session=7",
            super::super::pwa::asset_hash()
        )
    );
    let mut state = lock(&gateway.shared.state);
    state.tunnel = Some(super::super::tunnel::Tunnel::fixture(port).unwrap());
    for mode in [
        ConnectMode::Tunnel,
        ConnectMode::Lan,
        ConnectMode::Tailscale,
    ] {
        state.mode = mode;
        let MobileReply::PairingOffer { url, .. } = pairing_offer(&state, now_ms()).unwrap() else {
            panic!("페어링 응답 없음");
        };
        assert!(url.contains("/#pair="));
        assert!(url.contains("&key="));
        assert!(!url.contains("?pair="));
        assert!(!url.contains("&r="));
    }
    state.mode = ConnectMode::Tunnel;
    // 준비 확인 전 터널은 고정 출처를 쓰지 않고 찍힌 터널 URL로 강등한다.
    state.tunnel = Some(super::super::tunnel::Tunnel::stale(port).unwrap());
    let MobileReply::PairingOffer { url, .. } = pairing_offer(&state, now_ms()).unwrap() else {
        panic!("페어링 응답 없음");
    };
    assert!(url.starts_with("https://fixture-mobile.trycloudflare.com/#pair="));
    state.tunnel = None;
    assert!(pairing_offer(&state, now_ms()).is_err());
}
#[test]
#[cfg(unix)]
fn rendezvous_credentials_migrate_old_intent_and_survive_mode_changes() {
    use std::os::unix::fs::PermissionsExt;
    let dir = std::env::temp_dir().join(format!("mk-mobile-migrate-{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).unwrap();
    // 열거형의 실제 직렬화 표기를 사용해 구버전 파일을 만든다.
    let legacy = serde_json::json!({"mode":ConnectMode::Tunnel,"custom":null});
    super::super::devices::write_private(
        &dir.join(INTENT_FILE),
        &serde_json::to_vec(&legacy).unwrap(),
    )
    .unwrap();
    let first = Intent::load(Some(&dir)).unwrap();
    assert_eq!(first.mode, ConnectMode::Tunnel);
    assert_eq!(first.rid.len(), 32);
    assert_eq!(first.key.len(), 64);
    let gateway = Gateway::start(Weak::new(), Some(&dir), hello()).unwrap();
    gateway.request(MobileRequest::SetConnectMode {
        mode: ConnectMode::Lan,
        custom_address: None,
    });
    drop(gateway);
    let second = Intent::load(Some(&dir)).unwrap();
    assert_eq!(second.mode, ConnectMode::Lan);
    assert_eq!((first.rid, first.key), (second.rid, second.key));
    assert_eq!(
        std::fs::metadata(dir.join(INTENT_FILE))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn mint_waits_for_pending_download_without_holding_state_lock() {
    fn downloading(_: u16) -> io::Result<super::super::tunnel::Tunnel> {
        Err(io::Error::new(
            io::ErrorKind::WouldBlock,
            "cloudflared 내려받는 중",
        ))
    }
    let (_registry, gateway) = start();
    lock(&gateway.shared.state).tunnel_start = Some(downloading);
    let worker_gateway = Arc::clone(&gateway);
    let (send, receive) = std::sync::mpsc::channel();
    let worker = thread::spawn(move || {
        send.send(worker_gateway.request(MobileRequest::MintPairingTicket {
            mode: ConnectMode::Tunnel,
            custom_address: None,
            rotate: true,
        }))
        .unwrap();
    });
    let deadline = Instant::now() + Duration::from_secs(5);
    while lock(&gateway.shared.state)
        .devices
        .ticket(now_ms())
        .is_none()
        && Instant::now() < deadline
    {
        thread::sleep(Duration::from_millis(10));
    }
    assert!(
        lock(&gateway.shared.state)
            .devices
            .ticket(now_ms())
            .is_some()
    );
    assert!(receive.try_recv().is_err());
    {
        let mut state = lock(&gateway.shared.state);
        assert!(state.tunnel.is_none());
        assert_eq!(state.tunnel_failures, 0);
        state.tunnel_start = Some(super::super::tunnel::Tunnel::fixture);
        reconcile(&mut state, now_ms()).unwrap();
    }
    gateway.shared.wake();
    assert!(matches!(
        receive.recv_timeout(Duration::from_secs(5)).unwrap(),
        MobileReply::PairingOffer { .. }
    ));
    worker.join().unwrap();
}

#[test]
#[cfg(unix)]
fn closing_listener_waits_for_polling_accept_thread_to_release_its_fd() {
    let (_r, g) = start();
    let (port, _) = offer(&g);
    // 수락 스레드가 락 밖에서 리스너 복제 fd로 poll 중일 때 닫는다(깨우기 호출 없이).
    until(|| *lock(&g.shared.waker.polling));
    let mut s = lock(&g.shared.state);
    s.devices.clear_ticket();
    reconcile(&mut s, now_ms()).unwrap();
    assert!(!*lock(&g.shared.waker.polling));
    drop(s);
    assert!(TcpStream::connect((Ipv4Addr::LOCALHOST, port)).is_err());
    // 다시 열면 깨우기로 바로 수락한다.
    let (port, t) = offer(&g);
    Client::pair(port, &t).ipc();
}

#[test]
fn do_relay_daemon_challenge_pair_reconnect_concurrent_sessions_replay_and_1008_isolation() {
    use super::super::relay::{MUX_BINARY, MUX_CLOSE, MUX_OPEN, MUX_TEXT, decode_mux, encode_mux};
    fn tunnel_down(_: u16) -> io::Result<super::super::tunnel::Tunnel> {
        Err(io::Error::other("cloudflared unavailable"))
    }

    let relay_listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let relay_port = relay_listener.local_addr().unwrap().port();
    let (_r, g) = start();
    let (rid, rkey) = {
        let mut s = lock(&g.shared.state);
        s.publication.origin = format!("http://127.0.0.1:{relay_port}");
        s.relay_enabled = true;
        // cloudflared가 실패해도 DO Relay가 연결되면 순수 릴레이 QR을 즉시 발급한다.
        s.tunnel_start = Some(tunnel_down);
        (s.publication.rid.clone(), s.publication.key.clone())
    };

    let rid_srv = rid.clone();
    let server_handle = thread::spawn(move || {
        let (mut host_ws, _) = relay_listener.accept().unwrap();
        host_ws
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        host_ws
            .set_write_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut req = Vec::new();
        while !req.ends_with(b"\r\n\r\n") {
            let mut b = [0_u8; 1];
            host_ws.read_exact(&mut b).unwrap();
            req.extend_from_slice(&b);
        }
        let headers = String::from_utf8(req).unwrap();
        assert!(headers.starts_with(&format!(
            "GET /r/{rid_srv}?hash={}",
            super::super::pwa::asset_hash()
        )));
        assert!(headers.contains(&format!("Authorization: Bearer {rkey}\r\n")));
        let ws_key = headers
            .lines()
            .find_map(|l| l.strip_prefix("Sec-WebSocket-Key: "))
            .unwrap()
            .trim();
        let mut accept_in = Vec::from(ws_key.as_bytes());
        accept_in.extend_from_slice(b"258EAFA5-E914-47DA-95CA-C5AB0DC85B11");
        let accept = ws::base64(
            digest::digest(&digest::SHA1_FOR_LEGACY_USE_ONLY, &accept_in).as_ref(),
            false,
        );
        write!(
            host_ws,
            "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\n\r\n"
        )
        .unwrap();
        host_ws
    });

    let reply = g.request(MobileRequest::MintPairingTicket {
        mode: ConnectMode::Tunnel,
        custom_address: None,
        rotate: true,
    });
    let MobileReply::PairingOffer { url, .. } = reply else {
        panic!("expected pure DO relay offer, got {reply:?}");
    };
    let mut host_ws = server_handle.join().unwrap();
    let ticket = lock(&g.shared.state)
        .devices
        .ticket(now_ms())
        .unwrap()
        .clone();
    assert_eq!(
        url,
        format!(
            "http://127.0.0.1:{relay_port}/m/{}/#pair={}&key={}&r={rid}",
            super::super::pwa::asset_hash(),
            hex(&ticket.token),
            ws::base64(&ticket.secret, true)
        )
    );

    let read_mux_for = |host_ws: &mut TcpStream, target_conn: u32| -> (u8, Vec<u8>) {
        loop {
            let (op, body) = ws::read(host_ws).unwrap();
            assert_eq!(op, ws::BINARY);
            let (kind, cid, payload) = decode_mux(&body).unwrap();
            if cid == target_conn {
                return (kind, payload.to_vec());
            }
        }
    };
    // DO는 빈 MUX_OPEN만 보내고, 챌린지는 데몬이 만들어 MUX_TEXT로 클라이언트에게 내려보낸다.
    let open_conn = |host_ws: &mut TcpStream, cid: u32| -> [u8; 32] {
        ws::write(host_ws, ws::BINARY, &encode_mux(MUX_OPEN, cid, &[])).unwrap();
        let (kind, text) = read_mux_for(host_ws, cid);
        assert_eq!(kind, MUX_TEXT);
        let hello: serde_json::Value = serde_json::from_slice(&text).unwrap();
        assert_eq!(hello["version"], 2);
        decode_hex::<32>(hello["challenge"].as_str().unwrap()).unwrap()
    };

    // 1. conn_id = 1: 데몬 챌린지를 받은 뒤 AuthHello(MUX_TEXT) + AuthProof(MUX_BINARY) 파이프라인 페어링
    let challenge_1 = open_conn(&mut host_ws, 1);
    let nonce_1 = [0x22_u8; 32];
    let mut salt_1 = [0_u8; 64];
    salt_1[..32].copy_from_slice(&challenge_1);
    salt_1[32..].copy_from_slice(&nonce_1);
    let mut chan_1 = Channel::new(&ticket.secret, &salt_1, Endpoint::Client).unwrap();
    ws::write(
        &mut host_ws,
        ws::BINARY,
        &encode_mux(
            MUX_TEXT,
            1,
            &serde_json::to_vec(&AuthHello {
                token: Some(hex(&ticket.token)),
                device_id: None,
                client_nonce: hex(&nonce_1),
            })
            .unwrap(),
        ),
    )
    .unwrap();
    ws::write(
        &mut host_ws,
        ws::BINARY,
        &encode_mux(
            MUX_BINARY,
            1,
            &chan_1
                .seal(
                    &serde_json::to_vec(&AuthProof {
                        label: "relay-phone".into(),
                    })
                    .unwrap(),
                )
                .unwrap(),
        ),
    )
    .unwrap();

    let (kind, auth_sealed) = read_mux_for(&mut host_ws, 1);
    assert_eq!(kind, MUX_BINARY);
    let auth_1: Authenticated =
        serde_json::from_slice(&chan_1.open(&auth_sealed).unwrap()).unwrap();
    let cred_1 = decode_hex::<32>(auth_1.credential.as_deref().unwrap()).unwrap();

    // ServerHello 수신 후 ClientHello + List 전송
    let (kind, sh_sealed) = read_mux_for(&mut host_ws, 1);
    assert_eq!(kind, MUX_BINARY);
    let sh_plain = chan_1.open(&sh_sealed).unwrap();
    let (k, b) = ipc::read_frame(&mut sh_plain.as_slice()).unwrap().unwrap();
    ServerHello::decode(k, &b).unwrap();
    let ch_sealed = chan_1
        .seal(
            &client_hello(ClientRole::App, "relay-phone-1")
                .encode()
                .unwrap(),
        )
        .unwrap();
    ws::write(
        &mut host_ws,
        ws::BINARY,
        &encode_mux(MUX_BINARY, 1, &ch_sealed),
    )
    .unwrap();

    // 2. conn_id = 2: 동일 호스트 WS 위에서 두 번째 소켓(터미널 프리워밍 등) 동시 재인증
    let challenge_2 = open_conn(&mut host_ws, 2);
    let nonce_2 = [0x44_u8; 32];
    let mut salt_2 = [0_u8; 64];
    salt_2[..32].copy_from_slice(&challenge_2);
    salt_2[32..].copy_from_slice(&nonce_2);
    let cred_hash: [u8; 32] = digest::digest(&digest::SHA256, &cred_1)
        .as_ref()
        .try_into()
        .unwrap();
    let mut chan_2 = Channel::new(&cred_hash, &salt_2, Endpoint::Client).unwrap();
    // 재생 공격 검증용으로 conn 2의 클라이언트 프레임을 그대로 녹화해 둔다.
    let hello_2 = serde_json::to_vec(&AuthHello {
        token: None,
        device_id: Some(auth_1.device_id.clone()),
        client_nonce: hex(&nonce_2),
    })
    .unwrap();
    let proof_2 = chan_2
        .seal(
            &serde_json::to_vec(&AuthProof {
                label: "relay-phone".into(),
            })
            .unwrap(),
        )
        .unwrap();
    ws::write(&mut host_ws, ws::BINARY, &encode_mux(MUX_TEXT, 2, &hello_2)).unwrap();
    ws::write(
        &mut host_ws,
        ws::BINARY,
        &encode_mux(MUX_BINARY, 2, &proof_2),
    )
    .unwrap();
    let (kind, auth2_sealed) = read_mux_for(&mut host_ws, 2);
    assert_eq!(kind, MUX_BINARY);
    let auth_2: Authenticated =
        serde_json::from_slice(&chan_2.open(&auth2_sealed).unwrap()).unwrap();
    assert_eq!(auth_2.device_id, auth_1.device_id);
    assert!(auth_2.credential.is_none());
    let (kind, sh2_sealed) = read_mux_for(&mut host_ws, 2);
    assert_eq!(kind, MUX_BINARY);
    let _ = chan_2.open(&sh2_sealed).unwrap();
    // 인증 완료 후 변조된 암호문 프레임 전송 시 1000이 아니라 1008로 즉시 닫혀야 한다.
    ws::write(
        &mut host_ws,
        ws::BINARY,
        &encode_mux(MUX_BINARY, 2, &[0xde, 0xad, 0xbe, 0xef]),
    )
    .unwrap();
    let (kind, close2_payload) = read_mux_for(&mut host_ws, 2);
    assert_eq!(kind, MUX_CLOSE);
    assert_eq!(close2_payload, 1008_u16.to_be_bytes());

    // 3. conn_id = 3: 잘못된 키로 위조 증명 전송 -> conn_id=3만 1008로 닫히고 conn_id=1은 정상 동작
    let challenge_3 = open_conn(&mut host_ws, 3);
    let nonce_3 = [0x66_u8; 32];
    let mut salt_3 = [0_u8; 64];
    salt_3[..32].copy_from_slice(&challenge_3);
    salt_3[32..].copy_from_slice(&nonce_3);
    let mut bad_chan = Channel::new(&[0x99_u8; 32], &salt_3, Endpoint::Client).unwrap();
    ws::write(
        &mut host_ws,
        ws::BINARY,
        &encode_mux(
            MUX_TEXT,
            3,
            &serde_json::to_vec(&AuthHello {
                token: None,
                device_id: Some(auth_1.device_id.clone()),
                client_nonce: hex(&nonce_3),
            })
            .unwrap(),
        ),
    )
    .unwrap();
    ws::write(
        &mut host_ws,
        ws::BINARY,
        &encode_mux(
            MUX_BINARY,
            3,
            &bad_chan.seal(b"{\"label\":\"attacker\"}").unwrap(),
        ),
    )
    .unwrap();
    let (kind, close_payload) = read_mux_for(&mut host_ws, 3);
    assert_eq!(kind, MUX_CLOSE);
    assert_eq!(close_payload, 1008_u16.to_be_bytes());

    // 4. 재생 공격: 릴레이가 conn 2에서 녹화한 AuthHello·AuthProof를 새 연결에 그대로 다시 보낸다.
    // 챌린지는 데몬이 연결마다 새로 만들므로(릴레이는 모른다) 키가 달라져 증명이 풀리지 않고 1008로 닫힌다.
    let challenge_4 = open_conn(&mut host_ws, 4);
    assert_ne!(challenge_4, challenge_2);
    ws::write(&mut host_ws, ws::BINARY, &encode_mux(MUX_TEXT, 4, &hello_2)).unwrap();
    ws::write(
        &mut host_ws,
        ws::BINARY,
        &encode_mux(MUX_BINARY, 4, &proof_2),
    )
    .unwrap();
    let (kind, close4_payload) = read_mux_for(&mut host_ws, 4);
    assert_eq!(kind, MUX_CLOSE);
    assert_eq!(close4_payload, 1008_u16.to_be_bytes());

    // conn_id = 1은 여전히 살아 있어 RPC(List) 응답을 정상 수신한다.
    let list_sealed = chan_1
        .seal(&control::Request::List.encode().unwrap())
        .unwrap();
    ws::write(
        &mut host_ws,
        ws::BINARY,
        &encode_mux(MUX_BINARY, 1, &list_sealed),
    )
    .unwrap();
    let (kind, reply_sealed) = read_mux_for(&mut host_ws, 1);
    assert_eq!(kind, MUX_BINARY);
    let reply_plain = chan_1.open(&reply_sealed).unwrap();
    let (k, b) = ipc::read_frame(&mut reply_plain.as_slice())
        .unwrap()
        .unwrap();
    assert_eq!(
        control::Reply::decode(k, &b).unwrap(),
        control::Reply::Sessions(Vec::new())
    );
}

#[test]
fn daemon_restart_readopts_loopback_port_and_live_cloudflared_pid_in_zero_ms() {
    fn must_not_spawn(_: u16) -> io::Result<super::super::tunnel::Tunnel> {
        Err(io::Error::other("must re-adopt existing cloudflared PID"))
    }
    let dir = std::env::temp_dir().join(format!("mk-mobile-readopt-{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    let r = Arc::new(Registry::new(None, None));
    let g = Gateway::start(Arc::downgrade(&r), Some(&dir), hello()).unwrap();
    lock(&g.shared.state).tunnel_start = Some(super::super::tunnel::Tunnel::persistent_fixture);
    g.request(MobileRequest::SetConnectMode {
        mode: ConnectMode::Tunnel,
        custom_address: Some("eyJ-named-token@krill.example.com".into()),
    });

    let MobileReply::PairingOffer { port, url, .. } = g.request(MobileRequest::MintPairingTicket {
        mode: ConnectMode::Tunnel,
        custom_address: None,
        rotate: true,
    }) else {
        panic!("expected tunnel offer");
    };
    assert!(url.starts_with("https://fixture-mobile.trycloudflare.com/#pair="));
    let (ticket, pid) = {
        let s = lock(&g.shared.state);
        assert_eq!(s.tunnel_token.as_deref(), Some("eyJ-named-token"));
        assert_eq!(s.tunnel_hostname.as_deref(), Some("krill.example.com"));
        (
            s.devices.ticket(now_ms()).unwrap().clone(),
            s.tunnel.as_ref().unwrap().pid().unwrap(),
        )
    };
    let mut phone = Client::pair(port, &ticket);
    phone.ipc();
    let (device, credential) = (phone.device.clone(), phone.credential);
    drop(phone);

    // 1. 데몬 종료(Gateway::drop): 기기가 등록되어 있으므로 cloudflared PID는 죽지 않고 살아남는다.
    drop(g);
    assert!(
        super::super::tunnel::pid_alive(pid),
        "cloudflared process must survive Gateway::drop when a device is enrolled"
    );
    let saved = Intent::load(Some(&dir)).unwrap();
    assert_eq!(saved.port, Some(port));
    assert_eq!(saved.tunnel_pid, Some(pid));
    assert_eq!(
        saved.tunnel_url.as_deref(),
        Some("https://fixture-mobile.trycloudflare.com")
    );
    assert_eq!(saved.tunnel_token.as_deref(), Some("eyJ-named-token"));
    assert_eq!(saved.tunnel_hostname.as_deref(), Some("krill.example.com"));

    // 2. 데몬 재시작(Gateway::start): 새 cloudflared를 띄우지 않고 기존 port와 PID를 0초 만에 재입양한다.
    let g2 = Gateway::start(Arc::downgrade(&r), Some(&dir), hello()).unwrap();
    lock(&g2.shared.state).tunnel_start = Some(must_not_spawn);
    until(|| {
        let s = lock(&g2.shared.state);
        !s.listeners.is_empty() && s.tunnel.as_ref().is_some_and(|t| t.ready_url().is_some())
    });
    {
        let s = lock(&g2.shared.state);
        assert_eq!(s.selected.unwrap().port(), port);
        assert_eq!(s.tunnel.as_ref().unwrap().pid(), Some(pid));
        assert_eq!(
            s.tunnel.as_ref().unwrap().ready_url(),
            Some("https://fixture-mobile.trycloudflare.com")
        );
    }
    let mut reconnected = Client::reconnect(port, &device, &credential);
    reconnected.ipc();
    drop(reconnected);

    // 3. 모든 기기 해제 시 입양된 cloudflared 프로세스까지 즉시 종료하고 Intent의 터널 정보를 비운다.
    g2.request(MobileRequest::RevokeDevice {
        device_id: None,
        all: true,
    });
    until(|| !super::super::tunnel::pid_alive(pid));
    let cleared = Intent::load(Some(&dir)).unwrap();
    assert!(cleared.port.is_none());
    assert!(cleared.tunnel_pid.is_none());
    assert!(cleared.tunnel_url.is_none());
    drop(g2);
    std::fs::remove_dir_all(dir).unwrap();
}
