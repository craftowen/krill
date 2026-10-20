# Krill 클라이언트 멈춤 조사와 비동기 전환

2026-10-20. 기준 HEAD `e0a8a4e53a3c0265c4fb8345b54622a8e3bcd370` 위의 기존 변경을 보존한 공유 작업 트리. 이 문서는 개발 검증 기록이며 릴리스 전체 검사 결과가 아니다. 최종 검증 및 동결 기록은 아래에 추가한다.

## 확인한 문제

사용자가 겪은 수분간 정지한 순간 자체를 포착하지는 못했다. 실행 중인 앱 표본(`/tmp/krill-app-hang-sample.txt`)과 전체 클라이언트 호출 경로 조사에서 UI의 파일 접근·설정/저장소 조회·프로세스 상태 조회를 확인했다. 따라서 특정 호출이 실제 장애의 유일한 원인이었다고 단정하지 않는다.

별도로, 클립보드가 응답하지 않을 때 붙여넣기 뒤의 키 입력이 무기한 보류되고 다른 pane까지 영향을 받을 수 있는 입력 대기 경로를 확인했다. 이 경로는 OS 창이 계속 그려지더라도 터미널이 무반응처럼 보일 수 있다. HostManager의 UI getter도 죽은 SSH 터널을 정리하면서 프로세스 대기와 파일 삭제를 수행할 수 있었다. SSH 연결 대기 중 슬롯 잠금을 유지한다는 초기 가설은 소스 재검증으로 기각했다. 실제 연결 대기 전 잠금을 푼다.

## 변경

| 경로 | 조치와 보장 |
|---|---|
| Sidebar·hooks·프로젝트·lineage | 렌더는 수락한 메모리 스냅샷만 조회. 초기 파일/저장소 읽기, hook 검사/저장, 프로젝트 등록과 metadata 변경은 background. 순서 있는 앱 소유 쓰기 큐와 초기화-aware flush로 창이 사라져도 접수한 쓰기를 보존. |
| 에디터·파일 탐색·검색·diff | 읽기/분류/preview 파싱/저장/fsync/stat/파일 변경/Git을 background로 이동. 파일·pane·편집 revision·요청 세대 검증으로 늦은 결과를 거부. Explorer 작업은 제출 순서대로 처리하며 다른 폴더를 여는 최신 조작을 보존. |
| 설정·계정·호스트·시작 복원 | 생성자와 UI callback의 파일/IPC/프로세스 작업 제거. 직렬 쓰기·Config epoch/revision·호스트 경로 identity 검사. 설정 외부 변경 알림은 메모리 무효화 후 coalesced reload. |
| 클립보드·입력 | macOS/Windows native clipboard는 전용 소유 스레드. native read 정책 제한 2초, 전체 paste 준비 제한 3초. 대기열과 read admission 제한. 동일 pane 입력 순서는 보존하고 다른 pane은 독립 진행. 창/포커스/세션 변경과 timeout 뒤 늦은 결과 거부. 큰 데이터 복제/해제는 공유 스냅샷 잠금 밖에서 수행. |
| 백그라운드 작업 폭증 | chat poll·호스트 refresh·quick-open을 합치고 작업 수 제한. quick-open Git은 출력 상한과 deadline 적용. notebook cell/output은 미리 준비한 행과 가상 리스트로 렌더. |
| 외부 프로세스 | `process_io`는 Unix nonblocking pipes, 절대 deadline, 최대 16개 pipe worker admission을 제공. 직접 자식이 종료돼도 후손이 pipe를 보유하는 경우 blocking join 금지. diff/검색/git-ignore/usage/import/quick-open/PR·refs helper에 적용. |
| alert·crash·StarNag·종료 | OS sleep assertion은 같은 전용 스레드에서 acquire/drop. crash 수집/묶음 저장과 StarNag 읽기/쓰기는 background. 정상 종료는 에디터/레이아웃/설정/sidebar/StarNag 저장 barrier를 await 후 quit. GPUI의 짧은 quit hook만으로 저장 완료를 가정하지 않음. |

UI 이벤트 처리와 순수 메모리 계산 자체를 무조건 async로 바꾸지는 않았다. 창/IME/포커스/메뉴/그리기 API는 플랫폼 스레드에 남긴다. 동기 helper는 worker 내부에서 사용할 수 있으며, `async` 문법이 아니라 실제 실행 경계를 검사했다. PTY 소유권과 IPC 와이어 프로토콜은 변경하지 않았다.

