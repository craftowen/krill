#!/usr/bin/env node
import fs from 'node:fs';
import path from 'node:path';
import { readLedgerEvents, summarizeLedger } from './ledger.mjs';
import { isCliMain, parseCliFlags } from './lib/cli.mjs';
import {
  computeCombinedPatch,
  evaluateVerificationStamp,
  getHeadSha,
  getWorktreeStatus,
  isAncestor,
  isGitRepo,
  isValidStampObject,
  listCommitsBetween,
  readBaseSha,
  resolveCommitSha
} from './lib/git-state.mjs';

function parseListField(val) {
  if (!val) return [];
  if (Array.isArray(val)) return val.map((x) => String(x).trim()).filter(Boolean);
  return String(val)
    .split(/\s*\|\s*|\r?\n/)
    .map((x) => x.trim())
    .filter(Boolean);
}

function readDecisionSummary(tsvFile, repoDir, gitAvailable) {
  if (!fs.existsSync(tsvFile)) {
    return { exists: false, rowCount: 0, byVerdict: {}, recentSteps: [], orphanCommits: [] };
  }
  const lines = fs.readFileSync(tsvFile, 'utf8').replace(/(?:\r?\n)+$/, '').split(/\r?\n/);
  const byVerdict = {};
  const recentSteps = [];
  const orphanCommits = [];

  for (let i = 1; i < lines.length; i += 1) {
    const line = lines[i];
    if (!line) continue;
    const cols = line.split('\t');
    if (cols.length !== 8) continue;
    const [, route, step, , action, , verdict, commit] = cols;
    byVerdict[verdict] = (byVerdict[verdict] ?? 0) + 1;
    recentSteps.push({ route, step, action, verdict, commit: commit || null });

    if (gitAvailable && commit && commit.trim()) {
      const resolved = resolveCommitSha(repoDir, commit.trim());
      if (!resolved || !isAncestor(repoDir, resolved, 'HEAD')) {
        orphanCommits.push(commit.trim());
      }
    }
  }

  return {
    exists: true,
    rowCount: recentSteps.length,
    byVerdict,
    recentSteps: recentSteps.slice(-8),
    orphanCommits: [...new Set(orphanCommits)]
  };
}

export function saveCheckpoint(options = {}) {
  const repoDir = path.resolve(options.repoDir ?? process.cwd());
  if (!fs.existsSync(repoDir) || !fs.statSync(repoDir).isDirectory()) {
    throw new Error(`Target directory does not exist: ${repoDir}`);
  }

  const summary = String(options.summary ?? '').trim();
  if (!summary) {
    throw new Error('Saving a checkpoint requires a one-line status summary (--summary).');
  }

  const krigDir = path.join(repoDir, '.krig');
  const handoffFile = options.file ? path.resolve(options.file) : path.join(krigDir, 'handoff.json');
  fs.mkdirSync(path.dirname(handoffFile), { recursive: true });

  const gitAvailable = isGitRepo(repoDir);
  let gitInfo = null;

  if (gitAvailable) {
    const headSha = getHeadSha(repoDir);
    const worktree = getWorktreeStatus(repoDir);
    const dirtyFiles = [
      ...new Set([
        ...worktree.stagedFiles,
        ...worktree.unstagedFiles,
        ...worktree.untrackedFiles
      ])
    ].sort();

    if (headSha) {
      const { baseSha, source } = readBaseSha(repoDir, options.base);
      const patch = computeCombinedPatch(repoDir, baseSha);
      gitInfo = {
        baseSha,
        baseSource: source,
        headSha,
        patchId: patch.patchId,
        worktreeClean: worktree.isClean,
        dirtyFiles
      };
    } else {
      gitInfo = {
        baseSha: null,
        baseSource: 'unborn',
        headSha: null,
        patchId: null,
        worktreeClean: worktree.isClean,
        dirtyFiles
      };
    }
  }

  const decisions = readDecisionSummary(path.join(krigDir, 'decisions.tsv'), repoDir, gitAvailable);
  const verifiedPath = path.join(krigDir, 'verified.json');
  let verifiedPatchId = null;
  if (fs.existsSync(verifiedPath)) {
    try {
      const parsedStamp = JSON.parse(fs.readFileSync(verifiedPath, 'utf8'));
      if (isValidStampObject(parsedStamp)) {
        verifiedPatchId = parsedStamp.patchId;
      }
    } catch {
      verifiedPatchId = null;
    }
  }

  const record = {
    version: 1,
    timestamp: new Date().toISOString(),
    route: String(options.route ?? 'resume').trim(),
    summary,
    done: parseListField(options.done),
    next: parseListField(options.next),
    blockers: parseListField(options.blockers),
    git: gitInfo,
    decisionsRowCount: decisions.rowCount,
    verifiedPatchId
  };

  fs.writeFileSync(handoffFile, `${JSON.stringify(record, null, 2)}\n`, 'utf8');
  return { ok: true, handoffFile, record };
}

