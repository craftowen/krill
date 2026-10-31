// UI copy only. Transcript, repository names and terminal output are never translated.
(() => {
  const M = (globalThis.KrillMobile ||= {});
  let language = "en";
  const ko = {
    "Requested {0}": "요청한 설정 {0}",
    "Context used {0}%": "컨텍스트 {0}% 사용",
    "No model catalog is available. Open the model picker in Term.": "모델 목록을 불러올 수 없습니다. 터미널에서 모델을 선택하세요.",
    "Applying…": "적용 중…",
    "Effort unknown": "사고 수준 확인 불가",
    "Model unknown": "모델 확인 불가",
    "Model and effort": "모델 및 사고 수준",
    "Effort": "사고 수준",
    "This pairing code is invalid. Scan a fresh QR code from desktop Settings → Mobile.": "올바르지 않은 페어링 코드입니다. 데스크톱 설정 → 모바일에서 새 QR 코드를 스캔하세요.",
    "Desktop backup added.": "데스크톱의 예비 연결을 추가했습니다.",
    "Pair this desktop again?": "이 데스크톱을 다시 페어링할까요?",
    "Pair again": "다시 페어링",
    "The saved pairing was rejected. Pairing again replaces its saved credential on this phone.": "저장된 페어링 인증이 거부되었습니다. 다시 페어링하면 이 휴대폰에 저장된 인증 정보가 교체됩니다.",
    "This desktop code uses an older Tailscale address. Create a new Tailscale QR code in desktop Settings → Mobile.": "이 코드는 이전 방식의 Tailscale 주소를 사용합니다. 데스크톱 설정 → 모바일에서 새 Tailscale QR 코드를 만드세요.",
    "Automatic technical error collection": "기술적 오류 자동 수집",
    "Your browser privacy signal is enabled. Error collection is off.":
      "브라우저의 추적 방지 설정에 따라 오류 수집이 꺼져 있습니다.",
    "Automatic technical error reports help fix connection failures. No terminal content, addresses or credentials. Reports stay in memory until sent or this page closes. You can turn reporting off here.":
      "연결 문제를 해결하기 위해 기술적 오류를 자동 보고합니다. 터미널 내용·주소·인증 정보는 포함하지 않습니다. 보고는 전송되거나 이 페이지를 닫을 때까지 메모리에만 보관합니다. 여기서 오류 보고를 끌 수 있습니다.",
    "Error collection notice": "오류 수집 안내",
    "Technical errors are collected automatically to diagnose connection and app failures. You can turn collection off in Settings.":
      "연결과 앱 문제를 진단하기 위해 기술적 오류를 자동 수집합니다. 설정에서 수집을 끌 수 있습니다.",
    "Reports include connection stages, observed status codes, timings, coarse browser and OS categories, and public asset locations. Unsent reports stay only in memory for up to 30 minutes and are discarded when this page closes or reloads.":
      "보고에는 연결 단계, 확인된 상태 코드, 소요 시간, 브라우저·운영체제 종류, 공개 앱 파일의 오류 위치가 포함됩니다. 전송 전 보고는 최대 30분간 메모리에만 보관하며 페이지를 닫거나 새로고침하면 삭제됩니다.",
    "No terminal content, URLs, raw error text or credentials are included. Browser Do Not Track and Global Privacy Control stop collection.":
      "터미널 내용·URL·원문 오류 메시지·인증 정보는 포함하지 않습니다. 브라우저의 Do Not Track 또는 Global Privacy Control이 켜져 있으면 수집을 중단합니다.",
    "Paired device was revoked. Scan a new QR code.":
      "페어링된 기기의 접근 권한이 해제되었습니다. 새 QR 코드를 스캔하세요.",
    "Pairing link expired or was already used. Scan a new QR code.":
      "페어링 링크가 만료되었거나 이미 사용되었습니다. 새 QR 코드를 스캔하세요.",
    "Access denied. Scan a new QR code from desktop Settings → Mobile.":
      "접근이 거부되었습니다. 데스크톱 설정 → 모바일에서 새 QR 코드를 스캔하세요.",
    "Connection closed (1008): {0}": "연결이 종료되었습니다 (1008): {0}",
    "Disconnected. Reconnecting…": "연결이 끊겼습니다. 다시 연결하는 중…",
    "Offline — waiting for network": "오프라인 — 네트워크 연결을 기다리는 중",
    "Diff is too large to display on mobile (128 KB limit). Open this file on desktop to view the full diff.":
      "모바일에서 표시할 수 있는 변경 크기(128 KB)를 초과했습니다. 전체 변경은 데스크톱에서 이 파일을 열어 확인하세요.",
    "Diff is outside the current workspace scope.":
      "현재 작업 공간 범위 밖에 있는 변경입니다.",
    "No current diff for this file.": "이 파일에는 현재 변경이 없습니다.",
    "Reset order": "순서 초기화",
    "Move up": "위로 이동",
    "Move down": "아래로 이동",
    "Desktop device limit reached. Revoke an unused device in desktop Settings → Mobile, then scan a new QR code.":
      "데스크톱에 연결할 수 있는 기기 수를 초과했습니다. 데스크톱 설정 → 모바일에서 사용하지 않는 기기를 해제한 뒤 새 QR 코드를 스캔하세요.",
    "Authentication retry is cooling down. Wait before reconnecting.":
      "인증 재시도 대기 중입니다. 잠시 후 다시 연결하세요.",
    "Authentication retry is cooling down. Wait, or scan a fresh QR code.":
      "인증 재시도 대기 중입니다. 기다리거나 새 QR 코드를 스캔하세요.",
    "This QR code expired or was already used. Tap New QR code on desktop and scan again.":
      "QR 코드가 만료되었거나 이미 사용되었습니다. 데스크톱에서 새 QR 코드를 누른 뒤 다시 스캔하세요.",
    "Desktop rejected authentication. Access may have been revoked; scan a fresh QR code.":
      "데스크톱에서 인증을 거부했습니다. 접근 권한이 해제되었을 수 있으니 새 QR 코드를 스캔하세요.",
    "Disconnected. Pending actions were not resent. If desktop access was revoked, scan a fresh QR code.":
      "연결이 끊겼습니다. 대기 중이던 동작은 다시 보내지 않았습니다. 데스크톱 접근 권한이 해제되었다면 새 QR 코드를 스캔하세요.",
    Skill: "스킬",
    Global: "전역",
    "Antigravity also saves this as the default for new sessions.":
      "Antigravity는 이 모델을 새 세션의 기본값으로도 저장합니다.",
    "Changes the model in this running Antigravity chat. Antigravity also saves it as the default for new sessions.":
      "실행 중인 Antigravity 채팅의 모델을 바꿉니다. Antigravity는 새 세션의 기본값으로도 저장합니다.",
    "Antigravity CLI was not found on this computer.":
      "이 컴퓨터에서 Antigravity CLI를 찾지 못했습니다.",
    "Model control needs Antigravity CLI 1.4.12 or later. Keep chatting with the current model.":
      "모델 변경은 Antigravity CLI 1.4.12 이상에서 지원합니다. 현재 모델로 계속 대화하세요.",
    "Antigravity did not return its model list. Try again in a moment.":
      "Antigravity가 모델 목록을 주지 않았습니다. 잠시 후 다시 시도하세요.",
    "Antigravity models are still loading. Try again in a moment.":
      "Antigravity 모델 목록을 불러오는 중입니다. 잠시 후 다시 시도하세요.",
    "Wait for the running agent to become idle":
      "실행 중인 에이전트가 입력을 기다릴 때 바꿀 수 있습니다.",
    "Wait for an idle Antigravity chat with an empty composer.":
      "Antigravity가 입력을 기다리고 입력창이 비어 있을 때 바꿀 수 있습니다.",
    "Antigravity received the model command, but its confirmation was not recognized. It will not be resent.":
      "Antigravity가 모델 명령을 받았지만 확인 응답을 인식하지 못했습니다. 다시 보내지 않습니다.",
    "Claude also saves this as the default for new sessions.":
      "Claude는 이 모델을 새 세션의 기본값으로도 저장합니다.",
    "Verified model changes are unavailable on Windows. Keep chatting with the current model.":
      "Windows에서는 아직 모델 직접 변경을 지원하지 않습니다. 현재 모델로 대화를 계속할 수 있습니다.",
    "Changes the model in this running Codex chat session only.":
      "현재 Codex 세션의 모델만 변경합니다.",
    "Changes the model in this running Claude chat. Claude also saves its default for new sessions.":
      "현재 Claude 모델을 변경하고 새 세션의 기본값으로도 저장합니다.",
    "Wait for an idle Codex chat with an empty native composer. Direct model control needs Codex 0.164.0 or later.":
      "Codex 작업이 끝난 뒤 터미널 입력란을 비워 주세요. Codex 0.164.0 이상에서 모델 직접 변경을 지원합니다.",
    "Wait for an idle Codex 0.164.0 or later chat with an empty native composer":
      "Codex 작업이 끝난 뒤 터미널 입력란을 비워 주세요. Codex 0.164.0 이상에서 모델 직접 변경을 지원합니다.",
    "Wait for an idle Claude Code 2.1.311 or later chat with an empty native composer":
      "Claude 작업이 끝난 뒤 터미널 입력란을 비워 주세요. Claude Code 2.1.311 이상에서 모델 직접 변경을 지원합니다.",
    "Codex received the model command, but its session-only change was not confirmed. Check the chat; the command will not be resent.":
      "Codex에 모델 변경 명령을 전달했지만 적용은 확인되지 않았습니다. 활동 내역을 확인하세요. 명령을 자동으로 다시 보내지 않습니다.",
    "Claude received the model command, but its native confirmation was not recognized. It will not be resent.":
      "Claude에 모델 변경 명령을 전달했지만 적용은 확인되지 않았습니다. 명령을 자동으로 다시 보내지 않습니다.",
    "This running Codex session does not expose a verified model control connection. Discovered models are from local configuration/cache; no model was changed.":
      "이 Codex 세션에서는 모델 직접 변경을 지원하지 않습니다. 모델은 변경하지 않았습니다.",
    "This running Claude Code session does not expose verified model control. Configured models are shown for reference; no model was changed.":
      "이 Claude 세션에서는 모델 직접 변경을 지원하지 않습니다. 모델은 변경하지 않았습니다.",
    "This provider does not expose verified model control for the running chat session. No model was changed.":
      "이 에이전트에서는 모델 직접 변경을 지원하지 않습니다. 모델은 변경하지 않았습니다.",
    "Commands, skills and files": "명령어, 스킬 및 파일",
    "Close suggestions": "추천 닫기",
    "Workspace files": "작업 공간 파일",
    "Commands and skills": "명령어 및 스킬",
    "Searching…": "검색 중…",
    "Insert file path as text": "파일 경로를 텍스트로 삽입",
    "No matches": "검색 결과가 없습니다",
    "More results available. Keep typing.":
      "결과가 더 있습니다. 계속 입력해 범위를 좁히세요.",
    "Search unavailable. Try again.": "검색할 수 없습니다. 다시 시도하세요.",
    "Loading models…": "모델 목록 확인 중…",
    "Current model": "현재 모델",
    "Model selection is unavailable for this session.":
      "이 세션에서는 모델을 선택할 수 없습니다.",
    "Requesting model change…": "모델 변경 요청 중…",
    "Model change was not confirmed.": "모델 변경이 확인되지 않았습니다.",
    "Send not confirmed": "전송이 확인되지 않았습니다",
    "Accepted. Awaiting agent response…":
      "전송을 접수했습니다. 에이전트 응답을 기다리는 중…",
    "Offline. Reconnect to update activity.":
      "연결이 끊겼습니다. 다시 연결하면 활동 상태를 갱신합니다.",
    "Send not confirmed. Draft kept; check activity before retrying.":
      "전송이 확인되지 않아 초안을 보관했습니다. 다시 보내기 전에 활동 내역을 확인하세요.",
    "Session ended.": "세션이 종료되었습니다.",
    "Waiting for input": "입력 대기 중",
    "Agent blocked": "에이전트 진행이 막혔습니다",
    "Response received": "응답을 받았습니다",
    Offline: "연결 끊김",
    Sending: "전송 중",
    Accepted: "접수됨",
    "Default session view": "기본 세션 화면",
    "Find installed skills": "설치된 스킬 찾기",
    "Installed skills and commands": "설치된 스킬과 명령",
    "Search skills and commands": "스킬과 명령 검색",
    "Session for installed skills": "설치된 스킬을 조회할 세션",
    "Choose a session": "세션 선택",
    "Choose a session to see its installed skills and commands.":
      "설치된 스킬과 명령을 조회할 세션을 선택하세요.",
    "No live sessions. Open a session to find installed skills.":
      "실행 중인 세션이 없습니다. 세션을 열어 설치된 스킬을 찾아보세요.",
    "Session context changed. Reopen installed skills.":
      "세션 정보가 바뀌었습니다. 설치된 스킬 찾기를 다시 여세요.",
    "Could not load installed skills. Reopen to try again.":
      "설치된 스킬을 불러오지 못했습니다. 다시 열어 시도하세요.",
    "Installed skills are unavailable for this agent.":
      "이 에이전트에서는 설치된 스킬을 조회할 수 없습니다.",
    "Showing a limited list of installed skills.":
      "설치된 스킬 목록의 일부를 표시합니다.",
    "Add a skill to your saved quick actions.":
      "스킬을 저장된 퀵액션에 추가하세요.",
    "No skills or commands match your search.":
      "검색어와 일치하는 스킬이나 명령이 없습니다.",
    "No installed skills or commands found.":
      "설치된 스킬이나 명령이 없습니다.",
    "Already saved": "이미 저장됨",
    "Add to quick actions": "퀵액션에 추가",
    "Loading installed skills…": "설치된 스킬을 불러오는 중…",
    "This desktop uses a different Google account. Open Settings, sign out of Google, then sign in with the account used on this desktop.":
      "이 데스크톱과 Google 계정이 다릅니다. 설정에서 Google 계정을 로그아웃한 뒤 데스크톱에서 사용한 계정으로 로그인하세요.",
    "Sign in with Google": "Google로 로그인",
    "Sign in with Google to use the relay.":
      "릴레이를 사용하려면 Google로 로그인하세요.",
    "Sign out of Google": "Google 계정 로그아웃",
    "Could not preserve pairing. Allow session storage and try again.":
      "페어링 정보를 보관하지 못했습니다. 세션 저장소를 허용하고 다시 시도하세요.",
    "Could not sign out. Try again.":
      "로그아웃하지 못했습니다. 다시 시도하세요.",
    "No options selected. Send answer to continue without selections.":
      "선택한 항목이 없습니다. 답변 전송을 누르면 선택 없이 진행합니다.",
    "Answer the current question above": "위의 현재 질문 카드에서 답변하세요",
    "Current question": "현재 질문",
    "Past question": "지난 질문",
    "Answer question": "질문에 답변하기",
    "Continue answering": "이어서 답변",
    "Select one or more answers": "하나 이상 선택하세요",
    "Select one answer": "하나를 선택하세요",
    "Send answer": "답변 전송",
    "Sending answer…": "답변 전송 중…",
    "Write an answer in Term": "직접 입력은 터미널에서 계속",
    "Answer sent": "답변을 전송했습니다",
    "Selected {0}. Nothing is sent until you press Send answer.":
      "{0}개 선택 · 답변 전송을 누르기 전에는 전달되지 않습니다.",
    "Select an answer. It will not be sent immediately.":
      "답변을 고르세요. 선택만으로는 전송되지 않습니다.",
    "The question changed. Review the current question before sending.":
      "질문이 바뀌었습니다. 현재 질문을 다시 확인한 뒤 답변하세요.",
    "Waiting for the current question. Check Term if this continues.":
      "현재 질문을 확인하고 있습니다. 계속되면 터미널에서 확인하세요.",
    "Could not confirm the selection. Check the current question in Term.":
      "선택이 반영됐는지 확인하지 못했습니다. 터미널에서 현재 질문을 확인하세요.",

    Close: "닫기",
    Add: "추가",
    "Saved on this phone. Could not sync quick actions to desktop. Reconnect and save again.":
      "이 휴대폰에 저장했습니다. 데스크톱과 퀵액션을 동기화하지 못했습니다. 다시 연결한 뒤 저장하세요.",
    "Add quick action": "퀵액션 추가",
    "Skill, command, or prompt": "스킬, 명령어 또는 프롬프트",
    "Inserted into the active terminal or chat input without pressing Enter.":
      "활성 터미널 또는 채팅 입력란에 엔터 없이 입력됩니다.",
    "No quick actions yet. Save skills like /krill-orchestration, shell commands, or prompts to insert without pressing Enter.":
      "저장된 퀵액션이 없습니다. /krill-orchestration 같은 스킬, 셸 명령어, 프롬프트를 저장하고 엔터 없이 입력하세요.",
    "Insert a saved skill, command, or prompt. Press Enter separately to run.":
      "저장한 스킬, 명령어 또는 프롬프트를 입력합니다. 실행하려면 엔터를 따로 누르세요.",
    "Choose an answer": "답변을 선택하세요",
    "Select answers, then submit": "답변을 고른 뒤 제출하세요",
    "Submit answers": "답변 제출",
    Clear: "비우기",
    "Filter projects": "프로젝트 필터",
    "Filter projects...": "프로젝트 검색...",
    "Add project...": "프로젝트 추가...",
    "No projects match": "일치하는 프로젝트가 없습니다",
    "Remove {0} filter": "{0} 필터 제거",
    "{0} projects": "프로젝트 {0}개",
    "No terminals": "터미널 없음",
    "Split direction": "분할 방향",
    Automatic: "자동",
    "Side by side": "좌우",
    Stacked: "위아래",
    "Stacked: panes would be too narrow": "위아래로 표시: 칸 너비 부족",
    Active: "활성",
    "Activate pane": "칸 활성화",
    "Terminals in active pane": "활성 칸의 터미널",
    "Split terminal": "분할",
    "Open beside this terminal": "옆 칸에 열기",
    "Close pane": "칸 닫기",
    "No other terminals available.": "다른 터미널이 없습니다.",
    Sessions: "세션",
    "Hide session list": "세션 목록 숨기기",
    "Show session list": "세션 목록 보이기",
    "Recent sessions": "최근 세션",
    "Open Krill desktop → Settings → Mobile, then scan the QR code.":
      "PC의 Krill에서 설정 → 모바일을 열고 QR 코드를 스캔하세요.",
    "No terminals in this workspace.": "이 작업 공간에는 터미널이 없습니다.",
    Back: "뒤로",
    Cancel: "취소",
    Settings: "설정",
    Preferences: "환경 설정",
    "Settings sections": "설정 분류",
    Appearance: "화면 모드",
    System: "시스템 설정",
    "System default": "시스템 기본값",
    Light: "라이트",
    Dark: "다크",
    Language: "언어",
    "UI text size": "화면 글자 크기",
    Terminal: "터미널",
    Term: "터미널",
    Chat: "채팅",
    "Chat view": "채팅 보기",
    Workspace: "작업 공간",
    "Open workspaces": "작업 공간 열기",
    "Workspace Options": "작업 공간 옵션",
    "Workspace options ({0} filters)": "작업 공간 옵션(필터 {0}개)",
    "Saved on this device": "이 기기에 저장됨",
    "Reset filters": "필터 초기화",
    Projects: "프로젝트",
    "Group by": "묶음 기준",
    "Sort by": "정렬 기준",
    None: "없음",
    Status: "상태",
    Smart: "스마트",
    Recent: "최근",
    Created: "생성",
    Name: "이름",
    Filters: "필터",
    "Hide sleeping worktrees": "잠든 작업 공간 숨기기",
    "Always show default branch": "기본 브랜치는 항상 표시",
    "Hide automation worktrees": "자동화 작업 공간 숨기기",
    "Hide CLI worktrees": "CLI로 만든 작업 공간 숨기기",
    Todo: "할 일",
    "In Progress": "진행 중",
    "In Review": "검토 중",
    Completed: "완료",
    Apply: "적용",
    "All projects": "모든 프로젝트",
    "NEEDS YOU": "확인 필요",
    Search: "검색",
    "Clear search": "검색 지우기",
    "Search workspaces": "작업 공간 검색",
    "No matching workspaces": "일치하는 작업 공간이 없습니다",
    "No workspaces": "작업 공간이 없습니다",
    "Rename desktop": "PC 이름 변경",
    Save: "저장",
    "Desktop name": "PC 이름",
    "This name is saved on this phone.": "이 이름은 이 휴대폰에 저장됩니다.",
    "Enter a desktop name (1–80 characters).": "PC 이름을 1~80자로 입력하세요.",
    "Could not save desktop name.": "PC 이름을 저장하지 못했습니다.",
    Desktop: "컴퓨터",
    Desktops: "컴퓨터",
    "Quick actions": "퀵액션",
    "Switch to buffered input": "모아서 입력하기",
    "Switch to direct input": "직접 입력하기",
    "Desktop actions": "컴퓨터 동작",
    Connected: "연결됨",
    "Connecting…": "연결 중…",
    "Not connected": "연결 안 됨",
    Disconnected: "연결 끊김",
    Reconnect: "다시 연결",
    "Scan QR": "QR 스캔",
    "Scan QR code": "QR 코드 스캔",
    "Paste pairing link": "페어링 링크 붙여넣기",
    "Pairing link": "페어링 링크",
    Continue: "계속",
    "Pair Desktop": "컴퓨터 연결",
    "Connect your desktop": "컴퓨터 연결",
    Install: "설치",
    "Install Krill": "Krill 설치",
    "Add Krill to your Home Screen": "홈 화면에 Krill 추가",
    "Open Krill full screen from your home screen.":
      "홈 화면에서 Krill을 전체 화면으로 여세요.",
    "Tap Share, then Add to Home Screen. Open it and scan the desktop QR code if asked.":
      "공유에서 홈 화면에 추가를 누르세요. 설치 앱에서 연결을 요청하면 컴퓨터의 QR 코드를 다시 스캔하세요.",
    "Open your browser menu and choose Install app or Add to Home Screen.":
      "브라우저 메뉴에서 앱 설치 또는 홈 화면에 추가를 선택하세요.",
    "Krill is already installed": "Krill이 이미 설치되어 있습니다",
    Connection: "연결",
    "Connection Log": "연결 기록",
    "Recent connection events": "최근 연결 상태 기록",
    "Copy log": "기록 복사",
    Copy: "복사",
    "Copy answer": "답변 복사",
    Copied: "복사됨",
    "Tap Share in the Safari toolbar.": "Safari 도구 모음에서 공유를 누르세요.",
    "Choose Add to Home Screen, then Add.":
      "홈 화면에 추가를 선택한 뒤 추가를 누르세요.",
    "Open your browser menu (⋮).": "브라우저 메뉴(⋮)를 여세요.",
    "Choose Install app or Add to Home screen.":
      "앱 설치 또는 홈 화면에 추가를 선택하세요.",
    "Within 5 minutes, open Krill from your Home Screen.":
      "5분 안에 홈 화면에서 Krill을 여세요.",
    About: "정보",
    "About Krill": "Krill 정보",
    "Mobile terminal companion": "모바일 터미널 도우미",
    "Text size, input mode and accessory keys": "글자 크기, 입력 방식, 보조 키",
    Display: "화면",
    "Default input mode": "기본 입력 방식",
    "Direct input": "직접 입력",
    "Buffered input": "확인 후 입력",
    "Accessory keys": "보조 키",
    Accounts: "계정",
    "Refresh accounts": "계정 새로 고침",
    Orchestration: "오케스트레이션",
    "Confirmation requests": "확인 요청",
    "{0} request to review": "확인할 요청 {0}건",
    "{0} requests to review": "확인할 요청 {0}건",
    "Review questions and approvals for this desktop":
      "이 데스크톱에서 보낸 질문과 승인 요청을 확인하세요",
    "No requests waiting for you.": "지금 확인할 요청이 없습니다.",
    "Approval requested": "승인 요청",
    "Reply requested": "답변 요청",
    "Your reply": "답변을 입력하세요",
    "Request details": "요청 상세",
    active: "진행 중",
    ready: "준비됨",
    dispatched: "진행 중",
    blocked: "확인 대기",
    failed: "실패",
    "{0} / {1} tasks complete": "{1}개 중 {0}개 완료",
    "Task progress": "작업 진행률",
    "View tasks": "작업 목록 보기",
    "Send reply": "답장 보내기",
    "Reply to worker": "워커에게 답장",
    Approve: "승인",
    Deny: "거부",
    Waiting: "확인 대기",
    Blocked: "진행 불가",
    Working: "작업 중",
    Done: "완료",
    Exited: "종료됨",
    Unknown: "상태 미상",
    "Waiting for you": "확인 대기",
    "No recent update": "최근 상태 없음",
    "Git changes": "Git 변경",
    "Source Control": "소스 관리",
    "Select a changed file to read its diff.":
      "변경된 파일을 선택해 차이를 확인하세요.",
    "No changes": "변경 사항 없음",
    Refresh: "새로 고침",
    Retry: "다시 시도",
    "Hide keyboard": "키보드 닫기",
    "Match Desktop": "PC 화면에 맞추기",
    "Show keyboard": "키보드 열기",
    "Fit to Phone": "휴대폰 화면에 맞추기",
    "Attach photo": "사진 첨부",
    "Choose an image file": "이미지 파일을 선택하세요",
    "Photo attached to selected session": "선택한 세션에 사진을 첨부했습니다",
    "Photo must be smaller than 20 MB": "사진 크기는 20MB 미만이어야 합니다",
    "Photo is too large after resizing": "크기를 줄인 사진도 너무 큽니다",
    Photo: "사진",
    "Remove photo": "사진 제거",
    "Preparing photo…": "사진 준비 중…",
    Send: "보내기",
    "Send command": "명령 보내기",
    "Type a command…": "내용을 입력하세요…",
    "Edit terminal keys": "하단 버튼 편집",
    "Tool activity": "작업 내역",
    "Showing up to 1000 messages. Load earlier messages to read more.":
      "최근 메시지 최대 1000개를 표시합니다. 이전 대화를 불러와 더 볼 수 있습니다.",
    Command: "명령",
    "Send prompt": "프롬프트 보내기",
    "Select text": "텍스트 선택",
    "Latest output": "최신으로",
    "Loading earlier output…": "이전 출력을 불러오는 중…",
    "Beginning of saved output": "저장된 출력의 처음입니다",
    "Scroll up for earlier output": "위로 스크롤하면 이전 출력을 불러옵니다",
    "Load earlier output": "이전 출력 불러오기",
    "Load earlier messages": "이전 대화 불러오기",
    "Terminal actions": "세션 메뉴",
    "Terminal keys": "터미널 키",
    Terminals: "터미널",
    "Create Tab": "탭 만들기",
    Edit: "수정",
    Delete: "삭제",
    "Live input": "실시간 입력",
    "Tap to show keyboard": "탭하여 키보드 열기",
    "Keystrokes send immediately": "입력 즉시 전송됨",
    "Close terminal": "터미널 종료",
    "Close terminal?": "터미널을 종료할까요?",
    "Switch to Terminal": "터미널로 전환",
    "Switch to Chat": "채팅으로 전환",
    "Session not found": "세션을 찾을 수 없습니다",
    "Take photo": "사진 찍기",
    "Paste link instead": "대신 링크 붙여넣기",
    "Close scanner": "스캐너 닫기",
    "No connection events yet.": "아직 연결 기록이 없습니다.",
    "No accounts": "계정이 없습니다",
    "Add accounts or sign in again from Krill desktop Settings.":
      "컴퓨터의 Krill 설정에서 계정을 추가하거나 다시 로그인하세요.",
    "Your coding agents, from anywhere.":
      "어디서든 코딩 에이전트를 확인하세요.",
    "Check on agents, jump into any terminal, and answer prompts from your phone.":
      "휴대폰에서 에이전트를 확인하고, 터미널을 열고, 질문에 답하세요.",
    "Encrypted connection": "암호화된 연결",
    "Traffic to your paired Krill desktop is end-to-end encrypted. Terminal sessions stay on your Mac.":
      "연결한 Krill 컴퓨터와의 통신은 종단간 암호화됩니다. 터미널 세션은 컴퓨터에 유지됩니다.",
    "Resume session": "세션 이어서 열기",
    "Session no longer available.": "더 이상 연결할 수 없는 세션입니다.",
    "5h": "5시간",
    "7d": "7일",
    "Active account": "활성 계정",
    "Check failed": "확인 실패",
    "Could not load Git changes": "Git 변경을 불러오지 못했습니다",
    "Could not load diff": "변경 내용을 불러오지 못했습니다",
    "Could not refresh accounts": "계정을 새로 고치지 못했습니다",
    "Could not send reply": "답장을 보내지 못했습니다",
    "Could not switch account": "계정을 전환하지 못했습니다",
    "Decision sent.": "결정을 전송했습니다.",
    "Diff could not be loaded.": "변경 내용을 불러오지 못했습니다.",
    English: "영어",
    Folders: "폴더",
    From: "보낸 사람",
    General: "일반",
    "Home Screen installation and pairing guidance.":
      "홈 화면 설치와 페어링 안내",
    Installation: "설치",
    "Last updated": "마지막 갱신",
    "Loading Git changes…": "Git 변경을 불러오는 중…",
    "Loading accounts…": "계정을 불러오는 중…",
    "Loading diff…": "변경 내용을 불러오는 중…",
    "Move key down": "키를 아래로 이동",
    "Move key up": "키를 위로 이동",
    "No active orchestration tasks.":
      "진행 중인 오케스트레이션 작업이 없습니다.",
    "No textual diff.": "텍스트 변경이 없습니다.",
    "No workspaces match filters": "필터에 맞는 작업 공간이 없습니다",
    "Not installed": "설치되지 않음",
    "Pair with Krill on your computer to check on your agents, jump into any terminal, and drive work from your phone.":
      "컴퓨터의 Krill과 페어링해 휴대폰에서 에이전트를 확인하고 터미널을 조작하세요.",
    "Rate limited": "요청 제한",
    "Connection temporarily limited. Wait before reconnecting.":
      "일시적으로 연결이 제한됐습니다. 잠시 후 다시 연결하세요.",
    "Reconnect to load Git changes.": "Git 변경을 불러오려면 다시 연결하세요.",
    "Reconnect to load accounts.": "계정을 불러오려면 다시 연결하세요.",
    "Reconnect to reply.": "답장하려면 다시 연결하세요.",
    "Reconnect to switch accounts.": "계정을 전환하려면 다시 연결하세요.",
    "Refresh Git changes to load the current diff.":
      "현재 변경 내용을 보려면 Git 변경을 새로 고치세요.",
    "Refresh to check the latest status.":
      "최신 상태를 확인하려면 새로 고치세요.",
    "Reply sent.": "답장을 전송했습니다.",
    Repository: "저장소",
    Run: "실행",
    "Select a changed file": "변경된 파일을 선택하세요",
    "Selected file": "선택한 파일",
    "Sending…": "전송 중…",
    "Show key": "키 표시",
    "Showing all workspaces.": "모든 작업 공간을 표시합니다.",
    "Sign-in required": "로그인 필요",
    "Some sessions are omitted. Open Krill desktop to see all sessions.":
      "일부 세션이 생략되었습니다. 전체 세션은 컴퓨터의 Krill에서 확인하세요.",
    Stale: "갱신 지연",
    "Switch account": "계정 전환",
    Task: "작업",
    "Terminal text size": "터미널 글자 크기",
    "This Mac": "이 컴퓨터",
    "This request is no longer pending.":
      "이 요청은 더 이상 대기 중이 아닙니다.",
    Updated: "갱신됨",
    "Usage error": "사용량 조회 오류",
    "Usage only": "사용량만 조회",
    "WORKER QUESTION": "워커 질문",
    completed: "완료됨",
    pending: "대기 중",
    "tasks complete": "개 작업 완료",
    "{0} terminal": "터미널 {0}개",
    "{0} terminals": "터미널 {0}개",
    "{0} workspace": "작업 공간 {0}개",
    "{0} workspaces": "작업 공간 {0}개",
    "Resume input": "입력 다시 시작",
    "Waiting for input confirmation…": "입력 적용을 확인하는 중…",
    "Review paste — sending may run commands":
      "붙여넣기를 확인하세요. 전송하면 명령이 실행될 수 있습니다.",
    "Send paste": "붙여넣기 전송",
    Paste: "붙여넣기",
    "Review multiline paste": "여러 줄 붙여넣기 확인",
    "Review in Terminal": "터미널에서 확인",
    "Confirm in Term": "터미널에서 확인",
    "Current diff": "현재 변경",
    "Next retry in {0}s": "{0}초 뒤 다시 시도",
    "Point your camera at the QR code in desktop Settings → Mobile.":
      "컴퓨터의 설정 → 모바일에 표시된 QR 코드를 비추세요.",
    "Waiting for transcript…": "대화 기록을 기다리는 중…",
    "Back to latest": "최신 대화로",
    "Message agent (Bracketed Paste)…": "에이전트에게 메시지 보내기…",
    "Message {0}": "{0}에게 메시지",
    Model: "모델",
    "Default (recommended)": "기본값 (권장)",
    "Sends /model to {0}.": "{0}에 /model 명령을 보냅니다.",
    "Copy is not available in this browser":
      "이 브라우저에서는 복사할 수 없습니다",
    "Chat prompt": "채팅 메시지",
    "Photo preparation failed. Retry or remove it.":
      "사진 준비에 실패했습니다. 다시 시도하거나 제거하세요.",
    "Photo ready. It will be sent with this message.":
      "사진이 준비되었습니다. 메시지와 함께 전송됩니다.",
    "Retry photo": "사진 다시 준비",
    "Photo preparation unavailable": "사진을 준비할 수 없습니다",
    You: "나",
    "My request": "내 요청",
    "{0} reply": "{0}의 답변",
    "Read {0} file(s)": "파일 {0}개 확인",
    "Changed {0} file(s)": "파일 {0}개 수정",
    "Searched {0} time(s)": "{0}번 검색",
    "Ran {0} command(s)": "명령 {0}번 실행",
    "Did {0} other step(s)": "기타 작업 {0}건",
    Agent: "에이전트",
    "Tool call": "도구 호출",
    Permission: "권한 요청",
    Approval: "승인",
    "Historical request. Confirm the current request in Term.":
      "이전 요청입니다. 터미널에서 현재 요청을 확인하세요.",
    Question: "질문",
    "Other (write-in)…": "기타 답변 메모…",
    "Other answer notes": "기타 답변 메모",
    "These are answer notes only. Confirm the current question and enter your answer in Term.":
      "답변 메모입니다. 터미널에서 현재 질문을 확인하고 답을 입력하세요.",
    "This file is outside the current workspace diff.":
      "현재 작업 공간의 변경 범위 밖에 있는 파일입니다.",
    "No current changes for this file. This is a historical tool record; its earlier diff is not stored here.":
      "이 파일에는 현재 변경이 없습니다. 이전 도구 기록의 과거 변경 내용은 여기에 저장되지 않습니다.",
    bound: "대화 연결됨",
    "PTY live": "터미널 실시간",
    never: "기록 없음",
    "Chat history update failed": "대화 기록 갱신 실패",
    "Retry or check Term.": "다시 시도하거나 터미널을 확인하세요.",
    "Transcript changed. Return to latest and retry.":
      "대화 기록이 바뀌었습니다. 최신 대화로 돌아가 다시 시도하세요.",
    Reasoning: "생각 과정",
    "Tool error": "일부 작업 실패",
    "Tool output": "실행 결과",
    "Copy the pairing link in desktop Settings → Mobile and paste it here.":
      "컴퓨터의 설정 → 모바일에서 페어링 링크를 복사해 여기에 붙여넣으세요.",
    "Enter a new message": "새 메시지를 입력하세요",
    "No chat turns recorded yet.": "새 메시지를 입력하세요",
    "Waiting for agent hook or transcript binding…": "새 메시지를 입력하세요",
    "Reading earlier messages; live updates are paused.":
      "이전 대화를 읽는 동안 실시간 갱신이 멈춥니다.",
    "Could not remove desktop. Try again.":
      "저장된 컴퓨터를 삭제하지 못했습니다. 다시 시도하세요.",
    "New Workspace": "새 작업 공간",
    "New workspace": "새 워크트리",
    "Create worktree": "워크트리 만들기",
    Project: "프로젝트",
    "Run on": "실행 위치",
    "Local Mac": "이 Mac",
    "Name or 'Create From'": "이름 또는 기준 원본",
    Optional: "선택",
    "Type a name or search a source": "이름 입력 또는 원본 검색",
    Advanced: "고급",
    "Base branch": "기준 브랜치",
    "New session": "새 세션",
    "Choose desktop": "컴퓨터 선택",
    "Remove saved desktop?": "저장된 컴퓨터를 삭제할까요?",
    "Remove from this phone": "이 휴대폰에서 삭제",
    "Workspace name": "작업 공간 이름",
    "Base branch (optional)": "기준 브랜치 (선택)",
    "Create workspace": "작업 공간 만들기",
    "Creating workspace…": "작업 공간을 만드는 중…",
    "Workspace created": "작업 공간을 만들었습니다",
    "Run setup script?": "설정 스크립트를 실행할까요?",
    "Run setup and create": "설정 실행 후 만들기",
    "Skip setup and create": "설정 건너뛰고 만들기",
    "Review the setup script before running it on this desktop.":
      "이 컴퓨터에서 실행할 설정 스크립트를 확인하세요.",
    "Add a Git repository in Krill desktop first.":
      "먼저 컴퓨터의 Krill에 Git 저장소를 추가하세요.",
    "Reconnect to a desktop that supports workspace creation.":
      "작업 공간 생성을 지원하는 컴퓨터에 다시 연결하세요.",
    "Reading photo…": "사진을 읽는 중…",
    "No QR code found. Take the photo closer, with the whole code in view.":
      "QR 코드를 찾지 못했습니다. 코드 전체가 보이도록 더 가까이 촬영하세요.",
    "Take a photo of the QR code in desktop Settings → Mobile.":
      "컴퓨터의 설정 → 모바일에 표시된 QR 코드를 촬영하세요.",
    "Camera is unavailable. Take a photo of the QR code instead.":
      "카메라를 사용할 수 없습니다. QR 코드 사진을 선택하세요.",
    "Terminal #{0} will be stopped.": "터미널 #{0}이 종료됩니다.",
    "Write a prompt…": "프롬프트를 입력하세요…",
    Prompt: "프롬프트",
    "Touch and hold to select and copy.":
      "길게 눌러 텍스트를 선택하고 복사하세요.",
    "Latest terminal output": "최근 터미널 출력",
    "Choose photo": "사진 선택",
    "Choose image file": "이미지 파일 선택",
    "Terminal direct input": "터미널 직접 입력",
    "Terminal text": "터미널 텍스트",
    "No decision options provided. Review this request on desktop.":
      "선택지가 없습니다. 컴퓨터에서 이 요청을 확인하세요.",
    "Hand off to {0}": "{0}(으)로 이어하기",
    "Some changed files are omitted. Open Krill desktop to see all changes.":
      "일부 변경 파일이 생략되었습니다. 전체 변경은 데스크톱의 Krill에서 확인하세요.",
    // Pairing, connection and terminal messages also pass through t() at runtime.
    "Add this desktop to your saved desktops. You can switch between them from Home.":
      "이 데스크톱을 저장된 데스크톱에 추가합니다. 홈에서 전환할 수 있습니다.",
    "Add to Home Screen": "홈 화면에 추가",
    "Hand off to Claude": "Claude로 이어하기",
    "Hand off to Codex": "Codex로 이어하기",
    "Handed off to {0}.": "{0}(으)로 이어했습니다.",
    "Loading…": "불러오는 중…",
    Pair: "페어링",
    "The Home Screen app connects as its own device. You can remove it from desktop Settings → Mobile.":
      "홈 화면 앱은 별도 기기로 연결됩니다. 데스크톱 설정 → 모바일에서 제거할 수 있습니다.",
    "Tap Share in Safari, then choose Add to Home Screen.":
      "Safari에서 공유를 탭한 다음 홈 화면에 추가를 선택하세요.",
    "Encrypted · connected": "암호화됨 · 연결됨",
    "Invalid session link": "잘못된 세션 링크입니다",
    "Invalid workspace link": "잘못된 작업 공간 링크입니다",
    "Not a Krill pairing code. Scan the QR code in desktop Settings → Mobile.":
      "Krill 페어링 코드가 아닙니다. 데스크톱 설정 → 모바일의 QR 코드를 스캔하세요.",
    "Not a Krill pairing code. Scan the QR code shown in Krill on your Mac.":
      "Krill 페어링 코드가 아닙니다. Mac의 Krill에 표시된 QR 코드를 스캔하세요.",
    "Could not decode image file": "이미지 파일을 디코딩할 수 없습니다",
    "The source image cannot be decoded.":
      "원본 이미지를 디코딩할 수 없습니다.",
    "Terminal #{0}": "터미널 #{0}",
    "Not a Git repository": "Git 저장소가 아닙니다",
    "Showing first 1,000 changed files.":
      "변경된 파일 중 처음 1,000개만 표시합니다.",
    "Could not save local settings": "이 기기에 설정을 저장하지 못했습니다",
    "Could not load the QR scanner. Check the connection and try again.":
      "QR 스캐너를 불러오지 못했습니다. 연결을 확인하고 다시 시도하세요.",
    "Photo pixel dimensions are too large":
      "사진의 가로·세로 픽셀 크기가 너무 큽니다",
    "Photo target changed": "사진을 첨부할 대상이 바뀌었습니다",
    "Could not prepare the Home Screen app": "홈 화면 앱을 준비하지 못했습니다",
    "Screen target changed": "표시할 화면의 대상이 바뀌었습니다",
    "Could not load this screen. Check the connection and try again.":
      "화면을 불러오지 못했습니다. 연결을 확인하고 다시 시도하세요.",
    "Reconnect to desktop first": "먼저 데스크톱에 다시 연결하세요",
    "Unexpected daemon response":
      "데스크톱에서 예상하지 못한 응답을 받았습니다",
    "Could not save desktop details": "데스크톱 정보를 저장하지 못했습니다",
    "Could not save pairing. Keep this page open.":
      "페어링 정보를 저장하지 못했습니다. 이 페이지를 열어 두세요.",
    "Desktop is no longer saved.": "더 이상 저장되어 있지 않은 데스크톱입니다.",
    "Invalid update state": "업데이트 상태가 올바르지 않습니다",
    "Truncated response": "응답 데이터가 불완전합니다",
    "Invalid response": "응답 데이터가 올바르지 않습니다",
    "Desktop address is unavailable.": "데스크톱 주소를 사용할 수 없습니다.",
    "Desktop identity changed.": "데스크톱 식별 정보가 바뀌었습니다.",
    "Scan a fresh QR code from desktop Settings → Mobile.":
      "데스크톱 설정 → 모바일에서 새 QR 코드를 스캔하세요.",
    "Update destination not saved":
      "업데이트할 데스크톱 정보가 저장되지 않았습니다",
    "Unsupported terminal protocol": "지원하지 않는 터미널 통신 방식입니다",
    "Disconnected. Pending actions were not resent.":
      "연결이 끊겼습니다. 대기 중이던 동작은 다시 보내지 않았습니다.",
    "Update the desktop for mobile workflows":
      "모바일 기능을 사용하려면 데스크톱 앱을 업데이트하세요",
    "Disconnected. The action was not resent.":
      "연결이 끊겼습니다. 해당 동작은 다시 보내지 않았습니다.",
    "Only reads can be coalesced": "조회 요청만 묶어서 처리할 수 있습니다",
    "Unsupported session ID. Update the desktop.":
      "지원하지 않는 세션 ID입니다. 데스크톱 앱을 업데이트하세요.",
    "Could not connect to desktop": "데스크톱에 연결하지 못했습니다",
    "Unsupported desktop protocol": "지원하지 않는 데스크톱 통신 방식입니다",
    "Reconnecting… Pending actions were not resent.":
      "다시 연결하는 중… 대기 중이던 동작은 다시 보내지 않았습니다.",
    "Truncated screen": "터미널 화면 데이터가 불완전합니다",
    "Invalid screen integer": "터미널 화면의 숫자 데이터가 올바르지 않습니다",
    "Screen limit": "터미널 화면 데이터가 허용 범위를 초과했습니다",
    "Truncated screen text": "터미널 화면의 텍스트 데이터가 불완전합니다",
    "Trailing screen data":
      "터미널 화면에 예상하지 못한 추가 데이터가 있습니다",
    "Invalid boolean": "참·거짓 값이 올바르지 않습니다",
    "Invalid size": "터미널 화면 크기가 올바르지 않습니다",
    "Invalid scroll": "터미널 스크롤 데이터가 올바르지 않습니다",
    "Invalid dirty row": "터미널의 변경된 행 데이터가 올바르지 않습니다",
    "Invalid cursor": "터미널 커서 위치가 올바르지 않습니다",
    "Invalid terminal mount": "터미널 화면을 구성할 수 없습니다",
    "Input target changed": "입력할 터미널이 바뀌었습니다",
    "Unknown terminal key": "알 수 없는 터미널 키입니다",
    "Invalid terminal connection factory": "터미널 연결을 준비할 수 없습니다",
    "Unexpected terminal handshake":
      "터미널 연결 중 예상하지 못한 응답을 받았습니다",
    "Invalid screen frame": "터미널 화면 데이터 형식이 올바르지 않습니다",
    "Terminal input rejected. Resume input, then re-enter rejected text; it will not be resent.":
      "터미널 입력이 거부되었습니다. 입력을 다시 시작한 뒤 거부된 내용을 다시 입력하세요. 자동으로 다시 보내지 않습니다.",
    "Terminal resize rejected": "터미널 크기 변경이 거부되었습니다",
    "Terminal role rejected": "터미널 제어 권한 요청이 거부되었습니다",
    "Terminal disconnected": "터미널 연결이 끊겼습니다",
    "Wait for the live terminal to connect":
      "실시간 터미널이 연결될 때까지 기다리세요",
    "Resume terminal input before sending":
      "전송하기 전에 터미널 입력을 다시 시작하세요",
    "Input is too large (maximum 64 KiB)":
      "입력 크기가 너무 큽니다(최대 64 KiB)",
    "Input delivery is uncertain; input will not be resent.":
      "입력 전달 여부를 확인할 수 없습니다. 입력은 다시 보내지 않습니다.",
    "This terminal is no longer available.":
      "더 이상 사용할 수 없는 터미널입니다.",
    "This direct code cannot be opened here. Choose Relay server in desktop Settings → Mobile, then tap New QR code.":
      "이 직접 연결 코드는 여기서 열 수 없습니다. 데스크톱 설정 → 모바일에서 릴레이 서버를 선택한 다음 새 QR 코드를 누르세요.",
    "Update too large": "업데이트 크기가 너무 큽니다",
    "Unsupported update handoff": "지원하지 않는 업데이트 전달 방식입니다",
    "Invalid update inventory": "업데이트 파일 목록이 올바르지 않습니다",
    "Update cancelled": "업데이트가 취소되었습니다",
    "Update hash failed": "업데이트 파일의 무결성을 확인하지 못했습니다",
    "Update cache unavailable": "업데이트 임시 저장소를 사용할 수 없습니다",
    "Incomplete update": "업데이트 파일이 불완전합니다",
    "Update asset too large": "업데이트 파일 크기가 너무 큽니다",
    "Update hash mismatch": "업데이트 파일의 무결성 검증에 실패했습니다",
    "Update state not saved": "업데이트 상태가 저장되지 않았습니다",
    "Invalid key": "암호화 키가 올바르지 않습니다",
    "Frame limit": "통신 데이터가 허용 범위를 초과했습니다",
    "Frame size": "통신 데이터 크기가 올바르지 않습니다",
    "Replay rejected": "중복 수신된 통신 데이터가 거부되었습니다",
    "Authentication failed": "인증에 실패했습니다",
  };
  function t(text, ...values) {
    let out = language === "ko" ? ko[text] || text : text;
    values.forEach((v, i) => {
      out = out.replaceAll(`{${i}}`, String(v));
    });
    return out;
  }
  function configure(value) {
    language =
      value === "ko" || value === "en"
        ? value
        : /^ko\b/i.test(globalThis.navigator?.language || "")
          ? "ko"
          : "en";
    if (globalThis.document?.documentElement)
      document.documentElement.lang = language;
    return language;
  }
  M.i18n = {
    t,
    configure,
    get language() {
      return language;
    },
    dictionary: ko,
  };
  configure();
})();
