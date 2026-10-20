# 모바일 UI/UX 수정 및 검증 — 2026-10-20

대상은 HEAD `e0a8a4e53a3c0265c4fb8345b54622a8e3bcd370`에 기존 미커밋 작업과 이번 수정이 함께 있는 작업 트리다. 기존 변경을 보존했으며, 전체 Git diff를 이번 작업의 결과라고 간주하지 않는다. [원래 조사](../boost-2026-10-20-mobile-ux-audit.md)의 확인 항목 42건과 후보·정책 7건을 아래처럼 처리했다. LAN 관련 두 항목은 사용자의 **LAN 접속 제거** 지시에 따라 대체했다.

## 구현 결과

| 조사 항목 | 최종 동작 | 주요 검증 |
|---|---|---|
| TR-A01 | 과거 기록의 질문·권한 카드에서 현재 PTY에 답을 보내지 않는다. 현재 질문을 원자적으로 검증할 계약이 없어 기존 `approve`/`answer_question` RPC는 입력 전에 거절하고, 카드에서는 터미널로 이동해 확인한다. | 실제 데몬 RPC 거절·미입력 검사, Chat/Session 회귀 |
| TR-A02 | 초안 버전과 제출 잠금을 공유한다. 이전 전송 성공은 정확히 해당 버전만 비우며, 화면 재진입 후에도 미전송 편집을 보존하고 완료한 메시지를 중복 전송하지 않는다. | 지연·실패·remount·동일 문장으로 재편집 회귀, 브라우저에서 데몬 지연 중 새 초안 보존 |
| TR-A03/04/05 | terminal mode를 협상해 bracketed paste를 사용한다. 사용할 수 없는 여러 줄 붙여넣기는 실제 textarea에서 확인·수정·취소한다. 거절된 direct 입력 문맥을 무효화하고 명시적으로 재개한다. ACK 지연과 10초 불확실성 종료를 표시하며 자동 재전송하지 않는다. | Terminal 실패 경로·IME·모드·ACK 회귀, 브라우저 여러 줄 확인/취소 |
| TR-A06/07/08 | 기록 카드 DOM·로컬 답변 메모·펼침을 유지하고, 빈 reset은 즉시 반영한다. 기록 조회 오류에는 마지막 갱신·재시도·터미널 대안을 표시한다. | Chat 회귀 |
| TR-A10 | Chat으로 숨긴 터미널은 입력/Fit 크기 소유를 반환한다. 소켓은 유지하고 터미널 복귀 시 가시성과 Fit을 복원한다. | Terminal/Session 회귀, 실제 PTY 크기 계약 통합 검사 |
| TR-A11 | 사진은 초안에 준비·실패·재시도·제거 상태로 보관한다. 준비된 사진과 텍스트를 한 prompt 요청으로 보내며, 준비 자체가 PTY에 입력하지 않는다. | 준비/취소/실패/순서 회귀 및 Rust 단일 요청 검사 |
| TR-A12 | 현재 diff의 변경 없음·범위 밖을 구별해 설명한다. 과거 도구 기록이 과거 diff 보관을 뜻한다고 안내하지 않는다. | Chat/Git 회귀, 실제 데몬 범위 밖 결과 검사 |
| TR-A13/15/16 | 비활성 탭 메뉴는 해당 세션을 대상으로 동작한다. 보조 키와 물리 키 인코더를 공유하고 이미지 선택기에 `image/*`를 적용한다. | Session/Terminal 회귀 |
| TR-A14 | 250개 표시 상한과 이전 기록 탐색을 제공한다. 관측한 페이지의 시작·끝 바이트를 재생해 짧은 tail 페이지도 중복·역전 없이 읽는다. 이전 기록 조회 중 live poll을 막고 최신 보기로 복귀할 수 있다. | 300회 단일 append·50회 이전 탐색, 600개 페이지, UTF-8·동일 문장·부분 레코드·live 경합 회귀 |
| SC-A01/02/03 | 답장 입력 DOM·선택·IME를 유지하면서 주변 실행·질문을 갱신한다. run/task/from 맥락, pending/offline/이미 해결됨/실패/결과를 요청별로 표시한다. | Screens 회귀 |
| SC-A04/05 | 목록 노드를 key로 갱신해 위치를 유지한다. 앱 Back은 실제 방문 이력을 사용하고 새 이력을 쌓는 루프를 만들지 않는다. | Screens 회귀, 브라우저 Home→Accounts→Back→브라우저 Back |
| SC-A06/07/A08 | 검색을 닫거나 Escape를 누르면 쿼리를 비운다. Git은 loading/error/retry/refresh와 선택 파일을 표시하고 옛 응답을 버린다. | Screens 회귀, 브라우저 숨긴 검색 해제·Git 파일 선택 |
| SC-A09/10 | native dialog에 제목·설명을 연결한다. 실제 라우트 전환 때 문서 제목과 h1 focus를 갱신한다. | DOM 검사, 브라우저 h1 focus·title 확인 |
| SC-A11/12 | 보조 키 이름을 checkbox label로 연결한다. 계정의 로그인·오류·reset 설명을 노출하고 접근성 설명과 연결한다. | Screens 회귀, 브라우저 label 탭·320px 한글 계정 설명 확인 |
| SC-A13/14/15 | 감속 Working 링은 닫힌 정적 링이다. 상태 우선순위는 Waiting→Blocked→Working→Done→Exited→Unknown이며, 일부 세션만 받은 화면에는 생략 안내를 표시한다. | 소스/화면 회귀 |
| CO-A02/05 | **LAN 선택 UI·일반 CLI LAN 페어링·사설 LAN 신규 연결을 제거했다.** 저장된 LAN 의도는 Relay 경로로 옮기며 기기 인증은 유지한다. 직렬화 discriminant 0은 호환용 tombstone이다. Tailscale은 별도 경로이며 명시적 loopback은 격리 QA용으로 남긴다. | CLI, 주소 검증, 저장 설정 마이그레이션·기기 인증 보존 검사 |
| CO-A03/11 | 연결 전체 20초, RPC 15초 deadline과 재연결/QR 선택·재시도 대기 정보를 제공한다. 인증 차단·cooldown을 구별하며, 이전 세대의 lookup 응답은 새 소켓·경로·저장·이동을 바꾸지 못한다. | 전체 handshake 단계·RPC·세대 경합 회귀 및 독립 반증 |
| CO-A04 | 새 페어링 취소는 저장한 endpoint·credential·버전을 복원하고 원래 연결을 재개한다. | Transport/fixed-origin 회귀 |
| CO-A06/07/08 | PC에는 접근권한 해제의 영구 효과·기기 식별·재스캔 안내를 표시한다. QR만 취소하는 요청을 추가해 이미 연결한 기기를 보존한다. 기기 상한은 별도 오류/복구 안내다. | QR 0기기/인증 연결 보존·32기기 상한·close code 검사 |
| CO-A09/10/12 | direct offline shell을 캐시하되 인증 query는 캐시하지 않는다. 현재 Relay 명칭을 쓰고 연결 경로 변경의 즉시 효과를 설명한다. | direct/cloud SW 및 연결 회귀 |
| CO-A13/14 | 저장 PC 한 대를 명시하고 설정에서 설치 안내를 다시 열 수 있다. | Screens/설치 흐름 회귀 |

