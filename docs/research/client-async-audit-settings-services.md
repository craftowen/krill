# Client async audit — settings and services

작업 기준: HEAD `e0a8a4e53a3c0265c4fb8345b54622a8e3bcd370` 위의 기존 dirty 작업을 보존한 공유 작업 디렉터리, 2026-10-20. 커밋·리셋·체크아웃·push는 수행하지 않았다. Cargo 출력은 `/Volumes/krill-build/targets/krill`만 사용했다. 본 문서는 담당 범위의 조사/수정 보고서이며 전체 앱 릴리스 검증을 뜻하지 않는다.

## 조사 범위와 판정

`settings.rs`/하위 모듈, `accounts.rs`/하위 모듈, `onboarding.rs`/하위 모듈, `hosts.rs`/`hosts/link.rs`, `ssh_hosts.rs`, `open_in.rs`, `platform/**`, `updater.rs`/하위 모듈, `agent_history.rs`/하위 모듈, `orca_import.rs`, `link_ui.rs`/하위 모듈, `main.rs`를 조사했다. 파일 읽기/쓰기·metadata·경로 정규화, 프로세스 시작/대기, 소켓·핸드셰이크·키체인 간접 호출, mutex/file lock·수신 대기·sleep·join, 설정/히스토리/글꼴의 CPU 작업을 검색하고 UI 진입점까지 역추적했다. 테스트 전용 fixture IO는 제품 UI 호출과 구분했다.

- **BG**: `background_executor().spawn`/`background_spawn` 또는 전용 작업 스레드 안에서 호출한다. 동기 IO API 자체는 유지했다.
- **UI + BG**: UI에서 입력/세대 번호/대기 상태만 읽고 작업을 제출한다. 결과는 `WeakEntity::update`/`update_in`으로만 적용하며 대상/세대가 다르면 버린다. 전역 작업은 Config epoch/revision 또는 Orca discovery generation을 확인한다.
- **UI/native**: 창·포커스·메뉴·Dock·IME·secure event input 같은 AppKit/GPUI 주 스레드 API와 메모리 상태만 남긴다. 알림 권한 콜백의 대기, LaunchServices 요청, TIFF 변환, sleep assertion IPC, clipboard 원본 IO는 별도 백그라운드 경계로 보낸다.
- **CLI**: `--update-cli`/relauncher 등 그래픽 이벤트 루프를 돌리지 않는 별도 실행 모드다. UI 무응답 경로가 아니다.

## 수정한 UI 진입점

