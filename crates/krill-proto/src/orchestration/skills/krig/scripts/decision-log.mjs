#!/usr/bin/env node
import { execFileSync } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';
import { isCliMain, parseCliFlags } from './lib/cli.mjs';

export const HEADER = 'ts\troute\tstep\thypothesis\taction\tevidence\tverdict\tcommit';
const VALID_VERDICTS = new Set(['KEEP', 'REVERT', 'VERIFIED', 'NOT_VERIFIED', 'INCONCLUSIVE', 'NOTE']);

function cleanCell(value) {
  return String(value ?? '').replace(/[\t\r\n]+/g, ' ').trim();
}

function isPlaceholderValue(value) {
  const normalized = cleanCell(value).toLowerCase();
  return !normalized || normalized === 'none' || normalized === '-' || normalized === 'n/a';
}

function isValidGitRepo(repoDir) {
  if (!fs.existsSync(repoDir)) return false;
  try {
    execFileSync('git', ['-C', repoDir, 'rev-parse', '--git-dir'], {
      stdio: ['ignore', 'pipe', 'ignore']
    });
    return true;
  } catch {
    return false;
  }
}

export function initDecisionLog(tsvPath) {
  const abs = path.resolve(tsvPath);
  fs.mkdirSync(path.dirname(abs), { recursive: true });
  if (fs.existsSync(abs) && fs.readFileSync(abs, 'utf8').replace(/(?:\r?\n)+$/, '').length > 0) {
    return { created: false, path: abs };
  }
  fs.writeFileSync(abs, `${HEADER}\n`, 'utf8');
  return { created: true, path: abs };
}

export function appendDecisionRow(tsvPath, row) {
  initDecisionLog(tsvPath);
  const verdict = cleanCell(row?.verdict).toUpperCase();
  if (!VALID_VERDICTS.has(verdict)) {
    throw new Error(`유효하지 않은 verdict(${row?.verdict})입니다. 허용값: ${[...VALID_VERDICTS].join(', ')}`);
  }
  const route = cleanCell(row?.route || 'drive');
  const step = cleanCell(row?.step || 'step');
  const hypothesis = cleanCell(row?.hypothesis);
  const action = cleanCell(row?.action);
  const rawEvidence = cleanCell(row?.evidence);
  const rawCommit = cleanCell(row?.commit);
  const evidence = isPlaceholderValue(rawEvidence) ? '' : rawEvidence;
  const commit = isPlaceholderValue(rawCommit) ? '' : rawCommit;

  if (!hypothesis || !action) {
    throw new Error('hypothesis와 action은 비어 있을 수 없습니다.');
  }
  if ((verdict === 'KEEP' || verdict === 'VERIFIED') && !evidence) {
    throw new Error(`${verdict} 판정에는 측정치 또는 실행 근거(evidence)가 필수입니다.`);
  }

  const ts = cleanCell(row?.ts || new Date().toISOString());
  const line = [ts, route, step, hypothesis, action, evidence, verdict, commit].join('\t');
  fs.appendFileSync(tsvPath, `${line}\n`, 'utf8');
  return { ts, route, step, hypothesis, action, evidence, verdict, commit };
}

