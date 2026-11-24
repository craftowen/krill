//! 데스크톱 16kHz 모노 16-bit PCM WAV 녹음기(외부 오디오 크레이트 없음).
//!
//! - macOS: `AudioToolbox.framework` `AudioQueueNewInput` (16kHz, 1ch, s16le)
//! - Windows: `winmm` `waveIn*` (16kHz, 1ch, s16le, `CALLBACK_EVENT`)
//! - Linux: `pw-record` / `arecord` 자식 프로세스(오디오 하드웨어 없는 CI에서도 깨끗이 컴파일)

/// 16kHz 모노 16-bit PCM 샘플 슬라이스를 표준 44바이트 RIFF/WAVE 바이트로 감싼다.
#[must_use]
pub fn encode_wav_16k_mono(samples: &[i16]) -> Vec<u8> {
    let data_bytes = u32::try_from(samples.len().saturating_mul(2)).unwrap_or(u32::MAX - 36);
    let riff_size = data_bytes.saturating_add(36);
    let mut out = Vec::with_capacity(44_usize.saturating_add(samples.len().saturating_mul(2)));
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&riff_size.to_le_bytes());
    out.extend_from_slice(b"WAVE");
    out.extend_from_slice(b"fmt ");
    out.extend_from_slice(&16u32.to_le_bytes()); // PCM fmt chunk size
    out.extend_from_slice(&1u16.to_le_bytes()); // AudioFormat = PCM
    out.extend_from_slice(&1u16.to_le_bytes()); // NumChannels = 1
    out.extend_from_slice(&16_000u32.to_le_bytes()); // SampleRate = 16000
    out.extend_from_slice(&32_000u32.to_le_bytes()); // ByteRate = 16000 * 1 * 2
    out.extend_from_slice(&2u16.to_le_bytes()); // BlockAlign = 2
    out.extend_from_slice(&16u16.to_le_bytes()); // BitsPerSample = 16
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_bytes.to_le_bytes());
    for &sample in samples {
        out.extend_from_slice(&sample.to_le_bytes());
    }
    out
}

/// 원시 s16le 바이트열을 16kHz 모노 WAV 바이트로 감싼다.
#[must_use]
pub fn wrap_s16le_wav_16k_mono(pcm_bytes: &[u8]) -> Vec<u8> {
    let even_len = pcm_bytes.len() & !1;
    let data_bytes = u32::try_from(even_len).unwrap_or(u32::MAX - 36);
    let riff_size = data_bytes.saturating_add(36);
    let mut out = Vec::with_capacity(44_usize.saturating_add(even_len));
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&riff_size.to_le_bytes());
    out.extend_from_slice(b"WAVE");
    out.extend_from_slice(b"fmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&16_000u32.to_le_bytes());
    out.extend_from_slice(&32_000u32.to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_bytes.to_le_bytes());
    if let Some(slice) = pcm_bytes.get(..even_len) {
        out.extend_from_slice(slice);
    }
    out
}

