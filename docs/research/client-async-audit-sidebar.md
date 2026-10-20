# 클라이언트 동기 작업 조사 및 수정: Sidebar

대상: `crates/krill-app/src/sidebar.rs`, `src/sidebar/**` 전체. 기준 HEAD는 `e0a8a4e53a3c0265c4fb8345b54622a8e3bcd370`이며, 이미 존재하던 다수의 dirty 변경과 새 `sidebar/hooks.rs`를 보존한 작업 트리를 검사했다. 담당 범위 밖 코드, 하네스, 프로토콜 및 PTY 소유권은 수정하지 않았다.

## 변경 결과

`Sidebar::new`는 메모리만 초기화한다. 첫 connect/rebuild/render에서 `async_state.rs`의 실제 background executor가 초기 설정, 에이전트 프로필, 계보 이관, 외부 프로젝트와 아이콘을 읽는다. UI는 마지막 수락된 `WorktreeStore`·`OrchestrationStore` 및 프로젝트 메타 스냅샷만 사용한다. 디스크 결과는 쓰기 세대, 쓰기 진행 상태, 저장소 트리 및 작업 폴더 집합이 여전히 일치할 때만 수락한다. 변경 없는 디스크 결과는 notify/rebuild하지 않는다.

설정·계보·프로젝트 제거·훅 저장은 앱 소유의 oneshot tail을 공유해 순서대로 실행한다. 선행 작업 대기는 background future에서만 이루어지고, 파일 작업 자체도 background executor에서 실행된다. 작업을 detach하므로 창/Sidebar가 사라져도 제출한 쓰기는 남으며 `on_app_quit`이 최종 tail을 비동기로 기다린다. 결과는 WeakEntity completion에서만 UI에 반영하며 오류를 표시한다. 최초 로드 중의 이름·핀 등 현재 메모리 값은 초기 상태와 병합한 후 저장한다. 명시적 종료용 `sidebar::flush_file_io(&mut App) -> Task<()>`는 초기 로드 completion이 제출하는 지연 저장까지 기다리고 나서 순차 쓰기 barrier를 기다린다. 셸 담당자에게 API를 공유했다.

## 호출자별 전수 분류

