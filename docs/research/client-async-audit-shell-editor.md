# 클라이언트 동기 작업 감사: Shell · Editor · File Format · Terminal · Layout

검사 기준: HEAD `e0a8a4e53a3c0265c4fb8345b54622a8e3bcd370` 위의 공동 작업 중 dirty 트리, 2026-10-20. 이 보고서는 릴리스 전체 검사 통과를 뜻하지 않는다. 기존 dirty 변경과 다른 작업자의 변경을 보존했으며 reset/checkout/commit/push 및 하네스 변경은 하지 않았다.

## 결과와 실행 경계

렌더/파일 탭 전환에서 실행하던 파일 읽기·분류, 저장과 fsync, 재로드, 변경 유지 버튼의 stat, 터미널 파일 링크 stat, 클립보드 파일 준비, 초기 레이아웃/계정/SSH 목록 읽기, 상태바 저장소 읽기를 실제 GPUI background executor로 옮겼다. `cx.spawn`만 씌운 동기 실행은 비동기화로 분류하지 않았다. UI completion은 WeakEntity를 사용하고 경로·pane·에디터 entity id·편집 revision·요청 generation·호스트 Paths identity를 각 작업에 맞게 검사한다.

저장은 앱이 소유하는 직렬 detached 쓰기 큐로 전달한다. 탭이 버려지거나 completion의 WeakEntity가 사라져도 이미 접수한 원자적 쓰기를 취소하지 않는다. `Doc`의 기준 내용은 `Arc<str>`이며, 편집 뒤 전체 내용 비교와 저장용 Rope의 문자열 변환도 background에서 한다. 저장 중 발생한 편집은 성공한 파일 내용과 구분하여 계속 dirty로 남긴다. 저장 성공/실패가 확인되기 전에 닫기를 승인하지 않는다.

`Saver::Drop`은 채널만 끊고 join하지 않는다. 앱 수명 종료 관찰자가 join handle을 보유해 background에서 회수한다. 명시적인 정상 종료는 GPUI의 200ms `on_app_quit` 제한에 의존하지 않고 편집기·설정·사이드바·StarNag 쓰기와 창별 레이아웃 durable barrier를 모두 await한 뒤 `cx.quit()`을 요청한다. native 창 닫기 및 메인 CloseWindow 액션은 `Shell::request_close`의 확인/저장 barrier를 거친다. 저장 실패 시 레이아웃은 pending 상태를 유지하고 닫기 barrier가 실패한다.

붙여넣기는 native async clipboard read와 파일/이미지 준비 전체에 **3초 정책 제한**을 둔다. 같은 pane의 후속 입력은 제출 순서로 유지하고, 다른 pane은 독립적으로 진행한다. 시간 초과/탭 전환/닫기는 해당 paste를 취소하며 늦게 끝난 이미지 결과는 입력하지 않는다. 취소 시 이미 접수한 키는 존재하는 원래 session에만 전달한다. 실제 파일/이미지 준비는 동시에 최대 2개만 허용하며, permit은 UI timeout이 아니라 backend 작업이 실제 반환할 때 해제한다. 따라서 정지한 파일시스템에서 반복 붙여넣기로 worker가 무제한 증가하지 않는다. 이 제한은 성능 측정값이 아니라 UI 입력이 준비 작업을 무기한 기다리지 않게 하기 위한 정책이다.

## 검출 호출 전체 분류

아래 표는 담당 범위의 프로덕션 호출을 caller 단위로 합친 목록이다. 동일 함수 안의 연속 open/read/write/fsync는 한 행에 묶었다. 테스트 및 벤치 전용 호출은 뒤에 따로 기록한다. `PathBuf::join`, 문자열 `.join`, `Metadata::is_file`, 이미 획득한 데이터의 필드 조회는 디스크/스레드 대기가 아니다.