export function inspectSessionState(options = {}) {
  const repoDir = path.resolve(options.repoDir ?? process.cwd());
  if (!fs.existsSync(repoDir) || !fs.statSync(repoDir).isDirectory()) {
    throw new Error(`Target directory does not exist: ${repoDir}`);
  }

  const krigDir = path.join(repoDir, '.krig');
  const handoffFile = options.file ? path.resolve(options.file) : path.join(krigDir, 'handoff.json');
  const gitAvailable = isGitRepo(repoDir);

  let handoff = null;
  if (fs.existsSync(handoffFile)) {
    try {
      handoff = JSON.parse(fs.readFileSync(handoffFile, 'utf8'));
    } catch {
      handoff = { parseError: true };
    }
  }

  const decisions = readDecisionSummary(path.join(krigDir, 'decisions.tsv'), repoDir, gitAvailable);
  const ledgerFile = path.join(krigDir, 'deletion-ledger.jsonl');
  const ledgerSummary = fs.existsSync(ledgerFile) ? summarizeLedger(readLedgerEvents(ledgerFile)) : null;

  const verifiedFile = path.join(krigDir, 'verified.json');
  let verifiedStamp = null;
  if (fs.existsSync(verifiedFile)) {
    try {
      const rawStamp = JSON.parse(fs.readFileSync(verifiedFile, 'utf8'));
      if (isValidStampObject(rawStamp)) {
        verifiedStamp = rawStamp;
      }
    } catch {
      verifiedStamp = null;
    }
  }

  const driftFlags = [];
  let gitState = null;
  let verificationFresh = false;

  if (gitAvailable) {
    const headSha = getHeadSha(repoDir);
    const rawWorktree = getWorktreeStatus(repoDir);
    const worktree = {
      ...rawWorktree,
      dirtyFiles: [
        ...new Set([
          ...rawWorktree.stagedFiles,
          ...rawWorktree.unstagedFiles,
          ...rawWorktree.untrackedFiles
        ])
      ].sort()
    };

    if (worktree.stagedKrigFiles.length > 0) {
      driftFlags.push({
        code: 'STAGED_KRIG_ARTIFACT',
        detail: `.krig runtime files are staged in git (${worktree.stagedKrigFiles.join(', ')})`
      });
    }

    if (headSha) {
      const { baseSha, source: baseSource } = readBaseSha(repoDir, options.base);
      let effectiveBaseSha = baseSha;
      let patch = computeCombinedPatch(repoDir, baseSha);

      if (verifiedStamp) {
        const evalRes = evaluateVerificationStamp({
          repoDir,
          stamp: verifiedStamp,
          baseSha,
          currentPatch: patch,
          explicitBase: options.base ?? null
        });
        verificationFresh = evalRes.fresh;
        if (evalRes.matchedBaseSha && evalRes.matchedBaseSha !== baseSha) {
          effectiveBaseSha = evalRes.matchedBaseSha;
          patch = computeCombinedPatch(repoDir, effectiveBaseSha);
        }
        if (!verificationFresh) {
          driftFlags.push({
            code: 'STALE_VERIFICATION',
            detail: 'Code changed after the last verify stamp; re-verification is required.'
          });
        }
      }

      if (handoff && !handoff.parseError && handoff.git) {
        if (handoff.git.headSha && handoff.git.headSha !== headSha) {
          const fastForward = isAncestor(repoDir, handoff.git.headSha, headSha);
          driftFlags.push({
            code: fastForward ? 'HEAD_ADVANCED' : 'HEAD_REWRITTEN',
            detail: fastForward
              ? `New commits were added after the checkpoint (${handoff.git.headSha.slice(0, 8)}..${headSha.slice(0, 8)})`
              : `Checkpoint commit (${handoff.git.headSha.slice(0, 8)}) is not an ancestor of current HEAD (rebased or reset)`
          });
        }
        if (handoff.git.patchId) {
          const handoffEval = evaluateVerificationStamp({
            repoDir,
            stamp: { patchId: handoff.git.patchId, baseSha: handoff.git.baseSha },
            baseSha: effectiveBaseSha,
            currentPatch: patch,
            explicitBase: options.base ?? null
          });
          if (!handoffEval.fresh) {
            driftFlags.push({
              code: 'PATCH_DRIFT_FROM_HANDOFF',
              detail: 'Working tree or commit diff changed since the checkpoint was saved.'
            });
          } else if (handoffEval.matchedBaseSha && handoffEval.matchedBaseSha !== effectiveBaseSha) {
            effectiveBaseSha = handoffEval.matchedBaseSha;
            patch = computeCombinedPatch(repoDir, effectiveBaseSha);
          }
        }
      }

      const commitsSinceBase = listCommitsBetween(repoDir, effectiveBaseSha, headSha);
      gitState = {
        baseSha: effectiveBaseSha,
        baseSource,
        headSha,
        currentPatchId: patch.patchId,
        worktree,
        commitsSinceBase
      };

      if (decisions.orphanCommits.length > 0) {
        driftFlags.push({
          code: 'ORPHAN_DECISION_COMMITS',
          detail: `Commits recorded in decisions.tsv are missing from the current branch: ${decisions.orphanCommits.join(', ')}`
        });
      }
    } else {
      gitState = {
        baseSha: null,
        baseSource: 'unborn',
        headSha: null,
        currentPatchId: null,
        worktree,
        commitsSinceBase: []
      };
    }
  }

  const hasStagedKrig = Boolean(gitState && gitState.worktree.stagedKrigFiles.length > 0);
  const hasDirtyOrCommits = Boolean(
    gitState && (gitState.worktree.dirtyNonKrigCount > 0 || gitState.commitsSinceBase.length > 0 || hasStagedKrig)
  );

  let recommendedAction = 'CLEAN_START';
  if (handoff && Array.isArray(handoff.blockers) && handoff.blockers.length > 0) {
    recommendedAction = 'RESOLVE_BLOCKER';
  } else if (verificationFresh && !hasStagedKrig && (!handoff || !handoff.next || handoff.next.length === 0)) {
    recommendedAction = 'READY_TO_SHIP';
  } else if (handoff || decisions.exists || hasDirtyOrCommits) {
    recommendedAction = (handoff && handoff.next && handoff.next.length > 0) ? 'CONTINUE_DRIVE' : 'NEEDS_VERIFY';
  }

  return {
    repoDir,
    gitAvailable,
    hasHandoff: Boolean(handoff && !handoff.parseError),
    handoff,
    gitState,
    decisions,
    ledgerSummary,
    hasVerifiedStamp: Boolean(verifiedStamp),
    verificationFresh,
    driftFlags,
    recommendedAction
  };
}