| 진입점/호출자 | 검출 작업과 기존 위치 | 최종 경계와 순서/오래된 결과 처리 |
|---|---|---|
| `settings::init_at` → `reload`/1초 폴링 | UI에서 파일 7개의 metadata, settings/keys 읽기·TOML 파싱·legacy 저장·플래그 IO | `background_file_io`에서 읽기/변환/플래그 동기화, Config epoch/revision 확인 후 메인에서 언어·키 바인딩·테마 반영. 읽기 중복은 합치고 강제 재읽기는 후속 요청으로 유지 |
| 설정 토글/폰트 크기/언어 → `edit` | UI에서 최신 파일 읽기·변환·fsync·rename | UI는 즉시 미리보기, 순차 BG 큐에서 최신 디스크에 동일 편집 함수를 적용. 상대 조절·빈 배열 삭제·미래 키·손상 파일 보존. 최신 저장 실패는 마지막 확정 설정으로 원복하고 오류 표시 |
| 설정 파일 열기 → `open_file` | UI에서 exists/mkdir/초기 파일 저장 | 준비는 순차 BG 큐. 시스템 편집기 요청은 완료 뒤 메인 API 호출 |
| 초기 설정과 알림 | 초기 파일 읽기 전 기본값 부작용 가능 | `settings::is_loaded` 제공. 최초 결과 확정 전 false, 상태 디렉터리 없는 메모리 설정/Config 없음은 true. alert 소유자에게 경계 전달 |
| `SettingsView::run_doctor` | UI에서 파일 존재/헬퍼 탐색/진단 읽기 | 중복 제한된 BG 작업, WeakEntity 완료 |
| 계정 규칙/로컬 관리/대화형 로그인/고아 정리 | 격리 상태 디렉터리 fallback이 `request_local`/프로필 스캔/저장을 UI에서 수행 | 규칙·추가/삭제/전환·정리를 순차 BG 큐로 통일. 서로 다른 편집은 순서를 유지하고 같은 키의 오래된 UI 결과는 무시 |
| 계정/호스트 새로고침·Claude identity 조회 | IO 자체는 기존 BG. 느린 조회가 새 관리 결과를 덮을 수 있음 | mutation 중/스냅샷이 바뀌었으면 이전 계정 결과 무시. 호스트·agents 파일 캐시는 file revision 검사 |
| 로그인 폴링 → `take_login_outcome` | UI에서 mutex `.lock()` | UI는 `try_lock`; 경합이면 다음 폴링 때 다시 확인. background writer만 blocking lock 사용. 자식 생성/인증/cleanup은 기존 login worker 유지 |
| SSH 가져오기/추가/수정/삭제 | UI에서 hosts/ssh config 읽기·직렬화·저장 | 순차 BG 파일 작업. 오류는 기존 상태 영역에 표시. 입력은 성공 후에만 비우며, 사용자가 입력/편집 대상을 바꾸면 늦은 완료가 입력을 지우지 않음 |
| SSH 키 등록 | UI에서 기본 키 is_file 검사 | 키 유무 검사·키 생성은 BG, file revision이 바뀐 이전 대상 결과 무시. 터미널 명령 큐잉은 메인 메모리 동작 |
| Tailscale/SSH test/원격 probe/install | 기존 명령·파일·네트워크는 BG | 중복 probe/install/discovery 제한, 변경된 호스트에 대한 오래된 완료를 버리고 pending 상태 정리 |
| 에이전트 편집 저장/reset·hook/skills 설치·전체 준비 | 일부 버튼이 UI에서 profile/config 읽기·쓰기, staging copy, hook receipt/file lock, CLI 실행 | 모두 순차 BG 작업으로 이동. 전체 준비도 stage/setup/CLI 대기까지 BG. 성공/실패 상태는 WeakEntity 완료 |
| Agents 탭 렌더 | 파일 캐시는 기존 BG였으나 사용자 TOML 재파싱이 매 렌더 UI에 남음 | BG에서 읽기와 effective agent catalog 파싱을 함께 수행. 렌더는 Arc catalog와 메모리 캐시만 사용. 내장 소규모 기본 catalog 생성은 메모리 전용 |
| 터미널 글꼴 드롭다운 | 메뉴를 열 때 UI에서 전체 font list + 모든 글꼴의 advance 판정 | Send+Sync GPUI TextSystem을 BG executor에서 사용해 목록/고정폭 판정/정렬 준비. 메뉴는 캐시만 읽으며 첫 준비 중 빈 목록 선택은 막음 |
| Ghostty/Warp 가져오기 | 디렉터리 탐색·파일 파싱은 이미 BG | 기존 BG 유지. 설정 병합/영속화는 새 순차 저장 경계 사용 |
| Workflows 렌더/저장소 입력 observer | `load_cached`도 mutex/metadata miss를 유발, canonicalize/load가 UI에 남음 | worktree/orchestration/schedule/run 데이터를 BG에서 읽고 Arc snapshot 사용. UI는 문자열 키만 정규화, 실제 canonicalize는 BG alias/mutation 준비에서 수행. Workflows가 보일 때만 주기 새로고침 |
| worktree symlink/hook/orchestration reset | UI에서 `WorktreeStore::mutate`/`OrchestrationStore::mutate`의 file lock·읽기·쓰기 | 경로 문법 검증만 메인, 실제 변경은 순차 BG 큐. 같은 키의 서로 다른 저장 요청은 유실 없이 순서대로 수행. 반환 Result는 검증·작업 접수 결과이고 저장 오류는 완료 상태로 표시 |
| schedule 추가/수정/toggle/remove/run-now | UI에서 `exclusive_edit` 잠금, 읽기·쓰기, UnixStream connect/실행 요청 파일 | 순차 BG. daemon 필요 오류 보존. 새 모달을 열거나 닫은 뒤 옛 저장 완료가 새 모달을 닫지 않게 form generation 검사 |
| schedule 실행 로그 | UI에서 capture IPC | BG capture. 실행 키와 log generation을 함께 확인해 close/reopen/다른 실행 선택을 보호 |
| Space scan/cleanup | subprocess 작업은 기존 BG | 대상 repo 변경 시 generation 증가, 이전 scan/rescan 결과 무시. CLI 경로는 discovery cache를 사용 |
| Orca detect/CLI resolve/preview/import/banner | UI의 is_dir/is_file/sibling_exe; 매 banner 렌더의 큰 report clone | 경로 탐색은 BG, UI getter/banner는 캐시/참조만 사용. discovery/scan/import는 환경 generation과 pending 상태 확인. byte cap/deadline/소켓 probe의 기존 BG 구현 유지 |
| 히스토리 패널 scan/search/resume | daemon Call은 기존 BG | host/call/scope/close 시 generation 증가·캐시/대기 해제. 이전 검색 완료는 새 query의 재검색을 먼저 수행하고 오래된 내용을 표시하지 않음 |
| Link server refresh/clock/mutation | backend IO는 기존 BG, clock 결과가 mutation 뒤 되돌아올 수 있음 | server generation 검사. busy 동안 refresh 중복 차단. 클라이언트 reset/pair/nearby에도 generation fence |
| Link clipboard suggestion/paste/render | 원본 clipboard를 생성·붙여넣기·렌더에서 동기 조회 | 렌더 조회 제거, constructor/명시 paste는 async read await. clipboard generation과 입력값 비교 후 WeakEntity 완료. native backend의 실제 큐잉 구현은 coordinator 소유 |
| `RealLinkBackend::new` | UI 생성 중 공유 LinkHosts registry mutex | 생성은 경로만 보관. registry 접근은 첫 BG pair 시 OnceLock으로 수행 |
| `Lease::drop` | UI에 마지막 참조가 떨어지면 mutex/child.kill | 유일 소유 시 mutex get_mut로 child를 take, kill/wait·PID 정리는 worker에서 수행. 소유한 link 자식만 정리, PTY/holder 소유권 변경 없음 |
| `main` 초기화/재열기/메뉴 | migration/log init/helper 경로/Env/bounds 파일 탐색 | BG startup preload 뒤 native window 생성. 재열기도 BG에서 최신 bounds 읽기, 중복 생성 합치기와 요청 session 유지. CloseWindow는 Shell durable request_close 경계 사용 |
| update 시작/수동 restart | startup recovery/Journal read/save 및 relauncher spawn이 UI에 남음 | journal recovery/read/ACK·spawn BG, prompt만 main. ACK 전 원래 journal과 비교해 새 update journal을 덮지 않음. 설정 restart는 `shell::Quit` action을 통해 durability 경계 사용 |
| app quit → link cleanup | UI에서 begin_shutdown registry lock | registry 수거와 child shutdown 모두 quit background task. 설정 파일 작업은 `flush_file_io` barrier 제공; Shell explicit quit가 await하도록 전달 |
| Dock badge | UI에서 공유 mutex | AppKit과 같은 주 스레드의 thread-local RefCell 캐시로 전환 |

