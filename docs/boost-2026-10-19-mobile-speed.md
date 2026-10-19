# Krill 모바일 체감 속도 boost 보고서 (2차 검증본, 2026-10-19)

> 요청: "속도 최적화 진행해라. 할 수 있는 최대로". 1차 독립 조사 → 2차 전수 검증·release 재측정. 기준 트리 `571f4ba4`(v0.1.10 + OWE-906). 측정: 이 맥(LG U+ 유선), 폰 LTE 구간 미포함.

**결론: 체감 속도를 가장 크게 바꿀 3가지는 ① snapshot을 거의 전부 차지하는 git 조회 경로, ② 퀵 터널 직후 약 30초 동안 이어지는 530 응답, ③ 연결마다 붙는 accept 50ms 폴링·인증 fsync·RTT 1회입니다.** release 빌드로 다시 재 보니 snapshot은 loopback 268~356ms, 터널 330~344ms였고, 이 가운데 git 이외 부분(wire·Sidebar·Capture·orchestration)은 측정 잡음 수준(약 10ms 이하, 추정 상한)이었습니다. 이전 조사가 놓친 원인이 두 가지 있습니다. git 자식 프로세스를 10ms/5ms `sleep`으로 기다리는 폴링이 세션마다 약 25ms를 보탭니다. 퀵 터널은 한 번 성공한 뒤에도 약 30초 동안 요청의 약 70%가 530이었습니다.

측정 조건: 이 트리(571f4ba4)를 `/tmp/mx/target-speed`에 release로 빌드했습니다. 격리 상태는 `/tmp/mx/boostB/state`, zsh 세션 6개(cwd: mobile-speed 2개, 메인 minikrill 2개(변경 17개), `/tmp`, `$HOME`), 클라이언트는 Node 26 WS(E2EE v2)입니다. 터널 경로는 Mac→icn06→Mac이고 RTT는 약 20ms였습니다. 측정 중 mobile-speed 트리를 다른 작업이 고치고 있어서 그 저장소의 git 수치는 25~87ms로 흔들렸습니다. 측정 스크립트는 `/tmp/mx/boostB/bench/`에 있습니다. 격리 holder·krilld·cloudflared·세션 셸은 모두 종료했고, 운영 게이트웨이와 무관한 cloudflared(8142)는 건드리지 않았습니다. 저장소 파일은 고치지 않았습니다.

---

## 1. 이전 조사 정정 (주장 / 실제 / 근거)

