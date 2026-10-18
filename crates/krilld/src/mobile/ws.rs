//! 제한된 RFC 6455 서버: 클라이언트 마스킹 필수, 확장·fragmentation 거부, 길이 검사 후 할당.
use super::e2ee::{MAX_PLAINTEXT, invalid};
use ring::digest;
use std::io::{self, Read, Write};
use std::net::TcpStream;

pub const BINARY: u8 = 2;
pub const TEXT: u8 = 1;

pub fn base64(bytes: &[u8], url: bool) -> String {
    let alphabet = if url {
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_"
    } else {
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/"
    };
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let a = u32::from(*chunk.first().unwrap_or(&0));
        let b = u32::from(*chunk.get(1).unwrap_or(&0));
        let c = u32::from(*chunk.get(2).unwrap_or(&0));
        let value = (a << 16) | (b << 8) | c;
        let n = chunk.len().saturating_add(1);
        for shift in [18, 12, 6, 0].into_iter().take(n) {
            out.push(char::from(
                *alphabet
                    .get(usize::try_from((value >> shift) & 63).unwrap_or(0))
                    .unwrap_or(&b'?'),
            ));
        }
        if !url {
            for _ in n..4 {
                out.push('=');
            }
        }
    }
    out
}

/// 요청 하나만 읽는다. body·pipelining은 처리하지 않는다. 정적 에셋은 MK-125가 이 라우터에 등록한다.
pub fn upgrade(stream: &mut TcpStream, allowed_origin: Option<&str>) -> io::Result<bool> {
    let mut bytes = Vec::new();
    while !bytes.ends_with(b"\r\n\r\n") {
        if bytes.len() >= 8192 {
            return Err(invalid());
        }
        let mut byte = [0];
        stream.read_exact(&mut byte)?;
        bytes.extend(byte);
    }
    let text = std::str::from_utf8(&bytes).map_err(|_| invalid())?;
    let mut lines = text.split("\r\n");
    let mut request = lines.next().ok_or_else(invalid)?.split_whitespace();
    if request.next() != Some("GET") {
        return Err(invalid());
    }
    let target = request.next().ok_or_else(invalid)?;
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    if request.next() != Some("HTTP/1.1") || request.next().is_some() {
        return Err(invalid());
    }
    let mut headers = std::collections::HashMap::new();
    for line in lines.filter(|s| !s.is_empty()) {
        let (name, value) = line.split_once(':').ok_or_else(invalid)?;
        if headers
            .insert(name.to_ascii_lowercase(), value.trim())
            .is_some()
        {
            return Err(invalid());
        }
    }
    if path != "/ws" {
        let gzip = headers
            .get("accept-encoding")
            .is_some_and(|v| v.split(',').any(|part| part.trim() == "gzip"));
        super::pwa::serve(stream, path, gzip)?;
        return Ok(false);
    }
    if query
        .split('&')
        .filter_map(|part| part.split_once('='))
        .any(|(name, value)| name == "v" && value != super::pwa::asset_hash())
    {
        stream.write_all(
            b"HTTP/1.1 409 Conflict\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        )?;
        return Ok(false);
    }
    if !headers
        .get("upgrade")
        .is_some_and(|v| v.eq_ignore_ascii_case("websocket"))
        || !headers.get("connection").is_some_and(|v| {
            v.split(',')
                .any(|s| s.trim().eq_ignore_ascii_case("upgrade"))
        })
        || headers.get("sec-websocket-version") != Some(&"13")
    {
        return Err(invalid());
    }
    let host = headers.get("host").ok_or_else(invalid)?;
    if let Some(origin) = headers.get("origin")
        && Some(*origin) != allowed_origin
        && *origin != format!("http://{host}")
        && *origin != format!("https://{host}")
    {
        return Err(invalid());
    }
    let key = headers.get("sec-websocket-key").ok_or_else(invalid)?;
    if key.len() != 24
        || !key.ends_with("==")
        || !key
            .bytes()
            .take(22)
            .all(|b| b.is_ascii_alphanumeric() || b == b'+' || b == b'/')
    {
        return Err(invalid());
    }
    let challenge = format!("{key}258EAFA5-E914-47DA-95CA-C5AB0DC85B11");
    let accept = base64(
        digest::digest(&digest::SHA1_FOR_LEGACY_USE_ONLY, challenge.as_bytes()).as_ref(),
        false,
    );
    write!(
        stream,
        "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\nCache-Control: no-store\r\n\r\n"
    )?;
    Ok(true)
}

pub fn read(stream: &mut impl Read) -> io::Result<(u8, Vec<u8>)> {
    read_limited(stream, MAX_PLAINTEXT.saturating_add(24))
}

pub fn read_limited(stream: &mut impl Read, limit: usize) -> io::Result<(u8, Vec<u8>)> {
    let mut header = [0; 2];
    stream.read_exact(&mut header)?;
    let [flags, length] = header;
    if flags & 0xf0 != 0x80 || length & 0x80 == 0 {
        return Err(invalid());
    }
    let opcode = flags & 0x0f;
    if ![TEXT, BINARY, 8, 9, 10].contains(&opcode) {
        return Err(invalid());
    }
    let small = length & 0x7f;
    let len = match small {
        126 => {
            let mut b = [0; 2];
            stream.read_exact(&mut b)?;
            let n = u64::from(u16::from_be_bytes(b));
            if n < 126 {
                return Err(invalid());
            }
            n
        }
        127 => {
            let mut b = [0; 8];
            stream.read_exact(&mut b)?;
            let n = u64::from_be_bytes(b);
            if n < 65_536 {
                return Err(invalid());
            }
            n
        }
        n => u64::from(n),
    };
    let len = usize::try_from(len).map_err(|_| invalid())?;
    if len > limit || (opcode >= 8 && len > 125) || (opcode == TEXT && len > 4096) {
        return Err(invalid());
    }
    let mut mask = [0; 4];
    stream.read_exact(&mut mask)?;
    let mut body = vec![0; len];
    stream.read_exact(&mut body)?;
    for (byte, key) in body.iter_mut().zip(mask.into_iter().cycle()) {
        *byte ^= key;
    }
    if opcode == TEXT && std::str::from_utf8(&body).is_err() {
        return Err(invalid());
    }
    Ok((opcode, body))
}
pub fn write(stream: &mut impl Write, opcode: u8, body: &[u8]) -> io::Result<()> {
    stream.write_all(&[0x80 | opcode])?;
    let n = body.len();
    if n < 126 {
        stream.write_all(&[u8::try_from(n).map_err(|_| invalid())?])?;
    } else if n <= 65_535 {
        stream.write_all(&[126])?;
        stream.write_all(&u16::try_from(n).map_err(|_| invalid())?.to_be_bytes())?;
    } else {
        stream.write_all(&[127])?;
        stream.write_all(&u64::try_from(n).map_err(|_| invalid())?.to_be_bytes())?;
    }
    stream.write_all(body)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rfc_accept_and_base64_vectors() {
        assert_eq!(
            base64(
                digest::digest(
                    &digest::SHA1_FOR_LEGACY_USE_ONLY,
                    b"dGhlIHNhbXBsZSBub25jZQ==258EAFA5-E914-47DA-95CA-C5AB0DC85B11"
                )
                .as_ref(),
                false
            ),
            "s3pPLMBiTxaQ9kYGzzhZRbK+xOo="
        );
        assert_eq!(base64(&[255, 255, 255], true), "____");
        assert_eq!(base64(b"f", false), "Zg==");
        assert_eq!(base64(b"fo", false), "Zm8=");
    }
    #[test]
    fn rejects_unmasked_fragmented_and_oversized_frames() {
        for b in [
            vec![0x82, 1, 0],
            vec![0x02, 0x80, 0, 0, 0, 0],
            vec![0x82, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff],
        ] {
            assert!(read(&mut b.as_slice()).is_err());
        }
        assert_eq!(
            read(&mut [0x82, 0x82, 1, 2, 3, 4, b'a' ^ 1, b'b' ^ 2].as_slice()).unwrap(),
            (BINARY, b"ab".to_vec())
        );
    }
    fn handshake(origin: &str, allowed: Option<&str>, query: &str) -> (io::Result<bool>, String) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let mut client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (mut server, _) = listener.accept().unwrap();
        write!(client, "GET /ws{query} HTTP/1.1\r\nHost: tunnel.test\r\nOrigin: {origin}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\r\n").unwrap();
        let result = upgrade(&mut server, allowed);
        drop(server);
        let mut response = String::new();
        client.read_to_string(&mut response).unwrap();
        (result, response)
    }
    #[test]
    fn fixed_origin_is_opt_in_and_same_origin_is_preserved() {
        let origin = "https://krill-mobile.pages.dev";
        assert!(handshake(origin, None, "").0.is_err());
        assert!(handshake("https://evil.test", Some(origin), "").0.is_err());
        assert!(
            handshake(&format!("{origin}.evil.test"), Some(origin), "")
                .0
                .is_err()
        );
        for own in ["http://tunnel.test", "https://tunnel.test"] {
            assert!(handshake(own, None, "").0.unwrap());
        }
        let (result, response) = handshake(
            origin,
            Some(origin),
            &format!("?v={}", super::super::pwa::asset_hash()),
        );
        assert!(result.unwrap());
        assert!(response.starts_with("HTTP/1.1 101"));
    }
    #[test]
    fn different_asset_version_returns_empty_409() {
        for query in ["?v=old", "?other=1&v=", "?v=old&v=older"] {
            let (result, response) = handshake("https://tunnel.test", None, query);
            assert!(!result.unwrap());
            assert_eq!(
                response,
                "HTTP/1.1 409 Conflict\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            );
        }
        assert!(
            handshake("https://tunnel.test", None, "?other=1")
                .0
                .unwrap()
        );
    }
    proptest::proptest! {
        #[test]
        fn arbitrary_wire_never_panics(bytes in proptest::collection::vec(proptest::num::u8::ANY,0..1024)) {read(&mut bytes.as_slice()).ok();}
    }
}
