//! 원격 호스트 파일 시스템(탐색·열기·원자적 저장·Quick Open) 및 루프백 포트 포워딩 DTO.
//! [`crate::control::FEAT_CONTROL`]·[`crate::control::FEAT_REMOTE_FS`]를 협상한 제어 연결에서
//! [`crate::control::Request::RemoteFs`] / [`crate::control::Reply::RemoteFs`]로 오간다.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::control::bounded;

/// 원격 경로 바이트 상한.
pub const MAX_REMOTE_PATH: usize = 4096;
/// 프레임 하나에 실을 수 있는 파일 읽기·쓰기 청크 바이트 상한(`ipc::MAX_FRAME` 1MiB 안).
pub const MAX_REMOTE_CHUNK: usize = 512 * 1024;
/// 한 번에 조회할 수 있는 후보 경로 수 상한.
pub const MAX_PROBE_PATHS: usize = 64;
/// 디렉터리 조회·워크트리 스캔 한 번의 항목 수 상한.
pub const MAX_DIR_ENTRIES: usize = 2048;
/// Quick Open 파일 목록 수 상한.
pub const MAX_QUICK_OPEN_FILES: usize = 2000;
/// 오류·항목 이름 UTF-8 바이트 상한.
pub const MAX_REMOTE_TEXT: usize = 4096;

/// 원격 파일의 수정 시각·크기 스탬프(충돌 감지·변경 감시용).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireStamp {
    pub modified_unix_ns: Option<u64>,
    pub len: u64,
}

/// 원격 경로의 메타데이터.
#[expect(
    clippy::struct_excessive_bools,
    reason = "wire metadata preserves independent existence, file, directory and symlink facts"
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireStat {
    pub exists: bool,
    pub is_file: bool,
    pub is_dir: bool,
    pub is_symlink: bool,
    pub stamp: Option<WireStamp>,
}

/// 파일 탐색기(`⌘⇧E`) 항목.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireDirEntry {
    #[serde(deserialize_with = "remote_text")]
    pub name: String,
    #[serde(deserialize_with = "remote_path")]
    pub path: Vec<u8>,
    #[serde(deserialize_with = "remote_text")]
    pub relative_path: String,
    pub is_dir: bool,
    pub is_symlink: bool,
}

/// 파일 탐색기 디렉터리 조회·스캔 결과.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireDirListing {
    #[serde(deserialize_with = "dir_entries")]
    pub entries: Vec<WireDirEntry>,
    #[serde(deserialize_with = "ignored_list")]
    pub ignored: Vec<String>,
}

/// 파일 탐색기 쓰기 동작.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum WireExplorerOp {
    CreateFile {
        #[serde(deserialize_with = "remote_path")]
        parent: Vec<u8>,
        #[serde(deserialize_with = "remote_text")]
        name: String,
    },
    CreateFolder {
        #[serde(deserialize_with = "remote_path")]
        parent: Vec<u8>,
        #[serde(deserialize_with = "remote_text")]
        name: String,
    },
    Rename {
        #[serde(deserialize_with = "remote_path")]
        path: Vec<u8>,
        #[serde(deserialize_with = "remote_text")]
        new_name: String,
    },
    Delete {
        #[serde(deserialize_with = "remote_path")]
        path: Vec<u8>,
    },
}

/// 원격 파일 읽기 청크 응답.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireReadChunk {
    pub stat: WireStat,
    #[serde(deserialize_with = "chunk_bytes")]
    pub bytes: Vec<u8>,
    pub eof: bool,
}

/// 원격 원자적 저장 결과.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum WireWriteOutcome {
    /// 최종 원자적 교체 완료 및 새 스탬프.
    Saved(WireStamp),
    /// 중간 스테이징 청크 기록 완료.
    StageAck,
    /// 디스크의 파일이 기대한 스탬프와 달라 저장을 거부함.
    Conflict { current: Option<WireStamp> },
    /// 그 밖의 I/O 오류.
    Failed(#[serde(deserialize_with = "remote_text")] String),
}