| 주장 | 실제 | 근거 |
|---|---|---|
| snapshot 340~460ms(debug), wire 4개·Sidebar 재조회가 비용의 일부 | release loopback 268~356ms, 터널 330~344ms. 제어 요청 1회(wire 생성+Hello 포함)는 **0.1~0.2ms**, Sidebar(캐시 적중)도 0.1~0.2ms, orchestration 0.1ms. **snapshot ≈ 세션별 git 합계** | `snap.mjs`: close_terminal(없는 세션)=wire+1요청, create_terminal(없는 cwd_session)=wire+Sidebar로 분리해 측정 |
| 개선안 "제어 요청 여러 개를 wire 하나로" | 효과가 거의 없습니다(요청당 0.2ms). 우선순위에서 뺍니다 | 위 측정 |
| git RPC 82~127ms | 터널 83~99ms, loopback 25~87ms(저장소 세션), 비저장소 세션 12~17ms(git status가 실패하는데도 매번 실행) | `snap.mjs` git_1~6 |
| (원인 미기재) | git을 블로킹으로 직접 돌린 합(status+diff+diff --cached+branch)은 메인 저장소 51ms, mobile-speed 36ms. 같은 명령을 krilld 방식(nonblocking+10ms/5ms sleep)으로 흉내 내면 76ms. **폴링이 세션당 약 25ms를 보탭니다** | `git/changes.rs:365` `thread::sleep(10ms)`, `git/mod.rs:316` `sleep(5ms)`. Python 재현 비교 |
| 효과 400→60~80ms | 추정으로 둡니다. 항목별 근거는 3절에 있습니다 | — |
| seq 6초 프레임 17~18개 | release loopback 5초에 209프레임(약 40fps, 간격 p50 11.6ms), 터널 193프레임. 처리량 병목이 아니었고, 이전 수치는 debug 빌드 탓으로 보입니다 | `term.mjs seq` |
| 키 에코 loopback 1.7/4.4, 터널 23.1/25.7ms | release loopback p50/p90 0.5/0.7ms, 터널 20/25ms. 서버가 보태는 시간은 거의 없고 RTT가 전부입니다 | `term.mjs` |
| 터미널 첫 화면 60/184ms | loopback 46~68ms(대부분 accept 폴링), 터널 235ms | `term.mjs` |
| QR 직후 첫 GET 3회 중 1회 530 | **훨씬 심합니다.** QR 직후 6회 중 4회, 이어진 13초 동안 26회 중 20회가 530이었습니다. 단독 퀵 터널을 새로 띄워 재 보니 URL이 3.6초에 나왔고, 33초까지 530이 약 70%, 35초 이후에야 안정됐습니다. 로그상 `ha-connections:1`, 등록 연결은 1개(icn06)뿐이었습니다 | `cfprobe.sh`, curl 연속 측정 |
| accept 폴링, 연결당 평균 약 30ms | 맞습니다. loopback 응답 p50 31.5ms, max 57ms(50ms를 넘는 것은 타이머 지연), WS open p50 41.9ms(0.9~57.6) | `hs.mjs`, Python TCP 측정 |
| 인증 fsync 6.7~8.1ms | 맞습니다. 5.9~10.3ms, loopback auth 단계 p50 7.9ms와 일치합니다 | `fsync.py`, `hs.mjs` |
| 캔버스 draw dpr3 2.3/7.2/10.7ms. 크기 바뀔 때만 리사이즈·font 캐시로 7ms→0.15ms | 실제 `draw()` 코드를 헤드리스 Chrome(dpr3, Mac)에서 돌린 값은 2.2/8.8/13.9ms. **캔버스 재할당을 없애도 13.2ms, font 캐시까지 해도 12.8ms**라 효과가 작습니다. 비용은 칸마다 부르는 `fillText`입니다. 효과가 큰 것은 바뀐 행만 그리기+rAF입니다 | `/tmp/mx/boostB/draw/draw{,2,3}.html` |
| snapshot 응답 1.2KB | 세션 6개에 화면 내용이 있으면 9.3~9.5KB입니다(내용에 따라 다름) | `snap.mjs` bytes |
| JS ChaCha 64KB 2.4ms, 디코드 0.44/2.0/3.6ms | 맞습니다. 1.97ms(약 33MB/s), 디코드 0.49/2.13/3.49ms | 재실행 |
| 줄 번호 | 틀린 것: `client.js:23`→**48**(poll), `:38`→**63**(ClientHello), `gateway.rs:498`→**606**(wait_timeout 50ms), `:606`→**718**(devices.seen), `:207`→**270**(QR 대기 200ms), `tunnel.rs:83-89`→**125-142**(500ms 간격), probe `163`→**323**, `pwa.rs:108`→**137**(no-store·Connection: close), snapshot 범위 325-387→**325-382**. 나머지(`terminal.js:142/146/164-171/21-30`, `attach.rs:56/342`, `registry.rs:469`, `devices.rs:146/226/230`, `ws.rs:47/176-188`, `ipc.rs:138-145`)는 맞습니다 | 직접 열어 확인 |
| "동작 반영도 같은 시간 늦춤" | 구체적으로는 이렇습니다. RPC가 클라이언트에서 한 줄로 직렬화되어(`client.js:41-46`), 진행 중인 snapshot 뒤에서 최대 344ms를 기다립니다. 게다가 모든 동작이 끝난 뒤 `await ctx.refresh()`로 snapshot 전체를 다시 기다립니다(`session.js` prompt·approve·close, `details.js` gate·question). 터널에서 prompt 20ms + snapshot 344ms ≈ **364ms** 동안 버튼이 잠깁니다 | 코드 + 측정 |

## 2. 이미 적용된 것

