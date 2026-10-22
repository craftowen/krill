//! 공식 AI 프로바이더·에이전트 브랜드 아이콘 자산.
//!
//! SVG 경로 일부는 Orca(`.ref/orca`, Copyright (c) 2026 Lovecast Inc., MIT License)에서 차용했다.
//! 네트워크 요청이나 외부 이미지 디코더 없이 `gpui` 인라인 벡터(`IconSource::Data`)로 즉시 그린다.

use gpui::{Hsla, ParentElement as _, Styled as _, div, px, rgb};
use gpui_component::{Icon, Sizable as _};
use krill_proto::usage::ProviderKind;

/// Anthropic Claude 공식 스파크 아이콘 SVG (`viewBox="0 0 24 24"`).
/// Source: `.ref/orca/src/renderer/src/components/status-bar/icons.tsx` (MIT License).
pub const CLAUDE_SVG: &[u8] = br#"<svg viewBox="0 0 24 24" xmlns="http://www.w3.org/2000/svg"><path d="M4.709 15.955l4.72-2.647.08-.23-.08-.128H9.2l-.79-.048-2.698-.073-2.339-.097-2.266-.122-.571-.121L0 11.784l.055-.352.48-.321.686.06 1.52.103 2.278.158 1.652.097 2.449.255h.389l.055-.157-.134-.098-.103-.097-2.358-1.596-2.552-1.688-1.336-.972-.724-.491-.364-.462-.158-1.008.656-.722.881.06.225.061.893.686 1.908 1.476 2.491 1.833.365.304.145-.103.019-.073-.164-.274-1.355-2.446-1.446-2.49-.644-1.032-.17-.619a2.97 2.97 0 01-.104-.729L6.283.134 6.696 0l.996.134.42.364.62 1.414 1.002 2.229 1.555 3.03.456.898.243.832.091.255h.158V9.01l.128-1.706.237-2.095.23-2.695.08-.76.376-.91.747-.492.584.28.48.685-.067.444-.286 1.851-.559 2.903-.364 1.942h.212l.243-.242.985-1.306 1.652-2.064.73-.82.85-.904.547-.431h1.033l.76 1.129-.34 1.166-1.064 1.347-.881 1.142-1.264 1.7-.79 1.36.073.11.188-.02 2.856-.606 1.543-.28 1.841-.315.833.388.091.395-.328.807-1.969.486-2.309.462-3.439.813-.042.03.049.061 1.549.146.662.036h1.622l3.02.225.79.522.474.638-.079.485-1.215.62-1.64-.389-3.829-.91-1.312-.329h-.182v.11l1.093 1.068 2.006 1.81 2.509 2.33.127.578-.322.455-.34-.049-2.205-1.657-.851-.747-1.926-1.62h-.128v.17l.444.649 2.345 3.521.122 1.08-.17.353-.608.213-.668-.122-1.374-1.925-1.415-2.167-1.143-1.943-.14.08-.674 7.254-.316.37-.729.28-.607-.461-.322-.747.322-1.476.389-1.924.315-1.53.286-1.9.17-.632-.012-.042-.14.018-1.434 1.967-2.18 2.945-1.726 1.845-.414.164-.717-.37.067-.662.401-.589 2.388-3.036 1.44-1.882.93-1.086-.006-.158h-.055L4.132 18.56l-1.13.146-.487-.456.061-.746.231-.243 1.908-1.312-.006.006z" fill="currentColor" fill-rule="nonzero"/></svg>"#;

/// `OpenAI` / Codex 공식 블로섬 아이콘 SVG (`viewBox="0 0 24 24"`).
/// Source: `.ref/orca/src/renderer/src/components/status-bar/icons.tsx` (MIT License).
pub const OPENAI_SVG: &[u8] = br#"<svg viewBox="0 0 24 24" xmlns="http://www.w3.org/2000/svg"><path fill="currentColor" fill-rule="evenodd" d="M9.205 8.658v-2.26c0-.19.072-.333.238-.428l4.543-2.616c.619-.357 1.356-.523 2.117-.523 2.854 0 4.662 2.212 4.662 4.566 0 .167 0 .357-.024.547l-4.71-2.759a.797.797 0 00-.856 0l-5.97 3.473zm10.609 8.8V12.06c0-.333-.143-.57-.429-.737l-5.97-3.473 1.95-1.118a.433.433 0 01.476 0l4.543 2.617c1.309.76 2.189 2.378 2.189 3.948 0 1.808-1.07 3.473-2.76 4.163zM7.802 12.703l-1.95-1.142c-.167-.095-.239-.238-.239-.428V5.899c0-2.545 1.95-4.472 4.591-4.472 1 0 1.927.333 2.712.928L8.23 5.067c-.285.166-.428.404-.428.737v6.898zM12 15.128l-2.795-1.57v-3.33L12 8.658l2.795 1.57v3.33L12 15.128zm1.796 7.23c-1 0-1.927-.332-2.712-.927l4.686-2.712c.285-.166.428-.404.428-.737v-6.898l1.974 1.142c.167.095.238.238.238.428v5.233c0 2.545-1.974 4.472-4.614 4.472zm-5.637-5.303l-4.544-2.617c-1.308-.761-2.188-2.378-2.188-3.948A4.482 4.482 0 014.21 6.327v5.423c0 .333.143.571.428.738l5.947 3.449-1.95 1.118a.432.432 0 01-.476 0zm-.262 3.9c-2.688 0-4.662-2.021-4.662-4.519 0-.19.024-.38.047-.57l4.686 2.71c.286.167.571.167.856 0l5.97-3.448v2.26c0 .19-.07.333-.237.428l-4.543 2.616c-.619.357-1.356.523-2.117.523zm5.899 2.83a5.947 5.947 0 005.827-4.756C22.287 18.339 24 15.84 24 13.296c0-1.665-.713-3.282-1.998-4.448.119-.5.19-.999.19-1.498 0-3.401-2.759-5.947-5.946-5.947-.642 0-1.26.095-1.88.31A5.962 5.962 0 0010.205 0a5.947 5.947 0 00-5.827 4.757C1.713 5.447 0 7.945 0 10.49c0 1.666.713 3.283 1.998 4.448-.119.5-.19 1-.19 1.499 0 3.401 2.759 5.946 5.946 5.946.642 0 1.26-.095 1.88-.309a5.96 5.96 0 004.162 1.713z"/></svg>"#;