/// 원격 파일·탐색기·포트 포워딩 요청. variant는 끝에만 붙인다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RemoteFsRequest {
    /// 후보 경로들을 원격 `cwd`(및 `~`) 기준으로 해석해 실제 일반 파일(`is_file`)이면 절대 경로를 돌려준다.
    ProbePaths {
        #[serde(deserialize_with = "optional_remote_path")]
        cwd: Option<Vec<u8>>,
        #[serde(deserialize_with = "probe_paths")]
        paths: Vec<Vec<u8>>,
    },
    /// 단일 경로의 존재·종류·수정 스탬프를 조회한다.
    Stat {
        #[serde(deserialize_with = "remote_path")]
        path: Vec<u8>,
    },
    /// 원격 파일의 `offset`부터 최대 `max_bytes`(`MAX_REMOTE_CHUNK` 이하)만큼 읽는다.
    ReadFile {
        #[serde(deserialize_with = "remote_path")]
        path: Vec<u8>,
        offset: u64,
        max_bytes: u32,
    },
    /// 원격 파일을 원자적으로 저장한다(단일 청크 또는 `stage_id != 0` 다중 청크 스테이징 후 `commit`).
    WriteAtomic {
        #[serde(deserialize_with = "remote_path")]
        path: Vec<u8>,
        #[serde(deserialize_with = "chunk_bytes")]
        bytes: Vec<u8>,
        expected: Option<WireStamp>,
        stage_id: u64,
        offset: u64,
        commit: bool,
    },
    /// 파일 탐색기용 단일 디렉터리 조회(항목 목록 + git ignored 상대 경로 목록).
    ReadDir {
        #[serde(deserialize_with = "remote_path")]
        repo: Vec<u8>,
        #[serde(deserialize_with = "remote_path")]
        dir: Vec<u8>,
    },
    /// 파일 탐색기 검색 필터용 워크트리 전체 스캔.
    ScanWorktree {
        #[serde(deserialize_with = "remote_path")]
        repo: Vec<u8>,
        limit: u32,
    },
    /// 파일 탐색기 파일/폴더 생성·이름 변경·삭제.
    ExplorerOp {
        #[serde(deserialize_with = "remote_path")]
        repo: Vec<u8>,
        op: WireExplorerOp,
    },
    /// Quick Open(`⌘P`)용 워크트리 상대 파일 경로 목록 조회.
    QuickOpenList {
        #[serde(deserialize_with = "remote_path")]
        root: Vec<u8>,
    },
    /// 원격 호스트의 `127.0.0.1:<port>`로 TCP 연결을 맺고, 성공 응답 직후 제어 스트림을 양방향 바이트 터널로 전환한다.
    PortForward { port: u16 },
}

