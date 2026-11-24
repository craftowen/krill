// Adapted from Orca (Copyright (c) 2026 Lovecast Inc., MIT License).
//! Orca's "Bot templates" gallery: search and category chips over a grouped list on the left,
//! the picked template's details on the right, "Use template…" opens New bot pre-filled.

use gpui::{
    App, Context, Entity, EventEmitter, FocusHandle, FontWeight, KeyDownEvent, ScrollHandle,
    SharedString, Subscription, Window, div, prelude::*, px,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::{Icon, IconName, Sizable as _};
use krill_proto::bots::{
    BUILTIN_BOT_TEMPLATES, BotTemplate, BotTemplateCategory, BotTemplateEvent, BotTemplateSchedule,
    find_bot_skill,
};

use super::avatar;
use crate::i18n::{Language, catalog::bots as text};
use crate::ui;

/// Orca `w-[min(1080px,100vw-2rem)] h-[min(780px,100vh-2rem)]`.
const WIDTH: f32 = 1080.;
const HEIGHT: f32 = 780.;
/// Below this width the details replace the list (Orca's container query).
const SPLIT_MIN_WIDTH: f32 = 896.;

#[derive(Debug, Clone)]
pub enum Event {
    Use(&'static BotTemplate),
    /// "Blank bot": an empty New bot, with the search text as its job when there is one.
    Blank(Option<String>),
    Close,
}

/// Orca `#47241`: Korean translations for all 24 built-in bot templates (commit `510b0f63`).
struct TemplateKo {
    name: &'static str,
    description: &'static str,
    first_chat: &'static str,
    watches: &'static str,
    does: &'static str,
    reports: &'static str,
    needs_setup: Option<&'static str>,
    examples: [&'static str; 2],
}

#[expect(
    clippy::too_many_lines,
    reason = "static Korean localization table for all 24 built-in bot templates"
)]
fn template_ko(id: &str) -> Option<TemplateKo> {
    Some(match id {
        "pr-review" => TemplateKo {
            name: "PR 리뷰",
            description: "리뷰를 요청받은 PR을 살펴보고 위험한 변경을 정리합니다.",
            first_chat: "어떤 저장소를 볼지 물어본 뒤, 내 리뷰를 기다리는 열린 PR을 리뷰하고 결과를 파일과 줄 번호와 함께 이 채팅에 올립니다. 허락하기 전에는 GitHub에 댓글을 달지 않습니다.",
            watches: "선택한 저장소에서 내 리뷰를 기다리는 PR",
            does: "PR을 하나씩 검토해 위험한 변경과 빠진 테스트를 파일과 줄 번호와 함께 정리합니다.",
            reports: "이 채팅에서, 리뷰가 요청될 때",
            needs_setup: None,
            examples: [
                "#4121: 재시도 카운터가 초기화되지 않아 두 번째 장애부터는 재시도가 멈춥니다.",
                "새 타임아웃 경로를 다루는 테스트가 없습니다.",
            ],
        },
        "bug-investigation" => TemplateKo {
            name: "버그 조사",
            description: "제보된 버그를 재현하고 원인 위치를 찾습니다.",
            first_chat: "버그 리포트나 이슈 링크를 건네 주세요. 새 워크스페이스에서 에이전트를 시작해 재현 방법을 기록한 뒤 원인을 보고합니다. 수정은 동의한 뒤에만 작성합니다.",
            watches: "건네 주신 버그 리포트와 내게 할당된 Linear 이슈",
            does: "새 워크스페이스에서 버그를 재현하고, 시작 지점을 찾아 수정안을 제안합니다.",
            reports: "이 채팅에서, 재현과 원인을 찾았을 때",
            needs_setup: None,
            examples: [
                "Windows에서 KRL-640을 재현했습니다. 줄바꿈 없이 끝나는 텍스트를 붙여넣으면 마지막 줄이 사라집니다.",
                "원인: 붙여넣기 종료 마커가 마지막 청크보다 먼저 전송됩니다. 수정을 시작할까요?",
            ],
        },
        "dependency-updates" => TemplateKo {
            name: "의존성 업데이트",
            description: "오래되었거나 취약한 패키지를 확인하고 업그레이드를 제안합니다.",
            first_chat: "선택한 저장소의 오래되었거나 취약한 패키지를 긴급한 순서대로 정리합니다. 동의하면 에이전트가 그룹별로 PR을 하나씩 엽니다.",
            watches: "선택한 저장소의 패키지",
            does: "보안 권고와 오래된 패키지를 긴급한 순서로 정리하고, 동의하면 그룹별로 PR 하나씩 업그레이드합니다.",
            reports: "이 채팅에서, 매주 월요일 오전 9시",
            needs_setup: None,
            examples: [
                "권고 1건: http-client 6.19에 심각도 높음 취약점이 있으며 6.21에서 수정되었습니다.",
                "마이너 업그레이드 4건은 PR 하나로 묶어 올릴 수 있습니다.",
            ],
        },
        "unreviewed-work" => TemplateKo {
            name: "검토 안 된 에이전트 작업",
            description: "아직 아무도 보지 않은 에이전트 작업 결과를 매일 정리한 목록입니다.",
            first_chat: "에이전트가 변경을 마쳤지만 검토하지 않았거나 PR로 만들지 않은 워크스페이스를 정리합니다. 에이전트가 내 응답을 기다릴 때는 Krill이 이미 알려 주므로, 이 봇은 이미 끝난 작업을 다룹니다.",
            watches: "내 Krill 워크스페이스",
            does: "에이전트가 변경을 마쳤지만 아무도 검토하지 않았거나 PR을 열지 않은 워크스페이스를 정리합니다.",
            reports: "이 채팅에서, 평일 오전 9시",
            needs_setup: None,
            examples: [
                "mobile-pairing이 어제 340줄 변경으로 끝났고 PR이 없습니다.",
                "이번 주에 2건이 더 완료되었습니다.",
            ],
        },
        "workspace-cleanup" => TemplateKo {
            name: "워크스페이스 정리",
            description: "완료되었거나 방치된 워크스페이스를 찾아 삭제를 제안합니다.",
            first_chat: "PR이 병합되었거나 2주 동안 변경이 없는 워크스페이스를 커밋되지 않은 변경과 함께 정리합니다. 선택한 항목만 삭제합니다.",
            watches: "내 Krill 워크스페이스와 PR",
            does: "완료되었거나 방치된 워크스페이스를 커밋되지 않은 변경과 함께 정리하고, 선택한 항목만 삭제합니다.",
            reports: "이 채팅에서, 매주 금요일 오후 4시",
            needs_setup: None,
            examples: [
                "워크스페이스 6개가 완료되었습니다. 이번 주에 PR이 병합되었습니다.",
                "폴더 2개가 10월 20일 이후 변경이 없습니다.",
            ],
        },
        "security-check" => TemplateKo {
            name: "보안 점검",
            description: "병합된 변경에서 유출된 시크릿과 위험한 코드를 찾습니다.",
            first_chat: "선택한 저장소에서 지난주에 병합된 PR을 검토하고, 다시 살펴볼 항목을 파일과 줄 번호와 함께 정리합니다.",
            watches: "선택한 저장소에서 병합된 PR",
            does: "시크릿 유출, 인젝션, 인증 누락, 안전하지 않은 기본값을 점검합니다.",
            reports: "이 채팅에서, 매주 월요일 오전 9시와 PR이 병합될 때",
            needs_setup: None,
            examples: [
                "#4133이 인증 토큰을 포함한 요청 헤더 전체를 로그로 남깁니다.",
                "이번 주에는 그 외 항목이 없습니다.",
            ],
        },
        "release" => TemplateKo {
            name: "릴리스 준비 상태",
            description: "다음 릴리스를 막는 요소를 추적합니다.",
            first_chat: "실패한 체크, 리뷰 대기 중인 PR, 열린 블로커를 담은 짧은 준비 상태 목록을 유지합니다.",
            watches: "PR, 체크, 워크스페이스",
            does: "실패한 체크, 리뷰 대기 중인 PR, 열린 블로커를 담은 짧은 준비 상태 목록을 유지합니다.",
            reports: "이 채팅에서, 평일 아침",
            needs_setup: None,
            examples: [
                "1.5는 체크 1건 실패로 막혀 있습니다. #4121의 typecheck입니다.",
                "아직 리뷰가 필요한 PR이 2건 있습니다: #4108, #4130.",
            ],
        },
        "release-notes" => TemplateKo {
            name: "릴리스 노트",
            description: "병합된 PR로 다음 릴리스 노트 초안을 작성합니다.",
            first_chat: "마지막 릴리스 태그를 찾아 신규, 수정, 변경 세 그룹으로 노트 초안을 작성합니다. 편집은 이 채팅에서 하면 됩니다.",
            watches: "마지막 릴리스 이후 병합된 PR",
            does: "사용자용 릴리스 노트 초안을 신규, 수정, 변경 세 그룹으로 작성합니다.",
            reports: "이 채팅에서, 요청할 때",
            needs_setup: None,
            examples: [
                "1.5 초안: 신규 4건, 수정 9건, 변경 2건.",
                "가장 큰 변경: 봇끼리 스킬을 공유할 수 있습니다.",
            ],
        },
        "ci-failures" => TemplateKo {
            name: "CI 실패",
            description: "실패한 체크를 설명하고 불안정한 체크 목록을 관리합니다.",
            first_chat: "최근 2주간의 체크 실행 기록을 읽어 불안정한 체크를 정리한 뒤, 내 열린 PR에서 실패한 체크를 하나씩 설명합니다. 수정은 동의한 뒤에만 시작합니다.",
            watches: "선택한 저장소의 체크 실행 기록",
            does: "불안정한 체크와 진짜 실패를 구분하고, 실패마다 원인을 설명하며, 불안정한 체크 목록을 관리합니다.",
            reports: "이 채팅에서, 매주 월요일 오전 9시와 체크가 실패할 때",
            needs_setup: Some(
                "GitHub는 아직 브랜치별로 실패를 필터링할 수 없어, 봇이 저장소의 모든 실패를 보고 중요하지 않은 것은 건너뜁니다. 실패마다 턴이 한 번씩 사용됩니다.",
            ),
            examples: [
                "#4130 typecheck: export 이름이 바뀌었는데 호출 위치 2곳이 빠졌습니다. 수정은 커밋 1개입니다.",
                "이번 주 가장 불안정한 항목: 터미널 크기 조절 테스트, 30회 중 4회 실패.",
            ],
        },
        "deploy-watch" => TemplateKo {
            name: "배포 모니터링",
            description: "배포마다 바뀐 내용을 올리고 실패한 배포를 알려 줍니다.",
            first_chat: "배포 도구가 호출할 자동화 주소를 알려 주고, 배포된 PR을 바탕으로 배포마다 요약합니다.",
            watches: "배포 도구의 호출",
            does: "배포된 PR을 바탕으로 배포마다 요약하고 실패한 배포를 알려 줍니다.",
            reports: "이 채팅에서, 배포가 끝날 때마다",
            needs_setup: Some(
                "배포 도구 커넥터가 없어, 자동화 주소를 배포 도구에 직접 추가해야 합니다.",
            ),
            examples: [
                "운영 배포 14:02: PR 5건, 릴레이 재시도 수정 포함.",
                "스테이징 배포가 migrate 단계에서 실패했습니다: 컬럼이 이미 존재합니다.",
            ],
        },
        "support" => TemplateKo {
            name: "지원 분류",
            description: "아무도 답하지 않은 질문 목록을 관리합니다.",
            first_chat: "선택한 Slack 채널에서 답변이 없는 질문을 링크와 함께 정리하고, 이미 이슈가 있는 질문을 표시합니다.",
            watches: "선택한 Slack 채널",
            does: "답변이 없는 질문을 링크와 함께 정리하고, 이미 Linear 이슈가 있는 질문을 표시합니다.",
            reports: "이 채팅에서, 평일 2시간마다",
            needs_setup: None,
            examples: [
                "오전 11시 이후 #help에 새 질문 3건. 가장 오래된 질문: SSH 원격 연결이 계속 끊김, 2시간 경과.",
                "\"봇을 내보낼 수 있나요?\"는 이미 SUP-212가 있습니다.",
            ],
        },
        "feedback" => TemplateKo {
            name: "피드백 요약",
            description: "피드백을 주간 주제 목록으로 정리합니다.",
            first_chat: "선택한 채널의 최근 한 달 피드백을 주제별로 묶어 개수와 인용문을 함께 정리합니다.",
            watches: "Slack 피드백 채널",
            does: "피드백을 주제별로 묶어 인용문과 개수를 정리하고, 이미 있는 Linear 이슈를 연결합니다.",
            reports: "이 채팅에서, 매주 월요일 오전 9시",
            needs_setup: None,
            examples: [
                "가장 많은 주제: Windows 터미널에서 붙여넣기, 7명, KRL-640 연결됨.",
                "이번 주 새 요청: 3명이 봇 내보내기를 요청했습니다.",
            ],
        },
        "bug-intake" => TemplateKo {
            name: "버그 리포트 접수",
            description: "Slack의 버그 리포트를 재현 단계가 담긴 추적 가능한 이슈로 바꿉니다.",
            first_chat: "선택한 채널을 읽고 새 버그 리포트마다 이슈 초안을 작성합니다. 승인한 뒤에만 등록합니다.",
            watches: "버그가 제보되는 Slack 채널",
            does: "버그 리포트마다 재현 단계가 담긴 Linear 이슈 초안을 작성하고, 승인하면 등록합니다.",
            reports: "이 채팅에서, 버그가 제보될 때",
            needs_setup: None,
            examples: [
                "초안 작성: \"macOS 27에서 절전 후 터미널이 멈춤\", 재현 단계는 스레드에서 가져왔습니다.",
                "KRL-702와 중복일 수 있습니다.",
            ],
        },
        "issue-triage" => TemplateKo {
            name: "GitHub 이슈 분류",
            description: "새 이슈에 라벨을 붙이고, 중복을 찾고, 빠진 정보를 요청합니다.",
            first_chat: "선택한 저장소의 지난주 새 이슈를 훑어보고 라벨과 중복 후보를 이 채팅에 제안합니다.",
            watches: "선택한 저장소의 새 이슈",
            does: "새 이슈마다 라벨과 중복 후보를 제안하고 빠진 정보를 정리합니다.",
            reports: "이 채팅에서, 이슈가 열릴 때",
            needs_setup: None,
            examples: [
                "#2210: 버그, windows, #2188과 중복.",
                "#2214에 버전과 재현 단계가 없습니다. 요청할까요?",
            ],
        },
        "briefing" => TemplateKo {
            name: "프로젝트 브리핑",
            description: "선택한 프로젝트의 주간 브리핑입니다.",
            first_chat: "선택한 프로젝트에서 바뀐 점, 막힌 점, 결정이 필요한 점을 정리합니다.",
            watches: "Linear 프로젝트와 GitHub 저장소",
            does: "바뀐 점, 막힌 점, 결정이 필요한 점을 정리합니다.",
            reports: "이 채팅에서, 매주 금요일 오후 3시",
            needs_setup: None,
            examples: [
                "모바일 v2: 이슈 6건 완료, 2건은 디자인 대기 중입니다.",
                "결정 필요: KRL-902, 푸시 권한 문구.",
            ],
        },
        "docs-upkeep" => TemplateKo {
            name: "문서 관리",
            description: "병합된 코드와 더 이상 맞지 않는 문서를 알려 줍니다.",
            first_chat: "지난주에 병합된 PR을 읽고, 이제 내용이 틀려진 문서 페이지를 정리합니다.",
            watches: "선택한 저장소의 병합된 PR과 문서",
            does: "병합된 내용과 맞지 않는 문서 페이지를 지금 틀린 줄과 함께 정리합니다.",
            reports: "이 채팅에서, 매주 수요일 오전 10시",
            needs_setup: None,
            examples: [
                "docs/remote.md에는 SSH에 재시작이 필요하다고 되어 있지만, #4108에서 이미 없어졌습니다.",
                "옛 설정 이름이 남은 페이지가 1개 더 있습니다.",
            ],
        },
        "spec-drafts" => TemplateKo {
            name: "스펙 초안 작성",
            description: "논의 내용을 공유할 수 있는 짧은 스펙으로 정리합니다.",
            first_chat: "Slack 스레드나 문서를 건네 주세요. 한 페이지 스펙 초안을 작성하고 정해지지 않은 부분은 질문합니다.",
            watches: "건네 주신 Slack 스레드와 문서",
            does: "문제, 결정, 범위, 열린 질문을 담은 한 페이지 스펙 초안을 작성합니다.",
            reports: "이 채팅에서, 요청할 때",
            needs_setup: None,
            examples: [
                "초안: \"봇 공유 v2\". 결정 3건, 열린 질문 2건.",
                "내 Drive에 문서로 저장했습니다.",
            ],
        },
        "backlog-cleanup" => TemplateKo {
            name: "백로그 정리",
            description: "오래된 이슈, 중복 이슈, 담당자 없는 이슈를 찾습니다.",
            first_chat: "선택한 팀에서 60일 동안 변동이 없는 이슈, 중복으로 보이는 이슈, 담당자가 없는 이슈를 정리합니다.",
            watches: "선택한 팀의 Linear 이슈",
            does: "오래된 이슈, 중복으로 보이는 이슈, 담당자 없는 이슈를 정리합니다.",
            reports: "이 채팅에서, 매월 1일",
            needs_setup: None,
            examples: [
                "8월 이후 변동이 없는 이슈 14건.",
                "KRL-511과 KRL-688은 같은 버그로 보입니다.",
            ],
        },
        "incident-notes" => TemplateKo {
            name: "장애 기록",
            description: "장애 채널의 타임라인을 기록하고 회고 문서 초안을 작성합니다.",
            first_chat: "선택한 장애 채널과 스레드를 따라가며 시각이 찍힌 타임라인을 기록하고, 장애가 끝나면 비난 없는 회고 문서 초안을 작성합니다.",
            watches: "선택한 장애 채널과 그 스레드",
            does: "시각이 찍힌 타임라인을 기록한 뒤 비난 없는 회고 문서 초안을 작성합니다.",
            reports: "이 채팅에서, 장애 중과 장애 후",
            needs_setup: None,
            examples: [
                "14:02 알림 발생, 14:09 롤백 시작, 14:21 오류 정상화.",
                "회고 문서 초안이 준비되었습니다.",
            ],
        },
        "oncall-handoff" => TemplateKo {
            name: "온콜 인수인계",
            description: "알림, 장애, 남은 후속 조치를 주간으로 요약합니다.",
            first_chat: "지난주의 알림 채널과 장애 이슈를 읽고 인수인계 문서를 작성합니다.",
            watches: "내 알림 채널과 장애 이슈",
            does: "한 주의 알림과 장애를 요약하고 남은 후속 조치를 정리합니다.",
            reports: "이 채팅에서, 매주 금요일 오후 4시",
            needs_setup: Some("호출 도구 커넥터가 없어 Slack에 올라온 알림만 읽습니다."),
            examples: [
                "알림 12건, 장애 2건. cell-3의 디스크 알림이 5번 발생했습니다.",
                "후속 조치 3건이 아직 열려 있습니다.",
            ],
        },
        "followup" => TemplateKo {
            name: "답장할 항목",
            description: "아직 답장하지 않은 항목을 알려 줍니다.",
            first_chat: "Slack 스레드와 이슈 댓글에서 아직 답하지 않은 나에게 온 질문과 내가 하겠다고 말한 일을 찾습니다.",
            watches: "Slack 스레드와 Linear 댓글",
            does: "아직 답하지 않은 나에게 온 질문과 내가 하겠다고 말한 일을 찾습니다.",
            reports: "이 채팅에서, 매일 오후 4시",
            needs_setup: None,
            examples: [
                "2일 전 #design에서 팀원에게 \"내일 확인해 볼게요\"라고 말했습니다.",
                "KRL-881에 어제 온 질문이 있습니다.",
            ],
        },
        "standup" => TemplateKo {
            name: "데일리 스탠드업 노트",
            description: "어제 나와 에이전트가 한 일과 다음 할 일을 정리합니다.",
            first_chat: "내 워크스페이스, 병합된 PR, 상태가 바뀐 이슈를 바탕으로 어제 노트를 작성합니다. 팀이 보는 곳에는 직접 복사해 붙여넣으세요.",
            watches: "내 워크스페이스, PR, 이슈",
            does: "어제 노트와 오늘 계획을 작성합니다.",
            reports: "이 채팅에서, 평일 오전 9시",
            needs_setup: None,
            examples: [
                "어제: #4121 병합(릴레이 재시도). 에이전트가 워크스페이스 2개를 완료했습니다.",
                "오늘: KRL-702, #4130 리뷰.",
            ],
        },
        "email-digest" => TemplateKo {
            name: "이메일 요약",
            description: "하루 두 번, 내 답장이 필요한 이메일을 알려 줍니다.",
            first_chat: "오늘 받은 편지함을 읽고 답장이 필요한 이메일을 최신순으로 정리합니다. 이메일은 절대 보내지 않습니다.",
            watches: "내 받은 편지함",
            does: "답장이 필요한 이메일을 최신순으로 정리합니다. 이메일은 보내지 않습니다.",
            reports: "이 채팅에서, 평일 오전 9시와 오후 2시",
            needs_setup: None,
            examples: [
                "답장이 필요한 메일 2건: 보안 리뷰 일정, 청구서 문의.",
                "뉴스레터 14건은 건너뛰었습니다.",
            ],
        },
        "meeting-prep" => TemplateKo {
            name: "미팅 준비",
            description: "미팅마다 관련된 이슈, PR, 문서를 미리 정리합니다.",
            first_chat: "오늘 캘린더를 보고 미팅마다 짧은 준비 노트를 작성합니다.",
            watches: "내 캘린더",
            does: "미팅 전에 관련된 이슈, PR, 문서를 정리합니다.",
            reports: "이 채팅에서, 각 미팅 전에",
            needs_setup: None,
            examples: [
                "11시 모바일 싱크: KRL-902는 결정이 필요하고, #4130이 내 리뷰를 기다립니다.",
                "문서: \"푸시 권한 문구\".",
            ],
        },
        _ => return None,
    })
}

fn schedule_ko(tpl_id: &str, sched_id: &str) -> Option<(&'static str, &'static str)> {
    Some(match (tpl_id, sched_id) {
        ("dependency-updates", "weekly-check") => {
            ("매주 월요일 오전 9시", "새 보안 권고와 메이저 버전")
        }
        ("unreviewed-work", "daily-list") => ("평일 오전 9시", "검토를 기다리는 완료된 작업"),
        ("workspace-cleanup", "weekly-list") => {
            ("매주 금요일 오후 4시", "삭제할 수 있는 워크스페이스")
        }
        ("security-check", "weekly-review") => ("매주 월요일 오전 9시", "이번 주에 병합된 변경"),
        ("release", "readiness-list") => ("평일 오전 9시", "준비 상태 목록"),
        ("ci-failures", "flaky-list") => ("매주 월요일 오전 9시", "불안정한 체크"),
        ("support", "unanswered") => ("평일 오전 9시~오후 5시, 2시간마다", "답변 없는 질문"),
        ("feedback", "weekly-themes") => ("매주 월요일 오전 9시", "이번 주 주제"),
        ("briefing", "weekly-brief") => ("매주 금요일 오후 3시", "브리핑"),
        ("docs-upkeep", "weekly-docs") => ("매주 수요일 오전 10시", "수정할 문서"),
        ("backlog-cleanup", "monthly-list") => ("매월 1일 오전 9시", "정리 목록"),
        ("oncall-handoff", "weekly-handoff") => ("매주 금요일 오후 4시", "인수인계"),
        ("followup", "daily-owed") => ("매일 오후 4시", "아직 남은 답장"),
        ("standup", "daily-notes") => ("평일 오전 9시", "스탠드업 노트"),
        ("email-digest", "morning") => ("평일 오전 9시", "내 답장이 필요한 이메일"),
        ("email-digest", "afternoon") => ("평일 오후 2시", "내 답장이 필요한 이메일"),
        _ => return None,
    })
}

fn event_ko(tpl_id: &str, event_id: &str) -> Option<&'static str> {
    Some(match (tpl_id, event_id) {
        ("pr-review", "review-requested") => "PR 리뷰를 요청받을 때 · 리뷰하고 이 채팅에 올리기",
        ("bug-investigation", "linear-assigned") => "Linear 이슈가 나에게 할당될 때 · 조사하기",
        ("security-check", "pr-merged") => "PR이 병합될 때 · 확인하기",
        ("ci-failures", "check-failed") => {
            "체크가 실패할 때 · 불안정한 체크인지 진짜 실패인지 알려 주기"
        }
        ("deploy-watch", "deploy-called") => "배포 도구가 호출할 때 · 배포 요약하기",
        ("bug-intake", "bug-posted") => "버그 채널에 누군가 글을 올릴 때 · 이슈 초안 작성하기",
        ("issue-triage", "issue-opened") => "이슈가 열릴 때 · 라벨과 중복 후보 제안하기",
        ("incident-notes", "incident-posted") => {
            "장애 채널에 누군가 글을 올릴 때 · 타임라인에 추가하기"
        }
        ("meeting-prep", "meeting-soon") => "미팅 15분 전 · 준비 노트",
        _ => return None,
    })
}

