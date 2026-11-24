#!/usr/bin/env node
import fs from 'node:fs';
import path from 'node:path';
import { isCliMain, parseCliFlags } from './lib/cli.mjs';

const FORBIDDEN_BLIND_WORDS = Object.freeze([
  'eval',
  'evaluation',
  'benchmark',
  'rubric',
  'judge',
  'experiment',
  'candidate',
  'arena',
  'scoreboard',
  'treatment',
  'control-arm',
  'baseline-arm',
  '벤치마크',
  '루브릭',
  '채점',
  '심사위원',
  '대조군',
  '실험군',
  '평가셋'
]);

const BLIND_WORD_RE = new RegExp(
  `(?:^|[^A-Za-z0-9가-힣])(${FORBIDDEN_BLIND_WORDS.map((w) => w.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')).join('|')})(?=$|[^A-Za-z0-9가-힣])`,
  'i'
);

export function checkBlinding({ workspacePaths = [], promptText = '' } = {}) {
  const leaks = [];

  const promptMatch = BLIND_WORD_RE.exec(String(promptText ?? ''));
  if (promptMatch) {
    leaks.push({
      surface: 'prompt',
      token: promptMatch[1],
      detail: `Candidate prompt exposes evaluation meta-cue ('${promptMatch[1]}').`
    });
  }

  for (const rawDir of workspacePaths) {
    const absDir = path.resolve(rawDir);
    const dirMatch = BLIND_WORD_RE.exec(absDir);
    if (dirMatch) {
      leaks.push({
        surface: 'workspace-path',
        token: dirMatch[1],
        detail: `Workspace directory path (${absDir}) contains evaluation meta-cue ('${dirMatch[1]}').`
      });
    }

    if (fs.existsSync(absDir) && fs.statSync(absDir).isDirectory()) {
      const stack = [absDir];
      while (stack.length > 0) {
        const cur = stack.pop();
        for (const entry of fs.readdirSync(cur, { withFileTypes: true })) {
          if (entry.name === '.git' || entry.name === 'node_modules') continue;
          const rel = path.relative(absDir, path.join(cur, entry.name));
          const nameMatch = BLIND_WORD_RE.exec(entry.name);
          if (nameMatch) {
            leaks.push({
              surface: 'filename',
              token: nameMatch[1],
              detail: `Filename inside candidate workspace (${rel}) contains evaluation meta-cue ('${nameMatch[1]}').`
            });
          }
          if (entry.isDirectory()) {
            stack.push(path.join(cur, entry.name));
          }
        }
      }
    }
  }

  return {
    ok: leaks.length === 0,
    leaks
  };
}

function median(values) {
  if (values.length === 0) return 0;
  const sorted = [...values].sort((a, b) => a - b);
  const mid = Math.floor(sorted.length / 2);
  return sorted.length % 2 === 1 ? sorted[mid] : (sorted[mid - 1] + sorted[mid]) / 2;
}

function round1(num) {
  return Math.round(num * 10) / 10;
}

function renderBar(ratio, width = 20) {
  const clamped = Math.max(0, Math.min(1, Number.isFinite(ratio) ? ratio : 0));
  const filled = Math.round(clamped * width);
  return `${'#'.repeat(filled)}${'.'.repeat(width - filled)}`;
}