/// Google Antigravity (`agy`) 공식 아치 마크 SVG (`viewBox="0 0 24 24"`).
/// Source geometry adapted from `.ref/orca/src/shared/agent-icons/antigravity.png` (MIT License).
pub const ANTIGRAVITY_SVG: &[u8] = br#"<svg viewBox="0 0 24 24" xmlns="http://www.w3.org/2000/svg"><path fill="currentColor" d="M12 2.2C9.1 2.2 6.8 5.4 4.9 10.5C3.1 15.1 1.9 18.7 1.4 20.3C1.1 21.3 2.1 22.1 3.0 21.4C4.5 20.3 6.5 17.9 8.3 15.9C9.5 14.6 10.7 13.9 12 13.9C13.3 13.9 14.5 14.6 15.7 15.9C17.5 17.9 19.5 20.3 21.0 21.4C21.9 22.1 22.9 21.3 22.6 20.3C22.1 18.7 20.9 15.1 19.1 10.5C17.2 5.4 14.9 2.2 12 2.2Z"/></svg>"#;

/// Google Antigravity (`agy`) 상단 코랄/앰버 크레스트 레이어 SVG (`viewBox="0 0 24 24"`).
pub const ANTIGRAVITY_CREST_SVG: &[u8] = br#"<svg viewBox="0 0 24 24" xmlns="http://www.w3.org/2000/svg"><path fill="currentColor" d="M12 2.2C9.7 2.2 7.8 4.4 6.1 8.0C7.8 6.7 9.8 6.0 12 6.0C14.2 6.0 16.2 6.7 17.9 8.0C16.2 4.4 14.3 2.2 12 2.2Z"/></svg>"#;

/// `OpenCode` 공식 마크 SVG (`viewBox="0 0 512 512"`).
/// Source: `.ref/orca/src/renderer/src/lib/agent-icon-glyphs.tsx` (MIT License).
pub const OPENCODE_SVG: &[u8] = br#"<svg viewBox="0 0 512 512" fill="none" xmlns="http://www.w3.org/2000/svg"><path d="M320 224V352H192V224H320Z" fill="currentColor" fill-opacity="0.28"/><path fill-rule="evenodd" clip-rule="evenodd" d="M384 416H128V96H384V416ZM320 160H192V352H320V160Z" fill="currentColor"/></svg>"#;

/// `OpenCode` 내부 코어 블록 레이어 SVG (`viewBox="0 0 512 512"`).
pub const OPENCODE_CORE_SVG: &[u8] = br#"<svg viewBox="0 0 512 512" fill="none" xmlns="http://www.w3.org/2000/svg"><path d="M320 224V352H192V224H320Z" fill="currentColor"/></svg>"#;

/// Pi 공식 π 마크. Orca `src/renderer/src/lib/agent-icon-glyphs.tsx`의 `PiIcon`
/// (Copyright (c) 2026 Lovecast Inc., MIT License, 원본 `pi.dev/favicon.svg`).
/// 공식 단색 마크이므로 호출자의 테마 전경색으로 그린다.
pub const PI_SVG: &[u8] = br#"<svg viewBox="0 0 800 800" xmlns="http://www.w3.org/2000/svg"><path fill="currentColor" fill-rule="evenodd" d="M165.29 165.29 H517.36 V400 H400 V517.36 H282.65 V634.72 H165.29 Z M282.65 282.65 V400 H400 V282.65 Z"/><path fill="currentColor" d="M517.36 400 H634.72 V634.72 H517.36 Z"/></svg>"#;

/// Cursor 공식 아이소메트릭 큐브·커서 마크 SVG (`viewBox="0 0 24 24"`).
/// Source geometry adapted from `.ref/orca/src/shared/agent-icons/cursor.png` (MIT License).
pub const CURSOR_SVG: &[u8] = br#"<svg viewBox="0 0 24 24" xmlns="http://www.w3.org/2000/svg"><path fill="currentColor" fill-rule="evenodd" d="M12 1.75L20.88 6.88V17.12L12 22.25L3.12 17.12V6.88L12 1.75ZM5.4 8.2L12 12.0V19.6L18.6 8.2H5.4Z"/></svg>"#;

/// Cursor 큐브 상단 패싯 하이라이트 레이어 SVG (`viewBox="0 0 24 24"`).
pub const CURSOR_TOP_SVG: &[u8] = br#"<svg viewBox="0 0 24 24" xmlns="http://www.w3.org/2000/svg"><path fill="currentColor" d="M12 1.75L20.88 6.88L12 12.0L3.12 6.88L12 1.75Z"/></svg>"#;

/// Lucide `trash-2` 아이콘 SVG(ISC License). gpui-component 기본 아이콘 번들에 휴지통이 없어 인라인으로 둔다.
pub const TRASH_SVG: &[u8] = br#"<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" xmlns="http://www.w3.org/2000/svg"><path d="M10 11v6"/><path d="M14 11v6"/><path d="M19 6v14a2 2 0 0 1-2 2H7a2 2 0 0 1-2-2V6"/><path d="M3 6h18"/><path d="M8 6V4a2 2 0 0 1 2-2h4a2 2 0 0 1 2 2v2"/></svg>"#;

/// Lucide `layout-dashboard` 아이콘 SVG(ISC License). 사이드바 '에이전트 대시보드' 항목.
pub const LAYOUT_DASHBOARD_SVG: &[u8] = br#"<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" xmlns="http://www.w3.org/2000/svg"><rect width="7" height="9" x="3" y="3" rx="1"/><rect width="7" height="5" x="14" y="3" rx="1"/><rect width="7" height="9" x="14" y="12" rx="1"/><rect width="7" height="5" x="3" y="16" rx="1"/></svg>"#;

/// Lucide `folder-plus` 아이콘 SVG(ISC License). 사이드바 '프로젝트' 헤더 폴더 추가.
pub const FOLDER_PLUS_SVG: &[u8] = br#"<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" xmlns="http://www.w3.org/2000/svg"><path d="M12 10v6"/><path d="M9 13h6"/><path d="M20 20a2 2 0 0 0 2-2V8a2 2 0 0 0-2-2h-7.9a2 2 0 0 1-1.69-.9L9.6 3.9A2 2 0 0 0 7.93 3H4a2 2 0 0 0-2 2v13a2 2 0 0 0 2 2Z"/></svg>"#;

/// Lucide `crosshair` 아이콘 SVG(ISC License). 사이드바 하단 활성 워크트리 위치로 이동.
pub const CROSSHAIR_SVG: &[u8] = br#"<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" xmlns="http://www.w3.org/2000/svg"><circle cx="12" cy="12" r="10"/><line x1="22" x2="18" y1="12" y2="12"/><line x1="6" x2="2" y1="12" y2="12"/><line x1="12" x2="12" y1="6" y2="2"/><line x1="12" x2="12" y1="22" y2="18"/></svg>"#;

