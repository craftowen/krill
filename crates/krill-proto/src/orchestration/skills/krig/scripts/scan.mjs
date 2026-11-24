#!/usr/bin/env node
import fs from 'node:fs';
import path from 'node:path';
import { isCliMain, parseCliFlags } from './lib/cli.mjs';
import { collectProductionFiles, isTestPath, JS_EXTS, lexSource } from './lib/source-lex.mjs';

export { isTestPath };

function hasRangeAllow(allowLines, startLine, endLine) {
  const from = Math.max(0, startLine - 1);
  for (let idx = from; idx <= endLine; idx += 1) {
    if (allowLines.has(idx)) return true;
  }
  return false;
}

function findEnclosingTryAllow(cleanLines, allowLines, catchLine) {
  for (let k = catchLine; k >= Math.max(0, catchLine - 15); k -= 1) {
    if (/\btry\b/.test(cleanLines[k])) {
      if (allowLines.has(k) || (k > 0 && allowLines.has(k - 1))) {
        return true;
      }
      break;
    }
  }
  return false;
}

function isPythonAbstractMethodRaise(rawLines, cleanLines, raiseIdx) {
  const raiseRaw = rawLines[raiseIdx] ?? '';
  if (/\b(todo|fixme|pending|wip)\b/i.test(raiseRaw)) return false;

  const raiseIndent = (raiseRaw.match(/^\s*/)?.[0] ?? '').length;
  if (raiseIndent === 0) return false;

  let defIdx = -1;
  let defIndent = -1;
  for (let k = raiseIdx - 1; k >= 0; k -= 1) {
    const cleanTrim = cleanLines[k].trim();
    if (!cleanTrim || cleanTrim === '""""""' || cleanTrim === "''''''" || cleanTrim === '""' || cleanTrim === "''") {
      continue;
    }
    const ind = (rawLines[k].match(/^\s*/)?.[0] ?? '').length;
    if (ind < raiseIndent && /^(?:async\s+)?def\s+\w+\s*\(/.test(cleanTrim)) {
      defIdx = k;
      defIndent = ind;
      break;
    }
    return false;
  }

  if (defIdx === -1) return false;

  for (let k = raiseIdx + 1; k < cleanLines.length; k += 1) {
    const cleanTrim = cleanLines[k].trim();
    if (!cleanTrim) continue;
    const ind = (rawLines[k].match(/^\s*/)?.[0] ?? '').length;
    if (ind <= defIndent) break;
    return false;
  }

  if (defIndent > 0) return true;
  for (let k = defIdx - 1; k >= Math.max(0, defIdx - 5); k -= 1) {
    const cleanTrim = cleanLines[k].trim();
    if (!cleanTrim) continue;
    if (/^@(?:abc\.)?abstractmethod\b/.test(cleanTrim)) return true;
    if (!cleanTrim.startsWith('@')) break;
  }
  return false;
}

function extractJsCatchBlock(cleanLines, startIdx) {
  let braceDepth = 0;
  let started = false;
  let collected = '';
  const maxIdx = Math.min(cleanLines.length - 1, startIdx + 12);

  const firstLine = cleanLines[startIdx];
  const catchPos = firstLine.search(/\bcatch\b/);
  if (catchPos === -1) return null;

  for (let idx = startIdx; idx <= maxIdx; idx += 1) {
    const segment = idx === startIdx ? cleanLines[idx].slice(catchPos) : cleanLines[idx];
    for (let c = 0; c < segment.length; c += 1) {
      const ch = segment[c];
      if (ch === '{') {
        if (started) collected += ch;
        braceDepth += 1;
        started = true;
      } else if (ch === '}') {
        braceDepth -= 1;
        if (started && braceDepth === 0) {
          return { body: collected.trim(), endIdx: idx };
        }
        if (started) collected += ch;
      } else if (started) {
        collected += ch;
      }
    }
    if (started) collected += ' ';
  }
  return null;
}

export function scanFileContent(relFile, content) {
  const findings = [];
  const rawLines = content.split(/\r?\n/);
  const ext = path.extname(relFile).toLowerCase();
  const { allowLines, cleanLines, stringKeptLines } = lexSource(content, ext);

  for (let i = 0; i < cleanLines.length; i += 1) {
    const rawTrimmed = rawLines[i].trim();
    const clean = cleanLines[i];
    const cleanTrimmed = clean.trim();
    const withStrings = stringKeptLines[i];

    if (!cleanTrimmed) continue;
    if (hasRangeAllow(allowLines, i, i)) continue;

    if (
      /\b(jest\.fn|vi\.fn|sinon\.(stub|spy|mock)|MagicMock|AsyncMock|monkeypatch\.setattr)\b/.test(clean)
      || (ext === '.py' && /^\s*(?:from\s+unittest\s+import\s+mock|import\s+unittest\.mock|from\s+unittest\.mock\s+import)\b/.test(clean))
    ) {
      findings.push({
        file: relFile,
        line: i + 1,
        category: 'test-double-in-prod',
        confidence: 'high',
        snippet: rawTrimmed.slice(0, 140),
        remediation: 'Remove the test double from production code and move it behind a constructor/adapter injection seam.'
      });
    }

    const hasNotImpl = /\bNotImplementedError\b/.test(clean);
    const hasRustTodo = /\b(todo|unimplemented)!\s*\(/.test(clean);
    const hasJsTodoThrow = /\bthrow\s+new\s+Error\(\s*['"`](?:TODO|FIXME|not\s+implemented)\b/i.test(withStrings);

    if (hasRustTodo || hasJsTodoThrow || (hasNotImpl && !(ext === '.py' && isPythonAbstractMethodRaise(rawLines, cleanLines, i)))) {
      findings.push({
        file: relFile,
        line: i + 1,
        category: 'placeholder',
        confidence: 'high',
        snippet: rawTrimmed.slice(0, 140),
        remediation: 'Replace the unimplemented placeholder with real behavior or remove the unsupported entrypoint.'
      });
    }

    let nextNonEmptyIdx = i + 1;
    while (nextNonEmptyIdx < cleanLines.length && !cleanLines[nextNonEmptyIdx].trim()) {
      nextNonEmptyIdx += 1;
    }
    const twoLineWindow = `${cleanTrimmed} ${(cleanLines[nextNonEmptyIdx] ?? '').trim()}`;

    if (
      (JS_EXTS.has(ext) && /\bif\s*\(\s*!\s*(?:secret|token|apiKey|signature|webhookSecret)\s*\)\s*\{?\s*return\s+true\b/i.test(twoLineWindow))
      || (ext === '.py' && /^if\s+not\s+(?:secret|token|api_key|signature|webhook_secret)\s*:\s*return\s+True\b/.test(twoLineWindow))
    ) {
      if (!hasRangeAllow(allowLines, i, Math.min(cleanLines.length - 1, nextNonEmptyIdx))) {
        findings.push({
          file: relFile,
          line: i + 1,
          category: 'permissive-guard',
          confidence: 'high',
          snippet: rawTrimmed.slice(0, 140),
          remediation: 'Reject with an explicit error instead of returning true when a secret or signature is missing.'
        });
      }
    }

    if (JS_EXTS.has(ext) && /\bcatch\b/.test(clean)) {
      const catchBlock = extractJsCatchBlock(cleanLines, i);
      if (catchBlock && !hasRangeAllow(allowLines, i, catchBlock.endIdx) && !findEnclosingTryAllow(cleanLines, allowLines, i)) {
        if (catchBlock.body === '') {
          findings.push({
            file: relFile,
            line: i + 1,
            category: 'swallowed-error',
            confidence: 'high',
            snippet: rawTrimmed.slice(0, 140),
            remediation: 'Do not swallow exceptions in an empty catch block; propagate them or document an intentional fallback with demock: allow <reason>.'
          });
        } else if (/^return\s+(?:true|\{\s*\}|\[\s*\])\s*;?$/.test(catchBlock.body)) {
          findings.push({
            file: relFile,
            line: i + 1,
            category: 'swallowed-error',
            confidence: 'high',
            snippet: rawTrimmed.slice(0, 140),
            remediation: 'Do not disguise an exception as a normal success value (true, empty object/array); surface the failure.'
          });
        }
      }
    }

    if (ext === '.py' && /^except(?:\s+[^:]+)?\s*:/.test(cleanTrimmed)) {
      const colonIdx = cleanTrimmed.indexOf(':');
      const headerPart = cleanTrimmed.slice(0, colonIdx).trim();
      const inlineBody = cleanTrimmed.slice(colonIdx + 1).trim();
      const isBroad = headerPart === 'except' || /^except\s+(?:Exception|BaseException)\b/.test(headerPart);

      let bodyStmt = inlineBody;
      let bodyLineIdx = i;

      if (!bodyStmt) {
        const exceptIndent = (rawLines[i].match(/^\s*/)?.[0] ?? '').length;
        const bodyLines = [];
        for (let k = i + 1; k < cleanLines.length; k += 1) {
          const ct = cleanLines[k].trim();
          if (!ct) continue;
          const ind = (rawLines[k].match(/^\s*/)?.[0] ?? '').length;
          if (ind <= exceptIndent) break;
          bodyLines.push({ idx: k, text: ct });
        }
        if (bodyLines.length === 1) {
          bodyStmt = bodyLines[0].text;
          bodyLineIdx = bodyLines[0].idx;
        }
      }

      if (
        bodyStmt
        && !hasRangeAllow(allowLines, i, bodyLineIdx)
        && !findEnclosingTryAllow(cleanLines, allowLines, i)
      ) {
        if (bodyStmt === 'pass' && isBroad) {
          findings.push({
            file: relFile,
            line: i + 1,
            category: 'swallowed-error',
            confidence: headerPart === 'except' ? 'high' : 'medium',
            snippet: `${headerPart}: ${bodyStmt}`.slice(0, 140),
            remediation: 'Do not swallow broad exceptions (except/Exception) with pass; catch specific exceptions or log/propagate the error.'
          });
        } else if (/^return\s+(?:True|\{\s*\}|\[\s*\])(?:\s*;)?$/.test(bodyStmt)) {
          findings.push({
            file: relFile,
            line: i + 1,
            category: 'swallowed-error',
            confidence: 'high',
            snippet: `${headerPart}: ${bodyStmt}`.slice(0, 140),
            remediation: 'Do not return True or an empty container from an except block to fabricate success.'
          });
        }
      }
    }
  }

  return findings;
}

export function scanTargets(targets) {
  const allFindings = [];
  const seenFiles = new Set();

  for (const target of targets) {
    const files = collectProductionFiles(target);
    for (const { root, abs } of files) {
      if (seenFiles.has(abs)) continue;
      seenFiles.add(abs);
      const rel = path.relative(root, abs) || path.basename(abs);
      const content = fs.readFileSync(abs, 'utf8');
      allFindings.push(...scanFileContent(rel, content));
    }
  }

  return { scannedFiles: seenFiles.size, findings: allFindings };
}

function printHelp() {
  console.log('Usage: node scan.mjs [--json] [--repo <path>] [<path...>] (exit codes: 0=clean, 3=findings, 2=error)');
}

function main() {
  let parsed;
  try {
    parsed = parseCliFlags(process.argv.slice(2), {
      booleans: ['json', 'help'],
      allowedFlags: ['json', 'repo']
    });
  } catch (err) {
    console.error(err.message);
    process.exit(2);
  }

  if (parsed.flags.help) {
    printHelp();
    process.exit(0);
  }

  const effectiveTargets = parsed.positional.length > 0
    ? parsed.positional
    : [parsed.flags.repo ?? process.cwd()];

  try {
    const result = scanTargets(effectiveTargets);
    if (parsed.flags.json) {
      console.log(JSON.stringify(result, null, 2));
    } else if (result.findings.length === 0) {
      console.log(`OK: scanned ${result.scannedFiles} production files with no fake-success or test-double findings.`);
    } else {
      for (const f of result.findings) {
        console.log(`${f.file}:${f.line} [${f.category}/${f.confidence}] ${f.snippet}\n  Fix: ${f.remediation}`);
      }
      console.log(`Found ${result.findings.length} candidate(s) across ${result.scannedFiles} production files.`);
    }
    process.exit(result.findings.length === 0 ? 0 : 3);
  } catch (err) {
    console.error(err.message);
    process.exit(2);
  }
}

if (isCliMain(import.meta.url)) {
  main();
}