| 경로 / 검출 호출 | 호출자 및 원래 판정 | 수정 또는 현재 실행 경계 |
| --- | --- | --- |
| `load_state`: File::open/take/read_to_end, postcard decode, 손상 보존 rename, try_exists | `Sidebar::new`, UI | `Startup::load` worker로 이동. 생성자는 파일을 열지 않음. 손상 보존·쓰기 비활성화 동작 유지 |
| `Agents::load`, 자동 제목 설정/JSON, 코멘트 TOML 읽기 | 생성자와 poll completion, UI 경로 존재 | Startup/DiskSnapshot worker 및 기존 source poll worker에서만 읽음 |
| legacy 계보 파일 존재 검사와 WorktreeStore::mutate/filelock | 생성자, UI | Startup worker에서 이관. 에러를 UI에 전달 |
| include_imported_projects/apply_imported_project_prefs/imported_repo_paths | 생성자·apply completion·reload_external, UI | DiskSnapshot worker에서 TOML 읽기·정규화·git 연결 파일 조사. UI apply는 완료된 imported snapshot을 병합 |
| render_row/session 및 render_worktree의 WorktreeStore/OrchestrationStore::load_cached | 렌더링, UI; cached도 metadata+mutex 수행 | `checkpoint_parts_from_store`, `orch_parts_from_store`로 분리, 메모리 store 참조만 전달 |
| local_rows, issue_for의 load_cached | rebuild/메뉴/렌더, UI | `self.disk.store`만 조회 |
| merged_parents/load_store_parents의 metadata·부모 전역 cache mutex·Store::load | 계보 깊이·부모 후보·자손·메뉴·rebuild·apply, UI | 전역 IO cache 제거. worker가 계보를 검증해 메모리에 게시. 조회는 메모리만 사용하고 행마다 전체 계보를 재검증하지 않음 |
| sync_project_visuals/resolve_project_visuals | new/rebuild/변경 없는 poll completion, UI | DiskSnapshot worker로 이동. 결과가 바뀔 때만 다시 그림 |
| 아이콘 감지의 package.json·HTML·git config 읽기, .git/commondir, fs metadata/canonicalize/exists, magic 검사, base64/SHA 파싱, icon caches mutex | 위 project visuals를 통해 UI에 도달 | 모두 worker 내부. 아이콘 다운로드 curl/output과 캐시 쓰기는 기존 별도 thread 유지. 렌더는 준비된 경로/벡터만 전달 |
| save_state: postcard encode, mkdir/OpenOptions/write_all/sync_all/rename/sync_parent | pin/rename/fold/filter/selection/keyboard 등 write_state와 persist, UI | 값 스냅샷만 UI에서 캡처. 인코딩부터 디스크 동기화까지 순차 background 큐 |
| remove_imported_project: read/parse/serialize/write/rename | remove_project 클릭, UI | 순차 background 큐. UI에서 removed_projects를 즉시 적용 |
| WorktreeStore::mutate 및 worktree_key 정규화 | set/clear parent, 삭제 성공 completion의 계보 정리, UI | 부모 맵은 낙관적으로 갱신하고 디스크 mutation은 순차 background 큐. 삭제 요청 IPC는 원래 background 유지 |
| worktree::list_git_refs | 새 워크트리 모달 열기, UI | 초기 브랜치 제안은 메모리 meta 사용. refs 수집은 worker, 살아 있는 해당 Dialog의 WeakEntity에만 후보 전달 및 입력이 비어 있을 때만 ref 기본값 복원 |
| fs::metadata/modified | Jump 최근 작업 공간 생성, UI | folder_updated_at worker 스냅샷 사용 |
| HostManager::statuses/connection_info/target_for | sync_remote_hosts·ensure_remote_sources, UI; mutex·tunnel try_wait·factory 포함 | 전체 worker로 이동, remote_generation으로 오래된 목록 결과 차단 |
| HostManager::connection_info | 원격 fetch completion, UI | 마지막 remote_info 메모리 스냅샷 사용. poll 결과는 source Arc identity가 같을 때만 반영 |
| Source::comments/fetch/meta, Socket::with_conn/dirty/set_visible | local/remote polling 및 hide 작업 | 원래 background 실행: 유지. local poll source 교체/refresh에 poll_generation fence 추가 |
| ipc::Socket mutex(conn/watches/external), UnixStream connect/read/write/timeouts, git HEAD 파일 읽기 | 위 Source worker | safe: UI에서 직접 호출하지 않음 |
| ipc::ExternalCache: resolve_claude_bin/executable metadata, claude CLI spawn/output/try_wait/sleep/kill/wait, stdout reader join 및 JSON parse | Source::fetch worker | safe: 기존 동기 worker 구현 유지. 타이머는 gpui bg.timer이며 UI sleep 없음 |
| external::parent_map의 ps/output, process kinship CPU | ipc::fetch_from worker | safe: worker에서만 호출 |
| external::discover_claude_worktrees: metadata/read_dir/file_type/canonicalize/cache mutex | Source poll worker | safe: worker 경계 유지, UI completion에서 다시 탐색하지 않음 |
| external::register_project, load/save_comment 및 경로 canonicalize/TOML/filelock | 공개 helper, sidebar 밖 shell/folder 또는 테스트 | 담당 sidebar UI에는 호출 없음. shell/folder의 background 경계는 해당 담당자 통합 범위 |
| 공개 checkpoint_badge_* / session_orch_* 디스크 helper | 외부 settings/sidebar 단위 테스트; production render에서는 더 이상 호출 안 함 | 디스크형 helper는 호환 유지. UI에는 `_from_store` 함수만 사용 |
| hooks::inspect_repo_hooks 및 setup script mutation | 기존 hooks.rs worker | 감지 worker 유지. repo 목록·훅 세대·디스크 세대·saving 검사로 stale 결과 차단. 저장은 공통 순차 큐에 통합 |
| PR service refresh/create, git/gh/cache mutex | sidebar/pr.rs의 worker | 원래 safe. branch별 completion fencing 유지 |
| 메뉴 전용 계보·부모 후보·라벨 계산 | 매 visible row 렌더마다 계산하던 CPU 작업 | context_menu를 실제 여는 callback에서만 계산. 부모 후보는 저장소당 하나의 부모 맵으로 검사 |
| Target::open 프로세스 실행 | Open in 클릭 | 기존 background executor 유지; 테스트에서만 캡처 stub |

