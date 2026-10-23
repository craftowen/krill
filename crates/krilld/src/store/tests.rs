use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime};

use krill_proto::SessionId;
use krill_proto::holder::WinSize;

use super::segment::{self, Base, HEADER_LEN, Header, Record};
use super::{Limits, Store};

const SIZE: WinSize = WinSize { cols: 80, rows: 24 };

fn tmp(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("mk024-{name}-{}", std::process::id()));
    fs::remove_dir_all(&dir).ok();
    dir
}

fn small(seal: u64) -> Limits {
    Limits {
        seal_bytes: seal,
        ..Limits::default()
    }
}

/// 결정적 비압축 바이트(xorshift).
pub(super) fn noise(len: usize, mut x: u64) -> Vec<u8> {
    (0..len)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x.to_le_bytes()[0]
        })
        .collect()
}

fn lines(from: usize, to: usize) -> Vec<u8> {
    let mut out = Vec::new();
    for i in from..to {
        write!(out, "line-{i:06} 한글 ok\r\n").unwrap();
    }
    out
}

#[test]
fn header_fixture() {
    let h = Header {
        base: Base::Continue,
        encrypted: false,
        session: SessionId(7),
        start_seq: 0x0102_0304,
        size: SIZE,
    };
    let bytes = h.encode();
    // 포맷 v1 고정 바이트. 바뀌면 이전 krilld가 쓴 세그먼트를 못 읽는다(버전을 올려야 한다).
    let fixture: [u8; HEADER_LEN] = [
        b'M', b'K', b'S', b'G', 1, 0, 1, 0, 7, 0, 0, 0, 0, 0, 0, 0, 4, 3, 2, 1, 0, 0, 0, 0, 80, 0,
        24, 0, 0x09, 0x9e, 0xe0, 0x6a,
    ];
    assert_eq!(bytes, fixture, "{bytes:02x?}");
    assert_eq!(Header::decode(&fixture), Some(h));
    let mut bad = fixture;
    bad[9] ^= 1;
    assert_eq!(Header::decode(&bad), None, "체크섬");
    let mut v2 = fixture;
    v2[4] = 3;
    assert_eq!(Header::decode(&v2), None, "모르는 버전");
}

/// v1 세그먼트 파일 전체 fixture(헤더 + 레코드 3개 청크 + 봉인 꼬리). 지금 코드가 만든 것을 고정했다 — 이후 버전도 읽어야 한다.
pub(super) const SEGMENT_V1: &[u8] = &[
    0x4d, 0x4b, 0x53, 0x47, 0x01, 0x00, 0x00, 0x00, 0x2a, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x50, 0x00, 0x18, 0x00, 0x6f, 0x35, 0x0f, 0x58,
    0x15, 0x00, 0x00, 0x00, 0x85, 0xa6, 0x85, 0xfa, 0x28, 0xb5, 0x2f, 0xfd, 0x00, 0x50, 0x60, 0x00,
    0x00, 0x01, 0x07, 0x00, 0x00, 0x00, 0x68, 0x69, 0x0d, 0x0a, 0x1b, 0x5b, 0x6d, 0x0c, 0x00, 0x00,
    0x00, 0x96, 0x7e, 0x03, 0x87, 0x48, 0x00, 0x00, 0x02, 0x04, 0x00, 0x00, 0x00, 0x64, 0x00, 0x1e,
    0x00, 0x10, 0x00, 0x00, 0x00, 0xc9, 0x4d, 0xcf, 0x55, 0x68, 0x00, 0x00, 0x03, 0x08, 0x00, 0x00,
    0x00, 0x05, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x03, 0x00, 0x00, 0x00, 0x25, 0xb3, 0x83,
    0xfe, 0x01, 0x00, 0x00,
];

#[test]
fn segment_fixture_reads() {
    let dir = tmp("fixture");
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join("seg.krs");
    fs::write(&path, SEGMENT_V1).unwrap();
    let seg = segment::read(&path, None).unwrap().unwrap();
    assert_eq!(seg.header.session, SessionId(42));
    assert_eq!(seg.header.base, Base::Start);
    assert_eq!(
        seg.records,
        vec![
            Record::Data(b"hi\r\n\x1b[m".to_vec()),
            Record::Resize(WinSize {
                cols: 100,
                rows: 30
            }),
            Record::Gap(5),
        ]
    );
    assert!(seg.sealed);
    assert_eq!(seg.end_seq(), 12);
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn roundtrip_and_permissions() {
    let root = tmp("roundtrip");
    let store = Store::open(&root, Limits::default()).unwrap();
    let id = SessionId(42);
    let mut rec = store.recorder(id, SIZE).unwrap();
    rec.append(b"hi\r\n\x1b[m");
    rec.resize(WinSize {
        cols: 100,
        rows: 30,
    });
    rec.seal();
    let dir = store.session_dir(id);
    assert!(dir.ends_with("sessions/42.noindex"));
    let mode = |p: &std::path::Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&dir), 0o700);
    assert_eq!(mode(&root.join("sessions")), 0o700);
    let segs = super::segments(&dir).unwrap();
    assert_eq!(segs.len(), 1);
    assert_eq!(mode(&segs[0]), 0o600);
    let seg = segment::read(&segs[0], None).unwrap().unwrap();
    assert_eq!(seg.header.base, Base::Start);
    assert_eq!(
        seg.records,
        vec![
            Record::Data(b"hi\r\n\x1b[m".to_vec()),
            Record::Resize(WinSize {
                cols: 100,
                rows: 30
            })
        ]
    );
    assert!(seg.sealed);
    assert_eq!(store.written(), fs::metadata(&segs[0]).unwrap().len());
    fs::remove_dir_all(&root).ok();
}

#[test]
fn one_stream_encoder_per_segment() {
    // 레코드마다 인코더를 새로 만들면 반복 내용이 압축되지 않는다. 스트림 하나면 두 번째 레코드부터 거의 공짜다.
    let root = tmp("stream");
    let store = Store::open(&root, Limits::default()).unwrap();
    let mut rec = store.recorder(SessionId(1), SIZE).unwrap();
    let block = noise(16 << 10, 7);
    rec.append(&block);
    let first = store.written();
    for _ in 0..9 {
        rec.append(&block);
    }
    let rest = store.written() - first;
    assert!(
        rest < 9 * 200,
        "같은 16KiB 9번이 {rest}B — 레코드 사이에서 창을 공유하지 않는다"
    );
    let segs = super::segments(&store.session_dir(SessionId(1))).unwrap();
    assert_eq!(segs.len(), 1);
    assert_eq!(
        segment::read(&segs[0], None)
            .unwrap()
            .unwrap()
            .records
            .len(),
        10
    );
    fs::remove_dir_all(&root).ok();
}

