# Krill 코드 컨벤션 v1 (2026-10-14)

이 문서는 Rust·Cargo·Clippy·cargo-deny와 플랫폼·상위 프로젝트 문서를 검토해 정한 저장소 규칙이다. 내부 조사 원문은 공개하지 않으며, 아래 규칙 번호를 코드와 검토의 기준으로 사용한다.

강도: **MUST** = 어기면 병합 안 됨(대부분 `scripts/check.sh`가 기계로 막음). **SHOULD** = 어길 때 `reason`이나 커밋 설명에 이유. **MAY** = 선택.

## 0. ponytail과의 관계

ponytail 스킬(게으른 시니어: YAGNI → 기존 코드 → std → 플랫폼 → 설치된 의존성 → 한 줄 → 최소 코드)은 **쓴다.** Krill의 핵심 가치(가벼움)와 같은 방향이다. 단 아래 세 가지는 이 문서가 이긴다.

1. **테스트**: ponytail의 "검사 하나면 충분"은 적용하지 않는다. 생존·재생·프로토콜·파서는 §6의 테스트 의무를 따른다.
2. **의도된 경계는 YAGNI가 아니다**: `krill-term`(gpui 실패 시 교체 대비), 엔진 무관 키프레임 포맷, `krill-proto` 버전 협상, IPC 클라이언트 트레이트(gpui 테스트용)는 구현이 하나여도 유지한다. ARCHITECTURE의 "되돌리기 비용 높음" 결정에서 나온 경계다.
3. **설명을 줄이지 않는 곳**: `// SAFETY:`, `# Safety`/`# Errors`/`# Panics` 문서, 와이어 포맷 문서, `ponytail:` 주석(한계와 업그레이드 조건 필수)은 짧게 쓰되 빼지 않는다.

리뷰에서는 `/ponytail-review`(과잉 설계)와 `/code-review`(정확성)를 둘 다 쓴다. 마일스톤 끝마다 `/ponytail-debt`로 `ponytail:` 주석 장부를 확인한다.

## 1. 워크스페이스·툴체인

- **W1 MUST** 모든 크레이트는 `version`·`edition = "2024"`·`rust-version`을 `[workspace.package]`에서 상속, `publish = false`.
- **W3 MUST** 외부 의존성 버전은 루트 `[workspace.dependencies]`에만. 멤버는 `x = { workspace = true }`(cargo-deny `workspace-dependencies`가 막음).
- **W4 MUST** 패키지 레벨 `default-features = false` 덮어쓰기에 기대지 않는다(Cargo 1.99 전용, 로컬은 1.98.1).
- **W5 MUST** MSRV `rust-version = "1.98"`. 로컬은 Homebrew rustc(rustup 없음)라 `rust-toolchain.toml`은 효과 없음.
- **W6 MUST** cargo 명령에 `--locked`, `Cargo.lock` 커밋.
- **W7 MUST** 프로필은 루트에만. 패키지별 `panic`·`lto` 설정은 Cargo가 거부한다.
- **MUST** gpui: `gpui-pre`·**모든 `gpui-pre-*`**·`gpui-component`를 `=`로 고정하고 한 묶음으로만 올린다(스냅샷 안에서도 하위 크레이트 버전이 다르다). Xcode 없는 로컬·야간 빌드는 `font-kit` + `runtime_shaders` 기능(MK-003에서 확정).

## 2. 포맷·lint

- **L1 MUST** `rustfmt.toml`은 `style_edition = "2024"` 한 줄. unstable 옵션 금지(stable에서 조용히 무시됨).
- **L2 MUST** lint 정책은 루트 `[workspace.lints]`만. 멤버는 `[lints] workspace = true`. 더 엄격하게는 크레이트 루트 `#![deny(...)]`.
- **L3 MUST** `clippy::all = deny`, `clippy::pedantic = warn`(+ `-D warnings`로 사실상 차단), `must_use_candidate`·`module_name_repetitions`·`too_many_lines`·`similar_names`·`missing_errors_doc`·`missing_panics_doc`는 allow(야간 무인 개발의 노이즈 차단 — 문서 섹션은 리뷰에서 SHOULD로 본다). nursery 끔, restriction은 골라서.
- **L4 MUST** 억제는 `#[expect(lint, reason = "...")]`만. `#[allow]` 금지(`allow_attributes`).
- **L6 MUST** `clippy.toml`은 루트 하나만(크레이트 디렉터리에 두면 루트 설정을 통째로 대체).

