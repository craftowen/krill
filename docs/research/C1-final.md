# C1-final: 공식 문서로 본 Rust 데스크톱 앱·데몬 컨벤션 근거 (2차 검증본)

**결론(가장 중요한 3가지):**
- **① TCC 상속 근거 없음.** ARCHITECTURE §4는 "헬퍼를 앱과 같은 identifier·팀으로 서명하면 TCC 권한이 상속된다"고 쓰지만, 이를 뒷받침하는 Apple 공식 근거는 없다. Apple은 오히려 프로그램마다 고유 identifier를 쓰라고 하고, launchd 잡의 귀속은 SMAppService나 `AssociatedBundleIdentifiers`로 알리라고 한다. macOS 26부터는 앱을 종료한 뒤에도 남는 백그라운드 작업에 사용자 허용 프롬프트가 뜬다. MK-007 스파이크의 합격 기준에 이 두 가지를 넣어야 한다.
- **② 현재 하네스 게이트가 깨져 있다.** 지금 저장소에서 `scripts/check.sh`의 deny 단계는 **exit 4로 실패**한다. deny.toml이 없고, 크레이트에 license 필드가 없어 `licenses FAILED`가 난다. 아래 §3의 설정 초안(Cargo.toml lints, clippy.toml, rustfmt.toml, deny.toml)을 /tmp 복제본에 넣고 이 맥의 rustc 1.98.1·cargo-deny 0.20.2로 `check.sh`를 돌렸고, **exit 0**으로 통과했다.
- **③ panic 전략과 AGENTS 규칙 6이 서로 걸린다.** Cargo는 패키지별 `panic` 설정을 허용하지 않는다. `panic=abort`에서는 `catch_unwind`로 세션 단위 격리를 할 수 없다. 그래서 krilld에 대해 "abort하고 holder가 재기동"과 "unwind하고 세션 단위로 catch_unwind" 중 하나를 사람이 골라야 한다. 어느 쪽이든 `extern "C"` 콜백 안의 panic은 프로세스를 abort시킨다.

- 조사일: 2026-10-14. 로컬은 rustc/cargo 1.98.1(Homebrew), clippy 0.1.98, rustfmt 1.9.0, cargo-deny 0.20.2, cargo-nextest 0.9.148, macOS 27.0.1이다. 이 맥에는 rustup, cargo-audit, Miri, Xcode가 없다(CommandLineTools만 있음).
- 최신 stable은 **Rust 1.99.0(2026-10-01)**이다. 근거: https://github.com/rust-lang/rust/blob/master/RELEASES.md
- 검증 방법: 1차 보고서(.ref/drafts/conv/C1-A.md, 549줄)의 주장을 네 영역으로 나눠 원문 URL을 열고 하나씩 대조했다. 네 영역은 Rust 언어·std, Cargo·clippy·rustfmt, 공급망·gpui, Tauri·Apple이다. 설정값은 저장소를 /tmp에 복제한 뒤 실제 명령으로 재현했다. 저장소 파일은 이 보고서 외에 고치지 않았다.
- 표기: **[실측]**은 이 맥에서 명령으로 재현한 것이다. **[DTS]**는 Apple DTS 엔지니어의 포럼 답변으로, 준공식으로 취급한다. **[저자]**는 크레이트 저자 문서다. **추정**은 근거 없는 추론이다.

---

## 0. 트랙 질문 (1차 보고서를 열기 전에 정함)

1. 상용 품질의 Rust 데몬 + GUI에 **기계로 강제할 수 있는** 공식 관례는 무엇인가? 각 관례의 강제 수단(rustfmt, clippy, rustc lint, cargo-deny, CI, 리뷰)은 무엇인가?
2. unsafe FFI(openpty, SCM_RIGHTS, fork/exec, launchd, objc2)를 공식 문서 기준으로 어떻게 가두고 검증하는가?
3. panic, abort, catch_unwind가 "세션 손실 0"과 어떻게 맞물리는가?
4. 공급망 설정의 2026-10 현재 실제 키는 무엇인가? 이 맥의 도구에서 동작하는가?
5. Tauri v2 주의사항 중 옮길 것, 해당 없는 것, Tauri가 없어서 생기는 공백은 무엇인가?
6. Apple 규칙 중 코드와 번들 구조에 영향을 주는 것은 무엇인가? 특히 launchd와 TCC.
7. gpui 앱 구조 관례는 무엇이고, 실제로 쓰는 스냅샷에서 함정은 무엇인가?
8. (빈 관점) 무인 야간 개발에서 에이전트가 지키기 쉬운 규칙, 즉 실패하면 기계가 막는 규칙은 무엇인가?

---

## 1. 이전 조사(C1-A) 정정

1차 보고서의 인용 대부분(API Guidelines, Book, std, Nomicon, Reference, UCG, edition guide, Miri, Cargo book, Tauri, Apple)은 원문과 일치했다. 아래 표는 **틀렸거나, 부분적으로만 맞거나, 미확인을 이번에 해소한 것**만 모았다.

### 1.1 틀림·부분 정정

