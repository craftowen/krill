# Model Playbook (유한 상태 공간 BFS 검증 및 반례 재현)

## Trigger

재시도, 체크포인트 복구, 작업 임대(lease), 결제 상태 전이, 동시성 프로토콜처럼 다단계 인터리빙에서 교착 상태(데드락)나 불변식 깨짐이 발생할 수 있는 로직을 설계하거나 수정, 전수 검증할 때 `afk` 기어로 실행한다. 완료 후 구현이 함께 요청된 경우 [drive.md](drive.md)로, 검증 전용인 경우 [verify.md](verify.md)로 이어진다.

## Steps

1. **유한 상태 공간 모델 추출.** 검증 대상 프로토콜의 상태 변수(유한 도메인 값으로 추상화한 핵심 필드, 미설정 값은 `undefined` 대신 `null` 사용), 초기 상태(`initial`), 가드와 비결정적 장애 분기를 포함한 원자적 전이(`actions`), 비어 있지 않은 종단 조건(`terminal`), 안전 불변식(`invariants`), 공허한 통과를 막는 필수 도달 목표(`mustReach`)를 `.krig/models/<이름>.json` 또는 `.mjs`로 작성한다.
   - **지원 조건 연산자(`when` / `invariants` / `mustReach` / `terminal`)**: 리터럴 동치, `$eq`, `$ne`, `$lt`, `$lte`, `$gt`, `$gte`, `$in`, `$nin`, 상태 내 다른 경로 참조(`$eqRef`, `$neRef`, `$ltRef`, `$lteRef`, `$gtRef`, `$gteRef`), 논리 결합(`$and`, `$or`, `$not`, `$implies: [전제, 결론]`).
   - **지원 상태 변경 연산자(`set`)**: 리터럴 대입, `$inc`, `$dec`, `$ref`(타 경로 값 복사), `$toggle`, `$append`. 복잡한 순수 전이는 `.mjs` 파일에서 `export default { initial, terminal, actions: [{ name, guard: (s) => ..., apply: (s) => nextOrArray }], invariants, mustReach }`로 작성한다.

```json
{
  "initial": { "phase": "queued", "delivered": 0, "checkpointed": false },
  "terminal": [{ "phase": "done" }, { "phase": "quarantined" }],
  "actions": [
    {
      "name": "deliver_and_crash_or_ack",
      "when": { "phase": "queued" },
      "branches": [
        { "label": "ack", "set": { "phase": "done", "delivered": { "$inc": 1 }, "checkpointed": true } },
        { "label": "crash_in_flight", "set": { "phase": "ambiguous", "delivered": { "$inc": 1 } } }
      ]
    },
    {
      "name": "quarantine_uncertain_boot",
      "when": { "phase": "ambiguous", "checkpointed": false },
      "set": { "phase": "quarantined" }
    }
  ],
  "invariants": {
    "AtMostOnceConfirmedDelivery": { "delivered": { "$lte": 1 } }
  },
  "mustReach": {
    "CanCompleteCleanly": { "phase": "done" },
    "CanQuarantineAmbiguousCrash": { "phase": "quarantined" }
  }
}
```

2. **너비 우선 상태 전수 탐색 실행.** [statecheck.mjs](../scripts/statecheck.mjs)에 `--require-complete`를 붙여 유한 범위 내 모든 도달 가능 상태를 탐색하고 불변식, 데드락 부재(`NoDeadlock`), 도달성(`mustReach`)을 한 번에 검사한다.

```bash
node "<SKILL_DIR>/scripts/statecheck.mjs" --require-complete .krig/models/<이름>.json
node "<SKILL_DIR>/scripts/statecheck.mjs" --mermaid .krig/models/<이름>.json
```

3. **최단 반례의 실제 코드 재현 및 교정.**
   - 종료 코드가 `1`(위반 발견)이면 출력된 최단 반례 경로(`step 0, step 1, ...`)가 모델링 오타인지 실제 프로덕션 코드의 결함인지 대조한다.
   - 실제 프로덕션 결함이면 반례의 액션 순서를 그대로 재현하는 **실패 행동 테스트**를 먼저 작성해 적색 실패를 확인한 뒤, 상태 모델과 프로덕션 코드를 함께 고쳐 모델(`statecheck.mjs` 종료 코드 `0`)과 테스트를 모두 통과시킨다.
4. **탐색 결과 시각화 및 문서 인계.** 손으로 그린 추측성 상태도 대신 `statecheck.mjs --mermaid`가 실제 탐색 그래프에서 생성한 `stateDiagram-v2`를 설계 계약이나 검증 근거로 남긴다.

## Rules

- 모델이 불변식을 통과했더라도 `mustReach` 정상 완료 상태에 도달하지 못하면 가드가 지나치게 잠긴 공허한 통과(vacuous pass)로 간주해 수정한다.
- 상태 변수가 무한히 증가하는 카운터(예: 끝없는 재시도 횟수)를 포함하면 탐색 한도에 걸려 `complete=false`가 되므로, 의미 있는 상한(예: 최대 재시도 `2~3`회)으로 유한화해 `complete=true` 전수 완료를 확보한다.

## Verification

- `node "<SKILL_DIR>/scripts/statecheck.mjs" --json --require-complete <명세파일>` 실행 결과 `ok: true`, `complete: true`, `violations: []`임을 직접 확인한다.
- 프로덕션 코드를 함께 수정한 경우 반례 회귀 테스트를 포함한 테스트 스위트 종료 코드 `0`을 확인한다.