function printHelp() {
  console.log([
    'Usage:',
    '  node checkpoint.mjs status [--repo <dir>] [--base <rev>] [--file <path>] [--json]',
    '  node checkpoint.mjs save [--repo <dir>] [--base <rev>] [--file <path>] --summary "<summary>" [--route <route>] [--done "item1|item2"] [--next "item1|item2"] [--blockers "reason"] [--json]'
  ].join('\n'));
}

function main() {
  let parsed;
  try {
    parsed = parseCliFlags(process.argv.slice(2), {
      booleans: ['json', 'help'],
      allowedFlags: ['json', 'repo', 'base', 'file', 'route', 'summary', 'done', 'next', 'blockers']
    });
  } catch (err) {
    console.error(err.message);
    process.exit(2);
  }

  if (parsed.flags.help) {
    printHelp();
    process.exit(0);
  }

  const sub = parsed.positional[0] ?? 'status';
  if (!['status', 'save'].includes(sub)) {
    printHelp();
    process.exit(2);
  }

  try {
    if (sub === 'save') {
      const res = saveCheckpoint({
        repoDir: parsed.flags.repo,
        base: parsed.flags.base,
        file: parsed.flags.file,
        route: parsed.flags.route,
        summary: parsed.flags.summary,
        done: parsed.flags.done,
        next: parsed.flags.next,
        blockers: parsed.flags.blockers
      });
      if (parsed.flags.json) {
        console.log(JSON.stringify(res, null, 2));
      } else {
        console.log(`Checkpoint saved: ${res.handoffFile}`);
      }
      process.exit(0);
    }

    const state = inspectSessionState({
      repoDir: parsed.flags.repo,
      base: parsed.flags.base,
      file: parsed.flags.file
    });
    if (parsed.flags.json) {
      console.log(JSON.stringify(state, null, 2));
    } else {
      console.log(`Session recovery status: action=${state.recommendedAction}, handoff=${state.hasHandoff}, verificationFresh=${state.verificationFresh}`);
      if (state.handoff && !state.handoff.parseError) {
        console.log(`  Summary: ${state.handoff.summary}`);
        if (state.handoff.next?.length > 0) console.log(`  Next: ${state.handoff.next.join(', ')}`);
        if (state.handoff.blockers?.length > 0) console.log(`  Blockers: ${state.handoff.blockers.join(', ')}`);
      }
      if (state.driftFlags.length > 0) {
        for (const d of state.driftFlags) {
          console.log(`  Drift [${d.code}]: ${d.detail}`);
        }
      }
    }
    process.exit(0);
  } catch (err) {
    console.error(err.message);
    process.exit(2);
  }
}

if (isCliMain(import.meta.url)) {
  main();
}