| 호출/정의 | 실제 caller | 판정 및 조치 |
|---|---|---|
| `editor::open`: metadata → File::open → bounded read → classify/raster/CSV | `shell/files.rs::read_file` → `start_file_load`/`reload_file` | **수정**. render의 `sync_files`는 요청만 생성. 읽기와 분류는 background. initial load는 pane/path/request generation, reload는 여기에 Doc snapshot/revision/saving을 추가 검사. |
| `file_format::classify`, `raster::check`, CSV 1MiB head parsing | `editor::open` 및 `prepare_preview` | background. 기존 동기 순수 파서 API는 유지. |
| `ipynb::parse`, 노트북 row projection, SVG 바이트 해시/생성 | `prepare_preview` | **수정**. background에서 파싱/이미지 identity/셀·출력 row index 생성. 노트북은 variable-height GPUI list로 viewport 안의 cell 또는 output만 구성. 한 셀에 출력이 매우 많아도 전체 출력을 한꺼번에 렌더하지 않음. |
| Markdown `TextView` 파싱, gpui image path read/decode | `file_format/view.rs::markdown`/`ImageView::render` → vendored GPUI asset/text loader | 이미 background 경계. gpui-base는 작은 Markdown replace만 4KiB 한도로 동기 처리하고 큰 문서는 background parse. GPUI image asset도 background executor가 읽기·decode를 수행. 불필요한 다른 async 라이브러리 도입 없음. |
| `editor::stamp`: metadata | `watch_files` | 기존 background 유지. 이제 revision + Doc snapshot을 함께 반환하여 저장/재로드 뒤 도착한 옛 stat을 무시. |
| `editor::stamp` | “내 편집 유지” → `keep_file` | **수정**. UI의 즉시 stat 제거. background → Doc/revision/path 검사 후 적용. |
| `editor::stamp` | `save_atomic`의 conflict 검사·최종 stamp | 저장 background 작업 안에서만 호출. |
| `editor::save_atomic`: canonicalize, metadata, writable open, temp create, permissions, write_all, fsync, stamp, rename, cleanup, parent fsync | `Doc::save` → `Shell::save_file` | **수정**. ordered detached background write. path/entity identity 검사 후 saved baseline 반영. 저장 중 추가 편집은 dirty 재계산. |
| `Doc` dirty 비교 및 Rope → String | edit subscription → `file_edited`; Save action → `save_file` | **수정**. UI에서는 persistent Rope/Arc snapshot만 복제하고 비교/문자열화는 background. revision/source baseline fence. |
| CLI 요청 파일 try_exists/open/read/remove, file lock/unlock, JSON decode | `take_pending_file_requests` → `watch_files` | 기존 background + `try_lock` 변경 **보존**. 잠금 경합이면 큐를 소비하지 않고 다음 주기로 미룸. 렌더/각 프레임에서 호출하지 않음. |
| `git ls-files` spawn/poll/pipe read/wait, 파일 존재 확인, fallback read_dir/file_type | `editor/quick_open.rs::try_list` → `toggle_quick_open` | **개선**. background에 3초 process/scan deadline, 8MiB 출력 상한, global scan permit 1개를 추가. `process_io`의 deadline-aware bounded reader를 재사용해 후손이 물려받은 stdout에도 무기한 join하지 않음. admission 경합은 UI timer로 재시도하고 이전 요청은 generation/root fence로 종료. |
| 파일 경로 `is_file` | `terminal/path_link.rs::Cache::link_at` | **수정**. Shell hover/click은 background resolver 호출. UI는 결과 캐시만 사용. hover generation/현재 row/실제 포인터, click 별도 generation/pane/session으로 적용. `Cache`의 동기 API 자체는 worker용으로 유지. |
| OSC8 파일 대상 `is_file` | Shell 마우스 클릭 → `open_terminal_file` | **수정**. background 확인 뒤 source/focus/request fence. |
| URL/OSC8 디코드, 상대 경로 resolve, HOME 경로 구성 | `terminal.rs`, `terminal/path_link.rs` | 순수 파싱/환경값/경로 조합. 디스크 canonicalize/stat 없음. |
| native clipboard read | Shell Paste action → `paste_clipboard` | **수정**. `read_from_clipboard_async` await. macOS 구현의 실제 offload/owner timeout은 coordinator가 vendor 범위에서 담당. |
| clipboard ExternalPaths exists, TIFF→PNG 변환, 임시 폴더·권한·파일 write/fsync | `clipboard_paste_payload`/`save_clipboard_image` → `paste_clipboard` | **수정**. 전 과정 background. total preparation deadline, pane/session/source fence, per-pane order queue. UI는 file I/O를 기다리지 않음. |
| `layout::Layout::load` read/decode/손상 hard-link 보존/rename/parent fsync, try_exists | `Shell::new` → `restore_layout` | **수정**. background 복원. 완료 전 save 및 terminal attach 억제. 사용자 workspace 변경이 있으면 옛 복원 결과로 덮지 않으며, window bounds 변경은 별도로 보존. |
| `Layout::peek_window_bounds` | `main.rs::open_shell` 이전 startup preload | 담당 범위 밖. settings/services worker에게 전달하여 background preload와 연계. 함수 자체는 순수 UI API가 아닌 동기 I/O helper. |
| `Layout::save`, `preserve_corrupt`, `sync_parent`의 open/read/write/fsync/hard_link/remove/rename | Saver worker, editor background save, sidebar/settings background writer | 동기 helper는 유지하고 caller가 background인지 확인. `save`의 postcard 직렬화도 Saver worker. |
| Saver worker Mutex, `Drop::join`, worker recv/recv_timeout | `Shell::save`, 창/앱 종료 | **수정**. UI 소유 상태를 Rc/RefCell로 대체. UI send만 수행. worker recv/파일 저장은 전용 thread. Drop은 disconnect만 하며 background quit hook이 join. 명시 종료에서는 앞서 durable barrier를 await. |
| `accounts::refresh_local`, `ssh_hosts::load_hosts` | `Shell::new`, config 변경/재연결 | **수정**. 생성자 동기 읽기 제거. background snapshot, local/SSH generation 및 daemon link fence. |
| `accounts::request_local` fallback | `Shell::refresh_accounts` | **수정**. local fallback도 background로 이동. IPC 경로는 기존 background 유지. |
| accounts IPC, usage/manual refresh IPC | `refresh_accounts`, `poll_accounts`, `refresh_usage_manually`, status popout account poll | 이미 background. Shell accounts snapshot은 UI 메모리값. |
| `Paths::dev`의 실행파일/설치 경로 찾기, `sibling_exe` | `connect_daemon`, `start_link_hosts` | **수정**. background resolve 이후 WeakEntity completion에서 서비스 설치. |
| `LinkHosts::shared`, registrations의 registry/lease mutex 및 프로세스 작업 | `start_link_hosts` | **수정**. shared 서비스 생성과 registrations 모두 background. 상태 경로 identity 검사 후 설치. |
| HostManager `sync_specs`, `sync_external_hosts`, `specs`, `statuses`, `target_for`, `reconnect_now` | host sync, 새 세션 메뉴, pane host label, attach/close target 선택 | **수정**. 모든 실 manager 호출을 ordered detached background job으로 옮김. UI는 `host_statuses`/`host_targets` accepted snapshot만 조회. generation+Paths fence, 바뀐 source의 옛 target은 즉시 무효화. 동일 source의 주기 refresh는 active 작업 1개 + 최신 재요청 1개로 합쳐 오래 걸리는 manager 작업 뒤에 obsolete 작업이 쌓이지 않으며 reconnect 요청 순서를 보존. manager lock의 실제 네트워크 장기 보유 여부와 무관하게 UI lock/process-probe 경계를 제거. |
| `OrchestrationStore::load_cached`의 global mutex/metadata/read | `render_status_bar` | **수정**. render 호출 제거. 별도 serial background poll이 Arc snapshot을 갱신. UI는 accepted memory만 사용. |
| `OrchestrationStore::mutate`/file lock/write | popout 질문 답변, gate 처리 | 기존 background 유지. 결과/오류만 UI에서 적용. |
| 화면 URL 정규식 | `render_status_bar` | **수정**. terminal snapshot의 문자열화/URL 추출을 background poll에서 수행. render는 결과만 읽음. |
| 프로젝트 git root 판정/normalize/read/atomic write | new folder/clone/create → `sidebar::external::register_project` | **수정**. coordinator의 특별 승인으로 `Sidebar::register_project` 한 메서드만 추가. 기존 ordered sidebar writer를 사용하여 제거와 직렬화하고 정상 종료 flush 대상에 포함. |
| `updater::mark_ready`의 파일 쓰기 | `sync`, attach/cold completion | **수정**. background 작업으로 이동. |
| `StagedUpdate::spawn_relauncher`의 프로세스 시작 | update poll automatic restart 및 update card 버튼 | **수정**. process spawn은 background. 성공 뒤 UI completion에서 공통 pre-quit 경로로 종료. |
| IME trace open/append/write | 키/IME 이벤트 → `ime_trace` | **수정**. bounded try_send 채널 → 전용 worker. trace가 막히면 diagnostic만 드롭하고 키 이벤트에 backpressure를 걸지 않음. |
| `file_format::open_default_command`의 executable metadata + command.status | placeholder Open/Reveal 버튼 → `spawn_open` | **수정**. 기존 process wait뿐 아니라 command를 만드는 metadata 검사도 background 안으로 이동. |
| 800ms 주기 ChatPoll IPC | `shell/chat.rs::poll_session_chat` | **수정**. 기존 task를 매번 취소/교체하지 않고 요청 1개로 합침. 완료 때 generation/host/pane/session/offset을 검사하고 바뀐 source의 응답을 거부. 다른 활성 session이 대기 중이면 완료 후 다음 poll 시작. |
| session attach/resume/load_cold/frame read, appearance connect, recording check | `spawn_pane`, daemon connection task | 이미 background. `on_frame`은 메모리 protocol event/UI 처리. Writer의 send는 coordinator 검토 대상의 queue-only API. |
| Release capture/List/Close/IPC·file lock·sleep, Handoff readiness capture/wait/sleep | `release_worker_blocking`, `wait_agent_ready` | 모두 background executor caller. UI에서는 WeakEntity result/상태만 적용. sleep의 존재만으로 결함으로 분류하지 않음. |
| `Creation::publish`의 Mutex | `spawn_pane` 내부 background create 완료 | background 유지. |
| 명시 pane/tab close의 Creation mutex | `close_removed_sessions` | 공동 작업 중 발견된 기존 세션 종료 변경을 보존하며 **수정**. UI는 try_lock만 시도. 경합이면 cleanup ownership을 background로 넘겨 lock을 기다리고 Close까지 수행. view가 사라져도 cleanup을 완료. 중복 종료 및 다른 pane이 쓰는 session 보호. |
| `Live::cut` shutdown, reader handle try_clone | UI 연결 teardown/attach completion | peer 응답/프로세스 종료를 기다리지 않는 OS 소켓 종료 및 handle duplication. PTY를 소유/닫지 않음. blocking 데이터 read/write는 없음. |
| 플랫폼 focus/IME/native prompt/메뉴/클립보드 문자열 설정 | UI 이벤트 | native main-thread API. 파일 읽기·process wait와 구별. 플랫폼 구현의 추가 native blocking 검토는 coordinator/platform 소유 범위. |