/// Lucide `kanban` 아이콘 SVG(ISC License). 사이드바 하단 보드 뷰 토글.
pub const KANBAN_SVG: &[u8] = br#"<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" xmlns="http://www.w3.org/2000/svg"><path d="M6 5v11"/><path d="M12 5v6"/><path d="M18 5v14"/></svg>"#;

/// Lucide `shield` 아이콘 SVG(ISC License). 사이드바 하단 보안 키보드 입력 토글.
pub const SHIELD_SVG: &[u8] = br#"<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" xmlns="http://www.w3.org/2000/svg"><path d="M20 13c0 5-3.5 7.5-7.66 8.95a1 1 0 0 1-.67-.01C7.5 20.5 4 18 4 13V6a1 1 0 0 1 1-1c2 0 4.5-1.2 6.24-2.72a1.17 1.17 0 0 1 1.52 0C14.51 3.81 17 5 19 5a1 1 0 0 1 1 1z"/></svg>"#;

/// Lucide `arrow-left` 아이콘 SVG(ISC License). 타이틀바 뒤로 이동.
pub const ARROW_LEFT_SVG: &[u8] = br#"<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" xmlns="http://www.w3.org/2000/svg"><path d="m12 19-7-7 7-7"/><path d="M19 12H5"/></svg>"#;

/// Lucide `arrow-right` 아이콘 SVG(ISC License). 타이틀바 앞으로 이동.
pub const ARROW_RIGHT_SVG: &[u8] = br#"<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" xmlns="http://www.w3.org/2000/svg"><path d="M5 12h14"/><path d="m12 5 7 7-7 7"/></svg>"#;

/// Lucide `sliders-horizontal` 아이콘 SVG(ISC License). 사이드바 워크스페이스 옵션·필터 메뉴.
pub const SLIDERS_HORIZONTAL_SVG: &[u8] = br#"<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" xmlns="http://www.w3.org/2000/svg"><line x1="21" x2="14" y1="4" y2="4"/><line x1="10" x2="3" y1="4" y2="4"/><line x1="21" x2="12" y1="12" y2="12"/><line x1="8" x2="3" y1="12" y2="12"/><line x1="21" x2="16" y1="20" y2="20"/><line x1="12" x2="3" y1="20" y2="20"/><line x1="14" x2="14" y1="2" y2="6"/><line x1="8" x2="8" y1="10" y2="14"/><line x1="16" x2="16" y1="18" y2="22"/></svg>"#;

/// Lucide `moon` 아이콘 SVG(ISC License). 사이드바 유휴/수면 워크트리 숨기기 필터 행(OWE-791).
pub const MOON_SVG: &[u8] = br#"<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" xmlns="http://www.w3.org/2000/svg"><path d="M12 3a6 6 0 0 0 9 9 9 9 0 1 1-9-9Z"/></svg>"#;

/// Lucide `git-branch` 아이콘 SVG(ISC License). 사이드바 기본 브랜치 예외 필터 행(OWE-791).
pub const GIT_BRANCH_SVG: &[u8] = br#"<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" xmlns="http://www.w3.org/2000/svg"><line x1="6" x2="6" y1="3" y2="15"/><circle cx="18" cy="6" r="3"/><circle cx="6" cy="18" r="3"/><path d="M18 9a9 9 0 0 1-9 9"/></svg>"#;

/// Lucide `calendar-clock` 아이콘 SVG(ISC License). 사이드바 자동화 워크트리 숨기기 필터 행(OWE-791).
pub const CALENDAR_CLOCK_SVG: &[u8] = br#"<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" xmlns="http://www.w3.org/2000/svg"><path d="M21 7.5V6a2 2 0 0 0-2-2H5a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h3.5"/><path d="M16 2v4"/><path d="M8 2v4"/><path d="M3 10h5"/><path d="M17.5 17.5 16 16.3V14"/><circle cx="16" cy="16" r="6"/></svg>"#;

/// Lucide `square-terminal` 아이콘 SVG(ISC License). 사이드바 CLI 생성 워크트리 숨기기 필터 행(OWE-791).
pub const SQUARE_TERMINAL_SVG: &[u8] = br#"<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" xmlns="http://www.w3.org/2000/svg"><path d="m7 11 2-2-2-2"/><path d="M11 13h4"/><rect width="18" height="18" x="3" y="3" rx="2" ry="2"/></svg>"#;

/// Lucide `rotate-ccw` 아이콘 SVG(ISC License). 사이드바 필터 초기화 액션(OWE-791).
pub const ROTATE_CCW_SVG: &[u8] = br#"<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" xmlns="http://www.w3.org/2000/svg"><path d="M3 12a9 9 0 1 0 9-9 9.75 9.75 0 0 0-6.74 2.74L3 8"/><path d="M3 3v5h5"/></svg>"#;

/// Lucide `pin` 아이콘 SVG(ISC License). 탭 핀 고정 표시 및 우클릭 메뉴 항목(OWE-734).
pub const PIN_SVG: &[u8] = br#"<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" xmlns="http://www.w3.org/2000/svg"><path d="M12 17v5"/><path d="M9 10.76a2 2 0 0 1-1.11 1.79l-1.78.9A2 2 0 0 0 5 15.24V16a1 1 0 0 0 1 1h12a1 1 0 0 0 1-1v-.76a2 2 0 0 0-1.11-1.79l-1.78-.9A2 2 0 0 1 15 10.76V7a1 1 0 0 1 1-1 2 2 0 0 0 0-4H8a2 2 0 0 0 0 4 1 1 0 0 1 1 1z"/></svg>"#;

/// Lucide `pin-off` 아이콘 SVG(ISC License). 탭 핀 고정 해제 우클릭 메뉴 항목(OWE-734).
pub const PIN_OFF_SVG: &[u8] = br#"<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" xmlns="http://www.w3.org/2000/svg"><path d="M12 17v5"/><path d="M15 9.34V7a1 1 0 0 1 1-1 2 2 0 0 0 0-4H7.89"/><path d="m2 2 20 20"/><path d="M9 9v1.76a2 2 0 0 1-1.11 1.79l-1.78.9A2 2 0 0 0 5 15.24V16a1 1 0 0 0 1 1h11"/></svg>"#;

/// Lucide `sparkles` 아이콘 SVG(ISC License). AI 커밋 메시지·PR 설명 자동 생성 버튼.
pub const SPARKLES_SVG: &[u8] = br#"<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" xmlns="http://www.w3.org/2000/svg"><path d="M9.937 15.5A2 2 0 0 0 8.5 14.063l-6.135-1.582a.5.5 0 0 1 0-.962L8.5 9.936A2 2 0 0 0 9.937 8.5l1.582-6.135a.5.5 0 0 1 .963 0L14.063 8.5A2 2 0 0 0 15.5 9.937l6.135 1.581a.5.5 0 0 1 0 .964L15.5 14.063a2 2 0 0 0-1.437 1.437l-1.582 6.135a.5.5 0 0 1-.963 0z"/><path d="M20 3v4"/><path d="M22 5h-4"/><path d="M4 17v2"/><path d="M5 18H3"/></svg>"#;

