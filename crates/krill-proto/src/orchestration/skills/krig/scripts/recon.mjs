#!/usr/bin/env node
import { spawnSync } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';
import { isCliMain, parseCliFlags } from './lib/cli.mjs';
import { isGitRepo } from './lib/git-state.mjs';
import {
  collectCodeFiles,
  extractJsModuleSpecifiers,
  JS_EXTS,
  lexSource,
  maskJsImportExportSpans,
  resolveLocalModule
} from './lib/source-lex.mjs';

function escapeRegExp(str) {
  return String(str).replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
}

function buildIdentifierRegex(symbol) {
  const escaped = escapeRegExp(symbol);
  return new RegExp(`(?<![A-Za-z0-9_$])${escaped}(?![A-Za-z0-9_$])`);
}

function detectSymbolDeclaration(cleanLine, ext, symbol) {
  const esc = escapeRegExp(symbol);
  const endBound = '(?![A-Za-z0-9_$])';
  if (JS_EXTS.has(ext)) {
    const exported = /\bexport\b/.test(cleanLine);
    const rules = [
      { kind: 'function', re: new RegExp(`\\bfunction\\s*\\*?\\s+${esc}${endBound}`) },
      { kind: 'class', re: new RegExp(`\\bclass\\s+${esc}${endBound}`) },
      { kind: 'type', re: new RegExp(`\\b(?:interface|type|enum)\\s+${esc}${endBound}`) },
      { kind: 'const', re: new RegExp(`\\b(?:const|let|var)\\s+${esc}${endBound}`) }
    ];
    for (const rule of rules) {
      if (rule.re.test(cleanLine)) {
        return { kind: rule.kind, exported };
      }
    }
    return null;
  }

  if (ext === '.py') {
    if (new RegExp(`^\\s*(?:async\\s+)?def\\s+${esc}\\b`).test(cleanLine)) {
      return { kind: 'function', exported: !symbol.startsWith('_') };
    }
    if (new RegExp(`^\\s*class\\s+${esc}\\b`).test(cleanLine)) {
      return { kind: 'class', exported: !symbol.startsWith('_') };
    }
    if (new RegExp(`^${esc}\\s*=`).test(cleanLine)) {
      return { kind: 'const', exported: !symbol.startsWith('_') };
    }
    return null;
  }

  if (ext === '.rs') {
    const exported = /\bpub\b/.test(cleanLine);
    const rules = [
      { kind: 'function', re: new RegExp(`\\bfn\\s+${esc}\\b`) },
      { kind: 'type', re: new RegExp(`\\b(?:struct|enum|trait|type)\\s+${esc}\\b`) },
      { kind: 'const', re: new RegExp(`\\b(?:const|static)\\s+${esc}\\b`) }
    ];
    for (const rule of rules) {
      if (rule.re.test(cleanLine)) return { kind: rule.kind, exported };
    }
    return null;
  }

  if (ext === '.go') {
    const exported = /^[A-Z]/.test(symbol);
    if (new RegExp(`\\bfunc\\s+(?:\\([^)]*\\)\\s*)?${esc}\\b`).test(cleanLine)) {
      return { kind: 'function', exported };
    }
    if (new RegExp(`\\btype\\s+${esc}\\b`).test(cleanLine)) {
      return { kind: 'type', exported };
    }
  }

  return null;
}

function isNonJsImportLine(cleanLine, ext) {
  const trimmed = cleanLine.trim();
  if (ext === '.py') {
    return /^(?:import|from)\s+\S+/.test(trimmed);
  }
  if (ext === '.rs') {
    return /^(?:pub\s+)?use\s+/.test(trimmed);
  }
  return false;
}