| # | 주장 (C1-A 줄) | 실제 | 근거 |
|---|---|---|---|
| 1 | rustfmt stable 옵션 29개 목록 (L79-85) | 개수 29는 맞다. 하지만 나열된 이름은 28개이고 `fn_args_layout`(Stable: Yes, deprecated, `fn_params_layout`로 이름이 바뀜)이 빠졌다. 문서의 기본 브랜치는 `master`가 아니라 `main`이다(302 리다이렉트) | https://github.com/rust-lang/rustfmt/blob/main/Configurations.md |
| 2 | "unstable 옵션은 쓸 수 없다" (L86-90) | 보강: stable에서 unstable 옵션을 넣으면 **경고만 찍고 무시하며 exit 0**으로 끝난다. 즉 잘못된 rustfmt.toml이 CI를 조용히 통과한다 | [실측] `imports_granularity = "Crate"` 넣고 `cargo fmt --all --check` 실행 → "Warning: can't set … only available in nightly channel", exit 0 |
| 3 | release 기본값 `strip="none"` (L164) | 문서 표의 값은 맞다. 다만 Cargo 1.77부터 "Disabling debuginfo now implies strip = "debuginfo" (when strip is not set)"이다. 그래서 release 기본 빌드에서는 debuginfo가 제거된다 | https://doc.rust-lang.org/cargo/reference/profiles.html#strip , https://doc.rust-lang.org/cargo/CHANGELOG.html |
| 4 | `-Dwarnings`가 캐시를 무효화한다는 문장의 출처가 cargo config 문서 (L110) | cargo config 문서에는 그 문장이 없다. 출처는 clippy usage다: "this will invalidate build caches and is therefore not recommended" | https://doc.rust-lang.org/clippy/usage.html#elevating-warning-to-errors |
| 5 | 규칙 #1: resolver 누락 경고를 `build.warnings=deny`로 막는다 | **틀림.** "Only warnings that are lints … are affected". [실측] resolver 줄을 지우고 `CARGO_BUILD_WARNINGS=deny cargo check`를 돌리면 경고만 나오고 **exit 0**이다 | https://doc.rust-lang.org/cargo/reference/config.html#buildwarnings |
| 6 | `lint_groups_priority`가 priority 누락을 잡아준다 (L106, 규칙 #7) | 부분: "Known problems: Does not check lints inherited using `lints.workspace = true`"라고 적혀 있다. 워크스페이스 상속 구조에서는 이 lint를 믿을 수 없다 | https://rust-lang.github.io/rust-clippy/rust-1.98.0/index.html#lint_groups_priority |
| 7 | clippy.toml `msrv` 기본값은 Cargo.toml rust-version (L131) | 부분: 설명은 "Defaults to the rust-version field in Cargo.toml"이지만 표의 Default Value는 "current version"이다 | https://doc.rust-lang.org/clippy/lint_configuration.html#msrv |
| 8 | `disallowed-methods` 필드는 `path`와 `reason` (L133) | 부분: `replacement`와 `allow-invalid`(기본 false)도 있다. [실측] 철자가 틀린 std 경로는 "add `allow-invalid = true`…" 경고를 낸다. 반면 **의존성에 없는 크레이트 경로**(예: `libc::fork`)는 경고 없이 무시된다 | 같은 문서, [실측] |
| 9 | `missing_safety_doc`가 "모든 `unsafe fn`"을 강제한다 (규칙 #10) | 부분: 기본은 공개 항목만 검사한다. `check-private-items = true`로 넓힐 수 있다. 하지만 [실측] 이 키는 `missing_panics_doc`/`missing_errors_doc`도 비공개 항목으로 넓힌다. 그래서 **테스트 함수의 `assert_eq!`까지 `# Panics` 문서를 요구하며 실패한다**. 워크스페이스 전역에는 켜지 않는다. 공개 unsafe trait은 기본으로 잡힌다([실측] "docs for unsafe trait missing `# Safety` section") | https://rust-lang.github.io/rust-clippy/rust-1.98.0/index.html#missing_safety_doc , [실측] |
| 10 | `extern "C"` panic이 abort된다는 문장이 catch_unwind `#notes`에 있다 (L179) | 문장은 catch_unwind 페이지 **본문**에 있다(`#notes` 앵커 아래가 아님) | https://doc.rust-lang.org/std/panic/fn.catch_unwind.html |
| 11 | rustdoc 전용 lint 설명 옆에 둔 missing_docs (L67) | missing_docs는 예외로 rustc에서도 동작한다: "except for missing_docs, these lints are only available when running rustdoc" | https://doc.rust-lang.org/rustdoc/lints.html |
| 12 | 규칙 #25 "main.rs를 얇게 → 통합 테스트 대상" | 근거 보강: cargo-test 페이지는 doctest만 다룬다. 통합 테스트 근거는 Book ch11-03: "If our project is a binary crate that only contains a src/main.rs file … we can't create integration tests" | https://doc.rust-lang.org/book/ch11-03-test-organization.html |
| 13 | 규칙 #26 `fn_params_excessive_bools`가 bool 인자를 막는다 | 부분: 기본 `max-fn-params-bools = 3`이라 bool 1~3개는 잡지 않는다. [실측] 0으로 두면 1개부터 잡는다 | clippy lint_configuration, [실측] |
| 14 | cargo-deny `--locked` 사용 (규칙 #36) | **전역 옵션**이다. `cargo deny --locked check`는 동작하고, `cargo deny check --locked`는 "unexpected argument" 오류가 난다. 현재 check.sh는 올바른 순서다 | [실측] `cargo deny --help`, `cargo deny check --help` |
| 15 | gpui-kit의 caret 고정 경고가 README에 있다 (L408) | 그 문구는 **gpui-kit `Cargo.toml` 63-69행 주석**에 있다. 이어지는 문장: "Bump every snapshot crate together with its Cargo.lock entry; `script/check-gpui-pin.ts` fails CI when a pin is not exact." | https://github.com/longbridge/gpui-kit/blob/67265a36e260ffe03c5aaebdcd8d654dbf892eab/Cargo.toml |
| 16 | gpui 테스트 인용 2개(single-thread executor, 외부 대기 시 panic)가 `src/app/test_context.rs`에 있다 (L433-434) | 두 인용 모두 **`crates/gpui/examples/testing.rs` 283-284행, 308-313행**에 있다. `run_until_parked`는 test_context.rs 595행, `allow_parking`은 src/executor.rs 222행이다 | zed@93d745668805029476e6d7fbb19652116fed51de |
| 17 | `SEED`는 executor.rs `simulate_random_delay`에서 읽는다 (L435) | `SEED`와 `ITERATIONS` 환경변수는 **src/seeds.rs 6-24행**에서 읽는다 | 같은 SHA |
| 18 | gpui-pre 설명 "gpui-pre snapshot of zed@93d7456" (L265) | 전문은 "Zed's GPU-accelerated UI framework (gpui-pre snapshot of zed@93d7456)"이다 | https://crates.io/api/v1/crates/gpui-pre |
| 19 | 헬퍼 서명 식별자를 "bundle ID와 맞춘다" (L363) | 헬퍼 **자신의** bundle ID와 맞춘다는 뜻이고, 그 값은 앱 ID에 접미사를 붙인 별개 값이다(예: `com.example.apple-samplecode.AppWithTool.ToolX`). 앱과 같은 identifier를 쓰라는 근거가 아니다 | https://developer.apple.com/documentation/xcode/embedding-a-helper-tool-in-a-sandboxed-app |
| 20 | launchd.plist EXPECTATIONS에서 stdio를 /dev/null로 돌리는 것이 "금지" (L373, 규칙 #47) | MUST NOT은 daemon(3) 호출과 fork 후 부모 exit 두 가지뿐이다. stdio 리다이렉트 등은 "should not … as launchd will always implicitly perform them"으로, 금지가 아니라 불필요하다는 뜻이다 | [실측] `man launchd.plist`(macOS 27) |
| 21 | TN3179의 귀속 규칙을 TCC 전반으로 확장 (L385-390) | TN3179는 **Local Network 권한 전용** 문서다. 마이크·카메라 등으로 넓힌 것은 추론이다. 다만 아래 §4-1의 DTS 답변들이 같은 방향을 지지한다 | https://developer.apple.com/documentation/technotes/tn3179-understanding-local-network-privacy |
| 22 | 규칙 #49 경로 `~/Library/Application Support/minikrill` | Apple 관례는 "should be put in a subdirectory whose name matches the bundle identifier"다 | https://developer.apple.com/library/archive/documentation/FileManagement/Conceptual/FileSystemProgrammingGuide/MacOSXDirectories/MacOSXDirectories.html |
| 23 | Tauri updater "TLS is enforced in production mode" (L300) | 인용은 맞다. 다만 같은 문서에 HTTPS가 아닌 주소를 허용하는 `dangerousInsecureTransportProtocol` 옵션이 있다. 그래서 Minikrill 규칙은 "끄는 플래그 자체를 두지 않는다"여야 한다 | https://v2.tauri.app/plugin/updater/ |
| 24 | Tauri #9950, #14360 제목 (L308) | 실제 제목은 "[bug] nsis installer won't stop spawned sidecar when upgrading"(CLOSED 2026-09-15)과 "[feat] Add kill tree for sidecar"(CLOSED 2026-09-18)다. 손자 프로세스 생존 내용은 #14360 본문에 있다 | `gh issue view` |
| 25 | cargo-audit의 "lock 없으면 cargo update" → "엉뚱한 그래프 검사" (L242) | 인용은 맞다. README의 요지는 **보안**("Only run it on projects you trust… pass its lockfile with `--file`")이고, "엉뚱한 그래프"는 추론이다 | https://github.com/rustsec/rustsec/blob/main/cargo-audit/README.md |
| 26 | cargo-deny advisories의 삭제 키 (L248) | 맞다. 다만 문서가 `unsound`를 Removed로 적으면서 아래에서는 현행 키로 설명해 스스로 모순이다. [실측] 0.20.2는 `unsound = "all"`을 받아들인다. 삭제 키 `vulnerability`를 넣으면 `error[deprecated]: this key has been removed`가 난다 | https://embarkstudios.github.io/cargo-deny/checks/advisories/cfg.html , [실측] |

### 1.2 1차 "미확인"을 이번에 해소한 것

| 1차 미확인 | 결과 | 근거 |
|---|---|---|
| clippy.toml 크레이트별 조회 | 확인됨. `CLIPPY_CONF_DIR` → `CARGO_MANIFEST_DIR` → 현재 디렉터리 순으로 찾고, 없으면 상위로 올라간다. **[실측] 크레이트 디렉터리의 clippy.toml은 루트 파일과 병합되지 않고 통째로 대체한다.** 루트의 `disallowed-methods`가 사라졌다. 같은 페이지에 "The configuration file is unstable and may be deprecated in the future" 경고가 있다 | https://doc.rust-lang.org/clippy/configuration.html , [실측] |
| 사용자 정의 프로필 `inherits` | 확인됨: "you must specify the inherits key". [실측] `[profile.release-daemon] inherits="release"`, `panic="abort"`로 `cargo build -p krilld --profile release-daemon`이 성공하고 `target/release-daemon/`에 결과가 생긴다 | https://doc.rust-lang.org/cargo/reference/profiles.html#custom-profiles |
| `OwnedFd`/`BorrowedFd` 공식 문장 | OwnedFd: "This closes the file descriptor on drop. It is guaranteed that nobody else will close the file descriptor." BorrowedFd: "For the duration of that lifetime, it is guaranteed that nobody will close the file descriptor." | https://doc.rust-lang.org/std/os/fd/struct.OwnedFd.html , https://doc.rust-lang.org/std/os/fd/struct.BorrowedFd.html |
| `bans.workspace-dependencies` | 존재한다. 키는 `duplicates`, `unused`, `include-path-dependencies`. [실측] 두 멤버가 같은 크레이트를 직접 선언하면 `error[workspace-duplicate]`, 쓰이지 않는 workspace 의존성은 `error[unused-workspace-dependency]`가 난다. **AGENTS "의존성 버전은 여기서만"을 기계로 강제할 수 있다** | https://embarkstudios.github.io/cargo-deny/checks/bans/cfg.html , [실측] |
| `bans.deny`의 `wrappers` | 존재한다: `deny = [{ crate = "…", wrappers = ["…"] }]` | 같은 문서 |
| `[advisories] version = 2` 필요 여부 | 불필요("no longer used"). [실측] 넣어도 오류는 없다 | advisories 문서, [실측] |
| cargo-audit `--deny` 문법 | `-D, --deny <warnings\|unmaintained\|unsound\|yanked>`, 반복 지정 가능 | https://github.com/rustsec/rustsec/blob/main/cargo-audit/src/commands/audit.rs |
| gpui `Task`·`Subscription`의 `#[must_use]` | 둘 다 붙어 있다: `crates/scheduler/src/executor.rs` 401행, `crates/gpui/src/subscription.rs` 149행. [실측] 같은 모양의 타입으로 시험한 결과는 이렇다. `let _ = spawn()`은 기본 lint `let_underscore_future`가 잡고, `spawn();`은 rustc `unused_must_use`가 잡는다. **`let _t = spawn();`(쓰지 않는 이름 바인딩)은 아무 lint도 잡지 않는다** | zed@93d7456, [실측] |
| DTS 129980 원문 | 확인됨. 공식 문서에도 "Don't pass the --deep option to codesign when you sign code.", "Sign code from the inside out."가 있다 | https://developer.apple.com/documentation/xcode/creating-distribution-signed-code-for-the-mac |
| hardened runtime에서 리소스 엔타이틀먼트가 없을 때 | [DTS] "the hardened runtime prevents accidental access to certain resources, like the microphone. Rather, you have to tell macOS that your app expects to use those resources." 엔타이틀먼트 페이지 자체에는 이런 문장이 없다 | https://developer.apple.com/forums/thread/835256 |
| macOS sanitizer | nightly 전용(`-Zsanitizer=address/thread`). aarch64-apple-darwin과 x86_64-apple-darwin을 지원한다. RELEASES.md에 안정화 기록이 없다 | https://doc.rust-lang.org/nightly/unstable-book/compiler-flags/sanitizer.html |
| 2027 edition | 2026-10-14 기준 공식 발표가 없다. rustc의 `LATEST_STABLE_EDITION`은 2024이고, rustfmt `style_edition = "2027"`은 unstable이다([실측] stable에서 "unstable variants are only available in nightly channel") | https://github.com/rust-lang/rust/blob/master/compiler/rustc_span/src/edition.rs , https://blog.rust-lang.org/ |
| Tauri 업데이터 서명 방식 | 문서에는 없고, 소스 `plugins/updater/Cargo.toml`에 `minisign-verify = "0.3"`이 있다 | https://github.com/tauri-apps/plugins-workspace |

### 1.3 1차에서 맞다고 확인된 주요 사실 (재사용 가능)

- **[실측] 1차 실측 3건을 재현했다.**
  - `[lints] workspace = true`에 개별 lint를 덧붙이면 "cannot override `workspace.lints` in `lints`" 오류가 난다.
  - `[profile.release.package.X] panic="abort"`는 "`panic` may not be specified in a `package` profile" 오류가 난다.
  - 가상 워크스페이스에서 resolver를 생략하면 "virtual workspace defaulting to `resolver = "1"`" 경고가 난다.
- **`build.warnings` 확인.** Cargo 1.97(2026-07-09)에서 안정화됐다. clippy usage에 "Since Cargo 1.97, the recommended way…"가 실제로 있다. [실측] 1.98.1에서 `CARGO_BUILD_WARNINGS=deny cargo clippy`는 "warnings are denied by `build.warnings` configuration"과 함께 exit 101로 끝난다.
- **default-features 덮어쓰기.** `[workspace.dependencies]`에 둔 default-features를 패키지에서 덮어쓰려면 Rust 1.99+가 필요하다. 근거: https://doc.rust-lang.org/cargo/reference/workspaces.html#the-dependencies-table
- **panic 설정과 테스트.** "Tests, benchmarks, build scripts, and proc macros ignore the panic setting." 테스트를 abort로 빌드하면 의존성은 unwind로 강제된다(profiles#panic).
- **gpui-pre 상태(2026-10-12 발행).** gpui-pre 0.3.9와 gpui-component 0.7.3이 최신이다. 둘 다 crates.io 소유자는 `huacnlee` 1인이다. 0.3.9의 `[package.metadata.gpui-pre]`는 `zed-version="0.2.2"`, `zed-rev="93d745668805029476e6d7fbb19652116fed51de"`다. 공식 `gpui`는 0.2.2(2025-10-22)가 마지막이다.
- **Zed 라이선스(@93d7456).** `crates/terminal`과 `crates/terminal_view`는 `GPL-3.0-or-later`, `crates/gpui`는 `Apache-2.0`이다.

---

## 2. 확정 규칙

강도: **MUST**는 위반 시 병합 차단(가능하면 기계 차단), **SHOULD**는 예외 시 `reason` 또는 PR 설명에 사유를 남김, **MAY**는 선택이다. "기계"는 `scripts/check.sh`가 자동으로 막는다는 뜻이다. 야간 무인 개발에서 에이전트가 어기면 바로 실패하는 규칙이다. "리뷰"는 사람이나 지휘자가 확인해야 한다.

### 2.1 워크스페이스·툴체인

| # | 규칙 | 강제 수단 | 근거 URL | 강도 |
|---|---|---|---|---|
| W1 | 모든 크레이트는 `edition = "2024"`, `rust-version`, `version`을 `[workspace.package]`에서 `.workspace = true`로 상속한다 | Cargo + 리뷰 | https://doc.rust-lang.org/cargo/reference/workspaces.html#the-package-table | MUST |
| W2 | 가상 워크스페이스 루트에 `resolver = "3"`을 명시한다. 이 경고는 `build.warnings`로 막히지 않는다. 그래서 check.sh에 `cargo metadata` 경고 검사를 추가할 것을 제안한다(§4) | Cargo + (제안) check.sh | https://doc.rust-lang.org/cargo/reference/workspaces.html#virtual-workspace , https://doc.rust-lang.org/cargo/reference/resolver.html#resolver-versions | MUST |
| W3 | 외부 의존성 버전은 루트 `[workspace.dependencies]`에만 쓴다. 멤버는 `x = { workspace = true }`로 쓴다 | **cargo-deny `bans.workspace-dependencies`(기계, [실측])** | https://embarkstudios.github.io/cargo-deny/checks/bans/cfg.html | MUST |
| W4 | 패키지 레벨 `default-features = false` 덮어쓰기에 기대지 않는다(1.99+ 전용, 로컬은 1.98.1). 기능을 줄여야 하는 의존성은 workspace 쪽에서 `default-features = false`로 선언한다 | 리뷰 | https://doc.rust-lang.org/cargo/reference/workspaces.html#the-dependencies-table | MUST |
| W5 | MSRV는 `rust-version = "1.98"`로 고정한다(rustup이 없어 `rust-toolchain.toml`은 로컬에서 효과가 없다). CI를 도입하면 MSRV와 최신 stable 둘 다 빌드한다 | Cargo(미달 툴체인이면 오류) + CI | https://doc.rust-lang.org/cargo/reference/rust-version.html | MUST |
| W6 | 모든 cargo 명령에 `--locked`를 붙인다. `Cargo.lock`을 커밋한다(바이너리 배포, 결정적 빌드) | check.sh(이미 적용) | https://doc.rust-lang.org/cargo/faq.html#why-have-cargolock-in-version-control , https://doc.rust-lang.org/cargo/commands/cargo-build.html | MUST |
| W7 | 프로필은 루트 Cargo.toml에만 둔다. 패키지별 `panic`·`lto` override는 불가능하므로 시도하지 않는다 | Cargo(오류) | https://doc.rust-lang.org/cargo/reference/profiles.html#overrides | MUST |

### 2.2 포맷·lint

| # | 규칙 | 강제 수단 | 근거 URL | 강도 |
|---|---|---|---|---|
| L1 | `rustfmt.toml`은 `style_edition = "2024"` 한 줄만 둔다. 이유: `rustfmt`를 직접 부르면 기본값이 2015다([실측] `rustfmt --print-config default` → `style_edition = "2015"`). unstable 옵션(`imports_granularity`, `group_imports`, `wrap_comments` 등)은 넣지 않는다. 넣어도 stable에서는 **조용히 무시**된다 | `cargo fmt --all --check` | https://github.com/rust-lang/rustfmt/blob/main/Configurations.md#style_edition | MUST |
| L2 | lint 정책은 루트 `[workspace.lints]`에만 둔다. 모든 멤버는 `[lints] workspace = true`를 쓴다. 크레이트별로 **더 엄격하게** 할 때는 크레이트 루트 `#![deny(...)]` 속성을 쓴다(`[lints]` 부분 덮어쓰기는 Cargo 오류) | Cargo + clippy | https://doc.rust-lang.org/cargo/reference/workspaces.html#the-lints-table , [실측] | MUST |
| L3 | 그룹: `clippy::all = deny`, `clippy::pedantic = warn`(둘 다 `priority = -1`). pedantic 중 `must_use_candidate`만 allow(빈 스켈레톤에서도 실패해 노이즈가 크다, [실측]). nursery는 켜지 않는다. restriction은 그룹으로 켜지 않고 골라서 켠다. priority 누락은 `lint_groups_priority`가 workspace 상속에서 잡지 못하므로 리뷰로 본다 | clippy | https://doc.rust-lang.org/clippy/usage.html#clippypedantic , https://doc.rust-lang.org/clippy/usage.html#clippyrestriction | MUST |
| L4 | lint 억제는 `#[expect(lint, reason = "…")]`만 쓴다. `#[allow]`는 금지다. `expect`는 더는 필요 없어지면 `unfulfilled_lint_expectations`로 실패하므로 낡은 예외가 남지 않는다([실측]) | clippy `allow_attributes`, `allow_attributes_without_reason`(deny) | https://rust-lang.github.io/rust-clippy/rust-1.98.0/index.html#allow_attributes | MUST |
| L5 | 경고를 오류로 만드는 방식은 Clippy가 권하는 `CARGO_BUILD_WARNINGS=deny`(Cargo 1.97+)로 옮길 것을 제안한다. 현재 check.sh는 `-- -D warnings`를 쓰는데 이 방식은 캐시를 무효화한다 | check.sh(사람 승인 필요) | https://doc.rust-lang.org/clippy/usage.html#elevating-warning-to-errors , https://doc.rust-lang.org/cargo/reference/config.html#buildwarnings | SHOULD |
| L6 | clippy.toml은 **루트 하나만** 둔다. 크레이트 디렉터리에 두면 루트 설정이 병합되지 않고 통째로 대체된다([실측]). `disallowed-*` 항목을 추가할 때는 철자를 [실측] 테스트로 확인한다. 의존성에 없는 경로는 경고 없이 무시되기 때문이다 | 리뷰 | https://doc.rust-lang.org/clippy/configuration.html | MUST |

### 2.3 unsafe·FFI

| # | 규칙 | 강제 수단 | 근거 URL | 강도 |
|---|---|---|---|---|
| U1 | 워크스페이스 기본은 `unsafe_code = "deny"`다. FFI 모듈(mk-holder·krilld의 `sys`, mk-app의 `platform`/objc2)만 모듈 첫 줄에 `#![expect(unsafe_code, reason = "…")]`를 둔다. `mk-proto`, `mk-term`, `mk`은 크레이트 루트에 `#![forbid(unsafe_code)]`를 둔다. [실측] 워크스페이스가 deny이고 모듈이 `expect`면 그 모듈 안에서만 허용되고, 밖의 unsafe 블록은 오류가 난다 | rustc `unsafe_code` | https://doc.rust-lang.org/cargo/reference/workspaces.html#the-lints-table (예시가 `unsafe_code = "forbid"`) | MUST |
| U2 | 모든 `unsafe` 블록 바로 위에 `// SAFETY:` 주석을 단다. 블록 하나에는 unsafe 연산 하나만 둔다. unsafe가 아닌 문장에 SAFETY 주석을 달지 않는다 | clippy `undocumented_unsafe_blocks`, `multiple_unsafe_ops_per_block`(deny), `unnecessary_safety_comment`(warn) | https://std-dev-guide.rust-lang.org/policy/safety-comments.html , https://rust-lang.github.io/rust-clippy/rust-1.98.0/index.html#undocumented_unsafe_blocks | MUST |
| U3 | `unsafe fn`과 `unsafe trait`에는 `# Safety` 문서 섹션을 단다. 공개 항목은 기계가 잡는다. 비공개 `unsafe fn`은 리뷰로 본다. `check-private-items`를 켜면 테스트까지 `# Panics`를 요구하므로 켜지 않는다 | clippy `missing_safety_doc`(style, `all=deny`로 오류) + 리뷰 | https://rust-lang.github.io/api-guidelines/documentation.html#c-failure | MUST |
| U4 | `unsafe_op_in_unsafe_fn = "deny"`. edition 2024 기본값인 warn보다 한 단계 올린다 | rustc lint | https://doc.rust-lang.org/edition-guide/rust-2024/unsafe-op-in-unsafe-fn.html | MUST |
| U5 | fd는 `OwnedFd`/`BorrowedFd`로만 주고받는다. "a safe function that takes a regular integer, treats it as a file descriptor, and acts on it, is unsound". `FromRawFd`는 "must be an owned file descriptor"라는 안전 계약을 SAFETY 주석에 적는다 | 리뷰 | https://doc.rust-lang.org/std/io/index.html , https://doc.rust-lang.org/std/os/fd/trait.FromRawFd.html | MUST |
| U6 | 안전 래퍼는 건전해야 한다. 안전 코드로 어떻게 호출해도 UB가 나면 안 된다. raw pointer를 쥔 타입의 `Send`/`Sync`는 근거 주석과 함께 명시한다 | 리뷰 + Miri(가능한 범위) | https://rust-lang.github.io/unsafe-code-guidelines/glossary.html#soundness-of-code--of-a-library , https://rust-lang.github.io/api-guidelines/interoperability.html#c-send-sync | MUST |
| U7 | fork와 exec 사이(`pre_exec`)에서는 async-signal-safe 호출만 한다. 할당, `std::env`, 락, 로깅, `io::Error::new`/`other`는 금지다. macOS는 "All APIs … should be assumed to be unsafe after a fork() … you must exec"이라고 한다. `CommandExt::setsid`는 아직 nightly 전용이라 stable에서는 libc `setsid`를 pre_exec 안에서 부른다 | 리뷰(mk-holder) | https://doc.rust-lang.org/std/os/unix/process/trait.CommandExt.html#tymethod.pre_exec , https://keith.github.io/xcode-man-pages/fork.2.html , https://pubs.opengroup.org/onlinepubs/9799919799/functions/fork.html | MUST |
| U8 | `std::env::set_var`와 `remove_var`는 금지다. 자식 환경은 `Command::env`/`env_remove`/`env_clear`로 넘긴다 | clippy `disallowed-methods`(clippy.toml, [실측] 탐지 확인) | https://doc.rust-lang.org/std/env/fn.set_var.html , https://doc.rust-lang.org/edition-guide/rust-2024/newly-unsafe-functions.html | MUST |
| U9 | `static mut`는 금지다. `OnceLock`, `Atomic*`, `Mutex`를 쓴다 | rustc `static_mut_refs`(2024 기본 deny) | https://doc.rust-lang.org/edition-guide/rust-2024/static-mut-references.html | MUST |
| U10 | C나 objc2로 넘기는 콜백 본문에서 panic이 밖으로 나가지 않게 한다. 실패는 반환값으로 표현한다. `-unwind` ABI는 쓰지 않는다. `extern "C"`에서 panic이 나면 프로세스가 abort되고, 잘못된 ABI로 unwind하면 UB다 | 리뷰 | https://doc.rust-lang.org/std/panic/fn.catch_unwind.html , https://doc.rust-lang.org/nomicon/ffi.html#ffi-and-unwinding , https://doc.rust-lang.org/reference/panic.html#r-panic.unwind.ffi.undefined | MUST |
| U11 | Miri는 syscall이 없는 순수 로직(프레임 코덱, 링, cmsg 버퍼 구성)에만 쓴다. FFI 테스트는 `#[cfg_attr(miri, ignore)]`로 뺀다. Miri와 sanitizer는 nightly 전용이라 이 맥에서는 돌릴 수 없다. CI의 nightly 잡으로만 돌린다 | CI(nightly) | https://github.com/rust-lang/miri/blob/master/README.md , https://doc.rust-lang.org/nightly/unstable-book/compiler-flags/sanitizer.html | SHOULD |

### 2.4 panic·에러·데몬 견고성

| # | 규칙 | 강제 수단 | 근거 URL | 강도 |
|---|---|---|---|---|
| P1 | 비테스트 코드에서 `unwrap`, `expect`, `panic!`, `todo!`, `unimplemented!`, `dbg!`, `mem::forget`, main 밖의 `process::exit`는 금지다. 불변식이 증명된 곳만 `#[expect(clippy::expect_used, reason = "불변식: …")]`로 허용한다. 테스트는 clippy.toml `allow-*-in-tests`로 허용한다 | clippy restriction 골라 켜기(deny) + clippy.toml, [실측] | https://rust-lang.github.io/rust-clippy/rust-1.98.0/index.html#unwrap_used , https://doc.rust-lang.org/clippy/lint_configuration.html | MUST |
| P2 | `mk-proto`(와이어 파서)는 크레이트 루트에 `#![deny(clippy::indexing_slicing, clippy::arithmetic_side_effects, clippy::as_conversions)]`를 둔다. `mk-holder`와 `krilld`는 `#![deny(clippy::indexing_slicing, clippy::print_stdout, clippy::print_stderr)]`를 둔다. mk-app과 mk CLI는 화면 좌표 계산과 사용자 출력이 많아 제외한다 | 크레이트 속성([실측] check.sh 통과) | https://rust-lang.github.io/rust-clippy/rust-1.98.0/index.html#indexing_slicing , #arithmetic_side_effects | MUST |
| P3 | 사람이 읽는 텍스트(한글 포함)는 바이트 범위로 자르지 않는다(`&s[a..b]` 금지). `char_indices`나 grapheme 단위로 다룬다. 바이트 경계 panic을 막기 위해서다 | clippy `string_slice`(warn → check.sh에서 오류) | https://rust-lang.github.io/rust-clippy/rust-1.98.0/index.html#string_slice | MUST |
| P4 | **panic 전략은 사람이 결정한다(§4-4).** 선택지 A: krilld를 `release-daemon` 프로필(`panic="abort"`)로 따로 빌드하고 holder가 재기동한다. 선택지 B: unwind를 유지하고 세션 태스크마다 `catch_unwind`로 격리한다. A에서는 세션 단위 격리가 불가능하다("only catches unwinding panics") | Cargo 프로필([실측] 동작) + e2e | https://doc.rust-lang.org/cargo/reference/profiles.html#panic , https://doc.rust-lang.org/std/panic/fn.catch_unwind.html | MUST(결정) |
| P5 | panic 경로는 `cargo test`가 아니라 release 바이너리 e2e(패닉 주입, `kill -9`)로 검증한다. 테스트는 panic 설정을 무시한다 | e2e | https://doc.rust-lang.org/cargo/reference/profiles.html#panic | MUST |
| P6 | 크래시 기록은 panic hook 안에서 버퍼 없이 바로 쓰고 닫는다. hook은 abort 런타임에서도 돈다. `process::abort`는 hook을 부르지 않고 BufWriter도 비우지 않는다. SIGKILL은 잡을 수 없다. 그래서 디스크 세그먼트는 **잘린 꼬리를 버리고 복구할 수 있는 형식**이어야 한다 | 리뷰 + e2e | https://doc.rust-lang.org/std/panic/fn.set_hook.html , https://doc.rust-lang.org/std/process/fn.abort.html | MUST |
| P7 | 와이어 길이와 오프셋 계산은 `checked_*`/`try_from`으로 한다. release 프로필에 `overflow-checks = true`를 켠다(release 기본값은 false라 조용히 wrap된다). 성능 영향은 미측정이다 | clippy(P2) + Cargo 프로필 | https://doc.rust-lang.org/cargo/reference/profiles.html#overflow-checks | SHOULD |
| P8 | 라이브러리와 데몬 내부 오류는 `thiserror` enum으로 정의한다. 조건은 `Error + Send + Sync + 'static`, 메시지는 "lowercase without trailing punctuation", `()` 에러 금지다. `anyhow`는 `mk` CLI `main`과 앱 최상위에서만 쓴다. 공식 문서에는 thiserror/anyhow 권고가 없고 근거는 [저자] 문서다 | 리뷰 | https://rust-lang.github.io/api-guidelines/interoperability.html#c-good-err , https://github.com/dtolnay/thiserror#comparison-to-anyhow | SHOULD |
| P9 | 공개 함수에는 `# Errors`와 `# Panics` 섹션을 단다 | clippy `missing_errors_doc`, `missing_panics_doc`(pedantic) | https://rust-lang.github.io/api-guidelines/documentation.html#c-failure | SHOULD |
| P10 | `main`은 `ExitCode`나 `Result`를 반환한다. `process::exit`는 쓰지 않는다 | clippy `exit`(deny) | https://doc.rust-lang.org/std/process/fn.exit.html | SHOULD |

### 2.5 타입·문서·구조

| # | 규칙 | 강제 수단 | 근거 URL | 강도 |
|---|---|---|---|---|
| T1 | ID와 역할은 뉴타입·enum으로 만든다(`SessionId`, `Seq`, `ClientRole`). 의미가 있는 `bool`·`u64` 인자는 쓰지 않는다. 와이어 `kind: u8`는 `#[repr(u8)]` enum + `TryFrom<u8>`로 다룬다 | 리뷰(+ `fn_params_excessive_bools`, 필요하면 `max-fn-params-bools = 0`) | https://rust-lang.github.io/api-guidelines/type-safety.html#c-newtype , #c-custom-type | SHOULD |
| T2 | 공개 타입은 `Debug`를 구현한다. fd나 비밀을 담은 타입은 수동으로 구현해 마스킹한다 | rustc `missing_debug_implementations`(warn → check.sh에서 오류) | https://rust-lang.github.io/api-guidelines/debuggability.html#c-debug | SHOULD |
| T3 | 네이밍: `as_`/`to_`/`into_` 규약을 지키고, `get_` 접두어는 쓰지 않는다. 약어도 한 단어로 쓴다(`Pty`, `Uds`) | clippy `wrong_self_convention`(style) + 리뷰 | https://rust-lang.github.io/api-guidelines/naming.html | SHOULD |
| T4 | 바이너리 크레이트는 `main.rs`를 얇게 두고 로직은 `lib.rs`에 둔다. doctest와 통합 테스트는 lib 타깃만 대상이 되기 때문이다 | 리뷰 | https://doc.rust-lang.org/cargo/commands/cargo-test.html#documentation-tests , https://doc.rust-lang.org/book/ch11-03-test-organization.html | SHOULD |
| T5 | `mk-proto`와 `mk-term`은 gpui, tokio, nix, objc2에 의존하지 않는다 | cargo-deny `bans.deny` + `wrappers`(허용할 크레이트 지정) 또는 리뷰 | https://embarkstudios.github.io/cargo-deny/checks/bans/cfg.html | SHOULD |

### 2.6 공급망

| # | 규칙 | 강제 수단 | 근거 URL | 강도 |
|---|---|---|---|---|
| S1 | `cargo deny --locked check`(advisories, licenses, bans, sources)를 게이트로 쓴다. `--locked`는 `check` **앞**에 둔다 | check.sh(이미 있음, **deny.toml 필요**) | https://embarkstudios.github.io/cargo-deny/ , [실측] | MUST |
| S2 | 라이선스는 allow 목록으로 관리한다(Apache-2.0, MIT, BSD-2/3, ISC, Unicode-3.0, Zlib). GPL 계열은 넣지 않는다. 목록에 없으면 기본 거부다. `publish = false`인 자체 크레이트는 `[licenses.private] ignore = true`로 통과시킨다 | cargo-deny([실측]: MIT를 빼면 `error[rejected]`) | https://embarkstudios.github.io/cargo-deny/checks/licenses/cfg.html | MUST |
| S3 | 취약점은 항상 오류다(키 없음). `yanked = "deny"`, `unsound = "all"`. advisory를 무시할 때는 `{ id, reason }` 형식으로 사유를 반드시 적는다. 삭제된 키(`vulnerability`, `notice`, `severity-threshold`, `unlicensed`, `copyleft`)는 쓰지 않는다([실측] 삭제 키는 오류) | cargo-deny | https://embarkstudios.github.io/cargo-deny/checks/advisories/cfg.html | MUST |
| S4 | 소스는 crates.io만 허용한다. 알 수 없는 레지스트리와 git은 deny한다. git 의존성이 필요하면 `allow-git`에 넣고 `rev`로 고정한다 | cargo-deny `unknown-*="deny"`, `required-git-spec="rev"` | https://embarkstudios.github.io/cargo-deny/checks/sources/cfg.html , https://v2.tauri.app/security/lifecycle/ | MUST |
| S5 | 와일드카드 버전은 금지한다. 워크스페이스 path 의존성은 `allow-wildcard-paths = true`로 예외 처리한다([실측] 없으면 path 의존성 5곳이 `error[wildcard]`). 중복 버전은 warn으로 보고만 한다(gpui 트리를 deny로 걸면 통과 불가로 추정) | cargo-deny | https://embarkstudios.github.io/cargo-deny/checks/bans/cfg.html | MUST |
| S6 | `gpui-pre`, **모든 `gpui-pre-*`**, `gpui-component`는 `=` 정확 버전으로 고정하고 Cargo.lock 항목과 함께 **한 묶음으로** 올린다. vendor할 때는 `LICENSE-APACHE`를 보존하고, 패치한 파일에는 변경 사실을 표시한다(Apache-2.0 §4(b)). 업스트림 Zed와 gpui-pre에는 NOTICE 파일이 없다 | 리뷰 + (제안) 정확 고정 검사 | https://github.com/longbridge/gpui-kit/blob/67265a36e260ffe03c5aaebdcd8d654dbf892eab/Cargo.toml , https://www.apache.org/licenses/LICENSE-2.0.txt | MUST |
| S7 | Zed의 GPL 크레이트(`terminal`, `terminal_view`) 코드는 읽고 베끼지 않는다(클린룸). cargo-deny는 의존성만 막고, 복사는 막지 못한다 | 리뷰 + cargo-deny licenses | zed@93d7456 `crates/terminal/Cargo.toml` | MUST |
| S8 | 릴리스 바이너리는 `cargo auditable build`로 빌드하고, 출시 전 `cargo audit bin`을 돌린다(현재 이 맥에 미설치) | 릴리스 스크립트 | https://github.com/rust-secure-code/cargo-auditable , https://github.com/rustsec/rustsec/blob/main/cargo-audit/README.md | MAY |
| S9 | CI의 서드파티 액션은 커밋 SHA로 고정한다. 서명 키(Developer ID, 업데이트 ed25519)는 PR 빌드에서 접근할 수 없게 한다 | CI 리뷰 | https://v2.tauri.app/security/lifecycle/ | MUST |

### 2.7 IPC·업데이터 (Tauri에서 옮겨 온 원칙)

| # | 규칙 | 강제 수단 | 근거 URL | 강도 |
|---|---|---|---|---|
| I1 | UDS로 들어오는 모든 프레임은 같은 UID라도 신뢰하지 않는 입력으로 다룬다. 길이 상한(1MiB)을 먼저 검사하고, 알 수 없는 kind와 버전은 거부한다. 파서에는 panic 경로가 없어야 한다(P2) | 리뷰 + proptest/퍼징(MK-056) | https://v2.tauri.app/security/ , https://v2.tauri.app/concept/inter-process-communication/ | MUST |
| I2 | 명령별 호출 권한(app, cli, hook, observer)을 `mk-proto`에 선언적으로 둔다. 핸들러 진입 전 한 곳에서 검사하고 기본은 거부한다. 경로와 세션 범위 검증은 핸들러 안에서 한다(Tauri scope: "enforcing is implemented by the command itself") | 리뷰 + 단위 테스트 | https://v2.tauri.app/security/runtime-authority/ , https://v2.tauri.app/security/scope/ | SHOULD |
| I3 | 업데이터: 서명 검증을 끄는 경로나 플래그를 두지 않는다. Tauri에도 있는 `dangerousInsecureTransportProtocol` 같은 탈출구를 만들지 않는다. 다운그레이드는 거부하고 롤백은 로컬 백업으로만 한다. HTTPS만 쓴다. 서명은 매니페스트에 **내용**을 인라인으로 넣는다. 키를 잃으면 업데이트가 불가능하므로 키 보관·회전 절차를 문서화한다 | 리뷰 + 업데이터 테스트 | https://v2.tauri.app/plugin/updater/ | MUST |

### 2.8 Apple·launchd·번들

| # | 규칙 | 강제 수단 | 근거 URL | 강도 |
|---|---|---|---|---|
| A1 | launchd가 직접 띄우는 프로세스(mk-holder)는 `daemon(3)`을 호출하지 않고, fork 후 부모가 exit하지 않는다(MUST NOT). 자기 자신에게 `setsid`를 하지 않는다. 자식 셸의 setsid는 해당 없다. SIGTERM을 받으면 빠르게 정리하고 종료한다. 시작 후 10초 안에 종료하지 않는다. stdio 재지정 같은 초기화는 launchd가 하므로 하지 않는다 | 리뷰 + e2e(`launchctl bootout`) | `man launchd.plist`(EXPECTATIONS), https://developer.apple.com/library/archive/documentation/MacOSX/Conceptual/BPSystemStartup/Chapters/CreatingLaunchdJobs.html | MUST |
| A2 | 생성하는 plist에는 `AssociatedBundleIdentifiers = <앱 bundle id>`를 넣는다. `ExitTimeOut = 0`은 금지다. `RunAtLoad`(man: "should be avoided"; 우리는 on-demand bootstrap이라 추측 실행이 아님)와 `ProcessType=Interactive`("cannot be made Adaptive"일 때만)를 쓰는 이유는 생성 코드에 주석으로 남긴다. plist는 스냅샷 테스트로 고정한다 | 리뷰 + 스냅샷 테스트 | `man launchd.plist`, https://developer.apple.com/forums/thread/721918 (DTS) | MUST |
| A3 | 헬퍼(mk-holder, krilld, mk)는 앱과 **같은 팀**, **각자 고유한 identifier**(예: `<앱 id>.holder`)로 서명한다. 위치는 `Contents/MacOS/` 또는 `Contents/Helpers/`에 평평하게 둔다. 서명은 `-o runtime --timestamp`로 **안쪽부터 개별** 서명한다. 서명할 때 `--deep`은 금지다(검증할 때는 `codesign -vvv --deep --strict` 권장) | 릴리스 스크립트 + CI 검증 | https://developer.apple.com/documentation/xcode/creating-distribution-signed-code-for-the-mac , https://developer.apple.com/documentation/bundleresources/placing-content-in-a-bundle , https://developer.apple.com/forums/thread/732291 (DTS: "every program should have its own code signing identifier") | MUST |
| A4 | 엔타이틀먼트는 필요한 것만 넣는다. 값이 false인 엔타이틀먼트는 아예 넣지 않는다. `get-task-allow`가 없는지 CI에서 `codesign -d --entitlements :-`로 확인한다. 리소스 엔타이틀먼트(audio-input, camera, apple-events)가 필요한지는 MK-007에서 실측한다(§4-3) | CI 검증 | https://developer.apple.com/documentation/security/hardened-runtime , https://developer.apple.com/documentation/security/resolving-common-notarization-issues | MUST |
| A5 | 실행 중에는 번들에 아무것도 쓰지 않는다. 쓰면 서명 seal이 깨진다. 상태와 세그먼트는 `~/Library/Application Support/<bundle id>/`에 둔다. 재생성할 수 있는 캐시는 `~/Library/Caches/<bundle id>/`에 둔다 | 리뷰 + `codesign --verify --strict` e2e | https://developer.apple.com/documentation/xcode/embedding-nonstandard-code-structures-in-a-bundle , FileSystemProgrammingGuide(위 URL) | MUST |
| A6 | UDS 경로는 `sun_path` 104바이트 이하여야 한다. 경로를 만드는 함수에서 길이를 검사하고, 넘으면 오류를 반환한다(잘라 쓰지 않는다) | 단위 테스트 | [실측] SDK `sys/un.h` 79행 `char sun_path[104]`, `man 4 unix` | MUST |
| A7 | `mk` CLI는 단독으로 배포하지 않는다(단독 바이너리는 스테이플 불가). 앱 번들 안의 바이너리를 심링크로 노출한다 | 패키징 리뷰 | https://developer.apple.com/documentation/security/customizing-the-notarization-workflow | SHOULD |
| A8 | 로그에 명령줄, 경로, 터미널 내용 같은 동적 문자열을 공개로 남기지 않는다. os_log를 쓰면 기본 private를 유지하고, 파일 로그를 쓰면 리댁션한다 | 리뷰 | https://developer.apple.com/documentation/os/generating-log-messages-from-your-code | SHOULD |

### 2.9 gpui

| # | 규칙 | 강제 수단 | 근거 URL | 강도 |
|---|---|---|---|---|
| G1 | `cx.spawn`/`background_spawn`의 `Task`와 `Subscription`은 구조체 필드(`_task`, `_subscriptions: Vec<Subscription>`)에 보관하거나 `.detach()`한다. drop하면 즉시 취소된다. `let _ = task`와 `task;`는 기계가 잡지만, **`let _t = task;`는 잡지 못하므로 리뷰 항목**이다 | rustc `unused_must_use` + clippy `let_underscore_future`/`let_underscore_must_use` + 리뷰 | zed@93d7456 `crates/gpui/src/app/context.rs` 226-230행, `crates/scheduler/src/executor.rs` 399-402행, `crates/gpui/src/subscription.rs` 147-150행 | MUST |
| G2 | spawn 클로저는 `WeakEntity`만 캡처하고, `update` 실패(엔티티 해제)를 처리한다. 상태를 바꾼 뒤에는 `cx.notify()`를 부른다. 사건은 `EventEmitter<E>` + `cx.emit`/`cx.subscribe`로 표현한다 | 리뷰 | 같은 SHA `context.rs` 220행, `src/gpui.rs` 346행 | SHOULD |
| G3 | 단축키는 `actions!(minikrill, [...])` + `KeyBinding` + `key_context` + `on_action(cx.listener(..))`로만 정의한다 | 리뷰 | 같은 SHA `src/action.rs` 17-24행, `examples/testing.rs` 88-91행 | SHOULD |
| G4 | gpui 테스트는 `#[gpui::test]`로 쓴다. IPC 클라이언트는 트레이트 뒤에 두고 테스트에서는 인메모리 구현으로 바꾼다. 테스트 executor는 단일 스레드이고 외부 IO를 기다리면 panic하기 때문이다. 부수 효과는 `cx.run_until_parked()`로 진행시킨다. 경쟁 조건은 `iterations = N`과 `SEED`로 재현한다 | 리뷰 + check.sh(nextest) | 같은 SHA `examples/testing.rs` 283-313행·333행, `src/seeds.rs` 6-24행 | SHOULD |
| G5 | `mk-term`(렌더 모델·IME 상태기계)은 gpui에 의존하지 않고 일반 `#[test]`로 검증한다 | T5와 같음 | ARCHITECTURE §2 | MUST |
| G6 | macOS에서 `gpui_platform`에 `font-kit` 기능을 명시한다(없으면 "renders no glyphs"). Xcode가 없는 환경에서는 `runtime_shaders` 기능을 켠다(§4-6) | Cargo 기능 + 빌드 확인 | zed@93d7456 `crates/gpui/README.md` 31행, gpui-kit Cargo.toml 71행 | MUST |

### 2.10 Tauri 대비: 옮긴 것, 해당 없음, 공백

| 구분 | Tauri v2 | Minikrill |
|---|---|---|
| 옮김 | 신뢰 경계는 IPC 하나, 메시지 패싱("recipient is free to reject or discard requests") | I1: UDS 하나, 타입이 고정된 프레임, 상한, 거부 |
| 옮김 | capability, permission, runtime authority, deny 우선 | I2: 명령별 역할 표, 기본 거부 |
| 옮김 | 프로세스 분리("A crash in one component doesn't affect the whole system"), 최소 권한 | holder/krilld/app 3단. app은 PTY fd를 만지지 않음 |
| 옮김 | 업데이터 서명 필수, 비활성화 불가, 다운그레이드 거부, 키 분실 경고 | I3 |
| 옮김 | 생애주기: audit, vet/crev, git 해시 고정, 액션 고정, 서명 키 격리, 신고 창구 | S1~S9. 취약점 신고 창구(SECURITY.md)는 M4에 둔다 |
| 반면교사 | 사이드카 고아 이슈(#1896, #9950, #14360) → PR #14443(앱 트리 kill) | 데몬은 launchd 잡이고, 업데이트 순서(bootout → 교체 → bootstrap)를 코드로 고정하며, 고아 0을 e2e로 확인한다 |
| 해당 없음 | CSP, Isolation 패턴, 원격 URL capability, JSON IPC 직렬화 | 웹뷰와 JS가 없다. 대신 Hello 버전 협상과 크기 상한을 직접 책임진다 |
| 공백 | updater(minisign 기반), `tauri build`의 서명·공증, Info.plist 생성, single-instance, autostart, log, window-state, notification, global-shortcut 플러그인 | 전부 직접 구현한다. ARCHITECTURE §7의 자체 업데이터, A3, A4, 단일 인스턴스는 UDS bind 실패로 판정, 자동 시작은 launchd/SMAppService, 알림은 objc2 UNUserNotificationCenter |
| 공백 | 공식 크래시 리포팅 플러그인 없음(Tauri에도 없음) | P6 + ARCHITECTURE §7(panic hook → 로컬 파일 → opt-in 전송) |

---

## 3. 바로 쓸 설정 초안 (실측 완료)

**확인 방법:**
1. 저장소의 `Cargo.toml`, `Cargo.lock`, `crates/`를 `/tmp/c1lab`에 복사하고 아래 4개 파일을 넣었다.
2. 크레이트 루트 속성(§3.5)을 추가하고 `cargo fmt --all`을 돌렸다.
3. `mk-proto`의 `add`를 `saturating_add`로 바꿨다(`arithmetic_side_effects` 대응).
4. 저장소 `scripts/check.sh`를 그대로 실행했다(fmt, clippy `-D warnings`, nextest, doctest, `cargo deny --locked check`) → **exit 0**("check.sh: 전부 통과", "advisories ok, bans ok, licenses ok, sources ok").
5. 각 lint와 키가 실제로 동작하는지는 위반 코드를 넣은 탐침 파일로 따로 확인했다(§1 [실측] 항목).
6. deny.toml은 thiserror 2.0.21, nix 0.31.3, anyhow 1.0.104, zstd 0.13.3을 임시로 넣은 그래프에서도 통과했다.
7. 비교 대상인 **현재 저장소 그대로**의 결과: `cargo deny --locked check` → `licenses FAILED`, exit 4.

이 파일들은 하네스 파일이라 AGENTS 규칙 8에 따라 **사람 승인 후 반영**한다.

### 3.1 루트 `Cargo.toml` 추가·변경분

```toml
# 린트 정책은 docs/CONVENTIONS.md가 근거다. 바꾸려면 사람 승인 필요.
[workspace.lints.rust]
unsafe_code = "deny"                 # FFI 모듈만 #![expect(unsafe_code, reason = "...")]
unsafe_op_in_unsafe_fn = "deny"
missing_debug_implementations = "warn"

[workspace.lints.clippy]
all = { level = "deny", priority = -1 }
pedantic = { level = "warn", priority = -1 }
must_use_candidate = "allow"         # pedantic 중 노이즈 최대(빈 스켈레톤에서도 실패)
# unsafe
undocumented_unsafe_blocks = "deny"
multiple_unsafe_ops_per_block = "deny"
unnecessary_safety_comment = "warn"
# 무패닉 (테스트는 clippy.toml allow-*-in-tests)
unwrap_used = "deny"
expect_used = "deny"
panic = "deny"
todo = "deny"
unimplemented = "deny"
dbg_macro = "deny"
mem_forget = "deny"
exit = "deny"
# 억제는 #[expect(..., reason = "...")]만
allow_attributes = "deny"
allow_attributes_without_reason = "deny"
# gpui Task/Subscription 등 must_use 값 버리기 금지
let_underscore_must_use = "deny"
# 한글 텍스트 바이트 슬라이스 금지
string_slice = "warn"

[profile.release]
lto = "thin"
codegen-units = 1
debug = "line-tables-only"           # 크래시 심볼화용 최소 디버그 정보
split-debuginfo = "packed"           # [실측] target/release/<bin>.dSYM 생성
overflow-checks = true               # 와이어 길이 wrap 방지. 성능 영향 미측정

[profile.release-daemon]             # P4에서 선택지 A를 고를 때만
inherits = "release"
panic = "abort"
```

- lto, codegen-units, overflow-checks가 메모리·크기·속도에 미치는 영향은 **미측정**이다. 빌드가 성공하는 것만 확인했다. 확정 전에 MK 메모리 스파이크에서 측정한다.
- `print_stdout`/`print_stderr`는 워크스페이스 전역에 두지 않는다. [실측] 전역에 두면 `mk`와 `mk-app`의 `println!`이 실패한다. 그래서 데몬 크레이트 루트(§3.5)에만 둔다.

### 3.2 `clippy.toml` (루트 하나만)

```toml
allow-unwrap-in-tests = true
allow-expect-in-tests = true
allow-panic-in-tests = true
allow-indexing-slicing-in-tests = true
allow-print-in-tests = true
avoid-breaking-exported-api = false   # 공개 배포 크레이트가 없음(publish = false)
disallowed-methods = [
  { path = "std::env::set_var", reason = "멀티스레드 데몬에서 UB 위험. 자식 환경은 Command::env로" },
  { path = "std::env::remove_var", reason = "멀티스레드 데몬에서 UB 위험. 자식 환경은 Command::env_remove로" },
]
# check-private-items 는 켜지 않는다: 테스트 함수까지 # Panics 문서를 요구한다([실측]).
# msrv 는 생략: Cargo.toml rust-version을 따른다(문서 설명 기준).
```

### 3.3 `rustfmt.toml`

```toml
style_edition = "2024"
```

### 3.4 `deny.toml`

```toml
# cargo-deny 0.20.2에서 검증. 실행: cargo deny --locked check (--locked는 전역 옵션이라 check 앞에)
[graph]
targets = ["aarch64-apple-darwin", "x86_64-apple-darwin"]
all-features = false

[advisories]
# 취약점(vulnerability)은 키 없이 항상 오류다(0.20.x).
yanked = "deny"
unmaintained = "workspace"
unsound = "all"
ignore = []  # { id = "RUSTSEC-…", reason = "…" } 형식만, 사유 필수

[licenses]
allow = ["Apache-2.0", "MIT", "BSD-2-Clause", "BSD-3-Clause", "ISC", "Unicode-3.0", "Zlib"]
confidence-threshold = 0.93
unused-allowed-license = "allow"

[licenses.private]
ignore = true  # publish = false 인 워크스페이스 크레이트는 license 필드 없이 통과

[bans]
multiple-versions = "warn"
wildcards = "deny"
allow-wildcard-paths = true
workspace-dependencies = { duplicates = "deny", include-path-dependencies = true, unused = "deny" }
deny = []

[sources]
unknown-registry = "deny"
unknown-git = "deny"
required-git-spec = "rev"
allow-git = []
```

- `unmaintained = "workspace"`는 판단이다. 기본값 `all`이면 gpui 전이 의존성의 unmaintained 공지 하나로 야간 게이트가 막힐 수 있다(추정). 취약점은 이 설정과 관계없이 항상 오류다.
- `targets`에 x86_64를 넣을지는 Intel 지원 여부에 따라 사람이 결정한다.

### 3.5 크레이트 루트 속성

```rust
// crates/mk-proto/src/lib.rs, crates/mk-term/src/lib.rs, crates/mk/src/main.rs
#![forbid(unsafe_code)]
// crates/mk-proto/src/lib.rs (와이어 파서)
#![deny(
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::as_conversions
)]
// crates/krilld/src/{lib,main}.rs, crates/mk-holder/src/{lib,main}.rs
#![deny(clippy::indexing_slicing, clippy::print_stdout, clippy::print_stderr)]
// FFI 모듈 첫 줄 (예: crates/mk-holder/src/sys.rs)
#![expect(unsafe_code, reason = "mk-holder FFI 경계: openpty, SCM_RIGHTS, fork/exec")]
```

- [실측] mk-proto와 krilld에 이 속성을 넣은 상태에서 check.sh가 통과했다.
- [실측] 사용하지 않는 `#![expect(unsafe_code)]`는 `unfulfilled_lint_expectations`로 실패한다.
- [실측] 워크스페이스 `deny` + 모듈 `expect` 구조에서는 모듈 밖의 unsafe 블록이 오류가 난다.

---

## 4. ARCHITECTURE·AGENTS.md와 충돌하는 사실과 고칠 문장

모두 하네스 또는 설계 문서라 **사람 승인 대상**이다. 아래는 제안이다.

1. **TCC 상속 (ARCHITECTURE §4 "기동")**
   - 현재 문장: "헬퍼는 앱과 같은 identifier·팀으로 서명해 TCC 권한 상속(T2-D2, T4-D13)."
   - 사실:
     - TCC는 서명이 아니라 "responsible code"로 판단한다([DTS] 732291).
     - launchd 잡은 SMAppService나 `AssociatedBundleIdentifiers`로 귀속을 알려야 한다([DTS] 732291, 721918, 751802).
     - Apple은 "every program should have its own code signing identifier"라고 한다.
     - 같은 Team ID로 서명했는데도 상속이 실패한 사례가 있다([DTS] 805245).
     - `launchctl` 대신 SMAppService를 쓰라는 권고도 있다([DTS] 748205).
   - 고칠 문장: "헬퍼는 앱과 같은 팀, 각자 고유 identifier(`<앱 id>.holder` 등)로 서명하고, 생성 plist에 `AssociatedBundleIdentifiers = [<앱 bundle id>]`를 넣는다. TCC 귀속(마이크·카메라·보호 폴더·Apple Events)은 MK-007에서 실측으로 확정하며, 실패 시 SMAppService(macOS 13+, 영구 잡·사용자 승인) 경로를 대안으로 둔다."

2. **macOS 26+ 백그라운드 작업 프롬프트 (ARCHITECTURE §1, §4, ROADMAP MK-007)**
   - 사실: Apple 배포 가이드에 이렇게 적혀 있다. "On Mac computers with macOS 26 or later, if background tasks are started by an app and remain active after a user quits the app, the user is prompted to allow or not allow the background tasks to remain active." MDM으로만 억제할 수 있다. https://support.apple.com/guide/deployment/manage-login-items-background-tasks-mac-depdca572563/web
   - 앱 종료 후 데몬이 살아 있는 Minikrill 구조에 그대로 해당한다. 이 맥은 macOS 27이다.
   - 고칠 문장(MK-007 합격 기준에 추가): "앱 종료 후 데몬 생존 시 macOS 26+ 백그라운드 작업 프롬프트 발생 여부와 '허용 안 함' 선택 시 세션 생존 여부를 기록한다."

3. **빈 엔타이틀먼트 (ARCHITECTURE §7)**
   - 현재 문장: "Developer ID + hardened runtime + 빈 엔타이틀먼트".
   - 사실: hardened runtime은 리소스 엔타이틀먼트가 없으면 마이크 등의 접근을 막는다([DTS] 835256).
   - 우려: 에이전트가 터미널 안에서 이런 리소스를 쓸 때, 책임 프로세스(앱 또는 holder)의 엔타이틀먼트가 영향을 줄 수 있다(추정).
   - 고칠 문장: "엔타이틀먼트는 기본 비움. `device.audio-input`·`device.camera`·`automation.apple-events` 필요 여부는 MK-007에서 에이전트 시나리오로 실측해 결정."

4. **panic 정책 (AGENTS 규칙 6과 패닉 프로필)**
   - 현재 문장: "데몬은 panic으로 죽지 않는다: 세션 단위 격리".
   - 사실:
     - 세션 단위 격리는 `catch_unwind`, 즉 `panic=unwind`에서만 가능하다.
     - Cargo는 패키지별 panic 설정을 거부한다([실측]).
     - `extern "C"` 콜백 안의 panic은 설정과 관계없이 abort된다.
   - 고칠 문장(둘 중 하나를 택일): (A) "krilld는 `release-daemon`(panic=abort) 프로필로 따로 빌드하고, panic 시 holder가 재기동한다. 세션은 holder가 지킨다." 또는 (B) "krilld는 unwind이고 세션 작업 단위로 `catch_unwind` 격리한다. FFI 콜백은 panic을 반환값으로 바꾼다."
   - 어느 쪽이든 덧붙일 문장: "mk-holder는 panic 자체를 금지(P1·P2)."

5. **check.sh가 현재 실패한다 (AGENTS "명령")**
   - 사실: deny.toml이 없고 크레이트에 license 필드가 없어 `cargo deny --locked check`가 exit 4로 끝난다. 그래서 "작업 완료 주장 전 반드시 0"을 지금은 아무도 만족할 수 없다.
   - 조치: §3.4 deny.toml을 반영하거나, `[workspace.package] license`를 추가한다.

6. **gpui 셰이더 빌드 방법 (AGENTS "명령"의 MK-003 TODO)**
   - 사실:
     - `gpui-pre-apple` 0.3.9 build.rs는 기본적으로 `xcrun -sdk … metal`로 셰이더를 미리 컴파일한다.
     - 이 맥에서 `xcrun -f metal`을 실행하면 "unable to find utility "metal""이 나온다([실측]).
     - `runtime_shaders` 기능(`gpui-pre-platform/runtime_shaders`)을 켜면 컴파일 대신 셰이더 소스를 이어 붙인다.
     - gpui-kit도 `gpui_platform`에 `features = ["font-kit", …, "runtime_shaders"]`를 쓴다.
   - 고칠 문장(MK-003 확정 후): "로컬·야간 빌드는 `gpui_platform` 기능 `font-kit` + `runtime_shaders`. 출시 빌드만 Xcode 러너에서 프리컴파일."

7. **gpui 고정 단위 (ARCHITECTURE §5)**
   - 현재 문장: "`gpui-pre =0.3.9` + `gpui-component =0.7.3`".
   - 사실: 같은 스냅샷 안에서도 `gpui-pre-*` 크레이트 버전이 다르다(`gpui-pre-util-macros`/`-perf` 0.3.8, `-media` 0.3.5). 업스트림은 "Bump every snapshot crate together"를 요구한다.
   - 고칠 문장: "`gpui-pre`·모든 `gpui-pre-*`·`gpui-component`를 `=`로 고정하고 한 묶음으로만 올린다."

8. **경고 차단 방식 (scripts/check.sh)**
   - `cargo clippy … -- -D warnings`를 `CARGO_BUILD_WARNINGS=deny cargo clippy …`로 바꿀 것을 제안한다(Clippy 권고, 캐시 보존).
   - `build.warnings`가 잡지 못하는 Cargo 자체 경고(resolver 누락 등)를 막는 단계를 추가할 것을 제안한다. 예: `cargo metadata --locked --format-version 1 2>&1 >/dev/null | grep -q '^warning' && exit 1`. 이 명령 자체는 미실측이다.

9. **데이터 디렉터리 (ARCHITECTURE §4 "디스크")**
   - 현재 문장: "디렉터리 `.noindex`·0600"만 있다.
   - 고칠 문장: "`~/Library/Application Support/<bundle id>/`(상태·세그먼트, `.noindex`), 재생성 가능한 캐시는 `~/Library/Caches/<bundle id>/`, 소켓 경로는 104바이트 이하 검사."

10. **툴체인 표기 (AGENTS "툴체인")**
    - 사실: 최신 stable은 1.99.0(2026-10-01)이다. 로컬 1.98.1은 한 버전 뒤다. `[workspace.dependencies]`의 default-features 덮어쓰기는 1.99에서만 동작한다.
    - 고칠 문장(추가): "1.99 전용 Cargo 기능(패키지 레벨 default-features 덮어쓰기)에 기대지 않는다."

11. **AGENTS·WORKER가 `docs/CONVENTIONS.md`를 참조하지만 파일이 아직 없다.**
    - 이 보고서의 §2 표와 §3 설정이 그 초안의 근거가 된다.

---

## 5. 위험

1. **TCC와 BTM (높음).** 귀속 실패, macOS 26+ 백그라운드 프롬프트, launchctl 비권장 경로가 겹친다. "세션 손실 0"과 "권한 프롬프트 없음"이 MK-007 결과에 달려 있다. `launchctl bootstrap`이 "Login Items Added" 알림을 띄우는지는 공식 출처를 찾지 못했다.
2. **gpui-pre 공급망 (중간~높음).**
   - 1인(huacnlee) 게시자가 매주 내는 스냅샷이다. 스냅샷 안에서도 크레이트별 버전이 섞여 있다.
   - 대응: `=` 고정, vendor, `sources.unknown-*="deny"`, 갱신 때 diff 리뷰.
   - 공식 `gpui`는 0.2.2 이후 1년간 갱신이 없다.
3. **panic 전략 미결 (중간).** 결정하기 전에 krilld 코드를 쓰면 격리 구조를 다시 짜야 한다.
4. **lint 강도와 야간 처리량 (중간).**
   - pedantic(warn)에 `-D warnings`를 더하면 pedantic 전부가 사실상 차단 규칙이 된다.
   - gpui 코드에서 `cast_*`, `too_many_lines` 등의 노이즈 규모는 미측정이다. 실제 UI 코드가 들어오면 pedantic 중 allow할 목록을 다시 정해야 할 수 있다.
   - 대응: 예외는 `#[expect(reason)]`로만 남겨 추적할 수 있게 한다.
5. **clippy.toml 불안정 (낮음~중간).** Clippy 문서가 "may be deprecated in the future"라고 적고 있다. 크레이트 디렉터리에 clippy.toml을 두면 루트 설정이 조용히 사라진다(L6).
6. **조용한 무력화 (중간).** 아래 세 가지는 모두 **경고 없이 또는 exit 0으로** 규칙을 무력화한다. 하네스 파일을 바꿀 때는 위반 탐침으로 확인하는 절차가 필요하다.
   - rustfmt unstable 옵션 무시
   - 의존성에 없는 경로의 `disallowed-methods` 무시
   - resolver 경고가 `build.warnings`를 통과
7. **Miri와 sanitizer 사각지대 (중간).** 가장 위험한 코드(SCM_RIGHTS, fork/exec, openpty, objc2)는 Miri가 다루지 못한다. sanitizer는 nightly 전용인데 이 맥에는 rustup이 없다. 리뷰, e2e, 퍼징(MK-056)에 의존한다.
8. **release 프로필 수치 미측정 (낮음).** `lto`, `codegen-units`, `overflow-checks`, `panic`의 메모리·크기·지연 영향은 이 보고서에서 측정하지 않았다(AGENTS 규칙 7).

---

## 6. 미확인

- **`launchctl bootstrap gui/<uid>` + BTM 알림:** 비SMAppService plist가 "Login Items Added" 알림을 띄우는지 공식 출처가 없다. `man launchctl`에도 언급이 없다.
- **같은 팀 서명 시 TCC 공유:** 공식 문서에서 가장 가까운 근거는 TN3127의 "mutually compatible DRs … share access"다. 하지만 대상이 같은 앱의 두 배포판이라, launchd가 띄운 비번들 헬퍼에 적용하는 것은 추론이다.
- **TCC 전반으로의 확장:** TN3179는 Local Network 전용이다. 마이크·카메라·보호 폴더에 대한 공식 문서 문장은 없고, DTS 답변만 있다.
- **DTS 포럼 원문:** 129980, 805245, 732291, 721918, 751802, 748205, 835256은 curl로 열면 보안 확인 페이지가 나와 WebFetch 요약으로 읽었다. 805245는 "Load more" 뒤 답글을 못 봤다.
- **소켓 위치:** UDS를 어디에 둘지 정한 Apple 공식 문서는 찾지 못했다(`_CS_DARWIN_USER_TEMP_DIR` 0700이 후보).
- **`unsafe_code` lint와 외부 매크로:** 외부 크레이트 매크로(derive 등)가 만든 unsafe에서도 `unsafe_code`가 발화하는지 확인하지 않았다. gpui나 objc2 매크로를 mk-app 일반 모듈에서 쓸 때 오탐이나 미탐이 있을 수 있다.
- **check.sh 개선 명령:** §4-8의 `cargo metadata` 경고 검사 명령은 실측하지 않았다. `build.warnings`가 `cargo doc`의 rustdoc lint에도 적용되는지도 미확인이다.
- **nextest와 panic=abort:** nextest 문서에서 이 조합을 다룬 내용을 찾지 못했다. Cargo 규칙상 테스트는 unwind로 빌드되므로 영향이 없다고 추정한다.
- **release 프로필 수치:** §3.1 프로필은 빌드 성공만 확인했고, 메모리·크기·성능은 측정하지 않았다.
- **x86_64 지원 여부:** deny.toml `targets`에 x86_64-apple-darwin을 넣을지는 제품 결정이다.
- **cargo-audit와 cargo-auditable:** 이 맥에 설치돼 있지 않아 실행하지 않았다. 문서와 소스로만 확인했다.
- **Miri, sanitizer, rustup 경로:** 이 맥에서 실행할 수 없어 문서로만 확인했다.