## 후보·정책 처리와 검증 한계

| 후보 | 적용 내용과 남은 실기 범위 |
|---|---|
| CO-A01 | 설치 후 이미 페어링된다는 미검증 약속을 제거했다. 인증 query는 network/no-store로 취급하고 필요하면 QR을 다시 스캔하도록 안내한다. 실제 iOS 설치의 빈 저장소 인계는 미검증이다. |
| TR-A09 | Chat에 공통 visualViewport 키보드 inset을 적용하고 확대 동작을 구별한다. 실제 iOS/Android 키보드·회전·PWA 측정은 남는다. |
| SC-Q01 | 시스템/한국어/영어 선택과 `lang`을 제공한다. 고정 UI 문구만 번역하며 파일·질문 선택지 등 사용자 문자열을 번역하지 않는다. 한국어 환경의 실제 파일명 `Cancel` 원문 보존을 브라우저에서 확인했다. 서버/도구의 원문 오류는 그대로 표시될 수 있다. |
| SC-Q02 | 시스템/라이트/다크, UI 100/125/150%와 기존 터미널 글자 설정을 제공한다. 320×568, 390×844, 844×390 및 UI 200% 강제 적용을 점검했다. OS 전체 확대나 실제 스마트폰 검증으로 해석하지 않는다. |
| SC-Q03 | [모바일 토큰 대응](mobile-ui-tokens.md)에 공통 토큰·모바일 예외를 기록했다. |
| SC-Q04 | 주요 표시 버튼·보조 키·설정 조작부를 최소 44px로 확대했다. Chromium rect를 확인했으며 실제 한 손 오탭·보조공학 사용성은 측정하지 않았다. IME용 숨긴 capture는 표시 터치 대상이 아니다. |
| SC-Q05 | 긴 계정·질문·설정 텍스트를 줄바꿈한다. 확대된 한글 사용량 라벨은 한 줄로 유지하면서 좁은 화면에서는 사용량 열이 내려간다. 실기 스크린리더 발화 품질은 남는다. |

## 추가로 발견해 닫은 회귀

독립 검토에서 제출 후 remount 중복 전송, 짧은 기록 페이지의 중복·역전, 오래된 rendezvous lookup의 새 연결 종료를 재현해 수정했다. 추가로 이전 기록 조회와 live poll 경합, 서로 다른 동일 문장 레코드의 과도한 병합, Codex 이중 표현 쌍의 연쇄 병합을 수정하고 회귀를 남겼다. 최종 독립 반증 6그룹은 모두 통과했다. [독립 검토](mobile-ux-fix-evidence-2026-10-20/independent-review.md), [재현 스크립트](mobile-ux-fix-evidence-2026-10-20/independent-repro.cjs).

