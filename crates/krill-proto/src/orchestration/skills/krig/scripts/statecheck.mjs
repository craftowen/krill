#!/usr/bin/env node
import fs from 'node:fs';
import path from 'node:path';
import { pathToFileURL } from 'node:url';
import { isCliMain, parseCliFlags } from './lib/cli.mjs';

function isPlainObject(val) {
  return val !== null && typeof val === 'object' && !Array.isArray(val);
}

function isOperatorObject(val) {
  if (!isPlainObject(val)) return false;
  const keys = Object.keys(val);
  return keys.length > 0 && keys.every((k) => k.startsWith('$'));
}

function deepFreeze(obj) {
  if (obj === null || typeof obj !== 'object' || Object.isFrozen(obj)) return obj;
  Object.freeze(obj);
  if (Array.isArray(obj)) {
    for (const item of obj) deepFreeze(item);
  } else {
    for (const key of Object.keys(obj)) deepFreeze(obj[key]);
  }
  return obj;
}

export function canonicalizeState(value, fieldPath = 'state') {
  if (value === null || typeof value === 'boolean' || typeof value === 'string') {
    return JSON.stringify(value);
  }
  if (typeof value === 'number') {
    if (!Number.isFinite(value)) {
      throw new Error(`${fieldPath}에 유한하지 않은 숫자(${value})가 포함되어 있습니다.`);
    }
    return JSON.stringify(Object.is(value, -0) ? 0 : value);
  }
  if (Array.isArray(value)) {
    return `[${value.map((v, i) => canonicalizeState(v, `${fieldPath}[${i}]`)).join(',')}]`;
  }
  if (isPlainObject(value)) {
    const keys = Object.keys(value).sort();
    const parts = [];
    for (const k of keys) {
      if (value[k] === undefined) {
        throw new Error(`${fieldPath}.${k} 값이 undefined입니다. 명시적 null이나 리터럴 값을 사용하세요.`);
      }
      parts.push(`${JSON.stringify(k)}:${canonicalizeState(value[k], `${fieldPath}.${k}`)}`);
    }
    return `{${parts.join(',')}}`;
  }
  throw new Error(`${fieldPath}에 직렬화할 수 없는 타입(${typeof value})이 포함되어 있습니다.`);
}

function getByPath(obj, dottedKey) {
  const parts = String(dottedKey).split('.');
  let cur = obj;
  for (const part of parts) {
    if (cur === null || cur === undefined || typeof cur !== 'object') return undefined;
    cur = cur[part];
  }
  return cur;
}

function setByPath(obj, dottedKey, val) {
  const parts = String(dottedKey).split('.');
  let cur = obj;
  for (let i = 0; i < parts.length - 1; i += 1) {
    const seg = parts[i];
    if (Array.isArray(cur)) {
      if (!/^\d+$/.test(seg)) {
        throw new Error(`배열에 숫자가 아닌 키(${seg})를 지정할 수 없습니다: ${dottedKey}`);
      }
      const idx = Number.parseInt(seg, 10);
      if (idx < 0 || idx >= cur.length) {
        throw new Error(`배열 인덱스 범위를 벗어난 경로 갱신입니다: ${dottedKey}`);
      }
      if (cur[idx] === null || typeof cur[idx] !== 'object') cur[idx] = {};
      cur = cur[idx];
    } else {
      if (!isPlainObject(cur[seg]) && !Array.isArray(cur[seg])) cur[seg] = {};
      cur = cur[seg];
    }
  }
  const last = parts[parts.length - 1];
  if (Array.isArray(cur)) {
    if (!/^\d+$/.test(last)) {
      throw new Error(`배열에 숫자가 아닌 키(${last})를 지정할 수 없습니다: ${dottedKey}`);
    }
    const idx = Number.parseInt(last, 10);
    if (idx < 0 || idx > cur.length) {
      throw new Error(`배열 인덱스 범위를 벗어난 경로 갱신입니다: ${dottedKey}`);
    }
    cur[idx] = val;
  } else {
    cur[last] = val;
  }
}

function deepEqual(a, b) {
  if (a === undefined || b === undefined) return a === b;
  return canonicalizeState(a) === canonicalizeState(b);
}