/// Lucide `terminal` 아이콘 SVG(ISC License). 상태바 활성 세션 수 표시(OWE-788).
pub const TERMINAL_SVG: &[u8] = br#"<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" xmlns="http://www.w3.org/2000/svg"><path d="M12 19h8"/><path d="m4 17 6-6-6-6"/></svg>"#;

/// Lucide `bell` 아이콘 SVG(ISC License). 상태바 Needs You 알림 수 표시(OWE-788).
pub const BELL_SVG: &[u8] = br#"<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" xmlns="http://www.w3.org/2000/svg"><path d="M10.268 21a2 2 0 0 0 3.464 0"/><path d="M3.262 15.326A1 1 0 0 0 4 17h16a1 1 0 0 0 .74-1.673C19.41 13.956 18 12.499 18 8A6 6 0 0 0 6 8c0 4.499-1.411 5.956-2.738 7.326"/></svg>"#;

/// Lucide `workflow` 아이콘 SVG(ISC License). 상태바 오케스트레이션 메뉴(OWE-788).
pub const WORKFLOW_SVG: &[u8] = br#"<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" xmlns="http://www.w3.org/2000/svg"><rect width="8" height="8" x="3" y="3" rx="2"/><path d="M7 11v4a2 2 0 0 0 2 2h4"/><rect width="8" height="8" x="13" y="13" rx="2"/></svg>"#;

/// Lucide `plug` 아이콘 SVG(ISC License). 상태바 감지된 로컬 포트 메뉴(OWE-788).
pub const PLUG_SVG: &[u8] = br#"<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" xmlns="http://www.w3.org/2000/svg"><path d="M12 22v-5"/><path d="M15 8V2"/><path d="M17 8a1 1 0 0 1 1 1v4a4 4 0 0 1-4 4h-4a4 4 0 0 1-4-4V9a1 1 0 0 1 1-1z"/><path d="M9 8V2"/></svg>"#;

/// Lucide `server` 아이콘 SVG(ISC License). 상태바 원격 SSH 호스트 메뉴(OWE-788).
pub const SERVER_SVG: &[u8] = br#"<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" xmlns="http://www.w3.org/2000/svg"><rect width="20" height="8" x="2" y="2" rx="2" ry="2"/><rect width="20" height="8" x="2" y="14" rx="2" ry="2"/><line x1="6" x2="6.01" y1="6" y2="6"/><line x1="6" x2="6.01" y1="18" y2="18"/></svg>"#;

/// Lucide `panels-top-left` 아이콘 SVG(ISC License). 상태바 플로팅 터미널 팝아웃 버튼(OWE-788).
pub const PANELS_TOP_LEFT_SVG: &[u8] = br#"<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" xmlns="http://www.w3.org/2000/svg"><rect width="18" height="18" x="3" y="3" rx="2"/><path d="M3 9h18"/><path d="M9 21V9"/></svg>"#;

/// Lucide `rotate-ccw-clock` 아이콘 SVG(ISC License). 상태바 세션 히스토리 토글 버튼(OWE-788).
pub const HISTORY_SVG: &[u8] = br#"<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" xmlns="http://www.w3.org/2000/svg"><path d="M3 12a9 9 0 1 0 9-9 9.75 9.75 0 0 0-6.74 2.74L3 8"/><path d="M3 3v5h5"/><path d="M12 7v5l4 2"/></svg>"#;

/// Lucide `memory-stick` 아이콘 SVG(ISC License). 상태바 리소스 패널 토글 버튼(OWE-788).
pub const MEMORY_STICK_SVG: &[u8] = br#"<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" xmlns="http://www.w3.org/2000/svg"><path d="M12 12v-2"/><path d="M12 18v-2"/><path d="M16 12v-2"/><path d="M16 18v-2"/><path d="M2 11h1.5"/><path d="M20 18v-2"/><path d="M20.5 11H22"/><path d="M4 18v-2"/><path d="M8 12v-2"/><path d="M8 18v-2"/><rect x="2" y="6" width="20" height="10" rx="2"/></svg>"#;

/// Lucide `triangle-alert` 아이콘 SVG(ISC License). 상태바 경고 배지(설정 점검·기록 경고, OWE-788).
pub const TRIANGLE_ALERT_SVG: &[u8] = br#"<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" xmlns="http://www.w3.org/2000/svg"><path d="m21.73 18-8-14a2 2 0 0 0-3.48 0l-8 14A2 2 0 0 0 4 21h16a2 2 0 0 0 1.73-3"/><path d="M12 9v4"/><path d="M12 17h.01"/></svg>"#;

/// Lucide `circle-arrow-up` 아이콘 SVG(ISC License). 상태바 준비된 업데이트 배지(OWE-788).
pub const CIRCLE_ARROW_UP_SVG: &[u8] = br#"<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" xmlns="http://www.w3.org/2000/svg"><circle cx="12" cy="12" r="10"/><path d="m16 12-4-4-4 4"/><path d="M12 16V8"/></svg>"#;

/// Lucide `pencil` 아이콘 SVG(ISC License). 워크트리 이름 변경 버튼·메뉴 아이콘(OWE-846).
pub const PENCIL_SVG: &[u8] = br#"<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" xmlns="http://www.w3.org/2000/svg"><path d="M21.174 6.812a1 1 0 0 0-3.986-3.987L3.842 16.174a2 2 0 0 0-.5.83l-1.321 4.352a.5.5 0 0 0 .623.622l4.353-1.32a2 2 0 0 0 .83-.497z"/><path d="m15 5 4 4"/></svg>"#;

/// Krill 공식 소버린 코랄 크릴(A1) 모노크롬 마크 SVG (`viewBox="0 0 24 24"`).
/// Source: `packaging/macos/krill-mono.svg` (뾰족한 두흉부·눈·더듬이·3단 아치 등갑·부채꼴 미절).
pub const KRILL_SVG: &[u8] = br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24"><path fill="currentColor" fill-rule="evenodd" d="M20.36 7.07C18.5 5.8 15.02 4.99 12.24 5.91C11.31 6.26 10.73 6.73 10.36 7.43L11.55 10.7C13.86 10.9 16.76 9.97 18.5 8.7C19.2 8.18 19.78 7.65 20.36 7.07ZM15.14 7.42A0.81 0.81 0 1 0 16.76 7.42A0.81 0.81 0 1 0 15.14 7.42ZM18.27 6.49C15.6 3.01 9.22 2.08 3.77 5.91C9.11 2.9 15.26 3.94 17.81 6.73ZM17.69 6.84C15.02 4.52 10.38 4.06 6.32 6.61C10.27 4.75 14.68 5.33 17.23 7.07ZM9.49 7.82A6.84 6.84 0 0 0 6.2 11.75L9.5 12.82A3.36 3.36 0 0 1 11.12 10.89ZM6.4 12.29A6.5 6.5 0 0 0 6.97 16.9L9.48 15.57A3.65 3.65 0 0 1 9.16 12.98ZM7.59 17.06A6.03 6.03 0 0 0 10.84 19.6L11.48 17.61A3.94 3.94 0 0 1 9.35 15.95ZM11.66 18.21Q15.02 16.93 18.97 17.63Q17.11 19.14 12.82 19.48ZM12.82 19.72Q16.76 19.83 18.62 21.92Q14.79 21.92 11.66 19.83Z"/></svg>"#;