fn skill_ko(skill_id: &str) -> Option<(&'static str, &'static str)> {
    Some(match skill_id {
        "pr-review-checklist" => (
            "PR 리뷰 체크리스트",
            "diff에서 동작 변경, 빠진 테스트, 위험한 수정을 읽어내고 각 지적마다 근거를 제시합니다.",
        ),
        "bug-reproduction" => (
            "버그 재현 절차",
            "원인을 찾기 전에 버그를 보여 주는 정확한 단계를 먼저 기록합니다.",
        ),
        "ci-failure-triage" => (
            "CI 실패 분류",
            "불안정한 체크, 진짜 실패, 인프라 문제를 구분하고 문제가 시작된 커밋을 찾습니다.",
        ),
        "release-readiness" => (
            "릴리스 준비 점검",
            "릴리스를 막는 요소와 목록 작성 방법: 체크, 리뷰, 열린 블로커.",
        ),
        "release-notes-style" => (
            "릴리스 노트 작성 규칙",
            "사용자용 노트: 신규, 수정, 변경. 리팩터링과 코드명은 제외합니다.",
        ),
        "dependency-upgrade" => (
            "의존성 업그레이드",
            "한 번에 한 그룹씩 업그레이드하고, 변경 내역을 읽고, 테스트를 실행하고, 그룹마다 PR을 하나씩 만듭니다.",
        ),
        "security-checklist" => (
            "보안 체크리스트",
            "변경에서 살펴볼 시크릿, 인젝션, 인증 확인, 안전하지 않은 기본값.",
        ),
        "issue-triage" => (
            "이슈 분류",
            "라벨을 붙이고, 중복을 찾고, 빠진 재현 정보를 댓글 하나로 요청합니다.",
        ),
        "feedback-themes" => (
            "피드백 주제 정리",
            "피드백을 개수와 짧은 인용문이 있는 주제별로 묶고, 기존 이슈를 연결합니다.",
        ),
        "status-brief" => (
            "상태 브리핑",
            "바뀐 점, 막힌 점, 결정이 필요한 점을 열 줄 이내 브리핑으로 정리합니다.",
        ),
        "spec-format" => ("스펙 형식", "한 페이지 스펙: 문제, 결정, 범위, 열린 질문."),
        "incident-timeline" => (
            "장애 타임라인",
            "시각이 찍힌 타임라인을 기록한 뒤 비난 없는 회고 문서 초안을 작성합니다.",
        ),
        "workspace-hygiene" => (
            "워크스페이스 정리 규칙",
            "워크스페이스가 완료되었거나 방치된 시점과, 삭제 전에 확인할 사항.",
        ),
        _ => return None,
    })
}

