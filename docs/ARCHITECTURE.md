# Krill 아키텍처 v1 (2026-10-14)

> 2026-10-22 지원 정책: 출고·검증 대상은 Apple Silicon Mac(arm64)과 Windows x64다. Intel Mac·Linux의 제품 빌드 CI·패키징·설치 자산은 제거했다. 아래의 Linux 구현·측정 기록은 기존 호환 코드의 설명이며 현재 지원 플랫폼을 뜻하지 않는다.


기능·세션 데몬·터미널·에이전트 운영의 내부 설계 검토를 통합한 규범 문서다. `T2-D3` 같은 표기는 내부 결정 번호다. 수치에 "추정"이 없으면 기록된 Mac mini 환경의 실측이다.

## 1. 한 장 요약

```
 ┌──────────── launchd gui/<uid> (비지속 잡, AbandonProcessGroup) ─────────────┐
 │  krill-holder ── 에이전트·셸의 부모, PTY master fd 보관만. 작고 거의 안 바뀜 │
 │     │ (fork/exec, SCM_RIGHTS로 fd 공유, 내부 프로토콜 동결)                   │
 │     └── krilld ── 세션별 alacritty_terminal Term(핫 1,000행) · 질의 응답자     │
 │                   · zstd 세그먼트 기록 · hook 수신 · 상태 저장소 · git 작업 큐 │
 └───────────────────────────────┬────────────────────────────────────────────┘
                                 │ UDS 하나(0700 dir / 0600 sock, getpeereid)
          ┌──────────────────────┼───────────────────────┐
       Krill.app (gpui)       krill CLI              krill hook (에이전트가 호출)
      보이는 화면만 그림        데몬에 직접 붙음         stdin JSON → 소켓, {} 선출력
```