function areComparableOrdered(a, b) {
  return (typeof a === 'number' && typeof b === 'number' && Number.isFinite(a) && Number.isFinite(b))
    || (typeof a === 'string' && typeof b === 'string');
}

function evaluateFieldOperator(actual, opObj, rootState) {
  for (const [op, operand] of Object.entries(opObj)) {
    switch (op) {
      case '$eq':
        if (!deepEqual(actual, operand)) return false;
        break;
      case '$ne':
        if (deepEqual(actual, operand)) return false;
        break;
      case '$lt':
        if (!areComparableOrdered(actual, operand) || !(actual < operand)) return false;
        break;
      case '$lte':
        if (!areComparableOrdered(actual, operand) || !(actual <= operand)) return false;
        break;
      case '$gt':
        if (!areComparableOrdered(actual, operand) || !(actual > operand)) return false;
        break;
      case '$gte':
        if (!areComparableOrdered(actual, operand) || !(actual >= operand)) return false;
        break;
      case '$in':
        if (!Array.isArray(operand) || !operand.some((item) => deepEqual(actual, item))) return false;
        break;
      case '$nin':
        if (!Array.isArray(operand) || operand.some((item) => deepEqual(actual, item))) return false;
        break;
      case '$eqRef':
        if (!deepEqual(actual, getByPath(rootState, operand))) return false;
        break;
      case '$neRef':
        if (deepEqual(actual, getByPath(rootState, operand))) return false;
        break;
      case '$ltRef': {
        const refVal = getByPath(rootState, operand);
        if (!areComparableOrdered(actual, refVal) || !(actual < refVal)) return false;
        break;
      }
      case '$lteRef': {
        const refVal = getByPath(rootState, operand);
        if (!areComparableOrdered(actual, refVal) || !(actual <= refVal)) return false;
        break;
      }
      case '$gtRef': {
        const refVal = getByPath(rootState, operand);
        if (!areComparableOrdered(actual, refVal) || !(actual > refVal)) return false;
        break;
      }
      case '$gteRef': {
        const refVal = getByPath(rootState, operand);
        if (!areComparableOrdered(actual, refVal) || !(actual >= refVal)) return false;
        break;
      }
      default:
        throw new Error(`알 수 없는 조건 연산자입니다: ${op}`);
    }
  }
  return true;
}

export function evaluateCondition(cond, state) {
  if (cond === undefined || cond === null) return true;
  if (typeof cond === 'boolean') return cond;
  if (typeof cond === 'function') return Boolean(cond(state));
  if (!isPlainObject(cond)) {
    throw new Error(`유효하지 않은 조건 표현식입니다: ${JSON.stringify(cond)}`);
  }

  for (const [key, expected] of Object.entries(cond)) {
    if (key === '$and') {
      if (!Array.isArray(expected)) throw new Error('$and 값은 조건 배열이어야 합니다.');
      if (!expected.every((sub) => evaluateCondition(sub, state))) return false;
      continue;
    }
    if (key === '$or') {
      if (!Array.isArray(expected)) throw new Error('$or 값은 조건 배열이어야 합니다.');
      if (!expected.some((sub) => evaluateCondition(sub, state))) return false;
      continue;
    }
    if (key === '$not') {
      if (evaluateCondition(expected, state)) return false;
      continue;
    }
    if (key === '$implies') {
      if (!Array.isArray(expected) || expected.length !== 2) {
        throw new Error('$implies 값은 [전제조건, 결론조건] 길이 2 배열이어야 합니다.');
      }
      const [premise, conclusion] = expected;
      if (evaluateCondition(premise, state) && !evaluateCondition(conclusion, state)) return false;
      continue;
    }

    const actual = getByPath(state, key);
    if (isOperatorObject(expected)) {
      if (!evaluateFieldOperator(actual, expected, state)) return false;
    } else if (!deepEqual(actual, expected)) {
      return false;
    }
  }
  return true;
}