fn is_korean() -> bool {
    crate::i18n::language() == Language::Ko
}

#[must_use]
pub fn localized_template_name(tpl: &BotTemplate) -> &'static str {
    if is_korean()
        && let Some(ko) = template_ko(tpl.id)
    {
        return ko.name;
    }
    tpl.name
}

#[must_use]
pub fn localized_template_description(tpl: &BotTemplate) -> &'static str {
    if is_korean()
        && let Some(ko) = template_ko(tpl.id)
    {
        return ko.description;
    }
    tpl.description
}

#[must_use]
pub fn localized_template_first_chat(tpl: &BotTemplate) -> &'static str {
    if is_korean()
        && let Some(ko) = template_ko(tpl.id)
    {
        return ko.first_chat;
    }
    tpl.first_chat
}

#[must_use]
pub fn localized_template_watches(tpl: &BotTemplate, fallback: Option<String>) -> Option<String> {
    if is_korean()
        && let Some(ko) = template_ko(tpl.id)
    {
        return Some(ko.watches.to_owned());
    }
    fallback
}

#[must_use]
pub fn localized_template_does(tpl: &BotTemplate, fallback: Option<String>) -> Option<String> {
    if is_korean()
        && let Some(ko) = template_ko(tpl.id)
    {
        return Some(ko.does.to_owned());
    }
    fallback
}

