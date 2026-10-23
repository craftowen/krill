//! holder 감시 연결(OWE-493). 초기 상태와 구독 등록을 같은 잠금에서 처리해 상실 통보 경합을 막는다.
//! 고아 인계 소켓이 준비된 뒤만 `Orphan`을 알린다. 클라이언트가 없어도 기존 세션은 계속 서빙한다.

use krill_proto::os::net::UnixStream;
use std::collections::HashMap;
use std::io::{self, Write};
use std::net::Shutdown;
use std::sync::Mutex;
use std::time::Duration;

use krill_proto::daemon::HolderStatus;

use crate::session::lock;

struct State {
    status: HolderStatus,
    next: u64,
    clients: HashMap<u64, UnixStream>,
}

pub(crate) struct Watch {
    state: Mutex<State>,
}

impl Watch {
    pub(crate) fn new(status: HolderStatus) -> Self {
        Self {
            state: Mutex::new(State {
                status,
                next: 0,
                clients: HashMap::new(),
            }),
        }
    }

    /// `app`: 앱 연결의 피어 pid([`crate::attach::watch`]가 하트비트 지연을 기록한다).
    pub(crate) fn serve(&self, mut stream: UnixStream, app: Option<u32>) -> io::Result<()> {
        stream.set_write_timeout(Some(Duration::from_millis(100)))?;
        let id = {
            let mut st = lock(&self.state);
            stream.write_all(&st.status.encode()?)?;
            if st.status == HolderStatus::Orphan {
                return Ok(());
            }
            let id = st.next;
            st.next = st.next.wrapping_add(1);
            st.clients.insert(id, stream.try_clone()?);
            id
        };
        let result = crate::attach::watch(stream, app);
        lock(&self.state).clients.remove(&id);
        result
    }

    pub(crate) fn orphan(&self) {
        let clients = {
            let mut st = lock(&self.state);
            st.status = HolderStatus::Orphan;
            std::mem::take(&mut st.clients)
        };
        for (_, mut stream) in clients {
            if let Ok(frame) = HolderStatus::Orphan.encode() {
                stream.write_all(&frame).ok();
            }
            // 쓰기 실패도 EOF로 알리고 서버 감시 스레드도 깨운다.
            stream.shutdown(Shutdown::Both).ok();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::thread;

    #[test]
    fn loss_notifies_subscribers_and_late_connections() {
        let watch = Arc::new(Watch::new(HolderStatus::Linked));
        let mut clients = Vec::new();
        let mut servers = Vec::new();
        for _ in 0..2 {
            let (mut client, server) = UnixStream::pair().unwrap();
            client
                .set_read_timeout(Some(Duration::from_secs(1)))
                .unwrap();
            let w = Arc::clone(&watch);
            servers.push(thread::spawn(move || w.serve(server, None)));
            let (kind, body) = krill_proto::ipc::read_frame(&mut client).unwrap().unwrap();
            assert_eq!(
                HolderStatus::decode(kind, &body).unwrap(),
                HolderStatus::Linked
            );
            clients.push(client);
        }
        watch.orphan();
        for mut client in clients {
            let (kind, body) = krill_proto::ipc::read_frame(&mut client).unwrap().unwrap();
            assert_eq!(
                HolderStatus::decode(kind, &body).unwrap(),
                HolderStatus::Orphan
            );
            assert!(krill_proto::ipc::read_frame(&mut client).unwrap().is_none());
        }
        for server in servers {
            // macOS의 닫힌 소켓 timeout 설정(EINVAL)도 정리되어야 한다. IO 결과와 별개로 구독 제거를 본다.
            server.join().unwrap().ok();
        }
        assert!(lock(&watch.state).clients.is_empty());
        let (mut late, server) = UnixStream::pair().unwrap();
        watch.serve(server, None).unwrap();
        let (kind, body) = krill_proto::ipc::read_frame(&mut late).unwrap().unwrap();
        assert_eq!(
            HolderStatus::decode(kind, &body).unwrap(),
            HolderStatus::Orphan
        );
    }

    #[test]
    fn disconnected_client_is_removed() {
        let watch = Arc::new(Watch::new(HolderStatus::Linked));
        let (mut client, server) = UnixStream::pair().unwrap();
        let w = Arc::clone(&watch);
        let serving = thread::spawn(move || w.serve(server, None));
        krill_proto::ipc::read_frame(&mut client).unwrap();
        drop(client);
        serving.join().unwrap().ok();
        assert!(lock(&watch.state).clients.is_empty());
    }
}
