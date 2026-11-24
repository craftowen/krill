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
    throw new Error(`Invalid verdict (${row?.verdict}). Allowed: ${[...VALID_VERDICTS].join(', ')}`);
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
    throw new Error('hypothesis and action cannot be empty.');
  }
  if ((verdict === 'KEEP' || verdict === 'VERIFIED') && !evidence) {
    throw new Error(`Verdict ${verdict} requires measurement or execution evidence.`);
  }

  const ts = cleanCell(row?.ts || new Date().toISOString());
  const line = [ts, route, step, hypothesis, action, evidence, verdict, commit].join('\t');
  fs.appendFileSync(tsvPath, `${line}\n`, 'utf8');
  return { ts, route, step, hypothesis, action, evidence, verdict, commit };
}

export function verifyDecisionLog(tsvPath, options = {}) {
  const errors = [];
  if (!fs.existsSync(tsvPath)) {
    return { ok: false, rowCount: 0, errors: [`File not found: ${tsvPath}`] };
  }

  let repoValid = false;
  if (options.repo) {
    repoValid = isValidGitRepo(options.repo);
    if (!repoValid) {
      errors.push(`Not a valid git repository: ${options.repo}`);
    }
  }

  const strippedEof = fs.readFileSync(tsvPath, 'utf8').replace(/(?:\r?\n)+$/, '');
  if (!strippedEof) {
    return { ok: false, rowCount: 0, errors: [...errors, `Empty file: ${tsvPath}`] };
  }

  const lines = strippedEof.split(/\r?\n/);
  if (lines[0] !== HEADER) {
    errors.push(`Header mismatch: expected "${HEADER}"`);
  }

  let rowCount = 0;
  for (let i = 1; i < lines.length; i += 1) {
    const line = lines[i];
    if (line === '') continue;
    rowCount += 1;
    const cols = line.split('\t');
    if (cols.length !== 8) {
      errors.push(`Line ${i + 1}: wrong column count (${cols.length}/8)`);
      continue;
    }
    const [, , , hypothesis, action, evidence, verdict, commit] = cols;
    if (!VALID_VERDICTS.has(verdict)) {
      errors.push(`Line ${i + 1}: invalid verdict (${verdict})`);
    }
    if (!hypothesis.trim() || !action.trim()) {
      errors.push(`Line ${i + 1}: missing hypothesis or action`);
    }
    if ((verdict === 'KEEP' || verdict === 'VERIFIED') && isPlaceholderValue(evidence)) {
      errors.push(`Line ${i + 1}: verdict ${verdict} is missing execution evidence`);
    }
    if (repoValid && !isPlaceholderValue(commit)) {
      try {
        execFileSync('git', ['-C', options.repo, 'rev-parse', '--verify', `${commit.trim()}^{commit}`], {
          stdio: ['ignore', 'pipe', 'ignore']
        });
      } catch {
        errors.push(`Line ${i + 1}: git commit not found (${commit})`);
      }
    }
  }

  return { ok: errors.length === 0, rowCount, errors };
}

function printHelp() {
  console.log('Usage:\n  node decision-log.mjs init <decisions.tsv>\n  node decision-log.mjs append <decisions.tsv> --route <r> --step <s> --hypothesis "<h>" --action "<a>" --evidence "<e>" --verdict KEEP|REVERT|VERIFIED|NOT_VERIFIED|INCONCLUSIVE|NOTE [--commit <sha>]\n  node decision-log.mjs verify <decisions.tsv> [--repo <git_dir>] [--json]');
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
    console.error('TSV file path is required.');
    process.exit(2);
  }

  try {
    if (subcmd === 'init') {
      const res = initDecisionLog(tsvPath);
      console.log(parsed.flags.json ? JSON.stringify(res, null, 2) : `Initialized (created=${res.created}): ${res.path}`);
      return;
    }
    if (subcmd === 'append') {
      const row = appendDecisionRow(tsvPath, parsed.flags);
      console.log(parsed.flags.json ? JSON.stringify(row, null, 2) : `Appended: [${row.verdict}] ${row.step}`);
      return;
    }
    if (subcmd === 'verify') {
      const res = verifyDecisionLog(tsvPath, { repo: parsed.flags.repo });
      if (parsed.flags.json) {
        console.log(JSON.stringify(res, null, 2));
      } else if (res.ok) {
        console.log(`OK: ${res.rowCount} decision log row(s) verified.`);
      } else {
        for (const e of res.errors) console.error(`Error: ${e}`);
      }
      process.exit(res.ok ? 0 : 1);
    }
    console.error(`Unknown subcommand: ${subcmd}`);
    process.exit(2);
  } catch (err) {
    console.error(err.message);
    process.exit(2);
  }
}

if (isCliMain(import.meta.url)) {
  main();
}