#[must_use]
pub fn localized_template_reports(tpl: &BotTemplate, fallback: Option<String>) -> Option<String> {
    if is_korean()
        && let Some(ko) = template_ko(tpl.id)
    {
        return Some(ko.reports.to_owned());
    }
    fallback
}

#[must_use]
pub fn localized_template_needs_setup(tpl: &BotTemplate) -> Option<&'static str> {
    if is_korean()
        && let Some(ko) = template_ko(tpl.id)
    {
        return ko.needs_setup.or(tpl.needs_setup);
    }
    tpl.needs_setup
}

#[must_use]
pub fn localized_template_examples(tpl: &BotTemplate) -> [&'static str; 2] {
    if is_korean()
        && let Some(ko) = template_ko(tpl.id)
    {
        return ko.examples;
    }
    tpl.example
}

#[must_use]
pub fn localized_schedule(
    tpl_id: &str,
    sched: &BotTemplateSchedule,
) -> (&'static str, &'static str) {
    if is_korean()
        && let Some(ko) = schedule_ko(tpl_id, sched.id)
    {
        return ko;
    }
    (sched.when, sched.what)
}

#[must_use]
pub fn localized_event_name(tpl_id: &str, ev: &BotTemplateEvent) -> &'static str {
    if is_korean()
        && let Some(ko) = event_ko(tpl_id, ev.id)
    {
        return ko;
    }
    ev.name
}

