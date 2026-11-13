# Align Playbook (사전 감량, 프론티어 정렬, 수직 티켓화)

## Trigger

요구사항이 흐릿한 신규 기능 기획, 다단계 프로젝트 착수, 범위 감량 및 에픽 티켓 분해 요청이 들어왔을 때 실행한다. 기본 기어는 `hitl`(인간 개입 허용)이며, `full` 전 주기 호출일 때만 `afk` 기어로 동작한다.

## Steps

1. **사실 조사 분리.** 코드베이스 구조, 기존 스키마, 설치된 도구처럼 조회나 실행으로 알 수 있는 사실은 인간에게 묻지 않고 읽기 전용 탐색으로 먼저 확보한다.
2. **관성 요구사항 감량 및 첫 번째 못(`first_nail`) 도출.**
   - 법적 의무, 외부 계약, 안전 가드, 공개 호환성을 제외한 모든 요구사항에 대해 실명 출처, 실질 효용, 추후 재추가 비용을 따진다.
   - 남은 계획 전체를 무너뜨릴 수 있는 가장 위험한 단일 기둥 가정 1개(`first_nail`)와 최소 비용 반증 실험(프로토타입, 쿼리, 실제 API 호출 1회)을 특정한다.
3. **단일 라운드 프론티어 합의 (`hitl` 대 `afk` 분기).**
   - **`hitl` 기어**: 삭제 후보 목록, 첫 번째 못 반증 실험안, 그리고 선행 결정에 가로막히지 않은 최전선(frontier) 질문들을 추천 기본안과 함께 **단 한 번의 질문 라운드**로 묶어 제시한다.
   - **`afk` 기어 (`full` 파이프라인)**: 질문으로 멈추지 않고 권장 기본안을 채택한 뒤 첫 번째 못 반증 실험을 즉시 실행해 가정이 성립하는지 먼저 확인한다.
4. **원장 기록 및 용어집 고정.** 확정된 삭제(`deleted`), 유지(`kept`), 또는 삭제 후 필수성이 입증되어 되살린 항목(`restored`)을 [ledger.mjs](../scripts/ledger.mjs)로 `.krig/deletion-ledger.jsonl`에 기록하고, 합의된 핵심 도메인 용어는 작업 저장소의 `GLOSSARY.md`에 반영한다.

```bash
node "<SKILL_DIR>/scripts/ledger.mjs" record .krig/deletion-ledger.jsonl \
  --kind deleted --id "<REQ_ID>" --title "<항목>" --source "<출처>" --reason "<사유>"
node "<SKILL_DIR>/scripts/ledger.mjs" record .krig/deletion-ledger.jsonl \
  --kind kept --id "<REQ_ID>" --title "<항목>" --source "<출처>" --reason "<유지 근거>"
node "<SKILL_DIR>/scripts/ledger.mjs" summary .krig/deletion-ledger.jsonl --json
```

5. **수직 예광탄 티켓 발행.** 합의된 범위를 수평 계층(DB 따로, API 따로, UI 따로)이 아니라 처음부터 끝까지 관통해 검증 가능한 수직 슬라이스 단위로 쪼갠다. 프로젝트에 연결된 이슈 트래커(`gh issue`, Linear, Jira 등)가 있으면 블로킹 의존성까지 함께 등록하고, `full` 실행이면 위상 정렬 순서대로 첫 슬라이스부터 후속 `design` 및 `drive` 단계로 넘긴다.

## Rules

- 실험이나 코드 조회로 답을 얻을 수 있는 질문은 절대 인간에게 던지지 않는다.
- 삭제된 항목이 0건(`keptCount > 0`인데 `deletedCount == 0`)이거나 5건 이상 삭제 후 복구율(`addBackRate`)이 0.1 미만이면 과소 삭제 경고(`underDeletionWarning`)를 확인하고 추측성 요구사항을 한 번 더 점검한다.
- 삭제 이력이 없는 항목에는 `--kind restored`를 쓰지 않으며, 반드시 기존 `deleted` 항목을 되살릴 때만 구체적 복구 근거와 함께 기록한다.

## Verification

- `ledger.mjs summary .krig/deletion-ledger.jsonl --json` 출력으로 삭제/유지/복구 상태가 유효하게 집계되었는지 확인한다.
- 도메인 용어가 추가된 경우 `GLOSSARY.md` 반영 여부를 확인하고, 티켓 발행 대상이면 이슈 목록 조회 출력으로 의존성 연결을 확인한다.
