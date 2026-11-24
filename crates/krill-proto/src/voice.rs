//! Pipito 로컬 음성 받아쓰기 IPC 프로토콜(krill-app ↔ krilld).
//!
//! [`crate::control::Request::Voice`]·[`crate::control::Reply::Voice`]로 다중화한다.
//! 호환 규칙: enum variant와 struct 필드는 끝에만 추가한다.

use std::fmt;

use serde::{Deserialize, Serialize};

/// 단일 IPC 프레임에 실을 수 있는 WAV 청크 바이트 상한(1 MiB `MAX_FRAME` 여유 확보).
pub const MAX_VOICE_AUDIO_CHUNK_BYTES: usize = 512 * 1024;
/// 멀티 청크 업로드 최대 조각 수(60초 이상 16kHz 모노 PCM WAV 수용).
pub const MAX_VOICE_TOTAL_CHUNKS: u16 = 32;
/// 텍스트 필드(`before`/`after`/`raw`/`text`) UTF-8 바이트 상한.
pub const MAX_VOICE_TEXT_BYTES: usize = 16 * 1024;
/// 단어장 항목·단축키·대상 ID UTF-8 바이트 상한.
pub const MAX_VOICE_WORD_BYTES: usize = 256;
/// 단어장 리스트 최대 항목 수.
pub const MAX_VOICE_VOCAB_ITEMS: usize = 512;
/// 데스크톱 기본 받아쓰기 단축키.
pub const DEFAULT_DESKTOP_SHORTCUT: &str = "Alt+Shift+Space";

/// 로컬 Pipito 브리지 연결 상태. variant는 끝에만 붙인다.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VoiceConnectionStatus {
    /// 브리지 응답 성공 및 엔진 준비 완료.
    Ready,
    /// 브리지 응답 성공, 엔진 로딩 중.
    Loading,
    /// 브리지 미응답이나 Pipito 앱/데이터 디렉터리 존재.
    NotRunning,
    /// 브리지·Pipito 설치 모두 미감지.
    NotInstalled,
}

impl VoiceConnectionStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::Loading => "loading",
            Self::NotRunning => "not_running",
            Self::NotInstalled => "not_installed",
        }
    }
}

/// 받아쓰기 문맥 대상 종류. variant는 끝에만 붙인다.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VoiceTargetKind {
    Session,
    BotChat,
    GroupChat,
    Item,
}

impl VoiceTargetKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Session => "session",
            Self::BotChat => "bot_chat",
            Self::GroupChat => "group_chat",
            Self::Item => "item",
        }
    }

    #[must_use]
    pub fn parse_str(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "session" => Some(Self::Session),
            "bot" | "bot_chat" => Some(Self::BotChat),
            "group" | "group_chat" => Some(Self::GroupChat),
            "item" | "todo" => Some(Self::Item),
            _ => None,
        }
    }

    /// Pipito `/local/transcribe`의 `app_category` 값.
    #[must_use]
    pub const fn app_category(self) -> &'static str {
        match self {
            Self::Session => "code",
            Self::BotChat | Self::GroupChat | Self::Item => "chat",
        }
    }
}

/// 자동 학습 단어장 항목 하나.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VocabEntry {
    #[serde(deserialize_with = "word_text")]
    pub word: String,
    pub count: u32,
    pub last_seen_ms: u64,
}

/// `krilld`에 영구 저장하는 음성 설정과 단어장(`voice.json`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VoiceConfig {
    pub refine_enabled: bool,
    #[serde(deserialize_with = "word_text")]
    pub desktop_shortcut: String,
    #[serde(deserialize_with = "word_vec")]
    pub pinned_vocab: Vec<String>,
    #[serde(deserialize_with = "word_vec")]
    pub ignored_vocab: Vec<String>,
    #[serde(deserialize_with = "vocab_entries")]
    pub auto_vocab: Vec<VocabEntry>,
}

impl Default for VoiceConfig {
    fn default() -> Self {
        Self {
            refine_enabled: true,
            desktop_shortcut: DEFAULT_DESKTOP_SHORTCUT.to_owned(),
            pinned_vocab: Vec::new(),
            ignored_vocab: Vec::new(),
            auto_vocab: Vec::new(),
        }
    }
}

