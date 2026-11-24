//! 데스크톱 Krill ↔ krilld Pipito 음성 받아쓰기 클라이언트 및 단축키 매처.

use std::io::{self, Write as _};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use krill_proto::control::{FEAT_CONTROL, Failure, Reply, Request};
use krill_proto::daemon;
use krill_proto::ipc::{self, ClientRole};
use krill_proto::screen::Msg as _;
use krill_proto::voice::{
    DEFAULT_DESKTOP_SHORTCUT, VoiceConfig, VoiceConfigPatch, VoiceFeedbackResult, VoiceReply,
    VoiceRequest, VoiceStatusInfo, VoiceTargetKind, VoiceTranscribeResult,
};

use crate::hosts::HostTarget;

/// 단일 프레임 WAV 전송 상한(384 KiB ≒ 16kHz 16-bit 모노 약 12초).
pub const SINGLE_FRAME_WAV_MAX: usize = 384 * 1024;
/// 멀티 청크 업로드 조각 크기(256 KiB).
pub const CHUNK_WAV_BYTES: usize = 256 * 1024;

pub type VoiceCall = Arc<dyn Fn(&VoiceRequest) -> io::Result<VoiceReply> + Send + Sync>;

/// `krilld`에 `Request::Voice`를 보내는 라이브 IPC 호출기.
#[must_use]
pub fn daemon_voice_call(target: Option<HostTarget>) -> VoiceCall {
    #[cfg(test)]
    if target.is_none() {
        return Arc::new(|_| {
            Err(io::Error::new(
                io::ErrorKind::NotConnected,
                "daemon disabled in unit tests",
            ))
        });
    }
    Arc::new(move |req| {
        let resolved = match &target {
            Some(t) => t.clone(),
            None => HostTarget::local_dev()?,
        };
        let hello = daemon::client_hello(ClientRole::App, "krill-app-voice");
        let mut conn = resolved.connect(&hello).map_err(io::Error::other)?;
        if conn.negotiated.features & FEAT_CONTROL == 0 {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "krilld does not support FEAT_CONTROL",
            ));
        }
        let read_timeout = match req {
            VoiceRequest::Transcribe { .. } => Duration::from_secs(25),
            _ => Duration::from_secs(5),
        };
        conn.stream.set_read_timeout(Some(read_timeout)).ok();
        conn.stream
            .set_write_timeout(Some(Duration::from_secs(5)))
            .ok();
        let frame = Request::Voice(req.clone()).encode()?;
        conn.stream.write_all(&frame)?;
        let (kind, body) =
            ipc::read_frame(&mut conn.stream)?.ok_or(io::ErrorKind::UnexpectedEof)?;
        match Reply::decode(kind, &body)? {
            Reply::Voice(reply) => Ok(reply),
            Reply::Failed(Failure::BadRequest) => Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "krilld does not support voice RPC (restart daemon to update)",
            )),
            Reply::Failed(failure) => Err(io::Error::other(failure.to_string())),
            _ => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "unexpected reply for VoiceRequest",
            )),
        }
    })
}

pub fn fetch_status(call: &VoiceCall) -> io::Result<VoiceStatusInfo> {
    match call(&VoiceRequest::Status)? {
        VoiceReply::Status(info) => Ok(info),
        VoiceReply::Error(msg) => Err(io::Error::other(msg)),
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "unexpected voice status reply",
        )),
    }
}

pub fn send_warm(
    call: &VoiceCall,
    target_kind: Option<VoiceTargetKind>,
    target_id: Option<String>,
) -> io::Result<bool> {
    match call(&VoiceRequest::Warm {
        target_kind,
        target_id,
    })? {
        VoiceReply::Warmed { ok } => Ok(ok),
        VoiceReply::Error(msg) => Err(io::Error::other(msg)),
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "unexpected voice warm reply",
        )),
    }
}

pub fn fetch_config(call: &VoiceCall) -> io::Result<VoiceConfig> {
    match call(&VoiceRequest::ConfigGet)? {
        VoiceReply::Config(cfg) => Ok(cfg),
        VoiceReply::Error(msg) => Err(io::Error::other(msg)),
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "unexpected voice config reply",
        )),
    }
}

pub fn update_config(call: &VoiceCall, patch: VoiceConfigPatch) -> io::Result<VoiceConfig> {
    match call(&VoiceRequest::ConfigSet(patch))? {
        VoiceReply::Config(cfg) => Ok(cfg),
        VoiceReply::Error(msg) => Err(io::Error::other(msg)),
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "unexpected voice config update reply",
        )),
    }
}

pub fn send_feedback(
    call: &VoiceCall,
    before: String,
    after: String,
    target_kind: Option<VoiceTargetKind>,
    target_id: Option<String>,
) -> io::Result<VoiceFeedbackResult> {
    match call(&VoiceRequest::Feedback {
        before,
        after,
        target_kind,
        target_id,
    })? {
        VoiceReply::FeedbackRecorded(res) => Ok(res),
        VoiceReply::Error(msg) => Err(io::Error::other(msg)),
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "unexpected voice feedback reply",
        )),
    }
}