#[test]
fn torn_tail_and_corruption_are_dropped() {
    let root = tmp("torn");
    let store = Store::open(&root, Limits::default()).unwrap();
    let id = SessionId(3);
    let mut rec = store.recorder(id, SIZE).unwrap();
    for i in 0..5 {
        rec.append(&lines(i * 10, i * 10 + 10));
    }
    drop(rec); // 봉인 없이 끝남 = krilld 크래시와 같은 디스크 상태
    let path = super::segments(&store.session_dir(id)).unwrap().remove(0);
    let good = fs::read(&path).unwrap();

    // 잘린 꼬리: 마지막 청크 중간까지만 남음.
    let full = segment::read(&path, None).unwrap().unwrap();
    assert_eq!(full.records.len(), 5);
    assert!(!full.sealed);
    fs::write(&path, &good[..good.len() - 3]).unwrap();
    let torn = segment::read(&path, None).unwrap().unwrap();
    assert_eq!(torn.records.len(), 4);
    assert!(torn.valid_len < (good.len() - 3) as u64);

    // 손상 구간: 세 번째 청크 본문 1비트 → 그 청크부터 뒤를 버린다.
    let mut bad = good.clone();
    let third = {
        let mut off = HEADER_LEN;
        for _ in 0..2 {
            let len = u32::from_le_bytes(bad[off..off + 4].try_into().unwrap()) as usize;
            off += 8 + len;
        }
        off + 8 + 1
    };
    bad[third] ^= 0x10;
    fs::write(&path, &bad).unwrap();
    assert_eq!(
        segment::read(&path, None).unwrap().unwrap().records.len(),
        2
    );

    // 쓰레기 꼬리 + 재기동: 잘라 내고 이어 쓰며, 다음 세그먼트는 Gap(0)으로 시작한다.
    let mut junk = good.clone();
    junk.extend_from_slice(&[0xff; 37]);
    fs::write(&path, &junk).unwrap();
    let mut rec = store.recorder(id, SIZE).unwrap();
    assert_eq!(fs::metadata(&path).unwrap().len(), good.len() as u64);
    rec.append(&lines(50, 60));
    rec.seal();
    let segs = super::segments(&store.session_dir(id)).unwrap();
    assert_eq!(segs.len(), 2);
    let next = segment::read(&segs[1], None).unwrap().unwrap();
    assert_eq!(next.header.base, Base::Continue);
    assert_eq!(next.header.start_seq, full.end_seq());
    assert_eq!(next.records[0], Record::Gap(0));
    fs::remove_dir_all(&root).ok();
}

#[test]
fn crash_reconstruction_matches_live_screen() {
    let root = tmp("restore");
    let store = Store::open(&root, small(64 << 10)).unwrap();
    let id = SessionId(9);
    let mut rec = store.recorder(id, SIZE).unwrap();
    let mut live = crate::term::Engine::new(SIZE);
    let look = super::no_theme();
    let mut out = lines(0, 9_000);
    out.extend_from_slice(b"\x1b[1;31mred\x1b[0m prompt$ ");
    for part in out.chunks(4_096) {
        live.feed(part, crate::term::Feed::Live, &look);
        rec.append(part);
    }
    drop(rec); // 크래시
    let hist = store.history(id).unwrap();
    assert!(hist.len() > 3, "세그먼트 {}개", hist.len());
    let restored = hist.restore().unwrap();
    assert_eq!(restored.screen_text(), live.screen_text());
    assert_eq!(restored.end_seq, out.len() as u64);
    fs::remove_dir_all(&root).ok();
}

#[test]
fn cold_pages_match_original_beyond_10k_lines() {
    let root = tmp("pages");
    let store = Store::open(&root, small(64 << 10)).unwrap();
    let id = SessionId(5);
    let mut rec = store.recorder(id, SIZE).unwrap();
    let total = 12_000;
    let out = lines(0, total);
    for part in out.chunks(3_000) {
        rec.append(part);
    }
    rec.seal();
    let hist = store.history(id).unwrap();
    let mut got = Vec::new();
    let mut seq = 0;
    for i in 0..hist.len() {
        let page = hist.page(i).unwrap();
        assert_eq!(page.start_seq, seq);
        assert!(page.gaps.is_empty());
        for skip in [0, 7, 100, 1000, page.rows.len(), usize::MAX] {
            let (total_rows, range) = hist.rows(i, skip, 11).unwrap();
            let end = page.rows.len().saturating_sub(skip);
            let start = end.saturating_sub(11);
            assert_eq!(total_rows, page.rows.len());
            assert_eq!(range, page.rows[start..end]);
        }
        seq = page.end_seq;
        got.extend(page.lines());
    }
    let want: Vec<String> = (0..total).map(|i| format!("line-{i:06} 한글 ok")).collect();
    // 화면에 남은 행(마지막 23줄 + 빈 커서 행)만 빼고 전부 페이지에 있다.
    assert_eq!(got.len(), total - usize::from(SIZE.rows) + 1);
    assert_eq!(got[..], want[..got.len()]);
    fs::remove_dir_all(&root).ok();
}

#[test]
fn engine_panic_bytes_replay_without_panic() {
    // MK-006 발견 5(kitty 키보드 스택 overflow)로 라이브 엔진이 panic하는 바이트도 재생은 계속된다.
    let root = tmp("panic");
    let store = Store::open(&root, Limits::default()).unwrap();
    let id = SessionId(6);
    let mut rec = store.recorder(id, SIZE).unwrap();
    rec.append(&lines(0, 30));
    rec.append(&b"\x1b[>1u".repeat(4_097));
    rec.append(b"after\r\n");
    rec.seal();
    let hist = store.history(id).unwrap();
    assert!(hist.page(0).is_ok());
    assert!(hist.restore().unwrap().screen_text().contains("after"));
    fs::remove_dir_all(&root).ok();
}