/// `VoiceRequest::ConfigSet` 부분 패치.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct VoiceConfigPatch {
    pub refine_enabled: Option<bool>,
    #[serde(default, deserialize_with = "opt_word_text")]
    pub desktop_shortcut: Option<String>,
    #[serde(default, deserialize_with = "opt_word_text")]
    pub add_pinned: Option<String>,
    #[serde(default, deserialize_with = "opt_word_text")]
    pub remove_pinned: Option<String>,
    #[serde(default, deserialize_with = "opt_word_text")]
    pub ignore_word: Option<String>,
    #[serde(default, deserialize_with = "opt_word_text")]
    pub unignore_word: Option<String>,
}

/// Pipito 상태 및 Krill 음성 설정 요약.
#[expect(
    clippy::struct_excessive_bools,
    reason = "Pipito /local/health 와이어 스키마와 Krill refine_enabled 플래그를 그대로 전달"
)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VoiceStatusInfo {
    pub available: bool,
    pub status: VoiceConnectionStatus,
    #[serde(default, deserialize_with = "opt_word_text")]
    pub app_version: Option<String>,
    pub engine_ready: bool,
    #[serde(default, deserialize_with = "opt_text")]
    pub engine_error: Option<String>,
    pub refine_available: bool,
    pub refine_enabled: bool,
    pub vocab_count: u32,
}

/// Pipito `/local/transcribe` 결과.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VoiceTranscribeResult {
    pub silence: bool,
    #[serde(deserialize_with = "voice_text")]
    pub raw: String,
    #[serde(deserialize_with = "voice_text")]
    pub text: String,
    pub refined: bool,
    #[serde(default, deserialize_with = "opt_text")]
    pub refine_error: Option<String>,
    pub asr_ms: u64,
    pub refine_ms: u64,
}

/// 전송 전 수정 자동 학습 결과.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct VoiceFeedbackResult {
    #[serde(deserialize_with = "learned_pairs")]
    pub learned: Vec<(String, String)>,
    #[serde(deserialize_with = "word_vec")]
    pub boosted: Vec<String>,
}

/// 클라이언트 → krilld 음성 RPC. variant는 끝에만 붙인다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum VoiceRequest {
    Status,
    Warm {
        target_kind: Option<VoiceTargetKind>,
        #[serde(default, deserialize_with = "opt_word_text")]
        target_id: Option<String>,
    },
    Transcribe {
        #[serde(deserialize_with = "audio_chunk")]
        audio_wav: Vec<u8>,
        target_kind: Option<VoiceTargetKind>,
        #[serde(default, deserialize_with = "opt_word_text")]
        target_id: Option<String>,
        refine: Option<bool>,
        #[serde(default, deserialize_with = "opt_word_text")]
        upload_id: Option<String>,
        chunk_index: u16,
        total_chunks: u16,
    },
    Feedback {
        #[serde(deserialize_with = "voice_text")]
        before: String,
        #[serde(deserialize_with = "voice_text")]
        after: String,
        target_kind: Option<VoiceTargetKind>,
        #[serde(default, deserialize_with = "opt_word_text")]
        target_id: Option<String>,
    },
    ConfigGet,
    ConfigSet(VoiceConfigPatch),
}

impl VoiceRequest {
    /// Observer 역할이 호출할 수 있는 읽기 전용 요청 여부.
    #[must_use]
    pub const fn is_read_only(&self) -> bool {
        matches!(self, Self::Status | Self::Warm { .. } | Self::ConfigGet)
    }
}

/// krilld → 클라이언트 음성 RPC 응답. variant는 끝에만 붙인다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum VoiceReply {
    Status(VoiceStatusInfo),
    Warmed {
        ok: bool,
    },
    ChunkAccepted {
        #[serde(deserialize_with = "word_text")]
        upload_id: String,
        received: u16,
        total: u16,
    },
    Transcribed(VoiceTranscribeResult),
    FeedbackRecorded(VoiceFeedbackResult),
    Config(VoiceConfig),
    Error(#[serde(deserialize_with = "voice_text")] String),
}

fn bounded_string<'de, D: serde::Deserializer<'de>, const MAX: usize>(
    d: D,
) -> Result<String, D::Error> {
    struct Visitor<const M: usize>;
    impl<const M: usize> serde::de::Visitor<'_> for Visitor<M> {
        type Value = String;
        fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(f, "string with at most {M} bytes")
        }
        fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<String, E> {
            if value.len() > M {
                return Err(E::custom("voice string exceeds byte limit"));
            }
            Ok(value.to_owned())
        }
    }
    d.deserialize_str(Visitor::<MAX>)
}

fn voice_text<'de, D: serde::Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    bounded_string::<D, MAX_VOICE_TEXT_BYTES>(d)
}

