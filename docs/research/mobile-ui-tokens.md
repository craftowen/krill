# 모바일 UI 토큰 대응

모바일 PWA도 [STYLEGUIDE](../STYLEGUIDE.md)의 역할과 상태 우선순위를 따른다. 구현은 `crates/krilld/src/mobile/assets/app.css`, 한국어/영어 UI 문구는 같은 디렉터리의 `i18n.js`에 모은다. 터미널 셀·에이전트 대화·저장소 이름은 번역하거나 UI 확대 비율로 재작성하지 않는다.

| 모바일 CSS | Krill 역할 | 다크 / 라이트 |
|---|---|---|
| `--surface-base`, `--foreground` | background / foreground | #0a0a0a·#fafafa / #ffffff·#0a0a0a |
| `--surface-panel` | card / popover | #171717 / #ffffff |
| `--surface-raised` | secondary / muted | #262626 / #f5f5f5 |
| `--border` | border | white 8% / #e5e5e5 |
| `--editor-surface` | editor_surface | #1e1e1e / #ffffff |
| `--status-done`, `--status-connected` | success | #86efac / #15803d |
| `--radius-row`, `--radius-card` | 작은 제어 / 기본 카드 반경 | 6px / 10px |
| `--ui-scale` | UI 글자 확대 | 설정 1·1.25·1.5 배율 |
| `--touch-target` | 모바일 조작 영역 | 44 CSS px, 데스크톱 행 높이와 구분 |

터미널 팔레트는 터미널 콘텐츠 전용 토큰을 유지한다. UI 모드는 시스템/라이트/다크를 선택하고, 문구는 시스템/한국어/영어를 선택한다. 44px는 이번 모바일 조작 영역의 설계값이며 표준 적합성 인증이나 오탭률 측정값이 아니다. 폰의 노치/키보드 여백과 작은 화면에서의 반경·크기는 CSS 토큰으로 관리한다.
