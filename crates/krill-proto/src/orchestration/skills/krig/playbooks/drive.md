# Drive Playbook (과학적 구현, 이진 탐색 디버깅, 단일 변인 개선)

## Trigger

구체적 기능 구현 및 리팩터링(`feature`), 버그 재현 및 근본 원인 수정(`bugfix`), 또는 정량 지표 반복 개선(`hillclimb`) 요청 시 `afk` 기어로 실행한다. 완료 후 자동으로 [demock.md](demock.md)와 [verify.md](verify.md)를 연쇄 실행한다.

## Steps

1. **기준 커밋 및 감사 로그 초기화.** 후속 `demock`과 `verify`가 전체 변경 범위를 추적할 수 있도록 git 저장소이고 `.krig/base-sha`가 아직 없을 때 현재 `HEAD`를 `.krig/base-sha`에 기록하며(이전 세션에서 `resume`으로 이어받은 경우 기존 `.krig/base-sha` 보존), `.krig/design-spec.md`가 있으면 설계 기준서로 먼저 읽고 [decision-log.mjs](../scripts/decision-log.mjs)로 `.krig/decisions.tsv`를 초기화한다.

```bash
mkdir -p .krig
[ -s .krig/base-sha ] || git rev-parse HEAD >.krig/base-sha 2>/dev/null || true
node "<SKILL_DIR>/scripts/decision-log.mjs" init .krig/decisions.tsv
```

2. **세부 모드별 루프 실행 (`route.mjs`의 `driveMode` 기준).**
   - **`feature` 모드 (기능 구현 및 리팩터링)**:
     1. 죽은 코드나 불필요한 복잡도를 먼저 덜어낸다(추가 전 감산).
     2. 핵심 데이터 구조를 먼저 정의하고, 각 단계가 독립 검증으로 끝나는 수직 슬라이스 단위로 구현한다.
     3. 사용자 관측 행동만 단언하는 행동 테스트를 통과시킨 뒤 다음 슬라이스로 넘어간다.
   - **`bugfix` 모드 (버그 수정)**:
     1. 실제 발생 표면(CLI, API, UI, 테스트)에서 먼저 버그를 재현한다.
     2. 추측으로 방어문(guard)을 덧대지 않고, 입력부터 증상 지점 사이의 중간 상태를 런타임 계측으로 절반씩 좁혀(이진 탐색) 시스템이 처음 잘못된 근본 원인을 찾는다.
     3. 같은 전제를 공유하는 수정이 2회 연속 실패하면 전제 자체를 의심하고 전제를 직접 테스트한다.
     4. 버그를 증명하는 실패 테스트를 먼저 확인한 뒤 근본 원인을 고친다.
   - **`hillclimb` 모드 (단일 변인 정량 개선)**:
     1. 코드를 건드리기 전에 측정 하네스를 고정하고 기준선 수치, 반복 간 노이즈 폭, 실제 병목 지점(limiter), 에러 카운트 0 여부를 먼저 검증한다.
     2. 가설 1개당 코드 변인 1개만 바꾼다. 측정 하네스는 루프 도중 절대 수정하지 않는다.
     3. 노이즈 폭을 넘어서고 에러 카운트 0을 유지한 개선만 채택(`KEEP`)하며, 미달하거나 복잡도 대비 이득이 없으면 즉시 되돌린다(`REVERT`).
3. **결정 기록 누적.** 매 실험 또는 슬라이스 검증 단위마다 가설, 조치, 실제 측정 근거, 판정(`KEEP`, `REVERT`, `VERIFIED`, `NOT_VERIFIED`, `INCONCLUSIVE`, `NOTE`), 커밋 해시(커밋 전이면 생략)를 기록한다.

```bash
node "<SKILL_DIR>/scripts/decision-log.mjs" append .krig/decisions.tsv \
  --route drive --step "<단계>" --hypothesis "<가설>" --action "<조치>" \
  --evidence "<실측치 또는 테스트 출력>" --verdict KEEP --commit "<SHA>"
```

## Rules

- 가역적 구현 선택지나 실험으로 확인 가능한 동작을 두고 인간에게 질문하며 멈추지 않는다.
- 반복 수정 중에는 변경 대상 모듈/패키지 단위 테스트만 돌리고, 전체 무거운 체크 스위트는 최종 완료 직전 1회 실행한다.
- 대상 프로젝트나 호스트 지침(`AGENTS.md`, `CLAUDE.md`)에 지정된 빌드 산출물 경로(예: 외부 `--target-dir`)와 테스트 실행기 규칙을 최우선으로 따른다.

## Verification

- git 저장소면 `node "<SKILL_DIR>/scripts/decision-log.mjs" verify .krig/decisions.tsv --repo "$PWD"`, 비-git 폴더면 `--repo` 없이 실행해 종료 코드 `0`을 확인한다.
- 곧바로 [demock.md](demock.md)와 [verify.md](verify.md) 후속 단계를 실행해 실경로 정직성과 스펙/표준 부합을 검증한다.
