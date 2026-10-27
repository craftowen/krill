//! Local IPC account operations. Network calls run outside the gateway lock.
use super::{
    ConnectMode, Gateway, hex, invalid, io, lock, named_tunnel, now_ms, random, relay_on,
    save_intent, ws,
};
use crate::mobile::relay_auth::{Pending, Session, post};
use serde_json::{Value, json};

impl Gateway {
    pub(super) fn relay_account(&self, action: &str) -> io::Result<Value> {
        match action {
            "relay_auth_status" => {
                let state = lock(&self.shared.state);
                let session = state
                    .account
                    .session
                    .as_ref()
                    .filter(|s| s.valid(&state.publication.origin, now_ms()));
                Ok(
                    json!({"requires_sign_in":relay_on(&state) && named_tunnel(&state).is_none(),"signed_in":session.is_some(),"email":session.map(|s| &s.email),"expires_at_ms":session.map(|s| s.expires_at_ms),"pending":state.account.pending.as_ref().is_some_and(|p| p.expires_at_ms > now_ms())}),
                )
            }
            "relay_auth_start" => self.relay_login_start(),
            "relay_auth_poll" => self.relay_login_poll(),
            "relay_auth_logout" => {
                let (origin, session) = {
                    let mut state = lock(&self.shared.state);
                    Session::remove(state.account.dir.as_deref())?;
                    state.account.generation = state.account.generation.wrapping_add(1);
                    state.account.pending = None;
                    let session = state.account.session.take();
                    state.relay = None;
                    state.publication.session = None;
                    if state.mode == ConnectMode::Tunnel {
                        state.devices.clear_ticket();
                    }
                    (state.publication.origin.clone(), session)
                };
                self.shared.wake();
                // Local logout succeeds even offline. Server revocation failure is reported honestly.
                let revoked = session.is_none_or(|s| {
                    post(&origin, "/auth/device/revoke", &json!({}), Some(&s.token))
                        .is_ok_and(|(status, _)| (200..300).contains(&status) || status == 401)
                });
                Ok(json!({"signed_in":false,"revoked":revoked}))
            }
            _ => Err(invalid()),
        }
    }
    fn relay_login_start(&self) -> io::Result<Value> {
        let verifier = ws::base64(&random()?, true);
        let challenge = ws::base64(
            ring::digest::digest(&ring::digest::SHA256, verifier.as_bytes()).as_ref(),
            true,
        );
        let (origin, generation) = {
            let mut state = lock(&self.shared.state);
            state.account.generation = state.account.generation.wrapping_add(1);
            state.account.pending = None;
            (state.publication.origin.clone(), state.account.generation)
        };
        let (status, data) = post(
            &origin,
            "/auth/device/start",
            &json!({"code_challenge":challenge,"code_challenge_method":"S256"}),
            None,
        )?;
        if status != 200 {
            return Err(io::Error::other(
                "Google sign-in is unavailable. Try again.",
            ));
        }
        let code = required(&data, "device_code")?;
        let user_code = required(&data, "user_code")?;
        let verification = required(&data, "verification_uri_complete")?;
        // Only the configured origin's device approval page can be opened by the desktop.
        if !verification.starts_with(&format!("{origin}/auth/device?")) {
            return Err(invalid());
        }
        let ttl = data
            .get("expires_in")
            .and_then(Value::as_u64)
            .filter(|ttl| *ttl > 0 && *ttl <= 600)
            .ok_or_else(invalid)?;
        let mut state = lock(&self.shared.state);
        if state.account.generation != generation {
            return Err(io::ErrorKind::Interrupted.into());
        }
        state.account.pending = Some(Pending {
            code,
            verifier,
            expires_at_ms: now_ms().saturating_add(ttl * 1000),
            next_poll_at_ms: now_ms().saturating_add(5000),
        });
        Ok(
            json!({"verification_uri_complete":verification,"user_code":user_code,"expires_in":ttl,"interval":5}),
        )
    }
    fn relay_login_poll(&self) -> io::Result<Value> {
        let (origin, generation, pending) = {
            let mut state = lock(&self.shared.state);
            let Some(pending) = state.account.pending.as_mut() else {
                return Ok(json!({"pending":false}));
            };
            if pending.expires_at_ms <= now_ms() {
                state.account.pending = None;
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "Google sign-in expired. Try again.",
                ));
            }
            if pending.next_poll_at_ms > now_ms() {
                return Ok(json!({"pending":true}));
            }
            pending.next_poll_at_ms = now_ms().saturating_add(5000);
            let pending = pending.clone();
            (
                state.publication.origin.clone(),
                state.account.generation,
                pending,
            )
        };
        let (status, data) = post(
            &origin,
            "/auth/device/token",
            &json!({"device_code":pending.code,"code_verifier":pending.verifier}),
            None,
        )?;
        let mut state = lock(&self.shared.state);
        if state.account.generation != generation {
            drop(state);
            if let Some(token) = data.get("session_token").and_then(Value::as_str) {
                post(&origin, "/auth/device/revoke", &json!({}), Some(token)).ok();
            }
            return Err(io::ErrorKind::Interrupted.into());
        }
        if status == 400
            && data.get("error").and_then(Value::as_str) == Some("authorization_pending")
        {
            return Ok(json!({"pending":true}));
        }
        if status != 200 {
            state.account.pending = None;
            return Err(io::Error::other(
                "Google sign-in was not approved or expired. Try again.",
            ));
        }
        let owner = required(data.get("account").ok_or_else(invalid)?, "id")?;
        let session = Session {
            token: required(&data, "session_token")?,
            expires_at_ms: data
                .get("expires_at")
                .and_then(Value::as_u64)
                .ok_or_else(invalid)?,
            email: required(data.get("account").ok_or_else(invalid)?, "email")?,
            origin,
        };
        if !session.valid(&state.publication.origin, now_ms()) {
            return Err(invalid());
        }
        if let Some(dir) = state.account.dir.as_deref() {
            session.save(dir)?;
        }
        let previous_publication = state.publication.clone();
        let previous_owner = state.relay_owner.clone();
        let changed_owner = state
            .relay_owner
            .as_ref()
            .is_some_and(|previous| previous != &owner);
        if changed_owner {
            state.publication.rid = hex(&random()?.into_iter().take(16).collect::<Vec<_>>());
            state.publication.key = hex(&random()?);
        }
        state.relay_owner = Some(owner);
        if let Err(error) = save_intent(&state) {
            state.publication = previous_publication;
            state.relay_owner = previous_owner;
            Session::remove(state.account.dir.as_deref()).ok();
            return Err(error);
        }
        if changed_owner {
            state.devices.clear_ticket();
        }
        let email = session.email.clone();
        state.publication.session = Some(session.clone());
        state.account.session = Some(session);
        state.account.pending = None;
        state.account.generation = state.account.generation.wrapping_add(1);
        state.relay = None;
        Ok(json!({"signed_in":true,"email":email,"pending":false}))
    }
}
fn required(data: &Value, key: &str) -> io::Result<String> {
    data.get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty() && s.len() <= 4096)
        .map(str::to_owned)
        .ok_or_else(invalid)
}