export function compareAbRuns({ control = [], treatment = [], metadata = {} } = {}) {
  if (!Array.isArray(control) || !Array.isArray(treatment) || control.length === 0) {
    throw new Error('Non-empty control and treatment result arrays are required for comparison.');
  }
  if (control.length !== treatment.length) {
    throw new Error(`Task count mismatch between control (${control.length}) and treatment (${treatment.length}).`);
  }

  const controlMap = new Map(control.map((r) => [r.taskId, r]));
  const treatmentMap = new Map(treatment.map((r) => [r.taskId, r]));

  for (const id of controlMap.keys()) {
    if (!treatmentMap.has(id)) {
      throw new Error(`Treatment results are missing task '${id}' (cherry-picking forbidden).`);
    }
  }

  const pairedTasks = [];
  let controlPassedTotal = 0;
  let controlAssertionsTotal = 0;
  let treatmentPassedTotal = 0;
  let treatmentAssertionsTotal = 0;

  let controlDefectsTotal = 0;
  let treatmentDefectsTotal = 0;

  let treatmentWins = 0;
  let ties = 0;
  let controlWins = 0;

  const passRateDeltas = [];

  for (const taskId of [...controlMap.keys()]) {
    const c = controlMap.get(taskId);
    const t = treatmentMap.get(taskId);

    const cTotal = Number(c.totalAssertions ?? 0);
    const tTotal = Number(t.totalAssertions ?? 0);
    if (cTotal <= 0 || cTotal !== tTotal) {
      throw new Error(`Total oracle assertion count mismatch for task '${taskId}' (${cTotal} vs ${tTotal}).`);
    }

    const cPassed = Number(c.passedAssertions ?? 0);
    const tPassed = Number(t.passedAssertions ?? 0);
    const cDefects = Number(c.defectEscapes ?? 0);
    const tDefects = Number(t.defectEscapes ?? 0);

    controlPassedTotal += cPassed;
    controlAssertionsTotal += cTotal;
    treatmentPassedTotal += tPassed;
    treatmentAssertionsTotal += tTotal;

    controlDefectsTotal += cDefects;
    treatmentDefectsTotal += tDefects;

    const cRate = (cPassed / cTotal) * 100;
    const tRate = (tPassed / tTotal) * 100;
    const deltaPctPoints = round1(tRate - cRate);
    passRateDeltas.push(deltaPctPoints);

    let outcome = 'tie';
    if (tPassed > cPassed || (tPassed === cPassed && tDefects < cDefects)) {
      outcome = 'treatment_win';
      treatmentWins += 1;
    } else if (cPassed > tPassed || (cPassed === tPassed && cDefects < tDefects)) {
      outcome = 'control_win';
      controlWins += 1;
    } else {
      ties += 1;
    }

    pairedTasks.push({
      taskId,
      category: t.category ?? c.category ?? 'general',
      control: {
        passed: cPassed,
        total: cTotal,
        passRatePct: round1(cRate),
        defectEscapes: cDefects,
        prodLoc: c.prodLoc ?? null,
        durationMs: c.durationMs ?? null
      },
      treatment: {
        passed: tPassed,
        total: tTotal,
        passRatePct: round1(tRate),
        defectEscapes: tDefects,
        prodLoc: t.prodLoc ?? null,
        durationMs: t.durationMs ?? null
      },
      deltaPassRatePctPoints: deltaPctPoints,
      deltaDefectEscapes: tDefects - cDefects,
      outcome
    });
  }

  const controlOverallRate = round1((controlPassedTotal / controlAssertionsTotal) * 100);
  const treatmentOverallRate = round1((treatmentPassedTotal / treatmentAssertionsTotal) * 100);
  const liftPctPoints = round1(treatmentOverallRate - controlOverallRate);
  const defectReductionPct = controlDefectsTotal > 0
    ? round1(((controlDefectsTotal - treatmentDefectsTotal) / controlDefectsTotal) * 100)
    : (treatmentDefectsTotal === 0 ? 0 : -100);

  return {
    metadata: {
      taskCount: pairedTasks.length,
      totalOracleAssertions: controlAssertionsTotal,
      model: metadata.model ?? 'unspecified',
      evaluatedAt: metadata.evaluatedAt ?? new Date().toISOString()
    },
    aggregate: {
      controlPassed: controlPassedTotal,
      treatmentPassed: treatmentPassedTotal,
      totalAssertions: controlAssertionsTotal,
      controlPassRatePct: controlOverallRate,
      treatmentPassRatePct: treatmentOverallRate,
      passRateLiftPctPoints: liftPctPoints,
      medianTaskLiftPctPoints: round1(median(passRateDeltas)),
      controlDefectEscapes: controlDefectsTotal,
      treatmentDefectEscapes: treatmentDefectsTotal,
      defectReductionPct,
      treatmentWins,
      ties,
      controlWins
    },
    pairedTasks
  };
}