#[must_use]
pub fn localized_skill_title(skill_id: &str) -> String {
    if is_korean()
        && let Some((title, _)) = skill_ko(skill_id)
    {
        return format!("{title} ({skill_id})");
    }
    skill_id.to_owned()
}

#[must_use]
pub fn localized_skill_description(skill_id: &str, fallback: &'static str) -> &'static str {
    if is_korean()
        && let Some((_, desc)) = skill_ko(skill_id)
    {
        return desc;
    }
    fallback
}

/// Templates matching `category` and every word of `query` (name, line, category, connectors,
/// skills, id — both English and Korean), in catalog order.
#[must_use]
pub fn filter_templates(
    category: Option<BotTemplateCategory>,
    query: &str,
) -> Vec<&'static BotTemplate> {
    let q = query.trim().to_lowercase();
    BUILTIN_BOT_TEMPLATES
        .iter()
        .filter(|tpl| category.is_none_or(|c| tpl.category == c))
        .filter(|tpl| {
            if q.is_empty() {
                return true;
            }
            let mut hay = format!(
                "{} {} {} {}",
                tpl.name,
                tpl.description,
                tpl.id,
                category_label(tpl.category)
            );
            if let Some(ko) = template_ko(tpl.id) {
                hay.push(' ');
                hay.push_str(ko.name);
                hay.push(' ');
                hay.push_str(ko.description);
            }
            for c in tpl.connectors {
                hay.push(' ');
                hay.push_str(super::View::source_label(*c));
            }
            for s in tpl.skills {
                hay.push(' ');
                hay.push_str(s);
                if let Some((title, _)) = skill_ko(s) {
                    hay.push(' ');
                    hay.push_str(title);
                }
            }
            let hay = hay.to_lowercase();
            q.split_whitespace().all(|term| hay.contains(term))
        })
        .collect()
}

/// Localized label for a [`BotTemplateCategory`].
#[must_use]
pub fn category_label(cat: BotTemplateCategory) -> &'static str {
    match cat {
        BotTemplateCategory::Coding => text::CAT_CODING.get(),
        BotTemplateCategory::Releases => text::CAT_RELEASES.get(),
        BotTemplateCategory::Support => text::CAT_SUPPORT.get(),
        BotTemplateCategory::Planning => text::CAT_PLANNING.get(),
        BotTemplateCategory::Ops => text::CAT_OPS.get(),
        BotTemplateCategory::Personal => text::CAT_PERSONAL.get(),
    }
}

pub struct Templates {
    focus: FocusHandle,
    search: Entity<InputState>,
    query: String,
    category: Option<BotTemplateCategory>,
    selected: Option<&'static str>,
    /// Narrow windows show one template's details in place of the list.
    detail_only: bool,
    list_scroll: ScrollHandle,
    _subs: Vec<Subscription>,
}

impl EventEmitter<Event> for Templates {}

impl std::fmt::Debug for Templates {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Templates")
            .field("query", &self.query)
            .field("category", &self.category)
            .field("selected", &self.selected)
            .finish_non_exhaustive()
    }
}

fn category_icon(cat: Option<BotTemplateCategory>, color: gpui::Hsla) -> gpui::AnyElement {
    match cat {
        None => ui::icons::svg_icon(ui::icons::LAYOUT_TEMPLATE_SVG, ui::ICON_XS, color)
            .into_any_element(),
        Some(BotTemplateCategory::Coding) => {
            ui::icons::svg_icon(ui::icons::SQUARE_TERMINAL_SVG, ui::ICON_XS, color)
                .into_any_element()
        }
        Some(BotTemplateCategory::Releases) => {
            ui::icons::svg_icon(ui::icons::SPARKLES_SVG, ui::ICON_XS, color).into_any_element()
        }
        Some(BotTemplateCategory::Support) => Icon::new(gpui_kit_assets::IconName::MessagesSquare)
            .with_size(ui::ICON_XS)
            .text_color(color)
            .into_any_element(),
        Some(BotTemplateCategory::Planning) => Icon::new(IconName::FileText)
            .with_size(ui::ICON_XS)
            .text_color(color)
            .into_any_element(),
        Some(BotTemplateCategory::Ops) => {
            ui::icons::svg_icon(ui::icons::WRENCH_SVG, ui::ICON_XS, color).into_any_element()
        }
        Some(BotTemplateCategory::Personal) => Icon::new(IconName::User)
            .with_size(ui::ICON_XS)
            .text_color(color)
            .into_any_element(),
    }
}