// ---- 프로바이더·에이전트 아이콘 크기 토큰 (OWE-875) ----

/// 초소형 아이콘(10px): 사이드바 인라인 배지·트리 화살표 보조 표시.
pub const ICON_2XS: f32 = 10.0;
/// 소형 아이콘(12px): 사이드바 개별 세션 행, 에이전트 그룹 헤더, 필터 칩(AI Vault·Jump).
pub const ICON_XS: f32 = 12.0;
/// 기본 소형 아이콘(13px): 워크트리 카드 에이전트 요약, 탭 바 탭, 상태바 알약, 이어하기 모달 칩.
pub const ICON_SM: f32 = 13.0;
/// 중형 아이콘(14px): 상태바 사용량 팝오버 헤더, 메뉴 항목, 탭 드래그 프리뷰, 최근 탭 전환기, 대시보드 카드, AI Vault·Jump 결과 행, 온보딩·설정 버튼.
pub const ICON_MD: f32 = 14.0;
/// 대형 아이콘(16px): 설정 > 계정 프로바이더 섹션 헤더.
pub const ICON_LG: f32 = 16.0;
/// 특대형 아이콘(18px): 설정 > 에이전트 프로필 카드(32×32 배지 내부).
pub const ICON_XL: f32 = 18.0;

// ---- 프로바이더·에이전트 공식 브랜드 컬러 (OWE-875) ----

/// Krill 공식 리빙 코랄 브랜드 컬러(`#FF5238`). 브랜드 마크 전용이다 — 상태 의미·버튼 채움·포커스 링·선택 배경에 쓰지 않는다
/// (destructive·탭 Red와 색상각이 가까워 색약 사용자가 혼동한다, STYLEGUIDE §6.4).
pub const KRILL_CORAL: u32 = 0x00ff_5238;
/// 라이트 배경용 코랄(`#D63A24`, 흰 배경 4.68:1). `#FF5238`은 흰 배경에서 3.09:1이라 작은 마크가 흐려진다.
pub const KRILL_CORAL_LIGHT: u32 = 0x00d6_3a24;
/// 큰 브랜드 마크 타일(빈 작업 공간 히어로) 남색 바탕(`#08090E`, 앱 아이콘 그라데이션 끝색).
pub const BRAND_TILE_BG: u32 = 0x0008_090e;
/// Claude 공식 브랜드 컬러(`#D97757`, Terracotta).
pub const CLAUDE_COLOR: u32 = 0x00d9_7757;
/// `OpenAI` / Codex 공식 브랜드 컬러(`#10A37F`, Emerald).
pub const CODEX_COLOR: u32 = 0x0010_a37f;
/// Google Antigravity 공식 브랜드 블루(`#4285F4`).
pub const ANTIGRAVITY_BLUE: u32 = 0x0042_85f4;
/// Google Antigravity 상단 크레스트 코랄(`#EA4335`).
pub const ANTIGRAVITY_CORAL: u32 = 0x00ea_4335;
/// `OpenCode` 공식 브랜드 시안(`#06B6D4`).
pub const OPENCODE_COLOR: u32 = 0x0006_b6d4;
/// `OpenCode` 내부 코어 스카이(`#67E8F9`).
pub const OPENCODE_CORE_COLOR: u32 = 0x0067_e8f9;
/// Cursor 공식 브랜드 인디고(`#6366F1`).
pub const CURSOR_COLOR: u32 = 0x0063_66f1;
/// Cursor 큐브 상단 패싯 하이라이트(`#A5B4FC`).
pub const CURSOR_HIGHLIGHT_COLOR: u32 = 0x00a5_b4fc;
/// 사용자 정의 커스텀 에이전트 기본 브랜드 앰버(`#F59E0B`).
pub const CUSTOM_AGENT_COLOR: u32 = 0x00f5_9e0b;

/// 에이전트 ID에 대응하는 공식 SVG 바이트를 돌려준다.
#[must_use]
pub fn agent_svg_bytes(agent_id: &str) -> &'static [u8] {
    match krill_proto::agent::canonical_id(agent_id) {
        "claude" => CLAUDE_SVG,
        "codex" => OPENAI_SVG,
        "agy" => ANTIGRAVITY_SVG,
        "pi" => PI_SVG,
        "cursor" | "cursor-agent" => CURSOR_SVG,
        _ => OPENCODE_SVG,
    }
}

/// 프로바이더 종류에 대응하는 공식 SVG 바이트를 돌려준다.
#[must_use]
pub const fn provider_svg_bytes(provider: ProviderKind) -> &'static [u8] {
    match provider {
        ProviderKind::Claude => CLAUDE_SVG,
        ProviderKind::Codex => OPENAI_SVG,
        // `Gemini`는 제거된 wire tombstone(krill-proto `usage.rs`)이라 빈 값을 유지한다.
        ProviderKind::Gemini => &[],
        ProviderKind::Antigravity => ANTIGRAVITY_SVG,
        ProviderKind::OpenCode => OPENCODE_SVG,
    }
}

/// 프로바이더 대표 브랜드 색을 돌려준다.
#[must_use]
pub fn provider_brand_color(provider: ProviderKind, fg: Hsla) -> Hsla {
    match provider {
        ProviderKind::Claude => rgb(CLAUDE_COLOR).into(),
        ProviderKind::Codex => rgb(CODEX_COLOR).into(),
        // `Gemini`는 제거된 wire tombstone이라 호출자 전경색으로 폴백한다.
        ProviderKind::Gemini => fg,
        ProviderKind::Antigravity => rgb(ANTIGRAVITY_BLUE).into(),
        ProviderKind::OpenCode => rgb(OPENCODE_COLOR).into(),
    }
}

