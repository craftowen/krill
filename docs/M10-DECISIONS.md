# M10 (Orca 이관·워크벤치 확장) 설계 결정 및 판단 기록

이 문서는 Krill 마일스톤 **M10 Orca 이관·워크벤치 확장**(`MK-172`~`MK-187` / `OWE-726`~`OWE-741` 및 후속 이슈)을 완결하면서 내린 주요 자율 판단과, 사용자(Owen)의 취향에 따라 조정될 수 있는 경계 판단 사항을 기록한다.

---

## 1. 주요 자율 결정 사항 (최적 판단 적용 완료)

### 1.1 `[MK-183 / OWE-737]` 워크스페이스 신뢰 프리셋·브랜치 자동 개명·캐시 타이머 기본값
- **`workspace_trust_preset_enabled` 기본값 = `false` (Opt-in + 변경 경로 프리뷰 동의 모달)**
  - **판단 근거**: 활성화 시 사용자의 홈 디렉터리 전역 설정 파일(`~/.claude.json`, `~/.codex/config.toml`, `~/.gemini/trustedFolders.json`)에 새 워크트리 경로를 신뢰 목록으로 주입한다. 외부 공개 저장소 클론 시 사용자 동의 없이 에이전트 권한 프롬프트가 우회되는 보안 위험을 막기 위해 기본값은 `false`로 두고, Settings에서 켤 때 수정 대상 파일 3종의 경로와 동작을 명시한 동의 모달(`WorkspaceTrustConsentDialog`)을 거치도록 유지했다.
- **`auto_rename_branch_from_work` 기본값 = `true` (Opt-out)**
  - **판단 근거**: 자동 생성된 기본 워크트리 브랜치(`worktree-N`, `claude-N`, 동물 슬러그 등)만 첫 프롬프트 기반 슬러그로 1회 개명하며, 보호 브랜치(`main`·`master`·`dev` 등), 사용자가 직접 지정한 브랜치명, 이미 원격 추적 브랜치(`@{u}`)가 설정된 브랜치는 절대 건드리지 않는다. 부작용 없이 워크트리 식별성을 높여 기본 활성화(`true`)가 최적이다.
- **`prompt_cache_timer_enabled` 기본값 = `true` (Opt-out, 기본 TTL `5m`, 설정에서 `1h` 전환 가능)**
  - **판단 근거**: Claude `Stop`/`Notification` 훅 수신 시점부터 남은 프롬프트 캐시 유효 시간을 사이드바 카드 칩으로만 표시하는 순수 읽기 전용 시각 보조 기능이다. 성능·보안 부작용이 없으므로 기본 활성화(`true`)로 두었다.

### 1.2 `[MK-174 / OWE-728]` 저장소 파일(`orca.yaml` / `minikrill.toml`) `archive` 훅 CLI 승인 경로 추가
- **현상**: 저장소 내부 설정 파일(`orca.yaml` 또는 `minikrill.toml`)에 정의된 `archive` 훅은 원격 저장소 클론을 통한 임의 명령 실행을 막기 위해 `<state_dir>/setup-trust`에 `(repo_path, script)` SHA-256 해시가 등록된 경우에만 실행된다. 그러나 기존 CLI에는 `setup` 훅 승인 옵션(`krill worktree new --trust-setup <hash>`)만 있고 `archive` 훅을 CLI에서 승인하는 플래그가 없었다.
- **결정 및 조치**:
  - `krill worktree rm <path> [--trust-archive <sha256>]` 및 `krill worktree hooks [--trust <sha256>]` 플래그를 추가해 비대화형/CLI 워크플로에서도 해시를 검증·기록하고 `archive` 훅을 실행할 수 있게 했다.
  - 데몬 에러 메시지에도 `--trust-archive <hash>` 안내를 함께 표기했다.

### 1.3 `[MK-184 / OWE-738]` 원격(Remote) 없는 로컬 Git 저장소에서 동기화 버튼 비활성화
- **현상**: `FEAT_GIT_SYNC`가 협상된 데몬에 연결되면, 원격(`git remote`)이 하나도 설정되지 않은 순수 로컬 저장소에서도 우측 Git 패널의 `풀(Pull)`·`푸시(Push)`·`동기화(Sync)`·`커밋 및 푸시(Commit & Push)` 버튼이 활성 상태로 표시되어 클릭 시 불필요한 원격 에러가 발생했다.
- **결정 및 조치**:
  - `EditSource::has_remote(&repo)`(`git -C <repo> remote` 비어 있지 않음 확인)를 도입해 `edit::State::has_remote`를 추적하고, 원격이 없는 저장소에서는 `Pull`·`Push`·`Sync`·`Commit & Push` 버튼을 즉시 비활성화하며 `"원격 저장소가 설정되지 않았습니다"`(`"No git remote configured"`) 툴팁을 표시하도록 수정했다. 로컬 `스테이징`·`스테이징 해제`·`변경 취소`·`커밋`·`AI 커밋 메시지 생성`은 원격 유무와 무관하게 항상 동작한다.

