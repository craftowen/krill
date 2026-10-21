# 모바일 업데이트 후 연결 실패: 운영 복구 기록 (2026-10-21)

기준 앱 HEAD: `b214d1ae8539b58871580ec268f5cd894a55453e` (v0.1.25).
소스 검증은 이 HEAD 위 `relay.rs`·`ws.rs` 수정과 기존 모바일 WIP가 있는 작업 트리에서 실행했다.
아래 서버 배포와 앱 바이너리 배포를 구분한다. 사용자 휴대폰의 직접 확인은 아직 없다.

## 재현과 원인

1. 서버의 UI asset hash 불일치 409 거부를 제거한 Worker
   `f97b59fb-50eb-4cfb-9996-6ae8556af7a4`만으로는 전체 연결이 복구되지 않았다.
   합성 host/client의 WebSocket 101은 실제 데몬의 인증·첫 snapshot 성공을 보장하지 않는다.
2. 운영 중인 이 Mac의 기존 Home Screen 기기 검증자로 읽기 전용 연결을 확인했다.
   DO 경로는 인증·ServerHello 후 `1012 Host disconnected`로 종료됐다.
   같은 데몬의 direct tunnel은 36세션, 77,153~77,172바이트의 snapshot을 반환했다.
3. `Transport::write_masked`는 TLS plaintext를 `conn.writer().write_all`로 모두 버퍼링한 뒤에야
   `write_tls`를 호출했다. rustls 0.23.46의 기본 송신 버퍼는 64KiB이며, 이를 넘는 프레임은
   버퍼를 비우지 못하고 쓰기에 실패해 호스트 릴레이 전체를 끊는다.
4. 데몬의 direct `/ws`에도 asset hash 차이를 409로 거부하는 조건이 두 번 들어 있었다.
   릴레이 서버 조건만 고쳐서는 direct 경로의 구버전 연결을 보장할 수 없다.

## 운영 서버 복구 — 배포됨

Pages의 `/r/<rid>` WebSocket 경로가 해당 rid에 게시된 유효한 Quick Tunnel이 있으면
그 터널의 `/ws`로 연결한다. UI hash는 넘기지 않으며, E2EE 인증과 실제 프로토콜 협상은
그대로 krilld가 수행한다. Pages 쿠키·query·호스트 게시 credential은 전달하지 않는다.
실패하거나 5초 안에 연결되지 않으면 기존 DO multiplexing으로 돌아간다.
터널을 게시하지 않는 호스트의 DO 경로에는 아래 데몬 수정이 별도로 필요하다.

- Preview: `37646e60.krill-mobile.pages.dev`
- Production: `28a6057c.krill-mobile.pages.dev` (생산 별칭 `krill-mobile.pages.dev`)
- 변경 파일: Git 제외 경로 `cloud/mobile/rendezvous/logic.js`, `cloud/mobile/test/rendezvous.test.js`
- 동결 배포본: `/Volumes/krill-build/mobile-pwa-fix-20261021/pages-recovery/`
- 정적 PWA 파일은 v0.1.24 최종 검증 배포본을 그대로 재사용했다. 16개 hash의 기존 파일을 보존했다.
  canonical URL에서 공개 파일 276/276의 SHA-256이 동결본과 일치한다.

## 데몬 재발 방지 — 소스 수정·부분 검증 완료, 바이너리 미배포

- TLS는 `StreamOwned::write_all`과 `flush`로 전송해 부분 쓰기마다 암호문 버퍼를 비운다.
- `/ws?v=...`는 정상 16자리 소문자 hex asset hash의 차이를 허용한다.
  잘못된 형식·중복 값·허용되지 않은 Origin은 계속 거부한다.
- 로컬 테스트 전용 localhost 인증서·키 fixture로 실제 TLS를 연결하고 128KiB·최대 허용
  프레임 및 그 다음 메시지를 연속 전송해 바이트가 유지되는지 확인한다.
  해당 키는 테스트 fixture이며 서비스 인증에 사용하지 않는다.
- 기존 모바일 UI/자동 갱신 WIP 14파일은 별도로 보존했다. 이 기록의 변경에 포함하지 않는다.

## 검증 범위

- cloud/mobile Node 테스트: 21 PASS.
- krilld 영향 경로 nextest: 9 PASS (`mobile::ws::tests`, `mobile::relay::tests`).
- `cargo clippy -p krilld --all-targets --locked -- -D warnings`: PASS.
- fmt 및 diff 공백 검사: PASS.
- 운영 Pages 경로에서 브라우저와 동일한 Origin·기존 기기 검증자로 인증 후
  77,172바이트/36세션 snapshot을 약 1.8초, 7.6초, 13.4초에 연속 수신했다.
- 격리 Chromium, 390×844: 생산 v0.1.24 PWA와 구 v0.1.23 PWA
  (`7a81df8542af4d0f`) 모두 `ready / Encrypted · connected`, 36세션 표시,
  브라우저 오류 없음. 사용자의 원 credential을 추출하지 않고, 테스트 전용 credential
  대용값에만 로컬 검증자를 적용한 격리 브라우저를 사용했다. 검증 후 브라우저와 임시 파일을 닫고 지웠다.
- 사용자 휴대폰 자체, 다른 PC, 터널 없는 호스트에 대한 결과로 확대하지 않는다.
- 전체 `scripts/check.sh`나 새 데스크톱 릴리스 검증을 수행했다고 보고하지 않는다.

원시 검증·배포 로그: `/Volumes/krill-build/mobile-pwa-fix-20261021/`.
추적: Linear Lite OWE-989 (사용자 실패 지속 보고 후 다시 진행 중).
