#!/usr/bin/env node
import fs from 'node:fs';
import path from 'node:path';
import { isCliMain, parseCliFlags } from './lib/cli.mjs';
import {
  computeCombinedPatch,
  ensureGitRepo,
  evaluateVerificationStamp,
  getHeadSha,
  getWorktreeStatus,
  isValidStampObject,
  readBaseSha,
  scanDiffHazards
} from './lib/git-state.mjs';
import { stripInlineCode, updateFenceState } from './lib/markdown.mjs';

const PLACEHOLDER_EVIDENCE = new Set(['', '-', 'n/a', 'na', 'none', 'null', 'todo', 'tbd', 'pending']);
const CONVENTIONAL_TITLE_RE = /^(?:feat|fix|docs|refactor|test|chore|perf|build|ci)(?:\([a-zA-Z0-9._/-]+\))?!?:\s+\S[^\r\n]*$/;
const REQUIRED_PR_SECTIONS = ['Why', 'What changed', 'Scope', 'Blast Radius', 'Verification'];
const FORBIDDEN_BRIEF_CHARS = ['\u2014', '\u2013', '\u00b7', '\u2192', '\u2194'];

function defaultStampPath(repoDir) {
  return path.join(repoDir, '.krig', 'verified.json');
}

export function stampVerification(options = {}) {
  const repoDir = ensureGitRepo(options.repoDir ?? process.cwd());
  const evidence = String(options.evidence ?? '').trim();
  if (PLACEHOLDER_EVIDENCE.has(evidence.toLowerCase())) {
    throw new Error('Verification stamp requires an actual execution summary (--evidence).');
  }

  const { baseSha, source: baseSource } = readBaseSha(repoDir, options.base);
  const headSha = getHeadSha(repoDir);
  const worktree = getWorktreeStatus(repoDir);

  if (worktree.stagedKrigFiles.length > 0) {
    throw new Error(
      `.krig temporary files are staged in the git index (${worktree.stagedKrigFiles.join(', ')}). Unstage them first with: git reset HEAD .krig`
    );
  }

  const patch = computeCombinedPatch(repoDir, baseSha);
  if (patch.patchId === 'empty-diff') {
    throw new Error(
      `Diff against base commit (${baseSha.slice(0, 8)}) is empty; cannot mint a verification stamp. Specify --base <commit>.`
    );
  }

  const hazards = scanDiffHazards(patch.diffText, repoDir);
  if (hazards.length > 0) {
    throw new Error(`Diff contains ship-blocking hazards: ${hazards.map((h) => `${h.kind}(${h.file})`).join(', ')}`);
  }

  const stampFile = options.stampFile ? path.resolve(options.stampFile) : defaultStampPath(repoDir);
  fs.mkdirSync(path.dirname(stampFile), { recursive: true });

  const record = {
    version: 1,
    timestamp: new Date().toISOString(),
    baseSha,
    baseSource,
    headSha,
    patchId: patch.patchId,
    worktreeCleanAtStamp: worktree.isClean,
    evidence
  };

  fs.writeFileSync(stampFile, `${JSON.stringify(record, null, 2)}\n`, 'utf8');
  return { ok: true, stampFile, record };
}