## 테스트/벤치에서만 검출된 호출

`editor/tests.rs`, `layout/tests.rs`, `terminal/tests.rs`, `terminal/path_link/tests.rs`, `shell/**/tests.rs`, cfg(test) inline 테스트의 fs/process/join/sleep/mutex 호출은 fixture 생성·assertion·가짜 데몬용이다. 프로덕션 UI 경로가 아니다. 새 회귀에서는 oneshot gate로 worker를 보류해 deterministic하게 foreground 입력/그리기를 검증한다. 실제 contention 회귀는 작업자 lock을 잡은 상태에서 UI close가 먼저 반환하고 cleanup이 뒤따르는지 검사한다.

`Saver::flush`의 blocking recv는 cfg(test) 전용이며 production barrier와 구분했다. `shell/popout/bench.rs`의 footprint/ps 외부 프로세스는 background에서 실행되도록 CPU 측정 부분까지 수정했다. 벤치의 결과 출력과 테스트 타이머는 제품 UI 실행 경로가 아니다.

## 검증

빌드 산출물은 모두 `CARGO_TARGET_DIR=/Volumes/krill-build/targets/krill`에 두었다. 담당 Rust 파일만 rustfmt했고 저장소 전체 포맷을 덮어쓰지 않았다.

- `cargo check -p krill-app --locked`: 통과(`/tmp/krill-shell-check3.log`; 그 뒤 변경은 아래 회귀 재빌드에서 컴파일 확인).
- 최신 editor/file_format/Shell 합동 targeted regression: **69개 통과**, `/tmp/krill-shell-tests7.log`, `cargo nextest run -p krill-app --locked --no-fail-fast`와 명시적 test filter. 여기에는 5만 notebook cell/10만 projected row, 느린 ChatPoll, 프로세스/inherited-pipe deadline이 포함된다.
- 이전 Shell targeted regression: 32개 통과, `/tmp/krill-shell-tests6.log`.
- `cargo clippy -p krill-app --all-targets --locked -- -D warnings`: 마지막 실행은 담당 밖 `sidebar/async_state.rs:544`, `sidebar/tests.rs:1763` 테스트 fixture의 세미콜론 lint 2개로 실패(`/tmp/krill-shell-clippy-final2.log`). 담당 코드 진단은 모두 수정했으며 두 외부 진단은 coordinator에게 전달. 전체 clippy 통과라고 보고하지 않는다.
- 담당 파일 `rustfmt --check --config skip_children=true` 및 `git diff --check`: 통과.
- coordinator의 broad suite에서 발견된 담당 영역 11개 회귀(초기화 완료 전 스냅샷 주입/즉시 read 단언, background ready marker, 초기 기본 레이아웃 불필요 저장, i18n 로그)는 수정 후 **11개 모두 통과**, `/tmp/krill-shell-fixtures.log`. 단언은 유지하고 완료 경계를 기다리도록 바꿨다. vault 공통 원인 수정은 settings/services worker 소유.
- Windows test 빌드에서 발견된 새 ChatPoll 회귀의 Unix fixture import는 해당 테스트만 `cfg(unix)`로 제한했다. 최종 Windows check/clippy 결과는 coordinator가 기록한다.
- Host refresh coalescing 이후 마지막 좁은 회귀: **9개 통과**, `/tmp/krill-shell-final-regressions.log`; host coalescing, ChatPoll, quick-open admission/deadline, notebook virtualization, clipboard 순서/기한/독립성/늦은 결과 거부. source 변경은 모두 마감했으며 후속 통합 검증은 coordinator 소유.
- 최종 clipboard backend admission, Unix-gated ChatPoll, scope-path 갱신 이후 palette smoke: **3개 통과**, `/tmp/krill-shell-last-smoke.log`. 이후 source는 동결했으며 report 기록만 갱신.
- 워크스페이스 전체 릴리스 검사 및 Windows 최종 통합 검사는 coordinator 소유이며 여기서 통과를 주장하지 않는다.

