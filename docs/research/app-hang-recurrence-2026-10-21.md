# 앱 먹통 재발 조사와 스트림 복구 수정

2026-10-21 KST. 검사 기준 HEAD: `19450606e4b8e458869a5b19c5c269b571887eab` + 이 작업의 미커밋 변경. 공유 작업 트리에 이미 있던 설정·스킬·오케스트레이션 변경은 보존했다. 이 문서는 개발 검사 기록이며 릴리스 전체 검사 결과가 아니다.

## 실제 실행본에서 확보한 근거

- 실행 앱: `/Applications/Krill.app/Contents/MacOS/krill-app`, PID 29955, 번들 버전 0.1.23. 앱·holder·데몬을 종료하지 않고 조사했다.
- `~/Library/Logs/Krill/krill-app.log`에는 같은 앱 PID가 07:24:26.391, 07:43:04.028에 `daemon stopped responding, reconnecting`을 기록했다. 이는 감시 연결의 실패 근거이며, 사용자가 겪은 장애의 시각이나 UI 스레드 교착을 증명하지는 않는다.
- 15:10:45부터 실행 앱을 5초간 `sample`했다. 이 구간의 메인 스레드는 대부분 AppKit 이벤트 대기였으며 계속 그리기도 수행했다. 이 샘플에서 지속 교착은 관측하지 못했다. 장애 순간의 스택을 확보한 것으로 보고하지 않는다.
- 원본 샘플·검사 로그는 로컬 `/tmp/krill-freeze-20261021/`에 보존했다. 원본 스택에는 로컬 경로가 있으므로 저장소에는 넣지 않았다.

## 확인한 복구 누락

`shell.rs::spawn_pane`은 `pane.rs::attach` 이후 읽기 제한을 해제한 화면 소켓을 백그라운드에서 읽는다. 이 구조는 정상 유휴 터미널을 유지하는 데 필요하다. 그러나 EOF 없이 상대가 정지하면 읽기는 계속 기다린다.

기존의 `Pane::stalled`와 `Shell::stalled_banner`는 입력 ACK가 `input::PROBE`(설계값 2초)를 넘겼는지만 판정하고, 해당 소켓을 종료하거나 다시 연결하지 않았다. 별도의 로컬 데몬 감시 연결은 개별 화면·입력 연결의 생존을 보장하지 않는다. 특히 원격 pane은 로컬 감시 연결이 정상이어도 멈출 수 있다. 커서가 깜박이지 않고 새 프레임도 없으면 경고를 다시 그릴 계기도 보장되지 않았다.

또한 쓰기 큐 admission/쓰기 실패로 `Pane::unlink`가 먼저 실행되면 입력은 보류 상태가 되지만, 기존 writer와 reader가 실제로 닫히기 전까지 다음 연결 시도는 시작되지 않는다. 새 검사는 이 상태도 복구한다.

이것은 코드 및 결함 주입 테스트로 확인한 **터미널 무응답의 지속 경로**다. 이번 사용자의 모든 먹통이 이 원인이라는 사후 입증이나, 앱 전체 정지의 최초 원인 확정과는 구분한다. 이전 조사 기록(`client-async-audit-independent-review.md`)도 실제 다분간 정지의 재현이 아닌 소스 분석이라는 한계를 명시하고 있다.

## 변경