impl Templates {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder(text::TPL_SEARCH.get()));
        let subs = vec![cx.subscribe_in(&search, window, Self::on_search)];
        search.update(cx, |s, cx| s.focus(window, cx));
        Self {
            focus: cx.focus_handle(),
            search,
            query: String::new(),
            category: None,
            selected: BUILTIN_BOT_TEMPLATES.first().map(|t| t.id),
            detail_only: false,
            list_scroll: ScrollHandle::new(),
            _subs: subs,
        }
    }

    #[must_use]
    pub fn focus_handle(&self) -> FocusHandle {
        self.focus.clone()
    }

    fn on_search(
        &mut self,
        input: &Entity<InputState>,
        ev: &InputEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match ev {
            InputEvent::Change => {
                self.query = input.read(cx).value().to_string();
                self.keep_selection_visible();
                cx.notify();
            }
            InputEvent::PressEnter { .. }
                if !crate::jump::entity_input_is_composing(input, window, cx) =>
            {
                self.use_selected(cx);
            }
            _ => {}
        }
    }

    fn visible(&self) -> Vec<&'static BotTemplate> {
        filter_templates(self.category, &self.query)
    }

    /// Keeps a visible row selected after the filter changes.
    fn keep_selection_visible(&mut self) {
        let visible = self.visible();
        if !visible.iter().any(|t| Some(t.id) == self.selected) {
            self.selected = visible.first().map(|t| t.id);
        }
    }

    pub fn set_category(&mut self, category: Option<BotTemplateCategory>, cx: &mut Context<Self>) {
        self.category = category;
        self.keep_selection_visible();
        cx.notify();
    }

    fn select(&mut self, id: &'static str, cx: &mut Context<Self>) {
        self.selected = Some(id);
        self.detail_only = true;
        cx.notify();
    }

    fn step(&mut self, delta: isize, cx: &mut Context<Self>) {
        let visible = self.visible();
        if visible.is_empty() {
            return;
        }
        let at = visible
            .iter()
            .position(|t| Some(t.id) == self.selected)
            .map_or(0, |i| {
                (i.cast_signed() + delta)
                    .clamp(0, visible.len().cast_signed() - 1)
                    .cast_unsigned()
            });
        self.selected = Some(visible[at].id);
        // The list also holds one header per shown category.
        self.list_scroll
            .scroll_to_item(list_child_index(&visible, at));
        cx.notify();
    }

    fn selected_template(&self) -> Option<&'static BotTemplate> {
        let id = self.selected?;
        BUILTIN_BOT_TEMPLATES.iter().find(|t| t.id == id)
    }

    pub fn use_selected(&mut self, cx: &mut Context<Self>) {
        if let Some(t) = self.selected_template()
            && self.visible().iter().any(|v| v.id == t.id)
        {
            cx.emit(Event::Use(t));
        }
    }

    fn blank(&mut self, cx: &mut Context<Self>) {
        let q = self.query.trim();
        cx.emit(Event::Blank((!q.is_empty()).then(|| q.to_owned())));
    }

    fn on_key(&mut self, e: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if crate::jump::entity_input_is_composing(&self.search, window, cx) {
            return;
        }
        match e.keystroke.key.as_str() {
            "escape" => {
                cx.stop_propagation();
                // Orca: Esc clears the search first, then closes.
                if self.query.is_empty() {
                    cx.emit(Event::Close);
                } else {
                    self.search.update(cx, |s, cx| s.set_value("", window, cx));
                    self.query.clear();
                    self.keep_selection_visible();
                    cx.notify();
                }
            }
            "up" => {
                cx.stop_propagation();
                self.step(-1, cx);
            }
            "down" => {
                cx.stop_propagation();
                self.step(1, cx);
            }
            _ => {}
        }
    }

    fn render_chips(&self, p: &ui::Palette, cx: &mut Context<Self>) -> gpui::Div {
        let chip = |id: SharedString,
                    cat: Option<BotTemplateCategory>,
                    label: &'static str,
                    active: bool| {
            let fg = if active {
                p.accent_foreground
            } else {
                p.muted_foreground
            };
            div()
                .id(id.clone())
                .debug_selector(move || id.to_string())
                .role(gpui::Role::Button)
                .tab_index(0)
                .focus_visible(|d| d.border_color(p.ring))
                .flex_none()
                .h(px(24.))
                .px_2()
                .rounded(ui::RADIUS_SM)
                .border_1()
                .flex()
                .items_center()
                .gap_1p5()
                .text_size(ui::TEXT_SM)
                .cursor_pointer()
                .when(active, |d| {
                    d.bg(p.accent)
                        .border_color(p.accent)
                        .text_color(p.accent_foreground)
                        .font_weight(FontWeight::MEDIUM)
                })
                .when(!active, |d| {
                    d.border_color(p.border)
                        .text_color(p.muted_foreground)
                        .hover(|d| d.bg(p.accent.opacity(0.5)))
                })
                .child(category_icon(cat, fg))
                .child(label)
        };
        let mut row = div().flex().flex_wrap().gap_1p5().child(
            chip(
                "bot-tpl-cat-all".into(),
                None,
                text::CAT_ALL.get(),
                self.category.is_none(),
            )
            .on_click(cx.listener(|this, _, _, cx| this.set_category(None, cx))),
        );
        for cat in BotTemplateCategory::ALL {
            let cat = *cat;
            row = row.child(
                chip(
                    format!("bot-tpl-cat-{cat:?}").to_lowercase().into(),
                    Some(cat),
                    category_label(cat),
                    self.category == Some(cat),
                )
                .on_click(cx.listener(move |this, _, _, cx| this.set_category(Some(cat), cx))),
            );
        }
        row
    }

    fn render_row(
        &self,
        tpl: &'static BotTemplate,
        p: &ui::Palette,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let current = self.selected == Some(tpl.id);
        let id = SharedString::from(format!("bot-tpl-{}", tpl.id));
        let use_id = SharedString::from(format!("bot-tpl-use-{}", tpl.id));
        let name = localized_template_name(tpl);
        let desc = localized_template_description(tpl);
        div()
            .id(id.clone())
            .debug_selector(move || id.to_string())
            .role(gpui::Role::ListItem)
            .aria_label(name)
            .group("bot-tpl-row")
            .w_full()
            .px_2()
            .py_2p5()
            .rounded(ui::RADIUS_SM)
            .flex()
            .items_center()
            .gap_3()
            .cursor_pointer()
            .when(current, |d| d.bg(p.accent))
            .when(!current, |d| d.hover(|d| d.bg(p.accent.opacity(0.5))))
            .on_click(cx.listener(move |this, e: &gpui::ClickEvent, _, cx| {
                if e.click_count() >= 2 {
                    cx.emit(Event::Use(tpl));
                } else {
                    this.select(tpl.id, cx);
                }
            }))
            .child(template_mascot(tpl, 32., ui::RADIUS_LG, p))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap_0p5()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_1p5()
                            .child(
                                div()
                                    .truncate()
                                    .text_size(ui::TEXT_LG)
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(p.foreground)
                                    .child(name),
                            )
                            .when(tpl.needs_setup.is_some(), |d| {
                                d.child(ui::badge(
                                    p,
                                    ui::BadgeTone::Muted,
                                    text::TPL_NEEDS_SETUP.get(),
                                ))
                            }),
                    )
                    .child(
                        div()
                            .truncate()
                            .text_size(ui::TEXT_SM)
                            .text_color(p.muted_foreground)
                            .child(desc),
                    ),
            )
            .child(
                div()
                    .flex_none()
                    .when(!current, |d| {
                        d.invisible()
                            .group_hover("bot-tpl-row", gpui::Styled::visible)
                    })
                    .child(
                        ui::secondary(use_id.clone(), text::TPL_USE.get())
                            .xsmall()
                            .accessibility_label(text::TPL_USE_NAMED.format(&[("name", name)]))
                            .debug_selector(move || use_id.to_string())
                            .on_click(cx.listener(move |_, _, _, cx| {
                                cx.stop_propagation();
                                cx.emit(Event::Use(tpl));
                            })),
                    ),
            )
    }

    fn render_list(&self, p: &ui::Palette, cx: &mut Context<Self>) -> gpui::Stateful<gpui::Div> {
        let visible = self.visible();
        let mut list = div()
            .id("bot-tpl-list")
            .debug_selector(|| "bot-tpl-list".into())
            .role(gpui::Role::List)
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .track_scroll(&self.list_scroll)
            .px_6()
            .pb_8()
            .flex()
            .flex_col();
        if visible.is_empty() {
            let q = self.query.trim().to_owned();
            return list.child(
                div()
                    .id("bot-tpl-empty")
                    .debug_selector(|| "bot-tpl-empty".into())
                    .pt_10()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap_2()
                    .text_center()
                    .child(
                        div()
                            .text_size(ui::TEXT_LG)
                            .font_weight(FontWeight::MEDIUM)
                            .child(text::TPL_NO_MATCH.format(&[("query", &q)])),
                    )
                    .child(
                        div()
                            .text_size(ui::TEXT_SM)
                            .text_color(p.muted_foreground)
                            .child(text::TPL_NO_MATCH_HINT.get()),
                    )
                    .child(
                        div()
                            .mt_2()
                            .flex()
                            .gap_2()
                            .child(
                                ui::secondary(
                                    "bot-tpl-blank-for",
                                    text::TPL_BLANK_FOR.format(&[("query", &q)]),
                                )
                                .on_click(cx.listener(|this, _, _, cx| this.blank(cx))),
                            )
                            .child(
                                ui::ghost("bot-tpl-clear", text::SB_CLEAR_SEARCH.get()).on_click(
                                    cx.listener(|this, _, window, cx| {
                                        this.search.update(cx, |s, cx| s.set_value("", window, cx));
                                        this.query.clear();
                                        this.keep_selection_visible();
                                        cx.notify();
                                    }),
                                ),
                            ),
                    ),
            );
        }
        for cat in BotTemplateCategory::ALL {
            let in_cat: Vec<_> = visible.iter().filter(|t| t.category == *cat).collect();
            if in_cat.is_empty() {
                continue;
            }
            list = list.child(
                div()
                    .py_2()
                    .bg(p.popover)
                    .flex()
                    .items_center()
                    .gap_1p5()
                    .text_size(ui::TEXT_SM)
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(p.muted_foreground)
                    .child(category_icon(Some(*cat), p.muted_foreground))
                    .child(category_label(*cat)),
            );
            for tpl in in_cat {
                list = list.child(self.render_row(tpl, p, cx));
            }
        }
        list
    }

    fn render_detail(
        tpl: &'static BotTemplate,
        narrow: bool,
        p: &ui::Palette,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let name = localized_template_name(tpl);
        let desc = localized_template_description(tpl);
        let first_chat = localized_template_first_chat(tpl);
        let needs_setup = localized_template_needs_setup(tpl);
        let examples = localized_template_examples(tpl);
        let section = |title: &'static str| {
            div()
                .text_size(ui::TEXT_SM)
                .font_weight(FontWeight::MEDIUM)
                .text_color(p.foreground)
                .child(title)
        };
        let comes_with = comes_with_rows(tpl, p, |_| None);
        div()
            .id("bot-tpl-detail")
            .debug_selector(|| "bot-tpl-detail".into())
            .h_full()
            .min_h_0()
            .flex()
            .flex_col()
            .when(!narrow, |d| {
                d.w(gpui::relative(0.3))
                    .min_w(px(400.))
                    .border_l_1()
                    .border_color(p.border)
            })
            .when(narrow, gpui::Styled::flex_1)
            .child(
                div()
                    .flex_none()
                    .px_6()
                    .pt_6()
                    .pb_5()
                    .flex()
                    .flex_col()
                    .gap_5()
                    .when(narrow, |d| {
                        d.child(
                            ui::ghost("bot-tpl-back", text::CAT_ALL.get())
                                .icon(IconName::ArrowLeft)
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.detail_only = false;
                                    cx.notify();
                                })),
                        )
                    })
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_3()
                            .child(template_mascot(tpl, 56., ui::RADIUS_XL, p))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .flex()
                                    .flex_col()
                                    .gap_0p5()
                                    .child(
                                        div()
                                            .flex()
                                            .items_center()
                                            .gap_1p5()
                                            .child(
                                                div()
                                                    .text_size(ui::TEXT_XL)
                                                    .font_weight(FontWeight::SEMIBOLD)
                                                    .child(name),
                                            )
                                            .when(tpl.needs_setup.is_some(), |d| {
                                                d.child(ui::badge(
                                                    p,
                                                    ui::BadgeTone::Muted,
                                                    text::TPL_NEEDS_SETUP.get(),
                                                ))
                                            }),
                                    )
                                    .child(
                                        div()
                                            .flex()
                                            .items_center()
                                            .gap_1()
                                            .text_size(ui::TEXT_SM)
                                            .text_color(p.muted_foreground)
                                            .child(
                                                Icon::new(IconName::CircleCheck)
                                                    .with_size(ui::ICON_XS)
                                                    .text_color(p.muted_foreground),
                                            )
                                            .child(text::TPL_BY_KRILL.format(&[(
                                                "category",
                                                category_label(tpl.category),
                                            )])),
                                    ),
                            ),
                    )
                    .child(div().text_size(ui::TEXT_LG).child(desc))
                    .child(
                        ui::primary("bot-tpl-use-detail", text::TPL_USE_TEMPLATE.get())
                            .debug_selector(|| "bot-tpl-use-detail".into())
                            .on_click(cx.listener(move |_, _, _, cx| cx.emit(Event::Use(tpl)))),
                    ),
            )
            .child(
                div()
                    .id("bot-tpl-detail-body")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .px_6()
                    .pb_6()
                    .flex()
                    .flex_col()
                    .gap_5()
                    .child(
                        div()
                            .p_3()
                            .rounded(ui::RADIUS_MD)
                            .border_1()
                            .border_color(p.border)
                            .bg(p.muted.opacity(0.4))
                            .flex()
                            .flex_col()
                            .gap_2()
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_1p5()
                                    .text_size(ui::TEXT_SM)
                                    .text_color(p.muted_foreground)
                                    .child(template_mascot(tpl, 16., ui::RADIUS_XS, p))
                                    .child(text::TPL_IN_ITS_CHAT.format(&[("name", name)])),
                            )
                            .children(
                                examples
                                    .into_iter()
                                    .filter(|l| !l.is_empty())
                                    .map(|line| div().text_size(ui::TEXT_LG).child(line)),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(section(text::TPL_FIRST.get()))
                            .child(
                                div()
                                    .text_size(ui::TEXT_LG)
                                    .text_color(p.muted_foreground)
                                    .child(first_chat),
                            ),
                    )
                    .children(needs_setup.map(|note| {
                        div()
                            .p_3()
                            .rounded(ui::RADIUS_MD)
                            .border_1()
                            .border_color(p.border)
                            .bg(p.muted.opacity(0.4))
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(section(text::TPL_NEEDS_SETUP.get()))
                            .child(
                                div()
                                    .text_size(ui::TEXT_SM)
                                    .text_color(p.muted_foreground)
                                    .child(note),
                            )
                    }))
                    .when(!comes_with.is_empty(), |d| {
                        d.child(
                            div()
                                .flex()
                                .flex_col()
                                .child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .gap_1()
                                        .child(section(text::NB_COMES_WITH.get()))
                                        .child(ui::button_with_tooltip(
                                            Button::new("bot-tpl-comes-with-info")
                                                .ghost()
                                                .xsmall()
                                                .icon(
                                                    Icon::new(IconName::Info)
                                                        .with_size(ui::ICON_XS)
                                                        .text_color(p.muted_foreground),
                                                ),
                                            "bot-tpl-comes-with-info",
                                            text::TPL_COMES_WITH_INFO.get(),
                                        )),
                                )
                                .children(comes_with),
                        )
                    }),
            )
    }
}