/// [`RemoteFsRequest`]의 응답. variant는 끝에만 붙인다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RemoteFsReply {
    /// 요청한 후보 경로별 해석된 원격 절대 파일 경로(파일이 아니면 `None`).
    ProbedPaths(#[serde(deserialize_with = "probed_paths")] Vec<Option<Vec<u8>>>),
    /// 단일 경로의 메타데이터.
    Stat(#[serde(deserialize_with = "stat_result")] Result<WireStat, String>),
    /// 파일 읽기 청크 결과.
    FileChunk(#[serde(deserialize_with = "chunk_result")] Result<WireReadChunk, String>),
    /// 원자적 파일 저장 결과.
    Written(WireWriteOutcome),
    /// 디렉터리 조회·워크트리 스캔 결과(`entries`, `ignored` 상대 경로 목록).
    DirEntries(#[serde(deserialize_with = "dir_listing_result")] Result<WireDirListing, String>),
    /// 파일 탐색기 변경 작업 결과(성공 시 대상 절대 경로).
    ExplorerOpResult(#[serde(deserialize_with = "explorer_op_result")] Result<Vec<u8>, String>),
    /// Quick Open 상대 파일 경로 목록.
    QuickOpenFiles(#[serde(deserialize_with = "quick_open_result")] Result<Vec<Vec<u8>>, String>),
    /// 포트 포워딩 연결 결과. `Ok(())` 프레임 직후부터 원시 TCP 바이트가 흐른다.
    PortForwarded(#[serde(deserialize_with = "unit_result")] Result<(), String>),
}

impl RemoteFsRequest {
    /// 관찰자(Observer)가 호출할 수 있는 읽기 전용 요청인가.
    #[must_use]
    pub const fn is_read_only(&self) -> bool {
        matches!(
            self,
            Self::ProbePaths { .. }
                | Self::Stat { .. }
                | Self::ReadFile { .. }
                | Self::ReadDir { .. }
                | Self::ScanWorktree { .. }
                | Self::QuickOpenList { .. }
        )
    }
}

fn remote_path<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
    bounded::<D, u8, MAX_REMOTE_PATH>(d)
}

fn optional_remote_path<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<Option<Vec<u8>>, D::Error> {
    #[derive(Deserialize)]
    #[serde(transparent)]
    struct Path(#[serde(deserialize_with = "remote_path")] Vec<u8>);
    Ok(Option::<Path>::deserialize(d)?.map(|p| p.0))
}

fn probe_paths<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<Vec<u8>>, D::Error> {
    #[derive(Deserialize)]
    #[serde(transparent)]
    struct Path(#[serde(deserialize_with = "remote_path")] Vec<u8>);
    Ok(bounded::<D, Path, MAX_PROBE_PATHS>(d)?
        .into_iter()
        .map(|p| p.0)
        .collect())
}

fn probed_paths<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<Option<Vec<u8>>>, D::Error> {
    #[derive(Deserialize)]
    #[serde(transparent)]
    struct OptPath(#[serde(deserialize_with = "optional_remote_path")] Option<Vec<u8>>);
    Ok(bounded::<D, OptPath, MAX_PROBE_PATHS>(d)?
        .into_iter()
        .map(|p| p.0)
        .collect())
}

fn dir_entries<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<WireDirEntry>, D::Error> {
    bounded::<D, WireDirEntry, MAX_DIR_ENTRIES>(d)
}

fn ignored_list<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<String>, D::Error> {
    #[derive(Deserialize)]
    #[serde(transparent)]
    struct Text(#[serde(deserialize_with = "remote_text")] String);
    Ok(bounded::<D, Text, MAX_DIR_ENTRIES>(d)?
        .into_iter()
        .map(|t| t.0)
        .collect())
}

fn quick_open_paths<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<Vec<u8>>, D::Error> {
    #[derive(Deserialize)]
    #[serde(transparent)]
    struct Path(#[serde(deserialize_with = "remote_path")] Vec<u8>);
    Ok(bounded::<D, Path, MAX_QUICK_OPEN_FILES>(d)?
        .into_iter()
        .map(|p| p.0)
        .collect())
}

fn remote_text<'de, D: serde::Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    struct Text;
    impl serde::de::Visitor<'_> for Text {
        type Value = String;
        fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("bounded remote text")
        }
        fn visit_str<E: serde::de::Error>(self, text: &str) -> Result<String, E> {
            if text.len() > MAX_REMOTE_TEXT {
                return Err(E::custom("remote text exceeds limit"));
            }
            Ok(text.to_owned())
        }
    }
    d.deserialize_str(Text)
}

fn bounded_err_result<'de, D, T>(d: D) -> Result<Result<T, String>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    #[derive(Deserialize)]
    #[serde(transparent)]
    struct ErrText(#[serde(deserialize_with = "remote_text")] String);
    Ok(Result::<T, ErrText>::deserialize(d)?.map_err(|e| e.0))
}

fn stat_result<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<Result<WireStat, String>, D::Error> {
    bounded_err_result(d)
}

fn chunk_result<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<Result<WireReadChunk, String>, D::Error> {
    bounded_err_result(d)
}

fn dir_listing_result<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<Result<WireDirListing, String>, D::Error> {
    bounded_err_result(d)
}

fn explorer_op_result<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<Result<Vec<u8>, String>, D::Error> {
    #[derive(Deserialize)]
    #[serde(transparent)]
    struct Path(#[serde(deserialize_with = "remote_path")] Vec<u8>);
    Ok(bounded_err_result::<D, Path>(d)?.map(|p| p.0))
}

fn quick_open_result<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<Result<Vec<Vec<u8>>, String>, D::Error> {
    #[derive(Deserialize)]
    #[serde(transparent)]
    struct Paths(#[serde(deserialize_with = "quick_open_paths")] Vec<Vec<u8>>);
    Ok(bounded_err_result::<D, Paths>(d)?.map(|p| p.0))
}

fn unit_result<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Result<(), String>, D::Error> {
    bounded_err_result(d)
}

fn chunk_bytes<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
    struct Bytes;
    impl<'de> serde::de::Visitor<'de> for Bytes {
        type Value = Vec<u8>;
        fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("bounded remote file chunk")
        }
        fn visit_seq<A: serde::de::SeqAccess<'de>>(
            self,
            mut seq: A,
        ) -> Result<Self::Value, A::Error> {
            use serde::de::Error as _;
            if seq.size_hint().is_some_and(|n| n > MAX_REMOTE_CHUNK) {
                return Err(A::Error::custom("remote chunk exceeds byte limit"));
            }
            let mut bytes = Vec::new();
            while let Some(byte) = seq.next_element()? {
                if bytes.len() >= MAX_REMOTE_CHUNK {
                    return Err(A::Error::custom("remote chunk exceeds byte limit"));
                }
                bytes.push(byte);
            }
            Ok(bytes)
        }
    }
    d.deserialize_seq(Bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control::{Reply, Request};
    use crate::ipc::read_frame;
    use crate::screen::Msg as _;

    #[test]
    fn remote_fs_request_and_reply_roundtrip() {
        let req = Request::RemoteFs(RemoteFsRequest::ReadFile {
            path: b"/remote/repo/src/lib.rs".to_vec(),
            offset: 0,
            max_bytes: 65536,
        });
        let encoded = req.encode().unwrap();
        let (kind, body) = read_frame(&mut encoded.as_slice()).unwrap().unwrap();
        assert_eq!(Request::decode(kind, &body).unwrap(), req);

        let reply = Reply::RemoteFs(RemoteFsReply::FileChunk(Ok(WireReadChunk {
            stat: WireStat {
                exists: true,
                is_file: true,
                is_dir: false,
                is_symlink: false,
                stamp: Some(WireStamp {
                    modified_unix_ns: Some(123_456_789),
                    len: 11,
                }),
            },
            bytes: b"hello world".to_vec(),
            eof: true,
        })));
        let encoded = reply.encode().unwrap();
        let (kind, body) = read_frame(&mut encoded.as_slice()).unwrap().unwrap();
        assert_eq!(Reply::decode(kind, &body).unwrap(), reply);
    }

    #[test]
    fn remote_fs_rejects_oversized_chunk_and_paths() {
        let bad_req = Request::RemoteFs(RemoteFsRequest::WriteAtomic {
            path: vec![b'a'; MAX_REMOTE_PATH + 1],
            bytes: Vec::new(),
            expected: None,
            stage_id: 0,
            offset: 0,
            commit: true,
        });
        let raw = postcard::to_stdvec(&bad_req).unwrap();
        assert!(Request::decode(crate::control::kind::REQUEST, &raw).is_err());
    }
    #[test]
    fn remote_fs_reserved_capability_is_not_advertised_and_forwarding_is_not_read_only() {
        assert_eq!(crate::ipc::FEATURES & crate::control::FEAT_REMOTE_FS, 0);
        assert!(!RemoteFsRequest::PortForward { port: 8080 }.is_read_only());
        assert!(
            RemoteFsRequest::Stat {
                path: b"/repo".to_vec()
            }
            .is_read_only()
        );
    }

    #[test]
    fn remote_fs_rejects_oversized_nested_payloads() {
        for request in [
            RemoteFsRequest::WriteAtomic {
                path: b"/file".to_vec(),
                bytes: vec![0; MAX_REMOTE_CHUNK + 1],
                expected: None,
                stage_id: 0,
                offset: 0,
                commit: true,
            },
            RemoteFsRequest::ProbePaths {
                cwd: None,
                paths: vec![b"/file".to_vec(); MAX_PROBE_PATHS + 1],
            },
            RemoteFsRequest::ExplorerOp {
                repo: b"/repo".to_vec(),
                op: WireExplorerOp::CreateFile {
                    parent: b"/repo".to_vec(),
                    name: "x".repeat(MAX_REMOTE_TEXT + 1),
                },
            },
        ] {
            let raw = postcard::to_stdvec(&Request::RemoteFs(request)).unwrap();
            assert!(Request::decode(crate::control::kind::REQUEST, &raw).is_err());
        }
        for reply in [
            RemoteFsReply::QuickOpenFiles(Ok(vec![vec![]; MAX_QUICK_OPEN_FILES + 1])),
            RemoteFsReply::ProbedPaths(vec![Some(vec![0; MAX_REMOTE_PATH + 1])]),
            RemoteFsReply::Written(WireWriteOutcome::Failed("x".repeat(MAX_REMOTE_TEXT + 1))),
            RemoteFsReply::Stat(Err("x".repeat(MAX_REMOTE_TEXT + 1))),
        ] {
            let raw = postcard::to_stdvec(&Reply::RemoteFs(reply)).unwrap();
            assert!(Reply::decode(crate::control::kind::REPLY, &raw).is_err());
        }
    }

    proptest::proptest! {
        #[test]
        fn remote_fs_decoders_do_not_panic(bytes in proptest::collection::vec(proptest::num::u8::ANY, 0..1024)) {
            postcard::from_bytes::<RemoteFsRequest>(&bytes).ok();
            postcard::from_bytes::<RemoteFsReply>(&bytes).ok();
        }
    }
}
