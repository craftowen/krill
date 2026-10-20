# P1 connect / locate 세대 경합 수정

- Task: `task_13f1e948578e`, Dispatch: `ctx_1e9368c6e718`.
- 기준 HEAD: `e0a8a4e53a3c0265c4fb8345b54622a8e3bcd370`의 기존 미커밋 변경을 포함한 작업 트리. 기존 변경을 보존했다.
- 수정 파일: `crates/krilld/src/mobile/assets/client.js`, `crates/krilld/tests/mobile_transport.cjs`만.

## 결과

연결 실패 후 진행 중이던 이전 세대의 locate가 Reconnect 이후 완료되어 새 연결을 끊거나 목적지·저장 인증 정보·페이지 위치를 바꾸는 P1을 수정했다.

- client.js:44: `locate({isCurrent, signal}={})` 계약을 추가했다. 인자 없는 기존 호출도 동작한다.
- client.js:46–52: 조회 시작 rid를 캡처하고 fetch와 JSON 응답 직후 세대 유효성, 취소 여부, rid 일치 여부를 검사한다. 검증이 끝나기 전에는 routing, credentials, localStorage, location을 변경하지 않는다. 취소 신호를 fetch로 전달하고 JSON 처리까지 기존 5초 제한을 유지하며 타이머와 abort 리스너를 정리한다.
- client.js:86–109: 연결 시도마다 AbortController와 살아 있는 세대 검증 함수를 만든다. locate에 둘을 전달하며, locate 및 연결 과정의 각 await 이후 검증을 수행한다. 오래된 catch는 fallback, stopped, 오류 상태, disconnect를 변경하지 않고 종료한다.
- client.js:111–112: Reconnect와 close가 이전 조회를 취소한다. 이전 finally는 새 시도의 controller를 지우지 않는다.
- 테스트: 기존 VM/socket/clock fixture를 확장해 bootstrap endpoint와 동일한 handshake helper를 재사용한다. 구현 소스를 테스트에 복사하지 않았다.

## 검증

- `node --test crates/krilld/tests/mobile_transport.cjs`: **21/21 통과**. 기존 14개에 아래 7개 회귀를 추가했다.
  - 지연된 locate(false/true)가 새 소켓의 connecting/ready 각각에서 완료되는 4개 경우: 새 소켓 유지, 상태/fallback 불변, 새 handshake 완료, 이후 추가 Reconnect 성공.
  - fetch 응답 또는 JSON 응답이 지연된 2개 경우: 취소를 무시하고 성공 응답이 늦게 도착해도 url/rid/hash, 저장 credential, endpoint credential, location, 새 소켓/ready 상태 불변.
  - close 후 늦게 도착한 JSON 응답: 저장 정보와 location 불변.
- 같은 최종 테스트를 수정 전 작업 트리 client.js 스냅샷에 적용: **기존 14개 통과, 새 7개 모두 실패**. 새 소켓 닫힘, stale routing 적용, stale credential 저장으로 실패해 실제 동작 회귀를 검출한다. 로그: `/tmp/krill-mobile-fix-20261020/connect-baseline-regression.log`.
- 기존 `mobile_pwa.cjs`의 `fixedOriginTests()`를 파일에서 그대로 읽어 메모리에서 실행: **통과**. 실제 crypto 모듈을 사용하며 기본 locate 호출, relay/fallback, hash redirect, pre-open retry, resume, 인증 메타데이터, Home Screen handoff를 확인했다. 실행기: `/tmp/krill-mobile-fix-20261020/run-fixed-origin-fixture.cjs`.
- `node --check` 두 수정 파일 및 `git diff --check -- <두 파일>`: **통과**.
- 처음 인자 없이 전체 `mobile_pwa.cjs` 실행을 시도했으나 필수 daemon 경로 인자 누락으로 시작 전 종료했다. 이후 위의 기존 deterministic fixture만 분리 실행했으며 daemon/운영 세션을 띄우지 않았다.

## 남은 통합 작업

이 worker의 P1 범위는 완료했다. Cargo, gzip 산출물, app.js, mobile_pwa.cjs, 하네스, 운영 세션, 커밋/푸시/배포는 건드리지 않았다. gzip 갱신 및 전체 PWA/릴리스 검증은 메인 소유 작업으로 남아 있으며 이 보고서는 전체 검사 통과를 의미하지 않는다.