export function formatAbScoreboard(report) {
  const a = report.aggregate;
  const m = report.metadata;
  const cBar = renderBar(a.controlPassRatePct / 100);
  const tBar = renderBar(a.treatmentPassRatePct / 100);
  const maxDefects = Math.max(1, a.controlDefectEscapes, a.treatmentDefectEscapes);
  const cDefBar = renderBar(a.controlDefectEscapes / maxDefects);
  const tDefBar = renderBar(a.treatmentDefectEscapes / maxDefects);
  const sign = a.passRateLiftPctPoints >= 0 ? '+' : '';

  const lines = [
    `krig A/B quantitative evaluation (${m.taskCount} tasks, ${m.totalOracleAssertions} held-out assertions, model=${m.model})`,
    '',
    `  Oracle Pass Rate  no-skill  [${cBar}] ${a.controlPassRatePct}% (${a.controlPassed}/${a.totalAssertions})`,
    `                    with-krig [${tBar}] ${a.treatmentPassRatePct}% (${a.treatmentPassed}/${a.totalAssertions})  (${sign}${a.passRateLiftPctPoints}%p)`,
    `  Defect Escapes    no-skill  [${cDefBar}] ${a.controlDefectEscapes}`,
    `                    with-krig [${tDefBar}] ${a.treatmentDefectEscapes}  (reduction ${a.defectReductionPct}%)`,
    `  Paired Outcomes   with-krig wins=${a.treatmentWins}, ties=${a.ties}, no-skill wins=${a.controlWins}`,
    '',
    '  Per-Task Breakdown:'
  ];

  for (const row of report.pairedTasks) {
    const dSign = row.deltaPassRatePctPoints >= 0 ? '+' : '';
    lines.push(
      `    ${row.taskId}: no-skill ${row.control.passed}/${row.control.total} (defects=${row.control.defectEscapes}) vs with-krig ${row.treatment.passed}/${row.treatment.total} (defects=${row.treatment.defectEscapes}) [${dSign}${row.deltaPassRatePctPoints}%p, ${row.outcome}]`
    );
  }

  return lines.join('\n');
}

function printHelp() {
  console.log([
    'Usage:',
    '  node ab-eval.mjs check-blinding [--prompt "<text>"] [--dir <path...>] [--json]',
    '  node ab-eval.mjs compare --control <control.json> --treatment <treatment.json> [--model "<model_name>"] [--json]'
  ].join('\n'));
}

function main() {
  const rawArgs = process.argv.slice(2);
  const dirs = [];
  const filteredArgs = [];
  for (let i = 0; i < rawArgs.length; i += 1) {
    if (rawArgs[i] === '--dir' && rawArgs[i + 1] !== undefined) {
      dirs.push(rawArgs[i + 1]);
      i += 1;
    } else if (rawArgs[i].startsWith('--dir=')) {
      dirs.push(rawArgs[i].slice('--dir='.length));
    } else {
      filteredArgs.push(rawArgs[i]);
    }
  }

  let parsed;
  try {
    parsed = parseCliFlags(filteredArgs, {
      booleans: ['json', 'help'],
      allowedFlags: ['json', 'prompt', 'control', 'treatment', 'model']
    });
  } catch (err) {
    console.error(err.message);
    process.exit(2);
  }

  const sub = parsed.positional[0];
  if (!sub || parsed.flags.help) {
    printHelp();
    process.exit(0);
  }

  try {
    if (sub === 'check-blinding') {
      const res = checkBlinding({
        workspacePaths: [...dirs, ...parsed.positional.slice(1)],
        promptText: parsed.flags.prompt ?? ''
      });
      if (parsed.flags.json) {
        console.log(JSON.stringify(res, null, 2));
      } else if (res.ok) {
        console.log('OK: no blinding leaks found in candidate paths, filenames, or prompts.');
      } else {
        for (const leak of res.leaks) {
          console.error(`[BLIND_LEAK:${leak.surface}] ${leak.detail}`);
        }
      }
      process.exit(res.ok ? 0 : 1);
    }

    if (sub === 'compare') {
      if (!parsed.flags.control || !parsed.flags.treatment) {
        console.error('Both --control <file> and --treatment <file> are required.');
        process.exit(2);
      }
      const controlData = JSON.parse(fs.readFileSync(path.resolve(parsed.flags.control), 'utf8'));
      const treatmentData = JSON.parse(fs.readFileSync(path.resolve(parsed.flags.treatment), 'utf8'));
      const report = compareAbRuns({
        control: Array.isArray(controlData) ? controlData : controlData.results,
        treatment: Array.isArray(treatmentData) ? treatmentData : treatmentData.results,
        metadata: { model: parsed.flags.model ?? 'unspecified' }
      });

      if (parsed.flags.json) {
        console.log(JSON.stringify(report, null, 2));
      } else {
        console.log(formatAbScoreboard(report));
      }
      process.exit(0);
    }

    printHelp();
    process.exit(2);
  } catch (err) {
    console.error(err.message);
    process.exit(2);
  }
}

if (isCliMain(import.meta.url)) {
  main();
}