### 1.4 `[MK-180 / OWE-734]` 탭 우클릭 컨텍스트 메뉴의 9색 태그 스와치 단일 행 정렬
- **현상**: 탭 컨텍스트 메뉴 폭(`224px`)에서 `색상 없음` + 9색(`Red`~`Gray`) 총 10개 원형 스와치(`16px`, 간격 `6px`)가 배치될 때 마지막 10번째(`Gray`) 스와치가 둘째 줄로 줄바꿈되었다.
- **결정 및 조치**: 메뉴 폭을 `232px`로 넓히고 스와치 행을 줄바꿈 없는 단일 행(`flex`, `justify_between`, `gap_1`)으로 고정해 10개 스와치 전부가 한 줄에 균일하게 렌더링되도록 수정하고 `gpui::test`로 좌표를 검증했다.

### 1.5 `[MK-173 / OWE-727, OWE-767, OWE-803, OWE-900]` Orca 마이그레이션 시드 우선순위 및 대규모 워크트리 접기
- **결정**:
  - `imported-worktrees.json`은 최초 시드 및 보조 메타데이터로만 동작하며, 사용자가 Krill에서 변경한 핀 고정·이름 변경·삭제(`sidebar.bin` 및 `WorktreeStore`)를 폴링 시점에 다시 덮어쓰지 않는다(`OWE-767`).
  - macOS `/tmp` ↔ `/private/tmp` 심볼릭 링크 별칭 경로는 정규화(`canonicalize_or_clean`)해 중복 행 생성을 차단한다(`OWE-803`).
  - 100개 이상의 이관 워크트리도 축약 텍스트가 아닌 동일한 워크트리 카드로 렌더링하되, 유휴 워크트리가 11개를 초과하는 저장소는 기본 접힘(`collapsed`) 상태로 시작해 첫 화면 가독성과 60fps 성능을 함께 보장한다(`OWE-900`).

### 1.6 `[MK-177 / OWE-731, OWE-730]` 500행 그룹화·정렬 60fps 프레임 예산 최적화
- **현상**: `OWE-731`의 완료 기준(`500행 기준 60fps 유지`, `p99 < 16.67ms`)을 검증하는 벤치마크(`owe731_grouped_sorted_500_rows_60fps_budget`) 실행 결과, `render_row_with_menu`가 매 프레임 보이는 모든 워크트리 행마다 우클릭 컨텍스트 메뉴용 `eligible_parent_worktrees`·`lineage_descendants`·후보 `self.name(&cand)`를 즉시(eager) 계산해 프레임당 `O(가시 행 × 전체 워크트리 × 전체 세션)` 순회가 발생(`p50 = 217.8ms`, `p99 = 303.8ms`)하는 병목을 발견했다.
- **결정 및 조치**:
  - `render_row_with_menu`의 컨텍스트 메뉴 메타데이터(`TreeMenuMeta`·`SessionMenuMeta`·`HeaderMenuMeta`) 계산을 `.context_menu(...)` 클로저 내부로 지연(lazy)시켜, 실제 우클릭 시점에만 부모 후보·자손 수를 계산하도록 변경했다.
  - `rows_with_options`에서 워크트리별 세션 맵(`sessions_by_wt`)을 1회 사전 인덱싱하고 `sort_worktrees`에 `sort_by_cached_key`를 적용해 정렬·행 구성 비용을 `O(N × S)`에서 `O(N log N)`으로 줄였다.
  - `lineage_children`·`lineage_descendants`(`parents` 비어 있을 때 조기 반환), `worktree_auto_title`(`auto_titles` 비어 있을 때 조기 반환), `ports_for`(`titles` 비어 있을 때 조기 반환), `render_worktree`(`tree_sessions` 1회 스캔 재사용)를 최적화해 500행 벤치마크를 **`p50 = 8.7ms`, `p99 = 11.1ms`**(디버그 빌드 기준 27배 단축, 16.67ms 예산 통과)로 개선했다.

---

## 2. 판단이 애매하거나 사용자 확인이 유용한 항목 (보고용)

1. **`[OWE-737]` 워크스페이스 신뢰 프리셋(`workspace_trust_preset_enabled`)의 기본값 (`false` 유지 vs `true` 전환)**
   - 현재 보안 원칙에 따라 기본값을 `false`(Settings에서 토글 시 동의 모달 표시)로 유지했다. 만약 1인 로컬 개발 환경에서 새 워크트리를 만들 때마다 별도 설정 없이 즉시 `~/.claude.json` 등에 신뢰 경로가 등록되기를 원한다면 `crates/krill-app/src/settings.rs`의 기본값을 `true`로 바꿀 수 있다.
2. **Linear-Lite 내 타 프로젝트(`REA CAD`)의 동명 마일스톤(`M10 성능·품질·배포`) 제외**
   - Linear-Lite 조회 시 `Krill` 프로젝트의 `M10 Orca 이관·워크벤치 확장` 외에 `REA CAD — 경량 AutoCAD 대체제` 프로젝트에도 `M10 성능·품질·배포`(`OWE-344`~`OWE-349`)가 존재함을 확인했다. 현재 작업 워크스페이스가 `minikrill`(`Krill`)이고 `REA CAD`는 `M1`~`M11` 전체가 미착수 백로그 상태인 별도 저장소 프로젝트이므로, 본 작업에서는 `Krill`의 M10 본체 및 파생 이슈 전체를 완결 대상으로 처리했다.