- 쉬던 화면의 첫 변경은 바로 보냅니다. FRAME_MIN은 연속 출력일 때만 걸립니다(`attach.rs:54-56,340-345`). 측정한 에코 지연도 RTT뿐이었습니다.
- 인증 단계 클라이언트 쪽은 이미 묶여 있습니다. auth JSON과 proof를 연달아 보냅니다(`client.js:61`, `terminal.js:140`).
- `TCP_NODELAY`, 정적 에셋 사전 gzip(합계 33.8KB), snapshot 동시 호출 합치기(`client.js:47` `refreshing`).
- Sidebar의 worktree 탐색 3초 캐시(`registry/sidebar.rs:41-63`), 변경 파일이 없으면 diff 2회를 생략(`git/changes.rs:94`).
- attach 스냅샷은 빈 행을 생략하고, 스크롤은 별도 op로 보냅니다(`attach.rs:445`). attach 쓰기 루프가 매번 최신 화면을 캡처하므로 krilld 안에 프레임이 쌓이지 않습니다.
- 화면 복귀·online 이벤트에서 재연결·갱신합니다(`app.js` `resume`).
- 유휴 비용은 작습니다. 기기를 짝지은 채 연결이 없으면 krilld CPU는 20초에 0.03초였습니다.

## 3. 개선안 (우선순위순)

| # | 항목 | 근거 파일:줄 | 예상 효과 | 난이도 | 위험 |
|---|---|---|---|---|---|
| P0-1 | snapshot git 경로 정리: 비저장소 생략, cwd 중복 제거, 병렬 제출, 짧은 TTL 캐시 | `pwa_rpc.rs:342-375`, `:369`, `:402-441` | 이 구성에서 snapshot 약 330ms → 약 50~60ms(캐시 없음), 캐시 적중 시 약 10ms 이하(추정). git CPU는 snapshot당 약 216ms → 0~100ms(추정) | 중 | 낮음(표시 정보가 최대 TTL만큼 늦음) |
| P0-2 | git 자식 대기를 sleep 폴링에서 `poll(2)`로 | `git/changes.rs:365`, `git/mod.rs:316` | 저장소 세션 git 1회당 약 −25ms(재현 76→51ms). **데스크톱 앱 git watch에도 똑같이 적용됨** | 하~중 | 낮음(취소·타임아웃 의미 유지 필요) |
| P0-3 | 터널 준비 판정 강화 + 미리 데우기 | `tunnel.rs:125-142,323-346`, `gateway.rs:243-272,445-449` | QR 직후 첫 접속 530 확률(실측 약 70%)을 크게 낮춤(폰 미확인) | 중 | QR 대기가 늘 수 있음(UX 절충) |
| P1-4 | accept 50ms 폴링 제거 | `gateway.rs:531-615`(606) | WS·HTTP 연결마다 p50 약 −30~40ms(loopback 42→약 1ms 추정) | 중 | 중(리스너 수명 관리) |
| P1-5 | 인증 시 `devices.seen` fsync 제거·디바운스 | `gateway.rs:718`, `devices.rs:146-158,205-235` | 연결당(제어 1개 + 터미널 탭마다) −6~10ms, 상태 락 점유도 제거 | 하 | 낮음(last_seen이 표시용) |
| P1-6 | 핸드셰이크 파이프라이닝 | `client.js:61-64`, `terminal.js:140-142` | 연결당 −1 RTT(Mac 터널 20ms, 폰 LTE는 추정 40~80ms) | 하~중 | 낮음(서버 변경 불필요) |
| P1-7 | 동작 뒤 snapshot 대기 풀기·폴링 줄이기 | `client.js:48`, `session.js`(approve·prompt·close·handoff), `details.js` | 동작 후 UI 잠금 364ms → 약 RTT 20ms | 하 | 낮음 |
| P1-8 | 터미널 렌더 rAF 합치기 + 바뀐 행만 그리기 + 캔버스 크기 상한 | `terminal.js:146,164-171` | 한 행 변경 시 13.9ms → 행당 약 0.1~0.2ms(추정). iOS 캔버스 면적 한도 초과 방지 | 중 | 중(렌더 회귀) |
| P2-9 | 모바일 FRAME_MIN 33ms | `attach.rs:56,150,342`, `registry.rs:539` | 대량 출력 시 프레임·바이트 약 −25%(추정) | 하 | 연속 출력 중 에코 최대 +25ms |
| P2-10 | 터미널 탭 전환 시 새 WS 비용 줄이기(미리 인증한 예비 소켓) | `session.js` `attach()`, `client.js:67` `terminalConnect` | 탭 전환 첫 화면 터널 235ms → 약 1 RTT + 렌더(추정) | 중 | 중 |
| P2-11 | QR 대기 루프 200ms → 50ms | `gateway.rs:270` | QR 발급 평균 약 −75ms(추정) | 하 | 없음 |
| P2-12 | 고정 출처 가용성 검사를 probe와 병렬로 | `tunnel.rs:226-246` | 지금은 NXDOMAIN이라 약 10ms. 배포 뒤에는 HTTPS 왕복 1회 절약(추정) | 하 | 낮음 |
| P3 | ws 프레임 한 번에 쓰기, HTTP 헤더 1바이트 읽기 개선, `no-store`→`no-cache`+ETag, 재시도 상한 축소, snapshot 변경 없을 때 생략 | `ws.rs:176-188`, `ws.rs:41-48`, `pwa.rs:137`, `client.js:39` | 각각 작음 | 하 | 낮음 |