/// 에이전트 ID 대표 브랜드 색을 돌려준다.
#[must_use]
pub fn agent_brand_color(agent_id: &str, fg: Hsla) -> Hsla {
    match krill_proto::agent::canonical_id(agent_id) {
        "claude" => rgb(CLAUDE_COLOR).into(),
        "codex" => rgb(CODEX_COLOR).into(),
        "agy" => rgb(ANTIGRAVITY_BLUE).into(),
        "opencode" => rgb(OPENCODE_COLOR).into(),
        "cursor" | "cursor-agent" => rgb(CURSOR_COLOR).into(),
        "pi" => fg,
        _ => rgb(CUSTOM_AGENT_COLOR).into(),
    }
}

/// 버튼·팝업 메뉴 항목(`Button::icon`, `PopupMenuItem::icon`)에 넣는 브랜드 색상 적용 프로바이더 [`Icon`].
#[must_use]
pub fn provider_menu_icon(provider: ProviderKind) -> Icon {
    let color = provider_brand_color(provider, rgb(CLAUDE_COLOR).into());
    Icon::default()
        .data(provider_svg_bytes(provider))
        .text_color(color)
}

/// 버튼·팝업 메뉴 항목(`Button::icon`, `PopupMenuItem::icon`, `PopupMenu::submenu_with_icon`)에 넣는 브랜드 색상 적용 에이전트 [`Icon`].
#[must_use]
pub fn agent_menu_icon(agent_id: &str) -> Icon {
    let icon = Icon::default().data(agent_svg_bytes(agent_id));
    if agent_id == "pi" {
        icon
    } else {
        icon.text_color(agent_brand_color(agent_id, rgb(CLAUDE_COLOR).into()))
    }
}

/// 프로바이더 공식 로고 아이콘 요소를 만든다(관리자 페이지·상태바 공용).
pub fn provider_icon(provider: ProviderKind, size_px: f32, _fg: Hsla) -> gpui::Div {
    match provider {
        ProviderKind::Claude => svg_box(CLAUDE_SVG, size_px, rgb(CLAUDE_COLOR).into()),
        ProviderKind::Codex => svg_box(OPENAI_SVG, size_px, rgb(CODEX_COLOR).into()),
        // `Gemini`는 제거된 wire tombstone이라 빈 박스를 유지한다.
        ProviderKind::Gemini => div().size(px(size_px)),
        ProviderKind::Antigravity => antigravity_box(size_px),
        ProviderKind::OpenCode => opencode_box(size_px),
    }
}

/// 에이전트 ID에 맞는 공식 브랜드 아이콘 요소를 만든다(관리자 페이지·탭 바·사이드바 공용).
pub fn agent_brand_icon(agent_id: &str, size_px: f32, fg: Hsla) -> gpui::Div {
    match krill_proto::agent::canonical_id(agent_id) {
        "claude" => svg_box(CLAUDE_SVG, size_px, rgb(CLAUDE_COLOR).into()),
        "codex" => svg_box(OPENAI_SVG, size_px, rgb(CODEX_COLOR).into()),
        "agy" => antigravity_box(size_px),
        "opencode" => opencode_box(size_px),
        "cursor" | "cursor-agent" => cursor_box(size_px),
        "pi" => svg_box(PI_SVG, size_px, fg),
        other => {
            let brand: Hsla = rgb(CUSTOM_AGENT_COLOR).into();
            let letter = other
                .chars()
                .next()
                .map_or_else(|| "?".to_owned(), |c| c.to_ascii_uppercase().to_string());
            div()
                .size(px(size_px))
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .rounded(super::theme::RADIUS_XS)
                .bg(brand.opacity(0.14))
                .border_1()
                .border_color(brand.opacity(0.45))
                .text_size(super::theme::TEXT_2XS)
                .text_color(brand)
                .child(letter)
        }
    }
}

/// 인라인 SVG 바이트로 지정 크기·색상의 [`Icon`]을 만든다.
#[must_use]
pub fn svg_icon(bytes: &'static [u8], size_px: f32, color: Hsla) -> Icon {
    Icon::default()
        .data(bytes)
        .with_size(px(size_px))
        .text_color(color)
}

/// 인라인 SVG 바이트를 고정 크기 가운데 정렬 박스로 감싼다.
pub fn svg_box(bytes: &'static [u8], size_px: f32, color: Hsla) -> gpui::Div {
    div()
        .size(px(size_px))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .child(svg_icon(bytes, size_px, color))
}

/// 팔레트 바탕(라이트/다크)에 맞는 코랄. 밝은 바탕이면 [`KRILL_CORAL_LIGHT`], 어두운 바탕이면 [`KRILL_CORAL`].
#[must_use]
pub fn krill_coral(p: &super::Palette) -> Hsla {
    if p.background.l > 0.5 {
        rgb(KRILL_CORAL_LIGHT).into()
    } else {
        rgb(KRILL_CORAL).into()
    }
}

/// Krill 공식 리빙 코랄 브랜드 아이콘 요소를 만든다. 색은 현재 팔레트 바탕에 맞춘다([`krill_coral`]).
pub fn krill_brand_icon(size_px: f32, p: &super::Palette) -> gpui::Div {
    svg_box(KRILL_SVG, size_px, krill_coral(p))
}

/// 큰 히어로용 브랜드 타일: 남색 [`BRAND_TILE_BG`] 둥근 상자(`RADIUS_LG`) 가운데 코랄 마크.
/// `size_px`는 타일 한 변이고 마크는 그 2/3다. 바탕이 늘 남색이라 테마와 무관하게 [`KRILL_CORAL`]을 쓴다.
pub fn brand_mark_tile(size_px: f32) -> gpui::Div {
    div()
        .size(px(size_px))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded(super::theme::RADIUS_LG)
        .bg(rgb(BRAND_TILE_BG))
        .child(svg_icon(
            KRILL_SVG,
            size_px * 2. / 3.,
            rgb(KRILL_CORAL).into(),
        ))
}

fn layered_svg_box(
    base_svg: &'static [u8],
    base_color: Hsla,
    overlay_svg: &'static [u8],
    overlay_color: Hsla,
    size_px: f32,
) -> gpui::Div {
    div()
        .relative()
        .size(px(size_px))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .child(
            Icon::default()
                .data(base_svg)
                .with_size(px(size_px))
                .text_color(base_color),
        )
        .child(
            div()
                .absolute()
                .inset_0()
                .flex()
                .items_center()
                .justify_center()
                .child(
                    Icon::default()
                        .data(overlay_svg)
                        .with_size(px(size_px))
                        .text_color(overlay_color),
                ),
        )
}

fn antigravity_box(size_px: f32) -> gpui::Div {
    layered_svg_box(
        ANTIGRAVITY_SVG,
        rgb(ANTIGRAVITY_BLUE).into(),
        ANTIGRAVITY_CREST_SVG,
        rgb(ANTIGRAVITY_CORAL).into(),
        size_px,
    )
}

