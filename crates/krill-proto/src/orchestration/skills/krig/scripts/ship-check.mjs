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
    throw new Error('검증 스탬프에는 실제 실행 결과 요약(--evidence)이 필수입니다.');
  }

  const { baseSha, source: baseSource } = readBaseSha(repoDir, options.base);
  const headSha = getHeadSha(repoDir);
  const worktree = getWorktreeStatus(repoDir);

  if (worktree.stagedKrigFiles.length > 0) {
    throw new Error(
      `.krig 임시 파일이 git 인덱스에 스테이징되어 있습니다(${worktree.stagedKrigFiles.join(', ')}). git reset HEAD .krig 로 먼저 해제하세요.`
    );
  }

  const patch = computeCombinedPatch(repoDir, baseSha);
  if (patch.patchId === 'empty-diff') {
    throw new Error(
      `기준 커밋(${baseSha.slice(0, 8)}) 대비 변경분(diff)이 비어 있어 검증 스탬프를 발급할 수 없습니다. --base <기준커밋> 을 명시하세요.`
    );
  }

  const hazards = scanDiffHazards(patch.diffText, repoDir);
  if (hazards.length > 0) {
    throw new Error(`변경분에 출하 차단 위험이 있습니다: ${hazards.map((h) => `${h.kind}(${h.file})`).join(', ')}`);
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
      message: `.krig 런타임 파일이 커밋 대기열에 올라와 있습니다: ${worktree.stagedKrigFiles.join(', ')}`,
      fix: 'git reset HEAD .krig 명령으로 스테이징을 해제하세요.'
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
        message: `커밋되지 않은 변경 사항이 남아 있습니다(${dirtySummary.slice(0, 6).join(', ')})`,
        fix: '변경 사항을 논리적 커밋으로 묶거나 불필요한 임시 파일을 정리하세요.'
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
        message: `검증 스탬프 형식 또는 JSON이 손상되었습니다: ${stampFile}`,
        fix: '검증을 다시 수행하고 ship-check.mjs stamp 로 갱신하세요.'
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
          message: `검증 스탬프(${stamp.patchId.slice(0, 12)}) 이후 코드 변경이 발생해 현재 패치(${currentPatch.patchId.slice(0, 12)})와 일치하지 않습니다.`,
          fix: '변경된 코드에 대해 테스트 및 verify 단계를 다시 실행하고 스탬프를 재발급하세요.'
        });
      }
    }
  } else if (requireVerified) {
    issues.push({
      code: 'MISSING_VERIFICATION_STAMP',
      message: '기록된 검증 스탬프(.krig/verified.json)가 없습니다.',
      fix: 'verify 플레이북 검증을 마친 뒤 ship-check.mjs stamp --evidence "..." 를 실행하세요.'
    });
  }

  const hazards = scanDiffHazards(currentPatch.diffText, repoDir);
  for (const h of hazards) {
    issues.push({
      code: h.kind,
      message: `${h.file}: ${h.detail}`,
      fix: h.kind === 'MERGE_CONFLICT_MARKER'
        ? '충돌 마커를 완전히 해소하고 테스트를 다시 실행하세요.'
        : '하드코딩된 시크릿을 제거하고 환경변수나 키체인으로 분리하세요.'
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
    throw new Error(`유효하지 않은 maxLines 값입니다(5 이상의 정수 필요): ${maxLines}`);
  }

  const issues = [];
  const rawTitleStr = String(title ?? '');
  const hasTitleNewline = /[\r\n]/.test(rawTitleStr.trim());
  const cleanTitle = rawTitleStr.trim();

  if (!cleanTitle) {
    issues.push({
      code: 'EMPTY_PR_TITLE',
      message: 'PR 제목이 비어 있습니다.',
      fix: 'type(scope): subject 형식의 제목을 지정하세요.'
    });
  } else {
    if (hasTitleNewline || !CONVENTIONAL_TITLE_RE.test(cleanTitle)) {
      issues.push({
        code: 'INVALID_CONVENTIONAL_TITLE',
        message: `PR 제목은 개행 없는 단일 줄 Conventional Commits 형식이어야 합니다: ${cleanTitle.replace(/[\r\n]+/g, '\\n')}`,
        fix: 'feat(scope): imperative summary 처럼 한 줄 타입과 요약을 적으세요.'
      });
    }
    if (cleanTitle.endsWith('.')) {
      issues.push({
        code: 'TITLE_TRAILING_PERIOD',
        message: 'PR 제목 끝에 마침표가 있습니다.',
        fix: '제목 끝의 마침표를 제거하세요.'
      });
    }
    if (cleanTitle.length > 72) {
      issues.push({
        code: 'TITLE_TOO_LONG',
        message: `PR 제목 길이(${cleanTitle.length}자)가 72자를 초과했습니다.`,
        fix: '핵심 변경 심볼 중심 72자 이내로 압축하세요.'
      });
    }
  }

  const rawBody = String(body ?? '').replace(/\r\n/g, '\n').trim();
  const lines = rawBody ? rawBody.split('\n') : [];

  if (lines.length === 0) {
    issues.push({
      code: 'EMPTY_PR_BODY',
      message: 'PR 본문 브리핑이 비어 있습니다.',
      fix: '## Why, ## What changed, ## Scope, ## Blast Radius, ## Verification 섹션을 작성하세요.'
    });
    return { ok: false, lineCount: 0, issues };
  }

  if (lines.length > parsedMax) {
    issues.push({
      code: 'PR_BODY_TOO_LONG',
      message: `PR 본문 줄 수(${lines.length}줄)가 상한(${parsedMax}줄)을 초과했습니다.`,
      fix: `스쿼시 커밋 본문이 되도록 ${parsedMax}줄 이내의 1분 브리핑으로 줄이세요.`
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
        message: `본문 ${i + 1}행 산문에 금지 기호(가운뎃점, 대시, 화살표)가 포함되어 있습니다.`,
        fix: '쉼표, 마침표, 콜론, 슬래시로 교체하세요.'
      });
    }
    if (/\b[0-9a-f]{40}\b/i.test(proseOnly)) {
      issues.push({
        code: 'RAW_FULL_SHA_IN_BODY',
        message: `본문 ${i + 1}행에 40자리 전체 커밋 SHA 나열이 있습니다.`,
        fix: '리뷰어는 이미 diff를 보고 있으므로 본문에서 긴 SHA 나열을 빼세요.'
      });
    }
  }

  for (const req of REQUIRED_PR_SECTIONS) {
    if (!presentHeadings.has(req)) {
      issues.push({
        code: 'MISSING_PR_SECTION',
        message: `필수 PR 브리핑 섹션(## ${req})이 누락되었습니다.`,
        fix: `본문에 ## ${req} 제목과 1~3줄 요약을 추가하세요.`
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
    '사용법:',
    '  node ship-check.mjs stamp [--repo <dir>] [--base <rev>] [--file <경로>] --evidence "<검증근거>" [--json]',
    '  node ship-check.mjs verify [--repo <dir>] [--base <rev>] [--file <경로>] [--require-clean] [--require-verified] [--json]',
    '  node ship-check.mjs brief --title "<제목>" [--body-file <파일> | --stdin] [--max-lines <N>] [--json]'
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
        console.log(`검증 스탬프 기록 완료: patchId=${res.record.patchId.slice(0, 12)} (${res.stampFile})`);
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
          `OK: 출하 점검 통과 (patchId=${res.currentPatchId.slice(0, 12)}, clean=${res.worktreeClean}, verified=${res.verificationFresh})`
        );
      } else {
        for (const issue of res.issues) {
          console.error(`[${issue.code}] ${issue.message}\n  해결: ${issue.fix}`);
        }
      }
      process.exit(res.ok ? 0 : 1);
    }

    if (sub === 'brief') {
      let maxLines = 40;
      if (parsed.flags['max-lines'] !== undefined) {
        const rawMax = String(parsed.flags['max-lines']).trim();
        if (!/^\d+$/.test(rawMax)) {
          throw new Error(`유효하지 않은 --max-lines 값입니다(양의 정수 필요): ${rawMax}`);
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
        console.log(`OK: PR 브리핑 규격 통과 (${res.lineCount}줄)`);
      } else {
        for (const issue of res.issues) {
          console.error(`[${issue.code}] ${issue.message}\n  해결: ${issue.fix}`);
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