### 항목별 구현 메모

**P0-1 snapshot git** (`pwa_rpc.rs`)
- `snapshot()`에서 이미 계산하는 `session_group(...)`(353)의 `repo_key`가 `None`이면 git을 부르지 않고 `branch/added/deleted`를 null로 둡니다. 지금도 git 실패 시 `Value::Null`이라 동작은 같고, 비저장소 세션마다 12~17ms가 빠집니다.
- 369의 `self.git(info.session.0, None)`를 다음 순서로 바꿉니다. 각 cwd는 이미 가져온 `sidebar.sessions`에서 꺼내므로 `cwd()`로 Sidebar를 다시 묻지 않습니다.
  1. 고유 cwd를 모읍니다.
  2. cwd마다 `crate::git::changes::query(q, cwd, Arc::new(AtomicBool::new(false)))`와 `queue.submit(cwd, ["branch","--show-current"])`를 **먼저 전부 넣습니다**.
  3. 그다음 `recv()`로 모읍니다. 큐 워커가 4개(`git/mod.rs:31`)이고 같은 키만 직렬화되므로 서로 다른 저장소는 병렬로 돕니다.
- `Backend`에 `Mutex<HashMap<PathBuf,(Instant,Value)>>` 캐시를 두고 TTL은 약 5초로 합니다. `Action::Git`(상세 화면)은 경로가 있을 때만 캐시를 우회합니다. 폰 여러 대가 같은 결과를 함께 씁니다.
- (선택) `branch --show-current` 프로세스 제거: status를 이미 `--branch`로 부르는데 `StatusParser::record`(`changes.rs:158-160`)가 `#` 헤더를 버립니다. `# branch.head`를 파싱해 krilld 내부 함수(예: `changes::query_with_branch`)로 돌려주면 됩니다. `krill_proto::git::Changes`에 필드를 추가하면 postcard 와이어 호환이 깨지므로 하지 않습니다. `(detached)`는 빈 문자열로 바꿉니다.
- 회귀 테스트: 기존 `pwa_rpc` 테스트(685~)에 더해, 비저장소 cwd 카드는 git 필드가 null이고 같은 cwd 두 세션은 같은 값을 받는지 확인합니다. `node crates/krilld/tests/mobile_pwa.cjs`(diff 화면)를 돌립니다. 수치는 `snap.mjs`로 snapshot median이 줄었는지 확인합니다.

**P0-2 git 폴링** (`git/changes.rs` `stream()` 284-366, `git/mod.rs` `run_guarded()` 261-317)
- unix에서는 `rustix::event::poll(&mut [PollFd::new(&stdout, IN), PollFd::new(&stderr, IN)], Some(&Timespec{10ms}))`를 씁니다. 이미 `session.rs:38`에서 쓰는 안전 API입니다. 데이터나 EOF가 오면 바로 깨어나고, 타임아웃은 10ms 그대로라 취소 확인 주기가 지금과 같습니다. `watch.rs`의 `HIDE_REAP_WAIT` 100ms 전제도 유지됩니다.
- EOF 뒤 `try_wait`가 `None`이면 1ms 짧은 대기로 바꿉니다(EOF 뒤에는 poll이 즉시 반환되므로 바쁜 루프 방지).
- Windows는 `#[cfg(windows)]`로 기존 sleep을 둡니다.
- 회귀 테스트: `git/changes/tests.rs`의 `cap_reaps_a_producer_that_never_closes_stdout`, `cancellation_reaps_running_and_skips_queued_work`, `timeout_and_parse_failure_reap_children`, `git/watch/tests.rs`, `cargo nextest run -p krilld`. 수치는 git RPC median으로 확인합니다.

