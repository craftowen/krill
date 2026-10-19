# M10 (Orca 이관·워크벤치 확장) 설계 결정 및 판단 기록

이 문서는 Krill 마일스톤 **M10 Orca 이관·워크벤치 확장**(`MK-172`~`MK-187` / `OWE-726`~`OWE-741` 및 후속 파생 이슈 14건)을 완결하면서 내린 주요 기술 결정과 사용자 피드백 반영 사항을 기록한다.

---

## 1. 주요 기술 결정 사항

### 1.1 `[MK-183 / OWE-737]` 워크스페이스 신뢰 프리셋·브랜치 자동 개명·캐시 타이머 기본값 (`true` 일원화)
- **`workspace_trust_preset_enabled` 기본값 = `true` (Opt-out, 사용자 승인 반영)**
  - 초기 앱 실행(`settings::init` / `settings::reload`) 시 `<state_dir>/workspace-trust-preset.enabled` 플래그를 즉시 동기화해, 1인 로컬 개발 환경에서 별도 설정 없이 새 워크트리가 `~/.claude.json`·`~/.codex/config.toml`·`~/.gemini/antigravity-cli/settings.json` 신뢰 목록에 자동 등록되도록 기본값을 `true`로 설정했다(Settings에서 껐다가 다시 켤 때에는 수정 대상 파일 프리뷰 확인 상자를 제공한다).
- **`auto_rename_branch_from_work` 기본값 = `true` (Opt-out)**
  - 자동 생성된 기본 워크트리 브랜치(`worktree-N`, `claude-N`, 동물 슬러그 등)만 첫 프롬프트 기반 슬러그로 1회 개명하며, 보호 브랜치(`main`·`master`·`dev` 등), 사용자 지정 브랜치명, 원격 추적 브랜치(`@{u}`)가 있는 브랜치는 건드리지 않는다.
- **`prompt_cache_timer_enabled` 기본값 = `true` (Opt-out, 기본 TTL `5m`, 설정에서 `1h` 전환 가능)**
  - Claude `Stop`/`Notification` 훅 수신 시점부터 남은 프롬프트 캐시 유효 시간을 사이드바 카드 칩으로 표시한다.

### 1.2 `[MK-174 / OWE-728]` 저장소 파일(`orca.yaml` / `minikrill.toml`) `archive` 훅 CLI 승인 경로 추가
- `krill worktree rm <path> [--trust-archive <sha256>]` 및 `krill worktree hooks [--trust <sha256>]` 플래그를 추가해 비대화형/CLI 워크플로에서도 해시를 검증·기록하고 `archive` 훅을 실행할 수 있게 했다.

### 1.3 `[MK-184 / OWE-738]` 원격(Remote) 없는 로컬 Git 저장소에서 동기화 버튼 비활성화
- `EditSource::has_remote(&repo)`(`git -C <repo> remote` 비어 있지 않음 확인)를 도입해 `edit::State::has_remote`를 추적하고, 원격이 없는 저장소에서는 `Pull`·`Push`·`Sync`·`Commit & Push` 버튼을 비활성화하며 `"원격 저장소가 설정되지 않았습니다"`(`"No git remote configured"`) 툴팁을 표시한다.

### 1.4 `[MK-180 / OWE-734]` 탭 우클릭 컨텍스트 메뉴의 9색 태그 스와치 단일 행 정렬
- 탭 컨텍스트 메뉴 폭을 `232px`로 넓히고 스와치 행을 줄바꿈 없는 단일 행(`flex`, `justify_between`, `gap_1`)으로 고정해 `색상 없음` + 9색(`Red`~`Gray`) 10개 스와치 전부가 한 줄에 균일하게 렌더링되도록 수정했다.

### 1.5 `[MK-173 / OWE-727, OWE-767, OWE-803, OWE-900]` Orca 마이그레이션 시드 우선순위 및 대규모 워크트리 접기
- `imported-worktrees.json`은 최초 시드 및 보조 메타데이터로만 동작하며, 사용자가 Krill에서 변경한 핀 고정·이름 변경·삭제(`sidebar.bin` 및 `WorktreeStore`)를 폴링 시점에 다시 덮어쓰지 않는다(`OWE-767`).
- macOS `/tmp` ↔ `/private/tmp` 심볼릭 링크 별칭 경로는 정규화(`canonicalize_or_clean`)해 중복 행 생성을 차단한다(`OWE-803`).
- 100개 이상의 이관 워크트리도 동일한 워크트리 카드로 렌더링하되, 유휴 워크트리가 11개를 초과하는 저장소는 기본 접힘(`collapsed`) 상태로 시작한다(`OWE-900`).

### 1.6 `[MK-177 / OWE-731, OWE-730]` 500행 그룹화·정렬 60fps 프레임 예산 최적화
- `render_row_with_menu`의 컨텍스트 메뉴 메타데이터(`TreeMenuMeta`·`SessionMenuMeta`·`HeaderMenuMeta`) 계산을 `.context_menu(...)` 클로저 내부로 지연(lazy)시켜 실제 우클릭 시점에만 부모 후보·자손 수를 계산하도록 변경하고, `rows_with_options`에서 워크트리별 세션 맵(`sessions_by_wt`) 사전 인덱싱 및 `sort_by_cached_key`를 적용해 500행 벤치마크를 **`p50 = 8.7ms`, `p99 = 11.1ms`**(16.67ms 예산 통과)로 개선했다.

---

## 2. 사용자 피드백 반영 완료 항목

1. **`[OWE-737]` `workspace_trust_preset_enabled` 기본값 `true` 전환 완료**: `crates/krill-app/src/settings.rs`의 `Settings::default()` 및 serde 기본값을 `true`로 전환하고 회귀 테스트(`owe737_agent_settings_toggle_auto_rename_and_cache_timer_and_sync_flag_file`)를 갱신·통과시켰다.
2. **Linear-Lite `REA CAD — 경량 AutoCAD 대체제` 프로젝트 삭제 완료**: 해당 프로젝트 소속 이슈 94건(`OWE-283`~`OWE-376`) 전수 취소·아카이브, 마일스톤 12개(`M0`~`M11`) 전수 삭제, 프로젝트(`a20e7107-8a0b-433b-895c-8ddfb9dd1c85`) 취소·아카이브 처리를 완료해 활성 프로젝트 목록에서 완전히 제거했다.