## 3. unsafe·FFI

- **U1 MUST** 워크스페이스 기본 `unsafe_code = "deny"`. FFI 모듈(`krill-holder`·`krilld`의 `sys` 모듈 — `sys/windows.rs` 같은 Windows FFI 포함, `krill-app`의 `platform` 모듈 — objc2·Win32)만 모듈 첫 줄에 `#![expect(unsafe_code, reason = "…")]`. `krill-proto`·`krill-term`·`krill`은 `#![forbid(unsafe_code)]`.
- **U2 MUST** 모든 `unsafe` 블록 바로 위 `// SAFETY:` 주석, 블록 하나에 unsafe 연산 하나.
- **U3 MUST** `unsafe fn`·`unsafe trait`에 `# Safety` 문서.
- **U5 MUST** fd는 `OwnedFd`/`BorrowedFd`로만 주고받는다. 정수 fd를 받는 안전 함수 금지. Windows는 `OwnedHandle`/`BorrowedHandle`(소켓은 `OwnedSocket`/`BorrowedSocket`)로만 주고받고, 원시 `HANDLE`·`RawHandle`을 받는 안전 함수 금지. 다른 프로세스로 넘기는 핸들 값(`DuplicateHandle` 결과)은 FFI 모듈 안에서만 원시값으로 다룬다.
- **U7 MUST** fork~exec 사이(`pre_exec`)는 async-signal-safe 호출만(할당·`std::env`·락·로깅 금지). 자식 setsid는 `pre_exec` 안에서 libc `setsid`. WezTerm처럼 `pre_exec`에서 fd를 나열하는 코드는 따라 하지 않는다. Windows에는 fork·`pre_exec`가 없다: 자식은 `std::process::Command`(+ `CommandExt::creation_flags`)로 만들고, ConPTY·Job Object처럼 `STARTUPINFOEX` 속성이 필요할 때만 FFI 모듈에서 `CreateProcessW`를 부른다. 자식에게 넘길 핸들은 `PROC_THREAD_ATTRIBUTE_HANDLE_LIST`로 명시하고 전체 상속에 기대지 않는다.
- **U8 MUST** `std::env::set_var`/`remove_var` 금지(`clippy.toml disallowed-methods`). 자식 환경은 `Command::env`.
- **U10 MUST** C·objc2 콜백 밖으로 panic이 나가지 않게 한다(`extern "C"` 안 panic은 abort).
- **MUST** PTY 열기는 alacritty_terminal이 쓰는 `rustix-openpty` 또는 `nix::pty::openpty`(rustix 본체에는 openpty 없음). 선택은 MK-004에서.
- **MUST** krilld·holder 시작 시 `RLIMIT_NOFILE` 소프트 한도를 하드 한도(또는 정책 상한)까지 올린다. macOS 기본 소프트 한도 256은 세션 수십 개에 부족하다.

## 4. panic·에러·견고성

- **P1 MUST** 비테스트 코드에서 `unwrap`·`expect`·`panic!`·`todo!`·`unimplemented!`·`dbg!`·`mem::forget`·`process::exit` 금지. 불변식이 증명된 곳만 `#[expect(clippy::expect_used, reason = "불변식: …")]`.
- **P2 MUST** `krill-proto`는 `indexing_slicing`·`arithmetic_side_effects`·`as_conversions` deny, `krilld`·`krill-holder`는 `indexing_slicing`·`print_stdout`·`print_stderr` deny(크레이트 루트 속성으로 이미 적용).
- **P3 MUST** 사람이 읽는 텍스트(한글)는 바이트 범위로 자르지 않는다(`string_slice`).
- **P4 MUST**: krilld는 **unwind 유지 + 세션 작업 단위 `catch_unwind` 격리**(ARCHITECTURE의 기존 결정). 대안은 `panic="abort"` 전용 프로필 + holder 재기동. `krill-holder`는 panic 자체를 금지한다.
- **P5 MUST** panic·크래시 경로는 release 바이너리 e2e(패닉 주입, `kill -9`)로 검증한다(`cargo test`는 panic 설정을 무시).
- **P6 MUST** 디스크 세그먼트는 잘린 꼬리를 버리고 복구할 수 있는 형식(SIGKILL·abort는 버퍼를 비우지 않는다).
- **P7 SHOULD** 와이어 길이·오프셋은 `checked_*`/`try_from`. release `overflow-checks = true`.
- **P8 SHOULD** 라이브러리·데몬 오류는 `thiserror` enum(메시지는 소문자, 끝 마침표 없음). `anyhow`는 `krill` CLI `main`과 앱 최상위에서만.
- **P10 SHOULD** `main`은 `ExitCode`/`Result` 반환.

