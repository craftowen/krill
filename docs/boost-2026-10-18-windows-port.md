# Boost: Krill Windows 포팅 기획 검증 (2026-10-18)

**결론: 1차 보고서의 큰 방향(컴파일 계층 → 실기 스파이크 → 네이티브 구현)은 맞지만, 병렬 계획에 실측으로 확인한 오류가 3개 있습니다.** 효과가 큰 순서로 고칠 점은 다음 셋입니다.
1. **웨이브 1b는 서로 독립이 아닙니다.** krilld는 krill-holder에, krill-app은 krill-link·krill-hosts·krill-term(개발 의존으로 krilld)에, krill CLI는 krill-hosts에 의존합니다(`cargo metadata`). 의존 순서(DAG)대로 3단으로 다시 나눠야 합니다.
2. **Windows 의존성을 루트에 선언하면 macOS `scripts/check.sh`가 깨집니다.** 현재 `deny.toml`은 Apple 타깃만 봐서 `unused-workspace-dependency`로 `bans FAILED`가 됩니다(실측). 타깃에 Windows를 합치면 이번에는 dwrote(MPL-2.0) 라이선스 거부가 납니다(실측). 그래서 "의존성 선등록 + deny 타깃 + dwrote 예외"를 첫 작업 하나로 묶어야 합니다. 그러면 Cargo.lock 충돌도 사라집니다.
3. **krill-proto 쪽 일은 생각보다 작고, 실제 일은 holder·krilld에 몰려 있습니다.** `std::os::unix::{fs,net,ffi}`와 이름이 같은 얇은 대체 모듈(이하 "심")을 두고 import 경로만 바꾸면 krill-proto·krill-log·krill-term·krill-hosts가 `--all-targets` 오류 0이 됩니다(실측, krill-log만 1줄 추가). 첫 층 오류는 krilld 121건, holder 54건, link 10건, krill 5건입니다.

저장소 파일은 고치지 않았습니다. 검증용 산출물은 `/tmp/krill-lockexp`(HEAD 사본, 실험 수정 포함)와 `/tmp/krill-shimwork/unixshim.rs`에 있습니다.

---

## 1. 이전 조사 정정 (주장 vs 실제)