## 이미 background인 백엔드 — 동기 API 유지

다음 호출은 메인에서 직접 실행되지 않음을 확인했다. 비동기 라이브러리 도입이나 세션 계약 변경은 하지 않았다.

| 파일/백엔드 | 호출자와 실행 위치 | 남는 대기/위험 |
|---|---|---|
| accounts request/exchange/local refresh/profile usage/rollout/identity | SettingsView의 BG 요청/refresh, Shell BG 계정 요청; browser login 전용 worker | daemon/파일/CLI가 지연될 수 있으나 이벤트 루프에서 기다리지 않음. UI polling mutex 경합은 nonblocking |
| ssh_hosts load/save/import/tailscale/keygen/test/managed runtime resolve | settings 파일 작업 또는 기존 SSH BG request, Shell BG host refresh | 네트워크/SSH timeout 또는 filesystem 지연은 pending 상태에서 처리 |
| onboarding survey/login_path/doctor/quick checks/stage/install/state atomic write | welcome의 기존 BG collect/fix, actions prepare/apply/auto_prepare, 새 offer completion job | TCC 사람 응답/프로세스 종료를 worker에서 기다림. prompt 생성만 GPUI 메인 |
| open_in open/open_path | Sidebar open_in BG, diff explorer BG | canonicalize/LaunchServices process output/Windows ShellExecute 대기는 BG |
| hosts/link registry/registrations/paired/ensure/connection_info/shutdown | Shell async host manager source, RealLinkBackend BG pair, quit BG task; connector factory도 host target 준비 BG | backend mutex는 유지. UI HostManager 호출자는 Shell 소유자에게 추가 확인 요청 전달 |
| link_ui/real runtime.block_on, launch agent, invite/pair/revoke/nearby | ServerPanel/ClientDialog의 background executor | iroh/network/launchctl/file/keychain 대기는 BG. mock mutex도 test/backend 작업 내 사용 |
| updater config/plist/plutil/codesign/curl/gh/download/unpack/hash/signature/rename/recovery/relaunch/wait | Settings manual check BG, Shell update poll BG, startup BG, headless update/relauncher CLI | 별도 relauncher의 process/ready 대기는 GUI 프로세스 이벤트 루프와 분리 |
| settings mobile IPC/connected devices/offer | existing background Call 경계 | 모바일 QR만 UI에 남지만 입력 271 UTF-8 byte/57×57 격자로 고정된 순수 CPU 연산이며 대용량 작업이 아님 (`krill-proto/src/mobile/qr.rs`) |
| platform permission notification callbacks/folder/FDA/quarantine/read probes | onboarding BG collect/fix/probe/actions; 알림의 background 조회 | recv_timeout와 파일 probe는 UI가 기다리지 않음. OS 권한 창/네이티브 메뉴·IME/포커스 API는 main 유지 |
| platform TIFF conversion/sleep assertions | Shell clipboard background 준비, alert 전용 thread (해당 소유자 작업) | Windows assertion은 같은 전용 thread에서 acquire/drop, 주석 갱신 |

## 순서와 수명 보장

파일 큐는 `oneshot`으로 이전 작업 완료를 비동기 await하고 worker에서 동기 작업을 실행한다. UI는 mutex/file lock을 잡거나 `join`/`sleep`하지 않는다. 작업을 제출한 설정 view가 사라져도 제출된 파일 작업은 완료하고 WeakEntity 화면 적용만 실패한다. `flush_file_io`는 이미 접수된 파일 작업 뒤의 barrier이며 explicit quit 경로가 이를 await한다. GPUI quit hook의 짧은 종료 유예만으로 영속성을 가정하지 않는다.

Config 재초기화에는 별도 epoch를 쓰고, 편집에는 전역 증가 revision을 쓴다. 새 Config의 pending 상태를 옛 read completion이 건드리지 않는다. 외부 편집 파일은 작업이 실행될 때 다시 읽고 기존 원자 교체/CAS 경로로 저장하므로 기존 unknown key·주석·권한·손상파일 보존 규칙을 유지한다. 사용자 입력을 바꾼 사이 완료된 SSH 저장, 닫았다 다시 연 스케줄 로그, 다른 모달/호스트/검색/환경에 대한 완료에는 각각 값/대상/generation 검사가 있다.

## 검증

검증 대상은 위 HEAD 위의 dirty 작업 트리다. 전체 `scripts/check.sh`나 릴리스 승인을 대신하지 않는다. 최종 실행 결과와 로그는 아래에 추가한다.

추가한 회귀 시나리오:

- settings 파일 큐를 지연시켜도 UI command 입력을 처리하고 연속 편집을 순서대로 저장한다.
- 이전 Config의 지연 read가 재초기화된 다른 디렉터리 설정을 덮지 못한다.
- schedule file lock을 다른 소유자가 보유한 채로도 toggle callback이 반환하고 다른 탭 입력을 처리한다.
- 손상된 settings 파일을 보존하고 실패한 optimistic UI를 확정 설정으로 원복한다.
- 로그인 결과 mutex를 writer가 보유해도 UI polling은 즉시 None을 돌려준다.
- Orca 환경을 바꾸면 먼저 시작한 discovery 결과를 버린다.
- 기존 설정·규칙·계정·SSH·스케줄·테마·rollback·import UI 테스트는 실제 비동기 완료 뒤 상태/디스크를 검증하도록 조정했다.

## 잔여 한계와 범위 밖 연계