## 5. 타입·구조·이름

- **T1 SHOULD** ID·역할은 뉴타입·enum(`SessionId`, `Seq`, `ClientRole`). 의미 있는 `bool` 인자 금지. 와이어 `kind: u8`는 `#[repr(u8)]` enum + `TryFrom<u8>`.
- **T2 SHOULD** 공개 타입은 `Debug`. fd·비밀을 담은 타입은 수동 구현으로 마스킹.
- **T3 SHOULD** `as_`/`to_`/`into_` 규약, `get_` 접두어 금지, 약어는 한 단어(`Pty`, `Uds`).
- **T4 SHOULD** 바이너리 크레이트는 `main.rs`를 얇게, 로직은 `lib.rs`.
- **T5 MUST** `krill-proto`·`krill-term`은 gpui·tokio·nix·objc2에 의존하지 않는다.
- **MUST** `krill-holder` 줄 수 상한(AGENTS 구조, 결정 12-2, 2026-10-19 개정)은 플랫폼별로 센다: macOS 빌드(공통 코드 + macOS 분기) 1,300줄 이하(2026-10-19 개정: OWE-954 종료 단계 신호 보강 등으로 1,277줄), Linux 전용 `sys/linux.rs` 300줄 이하, Windows 빌드(공통 코드 + Windows 전용 `windows.rs`·`sys/windows.rs`·`link/windows.rs`) 2,000줄 이하(2026-10-18 개정: 1,200은 스파이크 추정치였고 ConPTY·Job/IOCP·핸들 전달·핸드오프 실구현이 약 1,620줄, OWE-832). 다른 OS 전용 파일은 그 플랫폼 몫에만 들어간다.
- **SHOULD** 파일 하나 800줄 넘으면 나눈다. 모듈은 기능 단위(`session/`, `ipc/`, `store/`), `utils.rs` 같은 잡동사니 모듈 금지.
- 주석·문서·커밋·로그 메시지는 한국어 또는 영어 중 하나로 일관되게(코드 식별자는 영어). 사용자에게 보이는 문자열은 ko/en 다국어 대비로 한곳에 모은다(MK-094 전까지는 상수로).

## 6. 테스트 (ponytail 예외)

- **MUST** 와이어 코덱·키프레임·세그먼트 포맷: 왕복(roundtrip) 테스트 + 고정 바이트 fixture(N-1 호환 검사).
- **MUST** 파서·디코더: proptest 또는 퍼징 대상(MK-056). 패닉 0.
- **MUST** 세션 생존·재생: 실제 프로세스를 띄우는 통합 테스트(앱 kill -9, krilld kill -9, 출력 해시 비교). 해시 입력은 테스트 전용 결정적 생성기로 만든다(`/usr/bin/seq 1 1000000`은 마지막 줄이 `1e+06`이라 쓰지 않는다).
- **MUST** gpui 테스트는 `#[gpui::test]`, IPC는 트레이트 뒤 인메모리 구현. 외부 IO를 기다리지 않는다.
- **SHOULD** 버그 수정에는 그 버그를 잡는 테스트 하나.
- 측정형 결과(메모리·지연)는 벤치 스크립트의 측정값으로 남긴다. 측정 안 한 수치는 쓰지 않는다.

## 7. gpui