export function verifyDecisionLog(tsvPath, options = {}) {
  const errors = [];
  if (!fs.existsSync(tsvPath)) {
    return { ok: false, rowCount: 0, errors: [`파일이 없습니다: ${tsvPath}`] };
  }

  let repoValid = false;
  if (options.repo) {
    repoValid = isValidGitRepo(options.repo);
    if (!repoValid) {
      errors.push(`유효한 git 저장소가 아닙니다: ${options.repo}`);
    }
  }

  const strippedEof = fs.readFileSync(tsvPath, 'utf8').replace(/(?:\r?\n)+$/, '');
  if (!strippedEof) {
    return { ok: false, rowCount: 0, errors: [...errors, `빈 파일입니다: ${tsvPath}`] };
  }

  const lines = strippedEof.split(/\r?\n/);
  if (lines[0] !== HEADER) {
    errors.push(`헤더 불일치: 기대값 "${HEADER}"`);
  }

  let rowCount = 0;
  for (let i = 1; i < lines.length; i += 1) {
    const line = lines[i];
    if (line === '') continue;
    rowCount += 1;
    const cols = line.split('\t');
    if (cols.length !== 8) {
      errors.push(`${i + 1}행 컬럼 수 오류(${cols.length}/8)`);
      continue;
    }
    const [, , , hypothesis, action, evidence, verdict, commit] = cols;
    if (!VALID_VERDICTS.has(verdict)) {
      errors.push(`${i + 1}행 잘못된 verdict(${verdict})`);
    }
    if (!hypothesis.trim() || !action.trim()) {
      errors.push(`${i + 1}행 hypothesis 또는 action 누락`);
    }
    if ((verdict === 'KEEP' || verdict === 'VERIFIED') && isPlaceholderValue(evidence)) {
      errors.push(`${i + 1}행 ${verdict} 판정에 실행 근거(evidence) 누락`);
    }
    if (repoValid && !isPlaceholderValue(commit)) {
      try {
        execFileSync('git', ['-C', options.repo, 'rev-parse', '--verify', `${commit.trim()}^{commit}`], {
          stdio: ['ignore', 'pipe', 'ignore']
        });
      } catch {
        errors.push(`${i + 1}행 존재하지 않는 git 커밋(${commit})`);
      }
    }
  }

  return { ok: errors.length === 0, rowCount, errors };
}

function printHelp() {
  console.log('사용법:\n  node decision-log.mjs init <decisions.tsv>\n  node decision-log.mjs append <decisions.tsv> --route <r> --step <s> --hypothesis "<h>" --action "<a>" --evidence "<e>" --verdict KEEP|REVERT|VERIFIED|NOT_VERIFIED|INCONCLUSIVE|NOTE [--commit <sha>]\n  node decision-log.mjs verify <decisions.tsv> [--repo <git_dir>] [--json]');
}

function main() {
  let parsed;
  try {
    parsed = parseCliFlags(process.argv.slice(2), {
      booleans: ['json', 'help'],
      allowedFlags: ['json', 'route', 'step', 'hypothesis', 'action', 'evidence', 'verdict', 'commit', 'ts', 'repo']
    });
  } catch (err) {
    console.error(err.message);
    process.exit(2);
  }

  const subcmd = parsed.positional[0];
  if (!subcmd || parsed.flags.help) {
    printHelp();
    process.exit(0);
  }

  const tsvPath = parsed.positional[1];
  if (!tsvPath) {
    console.error('TSV 파일 경로가 필요합니다.');
    process.exit(2);
  }

  try {
    if (subcmd === 'init') {
      const res = initDecisionLog(tsvPath);
      console.log(parsed.flags.json ? JSON.stringify(res, null, 2) : `초기화 완료(created=${res.created}): ${res.path}`);
      return;
    }
    if (subcmd === 'append') {
      const row = appendDecisionRow(tsvPath, parsed.flags);
      console.log(parsed.flags.json ? JSON.stringify(row, null, 2) : `기록 완료: [${row.verdict}] ${row.step}`);
      return;
    }
    if (subcmd === 'verify') {
      const res = verifyDecisionLog(tsvPath, { repo: parsed.flags.repo });
      if (parsed.flags.json) {
        console.log(JSON.stringify(res, null, 2));
      } else if (res.ok) {
        console.log(`OK: 총 ${res.rowCount}건의 결정 로그가 유효합니다.`);
      } else {
        for (const e of res.errors) console.error(`오류: ${e}`);
      }
      process.exit(res.ok ? 0 : 1);
    }
    console.error(`알 수 없는 하위 명령: ${subcmd}`);
    process.exit(2);
  } catch (err) {
    console.error(err.message);
    process.exit(2);
  }
}

if (isCliMain(import.meta.url)) {
  main();
}