const BOUNDARY_RULES = Object.freeze([
  {
    kind: 'env',
    re: /\b(?:process\.env(?:\.[A-Za-z0-9_]+|\[)|os\.environ|os\.getenv|std::env::var|os\.Getenv)\b/
  },
  {
    kind: 'cli',
    re: /\b(?:process\.argv|parseCliFlags|argparse|sys\.argv|clap::|os\.Args)\b/
  },
  {
    kind: 'subprocess',
    re: /\b(?:spawnSync|spawn|execFileSync|execFile|execSync|subprocess\.(?:run|Popen|check_output)|Command::new|exec\.Command)\b/
  },
  {
    kind: 'filesystem',
    re: /\b(?:fs\.(?:readFileSync|writeFileSync|appendFileSync|readdirSync|rmSync|unlinkSync|mkdirSync|symlinkSync|promises)|open\s*\(|std::fs::|os\.(?:ReadFile|WriteFile|RemoveAll))\b/
  },
  {
    kind: 'network',
    re: /\b(?:fetch\s*\(|https?\.(?:request|get)|requests\.(?:get|post|put|delete)|httpx\.|reqwest::|http\.(?:Get|Post|NewRequest))\b/
  }
]);

function queryGitCommits(repoDir, extraArgs) {
  const res = spawnSync(
    'git',
    ['-C', repoDir, 'log', '--format=%H%x09%ad%x09%an%x09%s', '--date=short', ...extraArgs],
    { encoding: 'utf8' }
  );
  if (res.status !== 0 || !res.stdout.trim()) return [];
  return res.stdout
    .trim()
    .split(/\r?\n/)
    .filter(Boolean)
    .map((row) => {
      const [sha = '', date = '', author = '', subject = ''] = row.split('\t');
      return { sha: sha.slice(0, 12), fullSha: sha, date, author, subject };
    });
}

export function analyzeRecon(options = {}) {
  const repoDir = path.resolve(options.repoDir ?? process.cwd());
  if (!fs.existsSync(repoDir) || !fs.statSync(repoDir).isDirectory()) {
    throw new Error(`Target repository directory does not exist: ${repoDir}`);
  }

  const symbol = options.symbol ? String(options.symbol).trim() : null;
  const rawTargetPath = options.targetPath ? String(options.targetPath).trim() : null;
  const parsedLimit = Number.parseInt(String(options.limit ?? '8'), 10);
  if (!Number.isInteger(parsedLimit) || parsedLimit < 1) {
    throw new Error(`Invalid --limit value (integer >= 1 required): ${options.limit}`);
  }
  const limit = parsedLimit;

  let resolvedTarget = null;
  if (rawTargetPath) {
    resolvedTarget = path.resolve(repoDir, rawTargetPath);
    if (!fs.existsSync(resolvedTarget)) {
      throw new Error(`Analysis target path does not exist: ${rawTargetPath}`);
    }
  }
  const hasSpecificTargetPath = Boolean(resolvedTarget && resolvedTarget !== repoDir);

  const allFiles = collectCodeFiles(repoDir, { includeTests: true });
  const parsedFiles = allFiles.map((entry) => {
    const content = fs.readFileSync(entry.abs, 'utf8');
    const rawLines = content.split(/\r?\n/);
    const ext = path.extname(entry.abs).toLowerCase();
    const lexed = lexSource(content, ext);
    const rel = path.relative(repoDir, entry.abs).replace(/\\/g, '/');
    const specifiers = JS_EXTS.has(ext) ? extractJsModuleSpecifiers(lexed) : [];
    const resolvedLocalImports = [];
    const externalImports = [];
    for (const spec of specifiers) {
      if (spec.startsWith('.')) {
        const targetAbs = resolveLocalModule(entry.abs, spec);
        if (targetAbs) {
          resolvedLocalImports.push(path.relative(repoDir, targetAbs).replace(/\\/g, '/'));
        }
      } else {
        externalImports.push(spec);
      }
    }
    const callerScanLines = JS_EXTS.has(ext)
      ? maskJsImportExportSpans(lexed.cleanLines)
      : lexed.cleanLines;

    return {
      ...entry,
      rel,
      ext,
      rawLines,
      lexed,
      callerScanLines,
      resolvedLocalImports: [...new Set(resolvedLocalImports)].sort(),
      externalImports: [...new Set(externalImports)].sort()
    };
  });

  const definitions = [];
  const productionCallers = [];
  const testCallers = [];

  if (symbol) {
    const idRe = buildIdentifierRegex(symbol);
    for (const file of parsedFiles) {
      for (let idx = 0; idx < file.lexed.cleanLines.length; idx += 1) {
        const cleanLine = file.lexed.cleanLines[idx];
        if (!cleanLine.trim() || !idRe.test(cleanLine)) continue;

        const snippet = (file.rawLines[idx] ?? '').trim().slice(0, 140);
        const decl = detectSymbolDeclaration(cleanLine, file.ext, symbol);
        if (decl) {
          definitions.push({
            file: file.rel,
            line: idx + 1,
            kind: decl.kind,
            exported: decl.exported,
            isTest: file.isTest,
            snippet
          });
          continue;
        }

        const callerLine = file.callerScanLines[idx] ?? '';
        if (!callerLine.trim() || !idRe.test(callerLine)) continue;
        if (isNonJsImportLine(callerLine, file.ext)) continue;

        const callerEntry = { file: file.rel, line: idx + 1, snippet };
        if (file.isTest) {
          testCallers.push(callerEntry);
        } else {
          productionCallers.push(callerEntry);
        }
      }
    }
  }

  const targetRelSet = new Set();
  if (hasSpecificTargetPath) {
    for (const file of parsedFiles) {
      if (file.abs === resolvedTarget || file.abs.startsWith(`${resolvedTarget}${path.sep}`)) {
        targetRelSet.add(file.rel);
      }
    }
  } else if (definitions.length > 0) {
    for (const def of definitions) {
      targetRelSet.add(def.file);
    }
  } else {
    for (const file of parsedFiles) {
      if (!file.isTest) targetRelSet.add(file.rel);
    }
  }

  const outboundLocal = new Set();
  const outboundExternal = new Set();
  const boundarySurfaces = [];

  for (const file of parsedFiles) {
    if (!targetRelSet.has(file.rel)) continue;
    for (const loc of file.resolvedLocalImports) {
      if (!targetRelSet.has(loc)) outboundLocal.add(loc);
    }
    for (const extPkg of file.externalImports) outboundExternal.add(extPkg);

    for (let idx = 0; idx < file.lexed.cleanLines.length; idx += 1) {
      const cleanLine = file.lexed.cleanLines[idx];
      if (!cleanLine.trim()) continue;
      for (const rule of BOUNDARY_RULES) {
        if (rule.re.test(cleanLine)) {
          boundarySurfaces.push({
            file: file.rel,
            line: idx + 1,
            kind: rule.kind,
            snippet: (file.rawLines[idx] ?? '').trim().slice(0, 140)
          });
        }
      }
    }
  }

  const productionImporters = [];
  const testImporters = [];
  for (const file of parsedFiles) {
    if (targetRelSet.has(file.rel)) continue;
    const touchesTarget = file.resolvedLocalImports.some((imp) => targetRelSet.has(imp));
    if (touchesTarget) {
      if (file.isTest) testImporters.push(file.rel);
      else productionImporters.push(file.rel);
    }
  }

  const gitAvailable = isGitRepo(repoDir);
  let pickaxeCommits = [];
  let recentCommits = [];

  if (gitAvailable) {
    if (symbol) {
      pickaxeCommits = queryGitCommits(repoDir, [`-n${limit}`, '-S', symbol]);
    }
    const relTarget = hasSpecificTargetPath ? path.relative(repoDir, resolvedTarget) : '';
    const commitPathArgs = relTarget
      ? ['--', relTarget]
      : definitions.length > 0
        ? ['--', ...new Set(definitions.map((d) => d.file))]
        : [];
    recentCommits = queryGitCommits(repoDir, [`-n${limit}`, ...commitPathArgs]);
  }

  const boundaryKinds = [...new Set(boundarySurfaces.map((b) => b.kind))].sort();
  const prodImpactCount = productionCallers.length + productionImporters.length;
  const highRiskBoundaries = boundaryKinds.filter((k) => k === 'network' || k === 'subprocess' || k === 'filesystem');

  let riskTier = 'low';
  if (prodImpactCount >= 5 || highRiskBoundaries.length >= 2) {
    riskTier = 'high';
  } else if (prodImpactCount >= 1 || boundaryKinds.length >= 1) {
    riskTier = 'medium';
  }

  const proofLadder = [
    `L1 lexical/symbol check: ${definitions.length} definition(s), ${productionCallers.length} production caller(s), ${testCallers.length} test caller(s)`,
    `L2 module dependency graph: ${productionImporters.length} production importer(s), ${testImporters.length} test importer(s)`,
    `L3 external boundary surface: ${boundaryKinds.length > 0 ? boundaryKinds.join(', ') : 'pure in-memory'}`,
    'L4 runtime probe: run concrete inputs before/after change at key entrypoints to verify return shape and side-effect invariants',
    'L5 execution proof of load-bearing safety fact: prove the highest-risk failure path directly with a behavioral test or CLI run'
  ];

  return {
    repoDir,
    gitAvailable,
    symbol,
    targetFiles: [...targetRelSet].sort(),
    definitions,
    productionCallers,
    testCallers,
    imports: {
      local: [...outboundLocal].sort(),
      external: [...outboundExternal].sort()
    },
    importers: {
      production: productionImporters.sort(),
      test: testImporters.sort()
    },
    boundarySurfaces,
    lineage: {
      pickaxeCommits,
      recentCommits
    },
    blastSummary: {
      riskTier,
      prodCallerCount: productionCallers.length,
      testCallerCount: testCallers.length,
      prodImporterCount: productionImporters.length,
      testImporterCount: testImporters.length,
      boundaryKinds,
      proofLadder
    }
  };
}

function printHelp() {
  console.log('Usage: node recon.mjs [--json] [--repo <repo>] [--symbol <name>] [--path <target_path>] [--limit <commits>] [<target_path>]');
}

function main() {
  let parsed;
  try {
    parsed = parseCliFlags(process.argv.slice(2), {
      booleans: ['json', 'help'],
      allowedFlags: ['json', 'repo', 'symbol', 'path', 'limit']
    });
  } catch (err) {
    console.error(err.message);
    process.exit(2);
  }

  if (parsed.flags.help) {
    printHelp();
    process.exit(0);
  }

  if (parsed.positional.length > 1) {
    console.error(`At most one positional path is allowed: ${parsed.positional.join(' ')}`);
    process.exit(2);
  }

  const targetPath = parsed.flags.path ?? parsed.positional[0] ?? null;

  try {
    const report = analyzeRecon({
      repoDir: parsed.flags.repo ?? process.cwd(),
      symbol: parsed.flags.symbol ?? null,
      targetPath,
      limit: parsed.flags.limit ?? 8
    });

    if (parsed.flags.json) {
      console.log(JSON.stringify(report, null, 2));
      return;
    }

    console.log(`Recon target files: ${report.targetFiles.length} (risk tier: ${report.blastSummary.riskTier})`);
    if (report.symbol) {
      console.log(`Symbol [${report.symbol}]: ${report.definitions.length} definition(s), ${report.productionCallers.length} production caller(s), ${report.testCallers.length} test caller(s)`);
      for (const d of report.definitions) {
        console.log(`  Def: ${d.file}:${d.line} (${d.kind}, exported=${d.exported})`);
      }
      for (const c of report.productionCallers.slice(0, 15)) {
        console.log(`  Call: ${c.file}:${c.line} ${c.snippet}`);
      }
    }
    if (report.importers.production.length > 0) {
      console.log(`Production importers: ${report.importers.production.join(', ')}`);
    }
    if (report.blastSummary.boundaryKinds.length > 0) {
      console.log(`External boundary surfaces: ${report.blastSummary.boundaryKinds.join(', ')} (${report.boundarySurfaces.length} site(s))`);
    }
    if (report.lineage.pickaxeCommits.length > 0) {
      console.log('Symbol change lineage (git log -S):');
      for (const c of report.lineage.pickaxeCommits) {
        console.log(`  ${c.sha} ${c.date} ${c.subject}`);
      }
    } else if (report.lineage.recentCommits.length > 0) {
      console.log('Recent commit lineage:');
      for (const c of report.lineage.recentCommits) {
        console.log(`  ${c.sha} ${c.date} ${c.subject}`);
      }
    }
  } catch (err) {
    console.error(err.message);
    process.exit(2);
  }
}

if (isCliMain(import.meta.url)) {
  main();
}