#[cfg(target_os = "macos")]
mod imp {
    use std::ffi::c_void;
    use std::ptr;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};

    use super::encode_wav_16k_mono;

    const K_AUDIO_FORMAT_LINEAR_PCM: u32 = u32::from_be_bytes(*b"lpcm");
    const K_AUDIO_FORMAT_FLAG_IS_SIGNED_INTEGER: u32 = 1 << 2;
    const K_AUDIO_FORMAT_FLAG_IS_PACKED: u32 = 1 << 3;
    const BUFFER_COUNT: usize = 3;
    /// 100ms @ 16kHz mono 16-bit = 3,200 bytes.
    const BUFFER_BYTES: u32 = 3_200;
    /// 최대 녹음 샘플 수(약 120초).
    const MAX_SAMPLES: usize = 16_000 * 120;

    type OSStatus = i32;
    type AudioQueueRef = *mut c_void;

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct AudioStreamBasicDescription {
        sample_rate: f64,
        format_id: u32,
        format_flags: u32,
        bytes_per_packet: u32,
        frames_per_packet: u32,
        bytes_per_frame: u32,
        channels_per_frame: u32,
        bits_per_channel: u32,
        reserved: u32,
    }

    #[repr(C)]
    struct AudioQueueBuffer {
        audio_data_bytes_capacity: u32,
        audio_data: *mut c_void,
        audio_data_byte_size: u32,
        user_data: *mut c_void,
        packet_description_capacity: u32,
        packet_descriptions: *const c_void,
        packet_description_count: u32,
    }

    type AudioQueueBufferRef = *mut AudioQueueBuffer;

    type AudioQueueInputCallback = extern "C" fn(
        user_data: *mut c_void,
        aq: AudioQueueRef,
        buffer: AudioQueueBufferRef,
        start_time: *const c_void,
        num_packets: u32,
        packet_descs: *const c_void,
    );

    #[link(name = "AudioToolbox", kind = "framework")]
    unsafe extern "C" {
        fn AudioQueueNewInput(
            in_format: *const AudioStreamBasicDescription,
            in_callback_proc: AudioQueueInputCallback,
            in_user_data: *mut c_void,
            in_callback_run_loop: *mut c_void,
            in_callback_run_loop_mode: *mut c_void,
            in_flags: u32,
            out_aq: *mut AudioQueueRef,
        ) -> OSStatus;
        fn AudioQueueAllocateBuffer(
            in_aq: AudioQueueRef,
            in_buffer_byte_size: u32,
            out_buffer: *mut AudioQueueBufferRef,
        ) -> OSStatus;
        fn AudioQueueEnqueueBuffer(
            in_aq: AudioQueueRef,
            in_buffer: AudioQueueBufferRef,
            in_num_packet_descs: u32,
            in_packet_descs: *const c_void,
        ) -> OSStatus;
        fn AudioQueueStart(in_aq: AudioQueueRef, in_start_time: *const c_void) -> OSStatus;
        fn AudioQueueStop(in_aq: AudioQueueRef, in_immediate: u8) -> OSStatus;
        fn AudioQueueDispose(in_aq: AudioQueueRef, in_immediate: u8) -> OSStatus;
    }

    struct CaptureState {
        samples: Mutex<Vec<i16>>,
        running: AtomicBool,
    }

    extern "C" fn input_callback(
        user_data: *mut c_void,
        aq: AudioQueueRef,
        buffer: AudioQueueBufferRef,
        _start_time: *const c_void,
        _num_packets: u32,
        _packet_descs: *const c_void,
    ) {
        std::panic::catch_unwind(|| {
            if user_data.is_null() || buffer.is_null() {
                return;
            }
            // SAFETY: `user_data`는 `AudioRecorder`가 `AudioQueueDispose` 전까지 유지하는 `CaptureState` 포인터다.
            let state = unsafe { &*(user_data as *const CaptureState) };
            // SAFETY: CoreAudio 콜백은 유효한 `AudioQueueBufferRef`를 전달한다.
            let buf = unsafe { &*buffer };
            if !buf.audio_data.is_null() && buf.audio_data_byte_size >= 2 {
                let sample_count = (buf.audio_data_byte_size as usize) / 2;
                // SAFETY: `audio_data`는 `audio_data_byte_size` 바이트의 s16le 버퍼를 가리킨다.
                let bytes = unsafe {
                    std::slice::from_raw_parts(buf.audio_data as *const u8, sample_count * 2)
                };
                let mut guard = state
                    .samples
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let remaining = MAX_SAMPLES.saturating_sub(guard.len());
                for &[lo, hi] in bytes.as_chunks::<2>().0.iter().take(remaining) {
                    guard.push(i16::from_le_bytes([lo, hi]));
                }
            }
            if state.running.load(Ordering::Acquire) {
                // SAFETY: 실행 중인 큐에 소유 버퍼를 다시 넣는다.
                let _ = unsafe { AudioQueueEnqueueBuffer(aq, buffer, 0, ptr::null()) };
            }
        })
        .ok();
    }

    pub struct AudioRecorder {
        queue: AudioQueueRef,
        state: *const CaptureState,
        // Drop 안전성을 위해 Arc로 소유권을 유지한다.
        owner: Arc<CaptureState>,
    }

    // SAFETY: `AudioQueueRef`는 CoreAudio 스레드 안전 API(`AudioQueueStop`/`AudioQueueDispose`)로만 닫는다.
    unsafe impl Send for AudioRecorder {}

    impl std::fmt::Debug for AudioRecorder {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("AudioRecorder").finish_non_exhaustive()
        }
    }

    impl AudioRecorder {
        pub fn start() -> Result<Self, String> {
            let fmt = AudioStreamBasicDescription {
                sample_rate: 16_000.0,
                format_id: K_AUDIO_FORMAT_LINEAR_PCM,
                format_flags: K_AUDIO_FORMAT_FLAG_IS_SIGNED_INTEGER | K_AUDIO_FORMAT_FLAG_IS_PACKED,
                bytes_per_packet: 2,
                frames_per_packet: 1,
                bytes_per_frame: 2,
                channels_per_frame: 1,
                bits_per_channel: 16,
                reserved: 0,
            };
            let state = Arc::new(CaptureState {
                samples: Mutex::new(Vec::with_capacity(16_000 * 4)),
                running: AtomicBool::new(true),
            });
            let state_ptr = Arc::as_ptr(&state);
            let mut queue: AudioQueueRef = ptr::null_mut();

            // SAFETY: 유효한 `fmt`, 콜백, `state_ptr`로 새 입력 AudioQueue를 생성한다.
            let status = unsafe {
                AudioQueueNewInput(
                    &raw const fmt,
                    input_callback,
                    state_ptr as *mut c_void,
                    ptr::null_mut(),
                    ptr::null_mut(),
                    0,
                    &raw mut queue,
                )
            };
            if status != 0 || queue.is_null() {
                return Err(format!(
                    "Microphone unavailable (AudioQueueNewInput {status})"
                ));
            }

            for _ in 0..BUFFER_COUNT {
                let mut buf: AudioQueueBufferRef = ptr::null_mut();
                // SAFETY: 생성된 `queue`에 `BUFFER_BYTES` 크기 버퍼를 할당한다.
                let alloc_status =
                    unsafe { AudioQueueAllocateBuffer(queue, BUFFER_BYTES, &raw mut buf) };
                if alloc_status != 0 || buf.is_null() {
                    // SAFETY: 실패 시 생성된 큐를 즉시 정리한다.
                    let _ = unsafe { AudioQueueDispose(queue, 1) };
                    return Err(format!(
                        "Microphone buffer allocation failed ({alloc_status})"
                    ));
                }
                // SAFETY: 방금 할당한 버퍼를 큐에 적재한다.
                let enq_status = unsafe { AudioQueueEnqueueBuffer(queue, buf, 0, ptr::null()) };
                if enq_status != 0 {
                    // SAFETY: 실패 시 큐를 즉시 정리한다.
                    let _ = unsafe { AudioQueueDispose(queue, 1) };
                    return Err(format!("Microphone buffer enqueue failed ({enq_status})"));
                }
            }

            // SAFETY: 버퍼 적재가 끝난 큐의 녹음을 시작한다.
            let start_status = unsafe { AudioQueueStart(queue, ptr::null()) };
            if start_status != 0 {
                // SAFETY: 시작 실패 시 큐를 즉시 정리한다.
                let _ = unsafe { AudioQueueDispose(queue, 1) };
                return Err(format!(
                    "Microphone start failed ({start_status}); check macOS Microphone permission"
                ));
            }

            Ok(Self {
                queue,
                state: state_ptr,
                owner: state,
            })
        }

        fn teardown(&mut self) {
            if !self.queue.is_null() {
                self.owner.running.store(false, Ordering::Release);
                // SAFETY: 즉시 정지하여 진행 중 콜백을 드레인한다.
                let _ = unsafe { AudioQueueStop(self.queue, 1) };
                // SAFETY: 동기 해제로 모든 콜백 종료를 보장한다(`owner` 해제 전).
                let _ = unsafe { AudioQueueDispose(self.queue, 1) };
                self.queue = ptr::null_mut();
            }
            let _ = self.state;
        }

        pub fn stop(mut self) -> Result<Vec<u8>, String> {
            self.teardown();
            let samples = self
                .owner
                .samples
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if samples.is_empty() {
                return Err("No audio captured from microphone".to_owned());
            }
            Ok(encode_wav_16k_mono(&samples))
        }

        pub fn cancel(mut self) {
            self.teardown();
        }
    }

    impl Drop for AudioRecorder {
        fn drop(&mut self) {
            self.teardown();
        }
    }
}

