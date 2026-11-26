//! Krill + Pipito 로컬 음성 받아쓰기 서비스(`krilld`).
//!
//! - Pipito Local Bridge v1 클라이언트([`bridge`])
//! - 음성 설정·자동 학습 단어장 영구 저장소([`vocab`])
//! - 0ms 포커스 세션 경량 문맥 캐시([`context`])
//! - 멀티 청크 WAV 조립기(모바일 E2EE·데스크톱 IPC 1 MiB 프레임 한도 대응)

pub mod bridge;
pub mod context;
pub mod vocab;

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::thread;
use std::time::{Duration, Instant};

use krill_proto::voice::{
    MAX_VOICE_AUDIO_CHUNK_BYTES, MAX_VOICE_TOTAL_CHUNKS, VoiceConfig, VoiceConfigPatch,
    VoiceConnectionStatus, VoiceFeedbackResult, VoiceReply, VoiceRequest, VoiceStatusInfo,
    VoiceTargetKind, VoiceTranscribeResult,
};

use bridge::{BridgeClient, TranscribePayload, base64_encode};
use context::ContextCache;
use vocab::{VocabStore, now_unix_ms};

/// 청크 업로드 만료 시간.
const CHUNK_UPLOAD_TTL: Duration = Duration::from_secs(60);
/// 조립된 전체 WAV 상한(8 MiB ≒ 16kHz 16-bit 모노 약 4분).
const MAX_ASSEMBLED_WAV_BYTES: usize = 8 * 1024 * 1024;
/// 동시 진행 중인 미완료 청크 업로드 상한.
const MAX_PENDING_UPLOADS: usize = 8;
/// 일반 프롬프트 학습 디스크 플러시 간격(ms).
const AUTO_VOCAB_FLUSH_INTERVAL_MS: u64 = 5_000;

#[derive(Debug)]
struct PendingUpload {
    total: u16,
    chunks: Vec<Option<Vec<u8>>>,
    total_bytes: usize,
    created_at: Instant,
}

#[derive(Debug, Default)]
struct ChunkUploadStore {
    uploads: HashMap<String, PendingUpload>,
}

impl ChunkUploadStore {
    fn put(
        &mut self,
        upload_id: &str,
        chunk_index: u16,
        total_chunks: u16,
        data: Vec<u8>,
    ) -> io::Result<ChunkOutcome> {
        self.prune_expired();
        if total_chunks == 0 || total_chunks > MAX_VOICE_TOTAL_CHUNKS {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid total_chunks",
            ));
        }
        if chunk_index >= total_chunks {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "chunk_index out of range",
            ));
        }
        if data.len() > MAX_VOICE_AUDIO_CHUNK_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "chunk exceeds byte limit",
            ));
        }
        if !self.uploads.contains_key(upload_id)
            && self.uploads.len() >= MAX_PENDING_UPLOADS
            && let Some(oldest) = self
                .uploads
                .iter()
                .min_by_key(|(_, u)| u.created_at)
                .map(|(k, _)| k.clone())
        {
            self.uploads.remove(&oldest);
        }
        let entry = self
            .uploads
            .entry(upload_id.to_owned())
            .or_insert_with(|| PendingUpload {
                total: total_chunks,
                chunks: vec![None; usize::from(total_chunks)],
                total_bytes: 0,
                created_at: Instant::now(),
            });
        if entry.total != total_chunks {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "mismatched total_chunks",
            ));
        }
        let idx = usize::from(chunk_index);
        if let Some(slot) = entry.chunks.get_mut(idx) {
            let prev_len = slot.as_ref().map_or(0, Vec::len);
            let next_total = entry
                .total_bytes
                .saturating_sub(prev_len)
                .saturating_add(data.len());
            if next_total > MAX_ASSEMBLED_WAV_BYTES {
                self.uploads.remove(upload_id);
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "assembled audio exceeds size limit",
                ));
            }
            entry.total_bytes = next_total;
            *slot = Some(data);
        }
        let received = u16::try_from(entry.chunks.iter().filter(|c| c.is_some()).count())
            .unwrap_or(total_chunks);
        if received < total_chunks {
            return Ok(ChunkOutcome::Pending {
                received,
                total: total_chunks,
            });
        }
        let completed = self
            .uploads
            .remove(upload_id)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "chunk upload disappeared"))?;
        let mut assembled = Vec::with_capacity(completed.total_bytes);
        for chunk in completed.chunks.into_iter().flatten() {
            assembled.extend_from_slice(&chunk);
        }
        Ok(ChunkOutcome::Complete(assembled))
    }

    fn prune_expired(&mut self) {
        let now = Instant::now();
        self.uploads
            .retain(|_, u| now.duration_since(u.created_at) < CHUNK_UPLOAD_TTL);
    }
}

