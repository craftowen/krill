**결론(가장 중요한 3가지): ① 1차 보고서의 핵심 설정 권고 두 가지가 틀렸다. 툴체인 규칙(`rust-toolchain.toml channel = "1.99.0"` + `rust-version = "1.99"`)은 이 맥(Homebrew rustc 1.98.1, rustup 없음)에서 툴체인 파일이 오류 없이 무시되고 `rust-version = "1.99"`는 빌드를 깨뜨린다(둘 다 실측). release `panic = "abort"` 권고는 Cargo가 패키지별 panic 설정을 금지하므로 krilld의 세션 단위 `catch_unwind` 격리를 불가능하게 만든다. 현재 커밋된 CONVENTIONS W5·P4(rust-version 1.98, unwind 유지)가 맞다. ② 조사를 시작할 때는 `deny.toml`이 없어 `scripts/check.sh`가 exit 4(6개 크레이트 `unlicensed`)였다. 작업 중 커밋 `1286c66`으로 deny·clippy·rustfmt 설정과 CONVENTIONS가 들어왔고, 지금은 exit 0이다. 이 커밋된 설정은 실제 의존성(nix·zstd·alacritty_terminal 0.26·objc2·postcard 등 111개 패키지)을 `[workspace.dependencies]`로 붙인 사본에서도 check.sh exit 0으로 통과했다(재실측). ③ 커밋된 설정에 아직 없는 것 중 기계로 강제할 수 있는 것이 셋 있다. 3장의 추가분은 사본에서 check.sh exit 0과 위반 시 실패를 모두 실측했다. (a) `Command::output/status`를 clippy.toml 금지 목록에 넣고, krilld git 큐 모듈만 `#![expect]`로 허용해 UI·데몬 스레드 블로킹을 막는다. (b) `RawFd` 타입 금지로 CONVENTIONS U5를 기계 검사한다. (c) `redundant_clone`·`get_unwrap`·`mod_module_files`를 추가한다. 여기에 launchd plist에 `SoftResourceLimits.NumberOfFiles`를 넣어야 한다(이 맥 launchd 잡의 maxfiles 소프트 한도 256 실측).**

