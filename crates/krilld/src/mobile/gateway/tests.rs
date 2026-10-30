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
pub(super) fn start() -> (Arc<Registry>, Arc<Gateway>) {
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
    websocket_address(SocketAddrV4::new(Ipv4Addr::LOCALHOST, port))
}
fn websocket_address(address: SocketAddrV4) -> (TcpStream, [u8; 32]) {
    let mut s = TcpStream::connect(address).unwrap();
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
        Self::auth_address(
            SocketAddrV4::new(Ipv4Addr::LOCALHOST, port),
            token,
            device_id,
            secret,
        )
    }
    fn auth_address(
        address: SocketAddrV4,
        token: Option<[u8; 32]>,
        device_id: Option<String>,
        secret: &[u8; 32],
    ) -> Self {
        let (mut s, challenge) = websocket_address(address);
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
        assert_eq!(a.desktop.as_ref().unwrap().host_id.len(), 32);
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
    assert!(!s.listeners.is_empty());
    assert!(
        s.listeners
            .iter()
            .all(|l| match l.local_addr().unwrap().ip() {
                std::net::IpAddr::V4(ip) => ip.is_loopback() || net::tailscale(ip),
                std::net::IpAddr::V6(_) => false,
            })
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
fn invalid_backup_selection_preserves_live_connection_ticket_and_port() {
    let (_registry, gateway) = start();
    let (port, ticket) = offer(&gateway);
    let mut phone = Client::pair(port, &ticket);
    phone.ipc();
    let before = {
        let mut state = lock(&gateway.shared.state);
        state.devices.mint(now_ms(), true).unwrap().token
    };
    assert!(matches!(
        gateway.request(MobileRequest::SetConnectMode {
            mode: ConnectMode::Tailscale,
            custom_address: Some("127.0.0.1:0".into()),
        }),
        MobileReply::Failed(_)
    ));
    {
        let state = lock(&gateway.shared.state);
        assert_eq!(state.mode, ConnectMode::Tunnel);
        assert_eq!(state.selected.unwrap().port(), port);
        assert_eq!(state.devices.ticket(now_ms()).unwrap().token, before);
        assert_eq!(state.devices.active_count(), 1);
    }
    phone.write(&control::Request::List.encode().unwrap());
    let (kind, body) = phone.read().unwrap();
    assert_eq!(
        control::Reply::decode(kind, &body).unwrap(),
        control::Reply::Sessions(Vec::new())
    );
}

#[test]
fn restored_tailscale_without_an_interface_keeps_relay_gateway_alive_without_localhost_qr() {
    let (_registry, gateway) = start();
    {
        let mut state = lock(&gateway.shared.state);
        state.mode = ConnectMode::Tailscale;
        state.interfaces.clear();
        state.devices.mint(now_ms(), true).unwrap();
        reconcile(&mut state, now_ms()).unwrap();
        assert!(
            !state.listeners.is_empty(),
            "loopback transport must be ready for relay recovery"
        );
        assert!(pairing_offer(&state, now_ms()).is_err());
    }
    assert!(gateway.deep_link(krill_proto::SessionId(9)).is_none());
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
fn legitimate_reconnects_and_pre_auth_eof_do_not_consume_failure_budget() {
    let (_r, g) = start();
    let (port, ticket) = offer(&g);
    let client = Client::pair(port, &ticket);
    let (device, credential) = (client.device.clone(), client.credential);
    drop(client);
    until(|| lock(&g.shared.state).connections.is_empty());
    for _ in 0..7 {
        // A network switch can drop the socket either before AuthHello or before its proof.
        let (socket, _) = websocket(port);
        drop(socket);
        until(|| lock(&g.shared.state).connections.is_empty());
        let (mut socket, _) = websocket(port);
        send(
            &mut socket,
            ws::TEXT,
            &serde_json::to_vec(&AuthHello {
                token: None,
                device_id: Some(device.clone()),
                client_nonce: hex(&random().unwrap()),
            })
            .unwrap(),
        );
        drop(socket);
        until(|| lock(&g.shared.state).connections.is_empty());
        let (mut socket, _) = websocket(port);
        send(&mut socket, 8, &1000_u16.to_be_bytes());
        assert!(receive(&mut socket).is_err());
        until(|| lock(&g.shared.state).connections.is_empty());
        let client = Client::reconnect(port, &device, &credential);
        assert_eq!(client.device, device);
        drop(client);
        until(|| lock(&g.shared.state).connections.is_empty());
    }
    // Five failures must still fit: none of the successful or dropped sockets counted.
    let mut state = lock(&g.shared.state);
    for _ in 0..5 {
        state.devices.attempt(now_ms()).unwrap();
    }
    assert!(state.devices.check_cooldown(now_ms()).is_err());
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
    let header_end = bytes
        .windows(4)
        .position(|part| part == b"\r\n\r\n")
        .unwrap()
        + 4;
    assert!(
        std::str::from_utf8(&bytes[..header_end])
            .unwrap()
            .starts_with("HTTP/1.1 101")
    );
    assert_eq!(&bytes[header_end..], &[0x88, 2, 0x03, 0xf5]); // 1013: 인증 cooldown
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
    assert_eq!(lock(&g.shared.state).mode, ConnectMode::Tunnel);
    g.request(MobileRequest::SetConnectMode {
        mode: ConnectMode::Tunnel,
        custom_address: None,
    });
    drop(g);
    let g = Gateway::start(Weak::new(), Some(&dir), hello()).unwrap();
    assert_eq!(lock(&g.shared.state).mode, ConnectMode::Tunnel);
    drop(g);
    // 손상된 파일은 기본 Relay 모드로 시작한다.
    std::fs::write(dir.join(INTENT_FILE), b"{").unwrap();
    let g = Gateway::start(Weak::new(), Some(&dir), hello()).unwrap();
    assert_eq!(lock(&g.shared.state).mode, ConnectMode::Tunnel);
    drop(g);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn tailscale_preference_restart_preserves_port_backup_address_and_device_credential() {
    let dir = std::env::temp_dir().join(format!("mk-mobile-backup-restart-{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    let registry = Arc::new(Registry::new(None, None));
    let gateway = Gateway::start(Arc::downgrade(&registry), Some(&dir), hello()).unwrap();
    lock(&gateway.shared.state).tunnel_start = Some(super::super::tunnel::Tunnel::fixture);
    let (port, ticket) = offer(&gateway);
    let mut phone = Client::pair(port, &ticket);
    phone.ipc();
    let (device, credential) = (phone.device.clone(), phone.credential);
    assert!(matches!(
        gateway.request(MobileRequest::SetConnectMode {
            mode: ConnectMode::Tailscale,
            custom_address: Some("100.127.255.254:0".into()),
        }),
        MobileReply::GatewayStatus { .. }
    ));
    drop(phone);
    drop(gateway);
    let intent = Intent::load(Some(&dir)).unwrap();
    assert_eq!(intent.port, Some(port));
    assert_eq!(
        intent.tailscale_address.as_deref(),
        Some("100.127.255.254:0")
    );
    let gateway = Gateway::start(Arc::downgrade(&registry), Some(&dir), hello()).unwrap();
    until(|| !lock(&gateway.shared.state).listeners.is_empty());
    assert_eq!(lock(&gateway.shared.state).selected.unwrap().port(), port);
    let mut phone = Client::reconnect(port, &device, &credential);
    phone.ipc();
    phone.write(&control::Request::List.encode().unwrap());
    let (kind, body) = phone.read().unwrap();
    assert_eq!(
        control::Reply::decode(kind, &body).unwrap(),
        control::Reply::Sessions(Vec::new())
    );
    drop(phone);
    drop(gateway);
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
    // 첫 요청 없이 MOBILE_ATTACH_TIMEOUT(테스트 2초)이 지나면 레지스트리가 끝낸다. WS도 닫혀야 하며 응답 없이 멈추면 안 된다.
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
    for mode in [ConnectMode::Tunnel, ConnectMode::Lan] {
        state.mode = mode;
        let MobileReply::PairingOffer { url, .. } = pairing_offer(&state, now_ms()).unwrap() else {
            panic!("페어링 응답 없음");
        };
        assert!(url.contains("/#pair="));
        assert!(url.contains("&key="));
        assert!(!url.contains("?pair="));
        assert!(!url.contains("&r="));
    }
    state.mode = ConnectMode::Tailscale;
    assert!(
        pairing_offer(&state, now_ms()).is_err(),
        "never advertise localhost as a tailnet backup"
    );
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
    let reply = gateway.request(MobileRequest::SetConnectMode {
        mode: ConnectMode::Lan,
        custom_address: None,
    });
    assert!(matches!(reply, MobileReply::Failed(_)));
    drop(gateway);
    let second = Intent::load(Some(&dir)).unwrap();
    assert_eq!(second.mode, ConnectMode::Tunnel);
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
fn do_relay_multiplexes_pipelined_pair_reconnect_concurrent_sessions_and_1008_isolation() {
    use super::super::relay::{MUX_BINARY, MUX_CLOSE, MUX_OPEN, MUX_TEXT, decode_mux, encode_mux};
    fn must_not_start_tunnel(_: u16) -> io::Result<super::super::tunnel::Tunnel> {
        panic!("a healthy relay must not start cloudflared")
    }

    let relay_listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let relay_port = relay_listener.local_addr().unwrap().port();
    let (_r, g) = start();
    let (rid, rkey) = {
        let mut s = lock(&g.shared.state);
        s.publication.origin = format!("http://127.0.0.1:{relay_port}");
        s.account.session = Some(super::super::relay_auth::Session {
            token: "test-session".into(),
            expires_at_ms: u64::MAX,
            email: "test@example.test".into(),
            origin: s.publication.origin.clone(),
        });
        s.relay_enabled = true;
        s.tunnel_start = Some(must_not_start_tunnel);
        (s.publication.rid.clone(), s.publication.key.clone())
    };

    let rid_srv = rid.clone();
    let server_handle = thread::spawn(move || {
        // 실제 고정 출처와 같이 배포된 PWA manifest 확인 뒤 WebSocket을 받는다.
        let (mut manifest, _) = relay_listener.accept().unwrap();
        manifest
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut header = Vec::new();
        while !header.ends_with(b"\r\n\r\n") {
            let mut byte = [0];
            manifest.read_exact(&mut byte).unwrap();
            header.extend(byte);
        }
        assert!(String::from_utf8(header).unwrap().starts_with(&format!(
            "GET /m/{}/manifest.json ",
            super::super::pwa::asset_hash()
        )));
        manifest
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}")
            .unwrap();
        drop(manifest);
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
        assert!(headers.contains("x-krill-session: test-session\r\n"));
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

    {
        let mut state = lock(&g.shared.state);
        let port = state.selected.unwrap().port();
        state.tunnel = Some(super::super::tunnel::Tunnel::fixture(port).unwrap());
        let MobileReply::PairingOffer { url: restored, .. } =
            pairing_offer(&state, now_ms()).unwrap()
        else {
            panic!("missing offer")
        };
        assert_eq!(restored, url, "relay must win over an existing tunnel URL");
    }
    assert!(
        g.deep_link(krill_proto::SessionId(42))
            .unwrap()
            .starts_with(&format!("http://127.0.0.1:{relay_port}/m/"))
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
    assert_eq!(
        auth_1.desktop.as_ref().unwrap().host_id,
        lock(&g.shared.state).host_id
    );
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

    // 5. 중계 서버가 챌린지를 지정한 OPEN은 거부한다. 챌린지는 데몬만 고른다.
    ws::write(
        &mut host_ws,
        ws::BINARY,
        &encode_mux(MUX_OPEN, 5, &[0x77; 32]),
    )
    .unwrap();
    let (kind, close5_payload) = read_mux_for(&mut host_ws, 5);
    assert_eq!(kind, MUX_CLOSE);
    assert_eq!(close5_payload, 1002_u16.to_be_bytes());

    // Changing the QR preference keeps the relay and authenticated direct
    // sockets alive, even when the selected tailnet interface is unavailable.
    let port = {
        let state = lock(&g.shared.state);
        state.selected.unwrap().port()
    };
    let credential = decode_hex::<32>(auth_1.credential.as_deref().unwrap()).unwrap();
    let address = {
        let state = lock(&g.shared.state);
        state
            .listeners
            .iter()
            .filter_map(|listener| listener.local_addr().ok())
            .find_map(|address| match address {
                std::net::SocketAddr::V4(address) if net::tailscale(*address.ip()) => Some(address),
                _ => None,
            })
            .unwrap_or_else(|| SocketAddrV4::new(Ipv4Addr::LOCALHOST, port))
    };
    let credential_hash = digest::digest(&digest::SHA256, &credential);
    let mut direct = Client::auth_address(
        address,
        None,
        Some(auth_1.device_id.clone()),
        &credential_hash.as_ref().try_into().unwrap(),
    );
    direct.ipc();
    assert!(matches!(
        g.request(MobileRequest::SetConnectMode {
            mode: ConnectMode::Tailscale,
            custom_address: Some("100.127.255.254:0".into()),
        }),
        MobileReply::GatewayStatus { .. }
    ));
    {
        let state = lock(&g.shared.state);
        assert_eq!(state.selected.unwrap().port(), port);
        assert!(relay_pairing(&state));
    }
    direct.write(&control::Request::List.encode().unwrap());
    let (kind, body) = direct.read().unwrap();
    assert_eq!(
        control::Reply::decode(kind, &body).unwrap(),
        control::Reply::Sessions(Vec::new())
    );
    assert!(
        g.deep_link(krill_proto::SessionId(43))
            .unwrap()
            .contains("/m/")
    );
    assert!(matches!(
        g.request(MobileRequest::SetConnectMode {
            mode: ConnectMode::Tunnel,
            custom_address: None,
        }),
        MobileReply::GatewayStatus { .. }
    ));

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

    // 6. 이미 열린 conn_id로 다시 온 OPEN은 거부한다(새 챌린지를 내지 않는다).
    ws::write(&mut host_ws, ws::BINARY, &encode_mux(MUX_OPEN, 1, &[])).unwrap();
    let (kind, dup_payload) = read_mux_for(&mut host_ws, 1);
    assert_eq!(kind, MUX_CLOSE);
    assert_eq!(dup_payload, 1002_u16.to_be_bytes());
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

#[test]
fn missing_relay_pwa_fails_fast_without_quick_tunnel_fallback() {
    fn must_not_start_tunnel(_: u16) -> io::Result<super::super::tunnel::Tunnel> {
        panic!("relay failure must not start a quick tunnel")
    }
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let (_registry, gateway) = start();
    {
        let mut state = lock(&gateway.shared.state);
        state.publication.origin = format!("http://{}", listener.local_addr().unwrap());
        state.account.session = Some(super::super::relay_auth::Session {
            token: "test-session".into(),
            expires_at_ms: u64::MAX,
            email: "test@example.test".into(),
            origin: state.publication.origin.clone(),
        });
        state.relay_enabled = true;
        state.tunnel_start = Some(must_not_start_tunnel);
    }
    let server = thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut header = Vec::new();
        while !header.ends_with(b"\r\n\r\n") {
            let mut byte = [0];
            socket.read_exact(&mut byte).unwrap();
            header.extend(byte);
        }
        socket
            .write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
            .unwrap();
    });
    let started = Instant::now();
    let MobileReply::Failed(message) = gateway.request(MobileRequest::MintPairingTicket {
        mode: ConnectMode::Tunnel,
        custom_address: None,
        rotate: true,
    }) else {
        panic!("expected relay error")
    };
    assert!(
        message.contains("릴레이") || message.contains("배포"),
        "{message}"
    );
    assert!(started.elapsed() < MINT_WAIT);
    assert!(lock(&gateway.shared.state).tunnel.is_none());
    server.join().unwrap();
}

#[test]
fn revoking_all_devices_cancels_a_pending_pairing_wait() {
    fn pending(_: u16) -> io::Result<super::super::tunnel::Tunnel> {
        Err(io::Error::new(io::ErrorKind::WouldBlock, "still preparing"))
    }
    let (_registry, gateway) = start();
    lock(&gateway.shared.state).tunnel_start = Some(pending);
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
    until(|| {
        lock(&gateway.shared.state)
            .devices
            .ticket(now_ms())
            .is_some()
    });
    assert!(matches!(
        gateway.request(MobileRequest::RevokeDevice {
            device_id: None,
            all: true,
        }),
        MobileReply::Revoked { .. }
    ));
    let reply = receive.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(matches!(reply, MobileReply::Failed(_)));
    assert!(lock(&gateway.shared.state).listeners.is_empty());
    worker.join().unwrap();
}

#[test]
fn retired_lan_intent_migrates_without_changing_device_credentials() {
    let dir = std::env::temp_dir().join(format!("mk-mobile-retired-{}", hex(&random().unwrap())));
    let mut devices = super::super::devices::Devices::open(Some(&dir)).unwrap();
    let ticket = devices.mint(1, true).unwrap();
    let (device, _) = devices.pair(&ticket.token, "saved phone", 2).unwrap();
    let secret = devices.device_secret(&device.device_id).unwrap();
    let before = std::fs::read(dir.join("mobile-devices.json")).unwrap();
    super::super::devices::write_private(
        &dir.join(INTENT_FILE),
        &serde_json::to_vec(&serde_json::json!({"mode":"Lan","custom":"192.168.1.4:1234"}))
            .unwrap(),
    )
    .unwrap();
    let intent = Intent::load(Some(&dir)).unwrap();
    assert_eq!(intent.mode, ConnectMode::Tunnel);
    assert_eq!(intent.custom, None);
    let reopened = super::super::devices::Devices::open(Some(&dir)).unwrap();
    assert_eq!(reopened.device_secret(&device.device_id).unwrap(), secret);
    assert_eq!(
        std::fs::read(dir.join("mobile-devices.json")).unwrap(),
        before
    );
    assert_eq!(Intent::load(Some(&dir)).unwrap().mode, ConnectMode::Tunnel);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn cancel_qr_with_zero_devices_closes_exposure_without_revoking_devices() {
    let (_registry, gateway) = start();
    let (_, ticket) = offer(&gateway);
    assert_eq!(
        gateway.request(MobileRequest::CancelPairingTicket),
        MobileReply::Revoked { count: 0 }
    );
    let state = lock(&gateway.shared.state);
    assert!(state.devices.ticket(now_ms()).is_none());
    assert!(
        state
            .devices
            .pairing_secret(&ticket.token, now_ms())
            .is_err()
    );
    assert!(state.listeners.is_empty());
    assert_eq!(state.devices.active_count(), 0);
}

#[test]
fn auth_close_codes_distinguish_capacity_cooldown_and_rejection() {
    assert_eq!(
        auth_close_code(&io::Error::new(
            io::ErrorKind::StorageFull,
            super::super::devices::AuthRejection::Capacity
        )),
        Some(4009)
    );
    assert_eq!(
        auth_close_code(&io::Error::new(
            io::ErrorKind::PermissionDenied,
            super::super::devices::AuthRejection::Cooldown
        )),
        Some(1013)
    );
    assert_eq!(
        auth_close_code(&io::ErrorKind::InvalidData.into()),
        Some(1008)
    );
    assert_eq!(auth_close_code(&io::ErrorKind::TimedOut.into()), None);
    assert_eq!(auth_close_code(&io::ErrorKind::StorageFull.into()), None);
    assert_eq!(
        auth_close_code(&io::ErrorKind::PermissionDenied.into()),
        None
    );
    assert!(reject_retired_mode(ConnectMode::Lan, None).is_err());
    assert!(reject_retired_mode(ConnectMode::Lan, Some("192.168.1.2:1234")).is_err());
    assert!(reject_retired_mode(ConnectMode::Lan, Some("127.0.0.1:1234")).is_ok());
}

#[test]
fn cancel_qr_preserves_paired_device_and_authenticated_connection() {
    let (_registry, gateway) = start();
    let (port, ticket) = offer(&gateway);
    let mut phone = Client::pair(port, &ticket);
    phone.ipc();
    let id = phone.device.clone();
    let verifier = lock(&gateway.shared.state)
        .devices
        .device_secret(&id)
        .unwrap();
    lock(&gateway.shared.state)
        .devices
        .mint(now_ms(), true)
        .unwrap();
    assert_eq!(
        gateway.request(MobileRequest::CancelPairingTicket),
        MobileReply::Revoked { count: 0 }
    );
    assert_eq!(
        lock(&gateway.shared.state)
            .devices
            .device_secret(&id)
            .unwrap(),
        verifier
    );
    phone.write(&MobileRequest::Status.encode().unwrap());
    let (kind, body) = phone.read().unwrap();
    assert!(matches!(
        MobileReply::decode(kind, &body).unwrap(),
        MobileReply::GatewayStatus {
            active_offer: false,
            ..
        }
    ));
}

#[test]
fn relay_account_requires_local_ipc_and_does_not_gate_direct_pairing() {
    let (_registry, gateway) = start();
    let request = || MobileRequest::Workflow("{\"action\":\"relay_auth_status\"}".into());
    let MobileReply::Workflow(local) = gateway.request_local(request()) else {
        panic!("local reply")
    };
    let local: serde_json::Value = serde_json::from_str(&local).unwrap();
    assert_eq!(local["ok"], true);
    assert_eq!(local["data"]["signed_in"], false);
    let MobileReply::Workflow(remote) = gateway.request(request()) else {
        panic!("remote reply")
    };
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&remote).unwrap()["ok"],
        false
    );
    assert!(matches!(
        gateway.request(MobileRequest::MintPairingTicket {
            mode: ConnectMode::Lan,
            custom_address: Some("127.0.0.1:0".into()),
            rotate: true
        }),
        MobileReply::PairingOffer { .. }
    ));
    {
        let mut state = lock(&gateway.shared.state);
        state.tunnel_start = None;
    }
    let MobileReply::Failed(message) = gateway.request(MobileRequest::MintPairingTicket {
        mode: ConnectMode::Tunnel,
        custom_address: None,
        rotate: true,
    }) else {
        panic!("relay must require sign-in")
    };
    assert!(message.contains("Google"));
}

#[test]
fn relay_logout_invalidates_pending_approval_without_revoking_pairing() {
    let (_registry, gateway) = start();
    let (_, ticket) = offer(&gateway);
    let device_id;
    {
        let mut state = lock(&gateway.shared.state);
        device_id = state
            .devices
            .pair(&ticket.token, "kept phone", now_ms())
            .unwrap()
            .0
            .device_id;
        state.devices.mint(now_ms(), true).unwrap();
        state.account.pending = Some(super::super::relay_auth::Pending {
            code: "secret-code".into(),
            verifier: "secret-verifier".into(),
            next_poll_at_ms: 0,
            expires_at_ms: u64::MAX,
        });
    }
    let before = lock(&gateway.shared.state).account.generation;
    let MobileReply::Workflow(reply) = gateway.request_local(MobileRequest::Workflow(
        "{\"action\":\"relay_auth_logout\"}".into(),
    )) else {
        panic!("logout reply")
    };
    let reply: serde_json::Value = serde_json::from_str(&reply).unwrap();
    assert_eq!(reply["data"]["signed_in"], false);
    assert!(!reply.to_string().contains("secret"));
    let state = lock(&gateway.shared.state);
    assert!(state.account.pending.is_none());
    assert!(state.account.generation > before);
    assert!(state.devices.ticket(now_ms()).is_none());
    assert_eq!(state.devices.active_count(), 1);
    assert!(state.devices.device_secret(&device_id).is_ok());
}

#[test]
fn target_named_configuration_controls_google_gate_before_any_mutation() {
    for mint in [false, true] {
        let (_registry, gateway) = start();
        lock(&gateway.shared.state).relay_enabled = true;
        let request = |custom_address| {
            if mint {
                MobileRequest::MintPairingTicket {
                    mode: ConnectMode::Tunnel,
                    custom_address,
                    rotate: true,
                }
            } else {
                MobileRequest::SetConnectMode {
                    mode: ConnectMode::Tunnel,
                    custom_address,
                }
            }
        };
        let named = gateway.request(request(Some("fixture-token@krill.example.test".into())));
        assert!(!matches!(named, MobileReply::Failed(_)), "{named:?}");
        let original_ticket = lock(&gateway.shared.state)
            .devices
            .ticket(now_ms())
            .cloned();
        if environment_named_tunnel().is_none() {
            let hosted = gateway.request(request(Some(String::new())));
            assert!(matches!(hosted, MobileReply::Failed(message) if message.contains("Google")));
        }
        let invalid = gateway.request(request(Some("@invalid".into())));
        assert!(
            matches!(invalid, MobileReply::Failed(message) if message.contains("Named Tunnel"))
        );
        let state = lock(&gateway.shared.state);
        assert_eq!(state.tunnel_token.as_deref(), Some("fixture-token"));
        assert_eq!(state.tunnel_hostname.as_deref(), Some("krill.example.test"));
        assert_eq!(
            state.devices.ticket(now_ms()).map(|ticket| ticket.token),
            original_ticket.map(|ticket| ticket.token)
        );
    }
}

/// 변경 피드 고정 장치: 상태 디렉터리가 있는 게이트웨이(설정 파일로 스냅샷 본문을 바꿀 수 있다).
fn start_in(dir: &Path) -> (Arc<Registry>, Arc<Gateway>) {
    let r = Arc::new(Registry::new(None, Some(dir)));
    r.start_mobile(hello()).unwrap();
    lock(&r.mobile_gateway().unwrap().shared.state).tunnel_start =
        Some(super::super::tunnel::Tunnel::fixture);
    let gateway = Arc::clone(r.mobile_gateway().unwrap());
    (r, gateway)
}
fn feed_dir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("mk-feed-{}", hex(&random().unwrap())));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}
fn set_quick_command(dir: &Path, name: &str) {
    std::fs::write(
        dir.join("settings.toml"),
        format!("[[quick_commands]]\nname = '{name}'\ntext = '/{name} '\n"),
    )
    .unwrap();
}
fn workflow_json(kind: u8, body: &[u8]) -> serde_json::Value {
    let MobileReply::Workflow(text) = MobileReply::decode(kind, body).unwrap() else {
        panic!("not a workflow frame");
    };
    serde_json::from_str(&text).unwrap()
}
fn quick_names(snapshot: &serde_json::Value) -> Vec<String> {
    snapshot["quick_commands"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["name"].as_str().unwrap().to_owned())
        .collect()
}

/// 한 연결 위의 워크플로 송수신. 직접 연결과 릴레이가 같은 시나리오를 돈다.
trait FeedWire {
    fn send_json(&mut self, request: serde_json::Value);
    /// 다음 IPC 프레임. `None`은 `timeout` 동안 아무것도 오지 않음.
    fn next(&mut self, timeout: Duration) -> Option<(u8, Vec<u8>)>;
}
impl FeedWire for Client {
    fn send_json(&mut self, request: serde_json::Value) {
        self.write(
            &MobileRequest::Workflow(request.to_string())
                .encode()
                .unwrap(),
        );
    }
    fn next(&mut self, timeout: Duration) -> Option<(u8, Vec<u8>)> {
        self.socket.set_read_timeout(Some(timeout)).unwrap();
        let mut probe = [0];
        match self.socket.peek(&mut probe) {
            Ok(0) => panic!("connection closed"),
            Ok(_) => {}
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) =>
            {
                return None;
            }
            Err(e) => panic!("{e}"),
        }
        self.socket
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        Some(self.read().unwrap())
    }
}
/// 다음 응답(푸시가 아닌 프레임)을 받는다. 그 사이 도착한 푸시는 함께 돌려준다.
fn reply(wire: &mut impl FeedWire) -> (serde_json::Value, Vec<serde_json::Value>) {
    let mut pushes = Vec::new();
    loop {
        let (kind, body) = wire
            .next(Duration::from_secs(10))
            .expect("reply within deadline");
        let value = workflow_json(kind, &body);
        if value.get("push").is_some() {
            pushes.push(value);
        } else {
            return (value, pushes);
        }
    }
}
fn feed_scenario(wire: &mut impl FeedWire, dir: &Path) {
    // 옛 클라이언트 요청(feed 없음): 응답 모양은 그대로이고 아무것도 켜지지 않는다.
    wire.send_json(serde_json::json!({"action":"snapshot"}));
    let (first, pushes) = reply(wire);
    assert!(pushes.is_empty());
    assert_eq!(first["ok"], true);
    let data = &first["data"];
    assert_eq!(data["capabilities"]["change_feed"], true);
    assert!(data.get("feed").is_none() && data.get("rev").is_none());
    assert_eq!(quick_names(data), ["one"]);
    let etag = data["etag"].as_str().unwrap().to_owned();
    set_quick_command(dir, "legacy");
    assert!(
        wire.next(Duration::from_millis(2500)).is_none(),
        "no push to a client that never asked for the feed"
    );

    // 가장 긴 간격으로 켜도 변경 요청 응답은 그 간격을 기다리지 않는다(HOL 없음).
    wire.send_json(serde_json::json!({"action":"snapshot","since":etag,"feed":600_000}));
    let (armed, _) = reply(wire);
    assert_eq!(armed["data"]["feed"], 60_000, "interval is clamped");
    let etag = armed["data"]["etag"].as_str().unwrap().to_owned();
    let started = Instant::now();
    wire.send_json(serde_json::json!({"action":"save_quick_commands","commands":[{"name":"mobile","text":"/mobile "}]}));
    let (saved, _) = reply(wire);
    assert_eq!(saved["ok"], true);
    assert!(started.elapsed() < Duration::from_secs(5));

    // 1초 간격: 데스크톱 쪽 파일 변경이 요청 없이 같은 E2EE 채널로 푸시된다.
    wire.send_json(serde_json::json!({"action":"snapshot","since":etag,"feed":1000}));
    let (rearmed, _) = reply(wire);
    assert_eq!(rearmed["data"]["feed"], 1000);
    assert_eq!(quick_names(&rearmed["data"]), ["mobile"]);
    let mut rev = rearmed["data"]["rev"].as_u64().unwrap();
    // 재무장 직후 같은 상태는 푸시하지 않는다.
    assert!(wire.next(Duration::from_millis(1500)).is_none());
    let changed = Instant::now();
    set_quick_command(dir, "desktop");
    let (kind, body) = wire
        .next(Duration::from_secs(5))
        .expect("push after desktop change");
    let latency = changed.elapsed();
    let push = workflow_json(kind, &body);
    assert_eq!(push["push"], "snapshot");
    assert_eq!(quick_names(&push["data"]), ["desktop"]);
    assert!(push["data"]["rev"].as_u64().unwrap() > rev);
    rev = push["data"]["rev"].as_u64().unwrap();
    // 간격(1초) + 스냅샷 계산 시간 안에 도착한다.
    assert!(latency < Duration::from_secs(3), "push latency {latency:?}");
    eprintln!("feed push latency after desktop edit: {latency:?}");
    assert!(
        wire.next(Duration::from_millis(2500)).is_none(),
        "unchanged state is not pushed again"
    );

    // 숨김(feed 0): 변경이 있어도 보내지 않고, 다시 켜면 최신 본문으로 재동기화한다.
    let since = push["data"]["etag"].as_str().unwrap().to_owned();
    wire.send_json(serde_json::json!({"action":"snapshot","since":since,"feed":0}));
    let (paused, _) = reply(wire);
    assert_eq!(paused["data"]["feed"], 0);
    assert_eq!(paused["data"]["unchanged"], true);
    set_quick_command(dir, "hidden");
    assert!(wire.next(Duration::from_millis(2500)).is_none());
    wire.send_json(serde_json::json!({"action":"snapshot","since":since,"feed":1000}));
    let (resumed, _) = reply(wire);
    assert_eq!(quick_names(&resumed["data"]), ["hidden"]);
    assert!(resumed["data"]["rev"].as_u64().unwrap() > rev);
}

#[test]
fn change_feed_pushes_on_the_direct_e2ee_channel_and_stops_with_the_connection() {
    let dir = feed_dir();
    set_quick_command(&dir, "one");
    let (_r, g) = start_in(&dir);
    let (port, t) = offer(&g);
    let mut c = Client::pair(port, &t);
    c.ipc();
    feed_scenario(&mut c, &dir);
    // 끊기면 브리지가 피드 스레드를 멈추고 회수한 뒤에야 연결 슬롯을 돌려준다.
    drop(c);
    until(|| lock(&g.shared.state).connections.is_empty());
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn change_feed_pushes_through_the_relay_mux_and_ends_with_mux_close() {
    use super::super::relay::{MUX_BINARY, MUX_CLOSE, MUX_OPEN, MUX_TEXT, decode_mux, encode_mux};
    fn must_not_start_tunnel(_: u16) -> io::Result<super::super::tunnel::Tunnel> {
        panic!("a healthy relay must not start cloudflared")
    }
    struct Relay {
        host: TcpStream,
        channel: Channel,
        conn: u32,
    }
    impl Relay {
        fn frame(&mut self, timeout: Duration) -> Option<(u8, Vec<u8>)> {
            self.host.set_read_timeout(Some(timeout)).unwrap();
            loop {
                let mut probe = [0];
                match self.host.peek(&mut probe) {
                    Ok(_) => {}
                    Err(e)
                        if matches!(
                            e.kind(),
                            io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                        ) =>
                    {
                        return None;
                    }
                    Err(e) => panic!("{e}"),
                }
                self.host
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let (op, body) = ws::read(&mut self.host).unwrap();
                if op != ws::BINARY {
                    continue;
                }
                let (kind, cid, payload) = decode_mux(&body).unwrap();
                if cid == self.conn {
                    return Some((kind, payload.to_vec()));
                }
            }
        }
    }
    impl FeedWire for Relay {
        fn send_json(&mut self, request: serde_json::Value) {
            let sealed = self
                .channel
                .seal(
                    &MobileRequest::Workflow(request.to_string())
                        .encode()
                        .unwrap(),
                )
                .unwrap();
            ws::write(
                &mut self.host,
                ws::BINARY,
                &encode_mux(MUX_BINARY, self.conn, &sealed),
            )
            .unwrap();
        }
        fn next(&mut self, timeout: Duration) -> Option<(u8, Vec<u8>)> {
            let (kind, payload) = self.frame(timeout)?;
            assert_eq!(kind, MUX_BINARY);
            let plain = self.channel.open(&payload).unwrap();
            ipc::read_frame(&mut plain.as_slice()).unwrap()
        }
    }

    let dir = feed_dir();
    set_quick_command(&dir, "one");
    let relay_listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let relay_port = relay_listener.local_addr().unwrap().port();
    let (_r, g) = start_in(&dir);
    {
        let mut s = lock(&g.shared.state);
        s.publication.origin = format!("http://127.0.0.1:{relay_port}");
        s.account.session = Some(super::super::relay_auth::Session {
            token: "test-session".into(),
            expires_at_ms: u64::MAX,
            email: "test@example.test".into(),
            origin: s.publication.origin.clone(),
        });
        s.relay_enabled = true;
        s.tunnel_start = Some(must_not_start_tunnel);
    }
    let server = thread::spawn(move || {
        let (mut manifest, _) = relay_listener.accept().unwrap();
        manifest
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut header = Vec::new();
        while !header.ends_with(b"\r\n\r\n") {
            let mut byte = [0];
            manifest.read_exact(&mut byte).unwrap();
            header.extend(byte);
        }
        manifest
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}")
            .unwrap();
        drop(manifest);
        let (mut host, _) = relay_listener.accept().unwrap();
        host.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        host.set_write_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut req = Vec::new();
        while !req.ends_with(b"\r\n\r\n") {
            let mut b = [0_u8; 1];
            host.read_exact(&mut b).unwrap();
            req.extend_from_slice(&b);
        }
        let headers = String::from_utf8(req).unwrap();
        let key = headers
            .lines()
            .find_map(|l| l.strip_prefix("Sec-WebSocket-Key: "))
            .unwrap()
            .trim();
        let mut accept_in = Vec::from(key.as_bytes());
        accept_in.extend_from_slice(b"258EAFA5-E914-47DA-95CA-C5AB0DC85B11");
        let accept = ws::base64(
            digest::digest(&digest::SHA1_FOR_LEGACY_USE_ONLY, &accept_in).as_ref(),
            false,
        );
        write!(
            host,
            "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\n\r\n"
        )
        .unwrap();
        host
    });
    let MobileReply::PairingOffer { .. } = g.request(MobileRequest::MintPairingTicket {
        mode: ConnectMode::Tunnel,
        custom_address: None,
        rotate: true,
    }) else {
        panic!("expected relay offer");
    };
    let mut host = server.join().unwrap();
    let ticket = lock(&g.shared.state)
        .devices
        .ticket(now_ms())
        .unwrap()
        .clone();
    ws::write(&mut host, ws::BINARY, &encode_mux(MUX_OPEN, 1, &[])).unwrap();
    let challenge = loop {
        let (_, body) = ws::read(&mut host).unwrap();
        let (kind, cid, text) = decode_mux(&body).unwrap();
        if cid == 1 && kind == MUX_TEXT {
            let hello: serde_json::Value = serde_json::from_slice(text).unwrap();
            break decode_hex::<32>(hello["challenge"].as_str().unwrap()).unwrap();
        }
    };
    let nonce = [0x33_u8; 32];
    let mut salt = [0_u8; 64];
    salt[..32].copy_from_slice(&challenge);
    salt[32..].copy_from_slice(&nonce);
    let channel = Channel::new(&ticket.secret, &salt, Endpoint::Client).unwrap();
    let mut relay = Relay {
        host,
        channel,
        conn: 1,
    };
    let auth = serde_json::to_vec(&AuthHello {
        token: Some(hex(&ticket.token)),
        device_id: None,
        client_nonce: hex(&nonce),
    })
    .unwrap();
    ws::write(&mut relay.host, ws::BINARY, &encode_mux(MUX_TEXT, 1, &auth)).unwrap();
    let proof = relay
        .channel
        .seal(
            &serde_json::to_vec(&AuthProof {
                label: "relay-feed".into(),
            })
            .unwrap(),
        )
        .unwrap();
    ws::write(
        &mut relay.host,
        ws::BINARY,
        &encode_mux(MUX_BINARY, 1, &proof),
    )
    .unwrap();
    let (kind, sealed) = relay.frame(Duration::from_secs(5)).unwrap();
    assert_eq!(kind, MUX_BINARY);
    let _: Authenticated = serde_json::from_slice(&relay.channel.open(&sealed).unwrap()).unwrap();
    let (k, b) = relay.next(Duration::from_secs(5)).unwrap();
    ServerHello::decode(k, &b).unwrap();
    let hello = relay
        .channel
        .seal(
            &client_hello(ClientRole::App, "relay-feed")
                .encode()
                .unwrap(),
        )
        .unwrap();
    ws::write(
        &mut relay.host,
        ws::BINARY,
        &encode_mux(MUX_BINARY, 1, &hello),
    )
    .unwrap();

    feed_scenario(&mut relay, &dir);

    // 중계 서버가 휴대폰 소켓 종료를 알리면 피드가 멈추고 연결 슬롯이 회수된다. 호스트 WS는 산다.
    ws::write(
        &mut relay.host,
        ws::BINARY,
        &encode_mux(MUX_CLOSE, 1, &1000_u16.to_be_bytes()),
    )
    .unwrap();
    until(|| {
        lock(&g.shared.state)
            .connections
            .values()
            .all(|c| c.device.is_none())
    });
    assert!(
        relay
            .frame(Duration::from_millis(2500))
            .is_none_or(|(kind, _)| kind == MUX_CLOSE)
    );
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn desktop_identity_is_seeded_once_and_survives_relay_rotation() {
    let dir = std::env::temp_dir().join(format!("mobile-desktop-id-{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).unwrap();
    let mut intent = Intent::load(Some(&dir)).unwrap();
    assert_eq!(intent.host_id, intent.rid);
    let identity = intent.host_id.clone();
    intent.rid = "aa".repeat(16);
    intent.save(&dir).unwrap();
    let restored = Intent::load(Some(&dir)).unwrap();
    assert_eq!(restored.host_id, identity);
    assert_eq!(restored.rid, "aa".repeat(16));
    std::fs::remove_dir_all(dir).unwrap();
}
#[test]
fn secure_tailscale_qr_fits_fixed_encoder_at_long_machine_name() {
    let (_registry, gateway) = start();
    let (port, ticket) = offer(&gateway);
    let mut state = lock(&gateway.shared.state);
    state.mode = ConnectMode::Tailscale;
    state.tailscale_serve = Some(super::super::tailscale_serve::Serve::fixture(
        port,
        format!("https://{}.tail187d9d.ts.net:18449", "a".repeat(63)),
    ));
    let url = pairing_url(&state, &ticket, &fixed_base(&state)).unwrap();
    assert_eq!(url.len(), 269);
    assert!(url.starts_with("https://krill-mobile.pages.dev/m/"));
    assert!(url.contains("/#p="));
    assert!(!url.contains("&h="));
    let token = url.split("#p=").nth(1).unwrap().split('&').next().unwrap();
    assert_eq!(token, ws::base64(&ticket.token, true));
    state.tailscale_serve = Some(super::super::tailscale_serve::Serve::fixture(
        port,
        format!("https://{}.tail187d9d.ts.net:18449", "a".repeat(100)),
    ));
    assert!(pairing_url(&state, &ticket, &fixed_base(&state)).is_err());
}
#[test]
fn changing_loopback_target_invalidates_only_the_owned_https_route() {
    let (_registry, gateway) = start();
    let (port, _) = offer(&gateway);
    let mut state = lock(&gateway.shared.state);
    state.tailscale_enabled = true;
    state.tailscale_serve = Some(super::super::tailscale_serve::Serve::fixture(
        port,
        "https://desktop.tail123.ts.net:18449".into(),
    ));
    let next = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    next.set_nonblocking(true).unwrap();
    let next_port = next.local_addr().unwrap().port();
    state.listeners = vec![next];
    request_tailscale(&mut state, false).unwrap();
    assert!(
        state
            .tailscale_serve
            .as_ref()
            .unwrap()
            .matches_port(next_port)
    );
    assert!(state.tailscale_serve.as_ref().unwrap().ready().is_none());
    assert!(!state.devices.list().is_empty() || state.devices.ticket(now_ms()).is_some());
}

#[test]
fn secure_tailscale_notification_uses_the_same_hosted_storage_origin() {
    let (_registry, gateway) = start();
    let (port, _) = offer(&gateway);
    {
        let mut state = lock(&gateway.shared.state);
        state.mode = ConnectMode::Tailscale;
        state.tailscale_serve = Some(super::super::tailscale_serve::Serve::fixture(
            port,
            "https://desktop.tail123.ts.net:18449".into(),
        ));
    }
    assert_eq!(
        gateway.deep_link(krill_proto::SessionId(8)).unwrap(),
        format!(
            "https://krill-mobile.pages.dev/m/{}/?session=8",
            super::super::pwa::asset_hash()
        )
    );
}