fn word_text<'de, D: serde::Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    bounded_string::<D, MAX_VOICE_WORD_BYTES>(d)
}

fn opt_text<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
    let val = Option::<String>::deserialize(d)?;
    if val.as_ref().is_some_and(|s| s.len() > MAX_VOICE_TEXT_BYTES) {
        return Err(serde::de::Error::custom("voice text exceeds byte limit"));
    }
    Ok(val)
}

fn opt_word_text<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
    let val = Option::<String>::deserialize(d)?;
    if val.as_ref().is_some_and(|s| s.len() > MAX_VOICE_WORD_BYTES) {
        return Err(serde::de::Error::custom("voice word exceeds byte limit"));
    }
    Ok(val)
}

fn audio_chunk<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
    struct AudioVisitor;
    impl<'de> serde::de::Visitor<'de> for AudioVisitor {
        type Value = Vec<u8>;
        fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(f, "audio bytes up to {MAX_VOICE_AUDIO_CHUNK_BYTES}")
        }
        fn visit_bytes<E: serde::de::Error>(self, v: &[u8]) -> Result<Self::Value, E> {
            if v.len() > MAX_VOICE_AUDIO_CHUNK_BYTES {
                return Err(E::custom("audio chunk exceeds byte limit"));
            }
            Ok(v.to_vec())
        }
        fn visit_seq<A: serde::de::SeqAccess<'de>>(
            self,
            mut seq: A,
        ) -> Result<Self::Value, A::Error> {
            let mut out = Vec::with_capacity(seq.size_hint().unwrap_or(0).min(4096));
            while let Some(b) = seq.next_element::<u8>()? {
                if out.len() >= MAX_VOICE_AUDIO_CHUNK_BYTES {
                    return Err(serde::de::Error::custom("audio chunk exceeds byte limit"));
                }
                out.push(b);
            }
            Ok(out)
        }
    }
    d.deserialize_bytes(AudioVisitor)
}

fn word_vec<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<String>, D::Error> {
    let items = bounded_vec::<D, String, MAX_VOICE_VOCAB_ITEMS>(d)?;
    if items.iter().any(|s| s.len() > MAX_VOICE_WORD_BYTES) {
        return Err(serde::de::Error::custom("vocab word exceeds byte limit"));
    }
    Ok(items)
}

fn vocab_entries<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<VocabEntry>, D::Error> {
    bounded_vec::<D, VocabEntry, MAX_VOICE_VOCAB_ITEMS>(d)
}

fn learned_pairs<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<Vec<(String, String)>, D::Error> {
    let items = bounded_vec::<D, (String, String), MAX_VOICE_VOCAB_ITEMS>(d)?;
    if items
        .iter()
        .any(|(a, b)| a.len() > MAX_VOICE_WORD_BYTES || b.len() > MAX_VOICE_WORD_BYTES)
    {
        return Err(serde::de::Error::custom("learned pair exceeds byte limit"));
    }
    Ok(items)
}