/// WAV 바이트를 단일 프레임(<= 384 KiB) 또는 256 KiB 멀티 청크로 `krilld`에 전달해 전사한다.
pub fn transcribe_wav(
    call: &VoiceCall,
    wav_bytes: &[u8],
    target_kind: Option<VoiceTargetKind>,
    target_id: Option<String>,
    refine: Option<bool>,
) -> io::Result<VoiceTranscribeResult> {
    if wav_bytes.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "empty audio recording",
        ));
    }
    if wav_bytes.len() <= SINGLE_FRAME_WAV_MAX {
        return match call(&VoiceRequest::Transcribe {
            audio_wav: wav_bytes.to_vec(),
            target_kind,
            target_id,
            refine,
            upload_id: None,
            chunk_index: 0,
            total_chunks: 1,
        })? {
            VoiceReply::Transcribed(res) => Ok(res),
            VoiceReply::Error(msg) => Err(io::Error::other(msg)),
            _ => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "unexpected voice transcribe reply",
            )),
        };
    }

    let chunks: Vec<&[u8]> = wav_bytes.chunks(CHUNK_WAV_BYTES).collect();
    let total_chunks = u16::try_from(chunks.len())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "recording too long"))?;
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let upload_id = format!("desk-{}-{stamp:x}", std::process::id());

    for (idx, chunk) in chunks.into_iter().enumerate() {
        let chunk_index = u16::try_from(idx).unwrap_or(u16::MAX);
        let reply = call(&VoiceRequest::Transcribe {
            audio_wav: chunk.to_vec(),
            target_kind,
            target_id: target_id.clone(),
            refine,
            upload_id: Some(upload_id.clone()),
            chunk_index,
            total_chunks,
        })?;
        let is_last = idx + 1 == usize::from(total_chunks);
        match reply {
            VoiceReply::ChunkAccepted { .. } if !is_last => {}
            VoiceReply::Transcribed(res) if is_last => return Ok(res),
            VoiceReply::Error(msg) => return Err(io::Error::other(msg)),
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "unexpected chunked transcribe reply",
                ));
            }
        }
    }
    Err(io::Error::new(
        io::ErrorKind::UnexpectedEof,
        "chunked transcription finished without result",
    ))
}

/// gpui `Keystroke`를 저장용 정규 단축키 문자열(예: `"Alt+Shift+Space"`)로 변환한다.
/// 단독 수식키나 `Escape`는 거부해 `None`을 반환한다.
#[must_use]
pub fn format_keystroke(ks: &gpui::Keystroke) -> Option<String> {
    let raw_key = ks.key.trim().to_ascii_lowercase();
    if raw_key.is_empty()
        || matches!(
            raw_key.as_str(),
            "escape"
                | "shift"
                | "control"
                | "alt"
                | "platform"
                | "meta"
                | "super"
                | "command"
                | "cmd"
                | "option"
        )
    {
        return None;
    }
    // 최소 1개의 수식키를 요구해 일반 타이핑을 가로채지 않는다.
    if !ks.modifiers.control && !ks.modifiers.alt && !ks.modifiers.shift && !ks.modifiers.platform {
        return None;
    }
    let mut parts = Vec::new();
    if ks.modifiers.control {
        parts.push("Ctrl");
    }
    if ks.modifiers.alt {
        parts.push("Alt");
    }
    if ks.modifiers.shift {
        parts.push("Shift");
    }
    if ks.modifiers.platform {
        parts.push("Cmd");
    }
    let key_label = canonical_key_label(&raw_key);
    parts.push(key_label.as_str());
    Some(parts.join("+"))
}

/// 현재 눌린 `Keystroke`가 설정된 단축키 문자열(예: `"Alt+Shift+Space"`)과 일치하는지 판정한다.
#[must_use]
pub fn matches_shortcut(ks: &gpui::Keystroke, shortcut: &str) -> bool {
    let effective = if shortcut.trim().is_empty() {
        DEFAULT_DESKTOP_SHORTCUT
    } else {
        shortcut.trim()
    };
    let mut want_ctrl = false;
    let mut want_alt = false;
    let mut want_shift = false;
    let mut want_cmd = false;
    let mut want_key: Option<String> = None;

    for token in effective.split('+') {
        let norm = token.trim().to_ascii_lowercase();
        match norm.as_str() {
            "ctrl" | "control" => want_ctrl = true,
            "alt" | "opt" | "option" => want_alt = true,
            "shift" => want_shift = true,
            "cmd" | "command" | "meta" | "super" | "platform" => want_cmd = true,
            "" => {}
            other => want_key = Some(normalize_key_name(other)),
        }
    }
    let Some(expected_key) = want_key else {
        return false;
    };
    ks.modifiers.control == want_ctrl
        && ks.modifiers.alt == want_alt
        && ks.modifiers.shift == want_shift
        && ks.modifiers.platform == want_cmd
        && normalize_key_name(&ks.key) == expected_key
}