| # | 1차 주장 | 실제 | 근거 |
|---|---|---|---|
| 1 | 웨이브 1b의 7개 작업은 "각자 자기 크레이트만" 맡아 병렬 가능 | **틀림.** krilld는 krill-holder를 일반 의존합니다. krill-app은 hosts·link·log·proto·term을 일반 의존하고 krilld를 개발 의존합니다. krill은 hosts·log·proto를 의존합니다. 하위 크레이트가 Windows에서 컴파일되지 않으면 `cargo check -p <상위>`는 상위 크레이트의 오류를 아예 보여주지 못합니다 | `cargo metadata --no-deps` |
| 2 | (암묵) holder와 krilld 사이 계약은 없다 | krilld가 `krill_holder::link::{Link, Packet}`를 씁니다(`krilld/src/holder.rs:21`, `rejoin.rs:32`, `registry.rs:24`). 그런데 `Link::new(OwnedFd)`, `send(&[u8], &[BorrowedFd])`는 Unix에만 있는 fd 타입입니다(`krill-holder/src/link.rs:6,48,62`). **holder가 Windows에서 어떤 API를 내보낼지 먼저 정해야** krilld 작업을 시작할 수 있습니다 | 원본 확인 |
| 3 | cargo-deny(Windows 그래프)는 `bans·licenses·sources ok` | **Windows 타깃만 넣었을 때만 맞습니다.** 실제 운영 설정처럼 Apple 2개 + Windows를 함께 넣으면 `dwrote 0.11.5`(MPL-2.0)가 거부됩니다. 경로는 zed-font-kit → gpui-pre-apple이고, 실제로는 빌드되지 않는 오탐입니다. 예외 1줄을 넣으면 통과합니다 | `/tmp/krill-lockexp`에서 실측 |
| 4 | (빠짐) 루트 `[workspace.dependencies]`에 Windows 의존성을 추가 | 현재 deny.toml(Apple 타깃만 봄, `workspace-dependencies.unused = "deny"`) 상태에서 `windows-sys`·`uds_windows`를 선언하면 **`bans FAILED` → macOS check.sh 실패** | 실측 |
| 5 | "루트 Cargo.toml·Cargo.lock은 한 작업만 만진다" | 원칙은 맞지만 세부가 빠졌습니다. 실측 결과: ① 이미 있는 의존성을 `[target.'cfg(..)'.dependencies]`로 옮겨도 **Cargo.lock은 바뀌지 않음**(`--locked` 통과). ② 새 직접 의존성을 추가하면 그 크레이트 블록에 1줄이 늘어남. ③ **features만 추가하면 Cargo.lock은 바뀌지 않음** | 실측 |
| 6 | linux-host 미커밋 변경 35개 파일(+295/−439) | **현재는 40개 파일(+685/−536)이고 지금도 수정 중**입니다. `popout.rs`가 오늘 11:11에 바뀌었습니다. Linux와 무관한 앱 대시보드 변경(`shell/popout.rs` 368줄)도 섞여 있습니다. 기반은 v0.1.4(`e52811a9`)이고, main이 v0.1.5로 오면서 바뀐 파일 중 2개(`krill-app/src/i18n.rs`, `krill/tests/cli.rs`)가 겹칩니다 | `git status`·`git diff --stat`·`stat` |
| 7 | Windows W1 전체를 linux-host 병합 뒤에 시작 | **일부만 막힙니다.** linux-host가 건드리지 않는 krill-log·krill-term·krill-link는 지금 시작할 수 있습니다. 겹치는 파일은 proto `daemon.rs`·`migrate.rs`, holder `holder.rs`·`sys.rs`, krilld `Cargo.toml`·`sys.rs`·`store/crypto.rs`·`shell.rs`, hosts `ssh.rs`, app `shell.rs`·`ssh_hosts.rs`·`sidebar.rs`·`i18n.rs`, krill `orca_import.rs`입니다 | 위 diff |
| 8 | krill-holder 테스트 외 코드가 정확히 1,200줄 | src 전체(`#[cfg(test)]` 포함)가 1,200줄입니다. `holder.rs:640~675`가 테스트 모듈이라 테스트 외는 **약 1,164줄**입니다 | `wc -l`, `holder.rs:640` |
| 9 | krill-term은 proto만 고치면 통과(추정), krill-hosts는 미측정 | **둘 다 실측으로 확인했습니다.** 심을 적용하면 `--all-targets` 오류 0입니다. term은 테스트 1줄(`paste.rs:68`), hosts는 import 경로만 바꾸면 됩니다 | 실측 |
| 10 | krill-proto 41건, krill-log 6건 | 맞습니다(lib 기준). **`--all-targets`로 재면 proto 75건, log 7건**(lib test)입니다. 둘 다 심 + log 1줄로 0이 됩니다 | 재측정 |
| 11 | (미측정) krilld·holder·link·krill 오류 수 | 첫 층 하한: **krilld 121건**(18개 이상 파일, `std::os::fd` import만 13개 파일) / **holder 54건**(sys 31, holder 14, link 6, teardown 3) / **link 10건**(tokio Unix 5, `rustix::process` 5) / **krill 5건**(doctor·hook·session/terminal) / krill-app은 link에 막혀 측정 불가 | 심 + 의존성 이동 + holder 스텁 상태에서 실측 |
| 12 | Windows 전용 의존 패키지 30개 | 패키지·버전 기준 **48개**, 이름 기준 38개입니다(557/565 총계는 맞음) | `cargo tree --target` 재측정 |
| 13 | 단축키 `cmd-` 66개 | `KeyBinding::new("cmd-` 57곳, `"cmd-` 문자열 전체 215회입니다. 세는 기준에 따라 달라 66은 재현하지 못했습니다 | grep |
| 14 | `ci-macos.yml` 주석의 "비공개 저장소"가 실제와 다르다 | 맞습니다. 다만 `ci.yml:4`, `ci-macos.yml:3`, `package-macos.yml:3` **세 곳**에 있습니다. 저장소는 PUBLIC으로 확인했습니다 | `gh repo view` |
| 15 | uds_windows가 `pair()`·타임아웃·nonblocking을 갖췄다 | 맞습니다(MIT, 의존은 windows-sys 0.61.2·tempfile·memoffset로 전부 이미 lock에 있음). 다만 **피어 인증 API(peer cred)는 없습니다.** `SIO_AF_UNIX_GETPEERPID`는 krilld FFI로 직접 불러야 합니다 | `uds_windows-1.2.1/src/stdnet/net.rs` |
| 16 | 통합 테스트 48개 중 36개가 Unix 사용 | 48개 중 37개입니다(grep 패턴 차이) | grep |