#[cfg(all(test, unix))]
#[expect(
    clippy::unwrap_used,
    reason = "local HTTP fixtures fail the test on invalid I/O"
)]
mod tests {
    use super::*;
    use std::{
        io::{Read as _, Write as _},
        net::{TcpListener, TcpStream},
        thread,
        time::Duration,
    };

    fn read_request(socket: &mut TcpStream) -> (String, Value) {
        socket
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut bytes = Vec::new();
        while !bytes.ends_with(b"\r\n\r\n") {
            let mut byte = [0];
            socket.read_exact(&mut byte).unwrap();
            bytes.extend(byte);
        }
        let headers = String::from_utf8(bytes).unwrap();
        let length: usize = headers
            .lines()
            .find_map(|line| line.strip_prefix("Content-Length: "))
            .unwrap()
            .parse()
            .unwrap();
        let mut body = vec![0; length];
        socket.read_exact(&mut body).unwrap();
        (headers, serde_json::from_slice(&body).unwrap())
    }
    fn respond(socket: &mut TcpStream, data: &Value) {
        let data = data.to_string();
        write!(socket,"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{data}",data.len()).unwrap();
    }
    #[test]
    fn device_flow_uses_pkce_and_never_returns_session_secret_to_ipc() {
        let (_registry, gateway) = super::super::tests::start();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let previous_rid = {
            let mut state = lock(&gateway.shared.state);
            state.publication.origin = origin.clone();
            state.relay_owner = Some("previous-owner".into());
            state.publication.rid.clone()
        };
        let server = thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            let (headers, start) = read_request(&mut socket);
            assert!(headers.starts_with("POST /auth/device/start "));
            assert_eq!(start["code_challenge_method"], "S256");
            let challenge = required(&start, "code_challenge").unwrap();
            respond(
                &mut socket,
                &json!({"device_code":"private-device-code","user_code":"ABCDEF0123456789","verification_uri_complete":format!("{origin}/auth/device?user_code=ABCDEF0123456789"),"expires_in":600,"interval":5}),
            );
            let (mut socket, _) = listener.accept().unwrap();
            let (headers, token) = read_request(&mut socket);
            assert!(headers.starts_with("POST /auth/device/token "));
            assert_eq!(token["device_code"], "private-device-code");
            let verifier = required(&token, "code_verifier").unwrap();
            assert_eq!(
                challenge,
                ws::base64(
                    ring::digest::digest(&ring::digest::SHA256, verifier.as_bytes()).as_ref(),
                    true
                )
            );
            respond(
                &mut socket,
                &json!({"session_token":"private-account-session","expires_at":now_ms()+60000,"account":{"id":"test-owner","email":"test@example.test"}}),
            );
            let (mut socket, _) = listener.accept().unwrap();
            let (headers, _) = read_request(&mut socket);
            assert!(headers.starts_with("POST /auth/device/revoke "));
            assert!(headers.contains("x-krill-session: private-account-session"));
            respond(&mut socket, &json!({"ok":true}));
        });
        let start = gateway.relay_account("relay_auth_start").unwrap();
        assert!(!start.to_string().contains("private-device"));
        assert!(!start.to_string().contains("code_verifier"));
        lock(&gateway.shared.state)
            .account
            .pending
            .as_mut()
            .unwrap()
            .next_poll_at_ms = 0;
        let poll = gateway.relay_account("relay_auth_poll").unwrap();
        assert_eq!(poll["signed_in"], true);
        {
            let state = lock(&gateway.shared.state);
            assert_ne!(state.publication.rid, previous_rid);
            assert_eq!(state.relay_owner.as_deref(), Some("test-owner"));
        }
        assert_eq!(poll["email"], "test@example.test");
        assert!(!poll.to_string().contains("private-account"));
        let status = gateway.relay_account("relay_auth_status").unwrap();
        assert!(!status.to_string().contains("private-account"));
        assert_eq!(
            gateway.relay_account("relay_auth_logout").unwrap()["revoked"],
            true
        );
        assert!(lock(&gateway.shared.state).account.session.is_none());
        server.join().unwrap();
    }
}
