# 모바일 브라우저 관찰 — 2026-10-20

대상 HEAD e0a8a4e53a3c0265c4fb8345b54622a8e3bcd370 + 미커밋 작업트리. Chromium, loopback HTTP, 격리 fixture. 실제 daemon HTTP 에셋과 소스 바이트 일치 비교 통과. iOS 실기기 미검증.

## B01 — 검색창을 닫아도 검색 필터가 남고 활성 표식이 없음 (P2, 재현 2회)

1. 데스크톱 워크스페이스 전체 4개 표시: [시작](screenshots/search-step-1.png).
2. Search 버튼 → feature-worktree 입력. 1개 결과: [검색](screenshots/search-step-2.png).
3. Search 버튼으로 닫기. 입력창/검색어/활성 표식 없이 1개만 표시하며 상단은 4 workspaces 유지: [결과](screenshots/search-step-3.png).

영상: [재현](videos/issue-search-hidden.webm). 기대: 닫을 때 해제하거나 검색중 필터 칩과 초기화 동작 표시. 사용자 영향: 나머지 작업이 사라진 것처럼 보임.

## B02 — 터미널 주요 컨트롤이 작음 (P2, DOM 실측)

390x844 viewport에서 Back/Git/actions/키보드/보조키/첨부 36px 높이, Chat/Term 30px 높이. 화면 전체 scrollWidth=390. 터치 면적 개선 후보이며 실제 오탭 빈도는 미측정. 스크린샷 02-session.png / 03-chat.png. 44px 같은 외부 표준 준수 여부를 주장하지 않음.

## 정상/제외 관찰

- 처음 연결 자동 페어링 후 workspace로 이동. 설치 안내 시트 표시.
- Chat 초안은 다른 terminal 탭으로 이동했다가 돌아오면 보존됨. 초안 소실로 지적하지 않음.
- Chat/Term aria-label이 즉시 snapshot에서 이전 값이었으나 이후 업데이트됨. 영구적 오라벨로 지적하지 않음.
- 가로 844x390에서 action sheet는 스크롤 컨테이너. 하단 버튼이 첫 viewport 밖이라는 이유만으로 접근 불가로 분류하지 않음.
- fixture 계정 사용량이 백그라운드 갱신 후 Unknown/Check failed가 됨. 실계정 인증이 없는 fixture이므로 제품 오류로 분류하지 않음.
- 브라우저 errors 없음(현재까지).

추가 정상 관찰:
- Source Control의 Back은 진입한 세션 Chat 화면으로 복귀함. 상위 Home으로 튄다고 지적하지 않음.
- Buffered command 초안도 세션 탭 왕복 뒤 보존됨(14-buffered-draft-return.png).
- Text size/Default input mode 및 Show Esc 변경이 reload 후 유지됨(Buffered 입력 및 Esc 숨김).
- Buffered에서 한글 `모바일 확인` 전송 후 격리 PTY read가 `got:모바일 확인`을 반환함. 브라우저 errors 없음.
- CDP offline 토글만으로 이미 열린 WebSocket의 실제 단절을 입증하지 못했으므로 Connected 표시는 결함으로 확정하지 않음.

## B03 — 응답 대기 중 새로 입력한 Chat 초안 소실 (P1, 실제 Chromium+격리 daemon, 2회 재현)

소스나 RPC monkeypatch 없이, 격리 fixture daemon에 SIGSTOP/SIGCONT로 응답 지연을 만들었다. 사용자/운영 데몬은 조작하지 않았다.

1. Chat composer에 first message 입력: [전송 전](screenshots/draft-step-1.png).
2. 격리 daemon 일시중지, Send 클릭. 응답 대기 중 아직 편집 가능한 composer에 `다음 질문 초안: 저장되어야 합니다`를 추가 입력: [응답 대기/새 편집](screenshots/draft-step-2.png). 영상 중 단축키로 전체 선택을 시도했지만 실제로는 텍스트가 추가 입력되었다. 이 추가분은 첫 Send 이후의 미전송 편집이다.
3. daemon 재개→첫 전송의 성공 응답 도착→추가 편집까지 전체 입력이 빈 값으로 바뀜: [결과](screenshots/draft-step-3.png).

영상: [재현](videos/issue-chat-draft-lost.webm). 기대: 전송한 버전만 정리하고 응답 대기 중 사용자 새 편집은 보존. 첫 검증에서는 first verify 전송 후 값을 second draft must survive로 바꿨으며 역시 응답 후 빈 값이었다. 실제 실행한 첫 확인과 기록재현 각각 1회. 단순 탭 전환 초안 보존과는 다른 경로다.

## B04 — workspace 목록 갱신 시 행의 키보드 포커스 소실 (P2, Chromium 2회 재현)

1. workspace 목록의 feature-worktree 행에 키보드 포커스: activeElement BUTTON, text=feature-worktreefeature/mobile1. [시작](screenshots/focus-step-1.png).
2. 격리 CLI로 다른 workspace(repo)에 새 세션을 생성하여 snapshot 변경 유발. 다음 snapshot 뒤 activeElement가 BODY로 바뀌고 포커스 윤곽이 사라짐. [결과](screenshots/focus-step-2.png).

[영상](videos/issue-workspace-focus.webm). 1차 확인과 영상 기록에서 각각 재현. 동일 실험에서 Search 헤더 버튼은 포커스를 유지했으므로 화면 전체의 모든 버튼이 사라진다고 일반화하지 않음. 실제 VoiceOver/TalkBack 탐색 커서 영향은 미확인. 기대: stable workspace 행 노드를 유지하거나 같은 행으로 키보드 포커스 복원.