브라우저 QA에서 발견한 라이트 모드 작은 primary 버튼 대비와 Chat/Term 전환 후 접근성 이름 불일치도 수정했다. 설정과 최종 Chat의 axe WCAG A/AA 자동 검사 위반은 0건이다. 계정 검사는 위반 0건, pseudo-element 배경 때문에 대비 판정 보류 1건이었다. 자동 검사를 VoiceOver/TalkBack 검증으로 간주하지 않는다.

## 검증 기록

Cargo 산출물은 `/Volumes/krill-build/targets/shared`, Windows 검사는 `/Volumes/krill-build/targets/windows-gnu`를 사용했다. 관련 크레이트는 `krill-proto`, `krilld`, `krill`, `krill-app`이며 세션 생존 검사는 `krill-holder`다. 전체 릴리스 `scripts/check.sh`는 실행하지 않았다.

- `cargo fmt --all --check`, `git diff --check`: 통과.
- 영향받은 네 크레이트 `cargo clippy --all-targets --locked -- -D warnings`: 통과.
- `scripts/check-windows.sh -p krill-proto -p krilld -p krill -p krill-app --clippy`: 통과. 기존 작업의 플랫폼 lint 방해 항목에 한정해 Unix 전용 test helper cfg, import, 동일 계약의 반환형 주석, 문서 backtick과 Option 표현을 정리했다. 하네스는 수정하지 않았다.
- 모바일·기록·PWA [nextest 103개](mobile-ux-fix-evidence-2026-10-20/nextest-final.log): **103개 모두 통과**. 자산/기록/PWA [집중 27개](mobile-ux-fix-evidence-2026-10-20/nextest-assets-final.log)도 모두 통과했다. 중간 hash 실패는 검사 도중 자산 변경에 의한 것으로, 최종 자산에서 재실행했다.
- transport/cloud [Node 39개](mobile-ux-fix-evidence-2026-10-20/cloud-node-final.log): 통과. Chat/Screens/Session/Terminal 및 독립 반증: 통과.
- `MKH_KILLS=5 MKH_UPGRADES=3`의 `krilld_kill`, `exec_handoff`, `exec_handoff_ack_precedes_log_io`: 3개 통과. 실제 사용자 데몬을 종료하지 않고 격리 holder fixture를 사용했다.
- 전체 원본/`.gz` 내용 일치, HTTP gzip 응답·현재 PWA 자산 해시 검사: 통과. [소스 SHA-256](mobile-ux-fix-evidence-2026-10-20/source-sha256.json)이 dirty 작업 트리의 검증 대상을 식별한다.

브라우저는 이름이 분리된 Chromium과 loopback 테스트 데몬/임시 저장소만 사용했다. 이전 조사 증거를 덮어쓰지 않고 [수정 후 증거](mobile-ux-fix-evidence-2026-10-20/)에 별도 저장했다. 운영 기기 인증·사용자 PTY·실제 계정은 조작하지 않았다. 커밋·push·배포·유료 작업은 하지 않았다.

## 병렬 작업 결과

Orca Run `run_70eb993a67c0`. 아래 Task는 모두 `succeeded`로 정산되었다. 최초 검토 Task는 발견 항목을 수정 Task로 인계하고 같은 terminal을 최종 검토에 재사용했다. 최종 worker terminal은 모두 해제되었고 미정산 reclaimable은 없다.

| Task | 결과·근거 |
|---|---|
| `task_62a9fe222e8a` | Terminal 입력·모드·크기·키 및 Session 수정, Terminal/Session 회귀 통과 |
| `task_c0a71f699393` | Chat·historical RPC·사진·diff·페이지 구현, Chat/Rust 회귀에 통합 |
| `task_4d9b07276299` | Home/Workspace/Git/Accounts/Orchestration/Settings 수정, Screens 회귀 통과 |
| `task_00f517a6f926` | LAN 제거·연결·QR/기기·SW·PC/CLI 수정, 관련 Rust/Node 통과 |
| `task_0d8318912233` | 독립 검토 및 P1 3건 재현, 수정에 인계 |
| `task_a40db2f669f0` | Chat remount·페이지 P1 수정, [수정 보고서](mobile-ux-fix-evidence-2026-10-20/chat-review-fixes.md) |
| `task_13f1e948578e` | stale lookup P1 수정, [수정 보고서](mobile-ux-fix-evidence-2026-10-20/connect-review-fixes.md) |
| `task_e5532a99dfdf` | 독립 반증 확대·최종 6그룹 통과, 추가 경합 검증 |

코드·로컬 통합 검증에는 재현된 잔여 blocker가 없다. 실제 iPhone/Android의 설치·키보드·스크린리더·실제 Relay 배포 네트워크는 이번 로컬 검증 범위 밖이며 실기 완료로 보고하지 않는다.