**원본과 일치함을 확인한 것.**
- gpui-pre-windows 셰이더: `build.rs:6`의 `#[cfg(target_os="windows")]`는 빌드 스크립트 기준이라 호스트 OS를 뜻합니다. `directx_renderer.rs:1869`의 `include!`와 디버그 빌드의 `:1832` canonicalize(빌드 머신 경로)도 맞습니다. Mac에서 `--release` check를 하면 `shaders_bytes.rs` 없음으로 실패하는 것도 재현했습니다.
- tokio `cfg_net_unix!` = `cfg(all(unix, feature="net"))`(`tokio-1.53.3/src/macros/cfg.rs:374-380`). 따라서 Windows에는 `tokio::net::UnixStream`이 없습니다.
- 서드파티 Windows check 결과(objc2 Apple 전용 오류, core-foundation 2건, jemalloc-sys 빌드 스크립트 실패, rustix-openpty 21건, alacritty·notify·iroh·gpui-pre-platform·gpui-component 통과)를 전부 재현했습니다.
- 인용한 줄 번호들: daemon.rs:99·218·242-249·345·487, krilld main.rs:30-31, Cargo.toml:19-20·28-29, app Cargo.toml:21-24, theme.rs:17, keystroke.rs:118, events.rs:154-155·1467·1691, direct_write.rs:478·1913, launchd.rs 265줄·:151·155.
- T1-final :99·:232·:455, ROADMAP 마지막 ID `MK-187`.
- 하네스: `source /tmp/krill-scripts/wingnu.env` 후 공식 rustc 1.98.1과 Homebrew cargo 조합이 동작합니다.

## 2. 이미 적용된 것 (중복 제안 금지)

| 항목 | 상태 | 근거 |
|---|---|---|
| Linux CI(proto·term clippy·nextest·deny) | 적용 완료 | `.github/workflows/ci.yml:26-61` |
| `#[cfg(unix)]` 인라인 선례 | orchestration.rs 7곳 | `krill-proto/src/orchestration.rs:11,1326,1981,2004,2016,2561,2916` |
| `sys/{macos,linux}.rs` 분할 패턴, sf·cf를 `cfg(target_os="macos")`로 이동 | **미커밋 진행 중**(linux-host) | `/Users/macmini/Documents/wProject/minikrill` `crates/krilld/Cargo.toml` diff, `krilld/src/sys/{linux,macos}.rs`(235/402줄), `krill-holder/src/sys/linux.rs`(292줄) |
| Linux 피어 검증 대체: `peer_token` → Unsupported, `verify_audit_token` → `CodeSign::invalid()` | 미커밋 | linux-host `krilld/src/sys/linux.rs:53-72` (Windows도 같은 방식 재사용 가능) |
| gpui Windows 백엔드(DirectX 11·DirectWrite·IMM32) | vendor에 있음 | `vendor/gpui-pre-windows`(13,172줄), `gpui-pre-platform/Cargo.toml:108-117` |
| alacritty ConPTY(conpty.dll을 먼저 시도) | vendor에 있음 | `vendor/alacritty_terminal/src/tty/windows/conpty.rs:31-55` |
| 0600/0700 쓰기 도우미 3벌 | 중복으로 존재 | `krill-link/src/store.rs:104`, `krilld/src/usage/accounts.rs:331`, `krill/src/orca_import.rs:3093,3111` |
| Windows check 하네스 | 저장소 밖 | `/tmp/krill-rust-official`(651MB), `/tmp/krill-mingw-dl`(99MB), `/tmp/krill-win-target`(2.6GB), `/tmp/krill-scripts/{wingnu.env,deny-win.toml}` |