fn bounded_vec<'de, D: serde::Deserializer<'de>, T: Deserialize<'de>, const MAX: usize>(
    d: D,
) -> Result<Vec<T>, D::Error> {
    struct Bounded<T, const M: usize>(std::marker::PhantomData<T>);
    impl<'de, T: Deserialize<'de>, const M: usize> serde::de::Visitor<'de> for Bounded<T, M> {
        type Value = Vec<T>;
        fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(f, "sequence with at most {M} items")
        }
        fn visit_seq<A: serde::de::SeqAccess<'de>>(
            self,
            mut seq: A,
        ) -> Result<Self::Value, A::Error> {
            let mut out = Vec::with_capacity(seq.size_hint().unwrap_or(0).min(64));
            while let Some(item) = seq.next_element()? {
                if out.len() >= M {
                    return Err(serde::de::Error::custom(
                        "voice sequence exceeds item limit",
                    ));
                }
                out.push(item);
            }
            Ok(out)
        }
    }
    d.deserialize_seq(Bounded::<T, MAX>(std::marker::PhantomData))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control::{Reply, Request};
    use crate::ipc::read_frame;
    use crate::screen::Msg as _;

    #[test]
    fn voice_request_and_reply_roundtrip_via_control_channel() {
        let requests = vec![
            VoiceRequest::Status,
            VoiceRequest::Warm {
                target_kind: Some(VoiceTargetKind::Session),
                target_id: Some("42".to_owned()),
            },
            VoiceRequest::Transcribe {
                audio_wav: b"RIFF1234WAVE".to_vec(),
                target_kind: Some(VoiceTargetKind::BotChat),
                target_id: Some("bot-1:chat-2".to_owned()),
                refine: Some(true),
                upload_id: None,
                chunk_index: 0,
                total_chunks: 1,
            },
            VoiceRequest::Feedback {
                before: "크릴 데몬".to_owned(),
                after: "krilld".to_owned(),
                target_kind: Some(VoiceTargetKind::GroupChat),
                target_id: Some("grp-1".to_owned()),
            },
            VoiceRequest::ConfigGet,
            VoiceRequest::ConfigSet(VoiceConfigPatch {
                refine_enabled: Some(false),
                desktop_shortcut: Some("Cmd+Shift+V".to_owned()),
                add_pinned: Some("Pipito".to_owned()),
                ..VoiceConfigPatch::default()
            }),
        ];

        for req in requests {
            let frame = Request::Voice(req.clone()).encode().unwrap();
            let (kind, body) = read_frame(&mut frame.as_slice()).unwrap().unwrap();
            assert_eq!(Request::decode(kind, &body).unwrap(), Request::Voice(req));
        }

        let replies = vec![
            VoiceReply::Status(VoiceStatusInfo {
                available: true,
                status: VoiceConnectionStatus::Ready,
                app_version: Some("1.2.0".to_owned()),
                engine_ready: true,
                engine_error: None,
                refine_available: true,
                refine_enabled: true,
                vocab_count: 14,
            }),
            VoiceReply::Warmed { ok: true },
            VoiceReply::ChunkAccepted {
                upload_id: "u1".to_owned(),
                received: 1,
                total: 2,
            },
            VoiceReply::Transcribed(VoiceTranscribeResult {
                silence: false,
                raw: "hello".to_owned(),
                text: "Hello.".to_owned(),
                refined: true,
                refine_error: None,
                asr_ms: 120,
                refine_ms: 85,
            }),
            VoiceReply::FeedbackRecorded(VoiceFeedbackResult {
                learned: vec![("크릴".to_owned(), "Krill".to_owned())],
                boosted: vec!["Krill".to_owned()],
            }),
            VoiceReply::Config(VoiceConfig::default()),
            VoiceReply::Error("Pipito is not running".to_owned()),
        ];

        for rep in replies {
            let frame = Reply::Voice(rep.clone()).encode().unwrap();
            let (kind, body) = read_frame(&mut frame.as_slice()).unwrap().unwrap();
            assert_eq!(Reply::decode(kind, &body).unwrap(), Reply::Voice(rep));
        }
    }

    #[test]
    fn voice_bounds_reject_oversized_audio_chunk() {
        let oversized = VoiceRequest::Transcribe {
            audio_wav: vec![0u8; MAX_VOICE_AUDIO_CHUNK_BYTES.saturating_add(1)],
            target_kind: None,
            target_id: None,
            refine: None,
            upload_id: None,
            chunk_index: 0,
            total_chunks: 1,
        };
        let wire = postcard::to_stdvec(&oversized).unwrap();
        assert!(postcard::from_bytes::<VoiceRequest>(&wire).is_err());
    }

    #[test]
    fn voice_target_kind_categories_match_contract() {
        assert_eq!(VoiceTargetKind::Session.app_category(), "code");
        assert_eq!(VoiceTargetKind::BotChat.app_category(), "chat");
        assert_eq!(VoiceTargetKind::GroupChat.app_category(), "chat");
        assert_eq!(VoiceTargetKind::Item.app_category(), "chat");
        assert_eq!(
            VoiceTargetKind::parse_str("session"),
            Some(VoiceTargetKind::Session)
        );
        assert_eq!(
            VoiceTargetKind::parse_str("bot_chat"),
            Some(VoiceTargetKind::BotChat)
        );
        assert_eq!(
            VoiceTargetKind::parse_str("group_chat"),
            Some(VoiceTargetKind::GroupChat)
        );
        assert_eq!(
            VoiceTargetKind::parse_str("item"),
            Some(VoiceTargetKind::Item)
        );
        assert_eq!(VoiceTargetKind::parse_str("unknown"), None);
    }

    proptest::proptest! {
        #[test]
        fn malformed_voice_never_panics(bytes in proptest::collection::vec(proptest::num::u8::ANY, 0..256)) {
            postcard::from_bytes::<VoiceRequest>(&bytes).ok();
            postcard::from_bytes::<VoiceReply>(&bytes).ok();
        }
    }
}