/// Position of `visible[at]` among the list's children (category headers included).
fn list_child_index(visible: &[&BotTemplate], at: usize) -> usize {
    let mut child = 0;
    for cat in BotTemplateCategory::ALL {
        let in_cat: Vec<usize> = (0..visible.len())
            .filter(|i| visible[*i].category == *cat)
            .collect();
        if in_cat.is_empty() {
            continue;
        }
        child += 1;
        if let Some(pos) = in_cat.iter().position(|i| *i == at) {
            return child + pos;
        }
        child += in_cat.len();
    }
    child
}

/// The template's mascot on the default background.
fn template_mascot(
    tpl: &BotTemplate,
    size: f32,
    radius: gpui::Pixels,
    p: &ui::Palette,
) -> gpui::AnyElement {
    let face = avatar::Avatar::from_fields(Some(tpl.mascot), None, None, tpl.name);
    avatar::render(&face, None, tpl.name, px(size), radius, p)
}

/// Orca's "Comes with" rows: skills (with `skill_trailing`, e.g. New bot's checkbox),
/// schedules (off), events (first chat) and connectors (not connected).
pub(super) fn comes_with_rows(
    tpl: &'static BotTemplate,
    p: &ui::Palette,
    mut skill_trailing: impl FnMut(&'static str) -> Option<gpui::AnyElement>,
) -> Vec<gpui::Div> {
    let svg = |icon: &'static [u8]| {
        ui::icons::svg_icon(icon, ui::ICON_LG, p.muted_foreground).into_any_element()
    };
    let status = |s: &'static str, emphasize: bool| {
        div()
            .text_size(ui::TEXT_SM)
            .text_color(if emphasize {
                p.foreground
            } else {
                p.muted_foreground
            })
            .child(s)
            .into_any_element()
    };
    let row = |icon: gpui::AnyElement,
               title: SharedString,
               detail: Option<SharedString>,
               trailing: Option<gpui::AnyElement>| {
        div()
            .w_full()
            .py_2p5()
            .border_b_1()
            .border_color(p.border.opacity(0.6))
            .flex()
            .items_start()
            .gap_3()
            .child(div().pt_0p5().flex_none().child(icon))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap_0p5()
                    .child(div().text_size(ui::TEXT_SM).child(title))
                    .children(detail.map(|d| {
                        div()
                            .text_size(ui::TEXT_SM)
                            .text_color(p.muted_foreground)
                            .child(d)
                    })),
            )
            .children(trailing.map(|t| div().flex_none().child(t)))
    };
    let mut rows = Vec::new();
    for skill in tpl.skills {
        let book = gpui_component::Icon::new(IconName::BookOpen)
            .with_size(ui::ICON_LG)
            .text_color(p.muted_foreground)
            .into_any_element();
        let detail = find_bot_skill(skill)
            .map(|s| localized_skill_description(s.name, s.description).into());
        rows.push(row(
            book,
            localized_skill_title(skill).into(),
            detail,
            skill_trailing(skill),
        ));
    }
    for s in tpl.schedules {
        let (when, what) = localized_schedule(tpl.id, s);
        rows.push(row(
            svg(ui::icons::CLOCK_SVG),
            when.into(),
            Some(what.into()),
            Some(status(text::NB_OFF.get(), false)),
        ));
    }
    for e in tpl.events {
        rows.push(row(
            svg(ui::icons::ZAP_SVG),
            localized_event_name(tpl.id, e).into(),
            None,
            Some(status(text::NB_FIRST_CHAT.get(), false)),
        ));
    }
    for c in tpl.connectors {
        rows.push(row(
            svg(ui::icons::PLUG_SVG),
            super::View::source_label(*c).into(),
            None,
            Some(status(text::NB_NOT_CONNECTED.get(), true)),
        ));
    }
    rows
}