추가한 핵심 회귀:

1. 느린 파일 열기 동안 draw/탭 교체 가능, 늦은 preview 결과 폐기.
2. save/fsync 대기 동안 입력 유지, 저장된 snapshot과 이후 dirty 편집 분리, inflight save로 닫기 승인 금지.
3. 늦은 reload가 새 입력을 덮어쓰지 않음.
4. 탭 discard 뒤에도 접수한 save 완료 및 pre-quit editor barrier가 실제 쓰기 종료를 기다림.
5. Saver Drop이 정지한 worker를 join하지 않음; 최종 레이아웃은 명시 worker 완료 후 최신 값으로 읽힘.
6. file request lock 경합에서 대기/큐 소비 없이 다음 주기로 미룸(기존 회귀 유지).
7. 느린 두 paste가 역순 완료돼도 paste/키 입력은 제출 순서 유지.
8. 다른 pane의 미완료 paste가 준비된 paste를 막지 않음.
9. clipboard 준비 deadline이 typing을 해제하고 늦은 이미지 결과를 거부.
10. 탭 전환 뒤 원래 paste가 새 session으로 전달되지 않음.
11. Creation lock 경합 중 UI close/새 탭 작업 진행 및 후속 정확한 session cleanup.
12. 느린 host source 작업 동안 실행 위치 메뉴/target 조회/draw 가능, 폐기된 source 결과 거부. 후속 검증에서는 반복 refresh 100회의 coalescing도 검사.
13. ChatPoll worker가 보류된 동안 40회의 추가 요청이 중복 IPC를 만들지 않고, host 변경 뒤 옛 응답을 버림.
14. `git` 직접 정지 및 후손이 stdout을 물려받은 경우에도 process/read deadline으로 반환.
15. 5만 notebook cell과 각 output(총 10만 row) 중 viewport 밖 row는 UI 요소를 생성하지 않음.
16. UI 요청을 취소해도 backend 준비 permit은 점유 상태를 유지하고, 실제 guard 반환 뒤에만 새 준비를 허용.

