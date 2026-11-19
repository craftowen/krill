# Ship Playbook (패치 신선도 출하 게이트 및 1분 PR 브리핑)

## Trigger

구현과 검증을 마친 변경분을 커밋으로 정리해 PR을 열거나, 병합 및 릴리스 직전 최종 출하 준비 상태를 점검할 때 `afk` 기어로 실행한다.

## Steps

1. **패치 신선도 및 작업 트리 위생 검증.** [ship-check.mjs](../scripts/ship-check.mjs)로 (a) `.krig/` 임시 파일의 git 인덱스 유출 여부, (b) 병합 충돌 마커 및 하드코딩 시크릿 유입 여부, (c) 마지막 [verify.md](verify.md) 통과 시점의 `git patch-id --stable` 스탬프(`.krig/verified.json`)와 현재 패치의 일치 여부를 검사한다.

```bash
node "<SKILL_DIR>/scripts/ship-check.mjs" verify --repo "$PWD" --require-verified
```

2. **검증 누락 또는 패치 드리프트 해소.**
   - `MISSING_VERIFICATION_STAMP` 또는 `STALE_VERIFICATION`(검증 이후 한 줄이라도 추가 수정됨)이 보고되면 출하를 멈추고 [demock.md](demock.md) 및 [verify.md](verify.md)를 먼저 통과시켜 새 스탬프를 발급받는다.
   - 로컬 커밋 정리가 필요하면 변경 파일만 선별해 논리적 단위로 커밋한 뒤 `--require-clean`을 붙여 작업 트리 청결까지 재확인한다.

```bash
node "<SKILL_DIR>/scripts/ship-check.mjs" verify --repo "$PWD" --require-clean --require-verified
```

3. **40줄 이내 1분 PR 브리핑 작성 및 규격 검증.** PR 생성이나 출하 요약이 필요한 경우 리뷰어가 1분 안에 읽을 수 있도록 Conventional Commit 제목(`type(scope): subject`, 마침표 없음, 72자 이내)과 5개 필수 섹션(`## Why`, `## What changed`, `## Scope`, `## Blast Radius`, `## Verification`)으로 구성된 40줄 이내 본문을 작성해 `ship-check.mjs brief`로 검증한다.

```bash
node "<SKILL_DIR>/scripts/ship-check.mjs" brief --title "<type(scope): subject>" --stdin <<'EOF'
## Why
<문제와 접근법 1~2문장>

## What changed
- <핵심 변경 심볼 또는 경계 1~3개>

## Scope
- <이번 변경에 포함한 범위와 의도적으로 제외한 후속 항목>

## Blast Radius
<영향받는 호출 경계와 안전한 근거 1~2문장>

## Verification
- <실제 실행한 검증 명령과 관측 결과>
EOF
```

4. **원격 반영 경계 준수.** 사용자가 명시적으로 푸시나 PR 생성을 지시한 경우에만 저장소 호스트 도구(`gh pr create` 등, 초안이 아닌 즉시 리뷰 가능 상태)로 제출하고, 로컬 점검만 요청한 경우에는 검증된 커밋과 브리핑 본문을 준비해 보고한다.

## Rules

- `verify` 완료 뒤에 "사소한 주석이나 포맷 수정"이라도 들어갔다면 이전 검증 결과를 유효하다고 주장하지 않고 반드시 패치 스탬프를 갱신한다(단, 패치 내용이 동일한 순수 커밋이나 순수 리베이스는 `git patch-id --stable`과 바이너리/파일 모드 다이제스트가 동일하게 유지된다).
- 출하 및 병합이 완전히 끝난 동일 체크아웃에서 무관한 새 작업을 시작할 때는 이전 작업의 `.krig/base-sha`, `.krig/decisions.tsv`, `.krig/verified.json`, `.krig/handoff.json`을 정리해 새 기준 커밋(`HEAD`)으로 시작한다.
- PR 본문에 40자리 전체 커밋 해시 나열, 에이전트 내부 시행착오 일지, 파일별 단순 변경 목록을 적지 않는다.
- 공유 브랜치 강제 푸시(`git push --force`)나 프로덕션 배포는 사용자 사전 확인 없이 실행하지 않는다.

## Verification

- `ship-check.mjs verify --repo "$PWD" --require-verified`(커밋 완료 상태면 `--require-clean` 포함) 종료 코드 `0`을 확인한다.
- PR 브리핑 작성 대상이면 `ship-check.mjs brief` 종료 코드 `0`을 확인한다.
