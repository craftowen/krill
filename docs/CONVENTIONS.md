# Krill 코드 컨벤션 v1 (2026-10-14)

근거: `docs/research/C1-final.md`(Rust·Cargo·Clippy·cargo-deny·Apple·Tauri·gpui 공식 문서, 설정은 이 맥 rustc 1.98.1·cargo-deny 0.20.2로 실측), `docs/research/C2-final.md`(Zed·Alacritty·WezTerm·zellij·shpool·Helix·uv·ripgrep·tokio·GitButler 등 저장소 비교). 규칙 번호(W1, U2 …)는 C1-final §2와 같다.

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

- **U1 MUST** 워크스페이스 기본 `unsafe_code = "deny"`. FFI 모듈(`krill-holder`·`krilld`의 `sys` 모듈, `krill-app`의 `platform` 모듈)만 모듈 첫 줄에 `#![expect(unsafe_code, reason = "…")]`. `krill-proto`·`krill-term`·`krill`은 `#![forbid(unsafe_code)]`.
- **U2 MUST** 모든 `unsafe` 블록 바로 위 `// SAFETY:` 주석, 블록 하나에 unsafe 연산 하나.
- **U3 MUST** `unsafe fn`·`unsafe trait`에 `# Safety` 문서.
- **U5 MUST** fd는 `OwnedFd`/`BorrowedFd`로만 주고받는다. 정수 fd를 받는 안전 함수 금지.
- **U7 MUST** fork~exec 사이(`pre_exec`)는 async-signal-safe 호출만(할당·`std::env`·락·로깅 금지). 자식 setsid는 `pre_exec` 안에서 libc `setsid`. WezTerm처럼 `pre_exec`에서 fd를 나열하는 코드는 따라 하지 않는다.
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
- **MUST** Zed의 `terminal`·`terminal_view`(GPL-3.0) 코드는 열어 보고 베끼지 않는다. 필요한 동작은 ARCHITECTURE·이슈 설명·Apache-2.0 gpui API로만 구현한다.

## 8. 공급망·라이선스

- **S1 MUST** `cargo deny --locked check` 통과(advisories·licenses·bans·sources).
- **S2 MUST** 허용 라이선스: Apache-2.0, MIT, BSD-2/3, ISC, Unicode-3.0, Zlib. GPL 계열 금지.
- **S4 MUST** crates.io만. git 의존성은 `allow-git` + `rev` 고정(사람 승인).
- **S6 MUST** vendor한 gpui 파일을 패치하면 변경 사실을 파일에 표시(Apache-2.0 §4(b)).
- **MUST** Orca(MIT) 설계·코드 차용 시 해당 파일 상단에 출처·저작권 표기.

## 9. Apple·데몬 운영

- **A1 MUST** launchd가 띄우는 `krill-holder`는 `daemon(3)` 호출·fork 후 부모 종료·자기 setsid 금지. SIGTERM에 빠르게 정리 후 종료.
- **A2 MUST** 생성 plist에 `AssociatedBundleIdentifiers`. plist는 스냅샷 테스트.
- **A3 MUST** 헬퍼는 앱과 같은 팀, **각자 고유 identifier**(`<앱 id>.holder` 등)로 서명. 서명할 때 `--deep` 금지. (같은 identifier로 TCC를 상속한다는 가정은 근거 없음 — MK-007에서 실측.)
- **A5 MUST** 실행 중 번들에 쓰지 않는다. 상태·세그먼트는 `~/Library/Application Support/<bundle id>/`(`.noindex`), 캐시는 `~/Library/Caches/<bundle id>/`.
- **A6 MUST** UDS 경로는 104바이트 이하 검사(넘으면 오류, 자르지 않음).
- **A8 SHOULD** 로그에 명령줄·경로·터미널 내용을 그대로 남기지 않는다(리댁션).

## 10. Git·작업

- 한 커밋 = 한 논리 변경, 마지막 줄 Co-Authored-By.
- 하네스 파일(AGENTS.md, 이 문서, scripts/, lint·deny·fmt·clippy 설정)은 메인테이너 승인 없이 바꾸지 않는다.