#[cfg(windows)]
mod imp {
    use std::ffi::c_void;
    use std::ptr;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    use std::thread;

    use super::wrap_s16le_wav_16k_mono;

    const WAVE_MAPPER: u32 = u32::MAX;
    const WAVE_FORMAT_PCM: u16 = 1;
    const CALLBACK_EVENT: u32 = 0x0005_0000;
    const WHDR_DONE: u32 = 0x0000_0001;
    const BUFFER_COUNT: usize = 3;
    const BUFFER_BYTES: usize = 3_200;
    const MAX_PCM_BYTES: usize = 16_000 * 2 * 120;

    type HWaveIn = *mut c_void;
    type MmResult = u32;

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct WaveFormatEx {
        w_format_tag: u16,
        n_channels: u16,
        n_samples_per_sec: u32,
        n_avg_bytes_per_sec: u32,
        n_block_align: u16,
        w_bits_per_sample: u16,
        cb_size: u16,
    }

    #[repr(C)]
    struct WaveHdr {
        lp_data: *mut u8,
        dw_buffer_length: u32,
        dw_bytes_recorded: u32,
        dw_user: usize,
        dw_flags: u32,
        dw_loops: u32,
        lp_next: *mut WaveHdr,
        reserved: usize,
    }

    #[link(name = "winmm")]
    unsafe extern "system" {
        fn waveInOpen(
            phwi: *mut HWaveIn,
            u_device_id: u32,
            pwfx: *const WaveFormatEx,
            dw_callback: usize,
            dw_instance: usize,
            fdw_open: u32,
        ) -> MmResult;
        fn waveInPrepareHeader(hwi: HWaveIn, pwh: *mut WaveHdr, cbwh: u32) -> MmResult;
        fn waveInUnprepareHeader(hwi: HWaveIn, pwh: *mut WaveHdr, cbwh: u32) -> MmResult;
        fn waveInAddBuffer(hwi: HWaveIn, pwh: *mut WaveHdr, cbwh: u32) -> MmResult;
        fn waveInStart(hwi: HWaveIn) -> MmResult;
        fn waveInStop(hwi: HWaveIn) -> MmResult;
        fn waveInReset(hwi: HWaveIn) -> MmResult;
        fn waveInClose(hwi: HWaveIn) -> MmResult;
    }

