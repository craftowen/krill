import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';

export const JS_EXTS = new Set(['.js', '.mjs', '.cjs', '.ts', '.tsx', '.jsx']);
export const CODE_EXTS = new Set([...JS_EXTS, '.py', '.rs', '.go']);
export const BUILD_OR_VCS_DIRS = new Set([
  '.git', '.krig', 'node_modules', 'target', 'dist', 'build', 'coverage', '.venv', 'venv', '__pycache__'
]);
export const TEST_DIRS = new Set(['test', 'tests', '__tests__', '__mocks__', 'fixtures', 'testdata']);

const PROJECT_ROOT_MARKERS = ['package.json', 'pyproject.toml', 'Cargo.toml', 'go.mod', '.git'];
const REGEX_PRECEDING_KEYWORDS = new Set([
  'return', 'throw', 'case', 'typeof', 'void', 'delete', 'in', 'of', 'instanceof', 'new', 'await', 'yield', 'else', 'do'
]);
const CONDITIONAL_PAREN_KEYWORDS = new Set(['if', 'while', 'for', 'with']);
const RUST_CHAR_LITERAL_RE = /^'(?:\\(?:x[0-9a-fA-F]{2}|u\{[0-9a-fA-F_]+\}|.)|[^'\\\r\n])'/;

export function isProjectBoundary(dir) {
  const resolved = path.resolve(dir);
  if (
    resolved === path.parse(resolved).root
    || resolved === path.resolve(os.tmpdir())
    || resolved === path.resolve(os.homedir())
  ) {
    return true;
  }
  return PROJECT_ROOT_MARKERS.some((marker) => fs.existsSync(path.join(resolved, marker)));
}

export function isTestPath(absFileOrDir, scanRoot = null) {
  const abs = path.resolve(absFileOrDir);
  const base = path.basename(abs);
  if (
    /\.(test|spec)\.[a-z0-9]+$/i.test(base)
    || /^test_.*\.py$/i.test(base)
    || /_test\.(py|go|rs)$/i.test(base)
    || base === 'conftest.py'
  ) {
    return true;
  }

  let cur = abs;
  if (fs.existsSync(abs)) {
    try {
      if (fs.statSync(abs).isFile()) {
        cur = path.dirname(abs);
      }
    } catch {
      cur = path.dirname(abs);
    }
  } else if (path.extname(base)) {
    cur = path.dirname(abs);
  }

  const stopAt = scanRoot ? path.resolve(scanRoot) : null;
  while (true) {
    if (isProjectBoundary(cur)) break;
    if (stopAt && cur === stopAt) {
      const segAtRoot = path.basename(cur);
      if (!scanRoot || isProjectBoundary(cur)) break;
      return TEST_DIRS.has(segAtRoot) || BUILD_OR_VCS_DIRS.has(segAtRoot);
    }
    const seg = path.basename(cur);
    if (TEST_DIRS.has(seg) || BUILD_OR_VCS_DIRS.has(seg)) return true;
    const parent = path.dirname(cur);
    if (parent === cur) break;
    cur = parent;
  }
  return false;
}

export function collectCodeFiles(targetPath, options = {}) {
  const includeTests = Boolean(options.includeTests);
  const exts = options.exts ?? CODE_EXTS;
  const abs = path.resolve(targetPath);
  if (!fs.existsSync(abs)) {
    throw new Error(`스캔 대상 경로가 존재하지 않습니다: ${targetPath}`);
  }

  const stat = fs.statSync(abs);
  if (stat.isFile()) {
    const isTest = isTestPath(abs);
    if (!exts.has(path.extname(abs).toLowerCase()) || (!includeTests && isTest)) return [];
    return [{ root: path.dirname(abs), abs, isTest }];
  }

  if (!includeTests && !isProjectBoundary(abs) && isTestPath(abs)) return [];

  const out = [];
  function walk(dir) {
    for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
      if (BUILD_OR_VCS_DIRS.has(entry.name)) continue;
      if (!includeTests && TEST_DIRS.has(entry.name)) continue;
      const full = path.join(dir, entry.name);
      if (entry.isDirectory()) {
        walk(full);
      } else if (entry.isFile() && exts.has(path.extname(entry.name).toLowerCase())) {
        const isTest = isTestPath(full, abs);
        if (!includeTests && isTest) continue;
        out.push({ root: abs, abs: full, isTest });
      }
    }
  }
  walk(abs);
  return out.sort((a, b) => a.abs.localeCompare(b.abs));
}