enum ChunkOutcome {
    Pending { received: u16, total: u16 },
    Complete(Vec<u8>),
}

#[derive(Debug)]
pub struct TranscribeInput<'a> {
    pub audio_wav: Vec<u8>,
    pub target_kind: Option<VoiceTargetKind>,
    pub target_id: Option<&'a str>,
    pub refine_override: Option<bool>,
    pub target_lang: Option<&'a str>,
    pub upload_id: Option<&'a str>,
    pub chunk_index: u16,
    pub total_chunks: u16,
}

#[derive(Debug)]
pub struct VoiceService {
    bridge: BridgeClient,
    vocab: RwLock<VocabStore>,
    context: Arc<ContextCache>,
    uploads: Mutex<ChunkUploadStore>,
    bootstrapped: AtomicBool,
}

impl VoiceService {
    #[must_use]
    pub fn new(state_dir: Option<&Path>) -> Self {
        Self::with_bridge_override(state_dir, None)
    }

    #[must_use]
    pub fn with_bridge_override(state_dir: Option<&Path>, bridge_path: Option<PathBuf>) -> Self {
        Self {
            bridge: BridgeClient::new(bridge_path),
            vocab: RwLock::new(VocabStore::load(state_dir)),
            context: Arc::new(ContextCache::new()),
            uploads: Mutex::new(ChunkUploadStore::default()),
            bootstrapped: AtomicBool::new(false),
        }
    }

    /// Pipito 연결 상태와 Krill 음성 설정 요약을 반환한다.
    #[must_use]
    pub fn status(&self) -> VoiceStatusInfo {
        let probe = self.bridge.health();
        let (refine_enabled, vocab_count) = {
            let guard = self
                .vocab
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            (guard.refine_enabled(), guard.vocab_count())
        };
        VoiceStatusInfo {
            available: probe.status == VoiceConnectionStatus::Ready,
            status: probe.status,
            app_version: probe.app_version,
            engine_ready: probe.engine_ready,
            engine_error: probe.engine_error,
            refine_available: probe.refine_available,
            refine_enabled,
            vocab_count,
        }
    }

    /// 비차단 방식으로 Pipito `/local/warm`과 포커스 대상 Git 문맥 갱신을 트리거한다.
    pub fn warm(
        &self,
        target_kind: Option<VoiceTargetKind>,
        target_id: Option<&str>,
        target_cwd: Option<PathBuf>,
    ) -> bool {
        if let (Some(kind), Some(id)) = (target_kind, target_id) {
            self.context.schedule_git_refresh(kind, id, target_cwd);
        }
        let bridge = self.bridge.clone();
        thread::Builder::new()
            .name("krill-voice-warm".to_owned())
            .stack_size(128 * 1024)
            .spawn(move || {
                bridge.warm().ok();
            })
            .ok();
        true
    }

    #[must_use]
    pub fn config_get(&self) -> VoiceConfig {
        self.vocab
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .config()
    }

    #[must_use]
    pub fn target_lang(&self) -> Option<String> {
        self.vocab
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .target_lang()
            .map(str::to_owned)
    }

    pub fn set_target_lang(&self, target_lang: Option<&str>) -> io::Result<Option<String>> {
        self.vocab
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .set_target_lang(target_lang)
    }

    pub fn config_set(&self, patch: VoiceConfigPatch) -> io::Result<VoiceConfig> {
        self.vocab
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .apply_patch(patch)
    }

