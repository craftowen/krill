export const ROUTE_TABLE = Object.freeze([
  Object.freeze({
    id: 'align',
    gear: 'hitl',
    playbook: 'playbooks/align.md',
    references: Object.freeze([]),
    scripts: Object.freeze(['scripts/ledger.mjs']),
    chain: Object.freeze([]),
    summary: '관성 요구사항 감량, 첫 번째 못 반증 실험, 프론티어 인터뷰, GLOSSARY 갱신, 수직 티켓 발행'
  }),
  Object.freeze({
    id: 'recon',
    gear: 'afk',
    playbook: 'playbooks/recon.md',
    references: Object.freeze([]),
    scripts: Object.freeze(['scripts/recon.mjs']),
    chain: Object.freeze([]),
    summary: '서브시스템 동작 원리(how), 커밋/설계 계보(why), 5단계 변경 영향도 증명(blast) 읽기 전용 조사'
  }),
  Object.freeze({
    id: 'design',
    gear: 'afk',
    playbook: 'playbooks/design.md',
    references: Object.freeze(['references/deepening.md']),
    scripts: Object.freeze([]),
    chain: Object.freeze(['verify']),
    summary: '깊은 모듈 경계 설계, 2-어댑터 이음매, 의존성 4분류, 병렬 후보 설계 접목'
  }),
  Object.freeze({
    id: 'model',
    gear: 'afk',
    playbook: 'playbooks/model.md',
    references: Object.freeze([]),
    scripts: Object.freeze(['scripts/statecheck.mjs']),
    chain: Object.freeze(['verify']),
    summary: '무의존 유한 상태 공간 BFS 탐색, 불변식/데드락/도달성 검증, 최단 반례 행동 테스트 재현'
  }),
  Object.freeze({
    id: 'drive',
    gear: 'afk',
    playbook: 'playbooks/drive.md',
    references: Object.freeze([]),
    scripts: Object.freeze(['scripts/decision-log.mjs']),
    chain: Object.freeze(['demock', 'verify']),
    summary: '기능 수직 슬라이스, 런타임 계측 이진 탐색 버그 수정, 고정 하네스 단일 변인 개선 루프'
  }),
  Object.freeze({
    id: 'demock',
    gear: 'afk',
    playbook: 'playbooks/demock.md',
    references: Object.freeze([]),
    scripts: Object.freeze(['scripts/scan.mjs']),
    chain: Object.freeze(['verify']),
    summary: '프로덕션 경로 테스트 더블, 삼킨 예외, 거짓 통과 가드, 미구현 플레이스홀더 제거'
  }),
  Object.freeze({
    id: 'verify',
    gear: 'afk',
    playbook: 'playbooks/verify.md',
    references: Object.freeze(['references/standards-baseline.md', 'references/leakage-8.md']),
    scripts: Object.freeze(['scripts/ship-check.mjs']),
    chain: Object.freeze([]),
    summary: 'Standards/Spec 2축 병렬 리뷰, 백지 콜드 리드, 실행 재현 판정 및 patch-id 검증 스탬프 발급'
  }),
  Object.freeze({
    id: 'slim',
    gear: 'afk',
    playbook: 'playbooks/slim.md',
    references: Object.freeze([]),
    scripts: Object.freeze(['scripts/slim.mjs']),
    chain: Object.freeze([]),
    summary: '저장소 전수 과잉 추상화/미사용 의존성/고아 모듈 감사 및 부패 위험(rotRisk) 기술 부채 원장'
  }),
  Object.freeze({
    id: 'resume',
    gear: 'afk',
    playbook: 'playbooks/resume.md',
    references: Object.freeze([]),
    scripts: Object.freeze(['scripts/checkpoint.mjs']),
    chain: Object.freeze([]),
    summary: '이전 세션 진행 상태 복구, 패치/커밋 드리프트 진단, 무손실 중단 체크포인트(handoff.json) 저장'
  }),
  Object.freeze({
    id: 'ship',
    gear: 'afk',
    playbook: 'playbooks/ship.md',
    references: Object.freeze([]),
    scripts: Object.freeze(['scripts/ship-check.mjs']),
    chain: Object.freeze([]),
    summary: 'git patch-id 검증 신선도 대조, 작업 트리/시크릿/충돌 위생 점검, 40줄 이내 1분 PR 브리핑 검증'
  }),
  Object.freeze({
    id: 'eval',
    gear: 'afk',
    playbook: 'playbooks/eval.md',
    references: Object.freeze(['references/leakage-8.md']),
    scripts: Object.freeze(['scripts/ab-eval.mjs']),
    chain: Object.freeze([]),
    summary: '블라인드 2군(no-skill 대 with-krig) 실험 격리, 보류된 결정론적 오라클 채점, 쌍대 향상폭 산출'
  }),
  Object.freeze({
    id: 'backprop',
    gear: 'afk',
    playbook: 'playbooks/backprop.md',
    references: Object.freeze([]),
    scripts: Object.freeze(['scripts/unwrap.mjs']),
    chain: Object.freeze([]),
    summary: '강제력 피라미드 구조 교정 및 탈도구화 v0 정본 역전파'
  })
]);

export const ROUTE_BY_ID = new Map(ROUTE_TABLE.map((entry) => [entry.id, entry]));
