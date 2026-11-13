# Verify Playbook (2축 병렬 리뷰, 백지 콜드 리드, 실행 재현 검증)

## Trigger

`design`, `model`, `drive`, `demock`의 최종 검증 관문으로 자동 실행되거나, 코드 리뷰, PR 검수, 설계 콜드 리드, 벤치마크 누수 감사 요청 시 `afk` 기어로 실행한다.

## Steps

1. **검증 범위 및 규모 결정.**
   - `.krig/base-sha`가 있으면 `git diff "$(cat .krig/base-sha)"`(신규 비추적 파일 포함)를 검증 범위로 잡고, 단독 호출이면 작업 트리 `git diff`, 브랜치 분기점 비교 `git diff <base>...HEAD`, 또는 사용자가 지정한 대상 파일/설계 문서를 범위로 잡는다.
   - 한두 줄의 자명한 오탈자나 설정 수정은 직접 테스트 및 빌드 실행 확인으로 갈음하고, 모듈 경계 변경이나 다중 파일 수정, 신규 설계, 성능 개선은 아래 2축 병렬 리뷰와 실행 검증을 수행한다.
2. **2축 독립 서브에이전트 병렬 리뷰.** 서로 컨텍스트를 공유하지 않는 두 개의 읽기 전용 서브에이전트를 병렬로 띄운다(`agy`에서는 둘 다 `TypeName: "research"`, `Model: "inherit"` 필수).
   - **Standards 리뷰어**: 검증 대상 변경분과 [standards-baseline.md](../references/standards-baseline.md)(및 대상 저장소의 `AGENTS.md`)만 읽고 경계 검증 위반과 Fowler 코드 스멜 12종 유입 여부를 `파일:줄` 근거로 보고한다.
   - **Spec 리뷰어**: 원본 사용자 요청/티켓 수용 기준(선행 설계가 있으면 `.krig/design-spec.md` 포함)과 검증 대상 변경분만 읽고 누락된 요구사항, 범위 초과 구현, 엣지케이스 결함을 `파일:줄` 근거로 보고한다.
3. **백지 콜드 리드 (문서/설계/복잡한 인터페이스 대상).** 작성 배경과 의도를 숨긴 채 산출물 본문만 새 읽기 전용 서브에이전트에 넘기고, 무엇을 하는 모듈/문서인지 요약하게 한 뒤 추측해야만 했던 모호한 구멍을 받아 본문을 교정한다.
4. **벤치마크 및 평가 누수 감사 (성능/평가 과제 한정).** 수치 개선 주장이 포함된 작업은 [leakage-8.md](../references/leakage-8.md)의 8대 항목을 대조해 평가셋 오염, 하네스 동시 변경, 노이즈 이내 착시를 걸러낸다.
5. **다수결 배제 실행 재현 판정.** 서브에이전트 몇 명이 지적했는지(투표수)로 채택하지 않는다. 지적된 결함마다 실제 명령 실행, 재현 입력, 또는 해당 코드 라인의 결정론적 추적 근거로 참(`VERIFIED`)이 입증된 항목만 수정하고, 오탐은 구체적 기각 근거를 남긴다.
6. **패치 검증 스탬프 발급.** git 저장소에 변경분(`git diff` 또는 신규 파일)이 있고 프로젝트 테스트와 정적 검사가 모두 통과했으면, 서브에이전트 리뷰 생략 여부와 무관하게 반드시 [ship-check.mjs](../scripts/ship-check.mjs)로 `.krig/verified.json` 스탬프를 발급한다.

```bash
node "<SKILL_DIR>/scripts/ship-check.mjs" stamp --repo "$PWD" --evidence "<실행한 테스트 및 스캔 통과 요약>"
```

## Rules

- 두 축(Standards, Spec)의 결과를 뭉뚱그려 평균 점수나 모호한 총평으로 합치지 않고 검증된 결함과 기각 사유를 분리해 정리한다.
- 컴파일 성공이나 정적 타입 통과만으로 동작 완료를 선언하지 않고 실제 산출물(CLI 실행, 테스트, DB 조회, 엔드포인트 응답)을 직접 관측한다.

## Verification

- 수정 반영 후 대상 프로젝트의 정적 게이트와 테스트 스위트를 재실행해 종료 코드 `0`과 실제 출력을 확인한다.
- git 작업 트리에서 변경분 검증이 모두 통과했으면 후속 [ship.md](ship.md) 출하 게이트가 패치 신선도를 대조할 수 있도록 [ship-check.mjs](../scripts/ship-check.mjs)로 `git patch-id` 스탬프(`node "<SKILL_DIR>/scripts/ship-check.mjs" stamp --repo "$PWD" --evidence "<검증 결과 요약>"`, `.krig/base-sha`가 없는 기커밋 브랜치면 `--base <기준커밋>` 추가)를 기록한다.
- 각 핵심 검증 항목의 최종 판정을 `VERIFIED`, `NOT_VERIFIED`, `INCONCLUSIVE`(미확인 사유 포함)로 명확히 구분한다.