/// UI 키캡 표시용 문자열(macOS 기호 또는 Windows/Linux 텍스트).
#[must_use]
pub fn display_shortcut(shortcut: &str) -> String {
    let effective = if shortcut.trim().is_empty() {
        DEFAULT_DESKTOP_SHORTCUT
    } else {
        shortcut.trim()
    };
    #[cfg(target_os = "macos")]
    {
        let mut out = String::new();
        for token in effective.split('+') {
            match token.trim().to_ascii_lowercase().as_str() {
                "ctrl" | "control" => out.push('⌃'),
                "alt" | "opt" | "option" => out.push('⌥'),
                "shift" => out.push('⇧'),
                "cmd" | "command" | "meta" | "super" | "platform" => out.push('⌘'),
                "space" => out.push_str("Space"),
                other => out.push_str(&canonical_key_label(other)),
            }
        }
        out
    }
    #[cfg(not(target_os = "macos"))]
    {
        effective.to_owned()
    }
}

fn normalize_key_name(raw: &str) -> String {
    match raw.trim().to_ascii_lowercase().as_str() {
        " " | "space" | "spacebar" => "space".to_owned(),
        "return" | "enter" => "enter".to_owned(),
        other => other.to_owned(),
    }
}

fn canonical_key_label(raw: &str) -> String {
    match normalize_key_name(raw).as_str() {
        "space" => "Space".to_owned(),
        "enter" => "Enter".to_owned(),
        "tab" => "Tab".to_owned(),
        "backspace" => "Backspace".to_owned(),
        other if other.len() == 1 => other.to_ascii_uppercase(),
        other => {
            let mut chars = other.chars();
            match chars.next() {
                None => String::new(),
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[test]
    fn shortcut_format_and_match_roundtrip() {
        let ks = gpui::Keystroke {
            modifiers: gpui::Modifiers {
                alt: true,
                shift: true,
                ..gpui::Modifiers::default()
            },
            key: "space".to_owned(),
            key_char: None,
        };
        assert_eq!(format_keystroke(&ks).as_deref(), Some("Alt+Shift+Space"));
        assert!(matches_shortcut(&ks, "Alt+Shift+Space"));
        assert!(matches_shortcut(&ks, "Option+Shift+Space"));
        assert!(!matches_shortcut(&ks, "Cmd+Shift+Space"));

        // Bare key without modifiers is rejected
        let bare = gpui::Keystroke {
            modifiers: gpui::Modifiers::default(),
            key: "v".to_owned(),
            key_char: Some("v".to_owned()),
        };
        assert_eq!(format_keystroke(&bare), None);
    }

    #[test]
    fn transcribe_wav_chunks_large_audio_and_single_frames_small_audio() {
        let seen: Arc<Mutex<Vec<(u16, u16, usize)>>> = Arc::new(Mutex::new(Vec::new()));
        let seen_clone = Arc::clone(&seen);
        let call: VoiceCall = Arc::new(move |req| match req {
            VoiceRequest::Transcribe {
                audio_wav,
                upload_id,
                chunk_index,
                total_chunks,
                ..
            } => {
                seen_clone
                    .lock()
                    .unwrap()
                    .push((*chunk_index, *total_chunks, audio_wav.len()));
                if chunk_index + 1 < *total_chunks {
                    Ok(VoiceReply::ChunkAccepted {
                        upload_id: upload_id.clone().unwrap_or_default(),
                        received: chunk_index + 1,
                        total: *total_chunks,
                    })
                } else {
                    Ok(VoiceReply::Transcribed(VoiceTranscribeResult {
                        silence: false,
                        raw: "ok".to_owned(),
                        text: "OK.".to_owned(),
                        refined: true,
                        refine_error: None,
                        asr_ms: 50,
                        refine_ms: 20,
                    }))
                }
            }
            _ => unreachable!(),
        });

        // Small -> single frame
        let small = vec![1u8; 1024];
        let res = transcribe_wav(
            &call,
            &small,
            Some(VoiceTargetKind::Session),
            Some("1".into()),
            None,
        )
        .unwrap();
        assert_eq!(res.text, "OK.");
        assert_eq!(*seen.lock().unwrap(), vec![(0, 1, 1024)]);

        // Large (600 KiB) -> 3 chunks of 256, 256, 88 KiB
        seen.lock().unwrap().clear();
        let large = vec![2u8; 600 * 1024];
        let res = transcribe_wav(
            &call,
            &large,
            Some(VoiceTargetKind::GroupChat),
            Some("g1".into()),
            Some(false),
        )
        .unwrap();
        assert_eq!(res.text, "OK.");
        assert_eq!(
            *seen.lock().unwrap(),
            vec![(0, 3, 256 * 1024), (1, 3, 256 * 1024), (2, 3, 88 * 1024)]
        );
    }
}