    pub struct AudioRecorder {
        stop_flag: Arc<AtomicBool>,
        pcm: Arc<Mutex<Vec<u8>>>,
        worker: Option<thread::JoinHandle<()>>,
    }

    impl std::fmt::Debug for AudioRecorder {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("AudioRecorder").finish_non_exhaustive()
        }
    }

    impl AudioRecorder {
        pub fn start() -> Result<Self, String> {
            let stop_flag = Arc::new(AtomicBool::new(false));
            let pcm = Arc::new(Mutex::new(Vec::with_capacity(16_000 * 8)));
            let (tx, rx) = std::sync::mpsc::channel::<Result<(), String>>();
            let stop_bg = Arc::clone(&stop_flag);
            let pcm_bg = Arc::clone(&pcm);

            let worker = thread::Builder::new()
                .name("krill-win-audio".to_owned())
                .spawn(move || {
                    run_winmm_capture(&stop_bg, &pcm_bg, &tx);
                })
                .map_err(|e| format!("Audio thread start failed: {e}"))?;

            match rx.recv() {
                Ok(Ok(())) => Ok(Self {
                    stop_flag,
                    pcm,
                    worker: Some(worker),
                }),
                Ok(Err(msg)) => {
                    worker.join().ok();
                    Err(msg)
                }
                Err(_) => {
                    worker.join().ok();
                    Err("Audio capture initialization failed".to_owned())
                }
            }
        }

        pub fn stop(mut self) -> Result<Vec<u8>, String> {
            self.stop_flag.store(true, Ordering::Release);
            if let Some(handle) = self.worker.take() {
                handle.join().ok();
            }
            let guard = self
                .pcm
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if guard.is_empty() {
                return Err("No audio captured from microphone".to_owned());
            }
            Ok(wrap_s16le_wav_16k_mono(&guard))
        }

        pub fn cancel(mut self) {
            self.stop_flag.store(true, Ordering::Release);
            if let Some(handle) = self.worker.take() {
                handle.join().ok();
            }
        }
    }

    impl Drop for AudioRecorder {
        fn drop(&mut self) {
            self.stop_flag.store(true, Ordering::Release);
            if let Some(handle) = self.worker.take() {
                handle.join().ok();
            }
        }
    }

    fn run_winmm_capture(
        stop_flag: &AtomicBool,
        pcm: &Mutex<Vec<u8>>,
        ready: &std::sync::mpsc::Sender<Result<(), String>>,
    ) {
        use windows_sys::Win32::Foundation::CloseHandle;
        use windows_sys::Win32::System::Threading::{CreateEventW, WaitForSingleObject};

        // SAFETY: 자동 리셋 Win32 이벤트 핸들을 생성한다.
        let event = unsafe { CreateEventW(ptr::null(), 0, 0, ptr::null()) };
        if event.is_null() {
            ready
                .send(Err("Failed to create audio event".to_owned()))
                .ok();
            return;
        }

        let fmt = WaveFormatEx {
            w_format_tag: WAVE_FORMAT_PCM,
            n_channels: 1,
            n_samples_per_sec: 16_000,
            n_avg_bytes_per_sec: 32_000,
            n_block_align: 2,
            w_bits_per_sample: 16,
            cb_size: 0,
        };
        let mut hwi: HWaveIn = ptr::null_mut();
        // SAFETY: 기본 마이크 장치를 16kHz 모노 16-bit PCM 이벤트 모드로 연다.
        let open_res = unsafe {
            waveInOpen(
                &raw mut hwi,
                WAVE_MAPPER,
                &raw const fmt,
                event as usize,
                0,
                CALLBACK_EVENT,
            )
        };
        if open_res != 0 || hwi.is_null() {
            // SAFETY: 실패 시 이벤트 핸들을 닫는다.
            let _ = unsafe { CloseHandle(event) };
            ready
                .send(Err(format!(
                    "Microphone unavailable (waveInOpen {open_res})"
                )))
                .ok();
            return;
        }

        let mut raw_bufs: Vec<Vec<u8>> =
            (0..BUFFER_COUNT).map(|_| vec![0u8; BUFFER_BYTES]).collect();
        let mut hdrs: Vec<WaveHdr> = raw_bufs
            .iter_mut()
            .map(|b| WaveHdr {
                lp_data: b.as_mut_ptr(),
                dw_buffer_length: u32::try_from(BUFFER_BYTES).unwrap_or(3200),
                dw_bytes_recorded: 0,
                dw_user: 0,
                dw_flags: 0,
                dw_loops: 0,
                lp_next: ptr::null_mut(),
                reserved: 0,
            })
            .collect();
        let hdr_size = u32::try_from(std::mem::size_of::<WaveHdr>()).unwrap_or(0);

        for hdr in &mut hdrs {
            let hdr_ptr = &raw mut *hdr;
            // SAFETY: 버퍼를 waveIn 장치에 준비한다.
            let _ = unsafe { waveInPrepareHeader(hwi, hdr_ptr, hdr_size) };
            // SAFETY: 준비된 버퍼를 장치 큐에 추가한다.
            let _ = unsafe { waveInAddBuffer(hwi, hdr_ptr, hdr_size) };
        }
        // SAFETY: 버퍼 추가 후 녹음을 시작한다.
        let start_res = unsafe { waveInStart(hwi) };
        if start_res != 0 {
            // SAFETY: 실패 시 장치를 리셋한다.
            let _ = unsafe { waveInReset(hwi) };
            for hdr in &mut hdrs {
                let hdr_ptr = &raw mut *hdr;
                // SAFETY: 준비된 헤더를 해제한다.
                let _ = unsafe { waveInUnprepareHeader(hwi, hdr_ptr, hdr_size) };
            }
            // SAFETY: 장치를 닫는다.
            let _ = unsafe { waveInClose(hwi) };
            // SAFETY: 이벤트 핸들을 닫는다.
            let _ = unsafe { CloseHandle(event) };
            ready
                .send(Err(format!("Microphone start failed ({start_res})")))
                .ok();
            return;
        }
        ready.send(Ok(())).ok();

        while !stop_flag.load(Ordering::Acquire) {
            // SAFETY: 최대 50ms 대기하며 완료된 버퍼를 수거한다.
            let _ = unsafe { WaitForSingleObject(event, 50) };
            for (hdr, buf) in hdrs.iter_mut().zip(raw_bufs.iter()) {
                if hdr.dw_flags & WHDR_DONE != 0 {
                    let recorded = (hdr.dw_bytes_recorded as usize).min(buf.len());
                    if recorded > 0
                        && let Some(slice) = buf.get(..recorded)
                    {
                        let mut guard = pcm
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner);
                        let room = MAX_PCM_BYTES.saturating_sub(guard.len());
                        if let Some(capped) = slice.get(..recorded.min(room)) {
                            guard.extend_from_slice(capped);
                        }
                    }
                    if !stop_flag.load(Ordering::Acquire) {
                        hdr.dw_bytes_recorded = 0;
                        let hdr_ptr = &raw mut *hdr;
                        // SAFETY: 완료된 버퍼를 다시 장치에 넣는다.
                        let _ = unsafe { waveInAddBuffer(hwi, hdr_ptr, hdr_size) };
                    }
                }
            }
        }

        // SAFETY: 녹음을 중지한다.
        let _ = unsafe { waveInStop(hwi) };
        // SAFETY: 대기 중 버퍼를 모두 반환시킨다.
        let _ = unsafe { waveInReset(hwi) };
        for (hdr, buf) in hdrs.iter_mut().zip(raw_bufs.iter()) {
            let recorded = (hdr.dw_bytes_recorded as usize).min(buf.len());
            if recorded > 0
                && let Some(slice) = buf.get(..recorded)
            {
                let mut guard = pcm
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let room = MAX_PCM_BYTES.saturating_sub(guard.len());
                if let Some(capped) = slice.get(..recorded.min(room)) {
                    guard.extend_from_slice(capped);
                }
            }
            let hdr_ptr = &raw mut *hdr;
            // SAFETY: 헤더 준비를 해제한다.
            let _ = unsafe { waveInUnprepareHeader(hwi, hdr_ptr, hdr_size) };
        }
        // SAFETY: 입력 장치를 닫는다.
        let _ = unsafe { waveInClose(hwi) };
        // SAFETY: 이벤트 핸들을 닫는다.
        let _ = unsafe { CloseHandle(event) };
    }
}