프로젝트 이미지의 실제 파일 읽기/디코딩은 GPUI `ImageAssetLoader::load`가 만든 Send future를 asset 시스템의 background executor에서 실행한다(`vendor/gpui-pre/src/elements/img.rs`, `asset_cache.rs`의 CachedLoad::new). Sidebar 렌더러가 파일을 열지는 않는다. UI에서 남는 작업은 gpui entity/global의 메모리 접근, 행 투영·정렬, 이벤트/포커스 처리와 디자인 토큰 렌더링이다. 세션 holder/PTY 작업이나 새로운 UI 기능은 추가하지 않았다.

## 테스트 및 검증

검사 결과는 아래와 같다. Cargo 산출물은 모든 명령에 `CARGO_TARGET_DIR=/Volumes/krill-build/targets/krill`을 지정했다. 담당 Rust 파일에만 rustfmt를 실행했다. 전체 workspace/릴리스 검사로 보고하지 않는다.

- `cargo test -p krill-app --locked sidebar:: --lib`: 새 회귀 4개를 포함한 73개 모두 통과. 최종 코드에는 메뉴 지연 계산, 후보 검사 최적화, 빈 원격 polling task 방어, 입력을 덮지 않는 비동기 ref 기본값 복원이 모두 포함된다. 출력: `73 passed; 0 failed; 0 ignored; 0 measured; 639 filtered out`.
- `cargo clippy -p krill-app --locked --lib -- -D warnings`: 최종 실행은 담당 범위 밖 lint 19개로 실패했다. sidebar 진단은 0개이며 onboarding.rs redundant closure, orca_import.rs excessive bools, settings.rs와 settings/schedules.rs·shortcuts.rs의 must-use/clone/default/closure/semicolon 지적을 조정자에게 전달했다. 부분 범위가 깨끗한 것을 전체 clippy 통과로 보고하지 않는다.
- 담당 파일 rustfmt 검사와 git diff --check 통과.
- 기존 사이드바 검사 첫 실행: 69개 중 64 통과. 나머지 5개는 async 저장/외부 reload의 완료 대기와 stable snapshot 재그리기 회귀였고 수정했다.
- `new_loaded`는 기존 저장/표시 fixture의 명시적인 테스트 전용 준비 함수다. production `new`는 IO를 하지 않으며 신규 회귀에서 별도로 검사한다.
- `blocked_write_queue_keeps_ui_running_and_saves_in_order`: 느린 선행 backend/lock을 pending oneshot으로 통제한다. 쓰기 미완료 중 foreground tick이 진행하고, Sidebar를 drop한 뒤에도 3개 쓰기가 순서대로 끝나는지 검증한다.
- `initial_load_preserves_edits_and_stale_file_scan_cannot_restore_parent`: 초기 로드 중의 이름 수정 유지 및 이전 파일 스냅샷의 부모 복원 방지.
- `explicit_flush_includes_edits_waiting_for_initial_load`: 새 Sidebar에서 초기 읽기가 끝나기 전에 핀을 수정하고 즉시 flush를 요청해도 파일에 저장 완료된 뒤 barrier가 끝남을 검증한다.
- `new_does_not_open_files_and_render_uses_captured_badges`: 읽을 수 없는 상태 경로에서도 new가 파일을 검사하지 않고, 배지/이슈/계보를 캡처한 메모리에서 읽는지 확인한다.
- 다른 담당 파일의 진행 중 컴파일 오류는 조정자에게 전달했다(settings, onboarding/orca_import trait import, diff/tests VisualTestContext 읽기). 해당 파일은 수정하지 않았다.

## 남는 한계

- source IPC/외부 Claude CLI와 icon curl은 background에서 동기 실행한다. 느린 디스크/네트워크는 데이터 갱신을 지연할 수 있지만 UI 스레드에서 기다리지 않는다. Claude stdout reader는 자손이 파이프를 계속 보유하면 join이 CLI 직접 자식의 timeout 이후에도 지연될 수 있는 기존 한계가 있다.
- 명시적 종료는 프레임워크 quit 전에 flush barrier를 기다리며, 직접 GPUI 정상 종료 경로에는 shutdown timeout 안에서 쓰기 tail을 기다리는 fallback이 있다. kill -9·정전과 그 timeout을 넘는 고장난 저장 장치의 내구성은 보장하지 않는다. PTY 생존 계약은 변경하지 않았다.
- UI의 행 투영·정렬은 메모리 연산으로 남는다. 읽는 파일 상한은 기존 프로토콜/모듈 제한을 따르며, 수 분 정지의 원인이었다고 측정하지 않은 CPU 수치나 지연 수치는 주장하지 않는다.