- 작성: C2 2차(검증) 워커, 2026-10-14. 이 파일 외에 저장소 파일은 고치지 않았다.
- 1차 보고서: `.ref/drafts/conv/C2-A.md`(560줄)를 끝까지 읽고 주장을 하나씩 원본과 대조했다.
- 방법: 모든 외부 파일은 아래 고정 커밋의 `raw.githubusercontent.com/<repo>/<sha>/<path>`로 내려받아 직접 읽었다. 이 SHA들은 2026-10-14 기준 각 저장소의 HEAD와 같다(`gh api repos/<r>/commits/HEAD`로 확인). 설정 실측은 저장소를 `/tmp/c2v/ws`로 복사한 사본에서 했다. 저장소 원본은 건드리지 않았다.
- 실측 환경: macOS 27.0.1, `rustc 1.98.1 (Homebrew)`, `cargo 1.98.1`, `clippy 0.1.98`, `cargo-deny 0.20.2`, `cargo-nextest 0.9.148`. 2026-10-14 현재 Rust stable은 **1.99.0**(2026-10-01)이다(<https://static.rust-lang.org/dist/channel-rust-stable.toml>의 `date = "2026-10-01"`, `rustc 1.99.0 (b940084d7 2026-09-28)`).

## 0. 트랙 질문과 조사 대상

스스로 정한 질문: (a) 검증된 Rust 앱·데몬은 툴체인·lint·포맷·공급망을 **무엇으로 강제**하는가? (b) 세션 데몬·PTY·FFI의 unsafe·panic·fd 규율은 무엇인가? (c) Tauri 공식 문서 중 gpui+데몬 구조로 옮길 것과, Tauri를 쓰지 않아서 생기는 공백은 무엇인가? (d) 그중 무인 야간 에이전트가 **기계적으로 지킬 수 있는**(자동 검사되는) 규칙은 무엇인가?

| 프로젝트 | 고정 커밋 | 용도 |
|---|---|---|
| zed-industries/zed | `c403bc6af644cd0ab60c90045ac6dc3436218809` | gpui·대형 앱 |
| alacritty/alacritty | `29dc55375da817d53540f4caefbe708703c57b07` | alacritty_terminal 원본·PTY |
| wezterm/wezterm | `e2a98e1b8af156988ad0ac778658ac27b60a55fb` | portable-pty |
| zellij-org/zellij | `519a23715c6b0f65a3ffada64e738ed6ec9380fc` | 세션 서버 |
| shell-pool/shpool | `3c41df9a610428b6c1766d78d36d3fefd5685c3b` | Rust 세션 데몬 |
| helix-editor/helix | `ba40e547426b0f9896c8bdc699a4ab11f2b37dbc` | |
| astral-sh/uv | `a79aac95dc8f1d79e4396fdcb262d22c520cc1f6` | 엄격 컨벤션 |
| astral-sh/ruff | `d47bd6a76f594838ba9f0802ea46bfbf7f7974d6` | 엄격 컨벤션 |
| BurntSushi/ripgrep | `3fce3b5bb0236da2df6d99672afb8a719642eca7` | |
| tokio-rs/tokio | `52ac7816230efabcae6fcc09cbe7ae630a48eddf` | |
| gitbutlerapp/gitbutler | `2d95f3748fe15e9016e332025bc963545102d5b0` | Tauri 상용 앱 |
| spacedriveapp/spacedrive | `4d41a379f612e23ac43a66ffd67df52ca77f57d4` | Tauri 앱 + gpui 시제품 |
| madsmtm/objc2 | `d634d98df5947d410a718c79b505333b6cd08bdc` | AppKit FFI |
| nix-rust/nix | `e35c00891f52468979f92b795de2dc1f3dd58a87` | unix FFI |
| bytecodealliance/rustix | `287214b889865d8e1406a0ee71cc409b6f6191c8` | unix FFI |
| tauri-apps/tauri | `141d1c0d6f3a746f7f34618d00c2676c902dca67` | |
| tauri-apps/tauri-docs | `4b74da68aede87a6acb410812911ffd7cd0d9bdc` | v2.tauri.app 원문 |
| rust-lang/rust | 태그 `1.99.0` | std `pre_exec`·spawn 구현 |

## 1. 이전 조사 정정

### 1.1 틀린 주장

| # | 1차 주장 | 실제 | 근거 |
|---|---|---|---|
| 1 | R1: `rust-toolchain.toml`에 `channel = "1.99.0"`(정확 버전)을 둔다. 초안 `rust-version = "1.99"` | 이 맥에는 rustup이 없고 Homebrew cargo만 있다(`which -a cargo` → `/opt/homebrew/bin/cargo`). `rust-toolchain.toml`(channel 1.99.0)을 두고 `cargo check`를 돌리면 **경고 없이 1.98.1로 빌드**된다. `rust-version = "1.99"`로 바꾸면 `mk-holder@0.1.0 requires rustc 1.99` 오류로 빌드가 실패한다. AGENTS.md도 "rustup 없음 → `rust-toolchain.toml` 대신 `rust-version`으로 MSRV 고정"이라고 적고 있다 | 사본 실측(`/tmp/c2v/ws`), [AGENTS.md "명령" 절](../../AGENTS.md) |
| 2 | R38: release `panic = "abort"`(SHOULD) | AGENTS.md 6항은 "데몬은 panic으로 죽지 않는다: 세션 단위 격리"다. panic을 세션 경계에서 잡으려면(`catch_unwind`) unwind가 필요하다. 프로필 `panic`은 패키지별로 덮어쓸 수 없다(Cargo Book: "Overrides cannot specify the panic, lto, or rpath settings"). 따라서 abort를 켜면 krilld까지 abort가 된다. **기본값 unwind 유지가 맞다** | [Cargo Book 1.98 profiles "Overrides"](https://doc.rust-lang.org/1.98.0/cargo/reference/profiles.html#overrides), [uv Cargo.toml#L477-L482](https://github.com/astral-sh/uv/blob/a79aac95dc8f1d79e4396fdcb262d22c520cc1f6/Cargo.toml#L477-L482)(uv는 단일 CLI라 abort가 맞는 경우) |
| 3 | Zed는 objc2 프레임워크 크레이트를 `=0.3.2` 정확 버전으로 고정한다. "Zed도 objc2로 옮기는 중" | 고정은 일부만 했다. `objc2-core-foundation/-core-graphics/-core-media/-core-video/-foundation/-quartz-core/-screen-capture-kit`은 `=0.3.2`다. 반면 `objc2-app-kit`·`objc2-core-text`·`objc2-security`는 `"0.3.2"`(caret), `objc2-user-notifications`는 `"0.3"`, `objc2`는 `"0.6"`이다. `cocoa = "=0.26.0"`도 아직 의존성에 있다 | [Zed Cargo.toml#L595, L703-L764](https://github.com/zed-industries/zed/blob/c403bc6af644cd0ab60c90045ac6dc3436218809/Cargo.toml#L703-L764) |
| 4 | Zed 릴리스는 `codegen-units = 1` | 맞지만 `[profile.release.package] zed = { codegen-units = 16 }`로 메인 크레이트만 16이다 | [Zed Cargo.toml#L1096-L1102](https://github.com/zed-industries/zed/blob/c403bc6af644cd0ab60c90045ac6dc3436218809/Cargo.toml#L1096-L1102) |
| 5 | Zed는 `cargo shear`·`typos`를 "로컬에서" 돌린다 | CI에서도 돌린다. typos는 SHA로 고정한 액션, shear는 `cargo shear --locked --deny-warnings --check-test-targets`다. `cargo fmt --all -- --check`와 `script/check-todos`도 CI 단계로 있다 | [Zed run_tests.yml#L166-L172, L694-L699](https://github.com/zed-industries/zed/blob/c403bc6af644cd0ab60c90045ac6dc3436218809/.github/workflows/run_tests.yml#L166-L172) |
| 6 | WezTerm 라이선스 allowlist는 "MIT·Apache-2.0·BSD·MPL-2.0·Zlib·ISC 등" | 이 외에 **LGPL-2.1, WTFPL, OpenSSL**, CC0-1.0도 허용한다. `wildcards = "allow"`, `unknown-git = "warn"`이다. 엄격한 모범 사례로 보기 어렵다 | [WezTerm deny.toml](https://github.com/wezterm/wezterm/blob/e2a98e1b8af156988ad0ac778658ac27b60a55fb/deny.toml) |
| 7 | uv `check-lint.yml`이 zizmor를 돌린다 | zizmor는 별도 워크플로 `check-zizmor.yml`에 있다. `check-lint.yml`에는 clippy·deny·shear·shellcheck가 있다 | [uv check-lint.yml#L121-L226](https://github.com/astral-sh/uv/blob/a79aac95dc8f1d79e4396fdcb262d22c520cc1f6/.github/workflows/check-lint.yml#L121-L226), [uv workflows 목록](https://github.com/astral-sh/uv/tree/a79aac95dc8f1d79e4396fdcb262d22c520cc1f6/.github/workflows) |
| 8 | 미확인: "uv `deny.toml` 위치(루트에 없음)" | **`.cargo/deny.toml`**에 있다. `[bans] wildcards = "deny"`, `[bans.build] allow-build-scripts`(빌드 스크립트 허용 목록), `[sources] unknown-registry/unknown-git = "deny"`, 라이선스 13종 allow를 담고 있다 | [uv .cargo/deny.toml](https://github.com/astral-sh/uv/blob/a79aac95dc8f1d79e4396fdcb262d22c520cc1f6/.cargo/deny.toml) |
| 9 | 공통 규칙 3: `[workspace.lints]` 한 곳 관리, tokio 포함 7곳 | tokio의 `[workspace.lints.rust]`는 `unexpected_cfgs`뿐이다. 실제 lint(`unused_must_use`, `unsafe_op_in_unsafe_fn` 등)는 `tokio/src/lib.rs` 크레이트 속성으로 건다. → 6곳(Zed·uv·ruff·GitButler·objc2·Tauri) | [tokio Cargo.toml#L28](https://github.com/tokio-rs/tokio/blob/52ac7816230efabcae6fcc09cbe7ae630a48eddf/Cargo.toml#L28), [tokio lib.rs#L1-L13](https://github.com/tokio-rs/tokio/blob/52ac7816230efabcae6fcc09cbe7ae630a48eddf/tokio/src/lib.rs#L1-L13) |
| 10 | GitButler PR #15807에서 "electron-builder를 올렸다" | PR은 **머지되지 않고 닫혔다**(`merged=false`). 대상도 Tauri가 아니라 Electron "Lite" 빌드다. 남는 사실은 PR 본문의 관찰(러너 이미지가 macOS 26.6으로 바뀌자 `security set-key-partition-list`가 실패했고 저장소 변경은 없었다)뿐이다 | `gh api repos/gitbutlerapp/gitbutler/pulls/15807`, [PR #15807](https://github.com/gitbutlerapp/gitbutler/pull/15807) |
| 11 | Spacedrive 주석 "zed pins 1.97.1"을 사실로 인용 | Spacedrive 주석이 낡았다. Zed는 `c403bc6`에서 `1.98.1`을 고정한다. gpui 최소 1.95(`std::hint::cold_path`)는 Spacedrive 주석에만 근거한다 | [Zed rust-toolchain.toml](https://github.com/zed-industries/zed/blob/c403bc6af644cd0ab60c90045ac6dc3436218809/rust-toolchain.toml), [Spacedrive rust-toolchain.toml](https://github.com/spacedriveapp/spacedrive/blob/4d41a379f612e23ac43a66ffd67df52ca77f57d4/rust-toolchain.toml) |
| 12 | Conductor 문서의 `src-tauri/icons/icon.png`는 "판별할 수 없다" | 사용자 저장소의 사이드바 아이콘을 찾는 **후보 경로 목록**의 한 줄이다(Conductor 자체 이야기가 아님). Conductor 스택은 창업자 트윗 재게시본에 "Conductor is built in Tauri. It has a Rust backend and uses the native Mac renderer."라고 나온다(2차 출처, X 원문 미확인) | <https://www.conductor.build/llms-full.txt>(L1110-L1125), [Thread Reader 재게시](https://threadreaderapp.com/thread/1945870105109246401.html) |
| 13 | Tauri updater 근거 줄 `L62-L96, L153-L154, L713~` | 실제 위치: "This cannot be disabled" L84, 키 분실 시 업데이트 불가 L89, `.env` 불가 L106, TLS 강제·`dangerousInsecureTransportProtocol` L205-L206, `version_comparator`(롤백) L309 | [tauri-docs plugin/updater.mdx](https://github.com/tauri-apps/tauri-docs/blob/4b74da68aede87a6acb410812911ffd7cd0d9bdc/src/content/docs/plugin/updater.mdx#L84-L106) |
| 14 | R11 "nix와 rustix 중 하나만 고른다(nix 하나면 충분)" | `alacritty_terminal 0.26.0`이 이미 `rustix 1.1.5`·`rustix-openpty 0.2.0`·`signal-hook`·`polling`을 끌고 온다(`cargo tree -p krilld` 실측). 의존성 트리에 rustix가 들어오는 것은 피할 수 없다. 규칙은 "**직접** 의존하는 unix 래퍼는 nix 하나"로 고친다 | 사본 실측, [alacritty_terminal/Cargo.toml#L31-L34](https://github.com/alacritty/alacritty/blob/29dc55375da817d53540f4caefbe708703c57b07/alacritty_terminal/Cargo.toml#L31-L34) |
| 15 | R5: "로컬은 warn, CI에서 `CARGO_BUILD_WARNINGS=deny`" | Minikrill에는 아직 CI가 없고(원격 금지, AGENTS 9항), 완료 게이트는 로컬 `scripts/check.sh`이며 이미 `-- -D warnings`를 쓴다. `CARGO_BUILD_WARNINGS=deny`는 cargo 1.98.1에서 동작한다(`warnings are denied by build.warnings configuration`, exit 101, 실측). 다만 check.sh가 이미 같은 효과를 내므로 바꿀 필요는 없다 | 사본 실측, [Cargo Book 1.98 config#buildwarnings](https://doc.rust-lang.org/1.98.0/cargo/reference/config.html#buildwarnings)("MSRV: Respected as of 1.97") |
| 16 | 초안 `disallowed-types = [{ path = "cocoa::base::id" }]` | cocoa는 Minikrill 의존성이 아니다. 존재하지 않는 경로로 막을 이유가 없어 초안에서 뺐다(대신 cargo-deny `[bans] deny`로 cocoa 크레이트 자체를 막는 것은 MAY) | — |
| 17 | 위험 4 "std는 pre_exec이 있으면 posix_spawn 대신 fork를 쓴다(원문 미확인)" | 확인했다. `pre_exec` 클로저가 하나라도 있으면 posix_spawn을 쓰지 않는다(`|| !self.get_closures().is_empty()` → `return Ok(None)`). macOS에서는 `process_group`/`setsid`를 쓸 때도 fork 경로로 간다(`POSIX_SPAWN_SETSID`는 linux-gnu에서만 사용) | [rust 1.99.0 sys/process/unix/unix.rs#L466-L474, L756-L762](https://github.com/rust-lang/rust/blob/1.99.0/library/std/src/sys/process/unix/unix.rs#L466-L474) |
| 18 | 미확인 "`POSIX_SPAWN_CLOEXEC_DEFAULT`·`POSIX_SPAWN_SETSID` Apple 원문·libc 노출" | macOS SDK `sys/spawn.h` L61-L62에 `POSIX_SPAWN_SETSID 0x0400`, `POSIX_SPAWN_CLOEXEC_DEFAULT 0x4000`이 있다. `man posix_spawnattr_setflags`는 CLOEXEC_DEFAULT를 "file_actions로 명시한 fd만 자식에 남는다"고 설명한다. **libc 0.2.190은 apple 타깃에 `POSIX_SPAWN_CLOEXEC_DEFAULT`만 노출하고 `POSIX_SPAWN_SETSID`는 노출하지 않는다** | 이 맥 `xcrun --show-sdk-path` 헤더, `man posix_spawnattr_setflags`, `~/.cargo/registry/.../libc-0.2.190/src/unix/bsd/apple/mod.rs:4060` |
| 19 | 미확인 "objc2 `define_class!`가 사용자 크레이트에서 `unsafe_code`를 유발하는지" | 유발하지 않는다. `unsafe_code = "deny"` 상태에서 `#[unsafe(super(NSObject))]`·`#[unsafe(method(...))]`를 쓴 `define_class!`는 통과했다. 오류는 직접 쓴 `unsafe { msg_send![...] }` 블록에서만 났다(`usage of an unsafe block` + `unsafe block missing a safety comment`) | 사본 실측(objc2 0.6.5, objc2-foundation 0.3.2) |
| 20 | 공통 규칙 14 "Actions SHA 고정 3곳(Zed·uv·GitButler)" | 이 조사에서 확인한 곳은 Zed(typos)와 uv(rust-cache) 2곳이다. GitButler는 열어 보지 않았다. **Tauri 본 저장소는 자기 문서와 달리 태그로 고정한다**(`actions/checkout@v7`, `rustsec/audit-check@v2`) | [Zed run_tests.yml#L172](https://github.com/zed-industries/zed/blob/c403bc6af644cd0ab60c90045ac6dc3436218809/.github/workflows/run_tests.yml#L172), [uv check-lint.yml#L121](https://github.com/astral-sh/uv/blob/a79aac95dc8f1d79e4396fdcb262d22c520cc1f6/.github/workflows/check-lint.yml#L121), [Tauri audit.yml#L41-L52](https://github.com/tauri-apps/tauri/blob/141d1c0d6f3a746f7f34618d00c2676c902dca67/.github/workflows/audit.yml#L41-L52) |
| 21 | 사소한 줄 번호: Zed clippy.toml `L7-L18`, Zed lints `L1110-L1151` 중 clippy 시작 | disallowed-methods는 L8-L19, `[workspace.lints.clippy]`는 L1113부터다(내용은 일치) | [Zed clippy.toml](https://github.com/zed-industries/zed/blob/c403bc6af644cd0ab60c90045ac6dc3436218809/clippy.toml#L8-L19) |

### 1.2 보강(1차가 놓친 사실)

| 사실 | 근거 |
|---|---|
| ruff는 워크스페이스에서 `disallowed_methods = "allow"`로 끄고, 해당 크레이트 루트에서만 `#![warn(clippy::disallowed_methods, reason = "...")]`로 다시 켠다. clippy.toml의 금지 목록은 워크스페이스 전체에 하나뿐이다. 이 패턴으로 "mk-app에서만 `Command::output` 금지"를 구현할 수 있다(사본에서 mk-app만 걸리고 krilld는 통과함을 실측). 다만 커밋된 clippy.toml에는 `std::env::set_var` 금지처럼 **전역으로 막아야 하는 항목**이 있어 워크스페이스에서 끌 수 없다. 그래서 3장은 반대로 "전역 금지 + 허용 모듈만 `#![expect]`"로 구성했다 | [ruff Cargo.toml#L274-L277](https://github.com/astral-sh/ruff/blob/d47bd6a76f594838ba9f0802ea46bfbf7f7974d6/Cargo.toml#L274-L277), [ruff ty_ide/src/lib.rs#L1-L4](https://github.com/astral-sh/ruff/blob/d47bd6a76f594838ba9f0802ea46bfbf7f7974d6/crates/ty_ide/src/lib.rs#L1-L4) |
| **이 맥에서 launchd의 maxfiles 소프트 한도는 256이다**(`launchctl limit maxfiles` → `256 unlimited`, `kern.maxfilesperproc = 122880`). `launchctl bootstrap`으로 띄울 holder·krilld는 이 값을 물려받는다. launchd.plist에 `SoftResourceLimits`/`HardResourceLimits`의 `NumberOfFiles` 키가 있다 | 실측, `man launchd.plist`("NumberOfFiles <integer> The maximum number of open files for this process") |
| GitButler PR #14687(열림)은 `rlimit::increase_nofile_limit`으로 소프트 한도를 올린다. 이 함수는 `rlim_max = RLIM_INFINITY`일 때 실제 상한이 `kern.maxfilesperproc`인 macOS 특성을 처리한다. 실패해도 치명 오류로 보지 않는다 | [PR #14687 본문](https://github.com/gitbutlerapp/gitbutler/pull/14687) |
| postcard 1.x를 기본 feature로 쓰면 비 Apple 타깃용 `atomic-polyfill`이 lockfile에 들어온다. deny `[graph] targets`를 비워 두면(전 타깃) **RUSTSEC-2023-0089(unmaintained)로 실패**한다. 커밋된 deny.toml(`targets = aarch64/x86_64-apple-darwin`)에서는 그래프에 들어오지 않아 통과한다. `[workspace.dependencies]`에서 `default-features = false, features = ["use-std"]`로 두면 lockfile에서도 사라진다 | 사본 실측(`cargo deny check advisories`, `cargo tree -i atomic-polyfill`) |
| alacritty_terminal은 `log` 크레이트로 로그를 남긴다. krilld가 tracing을 쓰면 `tracing-log` 브리지(`tracing-subscriber`의 log 호환)가 있어야 alacritty 경고가 보인다 | `cargo tree -p krilld` 실측(alacritty_terminal → log 0.4) |
| Alacritty는 LLM 생성 기여를 전면 금지한다("must not include content generated by large language models"). ripgrep은 "자율 에이전트 기여 금지"다. **두 저장소의 코드는 참고만 하고, Minikrill 에이전트가 이 저장소들에 기여하지 않는다** | [Alacritty CONTRIBUTING.md#L160-L164](https://github.com/alacritty/alacritty/blob/29dc55375da817d53540f4caefbe708703c57b07/CONTRIBUTING.md#L160-L164), [ripgrep AI_POLICY.md#L20-L21](https://github.com/BurntSushi/ripgrep/blob/3fce3b5bb0236da2df6d99672afb8a719642eca7/AI_POLICY.md#L20-L21) |
| Rust std `pre_exec` 문서는 `Error::new`·`Error::other`도 할당 함수로 명시하고 "사소하지 않은 오류는 `panic!`을 선호하라"고 적는다(`Command`가 hook 전에 `always_abort`를 호출하기 때문). 단 panic 메시지 포맷 인자도 안전해야 한다. `io::Error::last_os_error()`는 할당하지 않는다 | [rust 1.99.0 os/unix/process.rs#L60-L108](https://github.com/rust-lang/rust/blob/1.99.0/library/std/src/os/unix/process.rs#L60-L108) |
| uv AGENTS.md에는 무인 에이전트에 바로 쓸 수 있는 규칙이 있다. "clippy 경고·테스트 실패를 기존 문제라고 가정하지 말 것(main에 경고가 있는 일은 매우 드물다)", "릴리스 프로필로 빌드하지 말 것(요청 시 제외)" | [uv AGENTS.md#L21, L31-L32](https://github.com/astral-sh/uv/blob/a79aac95dc8f1d79e4396fdcb262d22c520cc1f6/AGENTS.md#L21-L32) |
| Zed `.rules`에는 "소스를 수정하면 README.md 첫 줄에 리뷰 확인 문구를 넣어라"라는 HARD RULE이 있다(Zed 기여자용). Zed의 에이전트 규칙 파일을 통째로 복사하면 이런 저장소 고유 규칙이 섞인다. 필요한 규칙만 골라 옮긴다 | [Zed .rules#L16](https://github.com/zed-industries/zed/blob/c403bc6af644cd0ab60c90045ac6dc3436218809/.rules#L16) |
| gpui-pre `0.3.9`와 gpui-component `0.7.3`은 둘 다 **2026-10-12**에 crates.io에 게시됐다(2일 전). WezTerm식 14일 쿨다운을 규칙으로 두면 ARCHITECTURE §5의 버전 핀과 바로 충돌한다 | crates.io API(`/api/v1/crates/gpui-pre`, `gpui-component`) |

### 1.3 원본과 일치해 유지한 주장(요약)

Zed `.rules`(unwrap 회피·`let _ =` 금지·`mod.rs` 금지·gpui `Task` drop 시 취소·`WeakEntity`)는 일치한다. clippy.toml `Command::spawn/output/status` 금지와 사유, `cocoa::foundation::NSString::alloc` 금지, `style = allow`와 그 이유 주석, `crashes` 크레이트(crash-handler + minidumper, C 런타임 abort 메시지 회수), terminal 크레이트 `GPL-3.0-or-later`, `.cargo/config.toml`의 `--cfg tokio_unstable`도 원본과 같다. 이 밖에 원본과 일치한 항목은 다음과 같다.
- Alacritty: `pre_exec` 순서, 시그널 6종 `SIG_DFL`, SAFETY 주석 없음, nightly 전용 rustfmt 옵션, MSRV 1.85.0.
- WezTerm: `sigprocmask` 초기화, Big Sur fd 누수 주석과 `/dev/fd` 나열, 14일 cooldown.
- zellij: ERROR_HANDLING의 `fatal`/`non_fatal`, dev 프로필에서 vte·alacritty_terminal `opt-level = 3`.
- shpool: `getpeereid`·`LOCAL_PEERPID` 검사, `// Safety:` 주석, `VERSION = "0.4.3"` 헤더, test_hooks, HACKING(비공개 인터페이스·Conventional Commits·macOS ignore·`SHPOOL_LEAVE_TEST_LOGS`).
- Helix: MSRV 세 곳 동기화, `ulimit -n 10240` 안내, `RUSTDOCFLAGS=-D warnings`.
- uv: lint 목록(pedantic `priority = -2`, allow 16개), `panic = "abort"`, `debug = "line-tables-only"`(20% 단축 주석), STYLE(stderr·`NO_COLOR`·`hint:`).
- ruff: Salsa 용량 축소 규칙, memory_report 워크플로.
- ripgrep: BrokenPipe 정상 종료, musl jemalloc 주석.
- tokio: `RUSTFLAGS: -Dwarnings`, deny.toml, `module: describe your change`.
- GitButler: lint, AGENTS(모호한 이름·시간 주입·`Code` 패턴), 오류 텔레메트리 60분·200건·키당 5건, 명령 레지스트리 어긋남, CSP·updater pubkey.
- Spacedrive: gpui rev 고정 "ritual" 주석, `default-members` 제외, `allow-unwrap-in-tests`, PROJECT_STATUS L202.
- objc2: lint, layered safety, `MainThreadMarker`, run loop.
- nix: `OpenptyResult`(OwnedFd), `forkpty` Safety, CONVENTIONS(libc 우선).
- rustix: 본체에 `openpty` 없음. `pty` 모듈에는 `openpt`/`grantpt`/`unlockpt`/`ptsname`만 있다([rustix src/pty.rs#L91-L196](https://github.com/bytecodealliance/rustix/blob/287214b889865d8e1406a0ee71cc409b6f6191c8/src/pty.rs#L91-L196)).
- Tauri 문서: 신뢰 경계, 메시지 전달, Core 최소 권한, 서명 필수, 공증, ad-hoc 서명, git 해시 고정, 액션 고정, 개발 서버 노출.

## 2. 확정 규칙

강도: **MUST** = check.sh(또는 이후 CI) 실패 또는 병합 거부. **SHOULD** = 지키되 사유를 적으면 예외. **MAY** = 선택. "강제 수단"에 **[자동]**이 붙은 규칙은 기계 검사라 무인 야간 에이전트가 어기면 바로 실패로 드러난다. **[리뷰]**만 붙은 규칙은 지휘자의 독립 검증 체크리스트에 넣어야 한다.

### 2.1 툴체인·포맷·lint

| # | 규칙 | 강제 수단 | 근거 URL | 강도 |
|---|---|---|---|---|
| C1 | MSRV는 `[workspace.package] rust-version = "1.98"` 한 곳에서 관리하고 각 크레이트는 `rust-version.workspace = true`로 받는다. `rust-toolchain.toml`은 두지 않는다(이 맥에서 무시됨). 툴체인을 올릴 때는 `brew upgrade rust` 뒤 `rust-version`을 함께 올리는 커밋 하나로 한다 | [자동] cargo가 `rust-version`보다 낮은 rustc를 거부한다(실측) | 실측, [zellij rust-toolchain.toml 주석](https://github.com/zellij-org/zellij/blob/519a23715c6b0f65a3ffada64e738ed6ec9380fc/rust-toolchain.toml), [Helix CONTRIBUTING#L62-L72](https://github.com/helix-editor/helix/blob/ba40e547426b0f9896c8bdc699a4ab11f2b37dbc/docs/CONTRIBUTING.md#L62-L72) | MUST |
| C2 | edition 2024를 워크스페이스에서 상속한다(현행 유지) | [자동] Cargo | [Alacritty Cargo.toml#L11](https://github.com/alacritty/alacritty/blob/29dc55375da817d53540f4caefbe708703c57b07/Cargo.toml#L11), [uv Cargo.toml#L11](https://github.com/astral-sh/uv/blob/a79aac95dc8f1d79e4396fdcb262d22c520cc1f6/Cargo.toml#L11) | MUST |
| C3 | `rustfmt.toml`은 `style_edition = "2024"`만 둔다(커밋된 형태. `cargo fmt`는 edition을 Cargo.toml에서 읽는다. Zed·ruff처럼 `edition` 줄을 더해도 결과는 같다). nightly 전용 옵션은 금지한다. stable rustfmt는 nightly 옵션을 **경고만 내고 무시**하므로(실측: `can't set imports_granularity = Module`) 설정이 있어도 효과가 없다 | [자동] `cargo fmt --all --check`(check.sh) | [Zed rustfmt.toml](https://github.com/zed-industries/zed/blob/c403bc6af644cd0ab60c90045ac6dc3436218809/rustfmt.toml), [ruff rustfmt.toml](https://github.com/astral-sh/ruff/blob/d47bd6a76f594838ba9f0802ea46bfbf7f7974d6/rustfmt.toml), 반례 [shpool rustfmt.toml](https://github.com/shell-pool/shpool/blob/3c41df9a610428b6c1766d78d36d3fefd5685c3b/rustfmt.toml) | MUST |
| C4 | lint는 루트 `[workspace.lints]`에서만 관리하고 크레이트는 `[lints] workspace = true`(현행). 크레이트·모듈 단위 예외는 소스 속성 `#![expect(lint, reason = "...")]`로 한다(Cargo는 `workspace = true`인 크레이트에서 `[lints]` 추가를 허용하지 않는다) | [자동] clippy | [uv Cargo.toml#L430-L465](https://github.com/astral-sh/uv/blob/a79aac95dc8f1d79e4396fdcb262d22c520cc1f6/Cargo.toml#L430-L465), [ruff ty_ide lib.rs#L1-L4](https://github.com/astral-sh/ruff/blob/d47bd6a76f594838ba9f0802ea46bfbf7f7974d6/crates/ty_ide/src/lib.rs#L1-L4) | MUST |
| C5 | `#[allow]` 대신 `#[expect(..., reason = "...")]`를 쓴다. 이유 없는 억제는 금지한다. 쓸모없어진 expect는 `unfulfilled_lint_expectations`로 실패하므로 낡은 예외가 남지 않는다. dead code 경고는 lint를 끄지 말고 코드를 지운다 | [자동] clippy `allow_attributes`(warn→check.sh에서 error), `allow_attributes_without_reason`(deny) | [uv AGENTS.md#L27](https://github.com/astral-sh/uv/blob/a79aac95dc8f1d79e4396fdcb262d22c520cc1f6/AGENTS.md#L27), [ruff AGENTS.md#L224](https://github.com/astral-sh/ruff/blob/d47bd6a76f594838ba9f0802ea46bfbf7f7974d6/AGENTS.md#L224), 실측 | MUST |
| C6 | clippy `all = deny`(현행) + `pedantic = warn`과 소음 lint allow 목록(uv 목록의 부분집합). check.sh가 `-D warnings`라 warn도 실패다 | [자동] clippy | [uv Cargo.toml#L434-L451](https://github.com/astral-sh/uv/blob/a79aac95dc8f1d79e4396fdcb262d22c520cc1f6/Cargo.toml#L434-L451), [GitButler Cargo.toml#L323-L334](https://github.com/gitbutlerapp/gitbutler/blob/2d95f3748fe15e9016e332025bc963545102d5b0/Cargo.toml#L323-L334) | SHOULD(5장 위험 3) |
| C7 | restriction lint 선택 적용: `dbg_macro`·`todo`·`unimplemented`(deny), `print_stdout`·`print_stderr`·`exit`·`mem_forget`·`get_unwrap`·`redundant_clone`(warn), `let_underscore_must_use`(deny). `mk` 크레이트만 `#![expect(clippy::print_stdout, reason)]` | [자동] clippy | [Zed Cargo.toml#L1113-L1120](https://github.com/zed-industries/zed/blob/c403bc6af644cd0ab60c90045ac6dc3436218809/Cargo.toml#L1113-L1120), [uv Cargo.toml#L452-L465](https://github.com/astral-sh/uv/blob/a79aac95dc8f1d79e4396fdcb262d22c520cc1f6/Cargo.toml#L452-L465), [Zed .rules#L8-L12](https://github.com/zed-industries/zed/blob/c403bc6af644cd0ab60c90045ac6dc3436218809/.rules#L8-L12) | MUST |
| C8 | 파일 구조: `mod.rs` 금지(`foo.rs` + `foo/`), 축약 변수명 금지, `Manager/Service/Helper/Util` 이름 금지, 한 번 쓰는 trait 금지 | [자동] `mod_module_files`(warn) + [리뷰] 나머지 | [Zed .rules#L5, L14, L18](https://github.com/zed-industries/zed/blob/c403bc6af644cd0ab60c90045ac6dc3436218809/.rules#L5-L18), [GitButler crates/AGENTS.md#L61-L66](https://github.com/gitbutlerapp/gitbutler/blob/2d95f3748fe15e9016e332025bc963545102d5b0/crates/AGENTS.md#L61-L66) | SHOULD |
| C9 | 주석은 "왜"만 쓴다. 과거형("기존 동작 유지")·대화·프롬프트 이력을 코드·커밋에 남기지 않는다. 비자명한 결정(버전 핀·vendor 패치·할당기)에는 근거 링크를 단다 | [리뷰] | [Zed .rules#L4](https://github.com/zed-industries/zed/blob/c403bc6af644cd0ab60c90045ac6dc3436218809/.rules#L4), [uv AGENTS.md#L41-L45](https://github.com/astral-sh/uv/blob/a79aac95dc8f1d79e4396fdcb262d22c520cc1f6/AGENTS.md#L41-L45), [ripgrep main.rs#L20-L41](https://github.com/BurntSushi/ripgrep/blob/3fce3b5bb0236da2df6d99672afb8a719642eca7/crates/core/main.rs#L20-L41) | SHOULD |
| C10 | `todo!`·`FIXME`를 병합하지 않는다. 남길 일은 `TODO(OWE-123)`처럼 이슈 번호를 붙인다 | [자동] clippy `todo` + (선택) check.sh에 `git grep` 단계 | [Zed script/check-todos](https://github.com/zed-industries/zed/blob/c403bc6af644cd0ab60c90045ac6dc3436218809/script/check-todos) | SHOULD |
| C11 | 에이전트는 clippy 경고·테스트 실패를 "원래 있던 것"으로 넘기지 않는다. main은 항상 check.sh 0이다 | [자동] check.sh + 지휘자 재실행(NIGHT-SHIFT) | [uv AGENTS.md#L31-L32](https://github.com/astral-sh/uv/blob/a79aac95dc8f1d79e4396fdcb262d22c520cc1f6/AGENTS.md#L31-L32) | MUST |

### 2.2 unsafe·FFI·프로세스

| # | 규칙 | 강제 수단 | 근거 URL | 강도 |
|---|---|---|---|---|
| U1 | 워크스페이스 `unsafe_code = "deny"`로 둔다. 허용은 FFI 모듈 파일 첫 줄의 `#![expect(unsafe_code, reason = "...")]`로만 한다(mk-holder·krilld FFI 모듈, mk-app objc2 모듈 — AGENTS 5항). objc2 `define_class!`는 이 lint를 유발하지 않는다(실측) | [자동] rustc lint + [리뷰] expect 위치 | [uv Cargo.toml#L430-L431](https://github.com/astral-sh/uv/blob/a79aac95dc8f1d79e4396fdcb262d22c520cc1f6/Cargo.toml#L430-L431)(uv·ruff는 warn), 실측 | MUST |
| U2 | 모든 `unsafe` 블록 바로 위에 `// SAFETY:`를 단다. `unsafe fn`에는 `# Safety` 문서를 단다. `unsafe_op_in_unsafe_fn = "deny"`(edition 2024 기본은 warn), clippy `undocumented_unsafe_blocks = "deny"`, `multiple_unsafe_ops_per_block = "warn"`, `missing_safety_doc`(clippy::all에 포함) | [자동] rustc + clippy(SAFETY 없는 블록에서 실패, 있는 블록은 통과 — 실측) | [Edition Guide 2024 unsafe_op_in_unsafe_fn](https://doc.rust-lang.org/1.98.0/edition-guide/rust-2024/unsafe-op-in-unsafe-fn.html), [shpool peer.rs#L53-L76](https://github.com/shell-pool/shpool/blob/3c41df9a610428b6c1766d78d36d3fefd5685c3b/libshpool/src/daemon/peer.rs#L53-L76), [tokio lib.rs#L13](https://github.com/tokio-rs/tokio/blob/52ac7816230efabcae6fcc09cbe7ae630a48eddf/tokio/src/lib.rs#L13), [objc2 Cargo.toml#L31-L35](https://github.com/madsmtm/objc2/blob/d634d98df5947d410a718c79b505333b6cd08bdc/Cargo.toml#L31-L35) | MUST |
| U3 | FFI 선언은 `libc`·`nix`·objc2-*에서 가져온다. 직접 `extern "C"`는 libc에 없을 때만 쓰고 이유를 주석으로 남긴다. 직접 의존하는 unix 래퍼는 nix 하나다(rustix는 alacritty_terminal을 통해 간접으로 들어온다) | [리뷰] + `cargo tree -e normal --depth 1` | [nix CONVENTIONS.md#L18-L30](https://github.com/nix-rust/nix/blob/e35c00891f52468979f92b795de2dc1f3dd58a87/CONVENTIONS.md#L18-L30), 실측 | SHOULD |
| U4 | fd는 `OwnedFd`/`BorrowedFd`로만 주고받는다. SCM_RIGHTS로 받은 fd는 즉시 `OwnedFd`로 감싼다. 타입 이름 `RawFd`는 FFI 모듈에서만 쓴다 | [자동] clippy `disallowed_types`(RawFd 사용 시 실패 — 실측), FFI 모듈은 `#![expect(clippy::disallowed_types, reason)]` | [nix pty.rs#L23-L27](https://github.com/nix-rust/nix/blob/e35c00891f52468979f92b795de2dc1f3dd58a87/src/pty.rs#L23-L27) | SHOULD |
| U5 | fork 후 exec 전 코드(`pre_exec` 클로저, `forkpty` 자식 분기)에서는 async-signal-safe 호출만 한다. 할당·`std::env`·mutex·`format!`·`io::Error::new/other`를 쓰지 않는다. 오류는 `io::Error::last_os_error()`로 반환한다. 이 코드는 mk-holder의 함수 하나에 모은다 | [리뷰] + 통합 테스트 | [rust 1.99.0 os/unix/process.rs#L60-L108](https://github.com/rust-lang/rust/blob/1.99.0/library/std/src/os/unix/process.rs#L60-L108), [nix pty.rs#L336-L351](https://github.com/nix-rust/nix/blob/e35c00891f52468979f92b795de2dc1f3dd58a87/src/pty.rs#L336-L351) | MUST |
| U6 | PTY 자식 준비 순서: 시그널 6종(CHLD·HUP·INT·QUIT·TERM·ALRM) `SIG_DFL` → `sigprocmask(SIG_SETMASK, 빈 집합)` → `setsid` → `TIOCSCTTY` → 필요한 fd만 `dup2`. **자식에서 `/dev/fd`를 `read_dir`로 나열하지 않는다**(WezTerm 방식은 할당이 있다). 부모가 여는 모든 fd를 CLOEXEC로 연다 | [리뷰] + 통합 테스트(자식의 열린 fd가 0·1·2뿐인지 `lsof -p`로 검사) | [WezTerm pty/src/unix.rs#L134-L176, L238-L280](https://github.com/wezterm/wezterm/blob/e2a98e1b8af156988ad0ac778658ac27b60a55fb/pty/src/unix.rs#L238-L280), [Alacritty tty/unix.rs#L248-L276](https://github.com/alacritty/alacritty/blob/29dc55375da817d53540f4caefbe708703c57b07/alacritty_terminal/src/tty/unix.rs#L248-L276) | MUST |
| U7 | macOS에서 fd 누수를 원천 차단하려면 `posix_spawn` + `POSIX_SPAWN_CLOEXEC_DEFAULT`(libc 0.2.190 노출)를 검토한다. `POSIX_SPAWN_SETSID`는 libc에 없어 상수를 직접 선언해야 하고, TIOCSCTTY를 대체할 수 있는지도 미확인이다 → 스파이크 대상 | 스파이크 | macOS SDK `sys/spawn.h` L61-L62, `man posix_spawnattr_setflags`, [rust unix.rs#L756-L762](https://github.com/rust-lang/rust/blob/1.99.0/library/std/src/sys/process/unix/unix.rs#L756-L762) | MAY |
| U8 | holder·krilld·`mk`는 시작할 때 `RLIMIT_NOFILE` 소프트 한도를 하드 한도(실제 상한 `kern.maxfilesperproc`) 쪽으로 올린다. 실패하면 경고만 남긴다. 이와 함께 launchd plist에 `SoftResourceLimits.NumberOfFiles`를 둔다 | [자동] 통합 테스트(launchd 기동 후 `getrlimit` 값 검사, 세션 256개 생성) | 실측(`launchctl limit maxfiles` = 256), `man launchd.plist`, [GitButler PR #14687](https://github.com/gitbutlerapp/gitbutler/pull/14687), [Helix CONTRIBUTING#L58-L60](https://github.com/helix-editor/helix/blob/ba40e547426b0f9896c8bdc699a4ab11f2b37dbc/docs/CONTRIBUTING.md#L58-L60) | MUST |
| U9 | 피어 검사는 `getpeereid`(UID 불일치 시 거부) + `LOCAL_PEERPID`를 한 함수로 처리하고, 실패하면 연결을 거부한다(fail-closed). 테스트는 "자기 프로세스 수락", "검사 전에 종료한 피어 거부" | [자동] 단위 테스트 | [shpool peer.rs#L48-L95, L124-L218](https://github.com/shell-pool/shpool/blob/3c41df9a610428b6c1766d78d36d3fefd5685c3b/libshpool/src/daemon/peer.rs#L48-L95) | MUST |
| U10 | 프로세스 종료는 한 함수로 처리한다: 그룹 SIGTERM → 유예 → 그룹 SIGKILL → setsid로 빠져나간 세션 후손 스윕. Conductor는 마지막 단계를 하지 않는다고 문서에 명시한다 | [자동] 통합 테스트(`setsid sleep` 손자가 남지 않음) | [Conductor "Process handling"](https://www.conductor.build/llms-full.txt), ARCHITECTURE §4 | MUST |
| U11 | AppKit 객체는 `MainThreadMarker`로만 만들고 다룬다. `msg_send!`를 직접 부르기보다 objc2-* 프레임워크 크레이트를 먼저 쓴다. 반환값은 `Retained<T>`로 받는다. `cocoa` 크레이트는 금지한다 | [자동] 타입 시스템(MainThreadOnly) + cargo-deny `[bans] deny`(MAY) + [리뷰] | [objc2 frameworks_soundness.md#L36-L75](https://github.com/madsmtm/objc2/blob/d634d98df5947d410a718c79b505333b6cd08bdc/docs/frameworks_soundness.md), [objc2 layered_safety.md](https://github.com/madsmtm/objc2/blob/d634d98df5947d410a718c79b505333b6cd08bdc/docs/layered_safety.md) | MUST |
| U12 | 데몬에서 Apple 프레임워크(IOKit 전원 단언 등)를 쓸 때는 런루프가 필요한지 확인해 주석으로 남긴다. 필요하면 전용 스레드에서 런루프를 돌린다 | [리뷰] | [objc2 run_loop.md#L5](https://github.com/madsmtm/objc2/blob/d634d98df5947d410a718c79b505333b6cd08bdc/docs/run_loop.md) | SHOULD |

### 2.3 에러·panic·로그

| # | 규칙 | 강제 수단 | 근거 URL | 강도 |
|---|---|---|---|---|
| E1 | 비테스트 코드에서 `unwrap`·`expect`·`panic!`·인덱싱을 쓰지 않는다(`unwrap_used`·`expect_used`·`panic`·`indexing_slicing`). 테스트는 clippy.toml `allow-*-in-tests`로 허용한다(실측: 테스트 모듈의 unwrap·인덱싱·println은 통과). 불변식이 증명된 곳은 `#[expect(clippy::expect_used, reason = "불변식: ...")]`(AGENTS 6항) | [자동] clippy | [Zed .rules#L6-L7](https://github.com/zed-industries/zed/blob/c403bc6af644cd0ab60c90045ac6dc3436218809/.rules#L6-L7), [zellij ERROR_HANDLING.md#L1-L12](https://github.com/zellij-org/zellij/blob/519a23715c6b0f65a3ffada64e738ed6ec9380fc/docs/ERROR_HANDLING.md), [ruff AGENTS.md#L222](https://github.com/astral-sh/ruff/blob/d47bd6a76f594838ba9f0802ea46bfbf7f7974d6/AGENTS.md#L222), [Spacedrive .clippy.toml](https://github.com/spacedriveapp/spacedrive/blob/4d41a379f612e23ac43a66ffd67df52ca77f57d4/.clippy.toml) | MUST(데몬·mk-proto), SHOULD(앱) |
| E2 | panic 전략은 unwind(기본값)로 둔다. krilld는 세션 작업 경계에서 `catch_unwind`로 panic을 잡아 그 세션만 `exited`로 표시한다. 전역 상태 손상은 로그를 남기고 종료해 holder가 재기동하게 한다(zellij의 `fatal`/`non_fatal` 구분) | [자동] e2e(세션 하나에 panic 주입 → 다른 세션 생존) | 1.1 #2, [zellij ERROR_HANDLING.md#L60-L95](https://github.com/zellij-org/zellij/blob/519a23715c6b0f65a3ffada64e738ed6ec9380fc/docs/ERROR_HANDLING.md) | MUST |
| E3 | 오류 처리: 라이브러리·데몬은 `thiserror` enum, `anyhow`(+`.context`)는 `mk` CLI `main`과 앱 최상위에서만 쓴다(커밋된 CONVENTIONS P8과 같음). 조사한 프로젝트 다수(zellij·shpool·GitButler)는 데몬에도 anyhow를 쓴다. 하지만 **프로세스 경계를 넘는 오류는 mk-proto의 안정 코드 enum**이어야 하고 받는 쪽이 문자열을 매칭하면 안 되므로 P8 쪽이 Minikrill에 맞다 | [리뷰] | [GitButler crates/AGENTS.md#L92-L94](https://github.com/gitbutlerapp/gitbutler/blob/2d95f3748fe15e9016e332025bc963545102d5b0/crates/AGENTS.md#L92-L94), [GitButler error-cleanup-checklist.md#L37](https://github.com/gitbutlerapp/gitbutler/blob/2d95f3748fe15e9016e332025bc963545102d5b0/error-cleanup-checklist.md#L37) | MUST |
| E4 | 실패할 수 있는 연산 결과를 `let _ =`로 버리지 않는다. 버려야 하면 경고 로그를 남기는 헬퍼(`.log_err()` 류)를 쓴다 | [자동] `let_underscore_must_use`(deny, 실측 동작) | [Zed .rules#L8-L12](https://github.com/zed-industries/zed/blob/c403bc6af644cd0ab60c90045ac6dc3436218809/.rules#L8-L12) | MUST |
| E5 | 로그는 `tracing`으로 남긴다. 데몬은 크기 상한이 있는 파일에 쓰고, 세션 작업에는 `session_id` span을 단다. alacritty_terminal의 `log` 출력은 `tracing-log` 브리지로 받는다. 로그·크래시 파일에 PTY 내용·환경 변수·토큰을 남기지 않는다(바이트는 길이·seq만) | [자동] `print_*` lint + [리뷰] 리댁션 | [shpool libshpool/Cargo.toml#L33-L34](https://github.com/shell-pool/shpool/blob/3c41df9a610428b6c1766d78d36d3fefd5685c3b/libshpool/Cargo.toml#L33-L34), [Spacedrive CLAUDE.md#L49](https://github.com/spacedriveapp/spacedrive/blob/4d41a379f612e23ac43a66ffd67df52ca77f57d4/CLAUDE.md#L49), [Tauri lifecycle.mdx#L105](https://github.com/tauri-apps/tauri-docs/blob/4b74da68aede87a6acb410812911ffd7cd0d9bdc/src/content/docs/security/lifecycle.mdx#L105) | MUST |
| E6 | `mk` CLI는 `main() -> ExitCode`로 끝낸다. `BrokenPipe`는 정상 종료로 처리한다(`mk ls \| head`). 데이터는 stdout, 진단은 stderr로 낸다. `NO_COLOR`를 존중하고 색 없이도 의미가 통해야 한다. 안내는 `hint:` 형식으로 쓴다 | [자동] 통합 테스트 | [ripgrep main.rs#L44-L63](https://github.com/BurntSushi/ripgrep/blob/3fce3b5bb0236da2df6d99672afb8a719642eca7/crates/core/main.rs#L44-L63), [uv STYLE.md#L100-L136](https://github.com/astral-sh/uv/blob/a79aac95dc8f1d79e4396fdcb262d22c520cc1f6/STYLE.md#L100-L136) | SHOULD |
| E7 | 네이티브 크래시(SIGSEGV·abort)는 별도 프로세스 minidump(crash-handler + minidumper)로 수집할지 M4에서 결정한다 | 스파이크 | [Zed crashes.rs](https://github.com/zed-industries/zed/blob/c403bc6af644cd0ab60c90045ac6dc3436218809/crates/crashes/src/crashes.rs) | MAY |

### 2.4 IPC·보안(Tauri 문서에서 옮긴 것 포함)

| # | 규칙 | 강제 수단 | 근거 URL | 강도 |
|---|---|---|---|---|
| S1 | 신뢰 경계를 문서로 정의하고, 경계마다 크기 상한·파서·권한을 mk-proto에 명시한다. 경계: ① UDS 클라이언트 → krilld, ② PTY 출력 바이트(신뢰 불가) → krilld 파서·앱 렌더러, ③ hook stdin JSON, ④ 업데이트 매니페스트 | [리뷰] + 퍼징(MAY) | [Tauri security/index.mdx#L28-L57](https://github.com/tauri-apps/tauri-docs/blob/4b74da68aede87a6acb410812911ffd7cd0d9bdc/src/content/docs/security/index.mdx#L28-L57) | MUST |
| S2 | mk-proto 디코더: 길이 상한(1MiB)을 먼저 검사한다. 모르는 `kind`는 거부하고, 디코드에 실패하면 연결만 끊는다(세션은 유지). 인덱싱 대신 `get`을 쓴다 | [자동] `indexing_slicing` + 골든 바이트 테스트 | [Tauri IPC index.mdx#L28](https://github.com/tauri-apps/tauri-docs/blob/4b74da68aede87a6acb410812911ffd7cd0d9bdc/src/content/docs/concept/Inter-Process%20Communication/index.mdx#L28) | MUST |
| S3 | mk-proto가 유일한 원천이다. 메시지 enum·버전 상수·feature 비트·허용표(메시지 × 호출자 등급: 앱/CLI/hook 세션 범위)를 한 크레이트에 둔다. 런타임 크레이트에 의존하지 않는다. 와이어 프로토콜은 semver상 비공개다 | [자동] `cargo tree -p mk-proto -e normal`이 비어 있는지 검사(check.sh 후보) | [shpool-protocol lib.rs#L21-L35](https://github.com/shell-pool/shpool/blob/3c41df9a610428b6c1766d78d36d3fefd5685c3b/shpool-protocol/src/lib.rs#L21-L35), [shpool HACKING.md#L98-L102](https://github.com/shell-pool/shpool/blob/3c41df9a610428b6c1766d78d36d3fefd5685c3b/HACKING.md#L98-L102), [GitButler electron-migration-assessment.md#L106-L129](https://github.com/gitbutlerapp/gitbutler/blob/2d95f3748fe15e9016e332025bc963545102d5b0/electron-migration-assessment.md#L106-L129) | MUST |
| S4 | 프로토콜 enum의 match에는 와일드카드 `_`를 쓰지 않는다(새 variant 추가 시 컴파일러가 처리를 강제하게) | [자동] `wildcard_enum_match_arm`을 mk-proto·krilld 루트에서 `#![warn]`(lint 이름 실재 확인) | [uv AGENTS.md#L24-L25](https://github.com/astral-sh/uv/blob/a79aac95dc8f1d79e4396fdcb262d22c520cc1f6/AGENTS.md#L24-L25), [GitButler crates/AGENTS.md#L68](https://github.com/gitbutlerapp/gitbutler/blob/2d95f3748fe15e9016e332025bc963545102d5b0/crates/AGENTS.md#L68) | SHOULD |
| S5 | krilld 소켓은 절대 TCP로 열지 않는다(원격은 SSH 포워딩) | [리뷰] | [Tauri lifecycle.mdx#L79-L96](https://github.com/tauri-apps/tauri-docs/blob/4b74da68aede87a6acb410812911ffd7cd0d9bdc/src/content/docs/security/lifecycle.mdx#L79-L96)(무인증 개발 서버 노출 경고) | MUST |
| S6 | 업데이터: 서명 검증을 끄는 코드 경로를 두지 않는다. HTTPS만 허용하고 다운그레이드는 기본 거부한다. 개인키는 오프라인 백업 2벌을 둔다(잃으면 기존 사용자에게 업데이트를 보낼 수 없다) | [리뷰] + 릴리스 체크리스트 | [Tauri updater.mdx#L84-L106, L205-L206, L309](https://github.com/tauri-apps/tauri-docs/blob/4b74da68aede87a6acb410812911ffd7cd0d9bdc/src/content/docs/plugin/updater.mdx#L84-L106) | MUST |
| S7 | 서명·공증: holder·krilld 헬퍼까지 같은 팀 ID로 서명하고 공증·staple한다. 릴리스 직전 `codesign --verify --strict` + `spctl -a`. CI 키체인은 임시 키체인 + `set-key-partition-list`로 다룬다. 러너 이미지가 바뀌면 서명이 깨질 수 있으므로 macOS 러너 이미지 버전을 고정한다 | 릴리스 스크립트 | [Tauri Sign/macos.mdx#L141, L184, L197-L208](https://github.com/tauri-apps/tauri-docs/blob/4b74da68aede87a6acb410812911ffd7cd0d9bdc/src/content/docs/distribute/Sign/macos.mdx#L184), [GitButler PR #15807 본문](https://github.com/gitbutlerapp/gitbutler/pull/15807) | MUST |
| S8 | 취약점 비공개 신고 경로(`SECURITY.md`)를 M4에서 준비한다 | 문서 | [Tauri security/index.mdx "Coordinated Disclosure"](https://github.com/tauri-apps/tauri-docs/blob/4b74da68aede87a6acb410812911ffd7cd0d9bdc/src/content/docs/security/index.mdx) | SHOULD |

### 2.5 의존성·공급망

| # | 규칙 | 강제 수단 | 근거 URL | 강도 |
|---|---|---|---|---|
| D1 | `deny.toml`을 루트에 둔다(커밋 `1286c66`에 반영됨. 이 조사 시작 시점에는 없어서 check.sh가 exit 4였다). 라이선스 allowlist에 GPL 계열을 넣지 않는다. 로컬 GPL 크레이트를 붙였을 때 `rejected`로 실패함을 실측했다. `publish = false` 크레이트는 `[licenses.private] ignore = true`로 검사에서 뺀다 | [자동] check.sh `cargo deny` | [tokio deny.toml](https://github.com/tokio-rs/tokio/blob/52ac7816230efabcae6fcc09cbe7ae630a48eddf/deny.toml), [uv .cargo/deny.toml](https://github.com/astral-sh/uv/blob/a79aac95dc8f1d79e4396fdcb262d22c520cc1f6/.cargo/deny.toml), [shpool deny.toml](https://github.com/shell-pool/shpool/blob/3c41df9a610428b6c1766d78d36d3fefd5685c3b/deny.toml), [Zed terminal Cargo.toml#L6](https://github.com/zed-industries/zed/blob/c403bc6af644cd0ab60c90045ac6dc3436218809/crates/terminal/Cargo.toml#L6) | MUST |
| D2 | `wildcards = "deny"` + `allow-wildcard-paths = true`. 버전 없는 워크스페이스 path 의존성은 이 키가 없으면 `wildcard`로 실패한다(실측) | [자동] cargo-deny | 실측, [tokio deny.toml](https://github.com/tokio-rs/tokio/blob/52ac7816230efabcae6fcc09cbe7ae630a48eddf/deny.toml) | MUST |
| D3 | `unknown-registry/unknown-git = "deny"`. gpui는 ARCHITECTURE대로 crates.io `gpui-pre` + `cargo vendor`로 받으므로 git 소스 예외가 필요 없다 | [자동] cargo-deny | [uv .cargo/deny.toml#L94-L96](https://github.com/astral-sh/uv/blob/a79aac95dc8f1d79e4396fdcb262d22c520cc1f6/.cargo/deny.toml#L94-L96) | MUST |
| D4 | lockfile은 `--locked`로 빌드한다(현행 check.sh). 갱신은 `cargo update --precise <crate>`로만 하고 전체 `cargo update`는 금지한다 | [자동] `--locked` + [리뷰] | [uv AGENTS.md#L29-L30](https://github.com/astral-sh/uv/blob/a79aac95dc8f1d79e4396fdcb262d22c520cc1f6/AGENTS.md#L29-L30) | MUST |
| D5 | 의존성을 추가할 때 `Cargo.toml`에 용도 한 줄 주석을 단다. 버전은 `[workspace.dependencies]`에서만 정한다(AGENTS 구조 절, deny `workspace-dependencies`). 기본 feature는 `[workspace.dependencies]` 쪽에서 끄고 필요한 것만 켠다(postcard 기본 feature는 비 Apple 타깃용 unmaintained 크레이트를 lockfile에 넣는다) | [리뷰] + cargo-deny advisories | [shpool libshpool/Cargo.toml#L21-L53](https://github.com/shell-pool/shpool/blob/3c41df9a610428b6c1766d78d36d3fefd5685c3b/libshpool/Cargo.toml#L21-L53), [Zed objc2 default-features=false](https://github.com/zed-industries/zed/blob/c403bc6af644cd0ab60c90045ac6dc3436218809/Cargo.toml#L703-L764), 실측 | SHOULD |
| D6 | 미사용 의존성은 `cargo shear --deny-warnings`로 잡는다(이 맥에 미설치 → 설치는 사람 결정) | 도구 추가 시 [자동] | [Zed run_tests.yml#L699](https://github.com/zed-industries/zed/blob/c403bc6af644cd0ab60c90045ac6dc3436218809/.github/workflows/run_tests.yml#L699), [uv check-lint.yml#L211-L226](https://github.com/astral-sh/uv/blob/a79aac95dc8f1d79e4396fdcb262d22c520cc1f6/.github/workflows/check-lint.yml#L211-L226) | MAY |
| D7 | gpui·gpui-component·vendor 패치는 정해진 절차로만 올린다: 둘을 함께 올리고 패치를 rebase한 뒤 S-IME와 메모리 게이트를 다시 측정한다. 절차는 문서에 둔다 | [리뷰] + 메모리 측정 | [Spacedrive Cargo.toml#L44-L53](https://github.com/spacedriveapp/spacedrive/blob/4d41a379f612e23ac43a66ffd67df52ca77f57d4/Cargo.toml#L44-L53) | MUST |
| D8 | 새로 게시된 버전 쿨다운(예: 7~14일)은 신규 의존성에만 SHOULD로 둔다. 보안 패치와 이미 결정한 gpui 핀은 예외로 하고 사유를 기록한다 | [리뷰](cargo-cooldown 미설치) | [WezTerm cooldown.toml](https://github.com/wezterm/wezterm/blob/e2a98e1b8af156988ad0ac778658ac27b60a55fb/cooldown.toml), 1.2 gpui-pre 게시일 | SHOULD |
| D9 | GitHub Actions를 도입하면(MK-017) 외부 액션을 커밋 SHA로 고정하고 `permissions: contents: read`를 기본으로 둔다 | CI 도입 시 zizmor | [Tauri lifecycle.mdx#L139-L140](https://github.com/tauri-apps/tauri-docs/blob/4b74da68aede87a6acb410812911ffd7cd0d9bdc/src/content/docs/security/lifecycle.mdx#L139-L140), [uv check-lint.yml#L12, L121](https://github.com/astral-sh/uv/blob/a79aac95dc8f1d79e4396fdcb262d22c520cc1f6/.github/workflows/check-lint.yml#L121) | SHOULD |

### 2.6 테스트·성능·메모리·앱

| # | 규칙 | 강제 수단 | 근거 URL | 강도 |
|---|---|---|---|---|
| T1 | 데몬 통합 테스트는 `sleep`으로 기다리지 않는다. `test-hooks` feature로 이벤트 소켓을 열고 이름 붙은 이벤트를 기다린다. `MK_LEAVE_TEST_LOGS=1`이면 로그를 보존한다 | [리뷰] | [shpool test_hooks.rs#L16-L23](https://github.com/shell-pool/shpool/blob/3c41df9a610428b6c1766d78d36d3fefd5685c3b/libshpool/src/test_hooks.rs#L16-L23), [shpool HACKING.md#L221-L240](https://github.com/shell-pool/shpool/blob/3c41df9a610428b6c1766d78d36d3fefd5685c3b/HACKING.md#L221-L240) | SHOULD |
| T2 | nextest 설정 `.config/nextest.toml`: 기본 `slow-timeout = { period = "60s", terminate-after = 1 }`, PTY·launchd 테스트는 `max-threads = 1` 그룹 | [자동] nextest | [Zed .config/nextest.toml](https://github.com/zed-industries/zed/blob/c403bc6af644cd0ab60c90045ac6dc3436218809/.config/nextest.toml) | SHOULD |
| T3 | 테스트 가능한 로직(토큰 버킷·보존 기간·하트비트)은 `SystemTime::now()`/`Instant::now()`를 직접 부르지 않고 시간을 주입받는다 | [리뷰] | [GitButler crates/AGENTS.md#L88](https://github.com/gitbutlerapp/gitbutler/blob/2d95f3748fe15e9016e332025bc963545102d5b0/crates/AGENTS.md#L88) | SHOULD |
| T4 | 메모리 회귀: 메모리에 영향을 주는 경로(링·Term·렌더)를 바꾸면 phys_footprint를 측정해 보고한다. 오래 유지하는 컬렉션은 `shrink_to_fit`이나 boxed slice로 남는 용량을 없앤다 | [자동] nightly 측정 스크립트 + [리뷰] | [ruff memory_report.yaml](https://github.com/astral-sh/ruff/blob/d47bd6a76f594838ba9f0802ea46bfbf7f7974d6/.github/workflows/memory_report.yaml), [ruff AGENTS.md#L204](https://github.com/astral-sh/ruff/blob/d47bd6a76f594838ba9f0802ea46bfbf7f7974d6/AGENTS.md#L204) | MUST |
| T5 | 처리량·지연에 영향을 주는 변경은 벤치 결과를 첨부한다(대량 출력, 키 에코 p95) | [리뷰] | [Alacritty CONTRIBUTING.md#L78-L80](https://github.com/alacritty/alacritty/blob/29dc55375da817d53540f4caefbe708703c57b07/CONTRIBUTING.md#L78-L80), [shpool HACKING.md(sshping)](https://github.com/shell-pool/shpool/blob/3c41df9a610428b6c1766d78d36d3fefd5685c3b/HACKING.md) | SHOULD |
| T6 | 프로필: dev `debug = "line-tables-only"`, dev에서도 `alacritty_terminal`·`vte`는 `opt-level = 3`. release `lto = "thin"`, `codegen-units = 1`, `strip = true`(dSYM 별도 보관). `panic`은 지정하지 않는다(E2) | [리뷰] | [uv Cargo.toml#L470-L482](https://github.com/astral-sh/uv/blob/a79aac95dc8f1d79e4396fdcb262d22c520cc1f6/Cargo.toml#L470-L482), [zellij Cargo.toml#L142-L206](https://github.com/zellij-org/zellij/blob/519a23715c6b0f65a3ffada64e738ed6ec9380fc/Cargo.toml#L142-L206), [Zed Cargo.toml#L1096-L1099](https://github.com/zed-industries/zed/blob/c403bc6af644cd0ab60c90045ac6dc3436218809/Cargo.toml#L1096-L1099) | SHOULD |
| T7 | 호출 스레드를 막는 `Command::output/status`를 금지한다. mk-app은 백그라운드 실행기로 돌리고, krilld는 git 작업 큐 모듈(전용 스레드) 첫 줄에 `#![expect(clippy::disallowed_methods, reason = "...")]`를 둔 곳에서만 허용한다 | [자동] clippy(clippy.toml 목록, `disallowed_methods`는 `clippy::all`에 포함돼 deny. expect가 없는 모듈에서 실패하고 expect가 있는 모듈은 통과 — 실측) | [Zed clippy.toml#L8-L19](https://github.com/zed-industries/zed/blob/c403bc6af644cd0ab60c90045ac6dc3436218809/clippy.toml#L8-L19), [ruff ty_ide lib.rs#L1-L4](https://github.com/astral-sh/ruff/blob/d47bd6a76f594838ba9f0802ea46bfbf7f7974d6/crates/ty_ide/src/lib.rs#L1-L4) | MUST |
| T8 | gpui `Task`는 await·`detach()`/`detach_and_log_err`·필드 보관 중 하나로 처리한다(drop하면 취소된다). 상호 참조는 `WeakEntity`로 하고, 클로저 안에서는 안쪽 `cx`만 쓴다. 이미 update 중인 entity를 다시 update하지 않는다(panic) | [리뷰] 체크리스트 | [Zed .rules#L63-L83](https://github.com/zed-industries/zed/blob/c403bc6af644cd0ab60c90045ac6dc3436218809/.rules#L63-L83) | MUST |
| T9 | 커밋 첫 줄은 AGENTS 규칙 `<OWE-n>: <무엇을>`을 따른다(Conventional Commits는 도입하지 않는다). 사용자에게 보이는 변경은 M4부터 CHANGELOG에 적는다 | [리뷰] | AGENTS.md "이슈·작업 흐름", [Alacritty CONTRIBUTING.md#L94-L98](https://github.com/alacritty/alacritty/blob/29dc55375da817d53540f4caefbe708703c57b07/CONTRIBUTING.md#L94-L98) | SHOULD |

### 2.7 공통 수렴 규칙(3개 이상 프로젝트가 같은 선택, 이 조사에서 원본 확인한 것만)

| 규칙 | 프로젝트(확인분) | 수 | Minikrill |
|---|---|---|---|
| CI에서 clippy 경고 = 오류 | Zed(`--deny warnings`), Helix(`-D warnings`), uv(`CARGO_BUILD_WARNINGS`), shpool(cranky `-D warnings`), Tauri(`-D warnings`), tokio(`RUSTFLAGS`), Alacritty(`cfg_attr(clippy, deny(warnings))`) | 7 | check.sh로 이미 채택 |
| 릴리스 LTO | Zed·Alacritty·Helix(thin), zellij·Tauri(true), uv·ruff(fat) | 7 | thin |
| edition 2024 | Zed(rustfmt), Alacritty, uv, ruff, ripgrep, GitButler, libshpool | 7 | 채택(현행) |
| 툴체인 파일 고정 | Zed, zellij, shpool, Helix, uv, ruff, GitButler, Spacedrive | 8 | **채택 불가**(rustup 없음) → `rust-version`(C1) |
| `[workspace.lints]` | Zed, uv, ruff, GitButler, objc2, Tauri | 6 | 채택(현행) |
| CI `cargo fmt --check` | Zed, Helix, ripgrep, zellij(xtask), shpool(+nightly) | 5 | 채택(현행) |
| cargo-deny | WezTerm, shpool, tokio, GitButler, uv | 5 | 채택, deny.toml 필수(D1) |
| tracing | Zed, uv, ruff, GitButler, shpool(+Spacedrive 문서) | 5 | 채택(E5) |
| anyhow + thiserror | Zed, uv, ruff, GitButler, WezTerm(+zellij·shpool anyhow) | 5 | 채택(E3) |
| 비테스트 unwrap 회피(문서) | Zed, zellij, uv, ruff | 4 | lint로 강제(E1) |
| release `codegen-units = 1` | Zed(메인 크레이트 제외), zellij, uv, Tauri | 4 | 채택 |
| dev 디버그 정보 축소 | Zed(`limited`), zellij·uv(`line-tables-only`) | 3 | 채택 |
| `dbg_macro` lint | Zed(deny), uv, ruff(warn) | 3 | deny |
| `print_stdout/stderr` 금지 | uv, ruff, Spacedrive(문서) | 3 | 채택(`mk` 예외) |
| `redundant_clone` | Zed(deny), ruff, Tauri(warn) | 3 | 채택 |
| release `strip` | zellij, uv, Tauri | 3 | 채택 |

갈리는 지점: rustfmt(빈 파일 ~ nightly 다수), clippy 강도(ripgrep은 CI에 clippy가 없음, Zed는 `style` 해제, uv·ruff는 pedantic), panic 전략(uv·Tauri abort / Zed unwind + 크래시 핸들러). 2곳에서만 확인된 것(SAFETY 주석: uv·shpool, `unsafe_op_in_unsafe_fn = deny`: tokio·objc2, pre_exec 시그널 복원: Alacritty·WezTerm, Actions SHA 고정: Zed·uv)은 수가 적지만 Minikrill 위험과 직결되므로 위 규칙에 넣었다.

### 2.8 프로젝트별 "가져올 것 / 가져오면 안 되는 것"

| 프로젝트 | 가져올 것 | 가져오면 안 되는 것(이유) |
|---|---|---|
| Zed | 2줄 rustfmt, `dbg_macro`·`todo` deny, `disallowed-methods`로 UI 블로킹 금지, `.rules`의 unwrap·`let _`·`mod.rs`·gpui Task 규칙, check-todos | `style = allow`(Zed는 clippy가 수 분 걸려서 내린 선택), `--cfg tokio_unstable` 전역 rustflags, dylint·typos·buf, 터미널 코드(GPL-3.0), `.rules`의 README 표식 HARD RULE(Zed 전용) |
| Alacritty | pre_exec 시그널 복원, 벤치 첨부 관례 | nightly rustfmt, SAFETY 없는 unsafe, CI의 MSRV·stable 이중 행렬(툴체인 하나뿐) |
| WezTerm | `sigprocmask` 초기화, "Cocoa가 자식에게 fd를 흘린다"는 사실 | pre_exec 안 `read_dir`(할당), edition 2018, 느슨한 deny.toml(LGPL·WTFPL·wildcards allow) |
| zellij | `fatal`/`non_fatal` 구분, dev에서 vte·alacritty_terminal 최적화 | xtask 래핑(6 크레이트에 과잉), miette(데몬은 화면이 없음) |
| shpool | 피어 검사, 프로토콜 크레이트 분리 + 버전 헤더, test_hooks, 의존성 용도 주석 | nightly rustfmt, cargo-cranky(0.3.0 고정, 대체됨), Debian MSRV 정책, Conventional Commits(AGENTS 형식과 충돌) |
| Helix | `RUSTDOCFLAGS=-D warnings` 문서 검사(MAY), macOS fd 한도 인지 | 낮은 MSRV 정책, lint 테이블 없음 |
| uv·ruff | lint 목록, `#[expect]`, SAFETY, 완전한 match, `--precise`, STYLE, 메모리 리포트, "경고를 기존 문제로 가정하지 말 것" | std::fs 전면 금지(`fs-err` 추가 필요), insta 대량 사용, release `panic = "abort"`(E2와 충돌) |
| ripgrep | BrokenPipe 처리, 비자명한 결정 주석 | 79열, beta·nightly 행렬, CI에서 clippy 생략 |
| tokio | `unsafe_op_in_unsafe_fn = deny`, deny.toml 형식 | miri·loom·ASan·semver-checks 전체(라이브러리 생태계용) |
| GitButler | 안정 오류 코드, 시간 주입, 모호한 이름 금지, 락은 최상위 경계에서, 명령 레지스트리 단일 원천 | 프런트엔드·N-API 규칙 |
| Spacedrive | gpui 업그레이드 절차, `allow-unwrap-in-tests` | `hard_tabs`, 787줄 AGENTS.md |
| objc2 | `MainThreadMarker`, layered safety, `unsafe_op_in_unsafe_fn = deny` | `clippy::cargo` 그룹(crates.io 배포용) |
| nix | libc 우선, `OwnedFd` 결과 타입 | — |

### 2.9 Tauri를 쓰지 않아 생기는 공백

GitButler가 Tauri에서 떠날 때 다시 짜야 한다고 꼽은 셸 고유 코드는 앱 메뉴, 멀티 윈도우·창 상태, 딥링크·단일 인스턴스, 업데이터다. GitButler는 이 중 업데이터 이전을 "가장 위험"으로 봤다([electron-migration-assessment.md#L86-L99](https://github.com/gitbutlerapp/gitbutler/blob/2d95f3748fe15e9016e332025bc963545102d5b0/electron-migration-assessment.md#L86-L99)).

| 공백 | Tauri가 주던 것 | Minikrill 담당 |
|---|---|---|
| 업데이터 | 서명 강제·TLS 강제 | 자체 업데이터(S6). 서명 비활성 경로 없음 |
| 서명·공증·DMG | `tauri build` | 릴리스 스크립트(S7) |
| 단일 인스턴스 | single-instance 플러그인 | 앱 두 번째 실행 → 기존 창 활성화. 데몬 단일성은 launchd 라벨·소켓으로 보장 |
| 창 상태 | window-state 플러그인 | 임시 파일 → rename 원자 저장 |
| 메뉴·알림 | Menu·Notification API | objc2 AppKit(U11) |
| 권한 ACL | capabilities·permissions·runtime authority | mk-proto 허용표 + 피어 검사(S3·U9) |
| IPC 바인딩 | `generate_handler!` | mk-proto enum 단일 원천(S3) |
| 웹뷰 CSP | CSP·isolation | 해당 없음. 대신 **터미널 이스케이프 시퀀스**(OSC 52 클립보드 쓰기, OSC 8 링크, 제목 주입)가 공격 표면이다 → T3에서 허용 정책 결정 |
| 딥링크 | deep-link 플러그인 | v1 범위 밖(YAGNI) |

## 3. 바로 쓸 설정 초안(이 맥에서 동작 확인)

### 3.0 확인 방법

- **1차 실측(커밋 `1286c66` 이전)**: 저장소 사본(`/tmp/c2v/ws`)에 3.5의 독립 초안을 적용했다. krilld에 `nix 0.31`·`zstd 0.14`·`tracing`·`anyhow`·`thiserror`·`postcard 1`·`alacritty_terminal =0.26.0`을, mk-app에 `objc2 0.6`·`objc2-app-kit 0.3`·`objc2-foundation 0.3`을 붙였다. 결과는 Cargo.lock 111개 패키지, `zsh scripts/check.sh` exit 0이다. 위반 코드도 넣어 각 lint가 실제로 실패하는지 확인했다: `buf[0]`, `let _ = fallible()`, `.unwrap()`, `eprintln!`, `RawFd`, 이유 없는 `#[allow]`, SAFETY 없는 unsafe, `#![expect(unsafe_code)]` 없는 unsafe, mk-app의 `Command::output`. 테스트 모듈 안 unwrap·인덱싱·println은 통과했다. 로컬 GPL 크레이트를 붙이면 deny가 `rejected`로 실패했다.
- **2차 실측(커밋된 설정 기준)**: 현재 저장소의 Cargo.toml·deny.toml·clippy.toml·rustfmt.toml을 사본(`/tmp/c2v/ws2`)에 복사하고 같은 의존성을 `[workspace.dependencies]` + `{ workspace = true }`로 붙였다. check.sh exit 0이었다. 여기에 3.1·3.2 추가분을 얹어도 check.sh exit 0이었다. 추가분이 실제로 막는지도 확인했다: mk-app의 `Command::output` → 실패, krilld git 큐 모듈은 `#![expect]`가 있으면 통과·없으면 실패.
- lint 이름은 `cargo clippy --explain <name>`으로 하나씩 존재를 확인했다(없는 이름은 `unknown lint`). clippy.toml 키는 clippy 1.98이 오류 없이 읽는 것으로 확인했다. **하네스 파일이므로 적용은 사람 승인 사항이다**(AGENTS 8항).

### 3.1 `Cargo.toml` `[workspace.lints.clippy]`에 추가(커밋된 목록 뒤)

```toml
redundant_clone = "warn"   # nursery라 clippy::all에 없음. Zed deny, ruff·Tauri warn
get_unwrap = "warn"        # uv·ruff
mod_module_files = "warn"  # Zed .rules "mod.rs 금지"를 기계로
```

### 3.2 `clippy.toml`에 추가

```toml
# disallowed-methods 배열에 두 줄 추가
  { path = "std::process::Command::output", reason = "호출 스레드를 막는다. mk-app은 백그라운드 실행기, krilld는 git 작업 큐 모듈에서만" },
  { path = "std::process::Command::status", reason = "호출 스레드를 막는다. mk-app은 백그라운드 실행기, krilld는 git 작업 큐 모듈에서만" },

# 새 키
disallowed-types = [
  { path = "std::os::fd::RawFd", reason = "OwnedFd/BorrowedFd를 쓴다(CONVENTIONS U5). FFI 모듈만 expect" },
]
```

허용할 모듈 첫 줄(사본에서 동작 확인):

```rust
#![expect(clippy::disallowed_methods, reason = "git 작업 큐는 전용 스레드에서 git CLI를 실행한다")]
```

### 3.3 `deny.toml`·`rustfmt.toml`

커밋된 파일을 그대로 유지한다. 커밋된 deny.toml은 이 보고서의 독립 초안(3.5)보다 엄격하다(`unmaintained = "workspace"`, `unsound = "all"`, `workspace-dependencies`, `required-git-spec = "rev"`). cargo-deny 0.20.2에서 실제 의존성 111개로 `advisories ok, bans ok, licenses ok, sources ok`를 확인했다. 주의할 점이 둘 있다.
- `[graph] targets`를 비우거나 Linux·임베디드 타깃을 넣으면 postcard 기본 feature의 `atomic-polyfill`이 그래프에 들어와 advisories가 실패할 수 있다(실측). postcard는 `[workspace.dependencies]`에서 `default-features = false, features = ["use-std"]`로 둔다.
- `allow-wildcard-paths = true`를 빼면 버전 없는 워크스페이스 path 의존성이 `wildcard` 오류가 된다(실측).

### 3.4 launchd plist(코드가 생성하는 파일, MK-007 범위)

```xml
<key>SoftResourceLimits</key>
<dict><key>NumberOfFiles</key><integer>8192</integer></dict>
```

키 이름은 `man launchd.plist`로 확인했다. `8192`는 예시값이다(세션 상한 256 × (master fd + 세그먼트 파일) + 소켓을 덮는 값, 측정 전). `gui/<uid>` 비지속 잡에 실제로 적용되는지는 미확인이다(6장). 코드 쪽 `RLIMIT_NOFILE` 상향은 CONVENTIONS에 이미 있다.

### 3.5 (참고) 커밋 전 독립 초안 — 1차 실측에서 check.sh exit 0

커밋된 설정과 비교할 수 있도록 남긴다. 커밋된 쪽과 다른 점: `all-features = true`, 워크스페이스 `print_*`·`unwrap_used` 등을 warn으로 둠, `unreachable_pub` 추가, `disallowed_methods = "allow"` + mk-app 루트 `#![warn]`(ruff 패턴), `edition`을 rustfmt에도 명시.

```toml
[workspace.lints.rust]
unsafe_code = "deny"
unsafe_op_in_unsafe_fn = "deny"
missing_debug_implementations = "warn"
unreachable_pub = "warn"
unused_must_use = "deny"

[workspace.lints.clippy]
all = { level = "deny", priority = -1 }
pedantic = { level = "warn", priority = -1 }
module_name_repetitions = "allow"
must_use_candidate = "allow"
missing_errors_doc = "allow"
missing_panics_doc = "allow"
too_many_lines = "allow"
similar_names = "allow"
undocumented_unsafe_blocks = "deny"
multiple_unsafe_ops_per_block = "warn"
allow_attributes = "warn"
allow_attributes_without_reason = "deny"
dbg_macro = "deny"
todo = "deny"
unimplemented = "deny"
print_stdout = "warn"
print_stderr = "warn"
exit = "warn"
mem_forget = "warn"
let_underscore_must_use = "deny"
unwrap_used = "warn"
expect_used = "warn"
panic = "warn"
indexing_slicing = "warn"
get_unwrap = "warn"
redundant_clone = "warn"
disallowed_methods = "allow"
disallowed_types = "deny"
mod_module_files = "warn"
```

```toml
# deny.toml(독립 초안)
[graph]
targets = ["aarch64-apple-darwin", "x86_64-apple-darwin"]
all-features = true
[advisories]
yanked = "deny"
[licenses]
allow = ["MIT", "Apache-2.0", "Apache-2.0 WITH LLVM-exception", "BSD-2-Clause", "BSD-3-Clause", "ISC", "Zlib", "Unicode-3.0"]
confidence-threshold = 0.93
unused-allowed-license = "allow"
[licenses.private]
ignore = true
[bans]
multiple-versions = "warn"
wildcards = "deny"
allow-wildcard-paths = true
[sources]
unknown-registry = "deny"
unknown-git = "deny"
allow-git = []
```

### 3.6 적용하지 않은 것(이유)

- `rust-toolchain.toml`: Homebrew 환경에서 무시된다(1.1 #1).
- release `panic = "abort"`: E2·CONVENTIONS P4와 충돌한다.
- `CARGO_BUILD_WARNINGS=deny`: check.sh의 `-D warnings`와 효과가 같다.
- `cargo shear`·zizmor·cargo-cooldown: 이 맥에 설치되어 있지 않아 실측하지 못했다(도구 설치는 사람 결정).

## 4. ARCHITECTURE·AGENTS.md·CONVENTIONS와 충돌하는 사실과 고칠 문장

이 조사를 하는 동안 커밋 `1286c66`과 작업 트리 수정으로 AGENTS.md 6항(panic 격리), ARCHITECTURE §4(PTY는 `rustix-openpty` 또는 nix, `RLIMIT_NOFILE` 상향, `catch_unwind`), CONVENTIONS(W5·P4·U1·U7)가 이미 이 보고서의 검증 결과와 같은 방향으로 바뀌었다. 아래는 **2026-10-14 현재 작업 트리 기준으로 남은 충돌**이다.

| 위치 | 현재 문장 | 충돌 사실 | 고칠 문장(제안) |
|---|---|---|---|
| ROADMAP MK-017 "CI: Linux 단위·clippy·cargo deny + macOS PTY e2e" | CI 범위 | mk-app(objc2·gpui)과 `cfg(target_os = "macos")` 코드는 Linux clippy가 검사하지 못한다. Zed도 `clippy_mac` 잡을 따로 둔다. deny도 `targets`가 macOS 둘이라 Linux에서 돌려도 결과가 같다 | "clippy·nextest는 macOS 러너(또는 로컬 check.sh), Linux 잡은 mk-proto·mk-term 단위 테스트만" |
| ARCHITECTURE §4 "기동: … 비지속 잡(`RunAtLoad`, `KeepAlive=false`, `AbandonProcessGroup=true`, `ProcessType=Interactive`)" | plist 키 목록 | launchd 잡의 maxfiles 소프트 한도는 256이다(`launchctl limit maxfiles` 실측). 코드에서 올리는 규칙은 있지만 plist 키가 없다 | 키 목록에 `SoftResourceLimits.NumberOfFiles`를 추가(3.4) |
| CONVENTIONS U5 "fd는 `OwnedFd`/`BorrowedFd`로만 주고받는다. 정수 fd를 받는 안전 함수 금지" | 리뷰 의존 | clippy `disallowed-types`로 `RawFd`를 막으면 기계 검사가 된다(실측) | 끝에 "(`clippy.toml disallowed-types`의 `RawFd`, FFI 모듈만 `#![expect]`)" 추가(3.2) |
| CONVENTIONS(해당 규칙 없음) | — | UI 스레드에서 `Command::output/status`를 금지하는 규칙이 없다. ARCHITECTURE §6은 "status·diff 조회는 앱"이라 바로 필요하다 | §7 gpui에 "**G MUST** 메인 스레드에서 `Command::output/status` 금지(clippy.toml), krilld는 git 큐 모듈만 `#![expect]`" 추가(3.2) |
| CONVENTIONS §4 P8 vs 조사 결과 | 오류 | 조사한 데몬 다수(zellij·shpool)는 anyhow를 쓴다. 다만 P8(데몬은 thiserror)이 IPC 안정 코드 요구와 맞으므로 **충돌 아님, 유지** | — |
| CONVENTIONS(해당 규칙 없음) | — | 2장 중 기계 검사나 리뷰 체크리스트로 옮길 만한 것이 CONVENTIONS에 없다: U9 피어 검사 fail-closed, U10 종료 절차 한 함수, T1 테스트 sleep 금지(test-hooks), E5 tracing + alacritty `log` 브리지, T3 시간 주입, S4 프로토콜 enum 와일드카드 금지 | 사람이 골라 CONVENTIONS에 추가(SHOULD 이상만) |
| ARCHITECTURE §7 "크래시: panic hook → 로컬 파일" | 크래시 | panic 전략은 §4(catch_unwind)와 CONVENTIONS P4에 있지만 §7에는 없다. 네이티브 크래시(SIGSEGV)는 panic hook으로 잡히지 않는다 | "panic 전략은 unwind(CONVENTIONS P4). 네이티브 크래시 수집(minidump, Zed `crashes` 방식)은 M4 결정" |
| ARCHITECTURE §5 gpui `=` 고정 | 공급망 | `gpui-pre 0.3.9`·`gpui-component 0.7.3`은 2026-10-12 게시(조사일 기준 2일 전)다. 쿨다운 규칙은 현재 없으므로 충돌은 아니다 | 쿨다운 규칙을 도입한다면 "ARCHITECTURE 명시 핀은 예외"를 함께 적는다 |

## 5. 위험

1. **하네스 설정이 조사 도중에 바뀌었다.** 이 보고서의 1차 실측은 커밋 `1286c66` 이전 상태(deny.toml 없음, check.sh exit 4)에서 했다. 결론과 3·4장은 커밋 이후 상태로 다시 실측해 고쳤다. 다만 작업 트리에 아직 커밋되지 않은 수정(ARCHITECTURE 등)이 있어, 사람이 병합할 때 4장 표를 다시 대조해야 한다.
2. **lint 강화 직후 기존 코드 위반.** 지금은 크레이트가 스텁(64줄)이라 영향이 없다. 하지만 코드가 쌓인 뒤 pedantic·`unwrap_used`를 켜면 한 번에 대량 수정이 필요하다. 지금 켜는 것이 가장 싸다.
3. **pedantic + `-D warnings`와 gpui 스타일 충돌.** gpui 빌더 체인·큰 클로저에서 pedantic 경고가 늘 수 있다(Zed는 반대로 `style`을 껐다). 대응: mk-app 루트에 `#![expect(clippy::pedantic 하위 lint, reason)]`를 모아 두고, S-IME 스파이크 코드로 경고 수를 실측한다. 이 조사에서는 mk-app 실제 코드가 없어 측정하지 못했다.
4. **`allow_attributes`(커밋된 설정은 deny)와 외부 매크로·생성 코드.** 사용자 코드의 `#[allow]`는 막지만 build.rs 생성 코드나 vendor 패치에 `#[allow]`가 있으면 걸릴 수 있다. vendor는 워크스페이스 밖이라 해당되지 않을 것으로 추정한다(미측정).
5. **unwind 유지의 비용.** 바이너리 크기와 unwind 테이블 비용이 abort보다 크다. 메모리 예산(앱 120MB·데몬 60MB)에 미치는 영향은 측정하지 않았다(추정: 미미).
6. **pre_exec·fork 경로.** std `Command`는 `pre_exec`이나 `setsid`가 있으면 macOS에서 fork 경로를 쓴다(1.1 #17). holder가 멀티스레드면 자식 쪽 제약(U5)이 전부 적용된다. holder를 단일 스레드로 유지하는 것을 T2와 함께 검토한다.
7. **cargo-deny GPL 차단은 의존성만 막는다.** Zed 터미널 코드를 복사해 넣는 것은 막지 못하므로 클린룸 규칙(AGENTS 4항)은 리뷰로만 강제된다.
8. **Conductor 스택 정보는 2차 출처**(Thread Reader 재게시)다. 컨벤션 결정 근거로 쓰지 않는다.
9. **GitButler RLIMIT 근거 PR(#14687)은 아직 열려 있다.** 다만 이 맥의 launchd 256 실측이 독립 근거다.

## 6. 미확인

- `POSIX_SPAWN_SETSID` + `posix_spawn`으로 PTY 자식을 띄울 때 controlling terminal을 얻을 수 있는지(TIOCSCTTY 대체 가능성). 스파이크가 필요하다.
- mk-app 실제 gpui 코드에 3.1 lint를 적용했을 때의 경고 수(코드 없음).
- `cargo shear`, zizmor, cargo-cooldown이 이 맥에서 동작하는지(미설치라 실행하지 않았다).
- Conductor 기술 스택의 1차 출처(X 원문, 앱 번들). 이 맥에 Conductor가 설치되어 있지 않다.
- launchd plist의 `SoftResourceLimits.NumberOfFiles`가 `gui/<uid>` 비지속 잡에 실제로 적용되는지(man 문서만 확인, 잡을 띄워 보지 않았다).
- GitButler가 Tauri에서 Electron으로 옮기려는 동기(평가서는 "어떻게"만 다룬다).
- unwind 대비 abort의 바이너리 크기·메모리 차이(측정하지 않았다).
- Tauri 저장소 `supply-chain/`(cargo-vet) 내용과 외부 감사 보고서 2건(Radically Open Security v1·v2 PDF)의 지적 사항. 파일이 있는 것만 확인하고 열지 않았다.