#[cfg(all(unix, not(target_os = "macos")))]
mod imp {
    use std::io::Read;
    use std::process::{Child, Command, Stdio};
    use std::sync::{Arc, Mutex};
    use std::thread;

    use super::wrap_s16le_wav_16k_mono;

    const MAX_PCM_BYTES: usize = 16_000 * 2 * 120;

    pub struct AudioRecorder {
        child: Option<Child>,
        pcm: Arc<Mutex<Vec<u8>>>,
        reader: Option<thread::JoinHandle<()>>,
    }

    impl std::fmt::Debug for AudioRecorder {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("AudioRecorder").finish_non_exhaustive()
        }
    }

    impl AudioRecorder {
        pub fn start() -> Result<Self, String> {
            let mut child = spawn_linux_recorder()?;
            let stdout = child
                .stdout
                .take()
                .ok_or_else(|| "Audio capture stdout unavailable".to_owned())?;
            let pcm = Arc::new(Mutex::new(Vec::with_capacity(16_000 * 8)));
            let pcm_bg = Arc::clone(&pcm);
            let reader = thread::Builder::new()
                .name("krill-linux-audio".to_owned())
                .spawn(move || {
                    let mut out = stdout;
                    let mut buf = [0u8; 4096];
                    loop {
                        match out.read(&mut buf) {
                            Ok(0) | Err(_) => break,
                            Ok(n) => {
                                let mut guard = pcm_bg
                                    .lock()
                                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                                let room = MAX_PCM_BYTES.saturating_sub(guard.len());
                                if room == 0 {
                                    break;
                                }
                                if let Some(slice) = buf.get(..n.min(room)) {
                                    guard.extend_from_slice(slice);
                                }
                            }
                        }
                    }
                })
                .map_err(|e| format!("Audio reader thread failed: {e}"))?;
            Ok(Self {
                child: Some(child),
                pcm,
                reader: Some(reader),
            })
        }

        pub fn stop(mut self) -> Result<Vec<u8>, String> {
            self.terminate();
            let guard = self
                .pcm
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if guard.is_empty() {
                return Err("No audio captured from microphone".to_owned());
            }
            Ok(wrap_s16le_wav_16k_mono(&guard))
        }

        pub fn cancel(mut self) {
            self.terminate();
        }

        fn terminate(&mut self) {
            if let Some(mut child) = self.child.take() {
                child.kill().ok();
                child.wait().ok();
            }
            if let Some(reader) = self.reader.take() {
                reader.join().ok();
            }
        }
    }

    impl Drop for AudioRecorder {
        fn drop(&mut self) {
            self.terminate();
        }
    }

    fn spawn_linux_recorder() -> Result<Child, String> {
        let candidates: &[(&str, &[&str])] = &[
            (
                "pw-record",
                &["--format=s16", "--rate=16000", "--channels=1", "-"],
            ),
            (
                "arecord",
                &[
                    "-q", "-f", "S16_LE", "-r", "16000", "-c", "1", "-t", "raw", "-",
                ],
            ),
        ];
        for (bin, args) in candidates {
            if let Ok(child) = Command::new(bin)
                .args(*args)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
            {
                return Ok(child);
            }
        }
        Err("Microphone capture unavailable (pw-record/arecord not found)".to_owned())
    }
}

