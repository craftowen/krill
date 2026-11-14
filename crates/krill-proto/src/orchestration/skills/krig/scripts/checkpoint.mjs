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
    throw new Error(`대상 디렉터리가 존재하지 않습니다: ${repoDir}`);
  }

  const summary = String(options.summary ?? '').trim();
  if (!summary) {
    throw new Error('체크포인트 저장에는 현재 상태 한 줄 요약(--summary)이 필수입니다.');
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
    throw new Error(`대상 디렉터리가 존재하지 않습니다: ${repoDir}`);
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
        detail: `.krig 런타임 파일이 스테이징되어 있습니다(${worktree.stagedKrigFiles.join(', ')})`
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
            detail: '마지막 verify 스탬프 이후 코드가 수정되어 재검증이 필요합니다.'
          });
        }
      }

      if (handoff && !handoff.parseError && handoff.git) {
        if (handoff.git.headSha && handoff.git.headSha !== headSha) {
          const fastForward = isAncestor(repoDir, handoff.git.headSha, headSha);
          driftFlags.push({
            code: fastForward ? 'HEAD_ADVANCED' : 'HEAD_REWRITTEN',
            detail: fastForward
              ? `체크포인트 이후 새 커밋이 추가되었습니다(${handoff.git.headSha.slice(0, 8)}..${headSha.slice(0, 8)})`
              : `체크포인트 커밋(${handoff.git.headSha.slice(0, 8)})이 현재 HEAD의 조상이 아닙니다(리베이스 또는 리셋 발생)`
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
              detail: '체크포인트 저장 시점 이후 작업 트리 또는 커밋 diff가 변경되었습니다.'
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
          detail: `decisions.tsv에 기록된 커밋 중 현재 브랜치에 없는 항목이 있습니다: ${decisions.orphanCommits.join(', ')}`
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
    '사용법:',
    '  node checkpoint.mjs status [--repo <dir>] [--base <rev>] [--file <경로>] [--json]',
    '  node checkpoint.mjs save [--repo <dir>] [--base <rev>] [--file <경로>] --summary "<요약>" [--route <라우트>] [--done "항목1|항목2"] [--next "항목1|항목2"] [--blockers "사유"] [--json]'
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
        console.log(`체크포인트 저장 완료: ${res.handoffFile}`);
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
      console.log(`세션 복구 진단: 권고=${state.recommendedAction}, 핸드오프=${state.hasHandoff}, 검증신선도=${state.verificationFresh}`);
      if (state.handoff && !state.handoff.parseError) {
        console.log(`  요약: ${state.handoff.summary}`);
        if (state.handoff.next?.length > 0) console.log(`  남은 작업: ${state.handoff.next.join(', ')}`);
        if (state.handoff.blockers?.length > 0) console.log(`  차단 요인: ${state.handoff.blockers.join(', ')}`);
      }
      if (state.driftFlags.length > 0) {
        for (const d of state.driftFlags) {
          console.log(`  드리프트 [${d.code}]: ${d.detail}`);
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