- **G1 MUST** `Task`·`Subscription`은 필드에 보관하거나 `.detach()`. `let _t = task;`는 기계가 못 잡으니 리뷰에서 본다.
- **G2 SHOULD** spawn 클로저는 `WeakEntity`만 캡처, 상태 변경 후 `cx.notify()`.
- **G3 SHOULD** 단축키는 `actions!` + `KeyBinding` + `key_context` + `on_action`.
- **G5 MUST** `krill-term`은 gpui 없이 일반 `#[test]`로 검증.
- **G6 MUST** 모든 UI/UX 작업(색·타이포그래피·간격·반경·컴포넌트·화면 전환·문구)은 [docs/STYLEGUIDE.md](STYLEGUIDE.md)와 `krill_app::ui` 디자인 시스템 토큰·프리미티브를 따른다. `ui/`·`terminal.rs` 밖에서 생 `gpui::rgb`/`hsla`/`black`/`white`·임의 픽셀 글자 크기(`.text_size(px(..))`) 하드코딩 금지, 미구현 기능 UI 노출 금지.
- **G7 MUST (레이아웃 시프트 금지 — 공간 사전 예약)**: 마우스 호버(`on_mouse_move`), 행/탭 선택 변경, 2단계 무장(`armed`), 진행 상태 전환, 폴링 갱신으로 내부 자식이 바뀌는 팝오버·모달·마스터-디테일·버튼은 1×1 `grid()` 중첩(`Styled::invisible`로 비활성 분기를 숨겨 레이아웃 치수만 유지, `ui::button_with_reserved_label`·`ui::danger_confirm` 패턴) 또는 고정 치수 예약으로 가장 넓은/높은 상태의 공간을 미리 확보한다. 상호작용 전후 외곽 컨테이너와 인접 컨트롤의 `bounds`가 흔들리면 안 되며 `#[gpui::test]`에서 `debug_bounds` 불변식으로 검증한다([docs/STYLEGUIDE.md §7.3](STYLEGUIDE.md)).
- **G8 MUST (Flex 행 텍스트 열 0px 세로 줄바꿈 금지)**: 가로 `flex` 행 안에서 고정 폭(`w(..)` + `flex_none()`) 형제들과 `flex_1().min_w_0()` 텍스트 열을 함께 배치할 때, 고정 폭 형제들의 합이 부모 폭을 모두 잠식해 텍스트 열이 `0px`로 붕괴하고 한 글자씩 세로로 접히는 결함(`계\n정` 등)을 금지한다. 텍스트 열에는 반드시 `.truncate()`와 충분한 잔여 폭(또는 `min_w(..)`)을 보장하거나 고정 폭 열들과 별도 행으로 분리한다([docs/STYLEGUIDE.md §6.3·§7.3](STYLEGUIDE.md)).
- **MUST** Zed의 `terminal`·`terminal_view`(GPL-3.0) 코드는 열어 보고 베끼지 않는다. 필요한 동작은 ARCHITECTURE·이슈 설명·Apache-2.0 gpui API로만 구현한다.

## 8. 공급망·라이선스

- **S1 MUST** `cargo deny --locked check` 통과(advisories·licenses·bans·sources).
- **S2 MUST** 허용 라이선스: Apache-2.0, MIT, BSD-2/3, ISC, Unicode-3.0, Zlib. GPL 계열 금지.
- **S4 MUST** crates.io만. git 의존성은 `allow-git` + `rev` 고정(사람 승인).
- **S6 MUST** vendor한 gpui 파일을 패치하면 변경 사실을 파일에 표시(Apache-2.0 §4(b)).
- **MUST** Orca(MIT) 설계·코드 차용 시 해당 파일 상단에 출처·저작권 표기.

## 9. Apple·데몬 운영