/// 출력 폭주 10분(가상 시계): 비압축 원본을 분당 240MB로 넣어도 디스크 쓰기는 첫 분 버스트 뒤 분당 8MB 이하.
#[test]
fn flood_10_minutes_stays_within_budget() {
    let root = tmp("flood");
    let store = Store::open(&root, Limits::default()).unwrap();
    let mut rec = store.recorder(SessionId(8), SIZE).unwrap();
    let pool = noise(4 << 20, 99);
    let t0 = Instant::now();
    let mut per_min = Vec::new();
    let mut last = 0;
    let mut off = 0;
    // 64KiB 묶음을 16ms마다(분당 약 240MB).
    for minute in 0..10u32 {
        for tick in 0..3_750u32 {
            let now = t0
                + Duration::from_secs(u64::from(minute) * 60)
                + Duration::from_millis(u64::from(tick) * 16);
            let part = &pool[off..off + (64 << 10)];
            off = (off + (64 << 10) * 7) % (pool.len() - (64 << 10));
            rec.append_at(part, now);
        }
        let w = store.written();
        per_min.push(w - last);
        last = w;
    }
    eprintln!("분당 디스크 쓰기(B): {per_min:?}");
    let dir = store.session_dir(SessionId(8));
    let on_disk: u64 = super::segments(&dir)
        .unwrap()
        .iter()
        .map(|p| fs::metadata(p).unwrap().len())
        .sum();
    eprintln!("세션 디렉터리 {on_disk}B");
    assert!(per_min[0] <= 32_000_000 + 8_000_000);
    for (m, w) in per_min.iter().enumerate().skip(1) {
        assert!(*w <= 8_000_000 + super::RESUME, "{}분째 {w}B", m + 1);
    }
    // 버킷은 분당 8MB만 다시 찬다. 2~10분 합은 그 9분치 + 1분째 끝에 버킷에 남아 있던 토큰(< RESUME + 묶음 하나)을 넘지 못한다.
    let sustained: u64 = per_min[1..].iter().sum();
    assert!(
        sustained <= 9 * 8_000_000 + super::RESUME + (64 << 10),
        "2~10분 합 {sustained}B"
    );
    assert!(per_min.iter().sum::<u64>() <= 32_000_000 + 10 * 8_000_000);
    assert!(on_disk <= 32_000_000, "세션 상한: {on_disk}B");
    // 버린 구간은 Gap으로 남는다.
    let hist = store.history(SessionId(8)).unwrap();
    let gaps: usize = (0..hist.len())
        .map(|i| {
            super::segment::read(&super::segments(&dir).unwrap()[i], None)
                .unwrap()
                .unwrap()
        })
        .map(|s| {
            s.records
                .iter()
                .filter(|r| matches!(r, Record::Gap(_)))
                .count()
        })
        .sum();
    assert!(gaps > 0);
    fs::remove_dir_all(&root).ok();
}

#[test]
fn gc_retention_and_global_cap() {
    let root = tmp("gc");
    let limits = Limits {
        seal_bytes: 32 << 10,
        session_bytes: 200 << 10,
        total_bytes: 300 << 10,
        ..Limits::default()
    };
    let store = Store::open(&root, limits).unwrap();
    // 세션 상한: 봉인할 때 오래된 것부터 지운다.
    let mut a = store.recorder(SessionId(1), SIZE).unwrap();
    for i in 0..20 {
        a.append(&noise(40 << 10, i));
    }
    a.seal();
    let size = |id| -> u64 {
        super::segments(&store.session_dir(SessionId(id)))
            .unwrap()
            .iter()
            .map(|p| fs::metadata(p).unwrap().len())
            .sum()
    };
    assert!(size(1) <= 200 << 10, "{}", size(1));
    let mut b = store.recorder(SessionId(2), SIZE).unwrap();
    for i in 0..5 {
        b.append(&noise(40 << 10, 100 + i));
    }
    // 전역 상한: 살아 있는 세션 2의 마지막(열린) 세그먼트는 남기고 오래된 것부터.
    store
        .gc(&[SessionId(1), SessionId(2)], SystemTime::now())
        .unwrap();
    assert!(size(1) + size(2) <= 300 << 10, "{} {}", size(1), size(2));
    assert!(
        !super::segments(&store.session_dir(SessionId(2)))
            .unwrap()
            .is_empty()
    );
    b.append(b"still writing");
    // 닫힌 세션 보존: 7일 지나면 디렉터리째 지운다. 살아 있으면 안 지운다.
    let later = SystemTime::now() + Duration::from_hours(8 * 24);
    store.gc(&[SessionId(2)], later).unwrap();
    assert!(!store.session_dir(SessionId(1)).exists());
    assert!(store.session_dir(SessionId(2)).exists());
    let soon = SystemTime::now() + Duration::from_hours(24);
    store.gc(&[], soon).unwrap();
    assert!(store.session_dir(SessionId(2)).exists());
    fs::remove_dir_all(&root).ok();
}