impl Render for Templates {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = ui::palette(cx);
        let viewport = window.viewport_size();
        let width = px(WIDTH).min(viewport.width - ui::SPACE_4 * 2.);
        let narrow = width < px(SPLIT_MIN_WIDTH);
        let selected = self
            .selected_template()
            .filter(|t| self.visible().iter().any(|v| v.id == t.id));
        let show_detail_only = narrow && self.detail_only && selected.is_some();

        let left = div()
            .flex_1()
            .min_w_0()
            .h_full()
            .min_h_0()
            .flex()
            .flex_col()
            .child(
                div()
                    .flex_none()
                    .px_6()
                    .pt_4()
                    .pb_2()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(
                        Input::new(&self.search).small().prefix(
                            gpui_component::Icon::new(IconName::Search)
                                .with_size(ui::ICON_MD)
                                .text_color(p.muted_foreground),
                        ),
                    )
                    .child(self.render_chips(p, cx)),
            )
            .child(self.render_list(p, cx));

        let body = div().flex_1().min_h_0().flex();
        let body = if show_detail_only {
            body.children(selected.map(|t| Self::render_detail(t, true, p, cx)))
        } else if narrow {
            body.child(left)
        } else {
            body.child(left)
                .children(selected.map(|t| Self::render_detail(t, false, p, cx)))
        };

        let card = div()
            .id("bot-templates")
            .debug_selector(|| "bot-templates".into())
            .role(gpui::Role::Dialog)
            .aria_label(text::TPL_TITLE.get())
            .occlude()
            .w(width)
            .h(px(HEIGHT).min(viewport.height - ui::SPACE_4 * 2.))
            .rounded(ui::RADIUS_LG)
            .bg(p.popover)
            .text_color(p.popover_foreground)
            .font_family(ui::FONT_UI)
            .text_size(ui::TEXT_MD)
            .border_1()
            .border_color(p.border)
            .shadow_lg()
            .overflow_hidden()
            .flex()
            .flex_col()
            .capture_key_down(
                cx.listener(|this, e: &KeyDownEvent, window, cx| this.on_key(e, window, cx)),
            )
            .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(
                div()
                    .flex_none()
                    .py_3p5()
                    .pl_6()
                    .pr_4()
                    .border_b_1()
                    .border_color(p.border)
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .text_size(ui::TEXT_LG)
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(text::TPL_TITLE.get()),
                    )
                    .child(
                        ui::secondary("bot-tpl-blank", text::TPL_BLANK.get())
                            .icon(IconName::Plus)
                            .debug_selector(|| "bot-tpl-blank".into())
                            .on_click(cx.listener(|this, _, _, cx| this.blank(cx))),
                    )
                    .child(div().h(px(20.)).w(px(1.)).bg(p.border.opacity(0.5)))
                    .child(ui::button_with_tooltip(
                        Button::new("bot-tpl-close")
                            .ghost()
                            .small()
                            .icon(IconName::Close)
                            .debug_selector(|| "bot-tpl-close".into())
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(Event::Close))),
                        "bot-tpl-close",
                        text::TPL_CLOSE.get(),
                    )),
            )
            .child(body);
        ui::modal_backdrop(
            viewport,
            div()
                .id("bot-templates-backdrop")
                .debug_selector(|| "bot-templates-backdrop".into())
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .on_mouse_down(
                    gpui::MouseButton::Left,
                    cx.listener(|_, _, _, cx| cx.emit(Event::Close)),
                )
                .child(ui::modal_focus_scope(card, &self.focus)),
        )
    }
}

impl gpui::Focusable for Templates {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_matches_every_word_across_name_connectors_and_skills() {
        assert_eq!(
            filter_templates(None, "").len(),
            BUILTIN_BOT_TEMPLATES.len()
        );
        let coding = filter_templates(Some(BotTemplateCategory::Coding), "");
        assert!(
            coding
                .iter()
                .all(|t| t.category == BotTemplateCategory::Coding)
        );
        assert!(
            filter_templates(None, "pull request")
                .iter()
                .any(|t| t.id == "pr-review")
        );
        // Connector names count (Orca searches them too).
        let slack = filter_templates(None, "slack");
        assert!(!slack.is_empty());
        assert!(slack.iter().all(|t| {
            t.connectors
                .contains(&krill_proto::bots::ExternalSource::Slack)
                || t.name.to_lowercase().contains("slack")
                || t.description.to_lowercase().contains("slack")
                || t.skills.iter().any(|s| s.contains("slack"))
        }));
        assert!(filter_templates(None, "zzzz-no-such-template").is_empty());
    }

    #[test]
    fn all_builtin_templates_have_korean_translations_and_search() {
        for tpl in BUILTIN_BOT_TEMPLATES {
            assert!(
                template_ko(tpl.id).is_some(),
                "missing Korean translation for template {}",
                tpl.id
            );
            for sched in tpl.schedules {
                assert!(
                    schedule_ko(tpl.id, sched.id).is_some(),
                    "missing Korean schedule translation for {}:{}",
                    tpl.id,
                    sched.id
                );
            }
            for ev in tpl.events {
                assert!(
                    event_ko(tpl.id, ev.id).is_some(),
                    "missing Korean event translation for {}:{}",
                    tpl.id,
                    ev.id
                );
            }
            for skill in tpl.skills {
                assert!(
                    skill_ko(skill).is_some(),
                    "missing Korean skill translation for {skill}"
                );
            }
        }
        assert!(
            filter_templates(None, "이메일")
                .iter()
                .any(|t| t.id == "email-digest")
        );
        assert!(
            filter_templates(None, "버그 조사")
                .iter()
                .any(|t| t.id == "bug-investigation")
        );
    }
}
