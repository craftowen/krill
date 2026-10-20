# 모바일 홈: 마지막 세션과 CLI 계정

검증 기준: `e0a8a4e53a3c0265c4fb8345b54622a8e3bcd370` 위의 수정된 작업 트리(2026-10-20). 전체 릴리스 검사가 아닌 아래 범위의 개발 검사다. Cargo 산출물은 `/Volumes/krill-build/targets/shared`를 사용했다.

- 홈의 마지막 접속 세션을 누르면 해당 세션을 연다. 연결이 끊겼다면 재연결한 새 스냅샷으로 존재 여부를 확인한다. 종료·삭제·작업 공간 불일치 세션은 연결하지 않는다.
- 마지막 세션 위치는 페어링 기기별로 저장하며, 이전 단일 기록도 읽는다. 호스트를 바꿔도 세션 ID가 다른 호스트로 넘어가지 않는다. 터미널 출력은 저장하지 않는다.
- 연결된 호스트 카드에 Claude Code, Codex, agy CLI의 활성 계정과 로그인 연결 상태를 표시한다. 계정 행을 누르면 기존 계정 화면을 연다. 조회 실패에는 재시도가 있고, 로그인 상태와 사용량 조회 오류는 구분한다.
- 계정 응답에 CLI 프로브의 현재 이메일을 반영한다. Claude는 기존 프로브 결과를 사용하고, Codex·agy는 로컬 자격 증명의 표시용 신원을 추출한다. 토큰·JWT 원문은 계정 응답에 넣지 않는다. 이메일을 확인할 수 없으면 미확인으로 표시하며, JWT 신원으로 로그인 성공을 판정하지 않는다.

## 검증

- `cargo check -p krilld --locked`: 통과.
- `cargo clippy -p krilld --all-targets --locked -- -D warnings`: 통과.
- 수정한 Rust 파일의 `rustfmt --edition 2024 --check`, `git diff --check`: 통과.
- `cargo nextest run -p krilld --locked -E 'test(usage::accounts::identity) | test(login_regression_tests) | test(antigravity) | test(mobile_frontend_regressions) | test(session_screen_regressions) | test(terminal_adapter_regressions) | test(embedded_routes_and_gzip_budget) | test(disk_hash_and_source_file_inventory_match)'`: **14개 통과**. 신원 파서의 잘못된 입력 proptest, 계정 응답 반영, 로그아웃 신원 제외, 기존 로그인 회귀, 모바일 화면·전송·터미널 회귀, gzip 일치·용량 검사를 포함한다.
- 내장 자산은 원본과 gzip 내용이 일치하고 기존 150KiB 압축 예산 안에 있다. 수정된 자산의 gzip은 결정적 Zopfli 압축으로 생성했으며 실행 의존성을 추가하지 않았다.
- `agent-browser` Chromium에서 실제 프런트엔드 자산과 격리된 가짜 계정/전송/터미널을 사용했다. 세션 방문→홈→바로 열기, 새로고침 후 기록 복원, 연결 끊김→세션 재접속, 재접속 중 종료된 세션 거부를 확인했다. 390×844와 320×568·글자 150%에서 긴 이메일의 줄바꿈 및 가로 넘침이 없음을 확인했다. 실제 스마트폰·실제 계정의 전체 네트워크 연결 검증은 아니다.
- 워크스페이스 전체 `cargo fmt --all --check`는 동시 작업 중인 별도 파일 `crates/krill-app/src/diff/explorer.rs`의 포맷 차이로 실패했다. 이 파일은 이번 작업에서 수정하지 않았다.
- 전체 `scripts/check.sh`, 배포 및 사용자 데몬 재시작은 실행하지 않았다.
