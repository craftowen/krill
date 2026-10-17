//! 앱 전체의 테마 연결(OWE-590). 감시 연결이 재접속할 때 새로 열고 마지막 UI 값을 다시 보낸다.

use std::io::{self, Write};

use krill_proto::daemon;
use krill_proto::ipc::ClientRole;
use krill_proto::screen::Msg as _;
use krill_proto::theme::{FEAT_THEME, Push};

use crate::hosts::HostTarget;
use crate::pane::Writer;

/// 테마 전용 연결. 미지원 데몬에는 Hello 이후 아무 메시지도 보내지 않는다.
pub fn connect(target: impl Into<HostTarget>) -> io::Result<Option<Writer>> {
    let target = target.into();
    let mut hello = daemon::client_hello(ClientRole::App, "krill-app-theme");
    hello.features = FEAT_THEME;
    let c = target
        .connect(&hello)
        .map_err(|e| io::Error::other(e.to_string()))?;
    if c.negotiated.features & FEAT_THEME == 0 {
        return Ok(None);
    }
    Writer::spawn(c.stream).map(Some)
}

/// 협상된 연결이 있을 때만 쓴다. 같은 Writer 큐가 연속 변경의 순서를 보장한다.
pub fn push(out: Option<&mut impl Write>, value: Push) -> io::Result<bool> {
    let Some(out) = out else { return Ok(false) };
    out.write_all(&value.encode()?)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unsupported_daemon_gets_no_push_and_supported_gets_palette() {
        let value = Push {
            theme: crate::ui::TERMINAL_LIGHT,
            cell: None,
        };
        assert!(!push(None::<&mut Vec<u8>>, value).unwrap());
        let mut frames = Vec::new();
        assert!(push(Some(&mut frames), value).unwrap());
        let (k, b) = krill_proto::ipc::read_frame(&mut frames.as_slice())
            .unwrap()
            .unwrap();
        assert_eq!(Push::decode(k, &b).unwrap(), value);
    }
}
