# 모바일 UI/UX 조사 증거 — 2026-10-20

최종 보고서: [mobile UX audit](../../boost-2026-10-20-mobile-ux-audit.md).

대상은 HEAD `e0a8a4e53a3c0265c4fb8345b54622a8e3bcd370`와 당시 미커밋 작업트리다. 코드 지적의 줄 번호는 작업트리 기준이다. 제품 코드 수정 없이 Orca 조사 워커 3개, 독립 검증 워커 1개, 코디네이터의 격리 Chromium 탐색을 수행했다.

## 증거별 검증 수준

- [브라우저 관찰](browser-findings.md): 실제 Chromium + 격리 holder/daemon/PTY. 검색 숨김, 전송 중 새 편집 소실, 목록 행 포커스 소실의 재현 단계·스크린샷·영상을 포함한다.
- [Node 재현 코드](terminal-A-repro.cjs), [stdout](terminal-A-repro.stdout.txt): 현재 JS를 가짜 DOM/소켓과 가상 시간에서 실행한다. 실제 모바일 키보드·네트워크·PTY 실측을 대신하지 않는다.
- [PTY 결과](draft-terminal-output.txt): 실제 fixture에서 전송된 메시지만 출력되고 응답 대기 중 추가 편집은 전송되지 않았음을 확인한다.
- [소스 SHA-256](source-hashes.json): UI 에셋 및 PC 모바일 설정 42개 파일. 브라우저 확인 전후 지문 변화 없음.
- `screenshots/`에는 390×844, 320×568, 844×390 viewport 화면이 있다. 각 파일의 viewport는 관찰 문서의 설명을 따른다.

실제 iPhone/Android, Safari/PWA 설치, VoiceOver/TalkBack, 실기기 한글 IME·화면 키보드·카메라·LTE/Wi-Fi 이동은 검증하지 않았다. 컨트롤 크기를 측정했지만 오탭 빈도나 접근성 표준 적합률은 측정하지 않았다.

## 브라우저 재현 영상

| 문제 | 단계별 화면 | 영상 |
|---|---|---|
| 검색창을 숨긴 뒤 필터 상태가 보이지 않음 | [전체](screenshots/search-step-1.png) · [검색](screenshots/search-step-2.png) · [숨김 뒤](screenshots/search-step-3.png) | [검색 재현](videos/issue-search-hidden.webm) |
| Chat 전송 응답이 새 편집까지 지움 | [전송 전](screenshots/draft-step-1.png) · [대기 중 편집](screenshots/draft-step-2.png) · [응답 후 소실](screenshots/draft-step-3.png) | [초안 소실 재현](videos/issue-chat-draft-lost.webm) |
| 다른 작업 변경 후 workspace 행 포커스 소실 | [포커스 있음](screenshots/focus-step-1.png) · [갱신 후](screenshots/focus-step-2.png) | [포커스 재현](videos/issue-workspace-focus.webm) |

세 동작은 첫 확인 후 기록 재현을 각각 수행했다. Chat 영상은 응답을 늦추기 위해 격리 fixture daemon만 SIGSTOP/SIGCONT했다. 응답 대기 중 전체 선택 단축키는 실제로 텍스트를 선택하지 못해 문자가 추가됐다. 첫 Send 이후 추가된 미전송 편집이 응답 때 함께 사라진다는 결론은 변하지 않는다. 사용자 데몬/세션은 조작하지 않았다.

## 격리 fixture 재현 방법

저장소 루트에서 기존 바이너리 위치를 지정한다. Cargo 빌드가 필요할 경우 target은 `/Volumes/krill-build/targets/` 아래에만 둔다.

```sh
M9_UI_QA=1 M9_BROWSER_ADDRESS=127.0.0.1:0 \
M9_BROWSER_METADATA=/tmp/krill-mobile-ux-fixture.json \
node crates/krilld/tests/mobile_pwa.cjs \
  /Volumes/krill-build/targets/shared/debug/krilld --keep
```

fixture metadata의 URL을 별도 agent-browser 세션으로 연다. metadata는 자격증명을 포함할 수 있어 이 증거 디렉터리에는 복사하지 않았다. fixture owner에 SIGTERM을 보내면 임시 holder/daemon/PTY가 정리된다.

브라우저 실행에는 기존 debug 바이너리를 사용했다. fixture의 HTTP gzip 해제 에셋과 현재 UI 소스의 바이트 일치 비교는 통과했다. Rust 전체를 이번 조사에서 재빌드하지 않았으므로 기존 바이너리가 미커밋 Rust 변경 전부를 포함한다고 주장하지 않는다.

```sh
node docs/research/mobile-ux-evidence-2026-10-20/terminal-A-repro.cjs "$PWD"
```

Node 재현은 제품 소스를 읽기만 하고 네트워크/실계정/실제 PTY를 사용하지 않는다. 전체 Cargo 검사·릴리스 검사 통과를 의미하지 않는다.
