import { spawnSync } from 'node:child_process';
import crypto from 'node:crypto';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';

export function isGitRepo(repoDir) {
  if (!repoDir || !fs.existsSync(repoDir)) return false;
  const res = spawnSync('git', ['-C', repoDir, 'rev-parse', '--is-inside-work-tree'], { encoding: 'utf8' });
  return res.status === 0 && res.stdout.trim() === 'true';
}

export function ensureGitRepo(repoDir) {
  const resolved = path.resolve(repoDir);
  if (!fs.existsSync(resolved) || !fs.statSync(resolved).isDirectory()) {
    throw new Error(`대상 경로가 존재하지 않거나 디렉터리가 아닙니다: ${repoDir}`);
  }
  if (!isGitRepo(resolved)) {
    throw new Error(`유효한 git 작업 트리가 아닙니다: ${resolved}`);
  }
  return resolved;
}

export function resolveCommitSha(repoDir, rev) {
  if (!rev || !String(rev).trim()) return null;
  const res = spawnSync('git', ['-C', repoDir, 'rev-parse', '--verify', `${String(rev).trim()}^{commit}`], {
    encoding: 'utf8'
  });
  return res.status === 0 ? res.stdout.trim() : null;
}

export function getHeadSha(repoDir) {
  return resolveCommitSha(repoDir, 'HEAD');
}

function splitNullDelimited(bufOrStr) {
  const text = Buffer.isBuffer(bufOrStr) ? bufOrStr.toString('utf8') : String(bufOrStr ?? '');
  if (!text) return [];
  return text.split('\0').filter((entry) => entry.length > 0);
}

function isKrigRelPath(relPath) {
  const norm = relPath.replace(/\\/g, '/');
  return norm === '.krig' || norm.startsWith('.krig/');
}

export function getWorktreeStatus(repoDir) {
  const hasHead = Boolean(getHeadSha(repoDir));
  const stagedArgs = hasHead
    ? ['-C', repoDir, 'diff', '--cached', '--name-only', '-z']
    : ['-C', repoDir, 'ls-files', '--cached', '-z'];
  const stagedRes = spawnSync('git', stagedArgs, { maxBuffer: 16 * 1024 * 1024 });
  const unstagedRes = spawnSync('git', ['-C', repoDir, 'diff', '--name-only', '-z'], { maxBuffer: 16 * 1024 * 1024 });
  const untrackedRes = spawnSync(
    'git',
    ['-C', repoDir, 'ls-files', '--others', '--exclude-standard', '-z'],
    { maxBuffer: 16 * 1024 * 1024 }
  );

  const allStaged = splitNullDelimited(stagedRes.stdout);
  const stagedKrigFiles = allStaged.filter((f) => isKrigRelPath(f));
  const stagedFiles = allStaged.filter((f) => !isKrigRelPath(f));
  const unstagedFiles = splitNullDelimited(unstagedRes.stdout).filter((f) => !isKrigRelPath(f));
  const untrackedFiles = splitNullDelimited(untrackedRes.stdout)
    .filter((f) => !isKrigRelPath(f))
    .sort();

  const dirtyNonKrigCount = stagedFiles.length + unstagedFiles.length + untrackedFiles.length;

  return {
    stagedFiles,
    stagedKrigFiles,
    unstagedFiles,
    untrackedFiles,
    dirtyNonKrigCount,
    isClean: dirtyNonKrigCount === 0 && stagedKrigFiles.length === 0
  };
}

function findBranchMergeBase(repoDir, headSha) {
  for (const candidateRef of ['refs/remotes/origin/HEAD', 'refs/heads/main', 'refs/heads/master']) {
    const refSha = resolveCommitSha(repoDir, candidateRef);
    if (!refSha || refSha === headSha) continue;
    const mb = spawnSync('git', ['-C', repoDir, 'merge-base', headSha, refSha], { encoding: 'utf8' });
    if (mb.status === 0) {
      const mbSha = mb.stdout.trim();
      if (mbSha && mbSha !== headSha) {
        return { baseSha: mbSha, source: `merge-base:${candidateRef}` };
      }
    }
  }
  return null;
}