## 남는 한계와 외부 경계

- `EditorState::default_value/set_value`의 Rope/display-map 구성은 vendor의 UI entity API 안에서 수행된다. 입력 파일은 기존 4MiB 상한을 유지하며 비교·파싱·저장 문자열화는 background로 옮겼다. prepared-Rope API가 없는 vendor 경계는 coordinator에 전달했다. UI entity 생성 자체의 비용이 0이라고 주장하지 않는다.
- Markdown의 작은 4KiB 동기 구간, CSV 열 너비 계산(최대 100행 × 256열), terminal 화면 복사 및 native UI 레이아웃/그리기는 명시적인 bounded CPU 작업이다. 큰 파일/느린 디스크/장기 프로세스 대기와 구분한다.
- 파일시스템이 영구 정지하면 해당 worker 작업 자체는 남을 수 있다. foreground executor는 기다리지 않는다. 정상 quit은 저장 내구성을 위해 마지막 쓰기를 기다리고, clipboard는 별도 3초 deadline으로 입력 큐를 해제한다.
- atomic save의 외부 작성자 검사와 rename 사이 경합은 기존 문서화된 범위다. 다른 프로세스의 비협조적 쓰기를 완전 배제하지 않으며, 실패/프로세스 강제 종료 시 이전 파일 또는 완성된 새 파일을 남기는 원자적 저장 계약을 유지한다.
- App/daemon/CLI 실패의 PTY 생존 계약은 바꾸지 않았다. 파일/레이아웃 작업자는 PTY를 소유하지 않는다. 명시 session 종료에 관한 기존 공동 변경은 보존했고, lock 대기만 UI에서 제거했다.