- A1~A3은 macOS launchd·서명 조항이다. Windows 데몬 기동 방식은 S-W4(MK-210/OWE-829)에서 정한다.
- **A1 MUST** launchd가 띄우는 `krill-holder`는 `daemon(3)` 호출·fork 후 부모 종료·자기 setsid 금지. SIGTERM에 빠르게 정리 후 종료.
- **A2 MUST** 생성 plist에 `AssociatedBundleIdentifiers`. plist는 스냅샷 테스트.
- **A3 MUST** 헬퍼는 앱과 같은 팀, **각자 고유 identifier**(`<앱 id>.holder` 등)로 서명. 서명할 때 `--deep` 금지. (같은 identifier로 TCC를 상속한다는 가정은 근거 없음 — MK-007에서 실측.)
- **A5 MUST** 실행 중 번들에 쓰지 않는다. 상태·세그먼트는 `~/Library/Application Support/<bundle id>/`(`.noindex`), 캐시는 `~/Library/Caches/<bundle id>/`. Windows는 상태·세그먼트를 `%LOCALAPPDATA%\Krill\`, 로그를 `%LOCALAPPDATA%\Krill\Logs\`에 두고(`%APPDATA%` 로밍 프로필 금지), 설치 디렉터리에 쓰지 않는다.
- **A6 MUST** UDS 경로는 104바이트 이하 검사(넘으면 오류, 자르지 않음). Windows AF_UNIX 소켓도 `%LOCALAPPDATA%\Krill\` 아래에 두고 같은 방식으로 플랫폼 상한(`SUN_PATH_MAX` cfg, MK-195)을 검사한다.
- **A8 SHOULD** 로그에 명령줄·경로·터미널 내용을 그대로 남기지 않는다(리댁션).

## 10. Git·작업

- 한 커밋 = 한 논리 변경, 마지막 줄 Co-Authored-By.
- 하네스 파일(AGENTS.md, 이 문서, scripts/, .cargo/, lint·deny·fmt·clippy 설정)은 메인테이너 승인 없이 바꾸지 않는다.

## 11. 변경 경계별 재발 방지

새 변경은 관련된 행의 계약과 실패 경로를 확인하고, 이를 잡는 기존 검사를 우선 실행한다. 이미 자동 검사가 있는 계약을 별도 문자열 검색으로 중복 구현하지 않는다. 내부 사고 원문·이슈 덤프·조사 보고서는 공개 규범에 첨부하지 않는다.

| 바꾸는 경계 | 지킬 계약과 관련 검증 |
| --- | --- |
| IPC·직렬화·기능 협상 | ARCHITECTURE §4.3의 번호를 먼저 배정한다. 필드 순서·생략 속성은 JSON 외 바이너리 코덱에도 영향을 준다. 빈 값·기본값·상한의 생산자→소비자 왕복과 기존 고정 바이트·N-1 fixture를 확인한다(`krill-proto`의 `ipc/tests.rs`, `control/tests.rs`). |
| PTY·업그레이드·재접속 | 소유권·PID 보존과 실제 입출력의 진행을 함께 확인한다. 준비 뒤 실패·취소·타임아웃·피어 상실에서도 reader 정지·락·임시 상태가 풀려야 한다. 해당 `survive`·`upgrade`·`rejoin` 통합 검사를 실행한다. 운영 중인 사용자 세션을 장애 주입 대상으로 삼지 않는다. |
| 공유 상태 파일·삭제 | 기존 저장소 API의 잠금·원자 교체를 모든 쓰기 진입점에서 재사용한다. 임시 파일 이름은 동시 쓰기에 충돌하지 않아야 한다. 읽기·상태 판정 실패를 빈 값·clean으로 바꿔 덮어쓰거나 삭제하지 않는다. 손상 원본 보존·동시 갱신·실패 후 재시도를 확인한다. |
| 계정·인증·권한 | 실제 dispatcher에서 권한을 거부해야 한다. 전환·실패·취소가 기존 프로필과 환경을 보존하는지, 미설정과 빈 값·심링크 alias·사용 중 계정을 구분하는지 확인한다. 앱에서 버튼을 숨기는 것만으로 권한 검증을 대신하지 않는다. |
| 비동기 완료·입력 | 작업 시작 시 host·session·account 등 필요한 대상과 세대를 고정하고, 대상 변경 뒤 늦게 도착한 결과가 다른 대상의 초안·선택·파일을 덮어쓰지 않도록 한다. 전송 결과가 불명확한 입력·변경 요청은 자동 재전송하지 않는다. 대상 전환·역순 완료·취소 경로를 검사한다. |
| 원격 host·경로·외부 명령 | 대상 호스트와 경로를 함께 전달한다. 원격 경로를 로컬 OS·파일 존재 여부로 판정하거나, 미지원 원격 작업을 같은 경로의 로컬 작업으로 대체하지 않는다. 외부 명령은 argv 경계를 보존하고, 원격 셸이 필요하면 인용을 포함한 실제 인자 왕복을 검사한다. 공백·따옴표·한글·drive/UNC 경로 중 관련 경계를 포함한다. |
| UI·타이머·Unicode | render·이벤트 경로의 동기 프로세스 대기·소켓 IO를 피하고 백그라운드 작업의 완료·취소를 관리한다(§7). 빈 목록·종료·선택 해제·로딩을 검사하고, 배치 변경은 실제 렌더와 상호작용 전후 bounds를 확인한다. 문자열은 IPC 바이트 한도·UTF-8 경계·화면 셀 폭을 구분하고 NFD 한글·wide cell·IME 편집/만료를 관련 테스트에 포함한다. 실제 화면이 수용 조건이면 실행 바이너리·OS·viewport/zoom와 관찰 결과를 남긴다. |
| 모바일 원본·파생 에셋 | 원본을 바꾸면 해당 `.gz`도 갱신하고 해제 내용의 일치를 확인한다. 기존 `pwa.rs`의 CRC·크기 예산·inventory/hash 검사도 실행한다. 독립 JS 동작은 `check-regressions.py --scope mobile`, 실제 데몬 연결은 `krilld`의 `mobile_pwa` 통합 검사로 구분한다. 고정 출처를 게시하는 릴리스는 기존 게시 스크립트의 배포 hash·연결 검증도 필요하다. |
| 메모리·지연·대량 출력 | §6의 실제 측정을 따른다. 상한 초과 시 검사 프로세스가 비0으로 끝나는 실패 fixture도 확인한다. 병렬 부하·실제 회귀·테스트 준비 실패를 분리하고, 기대값·상한·timeout만 늘려 실패를 숨기지 않는다. 계약이 바뀐 경우에는 이유와 검증을 남긴다. |
| 업데이트·패키징·게시 | 요청 수락과 실제 적용 완료를 구분하고 사후 상태로 성공을 확인한다. 패키징 계약은 `check-regressions.py --scope packaging`으로, 네이티브 서명·DMG는 기존 macOS CI의 `test_pipeline.py`·Homebrew 검사로 확인한다. 릴리스 증거는 최종 SHA·범위·도구 설정·실제 바이너리 hash와 일치해야 한다. 기존 릴리스를 덮어쓰지 않는다. |

**테스트 격리 MUST**: 사용자 설정·계정·launchd·설치 경로에 접근하는 테스트는 쓰기 전에 전용 임시 상태 경로(`KRILL_STATE_DIR`)와 소유한 helper/프로필을 명시한다. 임시 `HOME`만으로 실제 launchd 도메인·설치 앱 helper·상속된 `CLAUDE_CONFIG_DIR`/`CODEX_HOME`/XDG 경로가 격리됐다고 가정하지 않는다. 자식 `Command`에 필요한 환경만 덮어쓰거나 제거하고, 이번 테스트가 만든 프로세스·잡·파일만 종료/정리한다. 모든 테스트의 `HOME`을 전역으로 강제 변경해 실제 경로 계약을 가리지 않는다. 관련 fixture에서는 정상 종료와 실패 뒤 모두 사용자 설정 불변·잔여 자원 없음을 확인한다.

**준비·완료 판정 SHOULD**: 고정 sleep만으로 준비/완료를 판정하지 않는다. 조건과 상한 있는 대기로 바꾸고 실패 시 관찰 상태를 남긴다. 재시도 통과는 FLAKY로 기록하며 원인 조사 없이 회귀를 무시하지 않는다. `check.sh`의 fast/latency 필터와 nextest의 latency 그룹은 `scripts/test_harness_contracts.py`가 동일성을 검사한다. 운영 작업·벤치·검사기는 실패 시 비0 종료를 검증하고, 실제 적용/관찰 이전에 성공을 보고하지 않는다.

**회귀 추가 SHOULD**: 버그를 수정하면 관련 경계를 따라 실패 전 재현·수정 후 통과하는 최소 검사를 추가한다. 자동화하기 어려운 실기 조건은 미검증 범위와 재현 절차를 남긴다. Python/Git 훅·패키징 계약·독립 JS·수집기 검사는 `scripts/check-regressions.py`로 로컬 전체 검사와 hygiene CI에서 함께 실행한다. 새 Python 검사 파일은 해당 scope의 `test_*.py`로 추가하고, 모바일 검사는 데몬/브라우저 필요 여부를 구분해 목록에 등록한다. 새로운 사고는 원인·기존 방어·필요한 회귀 검사를 대조한 뒤 해당 규칙/검사 한 곳을 보완한다.