fn opencode_box(size_px: f32) -> gpui::Div {
    let core: Hsla = rgb(OPENCODE_CORE_COLOR).into();
    layered_svg_box(
        OPENCODE_SVG,
        rgb(OPENCODE_COLOR).into(),
        OPENCODE_CORE_SVG,
        core.opacity(0.45),
        size_px,
    )
}

fn cursor_box(size_px: f32) -> gpui::Div {
    let top: Hsla = rgb(CURSOR_HIGHLIGHT_COLOR).into();
    layered_svg_box(
        CURSOR_SVG,
        rgb(CURSOR_COLOR).into(),
        CURSOR_TOP_SVG,
        top.opacity(0.55),
        size_px,
    )
}

/// 사이드바 프로젝트(저장소·폴더) 헤더 아이콘(Orca `RepoIcon`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProjectIcon {
    /// 로컬 이미지 파일(저장소 안 파비콘·로고 또는 `<state_dir>/repo-icons/` 캐시).
    Image(std::path::PathBuf),
    /// Orca Lucide 아이콘 이름(`Folder`·`Code2`·`Rocket` 등). 모르는 이름은 폴더로 그린다.
    Lucide(String),
    Emoji(String),
}

/// 프로젝트 배지 색(`#RRGGBB`·`#RGB`)을 색으로 바꾼다. 없거나 형식이 틀리거나
/// Orca 기본 중립색(`#737373`)이면 `fallback`(보통 `muted_foreground`)을 쓴다.
#[must_use]
pub fn project_badge_color(badge_color: Option<&str>, fallback: Hsla) -> Hsla {
    let Some(hex) = badge_color.map(|c| c.trim().trim_start_matches('#')) else {
        return fallback;
    };
    let value = match hex.len() {
        6 => u32::from_str_radix(hex, 16).ok(),
        3 => u32::from_str_radix(hex, 16).ok().map(|v| {
            let (r, g, b) = ((v >> 8) & 0xF, (v >> 4) & 0xF, v & 0xF);
            (r * 17) << 16 | (g * 17) << 8 | (b * 17)
        }),
        _ => None,
    };
    value
        .filter(|&v| v != 0x0073_7373)
        .map_or(fallback, |v| rgb(v).into())
}

/// Orca `getRepoLucideIconOptions`의 이름 → 번들 Lucide 아이콘.
fn project_lucide(name: &str) -> gpui_kit_assets::IconName {
    use gpui_kit_assets::IconName as I;
    match name {
        "Code2" => I::CodeXml,
        "SquareTerminal" => I::SquareTerminal,
        "Bot" => I::Bot,
        "Package" => I::Package,
        "Database" => I::Database,
        "Globe" => I::Globe,
        "Server" => I::Server,
        "Cpu" => I::Cpu,
        "Layers" => I::Layers,
        "Braces" => I::Braces,
        "Rocket" => I::Rocket,
        "Wrench" => I::Wrench,
        "Briefcase" => I::Briefcase,
        "Building2" => I::Building2,
        "Palette" => I::Palette,
        "Gauge" => I::Gauge,
        "Sparkles" => I::Sparkles,
        "Shapes" => I::Shapes,
        "Box" => I::Box,
        _ => I::Folder,
    }
}