export function readBaseSha(repoDir, explicitBase = null) {
  if (explicitBase) {
    const sha = resolveCommitSha(repoDir, explicitBase);
    if (!sha) {
      throw new Error(`지정한 기준 커밋(--base ${explicitBase})을 찾을 수 없습니다.`);
    }
    return { baseSha: sha, source: 'explicit' };
  }

  const baseFile = path.join(repoDir, '.krig', 'base-sha');
  if (fs.existsSync(baseFile)) {
    const raw = fs.readFileSync(baseFile, 'utf8').trim();
    const sha = resolveCommitSha(repoDir, raw);
    if (!sha) {
      throw new Error(`.krig/base-sha에 유효하지 않은 커밋 해시가 기록되어 있습니다: ${raw || '(빈 파일)'}`);
    }
    return { baseSha: sha, source: '.krig/base-sha' };
  }

  const head = getHeadSha(repoDir);
  if (!head) {
    throw new Error('저장소에 아직 커밋(HEAD)이 없습니다.');
  }

  const worktree = getWorktreeStatus(repoDir);
  if (worktree.dirtyNonKrigCount === 0) {
    const mb = findBranchMergeBase(repoDir, head);
    if (mb) return mb;
  }

  return { baseSha: head, source: 'HEAD' };
}

export function isAncestor(repoDir, ancestorSha, descendantSha = 'HEAD') {
  if (!ancestorSha || !descendantSha) return false;
  const res = spawnSync('git', ['-C', repoDir, 'merge-base', '--is-ancestor', ancestorSha, descendantSha], {
    encoding: 'utf8'
  });
  return res.status === 0;
}