## 전수조사 장부

- [Sidebar 전체 호출 경로](client-async-audit-sidebar.md)
- [Shell·Editor·파일 형식·Terminal·Layout](client-async-audit-shell-editor.md)
- [Settings·Accounts·Onboarding·Platform·기타 서비스](client-async-audit-settings-services.md)
- [전체 모듈 교차 검토와 발견 사항별 처분](client-async-audit-independent-review.md)

Orca orchestration run `run_86b4a98344c9`에서 범위를 나눈 세 작업자를 감독했다. Sidebar 담당은 이후 다른 범위의 교차 검토를 수행했고, 자신이 수정한 Sidebar 부분은 독립 검토라고 주장하지 않는다. 기존 다른 세션의 dirty 변경은 보존했다. 작업 중 별도 릴리스 담당에게 진행 중인 추가 변경과 동결 대기를 전달했다.

## 한계

- OS 파일시스템이 영구 정지하면 background 작업 자체와 저장을 기다리는 정상 종료가 pending으로 남을 수 있다. 이때 UI는 진행하며 데이터를 버리고 종료한 것으로 간주하지 않는다.
- Windows의 표준 child pipe native read/write를 강제 중단하지는 않는다. 호출자는 deadline으로 돌아오고 실제 남은 pipe 작업은 공통 최대 16개 admission을 계속 점유한다. Windows에서는 크로스 컴파일/Clippy로 확인하며 실제 지연 제공자 실행 검증과 구별한다.
- native clipboard 제공자가 영구 정지하면 소유 스레드는 남는다. 추가 read는 거부하고 큐는 제한한다. 큐가 가득 찬 copy는 성공한 캐시로 표시하지 않고 로그를 남긴다. 기존 동기 clipboard API는 마지막 로컬 스냅샷만 제공하며 제품 paste는 최신 async 읽기를 쓴다.
- GPUI editor entity의 최대 4MiB Rope/display-map 구성, 작은 Markdown 처리, 화면 글꼴·그리기는 UI CPU 작업이다. background font 열거가 내부 플랫폼 글꼴 잠금 경합까지 제거한다는 보장은 하지 않는다.
- 모든 미래 정지를 불가능하게 했다고 주장하지 않는다. 이번에 확인한 UI blocking 경로와 무한 입력 대기를 제거하고, 지연·취소·오래된 결과·종료 저장에 대한 회귀를 추가했다.