function applyDeclarativePatch(state, patch) {
  if (!isPlainObject(patch)) {
    throw new Error('액션의 set 패치는 객체여야 합니다.');
  }
  const next = structuredClone(state);
  for (const [key, expr] of Object.entries(patch)) {
    const current = getByPath(state, key);
    if (isOperatorObject(expr)) {
      if ('$inc' in expr) {
        setByPath(next, key, Number(current ?? 0) + Number(expr.$inc));
      } else if ('$dec' in expr) {
        setByPath(next, key, Number(current ?? 0) - Number(expr.$dec));
      } else if ('$ref' in expr) {
        const refVal = getByPath(state, expr.$ref);
        setByPath(next, key, refVal === undefined ? null : structuredClone(refVal));
      } else if ('$toggle' in expr) {
        setByPath(next, key, !current);
      } else if ('$append' in expr) {
        const arr = Array.isArray(current) ? structuredClone(current) : [];
        arr.push(structuredClone(expr.$append));
        setByPath(next, key, arr);
      } else {
        throw new Error(`알 수 없는 상태 변경 연산자입니다: ${Object.keys(expr).join(',')}`);
      }
    } else {
      setByPath(next, key, structuredClone(expr));
    }
  }
  return next;
}

function normalizeActions(rawActions) {
  if (Array.isArray(rawActions)) {
    return rawActions.map((act, idx) => {
      if (!isPlainObject(act) || !act.name || !String(act.name).trim()) {
        throw new Error(`actions[${idx}]에 유효한 name 문자열이 필요합니다.`);
      }
      return { ...act, name: String(act.name).trim() };
    });
  }
  if (isPlainObject(rawActions)) {
    return Object.entries(rawActions).map(([name, body]) => {
      if (!isPlainObject(body)) {
        throw new Error(`액션 '${name}' 정의는 객체여야 합니다.`);
      }
      return { ...body, name };
    });
  }
  throw new Error('모델 명세에 actions 배열 또는 객체가 필요합니다.');
}

function validateTerminalSpec(terminalSpec) {
  if (terminalSpec === undefined || terminalSpec === null || typeof terminalSpec === 'function') {
    return;
  }
  if (Array.isArray(terminalSpec)) {
    if (terminalSpec.length === 0) {
      throw new Error('terminal 배열은 비어 있을 수 없습니다.');
    }
    for (const item of terminalSpec) {
      if (isPlainObject(item) && Object.keys(item).length === 0) {
        throw new Error('terminal 배열에 빈 조건 객체({})를 넣을 수 없습니다.');
      }
    }
    return;
  }
  if (isPlainObject(terminalSpec)) {
    if (Object.keys(terminalSpec).length === 0) {
      throw new Error('terminal 조건 객체는 최소 1개 이상의 필드를 지정해야 합니다.');
    }
    return;
  }
  throw new Error('terminal 명세는 조건 객체, 조건 배열, 또는 함수여야 합니다.');
}

function isTerminalState(terminalSpec, frozenState) {
  if (!terminalSpec) return false;
  if (typeof terminalSpec === 'function') return Boolean(terminalSpec(frozenState));
  if (Array.isArray(terminalSpec)) {
    return terminalSpec.some((cond) => evaluateCondition(cond, frozenState));
  }
  return evaluateCondition(terminalSpec, frozenState);
}

function parseStrictInt(rawVal, defaultVal, minVal, label) {
  if (rawVal === undefined || rawVal === null || rawVal === '') return defaultVal;
  const str = String(rawVal).trim();
  if (!/^\d+$/.test(str)) {
    throw new Error(`유효하지 않은 ${label} 값입니다(${minVal} 이상의 정수 필요): ${rawVal}`);
  }
  const num = Number.parseInt(str, 10);
  if (!Number.isSafeInteger(num) || num < minVal) {
    throw new Error(`유효하지 않은 ${label} 값입니다(${minVal} 이상의 정수 필요): ${rawVal}`);
  }
  return num;
}

function reconstructTrace(nodesById, targetId) {
  const chain = [];
  let cur = nodesById.get(targetId);
  while (cur) {
    chain.push({
      stateId: cur.id,
      action: cur.viaAction,
      state: structuredClone(cur.state)
    });
    cur = cur.parentId ? nodesById.get(cur.parentId) : null;
  }
  chain.reverse();
  return chain.map((item, idx) => ({
    step: idx,
    stateId: item.stateId,
    action: item.action,
    state: item.state
  }));
}

function summarizeStateInline(state) {
  const entries = Object.entries(state).map(([k, v]) => `${k}=${JSON.stringify(v)}`);
  const text = entries.join(', ');
  return text.length > 72 ? `${text.slice(0, 69)}...` : text;
}