export function collectProductionFiles(targetPath) {
  return collectCodeFiles(targetPath, { includeTests: false });
}

function canStartJsRegex(prevTokenChar, prevTwoChars, prevWord, afterConditionParen) {
  if (!prevTokenChar) return true;
  if (afterConditionParen) return true;
  if (prevTwoChars === '++' || prevTwoChars === '--') return false;
  if (prevTokenChar === '<') return false;
  if ('(,=:[!&|?{};~^+-*%<>'.includes(prevTokenChar)) return true;
  return REGEX_PRECEDING_KEYWORDS.has(prevWord);
}

function stripCommentSyntax(rawPart) {
  return rawPart
    .replace(/^\s*(?:\/\/+|#+|\/\*+|\*+\/?)/, '')
    .replace(/\*+\/\s*$/, '')
    .trim();
}

export function lexSource(content, ext) {
  const normalizedExt = String(ext ?? '').toLowerCase();
  const isPy = normalizedExt === '.py';
  const isJs = JS_EXTS.has(normalizedExt);
  const isRs = normalizedExt === '.rs';
  const isGo = normalizedExt === '.go';

  const allowLines = new Set();
  const comments = [];
  const stringValues = [];

  const noCommentsOrStrings = [];
  const noCommentsOrRegex = [];
  const moduleScanTokens = [];

  let lineNo = 0;
  let i = 0;
  let prevTokenChar = '';
  let prevTwoChars = '';
  let prevWord = '';
  let afterConditionParen = false;
  const parenKeywordStack = [];
  const templateExprStack = [];

  function updateTokenChar(ch) {
    if (!/\s/.test(ch)) {
      prevTwoChars = `${prevTokenChar}${ch}`;
      prevTokenChar = ch;
    }
  }

  function pushAll(ch) {
    noCommentsOrStrings.push(ch);
    noCommentsOrRegex.push(ch);
    moduleScanTokens.push(ch);
    if (ch === '\n') {
      lineNo += 1;
    } else {
      updateTokenChar(ch);
    }
  }

  function consumeTemplateText() {
    while (i < content.length) {
      const tc = content[i];
      if (tc === '\r') {
        i += 1;
        continue;
      }
      if (tc === '\\' && i + 1 < content.length) {
        if (content[i + 1] === '\r' && content[i + 2] === '\n') {
          pushAll('\n');
          i += 3;
          continue;
        }
        if (content[i + 1] === '\n') {
          pushAll('\n');
        } else {
          noCommentsOrRegex.push(' ', ' ');
        }
        i += 2;
        continue;
      }
      if (tc === '`') {
        noCommentsOrStrings.push('`');
        noCommentsOrRegex.push('`');
        moduleScanTokens.push('`');
        i += 1;
        updateTokenChar('`');
        prevWord = '';
        afterConditionParen = false;
        return;
      }
      if (tc === '$' && content[i + 1] === '{') {
        noCommentsOrStrings.push('$', '{');
        noCommentsOrRegex.push('$', '{');
        moduleScanTokens.push('$', '{');
        i += 2;
        templateExprStack.push(1);
        updateTokenChar('{');
        prevWord = '';
        afterConditionParen = false;
        return;
      }
      if (tc === '\n') {
        pushAll('\n');
      } else {
        noCommentsOrRegex.push(tc);
      }
      i += 1;
    }
  }

  function recordCommentText(commentStr, startLine) {
    const parts = commentStr.split('\n');
    for (let offset = 0; offset < parts.length; offset += 1) {
      const rawLineText = parts[offset];
      const body = stripCommentSyntax(rawLineText);
      if (body) {
        comments.push({
          line: startLine + offset + 1,
          raw: rawLineText.trim(),
          body
        });
      }
      if (/demock:\s*allow/i.test(rawLineText)) {
        allowLines.add(startLine + offset);
      }
    }
  }

  while (i < content.length) {
    const ch = content[i];
    const next = content[i + 1] ?? '';
    const next2 = content[i + 2] ?? '';

    if (ch === '\r') {
      i += 1;
      continue;
    }

    if (ch === '\n') {
      pushAll('\n');
      i += 1;
      continue;
    }

    if (isPy && ch === '#') {
      const start = i;
      const startLine = lineNo;
      while (i < content.length && content[i] !== '\n' && content[i] !== '\r') i += 1;
      recordCommentText(content.slice(start, i), startLine);
      continue;
    }

    if (!isPy && ch === '/' && next === '/') {
      const start = i;
      const startLine = lineNo;
      while (i < content.length && content[i] !== '\n' && content[i] !== '\r') i += 1;
      recordCommentText(content.slice(start, i), startLine);
      continue;
    }

    if (!isPy && ch === '/' && next === '*') {
      const start = i;
      const startLine = lineNo;
      i += 2;
      while (i < content.length && !(content[i] === '*' && content[i + 1] === '/')) {
        if (content[i] === '\n') {
          pushAll('\n');
        } else if (content[i] !== '\r') {
          noCommentsOrStrings.push(' ');
          noCommentsOrRegex.push(' ');
          moduleScanTokens.push(' ');
        }
        i += 1;
      }
      const end = Math.min(content.length, i + 2);
      recordCommentText(content.slice(start, end).replace(/\r/g, ''), startLine);
      i = end;
      continue;
    }

    if (isPy && ((ch === '"' && next === '"' && next2 === '"') || (ch === "'" && next === "'" && next2 === "'"))) {
      const quote = ch;
      noCommentsOrStrings.push(quote, quote);
      noCommentsOrRegex.push(quote, quote);
      moduleScanTokens.push(quote, quote);
      i += 3;
      while (i < content.length && !(content[i] === quote && content[i + 1] === quote && content[i + 2] === quote)) {
        if (content[i] === '\\' && i + 1 < content.length && content[i + 1] !== '\n') {
          i += 2;
          continue;
        }
        if (content[i] === '\n') {
          pushAll('\n');
        }
        i += 1;
      }
      i = Math.min(content.length, i + 3);
      updateTokenChar(quote);
      prevWord = '';
      afterConditionParen = false;
      continue;
    }

    if (isGo && ch === '`') {
      noCommentsOrStrings.push('`');
      noCommentsOrRegex.push('`');
      moduleScanTokens.push('`');
      i += 1;
      while (i < content.length && content[i] !== '`') {
        if (content[i] === '\n') {
          pushAll('\n');
        } else if (content[i] !== '\r') {
          noCommentsOrRegex.push(content[i]);
        }
        i += 1;
      }
      if (i < content.length && content[i] === '`') {
        noCommentsOrStrings.push('`');
        noCommentsOrRegex.push('`');
        moduleScanTokens.push('`');
        i += 1;
      }
      updateTokenChar('`');
      prevWord = '';
      afterConditionParen = false;
      continue;
    }

    if (isJs && ch === '`') {
      noCommentsOrStrings.push('`');
      noCommentsOrRegex.push('`');
      moduleScanTokens.push('`');
      i += 1;
      consumeTemplateText();
      continue;
    }

    if (isRs && ch === "'" && !RUST_CHAR_LITERAL_RE.test(content.slice(i))) {
      prevWord = '';
      afterConditionParen = false;
      pushAll(ch);
      i += 1;
      continue;
    }

    if (ch === '"' || ch === "'") {
      const quote = ch;
      const strChars = [];
      let extraNewlines = 0;
      noCommentsOrStrings.push(quote);
      noCommentsOrRegex.push(quote);
      i += 1;
      while (i < content.length && content[i] !== quote) {
        if (content[i] === '\\' && i + 1 < content.length) {
          if (content[i + 1] === '\r' && content[i + 2] === '\n') {
            noCommentsOrStrings.push('\n');
            noCommentsOrRegex.push('\n');
            extraNewlines += 1;
            lineNo += 1;
            i += 3;
            continue;
          }
          const esc = content[i + 1];
          if (esc === '\n') {
            noCommentsOrStrings.push('\n');
            noCommentsOrRegex.push('\n');
            extraNewlines += 1;
            lineNo += 1;
          } else {
            strChars.push(esc);
            noCommentsOrRegex.push(' ', ' ');
          }
          i += 2;
          continue;
        }
        if (content[i] === '\n') {
          break;
        }
        if (content[i] !== '\r') {
          strChars.push(content[i]);
          noCommentsOrRegex.push(content[i]);
        }
        i += 1;
      }
      const strId = stringValues.length;
      stringValues.push(strChars.join(''));
      moduleScanTokens.push(`"__KRIG_STR_${strId}__"`);
      for (let nl = 0; nl < extraNewlines; nl += 1) {
        moduleScanTokens.push('\n');
      }
      if (i < content.length && content[i] === quote) {
        noCommentsOrStrings.push(quote);
        noCommentsOrRegex.push(quote);
        i += 1;
      }
      updateTokenChar(quote);
      prevWord = '';
      afterConditionParen = false;
      continue;
    }

    if (
      isJs
      && ch === '/'
      && next !== '/'
      && next !== '*'
      && canStartJsRegex(prevTokenChar, prevTwoChars, prevWord, afterConditionParen)
    ) {
      let j = i + 1;
      let inCharClass = false;
      let validRegex = false;
      while (j < content.length) {
        const rc = content[j];
        if (rc === '\n' || rc === '\r') break;
        if (rc === '\\') {
          if (content[j + 1] === '\n' || content[j + 1] === '\r') break;
          j += 2;
          continue;
        }
        if (rc === '[') inCharClass = true;
        else if (rc === ']') inCharClass = false;
        else if (rc === '/' && !inCharClass) {
          validRegex = true;
          j += 1;
          while (j < content.length && /[a-z]/i.test(content[j])) j += 1;
          break;
        }
        j += 1;
      }
      if (validRegex) {
        noCommentsOrStrings.push('/', ' ', '/');
        noCommentsOrRegex.push('/', ' ', '/');
        moduleScanTokens.push('/', ' ', '/');
        i = j;
        updateTokenChar('/');
        prevWord = '';
        afterConditionParen = false;
        continue;
      }
    }

    if (/[A-Za-z_$]/.test(ch)) {
      const precededByDot = prevTokenChar === '.';
      let j = i + 1;
      while (j < content.length && /[A-Za-z0-9_$]/.test(content[j])) j += 1;
      const word = content.slice(i, j);
      for (const wc of word) {
        noCommentsOrStrings.push(wc);
        noCommentsOrRegex.push(wc);
        moduleScanTokens.push(wc);
      }
      prevWord = precededByDot ? '' : word;
      afterConditionParen = false;
      updateTokenChar(word[word.length - 1]);
      i = j;
      continue;
    }

    if (isJs) {
      if (ch === '(') {
        parenKeywordStack.push(prevWord);
        afterConditionParen = false;
      } else if (ch === ')') {
        const openerWord = parenKeywordStack.pop() ?? '';
        afterConditionParen = CONDITIONAL_PAREN_KEYWORDS.has(openerWord);
      } else if (!/\s/.test(ch)) {
        afterConditionParen = false;
      }

      if (templateExprStack.length > 0) {
        if (ch === '{') {
          templateExprStack[templateExprStack.length - 1] += 1;
        } else if (ch === '}') {
          templateExprStack[templateExprStack.length - 1] -= 1;
          if (templateExprStack[templateExprStack.length - 1] === 0) {
            templateExprStack.pop();
            prevWord = '';
            afterConditionParen = false;
            pushAll('}');
            i += 1;
            consumeTemplateText();
            continue;
          }
        }
      }
    }

    prevWord = '';
    pushAll(ch);
    i += 1;
  }

  return {
    allowLines,
    comments,
    cleanLines: noCommentsOrStrings.join('').split('\n'),
    stringKeptLines: noCommentsOrRegex.join('').split('\n'),
    moduleScanText: moduleScanTokens.join(''),
    stringValues
  };
}

const JS_RESOLVE_EXTS = ['.mjs', '.js', '.ts', '.tsx', '.cjs', '.jsx'];

export function extractJsModuleSpecifiers(input) {
  let scanText = '';
  let strTable = [];

  if (input && typeof input === 'object' && !Array.isArray(input) && Array.isArray(input.stringValues)) {
    scanText = String(input.moduleScanText ?? '');
    strTable = input.stringValues;
  } else {
    const raw = Array.isArray(input) ? input.join('\n') : String(input ?? '');
    const reLexed = lexSource(raw, '.mjs');
    scanText = reLexed.moduleScanText;
    strTable = reLexed.stringValues;
  }

  const specs = new Set();
  const patterns = [
    /\bimport\s+"__KRIG_STR_(\d+)__"/g,
    /\bimport\s+(?:type\s+)?(?:[A-Za-z_$][A-Za-z0-9_$]*|\*\s+as\s+[A-Za-z_$][A-Za-z0-9_$]*|\{[^{};]*\}|[A-Za-z_$][A-Za-z0-9_$]*\s*,\s*(?:\*\s+as\s+[A-Za-z_$][A-Za-z0-9_$]*|\{[^{};]*\}))\s+from\s+"__KRIG_STR_(\d+)__"/g,
    /\bexport\s+(?:type\s+)?(?:\*(?:\s+as\s+[A-Za-z_$][A-Za-z0-9_$]*)?|\{[^{};]*\})\s+from\s+"__KRIG_STR_(\d+)__"/g,
    /\b(?:import|require)\s*\(\s*"__KRIG_STR_(\d+)__"\s*\)/g
  ];

  for (const re of patterns) {
    let match;
    while ((match = re.exec(scanText)) !== null) {
      const idx = Number.parseInt(match[1], 10);
      const spec = (strTable[idx] ?? '').trim();
      if (spec) specs.add(spec);
    }
  }
  return [...specs];
}

export function maskJsImportExportSpans(cleanLines) {
  const joined = Array.isArray(cleanLines) ? cleanLines.join('\n') : String(cleanLines ?? '');
  const masked = joined
    .replace(
      /\bimport\s+(?:type\s+)?(?:[A-Za-z_$][A-Za-z0-9_$]*|\*\s+as\s+[A-Za-z_$][A-Za-z0-9_$]*|\{[^{};]*\}|[A-Za-z_$][A-Za-z0-9_$]*\s*,\s*(?:\*\s+as\s+[A-Za-z_$][A-Za-z0-9_$]*|\{[^{};]*\}))\s+from\s+['"][^'"]*['"]/g,
      (m) => m.replace(/[^\n]/g, ' ')
    )
    .replace(
      /\bexport\s+(?:type\s+)?(?:\*(?:\s+as\s+[A-Za-z_$][A-Za-z0-9_$]*)?|\{[^{};]*\})(?:\s+from\s+['"][^'"]*['"])?/g,
      (m) => m.replace(/[^\n]/g, ' ')
    );
  return masked.split('\n');
}

export function resolveLocalModule(fromFileAbs, specifier) {
  if (!specifier.startsWith('.')) return null;
  const baseDir = path.dirname(path.resolve(fromFileAbs));
  const rawCandidate = path.resolve(baseDir, specifier);
  const candidates = [rawCandidate];

  const ext = path.extname(rawCandidate).toLowerCase();
  if (ext === '.js' || ext === '.mjs' || ext === '.cjs') {
    const stem = rawCandidate.slice(0, -ext.length);
    candidates.push(`${stem}.ts`, `${stem}.tsx`, `${stem}.mts`, `${stem}.cts`);
  } else if (!ext) {
    for (const tryExt of JS_RESOLVE_EXTS) {
      candidates.push(`${rawCandidate}${tryExt}`);
    }
    for (const tryExt of JS_RESOLVE_EXTS) {
      candidates.push(path.join(rawCandidate, `index${tryExt}`));
    }
  }

  for (const cand of candidates) {
    if (fs.existsSync(cand) && fs.statSync(cand).isFile()) {
      return path.resolve(cand);
    }
  }
  return null;
}