export function verifyShipReadiness(options = {}) {
  const repoDir = ensureGitRepo(options.repoDir ?? process.cwd());
  const requireClean = Boolean(options.requireClean);
  const requireVerified = Boolean(options.requireVerified);
  const stampFile = options.stampFile ? path.resolve(options.stampFile) : defaultStampPath(repoDir);

  const issues = [];
  const worktree = getWorktreeStatus(repoDir);
  const headSha = getHeadSha(repoDir);
  const { baseSha } = readBaseSha(repoDir, options.base);

  if (worktree.stagedKrigFiles.length > 0) {
    issues.push({
      code: 'STAGED_KRIG_ARTIFACT',
      message: `.krig runtime files are staged for commit: ${worktree.stagedKrigFiles.join(', ')}`,
      fix: 'Unstage them with: git reset HEAD .krig'
    });
  }

  if (requireClean && !worktree.isClean) {
    const dirtySummary = [
      ...worktree.stagedFiles.map((f) => `staged:${f}`),
      ...worktree.unstagedFiles.map((f) => `modified:${f}`),
      ...worktree.untrackedFiles.map((f) => `untracked:${f}`)
    ];
    if (dirtySummary.length > 0) {
      issues.push({
        code: 'UNCOMMITTED_CHANGES',
        message: `Uncommitted changes remain (${dirtySummary.slice(0, 6).join(', ')})`,
        fix: 'Commit intended changes in logical commits or remove stray temporary files.'
      });
    }
  }

  let currentPatch = computeCombinedPatch(repoDir, baseSha);

  let stamp = null;
  let verificationFresh = false;
  let effectiveBaseSha = baseSha;

  if (fs.existsSync(stampFile)) {
    let rawParsed = null;
    let parseFailed = false;
    try {
      rawParsed = JSON.parse(fs.readFileSync(stampFile, 'utf8'));
    } catch {
      parseFailed = true;
    }

    if (parseFailed || !isValidStampObject(rawParsed)) {
      issues.push({
        code: 'CORRUPT_VERIFICATION_STAMP',
        message: `Verification stamp JSON or schema is corrupted: ${stampFile}`,
        fix: 'Re-run verification and refresh with ship-check.mjs stamp.'
      });
    } else {
      stamp = rawParsed;
      const evalRes = evaluateVerificationStamp({
        repoDir,
        stamp,
        baseSha,
        currentPatch,
        explicitBase: options.base ?? null
      });
      verificationFresh = evalRes.fresh;
      if (evalRes.matchedBaseSha && evalRes.matchedBaseSha !== baseSha) {
        effectiveBaseSha = evalRes.matchedBaseSha;
        currentPatch = computeCombinedPatch(repoDir, effectiveBaseSha);
      }
      if (!verificationFresh) {
        issues.push({
          code: 'STALE_VERIFICATION',
          message: `Code changed after verification stamp (${stamp.patchId.slice(0, 12)}) and no longer matches current patch (${currentPatch.patchId.slice(0, 12)}).`,
          fix: 'Re-run tests and the verify stage on the updated code, then mint a fresh stamp.'
        });
      }
    }
  } else if (requireVerified) {
    issues.push({
      code: 'MISSING_VERIFICATION_STAMP',
      message: 'No verification stamp (.krig/verified.json) was found.',
      fix: 'Complete the verify playbook and run ship-check.mjs stamp --evidence "...".'
    });
  }

  const hazards = scanDiffHazards(currentPatch.diffText, repoDir);
  for (const h of hazards) {
    issues.push({
      code: h.kind,
      message: `${h.file}: ${h.detail}`,
      fix: h.kind === 'MERGE_CONFLICT_MARKER'
        ? 'Resolve all merge conflict markers and re-run tests.'
        : 'Remove hardcoded secrets and move them to environment variables or keychain storage.'
    });
  }

  return {
    ok: issues.length === 0,
    repoDir,
    baseSha: effectiveBaseSha,
    headSha,
    currentPatchId: currentPatch.patchId,
    worktreeClean: worktree.isClean,
    hasStamp: Boolean(stamp),
    verificationFresh,
    stamp,
    issues
  };
}

export function validatePrBriefing({ title, body, maxLines = 40 }) {
  const parsedMax = Number(maxLines);
  if (!Number.isInteger(parsedMax) || parsedMax < 5) {
    throw new Error(`Invalid maxLines value (integer >= 5 required): ${maxLines}`);
  }

  const issues = [];
  const rawTitleStr = String(title ?? '');
  const hasTitleNewline = /[\r\n]/.test(rawTitleStr.trim());
  const cleanTitle = rawTitleStr.trim();

  if (!cleanTitle) {
    issues.push({
      code: 'EMPTY_PR_TITLE',
      message: 'PR title is empty.',
      fix: 'Provide a title in type(scope): subject format.'
    });
  } else {
    if (hasTitleNewline || !CONVENTIONAL_TITLE_RE.test(cleanTitle)) {
      issues.push({
        code: 'INVALID_CONVENTIONAL_TITLE',
        message: `PR title must be a single-line Conventional Commit header: ${cleanTitle.replace(/[\r\n]+/g, '\\n')}`,
        fix: 'Write a single-line header such as feat(scope): imperative summary.'
      });
    }
    if (cleanTitle.endsWith('.')) {
      issues.push({
        code: 'TITLE_TRAILING_PERIOD',
        message: 'PR title ends with a trailing period.',
        fix: 'Remove the trailing period from the title.'
      });
    }
    if (cleanTitle.length > 72) {
      issues.push({
        code: 'TITLE_TOO_LONG',
        message: `PR title length (${cleanTitle.length} chars) exceeds 72 characters.`,
        fix: 'Shorten the title to 72 characters or fewer around the key changed symbol.'
      });
    }
  }

  const rawBody = String(body ?? '').replace(/\r\n/g, '\n').trim();
  const lines = rawBody ? rawBody.split('\n') : [];

  if (lines.length === 0) {
    issues.push({
      code: 'EMPTY_PR_BODY',
      message: 'PR briefing body is empty.',
      fix: 'Include ## Why, ## What changed, ## Scope, ## Blast Radius, and ## Verification sections.'
    });
    return { ok: false, lineCount: 0, issues };
  }

  if (lines.length > parsedMax) {
    issues.push({
      code: 'PR_BODY_TOO_LONG',
      message: `PR body line count (${lines.length}) exceeds the limit (${parsedMax}).`,
      fix: `Trim the body to ${parsedMax} lines or fewer so it fits as a 1-minute squash commit message.`
    });
  }

  const presentHeadings = new Set();
  let fence = null;
  for (let i = 0; i < lines.length; i += 1) {
    const line = lines[i];
    const step = updateFenceState(fence, line);
    if (step.toggled) {
      fence = step.nextState;
      continue;
    }
    if (fence) continue;

    const h2 = /^##\s+(.+)$/.exec(line.trim());
    if (h2) {
      presentHeadings.add(h2[1].trim());
    }

    const proseOnly = stripInlineCode(line);
    if (FORBIDDEN_BRIEF_CHARS.some((ch) => proseOnly.includes(ch))) {
      issues.push({
        code: 'FORBIDDEN_BRIEF_PUNCTUATION',
        message: `Body line ${i + 1} prose contains forbidden punctuation (middle dot, dash, or arrow).`,
        fix: 'Replace with commas, periods, colons, or slashes.'
      });
    }
    if (/\b[0-9a-f]{40}\b/i.test(proseOnly)) {
      issues.push({
        code: 'RAW_FULL_SHA_IN_BODY',
        message: `Body line ${i + 1} lists a raw 40-character commit SHA.`,
        fix: 'Omit raw full SHAs from the PR body; reviewers already see commits in the diff.'
      });
    }
  }

  for (const req of REQUIRED_PR_SECTIONS) {
    if (!presentHeadings.has(req)) {
      issues.push({
        code: 'MISSING_PR_SECTION',
        message: `Required PR briefing section (## ${req}) is missing.`,
        fix: `Add a ## ${req} heading with a 1-3 line summary.`
      });
    }
  }

  return {
    ok: issues.length === 0,
    lineCount: lines.length,
    issues
  };
}

