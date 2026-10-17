//! GUI·CLI 공용 모바일 제어. 소켓 연결 수명과 비밀 출력 정책은 호출자가 정한다.
use super::{FEAT_MOBILE, MobileReply, MobileRequest};
use crate::{
    daemon::{self, Paths},
    ipc::{self, ClientRole},
    screen::Msg,
};
use std::io::{self, Write};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
pub fn request(
    paths: &Paths,
    request: &MobileRequest,
    role: ClientRole,
) -> io::Result<MobileReply> {
    let mut hello = daemon::client_hello(role, "mobile-admin");
    hello.features = FEAT_MOBILE;
    let mut connection = daemon::connect(paths, &hello).map_err(io::Error::other)?;
    if connection.negotiated.features & FEAT_MOBILE == 0 {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "update the daemon to use mobile control",
        ));
    }
    connection
        .stream
        .set_read_timeout(Some(Duration::from_secs(20)))?;
    connection
        .stream
        .set_write_timeout(Some(Duration::from_secs(20)))?;
    connection.stream.write_all(&request.encode()?)?;
    let (kind, body) =
        ipc::read_frame(&mut connection.stream)?.ok_or(io::ErrorKind::UnexpectedEof)?;
    let reply = MobileReply::decode(kind, &body)?;
    if let MobileReply::Failed(error) = reply {
        return Err(io::Error::other(error));
    }
    Ok(reply)
}
pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}