## 3. 개선안 (우선순위순)

| 순위 | 항목 | 근거 파일:줄 | 예상 효과 | 난이도 | 위험 |
|---|---|---|---|---|---|
| 1 | **웨이브를 DAG 순으로 재배치**: proto·log → term·hosts·holder·link → krilld·krill → app | `cargo metadata` 결과(krilld→holder, app→link·hosts·term·dev krilld, krill→hosts) | 워커가 막힌 채 기다리거나 남의 크레이트를 고치는 일을 막음 | 낮음 | 없음 |
| 2 | **의존성 선등록 작업 하나(W1-01)**: 루트에 `windows-sys = "0.61.2"`·`uds_windows = "1.2.1"` 선언(둘 다 이미 lock에 있음). 각 크레이트 Cargo.toml의 target 섹션 이동과 `cfg(windows)` 항목 추가, Cargo.lock 갱신, deny.toml 타깃에 `x86_64-pc-windows-msvc` 추가와 dwrote 예외. 이후 작업은 **자기 크레이트 Cargo.toml에 `features`만 추가** | `deny.toml:3`, `workspace-dependencies unused="deny"`, 실측 3건(1장 #3~#5) | Cargo.lock·루트 충돌 0, macOS check.sh 유지 | 낮음 | deny.toml은 하네스 파일이라 승인 필요(규칙 8) |
| 3 | **`krill_proto::os` 심**: unix에서는 `pub use std::os::unix::{ffi,fs,net}`라 macOS 동작이 바이트 단위로 같음. Windows에서는 쓰기 측(`OpenOptionsExt/DirBuilderExt::mode`, `Permissions::from_mode`)은 no-op, `net`은 uds_windows, `ffi`는 `as_encoded_bytes`로 대응. 하위 크레이트는 import 경로만 바꿈 | 실측: proto·log·term·hosts가 `--all-targets` 0 (29개 파일 +58/−45, 심 1개 제외) | 1차안(호출처 155곳을 `.private()`로 교체)보다 diff가 작고 macOS 회귀 위험이 0 | 낮음 | Windows 비공개 권한은 `%LOCALAPPDATA%` ACL 상속에 맡김(추정, Phase 3에서 명시 DACL) |
| 4 | **권한·소유자 "읽기" 측은 심에 넣지 않고 호출처에서 명시 cfg**: `which()` 실행 비트는 Windows에서 PATHEXT 확장자로 판정, `preflight`는 PE `MZ`, launchd 도메인 uid는 `cfg(macos)` | `krill-proto/src/agent.rs:213`, `daemon.rs:494`, `daemon/migrate.rs:271` | 실행 비트·소유자 검사가 Windows에서 조용히 통과하는 실패(fail-open)를 막음. Claude Code npm 설치본은 `claude.cmd`라 PATHEXT가 없으면 에이전트 실행이 실패함(추정) | 중 | `.cmd` 실행 시 인자 이스케이프(Rust std가 처리, 일반 사양) |
| 5 | **holder Windows 스텁 계약을 W1에서 먼저 확정**: `link`·`holder`·`sys`·`teardown`은 `cfg(unix)`, Windows에는 `run()` → `Unsupported`, `close_inherited()` → `Ok`, main의 `detach` 분기 | `krill-holder/src/lib.rs:10-17`, `main.rs:24-35` | 이렇게 바꾸니 holder lib+bin check 0(실측, 약 15줄). krilld 작업의 선행 조건이 분명해짐 | 낮음 | krilld가 holder 링크 경로를 `cfg(unix)`로 막아야 함(아래 W1-07c) |
| 6 | **krilld 중복 헬퍼 통합(Windows 이전에 macOS에서 리팩터)**: `nonblocking(pipe)` + `kill_process_group` 묶음이 6곳 이상 복제돼 있음 | `krilld/src/git/mod.rs:338-350`, `registry/schedule.rs:209-220`, `git/changes.rs:273`, `git/worktree/remove.rs:378-394`, `shell.rs:353`, `mobile/pwa_rpc.rs:526` | Windows 분기 지점이 6곳에서 1곳으로 줄어듦(Job Object 대체 지점이 하나) | 중 | 동작 동일 리팩터라 기존 nextest로 검증 |
| 7 | **O_NOFOLLOW 보안 대응**: W1에서는 `cfg(unix)`로 막고, Phase 3에서 `FILE_FLAG_OPEN_REPARSE_POINT` 등으로 같은 보장을 다시 넣음 | `krill-log/src/writer.rs:105`, `krilld/src/git/worktree/compat.rs:272`, `mobile/devices.rs:28`, `mobile/pwa_rpc.rs:442` | 심볼릭 링크 바꿔치기 방어가 Windows에서 빠지는 것을 추적함 | 중 | W1 동안 Windows는 실행물이 아니라 실제 노출은 없음 |
| 8 | **Windows CI의 줄바꿈 고정**: 저장소에 `.gitattributes`가 없음 → `* text=auto eol=lf` 추가 또는 checkout 전에 `core.autocrlf false` | 저장소 루트(파일 없음), `include_str!/include_bytes!` 19곳, `krill-proto/build.rs` 소스 해시 | 픽스처 비교 테스트와 빌드 ID 불일치를 예방 | 낮음 | Windows 러너 기본값 autocrlf=true는 일반 사양(미확인) |
| 9 | **하네스 운영 정리**: ① 워커별 `CARGO_TARGET_DIR=/tmp/krill-win-target-<ID>`(공유하면 cargo 빌드 디렉터리 잠금 때문에 직렬화됨, 하나에 2.6GB, 디스크 여유 97GB) ② `/tmp`에서 `~/.cache/krill-win`으로 이동(재부팅 시 소실 가능, 미확인) ③ `CARGO_FEATURE_PURE=1`은 blake3 msvc 우회용이니 gnu 검사에서는 영향 범위를 적어 둠 ④ 공식 clippy 1.98.1 추가 | `/tmp/krill-scripts/wingnu.env` | 병렬 워커 7개 이상이 실제로 동시에 검증 가능 | 낮음 | 없음(저장소 밖) |
| 10 | **린트 함정을 수용 기준에 명시**: 스텁은 `unimplemented!/todo!` 금지(clippy deny)라 `io::ErrorKind::Unsupported`를 반환. Windows에서만 죽은 코드는 `#[cfg_attr(windows, expect(dead_code, reason=".."))]`로 처리(그냥 `expect`를 쓰면 macOS에서 "unfulfilled expectation"). Windows FFI 모듈은 `#![expect(unsafe_code, reason)]` | `Cargo.toml` `[workspace.lints]` | 리뷰에서 되돌아오는 작업을 줄임 | 낮음 | 없음 |
| 11 | krill-link가 앱에 내보내는 API(`is_serve_running`, `host_status`, `host_socket_path`, `TunnelState`, `RouteKind`, `RemoteHost`, `Error` 등 약 15개)는 **Windows에서도 시그니처를 유지하고 스텁으로 둠** | `krill-app/src` grep | link와 app 작업이 서로 기다리지 않음 | 낮음 | 없음 |

## 4. 단계별 작업 분해 (최종)

**공통 정의.**
- `WCHK(X, 범위)` = `source <하네스>/wingnu.env && CARGO_TARGET_DIR=/tmp/krill-win-target-<ID> cargo check --locked --target $T -p X <범위> --message-format short 2>&1 | grep -cE '^crates/.*(error|warning)'`, 결과가 **0**이어야 함.
- `MAC(X)` = `cargo clippy -p X --all-targets --locked -- -D warnings && cargo nextest run -p X --locked --no-tests=pass`, 결과가 0이어야 함.
- 모든 작업의 마지막 기준: macOS `scripts/check.sh` 0 종료.
- macOS 전용 코드는 `cfg(unix)`가 아니라 `cfg(target_os="macos")`로 막습니다(Linux 회귀 방지).

### Phase 0 — 결정·선행 (사람)

| ID | 작업 | 담당 파일 | 선행 | 수용 기준 |
|---|---|---|---|---|
| WIN-D1 | 범위 결정: T1 D8·B12·비타깃 개정, 전략 A(네이티브)/B(WSL2)/C(앱만 먼저), 주 타깃 msvc, M11과 MK-188~ 신설 | `docs/PRD.md`, `docs/ROADMAP.md` | — | ROADMAP에 M11 행 추가 |
| WIN-D2 | 하네스 변경 승인: deny.toml 타깃+dwrote 예외, `ci-windows.yml`, AGENTS 규칙 5(앱 unsafe 구역을 "platform 모듈"로), CONVENTIONS U5·U7 Windows 조항, holder 1,200줄 상한 해석(플랫폼별인지 여부) | `deny.toml`, `AGENTS.md`, `docs/CONVENTIONS.md` | — | 승인 기록 |
| WIN-D3 | linux-host 정리: 40개 파일 중 Linux 몫만 커밋해 v0.1.5 기반으로 병합하고, popout 대시보드 등 무관한 변경은 분리 | linux-host 체크아웃 | — | main에 `sys/{macos,linux}.rs` 반영, `scripts/check.sh` 0 |

### Phase 1 — Mac에서 Windows `cargo check` 통과

| 웨이브 | ID | 작업 | 담당 파일 | 선행 | 수용 기준 |
|---|---|---|---|---|---|
| 1a | W1-00 | 하네스 정식화(저장소 밖): `~/.cache/krill-win`으로 이동, 공식 clippy 1.98.1 추가, 워커별 target dir 규약 | 저장소 밖 | — | `WCHK(krill-proto, --lib)` 결과가 정확히 41(현재 기준선) |
| 1a | W1-01 | 의존성 선등록: 루트 windows-sys·uds_windows 선언, krilld jemalloc 2개 → `cfg(not(windows))`, sf·cf → macos, holder rustix·rustix-openpty·libc → `cfg(unix)`, app objc2 4개 → macos, 각 크레이트에 `cfg(windows)` 항목 추가, Cargo.lock, deny.toml | 루트 `Cargo.toml`·`Cargo.lock`·`deny.toml`, 각 `crates/*/Cargo.toml` | D2, D3 | `cargo metadata --locked` OK, `cargo deny --locked check` ok, check.sh 0 |
| 1a | W1-02 | krill-log: `writer.rs` 모드·NOFOLLOW `cfg(unix)`, `lib.rs` 경로(`%LOCALAPPDATA%\Krill\Logs`)·심볼릭 링크 | `crates/krill-log/src/**` | 없음(지금 시작 가능) | `WCHK(krill-log, --all-targets)`, `MAC(krill-log)` |
| 1a | W1-03 | krill-proto: `os` 심(개선안 3), 읽기 측 명시 처리(개선안 4), `state_dir` Windows 경로, `migrate` `cfg(macos)`, EXE_SUFFIX, `SUN_PATH_MAX` cfg | `crates/krill-proto/src/**` | W1-01 | `WCHK(krill-proto, --all-targets)`, `MAC(krill-proto)`, Linux CI 녹색 |
| 1b | W1-04 | krill-term·krill-hosts: import 경로만 교체(term `paste.rs:68`, hosts `UnixStream` 6줄) | `crates/krill-term/**`, `crates/krill-hosts/**` | W1-03 | 두 크레이트 모두 `WCHK(--all-targets)`, `MAC` |
| 1b | W1-05 | krill-holder Windows 스텁(개선안 5), 통합 테스트 `#![cfg(unix)]` | `crates/krill-holder/**` | W1-03 | `WCHK(krill-holder, --all-targets)`, `MAC` + 생존 테스트(`tests/survive.rs`) 녹색 |
| 1b | W1-06 | krill-link: tokio Unix·launchd·`rustix::process`는 `cfg(unix)`, 앱용 API는 Windows 스텁(개선안 11) | `crates/krill-link/**` | W1-03 | `WCHK(krill-link, --all-targets)`, `MAC` |
| 1b | W1-07a | krilld 헬퍼 통합(macOS 리팩터, 개선안 6) | `krilld/src/{git/mod.rs, git/changes.rs, git/worktree/remove.rs, registry/schedule.rs, shell.rs, mobile/pwa_rpc.rs}` + 새 헬퍼 1개 | D3(shell.rs가 겹침) | `MAC(krilld)` (Windows check 불필요) |
| 1c | W1-07b | krilld 플랫폼 계층: `sys/windows.rs` 스텁(Unsupported), `main.rs` 할당기 cfg, `alloc.rs`, `store/crypto.rs` macOS cfg | `krilld/src/{sys.rs, sys/windows.rs, main.rs, alloc.rs, store/crypto.rs}` | W1-01, W1-05 | `WCHK(krilld, --lib)` 출력 중 `^crates/krilld/src/(sys|main|alloc|store/crypto)` 줄이 0(남의 파일 오류가 남아 있어도 판정 가능), `MAC` |
| 1c | W1-07c | krilld fd·PTY 경로 cfg: `std::os::fd` import 13개 파일, holder 링크·rejoin·ipc·session·termios | `krilld/src/{holder.rs, rejoin.rs, ipc.rs, session.rs, session/input.rs, attach*.rs, registry.rs, scope.rs, store/privacy.rs}` | W1-05, W1-07a | 위와 같은 방식으로 자기 파일 오류 0, 07b와 합친 뒤 `WCHK(krilld, --all-targets)` |
| 1c | W1-08 | krill CLI: `doctor.rs`·`hook.rs`·`session/terminal.rs`(첫 층 5건), `computer`는 Windows에서 숨김 | `crates/krill/**` | W1-04 | `WCHK(krill, --all-targets)`, `MAC` |
| 1c | W1-09 | krill-app: platform `cfg(macos)` + `platform/windows.rs` no-op, updater·permissions·menu_bar macOS cfg, 해당 UI 숨김(규칙 10), import 경로 교체 | `crates/krill-app/**` | W1-04, W1-06 (`--all-targets`는 W1-07 뒤) | 1차 `WCHK(krill-app, --lib --bins)`, 2차 `--all-targets`, `MAC` |
| 1d | W1-10 | `ci-windows.yml`: autocrlf 끄기, `cargo check --workspace --all-targets --locked`, `cargo build -p krill-app --release`(fxc 셰이더 확인) | `.github/workflows/ci-windows.yml`(+`.gitattributes`) | D2. 작성은 병렬로 해도 되고 활성화는 W1-09 뒤 | windows-latest 녹색 |
| 1d | W1-11 | 통합 검증 | — | 전부 | `cargo check --workspace --all-targets --target $T` 0, deny ok, check.sh 0 |

### Phase 2 — Windows 실기 스파이크 (병렬, 서로 독립)

| ID | 스파이크 | 환경 | 수용 기준 |
|---|---|---|---|
| S-W1 | ConPTY 생존: holder kill과 krilld kill 시 세션 생존, DuplicateHandle 재전달, conpty.dll·OpenConsole 번들, 출력 충실도, conhost의 DA/DSR 자체 응답 여부 | windows-latest (헤드리스 가능 여부는 추정) | §4.1 Windows 열 초안과 실측 표 |
| S-W2 | IPC: uds_windows 실동작, `SIO_AF_UNIX_GETPEERPID`와 토큰 SID 검사, `WSADuplicateSocketW` AF_UNIX 지원 여부. 안 되면 named pipe와 비교 | windows-latest | 통합 테스트 녹색 |
| S-W3 | 앱: 한글 IME 10케이스, 렌더링, 메모리 지표 정의(Private Working Set인지 PrivateUsage인지) | **실제 Windows 데스크톱(사람)** | 결과표와 녹화 |
| S-W4 | 데몬 수명: `DETACHED_PROCESS`·`CREATE_BREAKAWAY_FROM_JOB`, 로그오프, 작업 스케줄러와 HKCU Run 비교 | windows-latest + 실기 | 기동 방식 결정 |
| S-W5 | 에이전트 실행: PATHEXT로 `claude.cmd`·`codex.cmd` 실행, `.cmd` 인자 이스케이프, 기본 셸 결정(pwsh/cmd/Git Bash) | windows-latest | 실행 성공표 |

### Phase 3 — 네이티브 구현 (S-W1·S-W2 합격 뒤)

| ID | 작업 | 담당 파일 | 선행 | 수용 기준 |
|---|---|---|---|---|
| W3-0 | holder 프로토콜: 핸들 값 전달과 Resize 메시지(번호는 지휘자 배정) | `krill-proto/src/holder.rs` | S-W1 | proto 테스트, 와이어 호환 |
| W3-1 | holder Windows: ConPTY, Job Object + IOCP, DuplicateHandle, 업그레이드 정책 | `krill-holder/src/sys/windows.rs`·`holder.rs` | W3-0 | Windows 생존 e2e |
| W3-2 | krilld Windows: 리더 스레드, resize, DPAPI 마스터 키, 셸, 피어 PID·SID 검사, NOFOLLOW 재도입 | `krilld/src/sys/windows.rs`, `session.rs`, `store/crypto.rs`, `shell.rs` | W3-0 | Windows nextest 녹색 |
| W3-3 | 앱 Windows: `platform/windows.rs` 실구현, `secondary-` 단축키, 폰트, 열기 | `krill-app/src/platform/**`, `ui/theme.rs` | S-W3 | 실기 QA |
| W3-4 | CLI Windows: hook·doctor·statusline 경로 | `krill/src/{hook_install,doctor,statusline}.rs` | W1 | Windows nextest |
| W3-5 | e2e: 앱 `taskkill /F` 1,000회, krilld kill, 출력 해시 | 새 테스트 | W3-1, W3-2 | 손실 0 |

### Phase 4 — 배포·운영

설치 형식(zip·Inno·MSIX·winget), rename 방식 업데이터와 롤백, Authenticode 서명(유료라 사람이 결정), release 워크플로 Windows 잡, Windows 메모리 게이트.

### Phase 5 — 원격·부가 기능

krill-link·모바일 게이트웨이 Windows화(tokio Unix 대체), krill-hosts SSH 포워딩, WSL2 백엔드(전략 B를 택한 경우).

## 5. 미확인 항목

| 항목 | 이유 |
|---|---|
| krill-app의 Windows 오류 수 | krill-link에 막힘. TCP 별칭으로 link를 스텁하려 했으나 타입 오류가 연쇄돼 중단 |
| krilld·holder·link·krill의 실제 총 오류 수 | 해석(resolve) 단계에서 오류가 나면 타입 검사 오류가 가려집니다. 위 수치는 하한입니다(proto·log·term·hosts는 끝까지 컴파일돼 정확한 값) |
| Windows 타깃 clippy 결과 | 공식 clippy 구성요소를 설치하지 않음 |
| ConPTY에서 holder만 죽을 때 생존 여부, conhost VT 자체 응답, `WSADuplicateSocketW` AF_UNIX, uds_windows 실동작 | Windows 실기·러너 없음 |
| Windows 러너 autocrlf 기본값, GitHub 공개 저장소 Windows 러너 무료 여부 | 일반 사양, 과금 화면과 러너 미확인 |
| `/tmp` 재부팅 시 소실 여부 | `/etc/defaults/periodic.conf`가 없어 정책 확인 불가 |
| linux-host 최종 병합 형태 | 지금도 수정 중(11:11 변경). 수치는 11:13 기준 |
| Windows 시스템 힙 메모리 반납 특성, 앱·데몬 메모리 | 측정 안 함 |
| 작업량(일정) | 전부 추정 |

**관련 경로.**
- 검증 사본: `/tmp/krill-lockexp`
- 심 원본: `/tmp/krill-shimwork/unixshim.rs`
- 측정 로그: `/tmp/krilld-win.txt`, `/tmp/krill-cli-win.txt`
- 1차 보고서: `/tmp/boost-A-windows.md`
- 주요 근거 파일:
  - `/Users/macmini/orca/workspaces/minikrill/window/deny.toml`
  - `/Users/macmini/orca/workspaces/minikrill/window/Cargo.toml`
  - `/Users/macmini/orca/workspaces/minikrill/window/crates/krill-holder/src/link.rs`
  - `/Users/macmini/orca/workspaces/minikrill/window/crates/krill-holder/src/lib.rs`
  - `/Users/macmini/orca/workspaces/minikrill/window/crates/krilld/src/git/mod.rs`
  - `/Users/macmini/orca/workspaces/minikrill/window/crates/krill-proto/src/agent.rs`
  - `/Users/macmini/orca/workspaces/minikrill/window/vendor/gpui-pre-windows/build.rs`