/// 프로젝트 헤더 아이콘 박스(Orca `RepoIconGlyph`). 이미지는 디코드 전·실패 시 배지 색 폴더로 대신하고,
/// Lucide·폴더 글리프는 배지 색으로 칠한다.
pub fn project_header_icon(
    icon: Option<&ProjectIcon>,
    badge_color: Option<&str>,
    size_px: f32,
    fallback: Hsla,
) -> gpui::Div {
    use gpui::{IntoElement as _, StyledImage as _};
    let color = project_badge_color(badge_color, fallback);
    let folder = move || {
        Icon::new(gpui_kit_assets::IconName::Folder)
            .with_size(px(size_px))
            .text_color(color)
    };
    let boxed = div()
        .size(px(size_px))
        .flex_none()
        .flex()
        .items_center()
        .justify_center();
    match icon {
        Some(ProjectIcon::Image(path)) => boxed.overflow_hidden().rounded(super::RADIUS_XS).child(
            gpui::img(path.clone())
                .size(px(size_px))
                .object_fit(gpui::ObjectFit::Contain)
                .with_loading(move || folder().into_any_element())
                .with_fallback(move || folder().into_any_element()),
        ),
        Some(ProjectIcon::Emoji(emoji)) => boxed.text_size(super::TEXT_XS).child(emoji.clone()),
        Some(ProjectIcon::Lucide(name)) => boxed.child(
            Icon::new(project_lucide(name))
                .with_size(px(size_px))
                .text_color(color),
        ),
        None => boxed.child(folder()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::InteractiveElement as _;
    use gpui::IntoElement;

    // 크기 토큰 단조성은 컴파일 타임에 보장한다(OWE-875 회귀 방지).
    const _: () = {
        assert!(ICON_XS < ICON_SM && ICON_SM < ICON_MD && ICON_MD < ICON_LG && ICON_LG < ICON_XL);
    };

    #[test]
    fn icon_size_tokens_match_styleguide() {
        assert!((ICON_XS - 12.0).abs() < f32::EPSILON);
        assert!((ICON_SM - 13.0).abs() < f32::EPSILON);
        assert!((ICON_MD - 14.0).abs() < f32::EPSILON);
        assert!((ICON_LG - 16.0).abs() < f32::EPSILON);
        assert!((ICON_XL - 18.0).abs() < f32::EPSILON);
    }

    #[test]
    fn pi_mark_is_shared_by_agent_and_menu_paths_and_keeps_the_theme_foreground() {
        assert_eq!(agent_svg_bytes("pi"), PI_SVG);
        assert_ne!(agent_svg_bytes("pi"), OPENCODE_SVG);
        for p in [&crate::ui::LIGHT, &crate::ui::DARK] {
            assert_eq!(agent_brand_color("pi", p.foreground), p.foreground);
        }
        let svg = std::str::from_utf8(PI_SVG).expect("Pi SVG");
        assert!(svg.contains("fill-rule=\"evenodd\""));
        assert!(svg.contains("M517.36 400 H634.72 V634.72 H517.36 Z"));
    }

    #[test]
    fn all_providers_and_agents_have_chromatic_brand_colors_and_valid_svgs() {
        let fallback = gpui::hsla(0.0, 0.0, 0.5, 1.0);
        for provider in ProviderKind::ALL {
            let bytes = provider_svg_bytes(provider);
            let svg = std::str::from_utf8(bytes).expect("provider SVG must be valid UTF-8");
            assert!(
                svg.starts_with("<svg") && svg.trim_end().ends_with("</svg>"),
                "invalid SVG for {provider:?}"
            );
            let color = provider_brand_color(provider, fallback);
            assert!(
                color.s > 0.35,
                "provider {provider:?} brand color must be chromatic: {color:?}"
            );
            assert!(
                color.l > 0.25 && color.l < 0.80,
                "provider {provider:?} brand color must remain legible on light and dark surfaces: {color:?}"
            );
        }

        for agent in [
            "claude",
            "codex",
            "agy",
            "antigravity",
            "gemini",
            "opencode",
            "cursor",
            "cursor-agent",
            "custom-agent",
            "",
        ] {
            let bytes = agent_svg_bytes(agent);
            let svg = std::str::from_utf8(bytes).expect("agent SVG must be valid UTF-8");
            assert!(
                svg.starts_with("<svg") && svg.trim_end().ends_with("</svg>"),
                "invalid SVG for {agent}"
            );
            let color = agent_brand_color(agent, fallback);
            assert!(
                color.s > 0.35,
                "agent {agent} brand color must be chromatic: {color:?}"
            );
            assert!(
                color.l > 0.25 && color.l < 0.80,
                "agent {agent} brand color must remain legible on light and dark surfaces: {color:?}"
            );
        }
    }

    struct IconGallery;

    /// 갤러리 셀렉터는 `debug_bounds(&'static str)` 규격에 맞춰 정적 문자열로 둔다.
    const GALLERY_PROVIDERS: [(ProviderKind, &str); 4] = [
        (ProviderKind::Claude, "gallery-provider-claude"),
        (ProviderKind::Codex, "gallery-provider-codex"),
        (ProviderKind::Antigravity, "gallery-provider-agy"),
        (ProviderKind::OpenCode, "gallery-provider-opencode"),
    ];

    const GALLERY_AGENTS: [(&str, f32, &str, &str); 7] = [
        (
            "claude",
            ICON_XS,
            "gallery-agent-claude",
            "gallery-menu-claude",
        ),
        (
            "codex",
            ICON_SM,
            "gallery-agent-codex",
            "gallery-menu-codex",
        ),
        ("agy", ICON_MD, "gallery-agent-agy", "gallery-menu-agy"),
        ("pi", ICON_SM, "gallery-agent-pi", "gallery-menu-pi"),
        (
            "opencode",
            ICON_LG,
            "gallery-agent-opencode",
            "gallery-menu-opencode",
        ),
        (
            "cursor",
            ICON_XL,
            "gallery-agent-cursor",
            "gallery-menu-cursor",
        ),
        (
            "custom-bot",
            ICON_SM,
            "gallery-agent-custom-bot",
            "gallery-menu-custom-bot",
        ),
    ];

    impl gpui::Render for IconGallery {
        fn render(
            &mut self,
            _window: &mut gpui::Window,
            cx: &mut gpui::Context<Self>,
        ) -> impl IntoElement {
            let p = crate::ui::palette(cx);
            let mut root = div().flex().flex_col().gap_2();
            for (provider, sel) in GALLERY_PROVIDERS {
                root = root.child(
                    provider_icon(provider, ICON_LG, p.foreground)
                        .id(gpui::SharedString::from(sel))
                        .debug_selector(move || sel.to_owned()),
                );
            }
            for (agent, size, sel, menu_sel) in GALLERY_AGENTS {
                root = root.child(
                    agent_brand_icon(agent, size, p.foreground)
                        .id(gpui::SharedString::from(sel))
                        .debug_selector(move || sel.to_owned()),
                );
                root = root.child(
                    div()
                        .id(gpui::SharedString::from(menu_sel))
                        .debug_selector(move || menu_sel.to_owned())
                        .child(agent_menu_icon(agent)),
                );
            }
            root = root
                .child(
                    krill_brand_icon(ICON_MD, p)
                        .id("gallery-krill-mark")
                        .debug_selector(|| "gallery-krill-mark".to_owned()),
                )
                .child(
                    brand_mark_tile(72.)
                        .id("gallery-krill-tile")
                        .debug_selector(|| "gallery-krill-tile".to_owned()),
                );
            root
        }
    }

    #[gpui::test]
    fn gpui_renders_all_colored_provider_and_agent_icons_across_themes(
        cx: &mut gpui::TestAppContext,
    ) {
        let window = cx.add_window(|_, _| IconGallery);
        let mut vcx = gpui::VisualTestContext::from_window(window.into(), cx);
        vcx.run_until_parked();

        for (provider, sel) in GALLERY_PROVIDERS {
            assert!(
                vcx.debug_bounds(sel).is_some(),
                "missing provider icon bounds for {provider:?}"
            );
        }
        for (_, _, sel, menu_sel) in GALLERY_AGENTS {
            assert!(
                vcx.debug_bounds(sel).is_some(),
                "missing agent icon bounds for {sel}"
            );
            assert!(
                vcx.debug_bounds(menu_sel).is_some(),
                "missing menu icon bounds for {menu_sel}"
            );
        }
        assert!(vcx.debug_bounds("gallery-krill-mark").is_some());
        let tile = vcx.debug_bounds("gallery-krill-tile");
        assert_eq!(tile.map(|b| b.size.width), Some(px(72.)));
    }

    /// 코랄은 라이트 바탕에서 `#D63A24`, 다크 바탕에서 `#FF5238`. 히어로 타일은 늘 남색 위 `#FF5238`.
    #[test]
    fn krill_coral_follows_palette_background() {
        let light: Hsla = rgb(KRILL_CORAL_LIGHT).into();
        let dark: Hsla = rgb(KRILL_CORAL).into();
        assert_eq!(krill_coral(&crate::ui::LIGHT), light);
        assert_eq!(krill_coral(&crate::ui::DARK), dark);
    }

    #[test]
    fn project_badge_color_parses_hex_and_falls_back_on_neutral_or_invalid() {
        let fallback = gpui::hsla(0.2, 0.1, 0.5, 1.0);
        for raw in [
            None,
            Some(""),
            Some("#737373"),
            Some("737373"),
            Some("nope"),
        ] {
            assert_eq!(project_badge_color(raw, fallback), fallback, "{raw:?}");
        }
        let red = project_badge_color(Some("#ef4444"), fallback);
        assert_ne!(red, fallback);
        assert_eq!(
            project_badge_color(Some("#38f"), fallback),
            project_badge_color(Some("#3388ff"), fallback)
        );
        assert_eq!(project_lucide("Rocket"), gpui_kit_assets::IconName::Rocket);
        assert_eq!(project_lucide("Unknown"), gpui_kit_assets::IconName::Folder);
    }
}