function buildUntrackedFileDiffBuffer(repoDir, relFile) {
  if (fs.existsSync('/dev/null')) {
    const res = spawnSync(
      'git',
      ['-c', 'core.quotepath=false', 'diff', '--no-index', '--binary', '--', '/dev/null', relFile],
      {
        cwd: repoDir,
        maxBuffer: 32 * 1024 * 1024
      }
    );
    if (res.status !== 0 && res.status !== 1) {
      throw new Error(`미추적 파일 diff 생성 실패 (${relFile}): ${(res.stderr ?? Buffer.alloc(0)).toString('utf8').trim()}`);
    }
    return res.stdout ?? Buffer.alloc(0);
  }

  const emptyTmp = path.join(os.tmpdir(), `krig-empty-${process.pid}-${Math.random().toString(16).slice(2)}`);
  fs.writeFileSync(emptyTmp, '');
  try {
    const res = spawnSync(
      'git',
      ['-c', 'core.quotepath=false', 'diff', '--no-index', '--binary', '--', emptyTmp, relFile],
      {
        cwd: repoDir,
        maxBuffer: 32 * 1024 * 1024
      }
    );
    if (res.status !== 0 && res.status !== 1) {
      throw new Error(`미추적 파일 diff 생성 실패 (${relFile}): ${(res.stderr ?? Buffer.alloc(0)).toString('utf8').trim()}`);
    }
    const rawBuf = res.stdout ?? Buffer.alloc(0);
    const nlIdx = rawBuf.indexOf(0x0a);
    if (nlIdx === -1) return rawBuf;
    const normPosixTmp = emptyTmp.replace(/\\/g, '/').replace(/^\//, '');
    const headerText = rawBuf.subarray(0, Math.min(rawBuf.length, 2048)).toString('utf8');
    const fixedHeader = headerText
      .replace(`diff --git a/${normPosixTmp} b/`, `diff --git a/${relFile} b/`)
      .replace(`--- a/${normPosixTmp}`, '--- /dev/null');
    return Buffer.concat([
      Buffer.from(fixedHeader, 'utf8'),
      rawBuf.subarray(Math.min(rawBuf.length, 2048))
    ]);
  } finally {
    fs.rmSync(emptyTmp, { force: true });
  }
}

function extractBinaryAndModeSupplement(diffText) {
  const lines = diffText.split(/\r?\n/);
  let curFile = '';
  let inBinary = false;
  let hasBinaryPayload = false;
  const records = [];

  for (const line of lines) {
    if (line.startsWith('diff --git ')) {
      inBinary = false;
      const bMatch = / b\/(.+)$/.exec(line);
      curFile = bMatch ? bMatch[1] : line;
      continue;
    }
    if (
      line.startsWith('old mode ')
      || line.startsWith('new mode ')
      || line.startsWith('new file mode ')
      || line.startsWith('deleted file mode ')
    ) {
      records.push(`${curFile}|${line}`);
      continue;
    }
    if (line === 'GIT binary patch') {
      inBinary = true;
      hasBinaryPayload = true;
      records.push(`${curFile}|BINARY_START`);
      continue;
    }
    if (inBinary) {
      records.push(`${curFile}|${line}`);
    }
  }

  return { hasBinaryPayload, records };
}

export function computeCombinedPatch(repoDir, baseSha, options = {}) {
  const includeWorktree = options.includeWorktree !== false;
  const diffArgs = includeWorktree
    ? ['-C', repoDir, '-c', 'core.quotepath=false', 'diff', '--binary', baseSha, '--', '.', ':!.krig']
    : ['-C', repoDir, '-c', 'core.quotepath=false', 'diff', '--binary', `${baseSha}...HEAD`, '--', '.', ':!.krig'];

  const trackedDiff = spawnSync('git', diffArgs, { maxBuffer: 64 * 1024 * 1024 });
  if (trackedDiff.status !== 0) {
    throw new Error(`git diff 실행 실패: ${(trackedDiff.stderr ?? Buffer.alloc(0)).toString('utf8').trim()}`);
  }

  const chunks = [trackedDiff.stdout ?? Buffer.alloc(0)];

  if (includeWorktree) {
    const { untrackedFiles } = getWorktreeStatus(repoDir);
    for (const rel of untrackedFiles) {
      chunks.push(buildUntrackedFileDiffBuffer(repoDir, rel));
    }
  }

  const combined = Buffer.concat(chunks);
  if (combined.length === 0) {
    return { patchId: 'empty-diff', diffText: '', byteLength: 0 };
  }

  const patchRes = spawnSync('git', ['-C', repoDir, 'patch-id', '--stable'], {
    input: combined,
    encoding: 'utf8',
    maxBuffer: 64 * 1024 * 1024
  });
  if (patchRes.status !== 0) {
    throw new Error(`git patch-id 계산 실패: ${(patchRes.stderr ?? '').trim()}`);
  }

  const diffText = combined.toString('utf8');
  const firstToken = (patchRes.stdout ?? '').trim().split(/\s+/)[0] || '';
  const supplement = extractBinaryAndModeSupplement(diffText);

  let patchId = firstToken;
  if (supplement.hasBinaryPayload || (!firstToken && supplement.records.length > 0)) {
    const supHash = crypto
      .createHash('sha256')
      .update(supplement.records.join('\n'))
      .digest('hex')
      .slice(0, 16);
    patchId = `${firstToken || 'meta'}-${supHash}`;
  }

  return {
    patchId: patchId || 'empty-diff',
    diffText,
    byteLength: combined.length
  };
}

export function isValidStampObject(stamp) {
  return Boolean(
    stamp
    && typeof stamp === 'object'
    && !Array.isArray(stamp)
    && typeof stamp.patchId === 'string'
    && stamp.patchId.trim().length > 0
    && stamp.patchId !== 'empty-diff'
  );
}

export function evaluateVerificationStamp({
  repoDir,
  stamp,
  baseSha,
  currentPatch,
  explicitBase = null
}) {
  if (!isValidStampObject(stamp)) {
    return { valid: false, fresh: false, matchedBaseSha: null };
  }

  if (currentPatch.patchId === stamp.patchId) {
    return { valid: true, fresh: true, matchedBaseSha: baseSha };
  }

  if (explicitBase) {
    return { valid: true, fresh: false, matchedBaseSha: null };
  }

  const candidateBases = [];
  const resolvedStampBase = stamp.baseSha ? resolveCommitSha(repoDir, stamp.baseSha) : null;
  if (resolvedStampBase && resolvedStampBase !== baseSha) {
    candidateBases.push(resolvedStampBase);
  }

  const anchorBase = resolvedStampBase ?? baseSha;
  const headSha = getHeadSha(repoDir);
  if (headSha && anchorBase && isAncestor(repoDir, anchorBase, headSha)) {
    const revList = spawnSync(
      'git',
      ['-C', repoDir, 'rev-list', '--first-parent', '--max-count=20', `${anchorBase}..${headSha}`],
      { encoding: 'utf8' }
    );
    if (revList.status === 0 && revList.stdout.trim()) {
      const commits = revList.stdout.trim().split(/\r?\n/).filter(Boolean);
      for (const c of commits) {
        const parent = resolveCommitSha(repoDir, `${c}^`);
        if (parent && parent !== baseSha && !candidateBases.includes(parent) && isAncestor(repoDir, anchorBase, parent)) {
          candidateBases.push(parent);
        }
      }
    }
  }

  for (const candBase of candidateBases) {
    const altPatch = computeCombinedPatch(repoDir, candBase);
    if (altPatch.patchId === stamp.patchId) {
      return { valid: true, fresh: true, matchedBaseSha: candBase };
    }
  }

  return { valid: true, fresh: false, matchedBaseSha: null };
}

const SECRET_PATTERNS = Object.freeze([
  { name: 'openai-or-api-sk-token', re: /\bsk-(?:proj-)?[A-Za-z0-9_-]{20,}\b/ },
  { name: 'github-pat', re: /\b(?:ghp_[A-Za-z0-9]{30,}|github_pat_[A-Za-z0-9_]{20,})\b/ },
  { name: 'aws-access-key', re: /\bAKIA[0-9A-Z]{16}\b/ },
  { name: 'slack-token', re: /\bxox[baprs]-[A-Za-z0-9-]{20,}\b/ },
  { name: 'private-key-pem', re: /-----BEGIN (?:RSA |OPENSSH |EC |DSA )?PRIVATE KEY-----/ }
]);

export function scanDiffHazards(diffText, repoDir = null) {
  const hazards = [];
  if (!diffText) return hazards;

  const lines = diffText.split(/\r?\n/);
  let currentFile = '(unknown)';
  let openConflictInFile = false;
  const binaryFiles = new Set();

  for (const line of lines) {
    if (line.startsWith('diff --git ')) {
      openConflictInFile = false;
      const bMatch = / b\/(.+)$/.exec(line);
      if (bMatch) currentFile = bMatch[1].trim();
      continue;
    }
    if (line.startsWith('+++ b/')) {
      currentFile = line.slice('+++ b/'.length).trim();
      openConflictInFile = false;
      continue;
    }
    if (line === 'GIT binary patch' && currentFile !== '(unknown)') {
      binaryFiles.add(currentFile);
      continue;
    }
    if (!line.startsWith('+') || line.startsWith('+++')) continue;
    const added = line.slice(1);

    if (/^<{7}\s/.test(added)) {
      openConflictInFile = true;
      hazards.push({
        kind: 'MERGE_CONFLICT_MARKER',
        file: currentFile,
        detail: `해소되지 않은 병합 충돌 마커가 감지되었습니다: ${added.slice(0, 40)}`
      });
    } else if (/^>{7}\s/.test(added)) {
      openConflictInFile = false;
      hazards.push({
        kind: 'MERGE_CONFLICT_MARKER',
        file: currentFile,
        detail: `해소되지 않은 병합 충돌 마커가 감지되었습니다: ${added.slice(0, 40)}`
      });
    } else if (openConflictInFile && /^={7}$/.test(added)) {
      hazards.push({
        kind: 'MERGE_CONFLICT_MARKER',
        file: currentFile,
        detail: '해소되지 않은 병합 충돌 구분선(=======)이 감지되었습니다.'
      });
    }

    for (const rule of SECRET_PATTERNS) {
      if (rule.re.test(added)) {
        hazards.push({
          kind: 'SECRET_PATTERN_LEAK',
          file: currentFile,
          detail: `변경분에 시크릿 패턴(${rule.name}) 의심 문자열이 포함되어 있습니다.`
        });
      }
    }
  }

  if (repoDir && binaryFiles.size > 0) {
    for (const relFile of binaryFiles) {
      const absFile = path.resolve(repoDir, relFile);
      if (!fs.existsSync(absFile) || !fs.statSync(absFile).isFile()) continue;
      const rawText = fs.readFileSync(absFile).toString('latin1');
      for (const rule of SECRET_PATTERNS) {
        if (rule.re.test(rawText)) {
          hazards.push({
            kind: 'SECRET_PATTERN_LEAK',
            file: relFile,
            detail: `바이너리/혼합 파일에 시크릿 패턴(${rule.name}) 의심 문자열이 포함되어 있습니다.`
          });
        }
      }
    }
  }

  return hazards;
}

export function listCommitsBetween(repoDir, baseSha, headSha = 'HEAD') {
  if (!baseSha || baseSha === headSha) return [];
  const res = spawnSync(
    'git',
    ['-C', repoDir, 'log', '--format=%H%x09%s', `${baseSha}..${headSha}`],
    { encoding: 'utf8' }
  );
  if (res.status !== 0 || !res.stdout.trim()) return [];
  return res.stdout
    .trim()
    .split(/\r?\n/)
    .filter(Boolean)
    .map((line) => {
      const [sha = '', subject = ''] = line.split('\t');
      return { sha: sha.slice(0, 12), fullSha: sha, subject };
    });
}
