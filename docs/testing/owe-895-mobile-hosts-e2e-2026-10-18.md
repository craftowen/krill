# OWE-895 E2E 실동작 검증 보고서 (2026-10-18)

범위 A(모바일 PWA 짝짓기·세션 조작, 폰 없이 가능한 전부)와 범위 B(원격 호스트 연결)를
격리 환경에서 실측했다. 코드 변경 없음(검증 전용 워커).

## 환경

- 베이스: origin/dev `3694c215` (Merge OWE-801), dev 프로파일 빌드
  (`krill`·`krilld`·`krill-holder`, 빌드 로그 `/tmp/krill-owe895-out/`, 워크트리 밖)
- 격리 상태 디렉터리: `KRILL_STATE_DIR=/tmp/krill-owe895-state` (운영
  `~/Library/Application Support/dev.krill.app` 미사용, 실제 HOME 미사용)
- 앱 창·실화면 조작 없음. PWA 검증은 agent-browser 헤드리스(Chromium, 뷰포트 390x844).
- 캡처(리포지터리 미포함, gitignored): `.night/e2e-2026-10-18/mobile/01~04.png`

## 범위 A — 모바일 (전부 통과)

세션 입출력까지 검증하려고 holder+krilld 생산 스택으로 기동했다
(과제문 "krilld만"은 "앱 없이 데몬 측만"으로 해석, 앱 창은 띄우지 않음).
holder 없이(`krilld` 단독)는 세션 생성이 불가함을 먼저 확인:
`krill new` → `krill-holder is not connected; cannot create sessions`.

| # | 단계 | 결과 | 근거 |
|---|------|------|------|
| A1 | `krill mobile pair --json` → 티켓/URL 발급 | 통과 | URL `http://100.113.52.51:62967/?pair=…#key=…`, TTL 5분, `host=100.113.52.51` |
| A2 | Tailscale 인터페이스 바인딩 | 통과 | `interfaces=[en1 172.16.11.201(기본경로), utun7 100.113.52.51]`, 선택=Tailscale IP. `ifconfig`에 `100.113.52.51` 존재 확인 |
| A3 | PWA 짝짓기 완료 | 통과 | 헤드리스 브라우저로 페어링 URL 오픈 → Sessions 렌더(세션 카드 2개). `krill mobile devices`에 `Mobile browser paired` 등록. 캡처 01 |
| A4 | 세션 목록·출력 보기 | 통과 | PWA에 세션 3개 카드 표시, 실행 결과 `PWA-PROBE-895`가 PWA DOM에 2건 존재. 캡처 02 |
| A5 | 입력 보내기 | 통과 | PWA Write prompt → `echo PWA-PROBE-895` → 원격 셸 실행을 `krill read 3`으로 확인 (`PWA-PROBE-895` 출력) |
| A6 | 재접속(장기 credential) | 통과 | pair 파라미터 없이 `/` 재오픈 → `Encrypted · connected`, 출력 유지. 캡처 03 |
| A7 | `krill mobile revoke --all` 후 연결 끊김 | 통과 | 기기 상태 `revoked`, 게이트웨이 리스너 종료(listening:false, 재확인 시 포트 접속 거부) |
| A8 | 재발급 후 리스너 복구 | 통과 | `pair --json` 재호출 시 새 포트(63153)로 리스닝 재개, 재짝짓기 성공 |

참고: `revoke --all` 시 리스너가 닫히는 것은 Lazy Exposure 설계대로의 동작이다
(페어링된 기기가 없으면 외부 포트를 열지 않음).

사람 필요 항목: 실제 폰(iOS Safari)에서의 카메라/사진 첨부·푸시·모바일 네트워크
(LAN이 아닌 외부망) 경로는 폰 없이 검증 불가.

## 범위 B — 원격 호스트 (통과, 실동작 포함)

- 코드 흐름: 로컬 `krilld`를 띄우지 않고 `hosts.toml` 해석 → 원격 `krilld.sock`
  결정(`KRILL_REMOTE_SOCK` > `remote_socket` > 원격 `$HOME` 조회) → `ssh -N -L`
  단일 터널 → 포워딩 소켓에 IPC 핸드셰이크. 원격 데몬은 미리 떠 있어야 한다.
- 실동작(원격: `seolbeulli@100.95.225.11`, Tailscale): 바이너리 3종을
  `~/krill-e2e/bin/`에만 복사, 격리 `KRILL_STATE_DIR=~/krill-e2e/state`로
  holder+krilld 기동. 원격 사용자 파일·셸 설정·`~/.claude` 미접촉.

| # | 단계 | 결과 | 근거 |
|---|------|------|------|
| B1 | 원격 세션 목록 | 통과 | `krill --host seolyi-e2e ls` (SSH 터널+`remote_socket` 지정 경유) |
| B2 | 원격 새 세션 | 통과 | `krill --host … new --cwd /Users/seolbeulli -- /bin/echo remote-hello-895` → id 1, `read 1`에 출력 확인 |
| B3 | 원격 입출력 | 통과 | sh 세션에 `send 2 --text 'echo REMOTE-PROBE-895' --enter` → `read 2`에 출력 확인 |
| B4 | 원격 krilld kill -9 후 끊김 복구 | 통과 | holder가 krilld 자동 재기동(새 PID), 세션 1·2 생존, 동일 PTY에 `POST-RESTART-895` 입출력 연속 확인 |
| B5 | 기존 단위 테스트 | 통과 | `krill-hosts` 14/14, `krill --test remote` 6/6, `krilld mobile` 30/30 |

정리: 원격 `~/krill-e2e` 삭제·프로세스 전원 종료 확인(ALL-DEAD, REMOVED).
로컬 격리 데몬(holder·krilld)도 종료. 운영 데몬·타 워커 프로세스는 건드리지 않음.

## 결함

발견된 Krill 결함 없음. `lin new -l Bug` 발행 없음.
최초 SSH 실패(`macmini-seolyi` 기존 키 거부)는 환경 이슈로 코디네이터 답변
(`seolbeulli@100.95.225.11` 사용)으로 해결.

## 검증 명령 결과

- `cargo fmt --all --check`: exit 0
- `cargo nextest run -p krill-hosts`: 14 passed
- `cargo nextest run -p krill --test remote`: 6 passed
- `cargo nextest run -p krilld mobile`: 30 passed
- `cargo clippy -p krill-hosts -p krilld --all-targets -- -D warnings`: exit 0 (경고 0)
- 전체 `scripts/check.sh`는 미실행(코디네이터 지시).