- `shell/liveness.rs`: 첫 연결 때 창별 감시 작업을 시작하고 500ms마다 입력 장부를 검사한다. 입력 ACK 정체 또는 입력 전송 경로 소실 시 **해당 UI 소켓만** 닫아 기존 reader/reconnect 루프를 깨운다. PTY 소유자·데몬·다른 pane은 종료하지 않는다.
- ACK 대기 입력은 기존 `Uncertain` 알림으로 표시하고 재전송하지 않는다. 아직 보내지 않은 보류 입력만 같은 세션 재연결 후 전달한다. 계정 전환 대기·콜드·종료 화면은 이 자동 복구에서 제외한다. 유휴 정상 연결에는 입력이나 화면 요청을 보내지 않고, 갱신할 내용이 없으면 렌더를 요청하지 않는다.
- `responsiveness.rs` 및 앱 기동 연결: macOS UI의 500ms heartbeat를 별도 OS 스레드가 관찰한다. 5초 이상 heartbeat가 갱신되지 않으면 로컬 `sample`을 수집하고, 복귀도 로그에 남긴다. 감시 스레드는 GPUI foreground와 공유 background executor가 진행하지 않아도 동작한다. heartbeat 지연은 교착 확정 신호가 아니며 원인은 수집된 스택으로 판별해야 한다.
- 샘플은 `<상태 디렉터리>/diagnostics/app-hang-latest.sample.txt`, `app-hang-previous.sample.txt` 두 개만 유지한다. 파일당 최대 2MiB, 파일 권한 0600, 샘플 프로세스 대기 제한 10초다. 터미널 본문·힙 덤프·업로드는 수집 경로에 없다. 지속 정체 중 샘플을 반복 생성하지 않고 새 heartbeat 뒤의 다음 정체를 별도 사건으로 취급한다.

## 개발 검증

Cargo 타깃 디렉터리는 `/Volumes/krill-build/targets/krill`, Windows는 `/Volumes/krill-build/targets/krill-win`을 사용했다.

| 범위 | 결과 | 로컬 로그 |
|---|---|---|
| `cargo check -p krill-app --locked` | 통과 | `check.log` |
| `cargo clippy -p krill-app --all-targets --locked -- -D warnings` | 통과 | `clippy-final.log` |
| `cargo nextest run -p krill-app --lib --bin krill-app --locked -E 'test(shell::) \| test(pane::) \| test(responsiveness::)'` | 270개 통과 | `app-tests-final.log` |
| `cargo nextest run -p krill-app --test pane_e2e --locked -E 'test(app_kill9_reattaches_same_session)'` | 통과 | `survival.log` |
| `scripts/check-windows.sh -p krill-app --clippy` | 통과; 크로스 컴파일 검사 | `windows.log` |
| `cargo build -p krill-app --bin krill-app --locked` | 통과; `/Volumes/krill-build/targets/krill/debug/krill-app` 생성 | `build-final.log` |
| `cargo fmt --all -- --check`, `git diff --check` | 통과 | `fmt-final.log` |

로그 경로의 기준은 `/tmp/krill-freeze-20261021/`이다. 프로덕션 변경 후 최종 검사와 테스트를 수행했다. 마지막 테스트 fixture의 lint 주석은 테스트 빌드와 최종 Clippy에서 확인했다. 변경한 Rust 파일 여섯 개의 SHA-256은 같은 디렉터리의 `source-sha256.txt`에 기록했다.

새 회귀 테스트는 기존 감시를 끈 대조 상태에서 열린 소켓과 정체 장부가 그대로 남는 것을 확인한 뒤, 감시를 켜면 입력·프레임·렌더 없이 해당 소켓이 닫히는 것을 확인한다. 결과 불명 명령의 재전송 금지, 보류 입력 전달, 다른 pane의 연결 유지, ACK 수신 후 정상 연결, 계정 전환 예외도 확인한다.

별도 macOS 테스트는 GPUI 두 실행기를 전혀 진행시키지 않은 채 실제 OS 스레드가 스택 파일을 저장하는 것을 확인했다. 세션 통합 테스트는 앱 역할 프로세스의 SIGKILL 뒤 동일 세션 재연결, 이전 출력·히스토리 복원, holder·데몬 동시 종료 뒤 콜드 복원을 확인한다.

전체 `scripts/check.sh`, release GUI 생존 검사, Windows 실기 실행은 이 개발 검증 범위가 아니다. 현재 사용 중인 `/Applications/Krill.app`은 교체하거나 재시작하지 않았다. 이번 수정과 자동 진단은 새 바이너리를 적용한 뒤 동작한다.