export function renderMermaid(report) {
  const lines = ['stateDiagram-v2'];
  const violatedNodeIds = new Map();
  for (const v of report.violations) {
    if (v.stateId) {
      const prev = violatedNodeIds.get(v.stateId) ?? [];
      prev.push(`${v.kind}:${v.name}`);
      violatedNodeIds.set(v.stateId, prev);
    }
  }

  for (const node of report.nodes) {
    const safeLabel = summarizeStateInline(node.state).replace(/"/g, "'");
    lines.push(`  state "${node.id} (${safeLabel})" as ${node.id}`);
  }
  for (const initId of report.initialIds) {
    lines.push(`  [*] --> ${initId}`);
  }
  for (const edge of report.edges) {
    const safeAction = String(edge.action).replace(/:/g, '/');
    lines.push(`  ${edge.from} --> ${edge.to} : ${safeAction}`);
  }
  for (const node of report.nodes) {
    if (node.isTerminal) {
      lines.push(`  ${node.id} --> [*]`);
    }
    if (violatedNodeIds.has(node.id)) {
      lines.push(`  note right of ${node.id} : VIOLATION ${violatedNodeIds.get(node.id).join(', ')}`);
    }
  }
  return lines.join('\n');
}

export function verifyStateModel(spec, options = {}) {
  if (!isPlainObject(spec)) {
    throw new Error('모델 명세는 객체여야 합니다.');
  }

  const rawInitials = Array.isArray(spec.initial) ? spec.initial : [spec.initial];
  if (rawInitials.length === 0 || !rawInitials.every((s) => isPlainObject(s))) {
    throw new Error('initial 상태는 비어 있지 않은 상태 객체(또는 상태 객체 배열)여야 합니다.');
  }

  validateTerminalSpec(spec.terminal);

  const actions = normalizeActions(spec.actions);
  if (actions.length === 0) {
    throw new Error('최소 1개 이상의 액션이 필요합니다.');
  }

  const invariants = isPlainObject(spec.invariants) ? spec.invariants : {};
  const mustReach = isPlainObject(spec.mustReach) ? spec.mustReach : {};
  const checkDeadlock = spec.checkDeadlock !== false && options.checkDeadlock !== false;
  const maxStates = parseStrictInt(options.maxStates ?? spec.maxStates, 5000, 1, 'maxStates');
  const maxDepth = parseStrictInt(options.maxDepth ?? spec.maxDepth, 50, 0, 'maxDepth');

  const nodes = [];
  const nodesById = new Map();
  const keyToId = new Map();
  const edges = [];
  const edgeKeys = new Set();
  const initialIds = [];
  const queue = [];

  const violatedNames = new Set();
  const violations = [];
  const reachedTargets = new Set();

  let maxDepthReached = 0;
  let truncatedByMaxStates = false;
  let truncatedByMaxDepth = false;

  function registerState(rawState, depth, parentId, viaAction) {
    const key = canonicalizeState(rawState);
    if (keyToId.has(key)) {
      return { id: keyToId.get(key), isNew: false };
    }
    if (nodes.length >= maxStates) {
      truncatedByMaxStates = true;
      return { id: null, isNew: false };
    }
    const frozen = deepFreeze(structuredClone(rawState));
    const id = `S${nodes.length}`;
    const node = {
      id,
      key,
      state: frozen,
      depth,
      parentId,
      viaAction,
      isTerminal: isTerminalState(spec.terminal, frozen)
    };
    nodes.push(node);
    nodesById.set(id, node);
    keyToId.set(key, id);
    queue.push(node);
    if (depth > maxDepthReached) maxDepthReached = depth;

    for (const [invName, invCond] of Object.entries(invariants)) {
      if (violatedNames.has(`INVARIANT:${invName}`)) continue;
      if (!evaluateCondition(invCond, frozen)) {
        violatedNames.add(`INVARIANT:${invName}`);
        violations.push({
          kind: 'INVARIANT',
          name: invName,
          stateId: id,
          depth,
          message: `상태 ${id}(깊이 ${depth})에서 불변식 '${invName}'이 깨졌습니다.`,
          trace: reconstructTrace(nodesById, id)
        });
      }
    }

    for (const [targetName, targetCond] of Object.entries(mustReach)) {
      if (!reachedTargets.has(targetName) && evaluateCondition(targetCond, frozen)) {
        reachedTargets.add(targetName);
      }
    }

    return { id, isNew: true };
  }

  for (const initState of rawInitials) {
    const reg = registerState(initState, 0, null, null);
    if (reg.id) initialIds.push(reg.id);
  }

  let head = 0;
  let transitionCount = 0;

  while (head < queue.length) {
    const current = queue[head];
    head += 1;

    const enabledOutcomes = [];
    for (const action of actions) {
      const guardCond = action.guard ?? action.when;
      if (!evaluateCondition(guardCond, current.state)) continue;

      if (typeof action.apply === 'function') {
        const result = action.apply(current.state);
        if (Array.isArray(result)) {
          result.forEach((nextState, bIdx) => {
            if (!isPlainObject(nextState)) {
              throw new Error(`액션 '${action.name}'의 apply 배열 반환값[${bIdx}]이 객체가 아닙니다.`);
            }
            enabledOutcomes.push({
              label: result.length > 1 ? `${action.name}#${bIdx + 1}` : action.name,
              nextState
            });
          });
        } else if (isPlainObject(result)) {
          enabledOutcomes.push({ label: action.name, nextState: result });
        } else {
          throw new Error(`액션 '${action.name}'의 apply 반환값은 상태 객체 또는 배열이어야 합니다.`);
        }
      } else if (Array.isArray(action.branches)) {
        action.branches.forEach((branch, bIdx) => {
          if (!evaluateCondition(branch.when ?? branch.guard, current.state)) return;
          const label = branch.label ? `${action.name}[${branch.label}]` : `${action.name}#${bIdx + 1}`;
          enabledOutcomes.push({
            label,
            nextState: applyDeclarativePatch(current.state, branch.set ?? {})
          });
        });
      } else if (isPlainObject(action.set)) {
        enabledOutcomes.push({
          label: action.name,
          nextState: applyDeclarativePatch(current.state, action.set)
        });
      } else {
        throw new Error(`액션 '${action.name}'에 set, branches, apply 중 하나가 필요합니다.`);
      }
    }

    if (enabledOutcomes.length === 0) {
      if (checkDeadlock && !current.isTerminal && !violatedNames.has('DEADLOCK')) {
        violatedNames.add('DEADLOCK');
        violations.push({
          kind: 'DEADLOCK',
          name: 'NoDeadlock',
          stateId: current.id,
          depth: current.depth,
          message: `비종단 상태 ${current.id}(깊이 ${current.depth})에서 실행 가능한 전이가 없어 데드락이 발생했습니다.`,
          trace: reconstructTrace(nodesById, current.id)
        });
      }
      continue;
    }

    if (current.depth >= maxDepth) {
      truncatedByMaxDepth = true;
      continue;
    }

    for (const outcome of enabledOutcomes) {
      transitionCount += 1;
      const reg = registerState(outcome.nextState, current.depth + 1, current.id, outcome.label);
      if (reg.id) {
        const edgeKey = `${current.id}|${outcome.label}|${reg.id}`;
        if (!edgeKeys.has(edgeKey)) {
          edgeKeys.add(edgeKey);
          edges.push({ from: current.id, to: reg.id, action: outcome.label });
        }
      }
    }
  }

  const complete = !truncatedByMaxStates && !truncatedByMaxDepth;

  for (const targetName of Object.keys(mustReach)) {
    if (!reachedTargets.has(targetName)) {
      violations.push({
        kind: 'UNREACHABLE',
        name: targetName,
        stateId: null,
        depth: maxDepthReached,
        message: complete
          ? `도달 가능한 전체 상태(${nodes.length}개) 중 필수 목표 '${targetName}'에 도달하는 경로가 없습니다.`
          : `탐색 한도(상태 ${nodes.length}개, 깊이 ${maxDepthReached}) 내에서 필수 목표 '${targetName}'에 도달하지 못했습니다.`,
        trace: []
      });
    }
  }

  const report = {
    ok: violations.length === 0 && (!options.requireComplete || complete),
    complete,
    truncatedByMaxStates,
    truncatedByMaxDepth,
    exploredStates: nodes.length,
    transitions: transitionCount,
    uniqueEdges: edges.length,
    maxDepthReached,
    checkedInvariants: Object.keys(invariants),
    checkedMustReach: Object.keys(mustReach),
    reachedTargets: [...reachedTargets],
    violations,
    initialIds,
    nodes: nodes.map((n) => ({
      id: n.id,
      depth: n.depth,
      isTerminal: n.isTerminal,
      state: structuredClone(n.state)
    })),
    edges
  };

  return {
    ...report,
    mermaid: renderMermaid(report)
  };
}

async function loadSpecFromInput(filePath, useStdin) {
  if (useStdin) {
    const raw = fs.readFileSync(0, 'utf8').trim();
    if (!raw) throw new Error('표준 입력(stdin) 모델 명세가 비어 있습니다.');
    return JSON.parse(raw);
  }
  const abs = path.resolve(filePath);
  if (!fs.existsSync(abs)) {
    throw new Error(`모델 명세 파일을 찾을 수 없습니다: ${filePath}`);
  }
  const ext = path.extname(abs).toLowerCase();
  if (ext === '.mjs' || ext === '.js') {
    const mod = await import(pathToFileURL(abs).href);
    const candidate = mod.default ?? mod.spec ?? mod.model;
    if (!candidate) {
      throw new Error(`ES 모듈(${filePath})에서 default 또는 spec 객체를 export 해야 합니다.`);
    }
    return candidate;
  }
  return JSON.parse(fs.readFileSync(abs, 'utf8'));
}

function printHelp() {
  console.log([
    '사용법: node statecheck.mjs [--json] [--mermaid] [--stdin] [--max-states <N>] [--max-depth <D>] [--require-complete] [<명세파일.json|.mjs>]',
    '조건 연산자: $eq, $ne, $lt, $lte, $gt, $gte, $in, $nin, $eqRef~$gteRef, $and, $or, $not, $implies',
    '변경 연산자: $inc, $dec, $ref, $toggle, $append (ESM 명세는 default, spec, 또는 model 객체 export)'
  ].join('\n'));
}

async function main() {
  let parsed;
  try {
    parsed = parseCliFlags(process.argv.slice(2), {
      booleans: ['json', 'help', 'mermaid', 'stdin', 'require-complete'],
      allowedFlags: ['json', 'mermaid', 'stdin', 'max-states', 'max-depth', 'require-complete']
    });
  } catch (err) {
    console.error(err.message);
    process.exit(2);
  }

  if (parsed.flags.help) {
    printHelp();
    process.exit(0);
  }

  if (!parsed.flags.stdin && parsed.positional.length !== 1) {
    printHelp();
    process.exit(2);
  }

  try {
    const spec = await loadSpecFromInput(parsed.positional[0], Boolean(parsed.flags.stdin));
    const report = verifyStateModel(spec, {
      maxStates: parsed.flags['max-states'],
      maxDepth: parsed.flags['max-depth'],
      requireComplete: Boolean(parsed.flags['require-complete'])
    });

    if (parsed.flags.json) {
      console.log(JSON.stringify(report, null, 2));
    } else if (parsed.flags.mermaid) {
      console.log(report.mermaid);
    } else {
      const statusLabel = report.ok ? 'PASS' : 'FAIL';
      console.log(`[${statusLabel}] 탐색 상태 ${report.exploredStates}개, 전이 ${report.transitions}회, 최대 깊이 ${report.maxDepthReached}, 전수 완료=${report.complete}`);
      if (report.violations.length > 0) {
        for (const v of report.violations) {
          console.log(`위반 [${v.kind}:${v.name}] ${v.message}`);
          for (const step of v.trace) {
            const actLabel = step.action ? `(${step.action}) ` : '(초기 상태) ';
            console.log(`  step ${step.step}: ${actLabel}${step.stateId} ${JSON.stringify(step.state)}`);
          }
        }
      }
      if (parsed.flags['require-complete'] && !report.complete) {
        console.log('미완료: 탐색 한도에 도달해 전체 상태 공간을 끝까지 탐색하지 못했습니다.');
      }
    }

    process.exit(report.ok ? 0 : 1);
  } catch (err) {
    console.error(err.message);
    process.exit(2);
  }
}

if (isCliMain(import.meta.url)) {
  main();
}