지연된 파일/프로세스/네트워크 작업을 강제로 취소하는 기능을 추가하지 않았다. OS 파일 접근이 멎으면 background 작업이 pending으로 남고, 명시적 종료도 영속성 barrier를 기다릴 수 있다. 이때 UI 이벤트 루프는 실행할 수 있어야 한다. 전체 앱에서 framework 내부 글꼴/renderer/clipboard native lock까지 영원히 기다리지 않는다는 보장은 이 담당 보고서만으로 주장하지 않는다.

`crash.rs`, `star_nag.rs`, alert native sleep/notification, native clipboard worker, Shell/editor/sidebar/HostManager 호출자의 내부 경계는 coordinator 및 다른 소유자가 수정/검증한다. 관련 경계(`is_loaded`, `flush_file_io`, StartupBounds, request_close, native clipboard async getter)를 전달했다. 이 작업은 PTY·holder·daemon ownership, IPC enum/프로토콜, 하네스/스크립트/lint 설정을 변경하지 않는다.

## 검출 호출 위치 장부

아래는 담당 source에서 재검색한 동기 작업 후보를 함수 단위로 묶은 위치다. `BG`에는 UI callback 안에서 제출하는 backend closure도 포함한다. 타입/심볼의 `load`라는 이름 자체가 IO를 뜻하지 않는 항목(내장 Agents/KeyBinding)은 위 표의 메모리/native 판정을 따른다. 테스트 fixture IO는 제외했다.

### 추가 검수 사항

- `notify_external_change`의 간접 `file_stamps` 호출도 제거했다. 알림은 메모리 stamp 무효화와 coalesced BG reload만 접수한다. reload pending은 별도 ReloadState에 두어 변경 없는 1초 확인이 Config 관찰자와 계정 스캔을 매번 깨우지 않는다. 지연된 큐에서 외부 알림 뒤에도 입력 처리/최신 파일 적용이 가능하고, 변경 없는 폴링의 Config 알림이 0회임을 회귀테스트로 확인했다.
- import의 direct child 종료 뒤 descendant가 stdout/stderr를 붙잡는 경우 기존 join은 전체 deadline을 무력화했다. 공용 `process_io::read`/`PipeTask::finish(deadline + 50ms)`로 교체하고, descendant가 pipe를 보유한 fixture가 TimedOut으로 끝나는 테스트를 추가했다.
- 공용 pipe helper는 Unix에서 nonblocking과 내부 deadline으로 종료한다. Windows의 native read가 여전히 막혀 있으면 호출자는 deadline에 반환하고, 작업 종료까지 admission을 보유해 최대 16개의 private pipe worker 제한을 유지한다. Windows native IO 자체의 강제 취소를 구현했다고 주장하지 않는다.
- 글꼴 목록 worker의 TextSystem은 Send+Sync 계약을 따른다. 공유 native font lock의 최악 경합 시간을 실측하거나 해소했다는 뜻은 아니며, 이 부분은 framework 내부 경합 한계로 남긴다.
- Ghostty/Warp 가져오기도 중복 작업을 제한하고, 그동안 사용자가 선택한 테마를 늦은 Ghostty 자동 선택 결과로 덮지 않는다. UI에서 수행하는 custom theme 병합은 기존 200개 보관 상한의 메모리 작업이다.