#[test]
fn privacy_disabled_writes_zero_and_does_not_create_sessions() {
    let root = tmp("privacy-disabled");
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("history.toml"), "recording = false").unwrap();
    let store = Store::open(&root, Limits::default()).unwrap();
    let mut rec = store.recorder(SessionId(1), SIZE).unwrap();
    rec.append(&noise(1 << 20, 44));
    rec.resize(WinSize {
        cols: 100,
        rows: 30,
    });
    rec.seal();
    store.gc(&[], SystemTime::now()).unwrap();
    assert_eq!(rec.seq(), 1 << 20);
    assert_eq!(store.written(), 0);
    assert!(!root.join("sessions").exists());
    assert_eq!(store.history(SessionId(1)).unwrap().len(), 0);
    store.clear().unwrap();
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn privacy_invalid_config_fails_closed() {
    let root = tmp("privacy-config");
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("history.toml"), "recording = 'false'").unwrap();
    assert!(Store::open(&root, Limits::default()).is_err());
    assert!(!root.join("sessions").exists());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn cross_review_recorder_panic_recovery_marks_gap_and_continues() {
    let root = tmp("cross-recorder-panic");
    let store = Store::open(&root, Limits::default()).unwrap();
    let mut rec = store.recorder(SessionId(1), SIZE).unwrap();
    rec.append(b"before");
    let mut rec = rec.recover_after_panic(9, SIZE).unwrap();
    rec.append(b"after");
    rec.seal();
    assert_eq!(rec.seq(), 14);
    let mut pieces = Vec::new();
    store
        .raw(SessionId(1), 0, 14, |piece| {
            pieces.push(piece);
            Ok(())
        })
        .unwrap();
    assert!(
        pieces
            .iter()
            .any(|piece| matches!(piece, super::Piece::Gap { seq: 6, len: 3 }))
    );
    assert!(
        pieces
            .iter()
            .any(|piece| matches!(piece, super::Piece::Data { bytes, .. } if bytes == b"after"))
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn cross_review_malformed_settings_preserves_recording_and_fails_closed() {
    let root = tmp("cross-settings-closed");
    fs::create_dir_all(&root).unwrap();
    let settings = "recording_paused = [invalid";
    fs::write(root.join("settings.toml"), settings).unwrap();
    assert!(Store::open(&root, Limits::default()).is_err());
    assert_eq!(
        fs::read_to_string(root.join("settings.toml")).unwrap(),
        settings
    );
    assert_eq!(fs::read_dir(root.join("sessions")).unwrap().count(), 0);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn privacy_clear_invalidates_all_open_recorders_and_resumes_at_current_seq() {
    let root = tmp("privacy-clear");
    let store = Store::open(&root, Limits::default()).unwrap();
    let mut a = store.recorder(SessionId(1), SIZE).unwrap();
    let mut b = store.recorder(SessionId(2), SIZE).unwrap();
    a.append(b"secret-a");
    b.append(b"secret-b");
    let written = store.written();
    fs::write(root.join("unrelated-state"), b"keep").unwrap();
    store.clear().unwrap();
    assert!(!root.join("sessions").exists());
    // resize/seal must neither flush an unlinked fd nor recreate deleted paths.
    a.resize(SIZE);
    a.seal();
    b.seal();
    assert_eq!(store.written(), written);
    assert!(!root.join("sessions").exists());
    assert_eq!(fs::read(root.join("unrelated-state")).unwrap(), b"keep");
    assert_eq!(store.history(SessionId(1)).unwrap().len(), 0);
    a.append(b"new-a");
    b.append(b"new-b");
    a.seal();
    b.seal();
    for id in [SessionId(1), SessionId(2)] {
        let mut pieces = Vec::new();
        store
            .raw(id, 0, 13, |piece| {
                pieces.push(piece);
                Ok(())
            })
            .unwrap();
        assert_eq!(pieces[0], super::Piece::Gap { seq: 0, len: 8 });
        assert!(
            matches!(&pieces[1], super::Piece::Data { seq: 8, bytes } if bytes.starts_with(b"new-"))
        );
    }
    store.clear().unwrap();
    store.clear().unwrap();
    assert!(!root.join("sessions").exists());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn privacy_disabled_leaves_existing_recording_untouched() {
    let root = tmp("privacy-existing");
    let store = Store::open(&root, Limits::default()).unwrap();
    let mut rec = store.recorder(SessionId(1), SIZE).unwrap();
    rec.append(b"existing");
    drop(rec);
    drop(store);
    let path = super::segments(&root.join("sessions/1.noindex"))
        .unwrap()
        .remove(0);
    let before = fs::read(&path).unwrap();
    fs::write(root.join("history.toml"), "recording = false").unwrap();
    let store = Store::open(&root, Limits::default()).unwrap();
    let mut rec = store.recorder(SessionId(1), SIZE).unwrap();
    rec.append(b"private");
    rec.seal();
    assert_eq!(store.written(), 0);
    assert_eq!(fs::read(path).unwrap(), before);
    store.clear().unwrap();
    assert!(!root.join("sessions").exists());
    fs::remove_dir_all(root).unwrap();
}

#[test]
#[cfg_attr(not(target_os = "macos"), ignore = "macOS 전용: Time Machine(tmutil)")]
fn privacy_time_machine_excludes_recreated_recordings() {
    let root = tmp("privacy-backup");
    let store = Store::open(&root, Limits::default()).unwrap();
    let mut rec = store.recorder(SessionId(1), SIZE).unwrap();
    rec.append(b"before");
    for round in 0..2 {
        let result = std::process::Command::new("/usr/bin/tmutil")
            .arg("isexcluded")
            .arg(root.join("sessions"))
            .output()
            .unwrap();
        assert!(result.status.success());
        assert!(String::from_utf8_lossy(&result.stdout).contains("[Excluded]"));
        if round == 0 {
            store.clear().unwrap();
            rec.append(b"after");
        }
    }
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn seal_preserves_dropped_tail_and_restart_seq() {
    for prefix in [false, true] {
        let root = tmp(if prefix {
            "seal-drop-tail"
        } else {
            "seal-drop-all"
        });
        let store = Store::open(
            &root,
            Limits {
                burst: 1024,
                rate_per_min: 0,
                ..Limits::default()
            },
        )
        .unwrap();
        let mut rec = store.recorder(SessionId(1), SIZE).unwrap();
        if prefix {
            rec.append(b"kept");
        }
        rec.append(&vec![b'x'; 2048]);
        let end = rec.seq();
        rec.seal();
        rec.seal();
        let restored = store.history(SessionId(1)).unwrap().restore().unwrap();
        assert_eq!(restored.end_seq, end);
        let segs = super::segments(&store.session_dir(SessionId(1))).unwrap();
        assert!(segs.iter().any(|path| {
            segment::read(path, None)
                .unwrap()
                .unwrap()
                .records
                .contains(&Record::Gap(2048))
        }));
        let mut rec = store.recorder(SessionId(1), SIZE).unwrap();
        assert_eq!(rec.seq(), end);
        rec.append(b"next");
        rec.seal();
        assert_eq!(
            store
                .history(SessionId(1))
                .unwrap()
                .restore()
                .unwrap()
                .end_seq,
            end + 4
        );
        fs::remove_dir_all(root).unwrap();
    }
}

#[test]
fn open_recorders_write_while_privacy_gate_is_busy() {
    let root = tmp("parallel-open");
    let store = Store::open(&root, Limits::default()).unwrap();
    let mut recorders: Vec<_> = (1..=2)
        .map(|id| {
            let mut rec = store.recorder(SessionId(id), SIZE).unwrap();
            rec.append(b"initial");
            rec
        })
        .collect();
    let gate = super::lock(&store.gate);
    let (tx, rx) = std::sync::mpsc::channel();
    let threads: Vec<_> = recorders
        .drain(..)
        .map(|mut rec| {
            let tx = tx.clone();
            std::thread::spawn(move || {
                for _ in 0..32 {
                    rec.append(&[b'x'; 1024]);
                }
                tx.send(()).unwrap();
            })
        })
        .collect();
    let completed = (0..2).all(|_| rx.recv_timeout(Duration::from_secs(2)).is_ok());
    drop(gate);
    for thread in threads {
        thread.join().unwrap();
    }
    fs::remove_dir_all(root).unwrap();
    assert!(
        completed,
        "unrelated privacy work serialized open recorders"
    );
}

/// K-02: 크래시 후 출력 없이 봉인해도 이후 기록이 영구 정지하지 않는다.
/// 수정 전: 봉인 없는 마지막 세그먼트가 있으면 `seal()`이 seq 길이 0인
/// `Gap(0)` 세그먼트를 새로 만들고, 다음 기동의 첫 출력이 같은 이름으로
/// `create_new` → `AlreadyExists` → `abandon`을 영원히 반복한다.
#[test]
fn idle_seal_after_crash_does_not_halt_recording() {
    use super::Piece;
    let root = tmp("gap0-seal");
    let id = SessionId(9);
    // 크래시 전 기록. 봉인 없이 drop = kill -9.
    {
        let store = Store::open(&root, Limits::default()).unwrap();
        let mut rec = store.recorder(id, SIZE).unwrap();
        rec.append(b"before-crash");
    }
    // 재기동 후 출력 없이 봉인(업그레이드 halt·holder 인계).
    {
        let store = Store::open(&root, Limits::default()).unwrap();
        let mut rec = store.recorder(id, SIZE).unwrap();
        rec.seal();
    }
    // 다음 기동에서 출력이 다시 오면 전부 기록돼야 한다.
    {
        let store = Store::open(&root, Limits::default()).unwrap();
        let mut rec = store.recorder(id, SIZE).unwrap();
        rec.append(b"after-1");
        rec.append(b"after-2");
        rec.append(b"after-3");
        rec.seal();
    }
    let store = Store::open(&root, Limits::default()).unwrap();
    let mut datas = Vec::new();
    store
        .raw(id, 0, u64::MAX, |piece| {
            if let Piece::Data { bytes, .. } = piece {
                datas.extend_from_slice(&bytes);
            }
            Ok(())
        })
        .unwrap();
    assert_eq!(datas, b"before-crashafter-1after-2after-3");
    fs::remove_dir_all(root).unwrap();
}

/// 옛 `seal()`이 남긴 seq 길이 0의 봉인 세그먼트(`Gap(0)` 하나)를 손으로 재현한다.
/// `create`는 데이터가 없는 세그먼트만 치우고 다시 연다(K-02 복구 경로).
fn legacy_empty_segment(id: SessionId, start: u64) -> Vec<u8> {
    let header = Header {
        base: Base::Continue,
        encrypted: false,
        session: id,
        start_seq: start,
        size: SIZE,
    };
    let mut bytes = header.encode_with_keyframe(segment::KEYFRAME_NONE).to_vec();
    let mut rec = Vec::new();
    Record::Gap(0).encode_into(&mut rec);
    let mut enc = zstd::stream::write::Encoder::new(Vec::new(), super::LEVEL).unwrap();
    enc.window_log(super::WINDOW_LOG).unwrap();
    enc.write_all(&rec).unwrap();
    enc.flush().unwrap();
    let tail = enc.finish().unwrap();
    bytes.extend_from_slice(&segment::chunk(&tail));
    bytes
}

#[test]
fn create_reclaims_legacy_empty_segment() {
    use super::Piece;
    let root = tmp("gap0-legacy");
    let id = SessionId(11);
    {
        let store = Store::open(&root, Limits::default()).unwrap();
        let mut rec = store.recorder(id, SIZE).unwrap();
        rec.append(b"before-crash");
    }
    let dir = root.join("sessions").join("11.noindex");
    fs::write(
        dir.join("seg-00000000000000000012.krs"),
        legacy_empty_segment(id, 12),
    )
    .unwrap();
    {
        let store = Store::open(&root, Limits::default()).unwrap();
        let mut rec = store.recorder(id, SIZE).unwrap();
        assert_eq!(rec.seq(), 12);
        rec.append(b"after");
        rec.seal();
    }
    let store = Store::open(&root, Limits::default()).unwrap();
    let mut datas = Vec::new();
    store
        .raw(id, 0, u64::MAX, |piece| {
            if let Piece::Data { bytes, .. } = piece {
                datas.extend_from_slice(&bytes);
            }
            Ok(())
        })
        .unwrap();
    assert_eq!(datas, b"before-crashafter");
    fs::remove_dir_all(root).unwrap();
}

/// K-07: `sessions/` 안의 일반 파일(`.DS_Store` 등) 하나로 데몬 전체 기록이 꺼지지 않는다.
/// 수정 전: `has_encrypted`가 모든 항목에 `read_dir`을 걸어 ENOTDIR이 `Store::open`까지 전파됐다.
#[test]
fn stray_files_in_sessions_do_not_break_open() {
    use super::Piece;
    let root = tmp("stray-files");
    {
        let store = Store::open(&root, Limits::default()).unwrap();
        let mut rec = store.recorder(SessionId(1), SIZE).unwrap();
        rec.append(b"first");
        rec.seal();
    }
    fs::write(root.join("sessions").join(".DS_Store"), b"finder").unwrap();
    fs::write(root.join("sessions").join("notes.txt"), b"hi").unwrap();
    {
        let store = Store::open(&root, Limits::default()).unwrap();
        let mut rec = store.recorder(SessionId(1), SIZE).unwrap();
        rec.append(b"second");
        rec.seal();
    }
    let store = Store::open(&root, Limits::default()).unwrap();
    let mut datas = Vec::new();
    store
        .raw(SessionId(1), 0, u64::MAX, |piece| {
            if let Piece::Data { bytes, .. } = piece {
                datas.extend_from_slice(&bytes);
            }
            Ok(())
        })
        .unwrap();
    assert_eq!(datas, b"firstsecond");
    fs::remove_dir_all(root).unwrap();
}

/// K-20: 항목 하나의 IO 오류가 GC 전체를 중단시키지 않고, 세그먼트가 없는 죽은
/// 세션 디렉터리는 걷는다. 수정 전에는 `?` 하나로 이번 주기 전체가 중단됐고,
/// 빈 디렉터리는 `newest.is_some()` 조건에 영원히 남았다.
#[test]
fn gc_survives_unreadable_entry_and_clears_empty_dead_dirs() {
    use super::Piece;
    let root = tmp("gc-resilient");
    let store = Store::open(&root, Limits::default()).unwrap();
    let live = SessionId(21);
    let mut rec = store.recorder(live, SIZE).unwrap();
    rec.append(b"live-data");
    rec.seal();
    drop(rec);
    // 읽을 수 없는 세션 디렉터리 하나가 있어도 GC는 계속된다.
    let bad = store.session_dir(SessionId(22));
    fs::create_dir_all(&bad).unwrap();
    fs::write(bad.join("seg-00000000000000000000.krs"), b"junk").unwrap();
    fs::set_permissions(&bad, fs::Permissions::from_mode(0o000)).unwrap();
    // 세그먼트가 없는 죽은 세션 디렉터리는 걷는다.
    let empty = store.session_dir(SessionId(23));
    fs::create_dir_all(&empty).unwrap();
    assert!(store.gc(&[live], SystemTime::now()).is_ok());
    let mut datas = Vec::new();
    store
        .raw(live, 0, u64::MAX, |piece| {
            if let Piece::Data { bytes, .. } = piece {
                datas.extend_from_slice(&bytes);
            }
            Ok(())
        })
        .unwrap();
    assert_eq!(datas, b"live-data");
    assert!(!empty.exists(), "empty dead session dir is removed");
    assert!(store.session_dir(live).exists());
    fs::set_permissions(&bad, fs::Permissions::from_mode(0o700)).unwrap();
    fs::remove_dir_all(root).unwrap();
}

/// K-21: 삭제 중 크래시로 남은 `.history-deleted-*`는 다음 기동 때 치운다.
/// 수정 전에는 다시 정리하는 코드가 없어 평문 이력이 디스크에 영원히 남았다.
#[test]
fn open_sweeps_crashed_history_deletion_leftovers() {
    let root = tmp("sweep-deleted");
    fs::create_dir_all(&root).unwrap();
    let leftover = root.join(".history-deleted-999-0");
    fs::create_dir_all(&leftover).unwrap();
    fs::write(leftover.join("seg-00000000000000000000.krs"), b"plain").unwrap();
    fs::write(root.join(".history-deleted-999-1"), b"plain").unwrap();
    fs::write(root.join("keep.txt"), b"keep").unwrap();
    Store::open(&root, Limits::default()).unwrap();
    assert!(!leftover.exists(), "crashed deletion dir is swept");
    assert!(
        !root.join(".history-deleted-999-1").exists(),
        "crashed deletion file is swept"
    );
    assert!(root.join("keep.txt").exists(), "unrelated files stay");
    fs::remove_dir_all(root).unwrap();
}

/// OWE-873b: 스크롤백 지우기 기준선 이전 세그먼트·행은 히스토리/복원 스크롤백에서 제외되고,
/// 지운 뒤 들어온 새 출력만 스크롤백으로 돌아오며 마지막 보이는 화면은 그대로 유지된다.
#[test]
fn clear_scrollback_excludes_prior_rows_and_preserves_new_output_and_screen() {
    let root = tmp("clear-scrollback");
    let id = SessionId(77);
    let small = WinSize { cols: 20, rows: 3 };
    let store = Store::open(&root, Limits::default()).unwrap();
    let mut rec = store.recorder(id, small).unwrap();
    rec.append(b"l1\r\nl2\r\nl3\r\nl4\r\n");
    rec.seal();
    rec.append(b"l5\r\nl6\r\nl7\r\nl8");
    let cleared_at = rec.seq();

    // 지우기 전: 콜드 페이지 2개에 스크롤백 행이 있다.
    assert_eq!(store.history(id).unwrap().len(), 2);

    // 지운 직후: 콜드 페이지 수는 0이고, 복원 화면(l6..l8)은 유지되나 복원 스크롤백은 비어 있다.
    store.clear_scrollback(id, cleared_at);
    let h = store.history(id).unwrap();
    assert_eq!(h.len(), 0);
    assert!(h.is_empty());
    let restored = h.restore().unwrap();
    assert_eq!(
        restored
            .screen_text()
            .lines()
            .map(str::trim_end)
            .collect::<Vec<_>>(),
        vec!["l6", "l7", "l8"]
    );
    assert!(restored.history().is_empty());

    // 지운 뒤 새 출력: 그때부터 밀려난 행(l6..l9)만 히스토리에 잡히고 l1..l5는 돌아오지 않는다.
    rec.append(b"\r\nl9\r\nl10\r\nl11\r\nl12");
    rec.seal();
    drop(rec);

    for reopened in [store, Store::open(&root, Limits::default()).unwrap()] {
        let h = reopened.history(id).unwrap();
        assert_eq!(h.len(), 1);
        let page = h.page(0).unwrap();
        assert_eq!(page.lines(), vec!["l6", "l7", "l8", "l9"]);
        let (total, rows) = h.rows(0, 0, 10).unwrap();
        assert_eq!(total, 4);
        assert_eq!(rows.len(), 4);
        let restored = h.restore().unwrap();
        assert_eq!(
            restored
                .screen_text()
                .lines()
                .map(str::trim_end)
                .collect::<Vec<_>>(),
            vec!["l10", "l11", "l12"]
        );
        assert_eq!(restored.history().len(), 4);
    }
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn resumed_history_respects_clear_and_keeps_live_ancestors_during_gc() {
    let root = tmp("resume-archive-gc");
    let store = Store::open(
        &root,
        Limits {
            keep_closed: Duration::ZERO,
            ..Limits::default()
        },
    )
    .unwrap();
    let mut old = store.recorder(SessionId(1), SIZE).unwrap();
    old.append(b"old-screen\r\n");
    old.seal();
    drop(old);
    let written = store.written();
    store.set_paused(true);
    store.inherit_history(SessionId(2), SessionId(1)).unwrap();
    assert_eq!(
        store.written(),
        written,
        "계보 연결은 새 터미널 원본을 기록하지 않는다"
    );
    store.set_paused(false);
    let mut new = store.recorder(SessionId(2), SIZE).unwrap();
    new.append(b"new-screen\r\n");
    new.seal();
    drop(new);
    store
        .gc(&[SessionId(2)], SystemTime::now() + Duration::from_secs(1))
        .unwrap();
    assert!(store.session_dir(SessionId(1)).exists());
    assert_eq!(store.archive(SessionId(2)).unwrap().len(), 3);
    assert!(store.clear_cold_scrollback(SessionId(2)));
    assert_eq!(store.history_chain(SessionId(2)), [SessionId(2)]);
    store
        .gc(&[SessionId(2)], SystemTime::now() + Duration::from_secs(1))
        .unwrap();
    assert!(!store.session_dir(SessionId(1)).exists());
    // 손상된 순환 링크는 유한한 경로에서 멈춘다.
    fs::remove_file(store.session_dir(SessionId(2)).join(super::CLEARED_FILE)).unwrap();
    store.inherit_history(SessionId(1), SessionId(2)).unwrap();
    assert_eq!(
        store.history_chain(SessionId(2)),
        [SessionId(2), SessionId(1)]
    );
    fs::remove_dir_all(root).ok();
}

fn visited_pages(history: &super::History, limit: u64) -> Vec<(usize, super::Page)> {
    let mut pages = Vec::new();
    let mut rows = Vec::new();
    history.visit_until(limit, &|| false, |page, event| match event {
        super::HistoryEvent::Rows(batch) => rows.extend(batch),
        super::HistoryEvent::End(result) => {
            let mut data = result.unwrap();
            data.rows = std::mem::take(&mut rows);
            pages.push((page, data));
        }
    });
    pages
}

#[test]
fn forward_history_visitor_preserves_raw_parser_state_and_clear_boundaries() {
    let root = tmp("forward-parser");
    let id = SessionId(91);
    let size = WinSize { cols: 12, rows: 3 };
    let store = Store::open(&root, Limits::default()).unwrap();
    let mut rec = store.recorder(id, size).unwrap();
    // Deliberately split UTF-8, CSI, alternate screen and synchronized output across
    // segment boundaries. An ANSI screen snapshot cannot replace this parser state.
    for part in [
        &b"one\r\ntwo\r\nthree\r\n\xe9"[..],
        &b"\x9b\xaa\r\n\x1b[?20"[..],
        &b"26hhidden\r\nnext\r\n\x1b[?2026l\x1b[?1049hALT\r\n"[..],
        &b"alternate\r\nmore\r\n\x1b[?1049l\r\nend\r\n"[..],
    ] {
        rec.append(part);
        rec.seal();
    }
    let limit = rec.seq().saturating_sub(3);
    let history = store.history(id).unwrap();
    for (page, visited) in visited_pages(&history, limit) {
        assert_eq!(visited, history.page_until(page, limit).unwrap().unwrap());
    }
    store.clear_scrollback(id, rec.seq());
    rec.append(b"\r\nafter-clear\r\nnew\r\nlast\r\n");
    rec.seal();
    let history = store.history(id).unwrap();
    let pages = visited_pages(&history, rec.seq());
    assert_eq!(pages.len(), history.len());
    for (page, visited) in pages {
        assert_eq!(
            visited,
            history.page_until(page, rec.seq()).unwrap().unwrap()
        );
        assert!(!visited.lines().iter().any(|line| line == "one"));
    }
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn forward_history_visitor_stops_inside_a_replay_batch() {
    let root = tmp("forward-cancel");
    let id = SessionId(92);
    let store = Store::open(&root, small(1024)).unwrap();
    let mut rec = store.recorder(id, SIZE).unwrap();
    for _ in 0..8 {
        rec.append(&lines(0, 1000));
        rec.seal();
    }
    let history = store.history(id).unwrap();
    let checks = std::cell::Cell::new(0usize);
    let mut completed = 0;
    history.visit_until(
        rec.seq(),
        &|| {
            checks.set(checks.get() + 1);
            checks.get() >= 4
        },
        |_, event| {
            if matches!(event, super::HistoryEvent::End(_)) {
                completed += 1;
            }
        },
    );
    assert_eq!(
        completed, 0,
        "cancelled pages must not publish a partial total"
    );
    assert!(
        checks.get() < 10,
        "cancellation must not replay the remaining segments"
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn forward_history_visitor_keeps_gc_gap_metadata() {
    let root = tmp("forward-gap");
    let id = SessionId(93);
    let store = Store::open(&root, small(1024)).unwrap();
    let mut rec = store.recorder(id, SIZE).unwrap();
    for index in 0..4 {
        rec.append(&lines(index * 100, (index + 1) * 100));
        rec.seal();
    }
    let paths = super::segments(&store.session_dir(id)).unwrap();
    fs::remove_file(&paths[1]).unwrap();
    let history = store.history(id).unwrap();
    let pages = visited_pages(&history, rec.seq());
    assert_eq!(pages.len(), 3);
    assert!(pages[0].1.end_seq < pages[1].1.start_seq);
    for (page, visited) in pages {
        assert_eq!(
            visited,
            history.page_until(page, rec.seq()).unwrap().unwrap()
        );
    }
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn forward_history_resize_fallback_matches_authoritative_reflow_on_every_page() {
    let root = tmp("forward-resize");
    let id = SessionId(94);
    let store = Store::open(&root, Limits::default()).unwrap();
    let mut rec = store.recorder(id, WinSize { cols: 12, rows: 3 }).unwrap();
    rec.append(b"abcdefghijklmnopqrstuvwx\r\nold1\r\nold2\r\nold3\r\n");
    rec.seal();
    // Header-only growth can pull old history into the viewport.
    rec.resize(WinSize { cols: 24, rows: 6 });
    rec.append(b"grown\r\n");
    // In-segment shrink/grow also reflows history in the warm prefix.
    rec.resize(WinSize { cols: 8, rows: 2 });
    rec.append(b"more-content-than-a-row\r\n");
    rec.resize(WinSize { cols: 20, rows: 5 });
    rec.append(b"tail1\r\ntail2\r\n");
    rec.seal();
    for _ in 0..3 {
        rec.append(b"next1\r\nnext2\r\nnext3\r\n");
        rec.seal();
    }
    let history = store.history(id).unwrap();
    for (page, visited) in visited_pages(&history, rec.seq()) {
        assert_eq!(
            visited,
            history.page_until(page, rec.seq()).unwrap().unwrap(),
            "page {page}"
        );
    }
    fs::remove_dir_all(root).unwrap();
}

std::thread_local! {
    static VISITOR_RAW_READS: std::cell::Cell<Option<usize>> = const { std::cell::Cell::new(None) };
}

pub(super) fn note_raw_read() {
    VISITOR_RAW_READS.with(|reads| {
        if let Some(count) = reads.get() {
            reads.set(Some(count + 1));
        }
    });
}

pub(super) fn counted_raw_reads<T>(run: impl FnOnce() -> T) -> (T, usize) {
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            VISITOR_RAW_READS.with(|reads| reads.set(None));
        }
    }
    VISITOR_RAW_READS.with(|reads| reads.set(Some(0)));
    let reset = Reset;
    let result = run();
    let count = VISITOR_RAW_READS.with(|reads| reads.get().unwrap());
    drop(reset);
    (result, count)
}

fn assert_visitor_matches_warm(history: &super::History, limit: u64) {
    let visited = visited_pages(history, limit);
    let expected: Vec<_> = (0..history.len())
        .filter_map(|page| {
            history
                .page_until(page, limit)
                .map(|result| (page, result.unwrap()))
        })
        .collect();
    assert_eq!(visited, expected, "limit seq {limit}");
}

/// Resize before/inside/between segments while parser, UTF-8, primary/alternate
/// scrollback and synchronized output all straddle segment boundaries.
fn record_reflow_fixture(rec: &mut super::Recorder) -> Vec<u64> {
    let mut boundaries = Vec::new();
    for turn in 0..4 {
        rec.resize(WinSize { cols: 12, rows: 3 });
        rec.append(&lines(turn * 100, turn * 100 + 45));
        rec.append(b"\x1b]2;reflow-title\x07\x1b[?2026hqueued\r\n\xe9");
        rec.seal();
        boundaries.push(rec.seq());
        rec.resize(WinSize { cols: 24, rows: 7 });
        rec.append(b"\x9b\xaa\r\nsync-tail\r\n\x1b[?2026l\x1b[?1049hALT\r\na\r\nb\r\n\x1b[38;2;");
        rec.seal();
        boundaries.push(rec.seq());
        rec.resize(WinSize { cols: 8, rows: 2 });
        rec.append(b"12;34;56mcolored-wide-\xe7\x95\x8c\r\nmore-alt\r\n");
        rec.resize(WinSize { cols: 20, rows: 6 });
        rec.append(b"grow-alt\r\n\x1b[?1049lprimary-back\r\n\x1b[");
        rec.seal();
        boundaries.push(rec.seq());
        rec.resize(WinSize { cols: 10, rows: 4 });
        rec.append(b"2K\rtail\r\n\x1b7\x1b[2;3Hxy\x1b8\r\n\x1b[?2031h\x1b[?2031$p");
        rec.append(&lines(turn * 100 + 45, turn * 100 + 100));
        rec.seal();
        boundaries.push(rec.seq());
    }
    boundaries
}

#[test]
fn forward_history_resize_reads_each_segment_once_and_matches_every_warm_page() {
    let root = tmp("forward-resize-single-read");
    let id = SessionId(95);
    let store = Store::open(&root, Limits::default()).unwrap();
    let mut rec = store.recorder(id, WinSize { cols: 12, rows: 3 }).unwrap();
    let boundaries = record_reflow_fixture(&mut rec);
    let history = store.history(id).unwrap();
    let (visited, reads) = counted_raw_reads(|| visited_pages(&history, rec.seq()));
    assert_eq!(
        reads,
        history.len(),
        "resize must not reread any prefix segment"
    );
    assert_eq!(visited.len(), history.len());
    for (page, data) in visited {
        assert_eq!(
            data,
            history.page_until(page, rec.seq()).unwrap().unwrap(),
            "page {page}"
        );
    }
    // Cut inside UTF-8, incomplete CSI and pending synchronized output, at each
    // boundary and within later records; totals/coordinates are based on the same cutoff.
    for limit in boundaries
        .iter()
        .flat_map(|end| [end.saturating_sub(1), *end])
    {
        assert_visitor_matches_warm(&history, limit);
    }
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn forward_history_reflow_matches_warm_after_clear_gc_and_privacy_gap() {
    let root = tmp("forward-resize-clear-gap");
    let id = SessionId(96);
    let store = Store::open(&root, Limits::default()).unwrap();
    let mut rec = store.recorder(id, WinSize { cols: 12, rows: 3 }).unwrap();
    let boundaries = record_reflow_fixture(&mut rec);
    // A privacy pause makes an explicit Gap without replaying the secret data.
    store.set_paused(true);
    rec.append(b"PRIVATE-NEVER-RECORDED\r\n");
    store.set_paused(false);
    rec.append(b"after-privacy-gap\r\nnew-line\r\nlast-line\r\n");
    rec.resize(WinSize { cols: 30, rows: 8 });
    rec.append(b"after-growth\r\nmore\r\n");
    rec.seal();
    let history = store.history(id).unwrap();
    assert_visitor_matches_warm(&history, rec.seq());
    let pages = visited_pages(&history, rec.seq());
    assert!(pages.iter().any(|(_, page)| !page.gaps.is_empty()));
    assert!(
        !pages
            .iter()
            .flat_map(|(_, page)| page.lines())
            .any(|line| line.contains("PRIVATE"))
    );
    // Clear inside a segment, then remove another segment to simulate a GC hole.
    store.clear_scrollback(id, boundaries[4].saturating_sub(3));
    let paths = super::segments(&store.session_dir(id)).unwrap();
    fs::remove_file(&paths[9]).unwrap();
    let history = store.history(id).unwrap();
    let (_, reads) = counted_raw_reads(|| visited_pages(&history, rec.seq()));
    assert_eq!(
        reads,
        history.segs.len(),
        "clear prefix and GC runs must each be read once"
    );
    assert_visitor_matches_warm(&history, rec.seq());
    for limit in [
        boundaries[6],
        boundaries[10].saturating_sub(2),
        rec.seq().saturating_sub(1),
    ] {
        assert_visitor_matches_warm(&history, limit);
    }
    // A deleted incarnation cannot be retained by a process-global replay cache.
    store.clear().unwrap();
    rec.append(b"new-incarnation\r\nonly-new-data\r\nlast\r\n");
    rec.seal();
    let fresh = store.history(id).unwrap();
    assert_visitor_matches_warm(&fresh, rec.seq());
    assert!(
        !visited_pages(&fresh, rec.seq())
            .iter()
            .flat_map(|(_, page)| page.lines())
            .any(|line| line.contains("reflow-title") || line.contains("PRIVATE"))
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn forward_history_reflow_recovers_after_a_manifest_segment_becomes_corrupt() {
    let root = tmp("forward-reflow-corrupt");
    let id = SessionId(97);
    let store = Store::open(&root, Limits::default()).unwrap();
    let mut rec = store.recorder(id, WinSize { cols: 12, rows: 3 }).unwrap();
    record_reflow_fixture(&mut rec);
    let history = store.history(id).unwrap();
    fs::write(&history.segs[8], b"invalid segment").unwrap();
    let mut rows = Vec::new();
    let mut actual = Vec::new();
    history.visit_until(rec.seq(), &|| false, |page, event| match event {
        super::HistoryEvent::Rows(batch) => rows.extend(batch),
        super::HistoryEvent::End(result) => {
            let result = result
                .map(|mut data| {
                    data.rows = std::mem::take(&mut rows);
                    data
                })
                .map_err(|error| error.kind());
            actual.push((page, result));
        }
    });
    let expected: Vec<_> = (0..history.len())
        .filter_map(|page| {
            history
                .page_until(page, rec.seq())
                .map(|result| (page, result.map_err(|error| error.kind())))
        })
        .collect();
    assert_eq!(actual, expected);
    assert!(actual.iter().any(|(_, result)| result.is_err()));
    fs::remove_dir_all(root).unwrap();
}