macOS 객체는 한 소유 스레드에서만 사용하고 각 작업에 autorelease pool을 둔다([Apple thread safety](https://developer.apple.com/library/archive/documentation/Cocoa/Conceptual/Multithreading/ThreadSafetySummary/ThreadSafetySummary.html)). Windows `PeekNamedPipe`도 동기 handle에서 block할 수 있어 이를 무조건적인 timeout 보장으로 사용하지 않았다([Microsoft 문서](https://learn.microsoft.com/en-us/windows/win32/api/namedpipeapi/nf-namedpipeapi-peeknamedpipe)).

## 최종 검증

검증 대상은 위 HEAD의 작업 트리이며, 소스 snapshot `8080edc64c5083f6037d76af37e6f77b28e36214` 및 `/tmp/krill-async-final-source-sha256.tsv`로 보존했다. 마지막 manifest는 앱 전체와 vendor 변경을 합친 178개 파일이며 SHA256은 `9bba2986db2499434af77595f904c48bb7c87ccb81cdba06e4bae53dd84f559b`다. 이 파일 수는 기존 변경을 포함한 검사 범위이며 이번 작업의 순수 변경 파일 수가 아니다.

- `cargo nextest run -p krill-app --locked --no-fail-fast`: **958 passed, 2 ignored**, `/tmp/krill-async-app-final-tests.log`. 앱 kill -9 재연결, 업데이트·롤백 세션 생존 통합 테스트 포함.
- `cargo clippy -p krill-app --all-targets --locked -- -D warnings`: **exit 0**, `/tmp/krill-async-clippy-final2.log`.
- native macOS clipboard worker: **4 passed**, `/tmp/krill-async-clipboard-tests.log`. 실제 사용자 clipboard는 건드리지 않고 지연/queue-full/오래된 결과를 모형화.
- `cargo fmt --all -- --check`, `git diff --check`: **exit 0**. Cargo 타깃은 외장 `/Volumes/krill-build/targets/` 아래만 사용.
- 위 통과 뒤 다른 세션이 추가한 아이콘 테스트 2개를 포함해 다시 검사: **959 passed, 1 failed, 2 ignored**, `/tmp/krill-async-app-verified-tests.log`. 유일 실패는 `ico_project_icon_detection`의 `/var` 대 `/private/var` canonical 경로 단언이며 비동기 전환 회귀가 아니다. 릴리스 담당에게 실제 값/기대 값과 수정 지점을 전달했다. 전체 최신 소스가 통과했다고 보고하지 않는다.
- Windows native 프로덕션 경로는 크로스 빌드에서 컴파일됐다. 최종 `scripts/check-windows.sh -p krill-app --clippy`는 뒤늦게 추가된 아이콘 테스트의 `as usize` lint로 exit 101. 해당 테스트를 `usize::try_from(...).unwrap()`으로 보완했고, 원본 폴더가 다른 세션에서 main으로 전환돼 대기 중이던 재검사는 취소했다. **Windows 전체 통과는 아직 주장하지 않는다.** 후보 소스의 최종 Windows/전체 릴리스 gate는 릴리스 담당 `term_a139fdd5-5d25-425d-9a6f-0f21720a8ea5`가 이어서 검증한다.

작업 중 원본 폴더가 외부 작업으로 main `db4d450e42956f8751286d18f1240205c2235e0e`으로 전환된 것을 관측했다. 이 작업자와 main 전환 조정자는 해당 reset/stash/checkout을 실행하지 않았다고 확인했으며 실행자는 이 보고서에서 단정하지 않는다. 원래 전체 작업본은 별도 snapshot 및 stash `d2bc528ef859a6f70e769e6c073908c12c50717f`로 보존됐다. 이 HEAD에는 비동기 델타가 아직 적용되기 전이므로 이 HEAD의 검사를 위 결과와 혼동하지 않는다. 당시 대기 중이던 Mac Clippy가 main 0.1.22를 검사한 `/tmp/krill-async-clippy-verified.log`는 비동기 작업 검증 결과로 사용하지 않는다. 작업 소스는 백업 snapshot 및 릴리스 후보의 의미 병합으로 보존되며 후보 담당이 통합을 확인했다. 최신 source 목록과 SHA manifest, 후속 한 줄 테스트 보완과 남은 아이콘 단언을 모두 전달했다.

감독 작업자 완료 보고서를 검토하고 관리 터미널 2개를 회수했다. 나머지 터미널은 Orca의 `user_takeover` 판정으로 보존했으며, 회수 가능한 잔여 작업자는 0개다. 전체 `scripts/check.sh`, 설치·배포, 실제 Windows 실행은 이 작업의 통과 항목에 포함하지 않는다.

## 후속 보존 확인

2026-10-20 23:05 KST, main 전환 조정자가 stash의 tracked/untracked tree에서 최종 수정본을 재현한 snapshot `a0ff6d38164687e79291a04ecea2d2bc3dfb2e0c`를 전달했다. 최종 178파일 manifest SHA256 `9bba2986db2499434af77595f904c48bb7c87ccb81cdba06e4bae53dd84f559b`와 snapshot의 각 blob이 모두 일치한다고 확인했다. 재현 기록: `/Volumes/krill-build/main-migration-20261020-224654/reconstructed-source.json`. 비동기 수정본은 아직 현재 main에 적용되지 않았으며 릴리스 후보 통합 뒤 반영 예정이다. 별도로 진행 중인 main의 terminal 성능 작업 등은 `ad62c74a` snapshot으로 보존되며 이번 검증 범위와 구분한다.

2026-10-20 23:28 KST 후속: main 통합 조정자가 원본을 main `2992dfb4`로 통합 완료하고 동결 해제가 가능하다고 통보했다. 이후 별도 워크스페이스·터미널 성능 WIP도 보존·충돌 조정했으며, app/daemon/proto check·Clippy 및 관련 28개 테스트가 통과했다고 보고했다. 이 결과는 조정자의 보고이며 이번 조사자가 재실행한 결과는 아니다. 현재 작업 트리는 통합 HEAD와 새 WIP를 포함하므로 앞의 178파일 검증과 별도 범위다.
