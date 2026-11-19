# Resume Playbook (세션 복구, 드리프트 진단, 안전 중단 체크포인트)

## Trigger

이전 세션이나 다른 작업자로부터 넘어온 작업을 이어받을 때(`pickup`), 진행 현황을 재구성할 때, 또는 작업을 잠시 멈추며 무손실 인계 기록을 남길 때(`pause`) `afk` 기어로 실행한다. 이어서 구현 마무리까지 요청된 경우에는 복구 진단 직후 [drive.md](drive.md)로 넘어간다.

## Steps

1. **현장 상태 및 드리프트 진단.** 코드를 한 줄도 고치기 전에 [checkpoint.mjs](../scripts/checkpoint.mjs)로 `.krig/handoff.json`, `.krig/base-sha`, `.krig/decisions.tsv`, `.krig/verified.json`, `git status`, 커밋 이력을 교차 대조한다.

```bash
node "<SKILL_DIR>/scripts/checkpoint.mjs" status --json --repo "$PWD"
```

2. **완료 구간과 미완료 구간 분리.**
   - `commitsSinceBase`와 `decisions.tsv`의 `KEEP`/`VERIFIED` 행을 대조해 이미 검증된 작업은 재실행하지 않는다.
   - `driftFlags`에 `PATCH_DRIFT_FROM_HANDOFF`, `HEAD_ADVANCED`, `HEAD_REWRITTEN`, `STALE_VERIFICATION`, `ORPHAN_DECISION_COMMITS`, `STAGED_KRIG_ARTIFACT`가 있으면 작업 트리의 실제 diff(`git diff`)와 스테이징 상태를 먼저 읽고 기존 검증이 무효화된 구간만 재검증 대상으로 잡는다.
   - 미커밋 수정분(`gitState.worktree.dirtyFiles`)이 있으면 함부로 `git checkout`이나 `git reset`으로 날리지 않고 의도된 진행 중 작업인지 확인한다.
3. **재개 실행 또는 안전 중단(`save`) 분기.**
   - **작업 재개 시**: `recommendedAction` 판정에 따라 분기한다.
     - `RESOLVE_BLOCKER`: `.krig/handoff.json`의 `blockers` 항목이 해소되었는지 먼저 확인하고, 미해소 시 차단 사유를 보고한다.
     - `READY_TO_SHIP`: 검증 스탬프가 신선하므로 출하 준비 완료를 보고하거나 [ship.md](ship.md)로 이어간다.
     - `NEEDS_VERIFY`: 코드 변경분 대비 검증 스탬프가 없거나 무효화되었으므로 [verify.md](verify.md)를 실행한다.
     - `CONTINUE_DRIVE`: 미커밋 변경이나 남은 다음 작업(`handoff.next`)부터 [drive.md](drive.md)를 이어서 실행한다.
     - `CLEAN_START`: 진행 중이던 미완료 변경이나 인계 체크포인트가 없으므로 요청된 첫 단계부터 새로 시작한다.
   - **안전 중단/인계 시**: 현재까지 완료한 항목(`--done`), 다음에 바로 실행할 첫 작업(`--next`), 외부 승인이나 자격증명 대기 같은 차단 사유(`--blockers`)를 `.krig/handoff.json`에 저장한다.

```bash
node "<SKILL_DIR>/scripts/checkpoint.mjs" save --repo "$PWD" \
  --route resume --summary "<현재 상태 요약>" \
  --done "<검증 완료 항목1|항목2>" --next "<다음 첫 작업>"
```

## Rules

- 복구 과정에서 삭제, 결제, 외부 발송, 프로덕션 배포 같은 비가역 명령을 실제 반영 여부 확인 없이 재실행하지 않는다.
- 작업 트리에 본인이 만들지 않은 미커밋 변경분이 있을 때 사용자 확인 없이 폐기하지 않는다.

## Verification

- `checkpoint.mjs status --repo "$PWD"` 종료 코드 `0`과 `recommendedAction` 및 `driftFlags` 판정을 확인한다.
- 중단 인계 요청인 경우 `.krig/handoff.json`이 생성되고 현재 `patchId`와 일치하는지 확인한다.