function printHelp() {
  console.log([
    'Usage:',
    '  node ship-check.mjs stamp [--repo <dir>] [--base <rev>] [--file <path>] --evidence "<evidence>" [--json]',
    '  node ship-check.mjs verify [--repo <dir>] [--base <rev>] [--file <path>] [--require-clean] [--require-verified] [--json]',
    '  node ship-check.mjs brief --title "<title>" [--body-file <file> | --stdin] [--max-lines <N>] [--json]'
  ].join('\n'));
}

function main() {
  let parsed;
  try {
    parsed = parseCliFlags(process.argv.slice(2), {
      booleans: ['json', 'help', 'require-clean', 'require-verified', 'stdin'],
      allowedFlags: ['json', 'repo', 'base', 'file', 'evidence', 'require-clean', 'require-verified', 'title', 'body-file', 'stdin', 'max-lines']
    });
  } catch (err) {
    console.error(err.message);
    process.exit(2);
  }

  if (parsed.flags.help) {
    printHelp();
    process.exit(0);
  }

  const sub = parsed.positional[0];
  if (!sub || !['stamp', 'verify', 'brief'].includes(sub)) {
    printHelp();
    process.exit(2);
  }

  try {
    if (sub === 'stamp') {
      const res = stampVerification({
        repoDir: parsed.flags.repo,
        base: parsed.flags.base,
        stampFile: parsed.flags.file,
        evidence: parsed.flags.evidence
      });
      if (parsed.flags.json) {
        console.log(JSON.stringify(res, null, 2));
      } else {
        console.log(`Verification stamp recorded: patchId=${res.record.patchId.slice(0, 12)} (${res.stampFile})`);
      }
      process.exit(0);
    }

    if (sub === 'verify') {
      const res = verifyShipReadiness({
        repoDir: parsed.flags.repo,
        base: parsed.flags.base,
        stampFile: parsed.flags.file,
        requireClean: Boolean(parsed.flags['require-clean']),
        requireVerified: Boolean(parsed.flags['require-verified'])
      });
      if (parsed.flags.json) {
        console.log(JSON.stringify(res, null, 2));
      } else if (res.ok) {
        console.log(
          `OK: ship check passed (patchId=${res.currentPatchId.slice(0, 12)}, clean=${res.worktreeClean}, verified=${res.verificationFresh})`
        );
      } else {
        for (const issue of res.issues) {
          console.error(`[${issue.code}] ${issue.message}\n  Fix: ${issue.fix}`);
        }
      }
      process.exit(res.ok ? 0 : 1);
    }

    if (sub === 'brief') {
      let maxLines = 40;
      if (parsed.flags['max-lines'] !== undefined) {
        const rawMax = String(parsed.flags['max-lines']).trim();
        if (!/^\d+$/.test(rawMax)) {
          throw new Error(`Invalid --max-lines value (positive integer required): ${rawMax}`);
        }
        maxLines = Number.parseInt(rawMax, 10);
      }

      const body = parsed.flags.stdin
        ? fs.readFileSync(0, 'utf8')
        : parsed.flags['body-file']
          ? fs.readFileSync(path.resolve(parsed.flags['body-file']), 'utf8')
          : '';
      const res = validatePrBriefing({
        title: parsed.flags.title,
        body,
        maxLines
      });
      if (parsed.flags.json) {
        console.log(JSON.stringify(res, null, 2));
      } else if (res.ok) {
        console.log(`OK: PR brief validated (${res.lineCount} lines)`);
      } else {
        for (const issue of res.issues) {
          console.error(`[${issue.code}] ${issue.message}\n  Fix: ${issue.fix}`);
        }
      }
      process.exit(res.ok ? 0 : 1);
    }
  } catch (err) {
    console.error(err.message);
    process.exit(2);
  }
}

if (isCliMain(import.meta.url)) {
  main();
}