pub use imp::AudioRecorder;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_wav_16k_mono_produces_valid_riff_header() {
        let wav = encode_wav_16k_mono(&[0, 1000, -1000, 32767]);
        assert_eq!(wav.len(), 44 + 8);
        assert_eq!(&wav[0..4], b"RIFF");
        assert_eq!(&wav[8..12], b"WAVE");
        assert_eq!(&wav[12..16], b"fmt ");
        assert_eq!(u16::from_le_bytes([wav[20], wav[21]]), 1); // PCM
        assert_eq!(u16::from_le_bytes([wav[22], wav[23]]), 1); // mono
        assert_eq!(
            u32::from_le_bytes([wav[24], wav[25], wav[26], wav[27]]),
            16_000
        );
        assert_eq!(u16::from_le_bytes([wav[34], wav[35]]), 16); // 16-bit
        assert_eq!(&wav[36..40], b"data");
        assert_eq!(u32::from_le_bytes([wav[40], wav[41], wav[42], wav[43]]), 8);
    }

    #[test]
    fn wrap_s16le_truncates_odd_trailing_byte() {
        let wav = wrap_s16le_wav_16k_mono(&[1, 2, 3, 4, 5]);
        assert_eq!(wav.len(), 44 + 4);
        assert_eq!(u32::from_le_bytes([wav[40], wav[41], wav[42], wav[43]]), 4);
    }
}