**P0-3 터널 준비**
- `tunnel.rs:125-142` probe 루프를 "연속 3회 200(간격 300~500ms)"이어야 ready가 되도록 바꿉니다.
- 다만 실측상 안정까지 약 35초라 `MINT_WAIT` 15초(`gateway.rs:30`)를 넘으면 지금처럼 미준비 URL로 QR이 나갑니다. 그래서 함께 할 것:
  - (a) 설정 → 모바일 화면이 열려 `MobileRequest::Status`가 오면 Tunnel 모드에서 cloudflared를 미리 띄웁니다. 지금은 `reconcile`(445-449)이 기기도 티켓도 없으면 닫으므로 예외 조건이 필요합니다.
  - (b) 앱 QR 화면에 "터널 안정화 중"을 표시합니다(실제 상태가 있을 때만, AGENTS 10).
- 퀵 터널은 Cloudflare 무보증 서비스이고 연결이 1개(`ha-connections:1`)라는 점을 문서에 적습니다.
- 회귀 테스트: `tunnel.rs` 테스트의 probe 주입(`Probe` 타입)에 "성공·실패 교대" 가짜를 넣어, 연속 성공 전에는 ready가 아닌지 확인합니다. `gateway/tests.rs` `supervisor_replaces_unreachable_tunnel_with_backoff`, `mint_waits_for_pending_download_without_holding_state_lock`.

**P1-4 accept** (`gateway.rs:531-615`)
- 606의 `wait_timeout(50ms)` 대신 이렇게 합니다. 락을 풀고, unix에서 리스너 fd들과 `Shared`의 깨우기 fd(`UnixStream::pair`, nonblocking)를 `rustix::event::poll`로 기다립니다(타임아웃 1초). 미인증 5초 타임아웃과 reconcile은 1초 단위면 충분합니다.
- 지금 `shared.changed.notify_all()`을 부르는 곳(`request` 225, `handle` 310, `Connected::drop` 625, `serve` 664)에 깨우기 바이트 쓰기를 함께 넣는 `wake()` 헬퍼를 둡니다.
- Windows는 기존 코드를 cfg로 남깁니다.
- 회귀 테스트: `gateway/tests.rs` 전체(특히 `revoke_all_closes_pending_unauthenticated_connections`, `lazy_binding_expiry_and_mode_rotation`, `idle_authenticated_socket_is_pinged_then_closed_without_pong`). 수치는 `hs.mjs` loopback open p50이 약 5ms 미만인지 봅니다.

**P1-5 fsync** (`devices.rs:146-158`)
- `seen()`은 메모리의 `last_seen_ms`만 갱신하고, 디스크 기록은 마지막 저장 후 10분이 지났을 때만 합니다. 페어링·폐기는 지금처럼 즉시 `save`합니다.
- `authenticate`(`gateway.rs:709-728`)가 상태 락 안에서 fsync하던 것도 같이 사라집니다.
- 회귀 테스트: `devices/tests.rs`에 "창 안에서 seen을 두 번 해도 파일 mtime이 그대로이고 `list()`에는 새 값이 보이며, revoke는 바로 영속되는지" 추가.

**P1-6 파이프라이닝**
- 제어 연결(`client.js:61-64`): proof를 보낸 직후, 고정 기능 마스크로 ClientHello를 보내고 첫 snapshot 요청 프레임까지 보냅니다. 서버 `negotiate`가 교집합이라 서버 hello를 기다릴 필요가 없습니다. 벤치는 `1n<<56n`으로 동작했습니다. 이후 auth → hello(검증만) → snapshot 응답 순서로 읽습니다.
- 터미널(`terminal.js:140`): proof 직후 `send(2, …(1n<<56n)|1n|4n|4096n…)`, role `[0,2,0]`, attach(0x10)를 함께 보냅니다. phase 2는 hello 검증만 합니다.
- 서버는 `authenticate`가 proof 프레임만 읽고 나머지는 소켓 버퍼에 남겨 `bridge`가 이어서 전달하므로 바꿀 필요가 없습니다. 테스트로 확인할 것.
- 회귀 테스트: `gateway/tests.rs`에 proof+ClientHello+요청을 한 번에 쓰고 응답 순서를 확인하는 테스트 추가. `mobile_terminal.cjs`, `mobile_session.cjs`, `mobile_pwa.cjs` 하네스 갱신.
- **JS를 고치면 `.gz`를 결정적으로 다시 만들어야 하고**(`mobile_pwa.cjs:129`가 비교), `asset_hash`가 바뀌어 고정 출처 `/m/<hash>/`도 바뀝니다.

