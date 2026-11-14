#!/usr/bin/env node
import fs, { realpathSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { isCliMain, parseCliFlags } from './lib/cli.mjs';
import { ROUTE_BY_ID, ROUTE_TABLE } from './route-table.mjs';

const SKILL_DIR = path.resolve(path.dirname(realpathSync(fileURLToPath(import.meta.url))), '..');

const TECH_SPEC_IDS = new Set([
  'UTF-8', 'UTF-16', 'UTF-32',
  'SHA-1', 'SHA-224', 'SHA-256', 'SHA-384', 'SHA-512',
  'AES-128', 'AES-192', 'AES-256',
  'RSA-2048', 'RSA-4096',
  'ISO-8601', 'ISO-3166',
  'HTTP-1', 'HTTP-2', 'HTTP-3',
  'ECMA-262', 'IEEE-754'
]);

const AMBIGUOUS_BARE_ROUTES = new Set([
  'align', 'design', 'model', 'drive', 'slim', 'resume', 'ship', 'eval', 'full'
]);

const NEGATED_BUILD_RE = /(?:수정하?|변경하?|고치|바꾸|건드리|삭제하?|제거하?|작성하?|추가하?)지\s*말(?:고|아|며)?|\b(?:do\s+not|don't|without)\s+(?:modify(?:ing)?|edit(?:ing)?|chang(?:e|ing)|touch(?:ing)?|alter(?:ing)?|delet(?:e|ing)|remov(?:e|ing))\b/gi;
const DOC_ONLY_WRITE_RE = /(?:PR\s*(?:본문|브리핑|설명)|(?:조사\s*)?(?:보고서|리포트)|[A-Za-z0-9_.-]+\.md(?:\s*파일)?)\s*(?:로|에|을|를)?\s*(?:작성|생성|정리)/gi;
const ML_BACKPROP_RE = /(?:신경망|오차\s*역전파|기울기\s*소실|가중치|순전파|\b(?:neural|gradient|weights|forward\s+pass)\b)/i;
const DOMAIN_RUNTIME_RECOVERY_RE = /(?:워커|소비자|스트리밍|웹소켓|배치|소켓|커넥션|큐|오프셋)\s*.*(?:구현|수정|고쳐|개발)|\b(?:worker|consumer|stream|websocket|offset)\b[\s\S]*\b(?:implement|fix|build)\b/i;
const UI_PLACEHOLDER_RE = /(?:검색창|입력창|인풋|폼|모달|텍스트박스|버튼|UI|\b(?:input|search|form|textbox)\b)\s*.*플레이스홀더/i;

const PATTERNS = Object.freeze({
  eval: /(?:(?:스킬|프롬프트|하네스).*(?:쓰고\s*안\s*쓰고|적용\s*전후|유무).*(?:차이|정량|평가|비교|입증)|블라인드\s*A\/B|A\/B\s*(?:정량\s*)?(?:평가|비교|실험|검증)|대조군.*(?:실험군|적용군)|\b(?:blinded\s+a\/b|a\/b\s+(?:eval|evaluation|comparison)|with[\s-]?(?:skill|krig)\s+vs\s+(?:no|without)[\s-]?skill|no[\s-]?skill\s+vs\s+with[\s-]?(?:skill|krig))\b)/i,
  backprop: /(역전파|사후\s*회고|포스트모템|재발\s*방지\s*(?:규칙|가드레일|문서|원칙)|교훈.*(반영|부호화|문서화|규칙)|실수.*(역전파|재발|가드레일|규칙화)|정본.*역전파|\b(?:backprop|postmortem|encode\s+lessons?)\b)/i,
  resume: /(핸드오프|체크포인트\s*(저장|생성|남겨|확인|복구|점검|상태)|세션\s*(복구|인계|재개)|(?:이전\s*세션|중단된?\s*(?:세션|작업)|어제\s*하던|하던\s*(?:작업|일|거)).*(?:어디까지|이어서|재개|복구|파악|마무리)|어디까지\s*(?:됐|진행됐|했는지)|이어서\s*(?:진행|구현|마무리|작업해|해줘)|\b(?:handoff|session[\s-]?pickup|pick\s*up\s+where|where\s+(?:did\s+we|we)\s+left\s+off|resume\s+(?:the\s+)?(?:session|work|from\s+checkpoint)|pause\s+safely|save\s+checkpoint)\b)/i,
  resumePauseOnly: /(체크포인트\s*(저장|생성|남겨)|안전하게\s*(?:중단|멈춰)|인계\s*(?:파일|문서|기록)\s*(?:남겨|저장|작성)|어디까지\s*(?:됐는지|진행됐는지)\s*(?:상태만|파악만|확인만|알려)|\b(?:pause\s+safely|save\s+checkpoint|checkpoint\s+status)\b)/i,
  resumeContinueBuild: /(이어서\s*(?:구현|마무리|진행해|개발|수정|고쳐|완성)|마무리해\s*줘|끝까지\s*이어서|\b(?:and\s+finish|finish\s+(?:implementing|the\s+task|it)|continue\s+(?:implementing|building))\b)/i,
  ship: /(출하\s*(?:점검|게이트|준비|검증)|머지\s*(?:준비|전\s*점검|가능\s*여부)|병합\s*전\s*점검|릴리스\s*전\s*점검|PR(?:까지|도|을|를)?\s*(?:올려|올릴\s*준비|생성|오픈|열어|본문|브리핑|설명\s*작성)|검증\s*스탬프|패치\s*신선도|patch[\s-]?id\s*(?:검증|점검|확인)|\b(?:ship[\s-]?check|pre[\s-]?(?:ship|merge)\s+check|merge\s+readiness|ready\s+to\s+(?:ship|merge|land)|open\s+a\s+pr|prepare\s+(?:a\s+)?pr|pr\s+briefing)\b)/i,
  demock: /(가짜\s*성공|거짓\s*성공|삼킨\s*예외|예외\s*삼킴|프로덕션.*테스트\s*더블|테스트\s*더블\s*(제거|점검|스캔)|(?:^|[^가-힣a-zA-Z0-9_])목\s*제거|모킹\s*제거|미구현\s*(플레이스홀더|스텁)|플레이스홀더\s*(점검|스캔)|\b(?:demock|unmock|fake[\s-]?success|swallowed[\s-]?errors?)\b)/i,
  slim: /(과잉\s*(?:엔지니어링|추상화|설계)|(?:코드베이스|저장소)\s*(?:군살|다이어트|비대|과잉)|군살\s*(?:감사|스캔|점검|제거|정리)|군살을\s*(?:실제로\s*)?(?:제거|정리|삭제)|미사용\s*(?:의존성|패키지|모듈)|쓰이지\s*않는\s*의존성|안\s*쓰는\s*(?:의존성|패키지|모듈|래퍼).*(?:감사|스캔|찾아|전수|목록|정리|걷어내|삭제|제거)|(?:패스스루|순수\s*재수출)\s*(?:배럴|래퍼|모듈)|고아\s*모듈|기술\s*부채\s*(?:원장|목록|수집|스캔|감사)|부채\s*원장|단축키\s*부채|\b(?:over[\s-]?engineering\s+(?:audit|review|scan)|audit\s+(?:this\s+(?:repo|codebase)\s+)?for\s+(?:bloat|over[\s-]?engineering)|find\s+bloat|bloat\s+audit|debt\s+ledger|shortcut\s+debt|unused\s+dependencies|what\s+can\s+(?:we|i)\s+delete\s+from\s+this\s+repo)\b)/i,
  slimDebtFocus: /(기술\s*부채|부채\s*원장|단축키\s*부채|부패\s*위험|krig-debt|\b(?:debt\s+ledger|shortcut\s+debt|deferred\s+shortcuts?)\b)/i,
  slimApply: /(실제로\s*(?:삭제|제거|걷어내|정리)|걷어내고|삭제하고|제거하고|(?:삭제|제거|정리)해\s*줘|감량\s*적용|\b(?:apply\s+(?:the\s+)?cuts|delete\s+them|remove\s+unused|clean\s+up\s+unused|prune\s+and)\b)/i,
  model: /(모델\s*체킹|상태\s*공간\s*(?:탐색|검증|전수)|유한\s*(?:상태|범위)\s*(?:검증|전수|탐색|모델)|상태\s*(?:머신|기계|전이).*(?:전수|불변식|데드락|교착\s*상태|반례|검증|체킹|보장)|불변식.*(?:전수|상태\s*전이|반례|데드락|교착|모델)|(?:데드락|교착\s*상태)\s*(?:검증|유무|탐색|부재|발생하지\s*않)|\b(?:model[\s-]?check(?:ing)?|state[\s-]?space\s+(?:check|verify|verification|search|explore)|statechart\s+(?:verify|check|model)|deadlock[\s-]?free(?:dom)?|verify\s+state\s+machine|shortest\s+counterexample)\b)/i,
  verify: /(코드\s*리뷰|설계\s*리뷰|PR\s*리뷰|변경.*리뷰|리뷰해|검토해|검증해|검수해|콜드\s*리드|누수\s*감사|\b(?:code[\s-]?review|pr[\s-]?review|cold[\s-]?read|leakage[\s-]?audit|audit\s+the|review\s+(?:this|the|my|changes|pr|diff|code|design|branch|error))\b)/i,
  reconBlast: /(영향도\s*(?:분석|파악|추적|점검|확인|조사|검토)|영향\s*범위|파급\s*(?:범위|효과|분석)|바꾸면\s*어디.*(?:깨지|영향)|수정하면\s*어디.*(?:깨지|영향)|호출부\s*(?:전수\s*)?(?:추적|분석|파악|조사)|\b(?:blast[\s-]?radius|impact\s+analysis|what\s+breaks\s+if|callers?\s+of)\b)/i,
  reconWhy: /(왜\s*이렇게\s*(?:짜여|구현|설계|작성|돼|되어)|왜\s*이\s*(?:구조|방식|상수|로직|값).*(?:인지|쓰는지|되어|설정|추가됐|도입됐)|설계\s*유래|(?:도입|설계)\s*(?:배경|의도|맥락|이유|유래).*(?:조사|추적|찾아|파악|알려|설명|정리)|(?:커밋\s*이력|히스토리|계보|연혁).*(?:추적|조사|찾아|파악|알려)|\bgit\s+blame\b|\b(?:why\s+(?:is|was|does)\s+[\s\S]*?(?:written|designed|set|implemented|structured)|decision\s+lineage|commit\s+lineage|history\s+of)\b)/i,
  reconHow: /(어떻게\s*(?:동작|작동|돌아가|흘러가|구성되)|(?:동작|작동)\s*(?:원리|흐름|구조|방식).*(?:설명|분석|파악|알려|정리)|내부\s*(?:구조|동작|흐름).*(?:설명|분석|파악|알려|정리)|(?:코드(?:베이스)?|모듈|시스템|플러그인)\s*(?:아키텍처|구조|흐름).*(?:파악|설명|분석|브리핑|정리)|\b(?:how\s+does\s+[\s\S]+?\s+work|walk\s+me\s+through|architecture\s+walkthrough|explain\s+(?:the\s+)?(?:current\s+)?[\w\s-]*architecture|trace\s+(?:the\s+)?flow\s+of)\b)/i,
  design: /(모듈\s*설계|인터페이스\s*설계|경계\s*설계|어댑터\s*구조|도메인\s*모델링|아키텍처\s*(?:설계|잡아|재설계)|설계(?:해|만|하고|부터)|\b(?:architect\s+(?:a|an|the)|deep\s+modules?|design\s+(?:the|a|an|and|new|module|interface|api|adapter))\b)/i,
  align: /(기획부터|신규\s*기획|기능\s*기획|서비스\s*기획|프로젝트\s*기획|프론티어\s*인터뷰|인터뷰로\s*범위|요구사항\s*(감량|정리|정의|도출|확정|분석|점검|검토)|스펙\s*(정리|확정|정의|검토)|마일스톤\s*(수립|정리|생성|등록)|티켓\s*(쪼개|분해|나눠|발행|등록)|범위\s*(확정|정렬|산정|감량)|계획\s*세워|\b(?:scope\s+out|grill[\s-]?me|to[\s-]?tickets|clarify\s+requirements|vertical\s+slice\s+(?:issues|tickets)|plan\s+a\s+new\s+[\s\S]*?(?:system|service|feature))\b)/i,
  epicSplit: /(티켓\s*(으로\s*)?(쪼개|분해|나눠)|에픽\s*(쪼개|분해)|이슈\s*(쪼개|분해)|하위\s*티켓\s*(발행|생성|등록)|\b(?:split\s+into\s+tickets|break\s+down.*into\s+tickets)\b)/i,
  hillclimb: /((?:속도|성능|처리량|지연\s*시간|레이턴시|메모리)\s*.*(?:개선|최적화|향상|절감|단축|줄여)|시간\s*단축|벤치마크.*개선|최적화\s*루프|\d+%\s*(줄여|개선|향상|단축)|\b(?:hillclimb|(?:optimize|improve|reduce|speed\s*up)\s+[\s\S]*?\b(?:latency|throughput|memory|performance))\b)/i,
  hardDefect: /(버그|오류|에러|크래시|이중\s*적립|중복\s*승인|결함|안\s*돼|안\s*됨|깨져|깨짐|실패해|오작동|(?:간헐적|반복).*타임아웃|타임아웃\s*(?:버그|오류|에러|현상|발생|문제)|재현|회귀|오타|\b(?:bug|bugs|race\s+condition|regression|crash|crashes|crashing|broken|flaky|timeout\s+(?:bug|error|issue|failure))\b)/i,
  bugfix: /(버그|오류|에러|크래시|이중\s*적립|중복\s*승인|결함|안\s*돼|안\s*됨|깨져|깨짐|실패해|오작동|고쳐|(?:간헐적|반복).*타임아웃|타임아웃\s*(?:버그|오류|에러|현상|발생|문제)|재현|회귀|오타|\b(?:bug|bugs|fix|fixes|race\s+condition|regression|crash|crashes|crashing|broken|flaky|error)\b)/i,
  fullAutonomy: /(끝까지\s*자율|전\s*주기|풀\s*오토|알아서\s*끝까지|\b(?:full[\s-]?cycle|run\s+until\s+done|autonomous)\b)/i,
  buildVerb: /(?:구현(?:해|하|하고|할|합)|개발(?:해|하|하고)|만들(?:어|고|자)|작성(?:해|하|하고)|추가(?:해|하|하고)|보강(?:해|하|하고)|리팩터링|적용(?:해|하|하고)|반영(?:해|하|하고)|고쳐|고치(?:고|자)|수정(?:해|하|하고)|패치(?:해|하|하고)|변경(?:해|하|하고)|바꿔|바꾸(?:고|자)|삭제(?:해|하|하고)|제거(?:해|하|하고)|걷어내|개선(?:해|하|하고)|최적화(?:해|하|하고)|\b(?:implement|build|refactor|write|add|create|patch|fix|resolve|update|change|modify|remove|delete|improve|optimize)\b)/i
});

function hasBuildIntent(text) {
  const sanitized = String(text ?? '')
    .replace(NEGATED_BUILD_RE, ' ')
    .replace(DOC_ONLY_WRITE_RE, ' ');
  return PATTERNS.buildVerb.test(sanitized);
}

export function hasTicketId(text) {
  const re = /(?:^|[\s([{'"])([A-Z]{2,10}-\d+|#\d+)(?=$|[\s)\]}'",.:;!?]|[가-힣])/g;
  let match;
  while ((match = re.exec(text)) !== null) {
    const token = match[1];
    if (token.startsWith('#')) return true;
    if (/^RFC-\d+$/i.test(token)) continue;
    if (TECH_SPEC_IDS.has(token.toUpperCase())) continue;
    return true;
  }
  return false;
}

function isValidRouteName(candidate) {
  return candidate === 'full' || ROUTE_BY_ID.has(candidate);
}

function parseExplicitRoute(cleanedInput) {
  const tokens = cleanedInput.split(/\s+/);
  const rawFirst = tokens[0];
  const restJoined = tokens.slice(1).join(' ').trim();

  const prefixedMatch = /^(?:route:([a-z]+)|([a-z]+):|\[([a-z]+)\])$/i.exec(rawFirst);
  if (prefixedMatch) {
    const id = (prefixedMatch[1] || prefixedMatch[2] || prefixedMatch[3]).toLowerCase();
    if (isValidRouteName(id)) {
      return { primaryId: id, restText: restJoined || cleanedInput };
    }
  }

  const lowerFirst = rawFirst.toLowerCase();
  if (!isValidRouteName(lowerFirst)) return null;

  if (tokens.length === 1) {
    return { primaryId: lowerFirst, restText: cleanedInput };
  }

  if (rawFirst !== lowerFirst || AMBIGUOUS_BARE_ROUTES.has(lowerFirst)) {
    return null;
  }

  if (hasBuildIntent(restJoined)) {
    return null;
  }

  const second = tokens[1];
  const isTicketOrSlashPath = hasTicketId(second) || /[\\/]/.test(second);
  const isKoreanTarget = /[가-힣]/.test(second);

  if (isTicketOrSlashPath || isKoreanTarget) {
    return { primaryId: lowerFirst, restText: restJoined };
  }

  return null;
}

function detectReconSubMode(text) {
  if (PATTERNS.reconBlast.test(text)) return 'blast';
  if (PATTERNS.reconWhy.test(text)) return 'why';
  return 'how';
}

export function classifyTask(rawInput) {
  const strippedSlash = String(rawInput ?? '')
    .trim()
    .replace(/^(?:\/krig(?::krig)?|\$krig)(?=\s|$)\s*/i, '')
    .trim();

  if (!strippedSlash) {
    throw new Error('요청 문장이 비어 있습니다. 예: node route.mjs -- "PROJ-120 구현해줘"');
  }

  const explicit = parseExplicitRoute(strippedSlash);
  const explicitOverride = Boolean(explicit);
  let primaryId = explicit ? explicit.primaryId : null;
  const restText = explicit ? explicit.restText : strippedSlash;

  let driveMode = null;
  let reconMode = null;
  let slimMode = null;
  const reasons = [];

  const buildIntent = hasBuildIntent(restText);
  const prReviewSignal = /(?:PR|풀\s*리퀘스트|이\s*브랜치)\s*.*(?:리뷰|검토)|\b(?:pr[\s-]?review|review\s+(?:this\s+)?pr)\b/i.test(restText);
  const shipSignal = PATTERNS.ship.test(restText) && !prReviewSignal;

  if (!primaryId) {
    const ticketPresent = hasTicketId(restText);
    const hasReconSignal = PATTERNS.reconBlast.test(restText)
      || PATTERNS.reconWhy.test(restText)
      || PATTERNS.reconHow.test(restText);

    if (PATTERNS.fullAutonomy.test(restText) && PATTERNS.align.test(restText)) {
      primaryId = 'full';
      reasons.push('전 주기 자율 실행 및 기획 신호 감지');
    } else if (PATTERNS.eval.test(restText) && !PATTERNS.hardDefect.test(restText)) {
      primaryId = 'eval';
      reasons.push('블라인드 A/B 정량 평가/대조 실험 신호 감지');
    } else if (
      PATTERNS.backprop.test(restText)
      && !ML_BACKPROP_RE.test(restText)
      && !PATTERNS.hardDefect.test(restText)
      && !buildIntent
    ) {
      primaryId = 'backprop';
      reasons.push('교훈 역전파/재발 방지 규칙화 신호 감지');
    } else if (
      PATTERNS.resume.test(restText)
      && !PATTERNS.hardDefect.test(restText)
      && !DOMAIN_RUNTIME_RECOVERY_RE.test(restText)
    ) {
      primaryId = 'resume';
      reasons.push('세션 복구/핸드오프/체크포인트 신호 감지');
    } else if (shipSignal && !buildIntent && !PATTERNS.hardDefect.test(restText)) {
      primaryId = 'ship';
      reasons.push('패치 신선도 출하 게이트/PR 브리핑 신호 감지');
    } else if (
      PATTERNS.demock.test(restText)
      && !UI_PLACEHOLDER_RE.test(restText)
      && !buildIntent
      && !PATTERNS.hardDefect.test(restText)
    ) {
      primaryId = 'demock';
      reasons.push('프로덕션 가짜 성공/삼킨 예외 점검 신호 감지');
    } else if (PATTERNS.slim.test(restText) && !prReviewSignal) {
      primaryId = 'slim';
      reasons.push('저장소 전수 군살 감사/기술 부채 원장 신호 감지');
    } else if (PATTERNS.model.test(restText)) {
      primaryId = 'model';
      reasons.push('유한 상태 공간 BFS 모델 체킹/불변식 검증 신호 감지');
    } else if (
      PATTERNS.epicSplit.test(restText)
      || (PATTERNS.align.test(restText) && !ticketPresent && !PATTERNS.hardDefect.test(restText) && !buildIntent)
    ) {
      primaryId = 'align';
      reasons.push('신규 범위 정렬/요구사항 감량/티켓화 신호 감지');
    } else if (hasReconSignal && !buildIntent) {
      primaryId = 'recon';
      reasons.push('읽기 전용 코드 구조/계보/영향도 조사 신호 감지');
    } else if (PATTERNS.verify.test(restText) && !buildIntent) {
      primaryId = 'verify';
      reasons.push('독립 리뷰/검증 전용 요청 신호 감지');
    } else if (PATTERNS.design.test(restText) && !buildIntent && !PATTERNS.hardDefect.test(restText)) {
      primaryId = 'design';
      reasons.push('모듈 경계 및 인터페이스 설계 전용 신호 감지');
    } else {
      primaryId = 'drive';
      reasons.push('구체적 실행/수정 요청 기본 라우트');
    }
  } else {
    reasons.push(`명시적 라우트 지정(${primaryId})`);
  }

  const driveChain = ROUTE_BY_ID.get('drive').chain;
  let pipelineIds;
  if (primaryId === 'full') {
    pipelineIds = [...new Set(['align', 'design', 'drive', ...driveChain])];
  } else {
    const spec = ROUTE_BY_ID.get(primaryId);
    pipelineIds = [spec.id, ...spec.chain];

    if (primaryId === 'resume' && !PATTERNS.resumePauseOnly.test(restText) && PATTERNS.resumeContinueBuild.test(restText)) {
      pipelineIds = [...new Set(['resume', 'drive', ...driveChain])];
      reasons.push('복구 후 구현 마무리 신호가 함께 있어 drive 파이프라인 연쇄');
    } else if (primaryId === 'slim' && (PATTERNS.slimApply.test(restText) || buildIntent)) {
      pipelineIds = [...new Set(['slim', 'drive', ...driveChain])];
      reasons.push('실제 감량 코드 반영 신호가 함께 있어 drive 파이프라인 연쇄');
    } else if (primaryId === 'model' && (buildIntent || PATTERNS.hardDefect.test(restText))) {
      pipelineIds = [...new Set(['model', 'drive', ...driveChain])];
      reasons.push('상태 모델 검증과 실제 코드 구현/수정 신호가 함께 있어 drive 파이프라인 연쇄');
    } else if (primaryId === 'drive') {
      const prefixes = [];
      if (PATTERNS.reconBlast.test(restText)) {
        prefixes.push('recon');
        reasons.push('변경 영향도 조사 신호가 함께 있어 recon 선행 결합');
      }
      if (PATTERNS.design.test(restText)) {
        prefixes.push('design');
        reasons.push('경계 설계 신호가 함께 있어 design 선행 결합');
      }
      const suffixes = shipSignal ? ['ship'] : [];
      if (suffixes.length > 0) {
        reasons.push('구현 완료 후 출하/PR 생성 신호가 함께 있어 ship 후행 결합');
      }
      if (prefixes.length > 0 || suffixes.length > 0) {
        pipelineIds = [...new Set([...prefixes, 'drive', ...driveChain, ...suffixes])];
      }
    }
  }

  if (pipelineIds.includes('recon')) {
    reconMode = detectReconSubMode(restText);
    reasons.push(`조사 세부 모드(${reconMode})`);
  }

  if (pipelineIds.includes('slim')) {
    slimMode = PATTERNS.slimDebtFocus.test(restText) ? 'debt' : 'audit';
    reasons.push(`감량 세부 모드(${slimMode})`);
  }

  if (pipelineIds.includes('drive')) {
    if (PATTERNS.hardDefect.test(restText) || (!PATTERNS.hillclimb.test(restText) && PATTERNS.bugfix.test(restText))) {
      driveMode = 'bugfix';
      reasons.push('런타임 재현 및 이진 탐색 버그 수정(bugfix) 모드');
    } else if (PATTERNS.hillclimb.test(restText)) {
      driveMode = 'hillclimb';
      reasons.push('정량 지표 반복 개선(hillclimb) 모드');
    } else {
      driveMode = 'feature';
      reasons.push('수직 슬라이스 기능/리팩터링(feature) 모드');
    }
  }

  const steps = pipelineIds.map((id) => {
    const spec = ROUTE_BY_ID.get(id);
    return {
      id: spec.id,
      gear: primaryId === 'full' ? 'afk' : spec.gear,
      playbookPath: path.join(SKILL_DIR, spec.playbook),
      scriptPaths: spec.scripts.map((rel) => path.join(SKILL_DIR, rel)),
      lazyReferences: spec.references.map((rel) => path.join(SKILL_DIR, rel)),
      summary: spec.summary
    };
  });

  return {
    request: strippedSlash,
    explicitOverride,
    primaryRoute: primaryId,
    reconMode,
    slimMode,
    driveMode,
    reasons,
    pipeline: pipelineIds,
    steps
  };
}

function printHelp() {
  console.log('사용법: node route.mjs [--json] [--list] [--stdin] [-- "<요청 문장>"]');
}

function main() {
  let parsed;
  try {
    parsed = parseCliFlags(process.argv.slice(2), {
      booleans: ['json', 'help', 'list', 'stdin'],
      allowedFlags: ['json', 'list', 'stdin'],
      allowSingleDashPositional: true
    });
  } catch (err) {
    console.error(err.message);
    process.exit(2);
  }

  if (parsed.flags.help) {
    printHelp();
    process.exit(0);
  }

  if (parsed.flags.list) {
    if (parsed.flags.json) {
      console.log(JSON.stringify(ROUTE_TABLE, null, 2));
    } else {
      for (const r of ROUTE_TABLE) {
        console.log(`${r.id}\tgear=${r.gear}\tplaybook=${r.playbook}\tchain=${r.chain.join(',') || 'none'}`);
      }
    }
    process.exit(0);
  }

  const text = parsed.flags.stdin
    ? fs.readFileSync(0, 'utf8').trim()
    : parsed.positional.join(' ').trim();

  if (!text) {
    printHelp();
    process.exit(2);
  }

  try {
    const plan = classifyTask(text);
    if (parsed.flags.json) {
      console.log(JSON.stringify(plan, null, 2));
      return;
    }

    console.log(`라우트 파이프라인: ${plan.pipeline.join(' / ')}`);
    if (plan.reconMode) {
      console.log(`조사 세부 모드: ${plan.reconMode}`);
    }
    if (plan.slimMode) {
      console.log(`감량 세부 모드: ${plan.slimMode}`);
    }
    if (plan.driveMode) {
      console.log(`실행 세부 모드: ${plan.driveMode}`);
    }
    console.log(`판별 근거: ${plan.reasons.join(', ')}`);
    for (const step of plan.steps) {
      console.log(`- [${step.id}] (gear=${step.gear}) ${step.playbookPath}`);
    }
  } catch (err) {
    console.error(err.message);
    process.exit(2);
  }
}

if (isCliMain(import.meta.url)) {
  main();
}