    /// 단일 프레임 또는 멀티 청크 WAV를 받아 Pipito `/local/transcribe`로 전달한다.
    /// 문맥 조회와 단어장 병합은 메모리 캐시에서만 <1ms로 수행한다.
    pub fn transcribe(&self, input: TranscribeInput<'_>) -> io::Result<VoiceReply> {
        let TranscribeInput {
            audio_wav,
            target_kind,
            target_id,
            refine_override,
            target_lang,
            upload_id,
            chunk_index,
            total_chunks,
        } = input;
        let wav_bytes = if total_chunks > 1 {
            let uid = upload_id
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "upload_id required for chunked audio",
                    )
                })?;
            let outcome = self
                .uploads
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .put(uid, chunk_index, total_chunks, audio_wav)?;
            match outcome {
                ChunkOutcome::Pending { received, total } => {
                    return Ok(VoiceReply::ChunkAccepted {
                        upload_id: uid.to_owned(),
                        received,
                        total,
                    });
                }
                ChunkOutcome::Complete(assembled) => assembled,
            }
        } else {
            audio_wav
        };

        if wav_bytes.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "empty audio payload",
            ));
        }
        if wav_bytes.len() > MAX_ASSEMBLED_WAV_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "audio exceeds size cap",
            ));
        }

        // 0ms 핫 패스: 메모리 캐시에서만 문맥 스냅샷 + 병합 단어장을 읽는다.
        let snapshot = self.context.snapshot(target_kind, target_id);
        let now_ms = now_unix_ms();
        let (refine, vocab, effective_target_lang) = {
            let guard = self
                .vocab
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let refine = refine_override.unwrap_or_else(|| guard.refine_enabled());
            let vocab = guard.merged_vocab(&snapshot.session_vocab, now_ms);
            let effective_lang = match target_lang {
                Some(raw) => vocab::normalize_target_lang(raw),
                None => guard.target_lang().map(str::to_owned),
            };
            (refine, vocab, effective_lang)
        };

        let audio_wav_b64 = base64_encode(&wav_bytes);
        let payload = TranscribePayload {
            audio_wav_b64: &audio_wav_b64,
            refine,
            mode: "refine",
            target_lang: effective_target_lang.as_deref(),
            vocab: &vocab,
            surrounding: &snapshot.surrounding,
            app: "Krill",
            app_category: snapshot.app_category,
        };
        let resp = self.bridge.transcribe(&payload)?;
        Ok(VoiceReply::Transcribed(VoiceTranscribeResult {
            silence: resp.silence,
            raw: resp.raw,
            text: resp.text,
            refined: resp.refined,
            refine_error: resp.refine_error,
            asr_ms: resp.asr_ms,
            refine_ms: resp.refine_ms,
        }))
    }

    /// 전송 전 수정 피드백을 즉시 `auto_vocab`에 반영하고 Pipito `/local/learn`에 비동기 전달한다.
    pub fn feedback(
        &self,
        before: &str,
        after: &str,
        _target_kind: Option<VoiceTargetKind>,
        _target_id: Option<&str>,
    ) -> VoiceFeedbackResult {
        let now_ms = now_unix_ms();
        let boosted = self
            .vocab
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .boost_corrections(before, after, now_ms);

        let before_owned = before.to_owned();
        let after_owned = after.to_owned();
        let bridge = self.bridge.clone();
        // 백그라운드로 Pipito `/local/learn` 호출(핫 패스 비차단).
        thread::Builder::new()
            .name("krill-voice-learn".to_owned())
            .stack_size(128 * 1024)
            .spawn(move || {
                bridge.learn(&before_owned, &after_owned).ok();
            })
            .ok();

        VoiceFeedbackResult {
            learned: Vec::new(),
            boosted,
        }
    }

    /// IPC `VoiceRequest` 처리 진입점.
    pub fn handle_request(
        &self,
        req: VoiceRequest,
        resolve_cwd: impl FnOnce(VoiceTargetKind, &str) -> Option<PathBuf>,
    ) -> VoiceReply {
        match req {
            VoiceRequest::Status => VoiceReply::Status(self.status()),
            VoiceRequest::Warm {
                target_kind,
                target_id,
            } => {
                let cwd = match (target_kind, target_id.as_deref()) {
                    (Some(kind), Some(id)) => resolve_cwd(kind, id),
                    _ => None,
                };
                VoiceReply::Warmed {
                    ok: self.warm(target_kind, target_id.as_deref(), cwd),
                }
            }
            VoiceRequest::Transcribe {
                audio_wav,
                target_kind,
                target_id,
                refine,
                upload_id,
                chunk_index,
                total_chunks,
            } => match self.transcribe(TranscribeInput {
                audio_wav,
                target_kind,
                target_id: target_id.as_deref(),
                refine_override: refine,
                target_lang: None,
                upload_id: upload_id.as_deref(),
                chunk_index,
                total_chunks,
            }) {
                Ok(reply) => reply,
                Err(err) => VoiceReply::Error(err.to_string()),
            },
            VoiceRequest::Feedback {
                before,
                after,
                target_kind,
                target_id,
            } => VoiceReply::FeedbackRecorded(self.feedback(
                &before,
                &after,
                target_kind,
                target_id.as_deref(),
            )),
            VoiceRequest::ConfigGet => VoiceReply::Config(self.config_get()),
            VoiceRequest::ConfigSet(patch) => match self.config_set(patch) {
                Ok(cfg) => VoiceReply::Config(cfg),
                Err(err) => VoiceReply::Error(err.to_string()),
            },
        }
    }

    /// 사용자가 프롬프트·메시지를 보낼 때 비차단 경로로 자동 단어장과 문맥 캐시를 갱신한다.
    pub fn observe_prompt(
        &self,
        kind: VoiceTargetKind,
        target_id: &str,
        text: &str,
        cwd: Option<PathBuf>,
    ) {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return;
        }
        self.context
            .push_turn(kind, target_id, "user", trimmed, cwd.clone());
        if cwd.is_some() && self.bridge.is_installed() {
            self.context.schedule_git_refresh(kind, target_id, cwd);
        }
        let now_ms = now_unix_ms();
        let mut guard = self
            .vocab
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        guard.learn_text(trimmed, 1, now_ms);
        guard.flush_if_due(now_ms, AUTO_VOCAB_FLUSH_INTERVAL_MS);
    }

    /// 폴링·대화 응답에서 최근 턴을 문맥 캐시에 반영한다(디스크 쓰기 없음).
    pub fn observe_turns(
        &self,
        kind: VoiceTargetKind,
        target_id: &str,
        turns: &[(String, String)],
        cwd: Option<PathBuf>,
    ) {
        self.context
            .replace_turns(kind, target_id, turns, cwd.clone());
        if cwd.is_some() && self.bridge.is_installed() {
            self.context.schedule_git_refresh(kind, target_id, cwd);
        }
    }

    /// 기동 시(또는 첫 접근 시) 1회만 백그라운드 부트스트랩 스캔으로 기존 프롬프트·레포·봇 이름을 시드한다.
    pub fn bootstrap_once(self: &Arc<Self>, prompts: Vec<String>, names: Vec<String>) {
        if self.bootstrapped.swap(true, Ordering::AcqRel) {
            return;
        }
        if prompts.is_empty() && names.is_empty() {
            return;
        }
        let service = Arc::clone(self);
        thread::Builder::new()
            .name("krill-voice-boot".to_owned())
            .stack_size(128 * 1024)
            .spawn(move || {
                let now_ms = now_unix_ms();
                let mut guard = service
                    .vocab
                    .write()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                for prompt in prompts.iter().take(200) {
                    guard.learn_text(prompt, 1, now_ms);
                }
                guard.record_tokens(&names, 2, now_ms);
                // Keep bootstrap strictly in-memory so a detached background thread never
                // creates files in state_dir concurrently with teardown/cleanup; persistence
                // runs synchronously on the next prompt flush, config update, or feedback.
            })
            .ok();
    }

    #[must_use]
    pub fn context_snapshot(
        &self,
        kind: Option<VoiceTargetKind>,
        id: Option<&str>,
    ) -> context::ContextSnapshot {
        self.context.snapshot(kind, id)
    }
}

#[cfg(test)]
mod tests;