**P1-7 동작 후 갱신**
- `session.js`·`details.js`의 `await ctx.refresh()`를 기다리지 않는 갱신(`ctx.refresh().catch(ctx.error)`)으로 바꿔 버튼·시트를 바로 풉니다.
- `poll()`(`client.js:48`)은 `document.hidden`이면 건너뛰고, 라이브 터미널이 열려 있으면(`M.activeTerminal`) 간격을 10초로 늘립니다.
- 회귀 테스트: `mobile_session.cjs`에서 approve 뒤 버튼이 snapshot 완료 전에 다시 켜지는지 확인.

**P1-8 렌더** (`terminal.js`)
- 146에서는 `T.update()`의 `dirty`를 누적하고, rAF를 한 번만 예약해 `draw(dirty)`를 부릅니다.
- `draw`는 dirty 행만 지우고 다시 그립니다. Reset·scroll·글꼴·크기 변경 때만 전체를 그립니다(update가 이미 모든 행을 dirty로 표시).
- 166의 캔버스 크기는 바뀔 때만 다시 설정합니다(재할당 자체 비용은 작지만 전체 지움을 막음).
- 171의 aria-label은 500ms 디바운스합니다.
- 배율 상한: `scale = Math.min(dpr, Math.sqrt(16e6/(cssW*cssH)))`. 200x60, dpr3이면 4800×3240 = 15.55M px로 iOS Safari 캔버스 면적 한도(약 16.7M px, 실기 미확인)에 거의 닿고, 그보다 큰 격자는 넘습니다.
- 회귀 테스트: `mobile_terminal.cjs`의 가짜 캔버스에서 `fillText` 호출 수를 셉니다. 한 행만 바뀌면 그 행 칸 수만, Reset이면 전체.

**P2-9** `run_as`(`attach.rs:150`)에 `frame_min` 인자를 추가하고 342에서 씁니다. `registry.rs:539`에서는 `mobile`이면 33ms를 넘깁니다. 회귀 테스트는 `gateway/tests.rs` `ws_attach_input_and_resize_preserve_real_desktop_pty`와 attach 테스트입니다.

**P2-10** 탭을 바꿀 때마다 새 WS(TCP+TLS+upgrade+인증+attach)가 생깁니다. `terminalConnect()`가 미리 인증까지 마친 `{socket, channel}`을 하나 준비해 두고, 터미널은 attach만 보내게 합니다. 이전 조사의 "WS 통합 금지"와는 겹치지 않습니다.

**P3 메모**
- `ws::write`는 헤더와 본문을 `Vec` 하나에 담아 `write_all` 한 번으로 씁니다(LAN 직결 모드에서 세그먼트 3~4개 → 1개, 미측정).
- `ws::upgrade`는 101 응답 전에 브라우저가 프레임을 보내지 않으므로 작은 버퍼 읽기로 바꿀 수 있습니다.
- `pwa.rs:137`: pages.dev가 NXDOMAIN이라 지금 실사용자는 전부 터널로 PWA를 받고, 서비스 워커도 없습니다(SW는 고정 출처에서만 등록, `client.js:19`). `no-cache`+`ETag: asset_hash`로 304를 쓰면 매 실행 약 34KB를 아낍니다.
- snapshot 9.5KB × 약 2.8초 주기 ≈ 시간당 12MB(추정)이므로, P0-1 뒤에 해시를 비교해 `{"unchanged":true}`를 돌려주는 방안을 검토할 수 있습니다.

**하지 말 것(유지)**: JS 암호화 재작성(33MB/s 실측), permessage-deflate, wire 풀링(0.2ms 실측).

## 4. 미확인 항목