- **앱 재시작·크래시·업데이트** → 세션 영향 없음(데몬이 PTY 소유).
- **krilld 크래시·업그레이드** → holder가 fd를 들고 있어 세션 유지, holder가 krilld를 다시 띄우고 fd 재전달(T2-D1, S2).
- **holder와 krilld가 동시에 죽음** 또는 **재부팅** → 프로세스 소실. 디스크 세그먼트로 마지막 화면을 읽기 전용 복원하고, `claude --resume`·`codex resume`으로 대화 재개(T2 #24, T4-D6).
- 장애별 정확한 보장 범위는 §4.1 표가 규범이다. "세션 생존"(프로세스가 산다)과 "출력 무손실"(바이트가 빠짐없이 재생된다)은 다른 보장이다.

## 2. 프로세스와 크레이트

| 크레이트 | 역할 | 메모리 목표 |
|---|---|---|
| `krill-proto` | 와이어 포맷(프레임·메시지·키프레임 포맷). 엔진 무관 | — |
| `krill-holder` | spawn·waitpid·fd 보관·SCM_RIGHTS·exec 핸드오프. macOS 빌드 1,300줄 이하 + Linux 전용 `sys/linux.rs` 300줄 이하 + Windows 빌드 2,000줄 이하(2026-10-19 개정: macOS 1,250→1,300. 결정 12-2, 2026-10-18 개정: Linux 원격 호스트용 epoll·pidfd 구현 추가. 개정 시점 실측 1,218줄·292줄) | 수 MB(추정) |
| `krilld` | 에뮬레이터·IPC·디스크·상태·hook·git 큐·전원 | ≤ 60MB @10세션 |
| `krill-term` | 프레임워크 중립 터미널 그리드 렌더 모델 + IME 상태기계 | — |
| `krill-app` | gpui UI(사이드바·탭·분할·터미널 위젯·diff) | ≤ 120MB @창 1600×1000pt @2x |
| `krill` | CLI 단일 바이너리(`krill hook` 포함) | 호출당 RSS < 10MB |

- **제품명·CLI·앱 번들 이름·cask 이름은 `Krill` / `krill`로 통일**(2026-10-16 결정). Homebrew 코어·cask에 `krill` 충돌 없음.
- **번들 ID는 `dev.krill.app`**(2026-10-17 결정, MK-170): 상태 디렉터리 `~/Library/Application Support/dev.krill.app`, launchd 라벨 `dev.krill.app.holder`·`dev.krill.app.link`, Caches·Preferences·Saved Application State도 같은 id, 로그는 `~/Library/Logs/Krill`. 개명 전 이름의 데이터·식별자는 이관하지 않는다(2026-10-22 사용자 지시: 기존 사용자가 없어 새 배포로 시작).
- **디스크 식별자**: `~/.config/krill`·`~/.local/share/krill`, 워크트리 `~/Krill/worktrees`, 레포 설정 `krill.toml`. 훅·스킬 이름은 `krill`, 백업은 `.krill-bak-*`, 준비 디렉터리는 `.krill-preparing`, 워크트리 잠금은 `krill-worktree.lock`.
- **키·와이어 식별자**: 키체인 서비스 `com.krill.segment-key.v2`, 세그먼트 v2 KDF 문자열 `krill segment v2`, 업데이트 서명 도메인 `krill-update-v1`, iroh ALPN `krill/link/2`·`krill/pair/1`, 초대 링크 `krill://pair?`. 로컬 코드 서명 인증서는 `Krill Local Code Signing`, 키체인 파일은 `~/Library/Keychains/krill-codesign.keychain-db`(Ed25519 키는 같은 이름 + `.ed25519.key`). 훅에 박힌 `MK_PANE_ID`·`MK_HOOK_EVENT`는 유지한다(규칙 1).

## 3. 트랙 간 충돌 정리 (통합 결정)

| 주제 | 트랙 의견 | 통합 결정 | 이유 |
|---|---|---|---|
| CLI 이름 | T1 `mk` / T4 `krill` | **`krill`** | 프로젝트 결정(2026-10-16, 구 `mk`에서 `krill`로 통일) |
| 마일스톤 번호 | T1·T2: M1 스파이크·M2 코어… / T3·T4: M1 코어… | **T1 정의로 통일**: M1 스파이크, M2 코어, M3 병렬, M4 출시 준비, M5 v1.x, M6 v2 | 다수 트랙 정의 |
| 로컬 소켓 인증 | T1 토큰 / T4 pane 범위 토큰 / T2 토큰 없음 + 피어 PID 계보 | **토큰 없음.** `getpeereid` UID 검사 + `LOCAL_PEERPID`(+ 프로세스 시작 시각/audit token)로 세션 후손이면 그 세션 범위로 제한(T2-D11). **이것은 실수 방지용 범위 제한이지 같은 UID 악성 코드에 대한 보안 경계가 아니다.** 계보 조회 실패는 제한 권한으로 처리 | 같은 UID는 토큰 파일을 읽을 수 있어 토큰이 막지 못한다. hook은 세션 안에서 돌므로 PID 계보로 pane이 식별된다 |
| 압축 | T2 zstd-3 디스크 / T3 lz4_flex 메모리 링 | **zstd 하나**(메모리 링·디스크 동일 코덱, 레벨은 S-mem에서 확정) | 실측 압축률 zstd-3 15.5× vs lz4 약 3×. 코덱 하나가 코드·테스트 절반 |
| UI가 받는 데이터 | T2 키프레임+원본 바이트(UI가 Term 재구성) / T3 화면 스냅샷+dirty row 델타(UI는 Term 없음) | **기본은 T3 델타 방식**, M1 스파이크 S-delta에서 확정 | UI에 Term 미러를 두면 세션당 3.6MB 이상 중복. 질의 응답자가 데몬 하나뿐이라는 원칙(T2-D4)과도 맞다 |
| 데몬 업그레이드 | T4 세대 공존 + 세션 0 구 데몬 은퇴 / T2 holder 유지, krilld만 교체 | **T2 방식.** krilld는 holder가 새 바이너리로 재기동, holder는 exec 핸드오프. 세대 공존 없음. 앱·CLI는 Hello에서 N-1 호환 | 세대가 하나뿐이면 "고아 데몬 0"이 구조적으로 보장된다 |
| 메모리 목표 | T1 ≤150MB / T3 ≤180MB(상한 200) / T4 게이트 ≤200 | **목표 180MB(앱 120 + 데몬 60), 출시 차단선 200MB**, 터미널 10개, phys_footprint, 에이전트 프로세스 제외, 창 1600×1000pt @2x(MK-009). 창 drawable은 창 픽셀 수에 비례하므로 더 큰 창은 이 예산으로 판정하지 않는다(2026-10-22 실측: 2560×1319pt @2x에서 drawable 1장 52.6MB, 3장 약 158MB) | T3가 창 drawable(IOSurface)을 실측해 150MB가 비현실적임을 보였다 |
| diff 뷰 범위 | T1 v1.x / T3 M2 | **읽기 전용 diff + 변경 파일 목록은 M3(P1)**, 커밋 UI·줄 주석은 M5 | 에이전트 결과를 검토하는 핵심 동선이지만 편집 기능은 범위 밖 |
| 텔레메트리 | T1 v1 없음 / T4 opt-in | **v1 없음**, M5에서 opt-in(기본 꺼짐) | 두 의견이 양립 |

## 4. 세션 데몬 (T2)

- **기동**: 서명된 헬퍼 번들을 CDHash 이름의 안정 경로에 복사 → `launchctl bootstrap gui/<uid>` 비지속 잡(`RunAtLoad`, `KeepAlive=false`, `AbandonProcessGroup=true`, `ProcessType=Interactive`, **`AssociatedBundleIdentifiers=[앱 id]`**, `SoftResourceLimits.NumberOfFiles`(launchd 잡 기본 소프트 한도 256, C2 실측)). 헬퍼는 앱과 **같은 팀·각자 고유 identifier**(`<앱 id>.holder` 등)로 서명한다. "같은 identifier로 서명하면 TCC가 상속된다"는 가정은 **Apple 공식 근거가 없어 폐기**(C1-final §4-1). TCC 귀속과 macOS 26+ "앱 종료 후 남는 백그라운드 작업" 허용 프롬프트는 MK-007에서 실측하고, 실패 시 SMAppService 경로로 간다. ad hoc 개발 빌드는 `posix_spawn`+`setsid`(MK-025). 서명 경로 전환은 MK-106.
- **PTY**: `rustix-openpty`(alacritty_terminal이 사용) 또는 `nix::pty::openpty` — rustix 본체에는 openpty 없음(T2-C1), 선택은 MK-004. 시작 시 `RLIMIT_NOFILE` 소프트 한도를 올린다(macOS 기본 256). 프로세스 그룹 종료 단계와 setsid로 빠져나간 후손 스윕, 세션 상한 256.
- **상태 엔진**: krilld가 세션마다 `alacritty_terminal` 0.26 `Term`(버전 핀). 붙은 세션은 화면 + 핫 1,000행, 30분 미접속이면 화면만(200×50 실측: 1만 행 53.7MB, 1,000행 6.0MB, 화면만 0.68MB). **키프레임은 셀만이 아니라 다음 바이트 처리에 영향을 주는 상태 전부**(대체 화면, 스크롤 영역, 모드·키보드 스택, vte 파서의 미완성 UTF-8·CSI/OSC·DEC 2026 버퍼)를 담아야 한다. alacritty 공개 API로 못 꺼내는 private 상태는 좁은 포크 API 또는 "마지막 키프레임 + 원본 바이트 재생"으로 해결(MK-006에서 결정). 세션 panic은 `catch_unwind`로 격리(panic 전략은 CONVENTIONS P4), 같은 바이트로 재차 panic하면 그 구간을 Gap으로 건너뛴다.
- **질의 응답**: krilld만 응답(DA/DSR/OSC 10·11 등). 재생 중 응답 금지. 색 질의는 UI가 마지막으로 보낸 테마(디스크에 저장, 재시작 후에도 유지)로, 한 번도 받은 적 없으면 krilld 내장 기본 테마로 즉시 응답(앱이 닫혀도 응답 — MK-020). alacritty가 직접 응답하지 않는 질의(색·픽셀 크기)는 krilld가 만든다(T2-C3).
- **IPC**: UDS 1개, `[u32 LE len][u8 kind][body]`, 최대 1MiB, 제어=postcard(`default-features = false, features = ["use-std"]` — 기본 feature는 RUSTSEC-2023-0089로 cargo-deny 실패, C2 실측)·데이터=raw, Hello에서 버전 범위·feature 비트 협상. 소켓 이름에 버전 없음. postcard는 필드 순서 기반이라 `#[non_exhaustive]`만으로 N-1 호환이 안 된다 → 버전별 고정 DTO·명시 opcode·feature gate와 **고정 바이트 fixture 양방향 테스트**. 압축 해제 크기·셀 수·문자열·벡터 길이에도 상한. 소켓 경로 104바이트 이하 검사.
- **재접속**: seq 이어받기 → 키프레임 + 재생 → 지연 히스토리. 느린 클라이언트는 큐 상한 초과 시 `Resync`, PTY 읽기는 절대 멈추지 않음.
- **디스크**: append-only 세그먼트(키프레임 + zstd 레코드, 레코드마다 길이·체크섬 → 잘린 꼬리·손상 구간 버리고 복구), 토큰 버킷 지속 8MB/분·버스트 32MB, fsync 없음, 세션당 32MB·전역 1GB, 닫힌 세션 7일 보존, 위치 `~/Library/Application Support/<bundle id>/`(`.noindex`·0600), 캐시는 `~/Library/Caches/<bundle id>/`. ENOSPC·부분 쓰기 시 Gap 표시 후 계속. (Orca main은 4일간 16.9GB 기록 — 이를 구조적으로 막는다.)
- **클라이언트별 데이터 경로**: `krill-app`은 화면 스냅샷 + 행 델타(스크롤은 행 이동 연산), `krill attach`(일반 터미널)는 호스트 터미널용 ANSI 화면 복원 시퀀스 + 원본 바이트 스트림. 둘 다 같은 원본 seq를 공유하고 화면 revision은 별도.
- **다중 클라이언트**: `interactive`/`observer`, 크기는 가장 최근 입력·포커스한 크기 소유 interactive가 결정. 모바일은 `owns_size: false`로 입력·승인만 수행하며 명시적 `roles::Request::Resize` 뒤에만 크기를 소유한다. N-1의 `Interactive`·`Observer` 바이트를 보존하고 `InteractiveWithSize { owns_size }`를 끝에 추가한다.
- **M9 모바일**: `krilld::mobile`이 앱과 독립적으로 HTTP/WS 게이트웨이를 운영한다. 활성 기기·5분 일회용 티켓이 모두 없으면 TCP 리스너를 열지 않는다. 기본 Relay(Tunnel wire 모드)는 loopback만, Tailscale은 선택한 tailnet 인터페이스와 loopback만 바인딩한다. LAN 신규 접속·사용자 설정은 제거했으며 판별값 0은 wire tombstone으로 보존한다. 기존 Lan 저장 설정은 기기 credential·PTY를 지우지 않고 Relay로 이행한다(명시적 loopback 격리 QA 예외). ring HKDF-SHA256 방향별 키·ChaCha20-Poly1305(4바이트 방향 + 8바이트 단조 seq nonce)로 암호화·재전송 거부 후 `UnixStream::pair()`를 통해 기존 Registry 경로를 재사용한다. `mobile-devices.json`은 0600 원자 저장하며(페어링·회수는 즉시, 재접속 `last_seen`은 마지막 저장 뒤 10분이 지났을 때만 기록) 회수 시 열린 WS도 닫는다. 수락 스레드는 unix에서 상태 락 밖에서 리스너와 깨우기 소켓을 poll하고, 리스너를 닫는 쪽은 수락 스레드가 poll용 복제 fd를 놓을 때까지 기다린다. 고른 연결 모드는 `mobile-gateway.json`(0600)에 남겨 데몬 재시작 뒤에도 유지한다. 외부 접속(Tunnel 와이어 모드)은 고정 출처에 현재 PWA 버전이 배포됐는지 확인한 뒤 Durable Object E2EE 릴레이로 연결하고, 릴레이가 연결되면 QR을 낸다. DO는 빈 `MUX_OPEN`만 보내며 인증 챌린지는 데몬이 생성해 `MUX_TEXT`로 보낸다. 퀵터널은 무보증·엣지 연결 1개·재시작마다 주소 변경으로 불안정해 예비 경로에서 뺐다(2026-10-23). 릴레이 첫 시도가 실패하거나 현재 PWA 버전이 없으면 페어링 요청은 그 이유로 바로 실패하고 릴레이는 뒤에서 재시도하며, 이전 버전이 띄워 둔 퀵터널은 정리한다. 명시적 Named Tunnel만 터널을 띄운다. 아래 터널 감시·준비 판정은 Named Tunnel과 테스트 고정 터널에 적용되고, 터널은 리스너가 열려 있는 동안 cloudflared를 감시해 죽거나 90초 안에 준비되지 않으면 2초~1분 백오프로 다시 띄우고, 터널 준비는 현재 에셋 해시가 고정 출처에 배포돼 있으면 첫 HTTP 200에서(PWA가 터널 WS 530을 재시도로 견딘다), 아니면 URL이 찍히고 33초가 지났고 공개 URL이 HTTP 200을 8번 연속(400ms 간격) 돌려줬을 때로 판정하며(퀵 터널은 첫 200 뒤에도 URL 뒤 약 28~33초까지 530이 섞이고 그 사이 200이 20번 넘게 이어지기도 했다, 2026-10-19 단독 실측 11회), 페어링 IPC는 릴레이 또는 터널 준비를 최대 45초 기다린 뒤 답한다. 앱은 이를 백그라운드에서 처리하며, 대기 중에도 연결 경로 변경·기기 해제·다른 화면 이동을 허용한다. 요청 세대로 이전 결과를 무시하며 1초 이상 대기할 때 준비 상태를 표시한다. 퀵 터널은 Cloudflare 무보증 서비스이고 엣지 연결이 1개(`ha-connections:1`)다. 기한까지 준비되지 않아도 터널 URL이 찍혔다면 터널 출처 QR로 강등한다. 해당 에셋 해시가 배포돼 있으면 QR·알림은 고정 출처 `https://krill-mobile.pages.dev/m/<hash>/`를 사용하고, Tunnel WS만 허용한 Origin과 에셋 버전으로 연결한다. 랑데부 식별자·게시 키는 의도 파일에 영속화하고 터널 준비 후 현재 URL을 게시·재시도하며, 고정 출처가 준비되지 않았으면 터널 출처로 강등한다. cloudflared가 없으면 게이트웨이 락 밖에서 공식 고정 버전을 내려받아 SHA-256 검증 후 상태 디렉터리에 원자 설치한다. 모바일 화면 연결의 최소 프레임 간격은 33ms(데스크톱 8ms)다. 인증된 WS는 30초마다 ping하고 응답 없는 주기가 3번 이어지면 닫는다. 미인증 연결은 4개, 전체는 12개가 상한이다. 모바일 2대의 footprint 증가 ≤5MB·데몬 전체 ≤60MB가 예산이며 측정으로 검증한다.
- **전원**: `IOPMAssertionCreateWithName` 직접, 하트비트는 잠자기 시간 제외.

### 4.1 장애별 보장 범위 (규범)

| 장애 | 에이전트 프로세스 | 화면·출력 | 비고 | Windows (M11) |
|---|---|---|---|---|
| 앱 크래시·종료·업데이트 | 산다 | 무손실(링·세그먼트 범위 안), 넘치면 Gap 표시. 레이아웃 전체 복원(창 크기·위치·전체화면, 사이드바 펼침·선택, 탭·분할·포커스, 파일 커서·미리보기, 우측 패널 열림·탭·폭, 원격 호스트 매핑) 후 같은 세션 ID에 재연결 | 핵심 보장(OWE-842) | 산다·무손실(OWE-836: 헤드리스 앱 `taskkill /F` 1,000회 × inbox·번들, SHA-256 일치·Gap 0). 업데이트는 rename 교체(OWE-837), GUI 실행 중 교체 뒤 업그레이드 요청은 미확인 |
| krilld 크래시(첫 1회) | 산다(holder가 fd 보유) | krilld가 PTY에서 읽었지만 세그먼트에 쓰기 전 바이트는 **잃을 수 있다** → 재기동 후 Gap 표시 | 첫 크래시는 즉시 재기동, 반복 크래시만 백오프 | 산다(holder가 핸들 보유·재전달). 읽고 저장 전 바이트는 잃을 수 있고 Gap 표시(OWE-836: 30회 × 2모드, 18만~22만 줄 중 0~50줄) |
| krilld 업그레이드 | 산다 | 리더 정지·세그먼트 봉인 후 교체라 무손실(읽지 않은 출력은 커널 PTY 버퍼에서 새 krilld가 읽음) | MK-062: 앱이 새 빌드일 때만 요청(다운그레이드 금지, 같은 버전 개발 빌드는 소스 해시가 다르면 교체), holder exec 핸드오프 → 새 krilld 즉시 기동, 세대 하나 | krilld 재기동 경로의 세션 유지는 실측(OWE-833: 번들 모드 krilld 교체 후 같은 셸). 업그레이드 요청 경로의 실기 재검증은 대기 중. 업데이트 정책은 활성 세션의 holder를 유지하고 krilld만 교체한다. holder는 모드와 무관하게 세션을 보유하면 교체를 거절(EBUSY)한다. 이를 지원하지 않는 구 데몬에는 세션 목록 조회와 무관하게 Upgrade를 보류한다(조회 직후 Spawn 경합 방지). 구 런타임은 OS 재시작 등 자연 종료 뒤 새 바이너리로 기동된다 |
| holder만 크래시 | 산다(krilld도 fd 보유) | 무손실 | 종료 코드는 잃음(kqueue NOTE_EXIT로 종료만 감지). 개발 빌드는 클라이언트가 감시 연결의 상실 통보·초기 고아 상태를 보고 새 holder를 띄워 재결합(MK-018·OWE-493). 서명 경로는 MK-106 | 산다·무손실. krilld가 ConPTY·세션 프로세스·Job·IOCP 사본을 보유하고, 표준 앱·CLI 재접속이 새 holder를 띄워 Adopt한다. 기존 세션 PID 유지·실시간/재생 해시 일치·Gap 0, 재결합 후 새 세션 생성·크기 변경 요청·종료 및 정리 후 holder·krilld 잔여 0 확인(OWE-913: inbox·번들 각 5회). 재결합 전에는 새 세션 `NoHolder` |
| holder·krilld 동시 소실, 재부팅 | 죽는다 | 레이아웃은 위와 같이 복원. 죽은 세션 자리마다 최신 cwd(셸 내부 `cd` 반영, 폴더·`launches`와 분리 보관)·셸 설정으로 새 터미널을 자동 재스폰하고 이전 세그먼트 화면·스크롤백을 새 세션 위쪽 히스토리로 보존(재스폰 실패·holder 없음 때만 읽기 전용 콜드 화면). 에이전트 세션은 `Request::Resume`으로 대화 재개 | OWE-842 | 죽는다. 마지막 화면 읽기 전용 복원(OWE-836). 재부팅·로그오프는 미확인 |
| 맥 잠자기 | 산다 | 무손실 | 깨어난 뒤 자동 재접속 | 미확인(잠자기·절전 미측정) |

Windows 열은 2026-10-19 GCP Windows Server 2022 VM 실측(release, 헤드리스 앱 프로세스)이다. Windows 10/11 데스크톱·GUI 앱·대화형 로그온은 아직 미확인이라 Windows 배포 전 실기 재확인이 필요하다(PRD 플랫폼 절).

Windows SSH 클라이언트는 로컬 AF_UNIX 연결마다 `ssh -T -W <원격.sock>`을 띄워 POSIX 원격 krilld에 붙는다(OWE-915). ssh.exe의 Unix 소켓 `-L`과 로컬 TCP 포워딩은 쓰지 않는다. 세 stdio의 자식 끝도 overlapped named pipe로 열며, HOME 조회·관리형 SSH 명령도 같은 경로를 쓴다. GCP Windows Server 2022 → Mac 전용 sshd에서 기존 세션 Hello·Attach·원본 출력 수신과 터널 제거 뒤 같은 세션 PID 생존을 확인했다(내부 Windows SSH 검증 기록). Windows 앱 강제 종료 뒤 SSH 재연결·원격 krilld 크래시 생존을 이번 SSH 스파이크에서 별도로 재측정하지는 않았다.

Windows **원격** 호스트(macOS 앱·CLI → Windows PC)는 원격 krill이 중계한다. Windows용 OpenSSH sshd(9.5p2)는 Unix 소켓 포워딩(`-L`·`-W` 경로)을 `open failed`로 거부하고 원격 셸도 cmd.exe라 `/bin/sh` 질의가 실패한다(2026-10-24 Windows 11 실측). 그래서 POSIX 질의가 원격에서 실패하면(ssh 종료 코드 255가 아닌 실패) `krill __bridge --probe`로 다시 묻고, 되면 로컬 소켓 연결마다 `ssh … krill __bridge`를 띄워 그 stdin/stdout을 원격 `%LOCALAPPDATA%\Krill\run\krilld.sock`에 잇는다. keepalive(`--probe`)가 `ControlMaster`를 열어 연결별 ssh는 다시 인증하지 않는다. 원격 krilld는 깨우지 않으므로 Windows PC에서 Krill이 떠 있어야 하고, 원격 krill에 `__bridge`가 없으면(구버전) 업데이트 안내로 끝난다. 관리형 설치·업데이트는 하지 않는다(그 PC에서 한다). Windows 앱에서 Windows 원격으로 붙는 경로는 아직 없다. 가짜 Windows ssh로 진짜 holder·krilld의 세션 생성·입출력을 검증했고(`crates/krill/tests/remote_bridge.rs`), 실제 Windows PC에서의 세션 왕복은 `__bridge`가 들어간 Windows 빌드를 배포한 뒤 확인한다(미확인).

### 4.2 성능 설계

- PTY 리더: 직전 read가 1,024B로 꽉 찼을 때만 5~20µs 짧게 다시 읽는다(깨어남 15,513→246~577회, 처리량 1.6~2배). 타이머로 모으거나 잠들면 처리량 붕괴 — 금지(MK-019).
- vte sync 버퍼는 한 번 크게 쓰이면 세션당 약 1.9MB가 남는다 → 비활성 세션은 파서 재구성 등으로 축소(MK-009·MK-035).
- alacritty는 1줄 스크롤에도 화면 전체를 dirty로 표시 → 델타에 행 이동 연산 필수(MK-010·MK-023).
- zstd는 세그먼트당 스트리밍 인코더 하나 + 레코드 경계 flush(레코드마다 인코더 생성 금지, MK-024).
- krilld 재기동: 첫 재기동 즉시, listen 소켓은 holder가 쥐고, 재구성 끝난 세션부터 읽기 재개(재구성 1MiB당 3.7ms, MK-026·MK-062).

### 4.3 클라이언트 프로토콜 번호 등록표 (규범 — 새 번호는 여기 먼저 등록)

2026-10-15 야간에 서로 다른 워커가 같은 번호를 골라 FEAT_CONTROL·FEAT_INPUT(둘 다 `1<<1`), FEAT_HOOK(`2`)·hook EVENT/ACK(`0x30/0x31` = control REQUEST/REPLY)가 충돌했다. 번호를 새로 쓰는 이슈는 이 표에 행을 추가하는 것부터 한다. **병렬 작업 중에는 "다음 빈 번호"를 워커가 고르지 않는다 — 지휘자가 디스패치 사양에 번호를 배정한다**(2026-10-15 MK-043·OWE-493이 같은 `1<<5`를 고른 사고).

| 기능 비트 | 값 | 모듈 |
|---|---|---|
| FEAT_SCREEN | `1 << 0` | `krill_proto::screen` |
| FEAT_CONTROL | `1 << 1` | `krill_proto::control` |
| FEAT_INPUT | `1 << 2` | `krill_proto::input` |
| FEAT_HOOK | `1 << 3` | `krill_proto::hook` |
| FEAT_WORKTREE | `1 << 4` | `krill_proto::control` (워크트리 생성 확장) |
| FEAT_WORKTREE_REMOVE | `1 << 5` | `krill_proto::control` (워크트리 삭제·정리 확장) |
| FEAT_GIT_CHANGES | `1 << 6` | `krill_proto::control` (변경 파일·numstat 조회 확장, 기존 REQUEST/REPLY 사용) |
| FEAT_HOLDER_WATCH | `1 << 7` | `krill_proto::daemon` (감시 연결의 holder 상태·상실 통보) |
| FEAT_SEARCH | `1 << 8` | `krill_proto::search` (터미널 검색) |
| FEAT_CLEAR_VIEWPORT | `1 << 21` | `krill_proto::term_modes` (PTY 입력 없이 현재 커서 행만 위에 남기고 화면·스크롤백 지우기) |
| FEAT_SEARCH_OPTIONS | `1 << 20` | `krill_proto::search` (대소문자·정규식 옵션, SEARCH_OPTIONS만 새 피어에 전송) |
| FEAT_SCREEN_META | `1 << 9` | `krill_proto::screen` (화면 부가 정보: 제목·벨·커서 모양·OSC 8 링크 URI, OWE-490) |
| FEAT_CLI | `1 << 10` | `krill_proto::control` (CLI 화면 캡처·크기, 기존 REQUEST/REPLY 사용, OWE-410) |
| FEAT_WAIT_STATUS | `1 << 11` | `krill_proto::control` (세션 상태·원본 출력 정지 시간 조회, Request 9 / Reply 13, OWE-412) |
| FEAT_HISTORY_VIEW | `1 << 24` | `krill_proto::screen` (연결별 frozen history token; source/clear 세대·원본 식별과 bounded decoded range cache, 손실·상한은 명시적 상태) |
| FEAT_CAPTURE_SINCE | `1 << 23` | `krill_proto::control` (협상된 읽기 전용 plain capture; session/revision/seq 같으면 본문 생략, 기존 Capture DTO 불변) |
| FEAT_ROLES | `1 << 12` | `krill_proto::roles` (interactive/observer·크기·포커스, OWE-415 지휘자 배정) |
| FEAT_TERM_MODES | `1 << 13` | `krill_proto::term_modes` (입력 모드 동기화, OWE-394) |
| FEAT_SIDEBAR | `1 << 14` | `krill_proto::control` (세션 cwd·워크트리 조회, Request 10 / Reply 14, OWE-411 지휘자 배정) |
| FEAT_PROVIDER_DETAILS | `1 << 15` | `krill_proto::usage` (제공자별 상세 사용량·계정 전환 상태, 기존 사용량 응답 뒤 확장) |
| FEAT_ACCOUNT_CANCEL | `1 << 22` | `krill_proto::usage` (실패한 계정 전환 취소·원본 세션 입력 재개, 명시적 동의 후에만 보류 입력 전달) |
| FEAT_HISTORY | `1 << 16` | `krill_proto::history` (기록 삭제, 전용 요청·응답, OWE-440) |
| FEAT_USAGE | `1 << 17` | `krill_proto::usage` (사용량·프로바이더 다계정 조회·전환·자동전환, 전용 요청·응답, MK-084·MK-107·MK-108) |
| FEAT_ORCH_RETAIN | `1 << 19` | `krill_proto::orchestration` (retain 요청·상태, v0.1.23 지휘자 배정; 구 피어 응답은 Stopped로 변환) |
| FEAT_COLD | `1 << 18` | `krill_proto::control` (콜드 복원 화면 조회, Request 14 / Reply 18 — 2026-10-15 main e5f7ec4 기준, 병합 순서로 지휘자가 맞춘다, OWE-397 지휘자 배정) |
| FEAT_AGENTS | `1 << 31` | `krill_proto::control` (전역 권한·프로필, 세션 실행 권한 조회, OWE-488; 기존 Create 재사용) |
| FEAT_AI_VAULT | `1 << 32` | `krill_proto::agent_history` (AI Vault 멀티 에이전트 대화 전문 검색·재개 `Action::Search`·`Action::ResumeVault`/`Response::Searched`, MK-181·OWE-735 지휘자 배정) |
| FEAT_CLIPBOARD | `1 << 33` | `krill_proto::clipboard` (OSC 52 쓰기 통지, OWE-398 지휘자 배정) |
| FEAT_SESSION_FORK | `1 << 34` | `krill_proto::control` (에이전트 세션 포크 `Program::Agent { id: "fork:<spec>" }`, MK-182·OWE-736 지휘자 배정; 미협상 피어에 요청 금지) |
| FEAT_UPGRADE | `1 << 36` | `krill_proto::control` (krilld 업그레이드 요청 `Request::Upgrade`/`Reply::Upgrade` 한 쌍, MK-062 지휘자 배정) |
| FEAT_WINDOWS_UPGRADE | `1 << 28` | `krill_proto::control` (Windows 활성 세션의 holder를 유지하는 데몬 업그레이드. 미협상 구 데몬에는 Upgrade 요청 보류(런타임 자연 종료 뒤 새 바이너리 기동). window-error 지휘자 배정) |
| FEAT_RESOURCES | `1 << 38` | `krill_proto::resources` (Resource Manager `Request::Resources`/`Reply::Resources` 한 쌍, OWE-436 지휘자 배정) |
| FEAT_GIT_SYNC | `1 << 41` | `krill_proto::ipc` (Git 원격 동기화 `Edit::Push`·`Pull`·`Sync`·`CommitAndPush`, MK-184·OWE-738 지휘자 배정; 구 피어에는 전송 금지) |
| FEAT_RESUME_HISTORY | `1 << 30` | `krill_proto::control` (재개 전 세그먼트·마지막 화면의 지연 HISTORY 페이지, 지휘자 배정 2026-10-23; 미협상 피어는 기존 임시 prior 유지, 새 kind·DTO 없음) |
| FEAT_RESUME | `1 << 42` | `krill_proto::control` (세션 재개 요청 `Request::Resume`/`Reply::Resumed` 한 쌍, MK-052·OWE-417 지휘자 배정) |
| FEAT_GIT_WATCH | `1 << 44` | `krill_proto::control` (FSEvents 변경 감시·가시성 제어 `Request::GitWatch`/`Reply::GitWatch` 한 쌍, OWE-585 지휘자 배정) |
| FEAT_HEARTBEAT | `1 << 45` | `krill_proto::daemon` (앱 감시 연결 ping/pong, OWE-586 지휘자 배정) |
| FEAT_THEME | `1 << 46` | `krill_proto::theme` (앱 테마·셀 픽셀 크기 푸시, OWE-590 지휘자 배정) |
| FEAT_CLEAR_SCROLLBACK | `1 << 47` | `krill_proto::control` (세션 스크롤백 지우기 `Request::ClearScrollback`/`Reply::ScrollbackCleared` 한 쌍, OWE-873b 지휘자 배정; enum 끝에 추가) |
| FEAT_GIT_EDIT | `1 << 48` | `krill_proto::control` (Git index 변경·커밋 `Request::GitEdit`/`Reply::GitEdited` 한 쌍, MK-088·OWE-453 지휘자 배정) |
| FEAT_USAGE_REFRESH | `1 << 25` | `krill_proto::usage` (수동 사용량 새로고침 `UsageRequest::RefreshUsage`, 응답은 `UsageReply::Accounts`; 주기·Retry-After 무시. enum 끝에 추가, 미협상 피어에는 `ListAccounts`) |
| FEAT_USAGE_RESET | `1 << 49` | `krill_proto::usage` (Codex banked reset 크레딧 소진 `UsageRequest::ConsumeResetCredit`/`UsageReply::ResetConsumed` 한 쌍; enum 끝에 추가) |
| FEAT_AGENT_HISTORY | `1 << 50` | `krill_proto::agent_history` (요청 시 transcript 스캔·재개, control 한 쌍, MK-082·OWE-447 지휘자 배정) |
| FEAT_CHAT_POLL | `1 << 63` | `krill_proto::agent_history` (하이브리드 채팅 기록 조회·이전 대화 페이지) |
| FEAT_HIBERNATION | `1 << 51` | `krill_proto::control` (자동 종료 세션 재개 `Request::Hibernation`/`Reply::Hibernation`, MK-081·OWE-446 지휘자 배정) |
| FEAT_WORKTREE_PREPARE | `1 << 52` | `krill_proto::control` (`Request::PrepareWorktree`/`Reply::WorktreePreparation`, MK-095·OWE-460 지휘자 배정; enum 끝에 추가) |
| FEAT_USAGE_AGY | `1 << 53` | `krill_proto::usage` (Antigravity 계정 enum 확장; 미협상 피어 응답에서 해당 계정·전환 이벤트 제외) |
| FEAT_USAGE_PROVIDER_METRICS | `1 << 26` | `krill_proto::usage` (제공자별 자동전환 기준 확장. 미협상 피어에는 해당 규칙을 응답에서 제외하고 변경 요청 전송을 거부한다. v0.1.44 배정) |
| FEAT_USAGE_RULES | `1 << 54` | `krill_proto::usage` (계정별 자동전환 규칙 요청 및 응답 꼬리 확장) |
| FEAT_HISTORY_PAUSE | `1 << 55` | `krill_proto::history` (기록 일시중지·재개 요청/ACK; 세션·PTY·라이브 화면 유지) |
| FEAT_MOBILE | `1 << 56` | `krill_proto::mobile` (M9 기기·페어링·게이트웨이, OWE-678) |
| FEAT_USAGE_STATUS | `1 << 57` | `krill_proto::usage` (계정별 사용량 상태·갱신 시각 응답 꼬리 확장, MK-143·OWE-703) |
| FEAT_SESSION_ACCOUNT | `1 << 58` | `krill_proto::control` (세션별 사용 계정 `Request::SessionAccounts`/`Reply::SessionAccounts` 한 쌍, MK-147·OWE-707; enum 끝에 추가) |
| FEAT_USAGE_RATELIMIT | `1 << 60` | `krill_proto::usage` (상태 꼬리 한도 도달 구분 수신, 없으면 `Error`로 폴백, OWE-703·OWE-710) |
| FEAT_USAGE_MANAGE | `1 << 59` | `krill_proto::usage` (계정 라벨 변경·데이터 삭제·고아 프로필 정리 요청과 `UsageReply::Orphans`, MK-144·145·OWE-704·705 지휘자 배정; enum 끝에 추가) |
| FEAT_ACCOUNT_POLICY | `1 << 61` | `krill_proto::usage::policy` (`Create.account` 선택 꼬리·고급 자동전환 정책·전환 이력·모델별 주간 창, MK-152·OWE-712 지휘자 배정; 구 피어에는 꼬리 생략) |
| FEAT_ACCOUNT_CONNECTION | `1 << 62` | `krill_proto::usage::connections` (연결 상태·직접 사용량 설정 꼬리와 `SetDirectUsage`, OWE-719; 구 피어에는 꼬리 생략) |
| FEAT_REMOTE_FS | `1 << 27` | `krill_proto::control` (원격 파일 탐색·열기·원자적 저장·Quick Open·루프백 포트 포워딩 `Request::RemoteFs`/`Reply::RemoteFs` 한 쌍; enum 끝에 추가) |

OWE-722(MK-159)의 `SetProvisioning`·`GetProvisioning`/`UsageReply::Provisioning`은 미출시 `FEAT_USAGE_MANAGE`(`1 << 59`)에 포함한다(새 비트 없음, 미협상 피어에 요청 금지).

새 번호는 미병합 예약을 포함해 지휘자가 배정한다. 사용량 비트·kind는 재사용하지 않는다.

| 메시지 상수(UDS, 클라이언트 채널) | 값 | 방향·용도 |
|---|---|---|
| `mobile::MOBILE_REQ` | `0x45` | 클라→데몬 (FEAT_MOBILE 필요) |
| `mobile::MOBILE_REPLY` | `0x65` | 데몬→클라 (FEAT_MOBILE 필요) |
| `theme::THEME` | `0x44` | 클라→데몬 (FEAT_THEME 협상, 전체 범위 interactive 앱만) |
| `term_modes::TERM_MODES` | `0x2b` | 데몬→클라 |
| `ipc::SERVER_HELLO` | `0x01` | 데몬→클라 |
| `ipc::CLIENT_HELLO` | `0x02` | 클라→데몬 |
| `screen::ATTACH` | `0x10` | 클라→데몬 |
| `screen::RESYNC` | `0x11` | 클라→데몬 |
| `screen::HISTORY` | `0x12` | 클라→데몬 (FEAT_SCREEN, 지연 히스토리 요청) |
| `screen::ATTACHED` | `0x20` | 데몬→클라 (화면 연결 승인) |
| `screen::UPDATE` | `0x21` | 데몬→클라 (화면 snapshot/delta) |
| `screen::OUTPUT` | `0x22` | 데몬→클라 |
| `screen::GAP` | `0x23` | 데몬→클라 |
| `screen::CLOSED` | `0x24` | 데몬→클라 |
| `screen::REFUSED` | `0x25` | 데몬→클라 |
| `screen::HISTORY_ROWS` | `0x26` | 데몬→클라 |
| `screen::HISTORY_VIEW` | `0x18` | 클라→데몬 (FEAT_HISTORY_VIEW, frozen logical range) |
| `screen::HISTORY_VIEW_ROWS` | `0x2d` | 데몬→클라 (stable identity / Missing·Invalidated·Capacity) |
| `search::SEARCH` | `0x15` | 클라→데몬 |
| `term_modes::CLEAR_VIEWPORT` | `0x47` | 클라→데몬 (FEAT_CLEAR_VIEWPORT, interactive 권한 필요) |
| `search::SEARCH_OPTIONS` | `0x46` | 클라→데몬 (FEAT_SEARCH_OPTIONS 필요, 응답은 기존 SEARCH_RESULT) |
| `search::SEARCH_RESULT` | `0x2a` | 데몬→클라 |
| `input::INPUT` | `0x13` | 클라→데몬 (FEAT_INPUT 협상) |
| `input::INPUT_ACK` | `0x27` | 데몬→클라 (적용·종료·권한 거부 결과) |
| `hook::EVENT` | `0x14` | 클라→데몬 (hook 이벤트) |
| `hook::ACK` | `0x28` | 데몬→클라 (hook 수신 확인) |
| `history::HISTORY_REQ` | `0x40` | 클라→데몬 (FEAT_HISTORY 필요) |
| `history::HISTORY_REPLY` | `0x60` | 데몬→클라 |
| `usage::USAGE_REQ` | `0x41` | 클라→데몬 (FEAT_USAGE 협상, 사용량·계정 조회·전환) |
| `usage::USAGE_REPLY` | `0x61` | 데몬→클라 (FEAT_USAGE 협상, 사용량·계정 스냅샷) |
| `control::REQUEST` | `0x30` | 클라→데몬 |
| `control::REPLY` | `0x31` | 데몬→클라 |
| `roles::ROLE` | `0x17` | 클라→데몬 (FEAT_ROLES 필요) |
| `roles::ROLE_ACK` | `0x2c` | 데몬→클라 (FEAT_ROLES 필요) |
| `daemon::HOLDER_STATUS` | `0x29` | 데몬→클라 |
| `clipboard::CLIPBOARD` | `0x62` | 데몬→클라 (FEAT_CLIPBOARD, OSC 52 쓰기) |
| `daemon::PING` | `0x43` | 클라→데몬 (감시 연결만, FEAT_HEARTBEAT, OWE-586) |
| `daemon::PONG` | `0x63` | 데몬→클라 (감시 연결만, FEAT_HEARTBEAT, OWE-586) |

`term_modes::Modes.flags`는 bit 0 APP_CURSOR, 1 MOUSE_CLICK, 2 MOUSE_DRAG, 3 MOUSE_MOTION, 4 SGR_MOUSE, 5 BRACKETED_PASTE, 6 ALT_SCREEN, 7 ALTERNATE_SCROLL(DEC 1007)을 사용한다. 알 수 없는 비트는 무시한다. 대체 화면에서 마우스 보고가 꺼져 있고 DEC 1007이 켜져 있으면 클라이언트는 휠을 커서 모드에 맞는 위/아래 키로 보낸다.

`krill_proto::control`의 Request/Reply enum은 postcard 변형 순서가 곧 와이어 번호라 **끝에 추가만** 하고, 병렬 작업 중에는 지휘자가 변형 번호를 배정한다(2026-10-15 MK-045: Request 7·8, Reply 11·12).

Resource Manager(MK-071)는 control에 `Request::Resources(Action)`·`Reply::Resources` 한 쌍만 더한다(페이로드 `krill_proto::resources`, `FEAT_RESOURCES`를 협상했을 때만 보낸다 — 앱은 미지원 데몬이면 '데몬 업데이트 필요'를 보이고 요청하지 않는다). 메모리는 krilld가 `proc_pid_rusage`로 읽은 `phys_footprint`(요청자·krilld·holder·세션 루트와 ppid 후손 합), 디스크는 상태 디렉터리의 할당 블록 합이다. `RestartDaemon`은 전체 범위 interactive만, 확인 대화상자는 앱이 띄우고, krilld는 holder `UpgradeKrilld`(MK-068과 같은 경로) 승인 뒤 답을 보내고 끝난다. 세션 kill은 기존 `Close`.

**예약 번호 범위**: 기능 비트 `1 << 27`~`1 << 29`, kind 클라→데몬 `0x1A`~`0x1F`·데몬→클라 `0x2E`~`0x2F`. (`control` Request/Reply enum은 postcard 순차 인덱스라 끝에만 추가하며 현재 Request 0..=26, Reply 0..=30까지 배정됨).

오케스트레이션의 `Request::Orchestration` 내부 opcode는 기존 0..15를 유지한다. 16은 Run 필터 메시지 조회, 17은 메시지 ID별 ACK이며, v0.1.46의 구성된 워커 시작·실행·실패 기록·세션별 Run 선택·배치 조회·확장 스냅샷·워커 관리 요청은 18..24에 덧붙인다. 새 요청이 구버전 데몬에서 거부되면 같은 변경을 디스크로 재실행하지 않는다. 기존 `Snapshot`·`Dispatch` 응답은 N-1 postcard 구조를 고정한 별도 wire 타입으로 직렬화하며, 배달 영수증과 확장 스냅샷·디스패치는 새 응답 variant로만 보낸다. 배치 ACK는 조회 시 고정한 메시지 ID만 확인 처리해 나중에 도착한 메시지를 소비하지 않는다. 워커 해제는 실행 상태·세션 소유권을 먼저 확인하며, 빌린 세션이나 다른 디스패치로 재배정된 PTY를 닫지 않는다.

**kind 확장 범위(2026-10-15)**: `0x10`~`0x1F`·`0x20`~`0x2F`가 차서 새 전용 kind는 클라→데몬 `0x40`~, 데몬→클라 `0x60`~를 배정한다. control enum에 빈 번호를 채우는 예약 변형(`#[serde(skip)]`)은 금지 — 직렬화·역직렬화 변형 번호가 어긋날 수 있다.

holder 채널(`krill_proto::holder`)은 별도 번호 공간이다(`0x01`~`0x08`, `0x81`~`0x88`). `krill-proto`에는 기능 비트 겹침 const assert와 kind 중복 테스트를 둔다.

### 4.4 모바일 E2EE v2 전송 계약 (M9)

모바일 관리 DTO는 별도 UDS 연결에서 `FEAT_MOBILE`을 협상해 사용한다. 게이트웨이 `/ws`는 새 server challenge(32B hex)와 client nonce(32B hex)를 교환한 뒤, salt=`server || client`와 QR secret(최초) 또는 SHA256(장기 credential raw 32B)(재접속)으로 HKDF-SHA256 키를 만든다. 방향별 info는 `mk-mobile-v2/client-to-daemon` / `mk-mobile-v2/daemon-to-client`, nonce prefix는 ASCII `MKC2` / `MKD2`다. nonce는 prefix 4B + seq 8B LE, AAD는 seq 8B LE이며 seq는 연결마다 0부터 시작한다. 따라서 재접속에서도 키·nonce 조합을 재사용하지 않는다.

첫 encrypted payload는 클라의 label JSON proof와 데몬의 device_id·credential JSON 응답이다. 인증 성공 뒤부터 각 encrypted WS payload는 기존 IPC 프레임 하나이며 내부 UnixStream pair와 동일 Registry 경로를 탄다. 데몬의 장기 검증자는 공개 기기 목록·상태에서 지운다. 전체 회수는 대기 QR까지 폐기하고 리스너·열린 WS를 즉시 닫는다.

PWA는 `/`, `/manifest.json`, `/app.js`, `/app.css`, `/icon.svg`를 gzip 사전 압축 에셋으로 내장한다. Tailscale HTTP에서도 순수 JS SHA256·HKDF·ChaCha20-Poly1305와 `crypto.getRandomValues`로 동작하며, QR fragment를 즉시 지우고 기기 credential은 localStorage에 보관한다. 기존 모바일 enum 끝에 추가한 `MobileRequest::Workflow(String)`(postcard variant 7) / `MobileReply::Workflow(String)`(variant 6)은 인증된 JSON 워크플로를 운반한다. 끝에 추가한 `CancelPairingTicket`(variant 8)은 기기 권한·PTY를 유지하고 대기 QR만 취소한다. 인증 거부 close 1008, 기기 상한 4009, 인증 cooldown 1013을 구분한다. 클라이언트는 연결 전체 20초·직렬 RPC 대기 포함 15초의 deadline 뒤 연결을 복구하며 입력·변경 요청을 자동 재전송하지 않는다. direct service worker는 정적 offline shell을 저장하며 auth 쿼리는 direct/cloud 모두 원본 요청을 네트워크로만 보내고 캐시하지 않는다. 고정 출처 manifest는 정적 루트 런처이므로 설치 첫 실행의 인증 승계는 보장하지 않고 필요한 경우 새 QR 스캔을 안내한다. 세션·입력·계정·Git 정보는 UnixStream pair의 기존 Registry 경로를 사용하고 M8 질문·gate는 동일한 원자 저장소를 사용한다. observer·세션 범위 피어는 Workflow를 호출할 수 없다. Chat의 역사적 권한·질문 카드는 `review_in_term`으로 현재 터미널에서 확인한다. 현재 질문의 원자 ID가 없는 legacy JSON `approve`·`answer_question`은 fail-closed이며 실제 Terminal 직접 키 입력은 유지한다. JSON `snapshot`은 경로 바이트 기반 repo/worktree 그룹·상태 근거·64세션 제한 여부·실제 hostname/platform과 capabilities를 제공하며, `create_terminal {cwd_session}`은 서버에서 조회한 cwd의 로그인 셸만 생성하고 `close_terminal {session}`은 명시 요청을 기존 Close IPC로 전달한다.

모바일 PWA는 `krill-mobile-hosts-v1`에 PC별 credential·릴레이 ID·표시 이름과 마지막 선택 PC를 보관하고, `krill-mobile-v2`에 현재 연결을 보관한다. 고정 출처의 릴레이 PWA에서는 새 페어링이 기존 PC를 지우지 않으며 저장한 PC마다 독립된 인증·스냅샷 연결을 동시에 유지한다. 홈의 PC 카드는 각 연결의 실제 상태를 표시하며 선택하면 해당 PC의 작업 공간으로 바로 들어간다. 화면을 바꿔도 다른 PC 연결은 닫지 않고, 터미널 렌더러만 정리한다. 세션·초안·Chat 캐시는 PC와 기기 ID로 구분하고 이전 화면의 늦은 요청은 다른 PC로 전달하지 않는다. 최근 세션은 기기·세션 ID·그룹 키·시각만 저장하고 제목·본문은 현재 스냅샷에서 얻는다. 연결이 끊긴 PC의 최근 세션은 입력할 수 없으며 다른 PC는 계속 사용할 수 있다. LAN 직접 접속의 저장소는 브라우저 출처마다 분리되므로 여러 PC를 한 목록에서 쓰려면 고정 출처 릴레이 PWA를 사용한다. 홈의 `Pair Desktop`은 QR/링크 페어링을 열고, 작업 공간 화면의 생성 동작은 연결된 데몬이 `worktree_create`를 제공할 때 활성화한다. PC가 여러 대 저장돼 있으면 생성할 PC를 먼저 선택하고 인증·스냅샷을 기다린다. `create_workspace`는 서버에서 등록된 저장소 키를 조회해 기존 `NewWorktree` IPC·Git 작업 큐를 사용하며 임의 cwd를 받지 않는다. setup 신뢰가 필요하면 스크립트와 해시를 반환하고 사용자의 실행 승인 또는 건너뛰기를 받는다. 생성 결과는 새 워크트리 키·터미널·setup 세션·경고를 구분해 반환한다.

## 5. UI와 터미널 렌더링 (T3)

- **프레임워크**: gpui(`gpui-pre`·**모든 `gpui-pre-*`**·`gpui-component`를 `=`로 묶어 고정, `cargo vendor`, Apache-2.0). gpui-pre는 Zed 공식 배포가 아니라 1인 게시 스냅샷이다(공식 `gpui`는 0.2.2 이후 1년 갱신 없음) — 공급망 위험으로 관리. 근거는 메모리가 아니라 **macOS IME 구현 경로(NSTextInputClient)가 있다는 점**이며, 실제 한글 동작은 MK-002에서 확정한다(T3 기준 일부 메서드는 미확인). Xcode 없는 빌드는 `font-kit` + `runtime_shaders`. **첫 스파이크(MK-002) 실패 시 objc2 AppKit + 커스텀 Metal로 전환.**
- **렌더**: 같은 스타일 연속 셀을 run으로 묶어 gpui 텍스트 시스템으로 그림, 셀 폭 강제, 배경 병합, vsync당 1회. **Zed 터미널 코드는 GPL-3.0이라 클린룸 구현**(구조만 참고).
- **IME**: preedit는 UI 상태로만 두고 커서 셀 위 오버레이, PTY에는 확정 텍스트만, 포커스 이탈·탭 전환 시 확정.
- **vendor 패치**: 글리프 아틀라스 nearest 샘플링(한글 획 번짐, Zed #57777), `maximumDrawableCount` 3→2(장당 25.6MB).
- **범위 밖**: 파일 에디터, 내장 브라우저, 네이티브 채팅, 자체 STT, 터미널 인라인 이미지 프로토콜(Kitty Graphics·Sixel·iTerm2 — `alacritty_terminal` 범위 밖이며, MK-102는 `libghostty-vt` C API 재평가 문서화이고 클립보드 이미지 붙여넣기는 파일 경로 삽입(MK-110)으로 처리).

## 6. 에이전트·Git (T4)

- **hook**: `krill hook <agent>`가 stdout에 `{}`를 즉시 출력·flush하고 stdin JSON을 소켓으로 전달. **전체 wall-clock 50ms 예산**(재시도 포함), 실패 시 spool 파일(0600)에 append 후 종료(fail-open), krilld가 기동 시 수거 → krilld 재기동 중 SessionStart·Stop 유실 방지. 이벤트 ID로 중복 제거. 시제품 p50 2.6ms·RSS 2.2MB.
- **주입**: Claude는 세션별 `--settings <0600 파일>`(사용자 hook과 병합 실측 확인) + `--session-id` 사전 발급. Codex는 공용 훅 설치기가 app-server로 trust를 등록한 뒤(MK-012) krilld가 완료된 설치 receipt의 고정 명령으로 세션마다 `-c hooks.<Event>`를 붙인다. 앱의 상태 감지 설정이 켜져 있으면 로컬 에이전트 생성·재개 전에 누락된 훅을 자동 설치한다. 기본 상태 디렉터리를 가리키는 명시적 오버라이드는 허용하되 별도 격리 상태나 원격 호스트 때문에 로컬 전역 설정을 바꾸지 않는다. 자동 Codex 설치는 app-server가 확인한 Krill 명령의 session trust만 갱신하며, 이전 값은 receipt에 저장해 제거 시 복원한다(이후 사용자가 수정한 값은 보존). 수동 CLI 설치의 충돌 거부와 diff 확인은 유지한다. 그 외 에이전트도 설정 병합 + 원복 기록 + `MK_PANE_ID` 가드를 쓴다. krilld는 모든 세션 자식 환경에 `MK_PANE_ID`(세션 id)와 `KRILL_STATE_DIR`만 새로 넣는다(세션 귀속은 PID 계보, OWE-503).
- **상태 모델**: `working / waiting / blocked / done / exited / unknown` + 근거 출처(`hook/process/title/screen`) + 신뢰도. 감지 우선순위: hook > 프로세스(SIGCHLD·tcgetpgrp) > OSC 제목 > 화면 규칙. `idle`은 상태가 아니라 파생 조건(working·unknown이 아니고 출력 정지 N초) — `krill wait --for idle`의 정의(MK-047).
- **CLI 대기(MK-047)**: `krill wait <id> --for idle|waiting|blocked|pattern|quiet [--timeout <초>]`. idle·quiet의 출력 정지 기본값은 3초이며 `--quiet-ms <밀리초>`로 바꾼다. quiet는 상태와 관계없이 원본 PTY 출력이 N초 멈춘 조건이고, idle은 여기에 상태가 working·unknown이 아니라는 조건을 더한다(waiting·blocked·done·exited 포함). 화면이 그대로인 제어열·미완성 UTF-8·sync 출력도 정지 시간을 초기화한다. 정지 시간은 krilld의 단조 시계로 재며, 새 세션·데몬 재기동 때부터 다시 센다. pattern은 `--pattern <정규식>`으로 지정하고 현재 화면의 ANSI 없는 평문에 적용한다(콜드 이력 검색은 MK-059). `--for pattern:<정규식>`·`--for quiet:<밀리초>`도 지원한다. 조건 충족은 0, 타임아웃은 2, 연결·세션 오류는 1로 종료한다. 타임아웃 0은 현재 조건을 한 번 검사하며, 생략하면 무제한 대기한다. 기존 working·done·exited·unknown 대기도 유지한다.
- **Hibernation(MK-081)**: 기본 꺼짐. 상태 디렉터리의 `hibernation.enabled`로 opt-in하며, 확정 hook done·30분 단조 시계·연결/입력/출력/재개 ID/자식 veto를 같은 cwd 묶음에 적용한다. 종료 표식과 대화 ID를 남긴 뒤 holder에 Terminate를 보내고, 앱에서 해당 pane을 열면 MK-052 경계로 자동 재개한다. 미지원 Dispatch 조건과 보수적 roster 정책, 복구 한계는 [8조건 대조표](agents/hibernation.md)를 따른다.
- **Git**: git CLI만(`-c core.quotePath=false`, `GIT_OPTIONAL_LOCKS=0`, `GIT_TERMINAL_PROMPT=0`). 워크트리 생성·삭제는 데몬 작업 큐(앱 꺼져도 완료), status·diff 조회는 앱.
- **워크트리 사전 준비(MK-095)**: 생성 창에서만 요청하며 데몬 전체에 준비본 하나, 디스크 예산 512 MiB, 요청 후 TTL 5분이다. `--detach --no-checkout --lock` 후 백그라운드 체크아웃하고, 제출 시 저장소·기준 커밋이 일치하면 이동·브랜치 생성으로 승격한다. 준비 중 setup·include 복사·checkout/fsmonitor hook은 실행하지 않으며 smudge/process filter 설정이 있거나 예산을 넘는 저장소는 일반 생성으로 돌아간다. 창 닫기·기준 ref 변경·명시적 취소는 해당 토큰을 취소한다. 소유 마커로 데몬 재시작 시 미완성 준비본만 정리하고, 승격한 워크트리는 보존한다.
- **설정 호환**: `krill.toml`, `orca.yaml`의 `scripts.setup`과 `.worktreeinclude`는 읽기 호환(MK-080, `scripts.archive`는 미사용). 환경 변수 `KRILL_*` + `ORCA_ROOT_PATH`·`ORCA_WORKTREE_PATH`·`CONDUCTOR_ROOT_PATH`.
- **권한 기본값**: YOLO(지원하는 에이전트의 승인 확인 우회). 데스크톱 `+`·모바일에서 만드는 새 에이전트 세션에 적용하며, 전역 토글에서 Manual을 명시적으로 선택하면 에이전트 자체 권한을 쓴다. 토글은 `agents.toml`에 저장하고 이후 생성하는 세션에 적용한다. pane 배지는 krilld가 생성 전에 저장한 세션별 실행 권한을 조회하므로 앱·데몬 재시작과 전역 토글 변경에도 유지된다.

## 7. 상용 운영 (T4)

- Developer ID + hardened runtime + notarytool + staple(macOS 러너). 엔타이틀먼트는 기본 비움, `device.audio-input`·`device.camera`·`automation.apple-events` 필요 여부는 MK-007에서 에이전트 시나리오로 실측. 서명은 안쪽부터 개별(`--deep` 금지). 인증서는 MK-104(사람).
- 자체 최소 업데이터: `latest.json` + ed25519 + `codesign --verify` + `ditto` + 교체 + 롤백. `rename(2)`는 비어 있지 않은 `.app`을 덮어쓰지 못하므로 APFS `renamex_np(RENAME_SWAP)` 또는 2단계 이동(현재→`.prev`, staging→현재)과 단계별 저널로 중간 크래시 복구. 검증·교체·재실행·health 확인·롤백은 별도 relauncher가 맡는다. Velopack과 S-updater에서 비교.
- 크래시: panic hook → 로컬 파일 → 다음 실행 때 "진단 보내기"(리댁션). 업로드는 opt-in.
  (MK-067 구현: `krill_proto::diag`. 앱·krilld panic hook → `<상태 디렉터리>/crash/crash-<unix초>-<pid>-<프로세스>.txt`(0600, MK-069 로그와 별도, 미확인 32개 상한). 앱이 다음 실행에 대화상자 → "진단 번들 만들기"는 `<상태 디렉터리>/diagnostics/`에 리댁션한 텍스트(≤4MiB, 터미널 본문 없음)를 쓰고 Finder로 보인다. 같은 번들은 `krill doctor --bundle`. `krill doctor`는 데몬 바이너리 누락·상태 디렉터리 쓰기 불가·git 없음·데몬 연결 실패를 FAIL, 미확인 크래시 로그를 WARN으로 보인다. opt-in 업로드는 아직 없다.)
- 배포: DMG(GitHub Releases) + 자체 Homebrew tap(`auto_updates true`).
- Windows 배포(M11, 후보만 — 미결정): 설치 형식은 zip·Inno Setup·MSIX·winget 중 MK-218(OWE-837)에서 고른다. 실행 중인 exe는 덮어쓸 수 없어 rename 방식 교체 + 롤백, holder exe는 버전별 경로, Velopack을 검토한다. 서명은 Authenticode(유료 인증서, 사람 결정 — MK-219/OWE-838). release 워크플로 Windows 잡과 Windows 메모리 게이트는 MK-220(OWE-839), 메모리 지표 정의는 S-W3(MK-209/OWE-828).
- 품질 게이트(macOS nightly + 릴리스 차단): 메모리 ≤ 200MB, 앱 kill -9·업데이트·krilld 크래시 생존 e2e.

## 8. 성공 지표 (출시 차단 기준)

| 지표 | 목표 | 차단선 |
|---|---|---|
| 앱+데몬 메모리(터미널 10개, phys_footprint) | ≤ 180MB | 200MB |
| 앱 크래시·SIGKILL·업데이트 시 세션 생존 | 1,000회 반복 손실 0 | 99.9% |
| 놓친 출력 재생 | 링 안에서 바이트 100%(`seq 1 1000000` 해시 일치), 넘치면 생략 표시 | 표시 없는 손실 0 |
| 업데이트 뒤 고아 데몬 | 0 | 0 |
| 앱 재시작 → 10세션 마지막 화면 | ≤ 500ms | 1.5s |
| 데몬 끊김 자동 복구 | 5초, 조작 없이, 배너 표시 | 30초 |
| 대량 출력 프레임 | p99 ≤ 16.7ms | — |
| 키 입력 에코 | p95 ≤ 16ms | 33ms |
| 출력 폭주 격리(1GB cat 중 다른 세션 입력) | p99 < 33ms | 100ms |
| 데몬 디스크 쓰기 | 지속 ≤ 8MB/분 | — |