| 함수 위치 | 판정 | 후보 호출 위치 |
|---|---|---|
| `accounts.rs:229` `exchange` | BG | `258: write_all(`<br>`260: read_frame(` |
| `accounts.rs:266` `request_local` | BG | `333: std::fs::` |
| `accounts.rs:468` `claude_identity` | BG | `469: Command::` |
| `accounts.rs:581` `lock_login_outcomes` | BG | `584: .lock(` |
| `accounts.rs:600` `take_login_outcome` | UI nonblocking try_lock | `601: .try_lock(` |
| `accounts.rs:608` `run_browser_login` | BG | `618: fs::`<br>`629: std::fs::`<br>`634: std::fs::`<br>`635: std::fs::`<br>`641: std::fs::`<br>`645: std::fs::`<br>`650: std::fs::`<br>`651: std::fs::`<br>`655: Command::`<br>`672: std::fs::`<br>`684: .wait(`<br>`688: std::fs::`<br>`688: read_to_string(`<br>`695: Command::`<br>`697: .status(`<br>`706: thread::sleep(`<br>`710: .wait(`<br>`724: std::fs::`<br>`743: thread::sleep(`<br>`753: std::fs::`<br>`787: std::fs::` |
| `accounts.rs:954` `load_file_result` | BG | `956: std::fs::`<br>`956: read_to_string(` |
| `accounts.rs:977` `save_file` | BG | `978: fs::`<br>`985: std::fs::`<br>`993: std::fs::`<br>`999: write_all(`<br>`1000: .sync_all(`<br>`1001: std::fs::`<br>`1004: std::fs::` |
| `accounts.rs:1035` `refresh_local` | BG | `1071: std::fs::`<br>`1071: read_to_string(` |
| `accounts.rs:1184` `read_profile_usage` | BG | `1185: std::fs::`<br>`1185: read_to_string(`<br>`1202: std::fs::`<br>`1202: read_to_string(` |
| `accounts.rs:1266` `read_codex_rollout` | BG | `1270: std::fs::`<br>`1278: .is_dir(`<br>`1286: .is_file(`<br>`1292: metadata(`<br>`1300: std::fs::`<br>`1301: metadata(`<br>`1305: read_to_end(` |
| `agent_history.rs:30` `daemon_call_for` | BG backend / UI 제출·완료 | `46: .connect(` |
| `agent_history.rs:54` `exchange` | BG backend / UI 제출·완료 | `70: write_all(`<br>`71: read_frame(` |
| `hosts/link.rs:47` `shared` | BG | `49: .lock(` |
| `hosts/link.rs:73` `registrations` | BG | `77: .lock(`<br>`103: .lock(`<br>`113: .lock(` |
| `hosts/link.rs:132` `owned_pids` | BG | `134: .lock(`<br>`138: .lock(`<br>`139: .lock(` |
| `hosts/link.rs:146` `paired` | BG | `147: .lock(`<br>`152: .lock(`<br>`158: .lock(` |
| `hosts/link.rs:165` `shutdown` | BG | `167: .lock(`<br>`169: .lock(` |
| `hosts/link.rs:178` `begin_shutdown` | BG | `181: .lock(` |
| `hosts/link.rs:195` `connector` | BG | `196: .lock(` |
| `hosts/link.rs:239` `ensure` | BG | `256: .lock(`<br>`261: .wait(`<br>`262: .lock(`<br>`267: .exists(`<br>`271: .lock(`<br>`280: .lock(`<br>`285: Command::`<br>`294: .lock(`<br>`309: .lock(`<br>`315: .exists(`<br>`324: thread::sleep(` |
| `hosts/link.rs:328` `stop` | BG | `329: .lock(`<br>`334: .wait(`<br>`335: .lock(` |
| `hosts/link.rs:350` `drop` | BG | `362: .wait(`<br>`363: .lock(` |
| `link_ui/backend.rs:258` `now_secs` | BG | `260: .lock(` |
| `link_ui/backend.rs:265` `set_now_secs` | BG | `266: .lock(` |
| `link_ui/backend.rs:272` `advance_secs` | BG | `273: .lock(` |
| `link_ui/backend.rs:280` `active_invite` | BG | `281: .lock(` |
| `link_ui/backend.rs:285` `set_next_pair_outcome` | BG | `286: .lock(` |
| `link_ui/backend.rs:292` `set_nearby` | BG | `293: .lock(` |
| `link_ui/backend.rs:299` `set_devices` | BG | `300: .lock(` |
| `link_ui/backend.rs:307` `enable_server` | BG | `308: .lock(` |
| `link_ui/backend.rs:313` `disable_server` | BG | `314: .lock(` |
| `link_ui/backend.rs:320` `server_status` | BG | `321: .lock(` |
| `link_ui/backend.rs:327` `create_invite` | BG | `328: .lock(` |
| `link_ui/backend.rs:339` `pair` | BG | `340: .lock(` |
| `link_ui/backend.rs:384` `devices` | BG | `385: .lock(` |
| `link_ui/backend.rs:389` `revoke` | BG | `390: .lock(` |
| `link_ui/backend.rs:395` `nearby` | BG | `396: .lock(` |
| `link_ui/real.rs:34` `run` | BG | `41: block_on(` |
| `link_ui/real.rs:58` `enable_server` | BG | `66: thread::sleep(` |
| `onboarding/actions.rs:256` `stage_helpers` | BG | `265: .is_file(`<br>`270: stage_binaries(` |
| `onboarding/actions.rs:279` `install_managed_cli_at` | BG | `295: std::fs::`<br>`296: std::fs::`<br>`297: std::fs::`<br>`298: std::fs::`<br>`301: std::fs::`<br>`309: std::fs::`<br>`311: std::fs::`<br>`326: std::fs::`<br>`327: std::fs::`<br>`330: fs::` |
| `onboarding/actions.rs:345` `safe_cli_dir` | BG | `348: fs::`<br>`355: std::fs::`<br>`360: .is_dir(`<br>`364: std::fs::` |
| `onboarding/actions.rs:372` `safe_managed_dir` | BG | `382: std::fs::`<br>`383: .is_dir(` |
| `onboarding/actions.rs:393` `canonical_path` | BG | `401: std::fs::`<br>`406: .exists(`<br>`416: std::fs::` |
| `onboarding/actions.rs:579` `hook_installed` | BG | `580: std::fs::` |
| `onboarding/actions.rs:595` `recheck_needed` | BG | `632: .is_dir(`<br>`633: std::fs::`<br>`635: std::fs::` |
| `onboarding/actions.rs:669` `auto_prepare` | BG | `675: install_all_skills_at(` |
| `onboarding/state.rs:85` `load` | BG | `86: std::fs::`<br>`86: read_to_string(` |
| `onboarding/state.rs:105` `migrate_or_fresh` | BG | `106: .exists(` |
| `onboarding/state.rs:130` `atomic_write_0600` | BG | `131: fs::`<br>`133: fs::`<br>`140: std::fs::`<br>`147: std::fs::`<br>`153: std::fs::`<br>`156: write_all(`<br>`157: .sync_all(`<br>`158: std::fs::`<br>`161: std::fs::` |
| `onboarding/welcome.rs:263` `fix_all` | BG | `284: setup_all_agents_and_skills_at(` |
| `onboarding.rs:74` `read_current` | BG | `79: sibling_exe(` |
| `onboarding.rs:142` `login_path` | BG | `144: Command::`<br>`156: fs::`<br>`175: thread::sleep(`<br>`186: recv_timeout(`<br>`188: .wait(` |
| `onboarding.rs:198` `survey` | BG | `202: std::fs::`<br>`202: read_to_string(`<br>`203: ::load(`<br>`204: ::load(` |
| `onboarding.rs:253` `is_broken_symlink` | BG | `254: std::fs::`<br>`254: metadata(` |
| `onboarding.rs:264` `cli` | BG | `268: .is_file(`<br>`269: std::fs::`<br>`271: std::fs::`<br>`272: std::fs::` |
| `onboarding.rs:296` `pending_hooks` | BG | `305: .exists(` |
| `onboarding.rs:311` `mk_hook` | BG | `312: .is_file(`<br>`317: Command::`<br>`321: .output(` |
| `onboarding.rs:331` `install_codex_hook_with_login_path` | BG | `335: .is_file(`<br>`340: stage_binaries(` |
| `onboarding.rs:440` `offer` | BG | `443: ::load(` |
| `onboarding.rs:664` `command_version` | BG | `665: Command::`<br>`669: .output(` |
| `onboarding.rs:722` `quick_state` | BG | `725: .exists(` |
| `onboarding.rs:921` `collect` | BG | `923: .is_file(`<br>`925: stage_binaries(` |
| `onboarding.rs:948` `run_doctor_settled` | BG | `958: thread::sleep(` |
| `onboarding.rs:966` `run_doctor_checks` | BG | `967: Command::`<br>`971: .output(` |
| `onboarding.rs:1058` `stage_binaries` | BG | `1058: stage_binaries(`<br>`1059: .is_file(`<br>`1070: .is_file(` |
| `onboarding.rs:1080` `install_cli_link` | BG | `1081: stage_binaries(`<br>`1083: std::fs::`<br>`1087: fs::` |
| `onboarding.rs:1098` `stage_executable` | BG | `1100: fs::`<br>`1102: std::fs::`<br>`1102: .is_file(`<br>`1106: std::fs::`<br>`1109: std::fs::`<br>`1111: std::fs::`<br>`1112: std::fs::` |
| `onboarding.rs:1136` `access` | BG | `1137: std::fs::`<br>`1137: metadata(` |
| `open_in.rs:46` `command` | BG | `47: Command::` |
| `open_in.rs:56` `open` | BG | `57: std::fs::`<br>`57: .is_dir(` |
| `open_in.rs:67` `open_path` | BG | `68: std::fs::`<br>`77: .output(` |
| `orca_import.rs:584` `find_orca_dir` | BG | `586: .is_dir(`<br>`590: .is_dir(`<br>`597: .is_dir(`<br>`602: .is_dir(` |
| `orca_import.rs:670` `probe_daemon_handshake` | BG | `675: .exists(` |
| `orca_import.rs:701` `find_krill_bin` | BG | `703: .is_file(`<br>`707: sibling_exe(`<br>`708: .is_file(`<br>`715: .is_file(`<br>`723: .is_file(` |
| `orca_import.rs:734` `drop` | BG | `736: .wait(` |
| `orca_import.rs:761` `run_krill_import_with_timeout` | BG | `770: .is_file(`<br>`776: Command::` |
| `orca_import.rs:843` `read_import_output` | BG | `858: thread::sleep(` |
| `platform/menu_bar.rs:96` `take_command` | UI/native | `98: Command::`<br>`99: Command::` |
| `platform/permissions.rs:405` `query_notification_status` | BG | `422: recv_timeout(` |
| `platform/permissions.rs:434` `request_notification_authorization` | BG | `462: recv_timeout(` |
| `platform/permissions.rs:505` `has_quarantine_attr` | BG | `507: fs::` |
| `platform/permissions.rs:523` `probe_full_disk` | BG | `524: std::fs::` |
| `platform/permissions.rs:535` `probe_folder_readable` | BG | `536: std::fs::` |
| `platform/permissions.rs:545` `probe_folders` | BG | `559: recv_timeout(` |
| `platform.rs:20` `has_exec_bits` | BG | `20: std::fs::`<br>`23: fs::` |
| `settings/schedules.rs:87` `capture_session_output` | BG backend / UI 제출·완료 | `93: .exists(`<br>`107: .connect(`<br>`120: write_all(`<br>`121: read_frame(` |
| `settings/schedules.rs:416` `submit_schedule_modal` | BG backend / UI 제출·완료 | `525: exclusive_edit(`<br>`526: ::load(` |
| `settings/schedules.rs:558` `toggle_schedule_enabled` | BG backend / UI 제출·완료 | `566: exclusive_edit(`<br>`567: ::load(` |
| `settings/schedules.rs:582` `remove_schedule` | BG backend / UI 제출·완료 | `591: exclusive_edit(`<br>`592: ::load(` |
| `settings/schedules.rs:613` `trigger_schedule_run_now` | BG backend / UI 제출·완료 | `624: .exists(` |
| `settings/shortcuts.rs:391` `edit_keys` | BG backend / UI 제출·완료 | `393: std::fs::`<br>`393: read_to_string(`<br>`401: std::fs::` |
| `settings/space.rs:56` `process` | BG backend / UI 제출·완료 | `58: Command::`<br>`68: .output(` |
| `settings.rs:434` `bindings` | UI 메모리·내장값 / 설치 callback BG | `479: ::load(` |
| `settings.rs:545` `modified` | BG backend / UI 제출·완료 | `546: std::fs::`<br>`546: metadata(` |
| `settings.rs:640` `run_managed_install` | BG backend / UI 제출·완료 | `674: std::fs::` |
| `settings.rs:1029` `read_config` | BG backend / UI 제출·완료 | `1040: std::fs::`<br>`1040: read_to_string(`<br>`1063: std::fs::`<br>`1063: read_to_string(`<br>`1078: .is_file(` |
| `settings.rs:1323` `sync_hibernation_file` | BG backend / UI 제출·완료 | `1327: .is_file(`<br>`1331: std::fs::`<br>`1333: std::fs::`<br>`1335: std::fs::` |
| `settings.rs:1341` `sync_auto_rename_file` | BG backend / UI 제출·완료 | `1345: std::fs::`<br>`1347: .is_file(`<br>`1351: std::fs::`<br>`1353: std::fs::` |
| `settings.rs:1359` `sync_workspace_trust_file` | BG backend / UI 제출·완료 | `1363: .is_file(`<br>`1367: std::fs::`<br>`1369: std::fs::`<br>`1371: std::fs::` |
| `settings.rs:1457` `persist_settings_edit` | BG backend / UI 제출·완료 | `1462: std::fs::`<br>`1462: read_to_string(` |
| `settings.rs:1493` `save_settings` | BG backend / UI 제출·완료 | `1494: fs::`<br>`1499: std::fs::`<br>`1499: read_to_string(`<br>`1535: std::fs::`<br>`1542: std::fs::`<br>`1548: write_all(`<br>`1549: .sync_all(`<br>`1550: std::fs::`<br>`1553: std::fs::` |
| `settings.rs:1561` `write_settings` | BG backend / UI 제출·완료 | `1568: std::fs::`<br>`1575: std::fs::`<br>`1581: std::fs::`<br>`1584: write_all(`<br>`1585: .sync_all(`<br>`1586: std::fs::`<br>`1589: std::fs::` |
| `settings.rs:1597` `save_agent_profile` | BG backend / UI 제출·완료 | `1601: std::fs::`<br>`1601: read_to_string(`<br>`1606: ::load(`<br>`1618: std::fs::` |
| `settings.rs:1624` `open_file` | BG backend / UI 제출·완료 | `1632: .exists(`<br>`1633: std::fs::`<br>`1637: std::fs::` |
| `settings.rs:2183` `install_agent_hook_at` | BG backend / UI 제출·완료 | `2191: .is_file(` |
| `settings.rs:2201` `install_all_skills_at` | BG backend / UI 제출·완료 | `2201: install_all_skills_at(` |
| `settings.rs:2223` `setup_all_agents_and_skills_at` | BG backend / UI 제출·완료 | `2223: setup_all_agents_and_skills_at(`<br>`2230: install_all_skills_at(` |
| `settings.rs:2237` `agent_install_command` | UI 메모리·내장값 / 설치 callback BG | `2239: ::load(` |
| `settings.rs:2262` `install_hook_file_with_exe` | BG backend / UI 제출·완료 | `2269: std::fs::` |
| `settings.rs:2283` `canonical_hook_path` | BG backend / UI 제출·완료 | `2286: .exists(`<br>`2297: std::fs::` |
| `settings.rs:2306` `run_doctor_check` | BG backend / UI 제출·완료 | `2308: .exists(`<br>`2313: .exists(`<br>`2321: .exists(` |
| `settings.rs:2350` `request_history_clear_on` | BG backend / UI 제출·완료 | `2356: .connect(`<br>`2364: write_all(`<br>`2370: read_frame(` |
| `settings.rs:2403` `request_history_pause_on` | BG backend / UI 제출·완료 | `2413: .connect(`<br>`2421: write_all(`<br>`2428: read_frame(` |
| `settings.rs:2567` `canonical_repo_key` | BG backend / UI 제출·완료 | `2568: std::fs::` |
| `settings.rs:2841` `refresh_font_names` | BG backend / UI 제출·완료 | `2855: all_font_names(`<br>`2857: usable_terminal_fonts(` |
| `settings.rs:2874` `save_agent_edit` | BG backend / UI 제출·완료 | `2888: std::fs::`<br>`2888: read_to_string(`<br>`2890: ::load(` |
| `settings.rs:2951` `refresh_service_cache` | BG backend / UI 제출·완료 | `2971: ::load(`<br>`2975: ::load(`<br>`2985: ::load(` |
| `settings.rs:3247` `ensure_agents_file` | BG backend / UI 제출·완료 | `3262: std::fs::`<br>`3262: metadata(`<br>`3266: std::fs::`<br>`3266: read_to_string(`<br>`3268: ::load(` |
| `settings.rs:7744` `render_agents_tab` | UI 메모리·내장값 / 설치 callback BG | `7908: stage_binaries(`<br>`7910: setup_all_agents_and_skills_at(`<br>`7957: ::load(` |
| `settings.rs:8631` `render_skills_card` | BG backend / UI 제출·완료 | `8663: install_all_skills_at(` |
| `settings.rs:9669` `add_worktree_symlink_for` | BG backend / UI 제출·완료 | `9703: ::mutate(` |
| `settings.rs:9714` `remove_worktree_symlink_for` | BG backend / UI 제출·완료 | `9727: ::mutate(` |
| `settings.rs:9750` `save_repo_hooks_for` | BG backend / UI 제출·완료 | `9779: ::mutate(` |
| `settings.rs:9894` `render_workflows_tab` | BG backend / UI 제출·완료 | `9929: ::mutate(` |
| `ssh_hosts.rs:279` `load_hosts` | BG | `281: std::fs::`<br>`281: read_to_string(` |
| `ssh_hosts.rs:289` `save_hosts` | BG | `296: std::fs::`<br>`298: std::fs::` |
| `ssh_hosts.rs:524` `import_from_ssh_config` | BG | `533: std::fs::`<br>`533: read_to_string(` |
| `ssh_hosts.rs:671` `discover_tailscale_peers` | BG | `681: Command::`<br>`686: .output(` |
| `ssh_hosts.rs:737` `has_default_ssh_key` | BG | `738: .is_file(` |
| `ssh_hosts.rs:742` `generate_default_ssh_key` | BG | `744: fs::`<br>`747: .is_file(`<br>`751: std::fs::`<br>`753: std::fs::`<br>`757: Command::`<br>`763: .output(` |
| `ssh_hosts.rs:799` `test_connection` | BG | `806: Command::`<br>`812: .output(` |
| `ssh_hosts.rs:888` `find_local_runtime_in` | BG | `898: .is_file(` |
| `updater/relaunch.rs:80` `load` | BG | `82: fs::`<br>`82: read_to_string(` |
| `updater/relaunch.rs:91` `save` | BG | `98: fs::` |
| `updater/relaunch.rs:140` `swap_with` | BG | `155: .exists(`<br>`155: fs::`<br>`174: .exists(`<br>`190: fs::`<br>`196: .exists(`<br>`197: .exists(`<br>`200: fs::` |
| `updater/relaunch.rs:213` `sync_bundle_in_place` | BG | `214: fs::` |
| `updater/relaunch.rs:219` `copy_dir_in_place` | BG | `220: fs::`<br>`221: fs::`<br>`226: .is_dir(`<br>`230: fs::`<br>`232: fs::`<br>`234: fs::`<br>`247: fs::`<br>`248: fs::`<br>`248: metadata(`<br>`249: fs::`<br>`252: fs::`<br>`253: fs::` |
| `updater/relaunch.rs:261` `prune_extraneous_entries` | BG | `262: fs::`<br>`267: .exists(`<br>`268: .is_dir(`<br>`269: fs::`<br>`271: fs::`<br>`273: .is_dir(` |
| `updater/relaunch.rs:295` `recover_with` | BG | `296: ::load(`<br>`308: .exists(`<br>`311: fs::`<br>`314: .exists(`<br>`316: fs::`<br>`322: .exists(`<br>`323: fs::`<br>`330: .exists(`<br>`335: .exists(`<br>`336: fs::` |
| `updater/relaunch.rs:352` `rollback_with` | BG | `359: .exists(`<br>`367: ::load(`<br>`372: fs::`<br>`373: .exists(`<br>`375: fs::`<br>`377: fs::`<br>`380: ::load(` |
| `updater/relaunch.rs:404` `relaunch_and_verify` | BG | `424: ::load(`<br>`439: fs::`<br>`446: .is_file(`<br>`447: ::load(`<br>`462: thread::sleep(`<br>`466: .wait(` |
| `updater/relaunch.rs:475` `spawn_bundle_app` | BG | `480: Command::` |
| `updater/relaunch.rs:492` `wait_process_exit` | BG | `512: thread::sleep(` |
| `updater/relaunch.rs:519` `wait_process_exit` | BG | `528: Command::`<br>`532: .output(`<br>`542: thread::sleep(` |
| `updater/relaunch.rs:552` `spawn_detached_relauncher` | BG | `564: fs::`<br>`565: fs::`<br>`567: fs::`<br>`569: Command::` |
| `updater/relaunch.rs:654` `mark_ready` | BG | `668: ::load(` |
| `updater/relaunch.rs:688` `load_rollback` | BG | `695: ::load(`<br>`727: ::load(` |
| `updater/rename.rs:30` `check_install_entries` | BG | `31: fs::`<br>`34: .exists(` |
| `updater/rename.rs:49` `move_tree` | BG | `50: fs::`<br>`51: fs::`<br>`55: .is_dir(`<br>`57: fs::`<br>`62: fs::` |
| `updater/rename.rs:69` `clear` | BG | `70: fs::`<br>`78: .is_dir(`<br>`80: fs::` |
| `updater/rename.rs:89` `purge` | BG | `90: fs::`<br>`95: .is_dir(`<br>`97: fs::`<br>`99: fs::`<br>`102: fs::` |
| `updater/rename.rs:106` `discard` | BG | `107: fs::`<br>`110: fs::`<br>`115: fs::` |
| `updater.rs:227` `load_for_bundle` | BG | `229: fs::`<br>`229: read_to_string(` |
| `updater.rs:303` `read_bundle_info_plist_json` | BG | `305: .is_file(`<br>`309: fs::`<br>`311: Command::`<br>`314: .output(` |
| `updater.rs:589` `validate_bundle_layout` | BG | `597: .is_file(`<br>`607: .is_file(`<br>`610: .is_file(`<br>`616: .is_file(` |
| `updater.rs:625` `validate_install_layout` | BG | `634: .is_file(`<br>`642: .is_file(` |
| `updater.rs:652` `verify` | BG | `663: Command::`<br>`666: .output(` |
| `updater.rs:686` `run_codesign` | BG | `687: Command::`<br>`690: .output(` |
| `updater.rs:700` `fetch_bytes` | BG | `703: Command::`<br>`719: .output(` |
| `updater.rs:768` `find_gh_binary` | BG | `773: .is_file(`<br>`782: .is_file(`<br>`789: .is_file(` |
| `updater.rs:806` `fetch_github_release_asset_with_bin` | BG | `813: Command::`<br>`819: .output(` |
| `updater.rs:879` `stage_verified_archive` | BG | `898: fs::`<br>`903: fs::`<br>`904: fs::`<br>`905: fs::`<br>`909: fs::`<br>`910: fs::`<br>`914: fs::` |
| `updater.rs:1127` `run_update_cli_args` | CLI (그래픽 루프 없음) | `1236: Command::`<br>`1238: .status(`<br>`1258: ::load(`<br>`1273: .exists(`<br>`1273: .is_file(` |
| `updater.rs:1309` `find_running_gui_pid` | BG | `1311: fs::`<br>`1314: Command::`<br>`1316: .output(` |
| `updater.rs:1345` `current_app_bundle_in` | BG | `1351: .is_file(`<br>`1354: .is_file(` |
| `updater.rs:1371` `unpack_with_ditto` | BG | `1372: fs::`<br>`1374: Command::`<br>`1378: Command::`<br>`1382: .output(`<br>`1390: fs::` |
| `updater.rs:1394` `find_extracted_bundle` | BG | `1395: .is_file(`<br>`1398: fs::`<br>`1401: .is_file(` |
| `updater.rs:1411` `reject_escaping_symlinks` | BG | `1412: .canonicalize(`<br>`1413: fs::`<br>`1418: fs::`<br>`1425: .canonicalize(`<br>`1433: .is_dir(` |
| `updater.rs:1440` `write_private_file` | BG | `1450: write_all(` |