- **폰 실기**: LTE RTT, 폰 CPU에서의 draw·디코드 비용(Mac 대비 2~4배 느리다는 것은 추정), iOS 캔버스 한도 동작. 폰에서 측정하지 않았습니다.
- **폰 엣지 기준 530 지속 시간**: Mac은 icn06에서만 측정했습니다. 콜로·시간대에 따른 차이와, 이름 있는 터널이면 문제가 없는지도 확인하지 못했습니다.
- **cloudflared 쪽 버퍼링**: 대량 출력 중 LTE에서 버퍼가 쌓이는지는 병목을 흉내 내지 못해 재지 못했습니다.
- **WS over HTTP/2(RFC 8441)와 TLS 재사용**: Cloudflare 엣지가 지원해 터미널 소켓이 TLS를 다시 맺지 않아도 되는지 확인하지 못했습니다.
- **WS open 1000ms 이상치**: 터널에서 15회 중 1회 나왔고 원인은 모릅니다.
- **개선 효과 수치**: P0-2(재현으로 −25ms)를 빼고 모두 추정입니다. 구현 뒤 `/tmp/mx/boostB/bench/{snap,hs,term}.mjs`로 다시 재야 합니다.
- **기타 추정·미측정**:
  - git CPU 점유는 rusage 재현에서 얻은 세션당 45~57ms를 바탕으로 한 추정입니다.
  - Capture·List·States는 따로 떼어 재지 않았고, snapshot − Σgit이 잡음 수준이라는 데서 나온 상한 추정입니다.
  - 고정 출처 Pages 사이트(sw.js)는 저장소에 없어 확인하지 못했습니다.
  - Windows 경로와 LAN 직결 모드는 측정하지 않았습니다.

---

## 5. 적용 결과 (2026-10-19, 브랜치 `craftowen/mobile-speed`)

적용: P0-1 snapshot git(비저장소 생략·cwd 중복 제거·병렬·5초 캐시·`# branch.head` 파싱), P0-2 git 자식 대기 `poll(2)`, P0-3 터널 준비(터널 출처: URL 뒤 33초 + 연속 8회 200, `MINT_WAIT` 45초 / 고정 출처 배포 시: 첫 200), P1-4 accept `poll` 대기, P1-5 인증 fsync 제거(10분 디바운스), P1-6 핸드셰이크 파이프라이닝, P1-7 동작 후 비차단 갱신·숨김 탭 폴링 중지·라이브 터미널 10초, P1-8 dirty 행 rAF 그리기·캔버스 면적 상한, P2-9 모바일 프레임 간격 33ms, P2-11 QR 대기 50ms, P3 WS 프레임 1회 쓰기.
추가 수정: 인증 뒤 첫 요청 전 5초 넘게 쉬거나 레지스트리가 먼저 끝나면 WS가 닫히지도 응답하지도 않던 멈춤(회귀 테스트로 수정 전 10초 멈춤 확인), 인증 거부 시 파이프라인 프레임을 비워 1008 유지, 첫 페어링도 전송 실패 시 재시도.

검증: `scripts/check.sh` exit 0(2070 passed, 15 skipped, flaky 1 = usage 계정 테스트, 재시도 통과).

통합 release 실측(loopback, 격리 데몬, zsh 세션 6개, 2회 실행 값):

| 항목 | 571f4ba4 | 적용 후 |
|---|---|---|
| snapshot 연속 20회 median(대부분 캐시 적중) | 289.8 / 299.8ms | 1.9 / 4.9ms |
| snapshot 캐시 미스 median(5.3초 간격 5회) | 353.3 / 342.6ms | 125.7 / 148.9ms |
| WS open p50 / p90 | 32.9·24.2 / 52.3·49.4ms | 0.8·0.8 / 1.3·1.4ms |
| 인증 p50 | 7.2 / 8.0ms | 0.2 / 0.3ms |
| 핸드셰이크 합계 p50 | 40.1 / 34.3ms | 1.1 / 1.2ms |
| 터미널 한 행 변경 그리기(헤드리스 Chrome dpr3, 200×60) | 14.9~19.6ms | 0.2~0.3ms |
| 새 터널 QR 뒤 non-200(20초 동안) | 21/47, 50/50 | 0/50 ×3 |
| 새 터널 QR 발급(터널 출처) | 6.0 / 15.1초 | 37.0 / 38.8 / 39.2초 |

절충: 고정 출처가 배포되지 않은 지금은 첫 QR이 약 37~39초 걸리는 대신 첫 스캔이 530 없이 열린다. pages.dev 배포 뒤에는 첫 200에서 QR이 나온다(미측정).
미확인: 폰 LTE 실기, 고정 출처 빠른 경로 e2e, 파이프라이닝으로 줄어든 RTT의 실측, Windows 빌드.
