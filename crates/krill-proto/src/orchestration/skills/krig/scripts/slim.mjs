#!/usr/bin/env node
import fs from 'node:fs';
import path from 'node:path';
import { isCliMain, parseCliFlags } from './lib/cli.mjs';
import {
  BUILD_OR_VCS_DIRS,
  collectCodeFiles,
  extractJsModuleSpecifiers,
  JS_EXTS,
  lexSource,
  resolveLocalModule
} from './lib/source-lex.mjs';

const DEBT_HEADER_RE = /^(?:(krig-debt|ponytail)\s*:\s*(.+)|(TODO|FIXME|HACK|WORKAROUND|XXX)(?:\(([^)]*)\))?\s*(?:[:\-]\s*(.+)|\s+(.+)))$/i;
const UPGRADE_SPLIT_RE = /\s*(?:->|=>|\bupgrade\s*:\s*|\brevisit\s*:\s*|\btrigger\s*:\s*)/i;
const CONDITIONAL_TRIGGER_RE = /\b(?:when|until|once|after|if\s+\w+|before\s+v\d)\b|(?:초과\s*시|이상\s*시|도달\s*시|전환\s*시|도입\s*시|제거\s*시)/i;

function escapeRegExp(str) {
  return String(str).replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
}

export function parseDebtComment(commentBody) {
  const trimmed = String(commentBody ?? '').trim();
  const match = DEBT_HEADER_RE.exec(trimmed);
  if (!match) return null;

  const structuredTag = match[1] ? match[1].toLowerCase() : null;
  const standardTag = match[3] ? match[3].toUpperCase() : null;
  const tag = structuredTag ?? standardTag;
  const ownerHint = match[4] ? match[4].trim() : null;
  const rawRest = (match[2] ?? match[5] ?? match[6] ?? '').trim();
  if (!rawRest) return null;

  let ceiling = rawRest;
  let upgradeTrigger = null;

  const splitMatch = UPGRADE_SPLIT_RE.exec(rawRest);
  if (splitMatch) {
    ceiling = rawRest.slice(0, splitMatch.index).replace(/[,;]\s*$/, '').trim();
    upgradeTrigger = rawRest.slice(splitMatch.index + splitMatch[0].length).trim() || null;
  } else if (CONDITIONAL_TRIGGER_RE.test(rawRest)) {
    upgradeTrigger = rawRest;
  }

  return {
    tag,
    owner: ownerHint,
    ceiling: ceiling || rawRest,
    upgradeTrigger,
    rotRisk: !upgradeTrigger
  };
}

function normalizePackageRoot(specifier) {
  if (!specifier || specifier.startsWith('.') || specifier.startsWith('/') || specifier.startsWith('node:')) {
    return null;
  }
  if (specifier.startsWith('@')) {
    const parts = specifier.split('/');
    return parts.length >= 2 ? `${parts[0]}/${parts[1]}` : specifier;
  }
  return specifier.split('/')[0];
}

function collectAuxTextFiles(rootDir) {
  const texts = [];
  function visit(dir, depth) {
    if (depth > 3 || !fs.existsSync(dir)) return;
    for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
      if (BUILD_OR_VCS_DIRS.has(entry.name)) continue;
      const full = path.join(dir, entry.name);
      if (entry.isDirectory()) {
        if (entry.name === '.github' || entry.name === 'scripts' || entry.name === 'skills' || entry.name === 'workflows') {
          visit(full, depth + 1);
        }
      } else if (/\.(?:json|ya?ml|toml|sh|md)$/i.test(entry.name) && entry.name !== 'package-lock.json') {
        texts.push({ abs: full, rel: path.relative(rootDir, full).replace(/\\/g, '/'), content: fs.readFileSync(full, 'utf8') });
      }
    }
  }
  visit(rootDir, 0);
  return texts;
}

function isPureReexportModule(cleanLines) {
  const stmts = cleanLines.map((l) => l.trim()).filter(Boolean);
  if (stmts.length === 0) return false;
  const joined = stmts.join('\n');
  const stripped = joined
    .replace(/\bexport\s+(?:type\s+)?\*\s*(?:as\s+[A-Za-z0-9_$]+\s+)?from\s+['"][^'"\r\n]*['"]\s*;?/g, '')
    .replace(/\bexport\s+(?:type\s+)?\{[^{};]*\}\s+from\s+['"][^'"\r\n]*['"]\s*;?/g, '')
    .trim();
  return stripped === '';
}

function isLikelyEntrypoint(file, rawContent, pkgEntrypointRels, auxCorpus) {
  if (pkgEntrypointRels.has(file.rel)) return true;
  if (rawContent.startsWith('#!')) return true;
  if (/\bisCliMain\s*\(/.test(rawContent)) return true;
  const base = path.basename(file.rel);
  if (/^(?:index|main|cli|server|app|mod|plugin)\.[a-z]+$/i.test(base) && !file.rel.includes('/lib/') && !file.rel.includes('/helpers/')) {
    return true;
  }
  const relBoundaryRe = new RegExp(`(?:^|[^A-Za-z0-9_.-])${escapeRegExp(file.rel)}(?=$|[^A-Za-z0-9_.-])`);
  return relBoundaryRe.test(auxCorpus);
}

function collectPkgEntrypoints(rootDir, pkgJson) {
  const rels = new Set();
  if (!pkgJson || typeof pkgJson !== 'object') return rels;

  function addCandidate(val) {
    if (typeof val === 'string') {
      const cleaned = val.replace(/^\.\//, '');
      const abs = path.resolve(rootDir, cleaned);
      if (fs.existsSync(abs) && fs.statSync(abs).isFile()) {
        rels.add(path.relative(rootDir, abs).replace(/\\/g, '/'));
      }
    } else if (val && typeof val === 'object') {
      for (const sub of Object.values(val)) addCandidate(sub);
    }
  }

  addCandidate(pkgJson.main);
  addCandidate(pkgJson.module);
  addCandidate(pkgJson.bin);
  addCandidate(pkgJson.exports);
  if (pkgJson.scripts && typeof pkgJson.scripts === 'object') {
    const scriptBlob = Object.values(pkgJson.scripts).join('\n');
    const tokenRe = /[A-Za-z0-9_./-]+\.(?:mjs|js|ts|cjs|py|sh)/g;
    let m;
    while ((m = tokenRe.exec(scriptBlob)) !== null) {
      addCandidate(m[0]);
    }
  }
  return rels;
}

export function analyzeSlim(targetDir = process.cwd(), options = {}) {
  const rootDir = path.resolve(targetDir);
  if (!fs.existsSync(rootDir) || !fs.statSync(rootDir).isDirectory()) {
    throw new Error(`Audit target directory does not exist: ${targetDir}`);
  }

  const rawHotspot = options.hotspotLines ?? '350';
  const parsedHotspot = Number.parseInt(String(rawHotspot), 10);
  if (!Number.isInteger(parsedHotspot) || parsedHotspot < 1) {
    throw new Error(`Invalid --hotspot-lines value (positive integer required): ${rawHotspot}`);
  }
  const hotspotLines = Math.max(50, parsedHotspot);
  const allCodeFiles = collectCodeFiles(rootDir, { includeTests: true });
  const auxFiles = collectAuxTextFiles(rootDir);

  let pkgJson = null;
  const pkgPath = path.join(rootDir, 'package.json');
  if (fs.existsSync(pkgPath)) {
    try {
      pkgJson = JSON.parse(fs.readFileSync(pkgPath, 'utf8'));
    } catch (err) {
      throw new Error(`Failed to parse package.json (${pkgPath}): ${err.message}`);
    }
  }

  const hasTsFilesOrConfig = fs.existsSync(path.join(rootDir, 'tsconfig.json'))
    || allCodeFiles.some((f) => /\.tsx?$/i.test(f.abs));

  const pkgEntrypointRels = collectPkgEntrypoints(rootDir, pkgJson);
  const auxWithoutPkgDeps = auxFiles
    .filter((f) => path.basename(f.abs) !== 'package.json')
    .map((f) => f.content)
    .join('\n');

  const debtLedger = [];
  const referencedPackages = new Set();
  const parsedProdJs = [];
  const prodImportersByRel = new Map();
  const testImportersByRel = new Map();
  const hotspots = [];

  for (const entry of allCodeFiles) {
    const rawContent = fs.readFileSync(entry.abs, 'utf8');
    const ext = path.extname(entry.abs).toLowerCase();
    const rel = path.relative(rootDir, entry.abs).replace(/\\/g, '/');
    const lexed = lexSource(rawContent, ext);

    if (!entry.isTest) {
      for (const c of lexed.comments) {
        const parsedDebt = parseDebtComment(c.body);
        if (parsedDebt) {
          debtLedger.push({
            file: rel,
            line: c.line,
            isTest: false,
            ...parsedDebt
          });
        }
      }
    }

    if (JS_EXTS.has(ext)) {
      const specs = extractJsModuleSpecifiers(lexed);
      const localTargets = [];
      for (const spec of specs) {
        if (spec.startsWith('.')) {
          const resolved = resolveLocalModule(entry.abs, spec);
          if (resolved) {
            const targetRel = path.relative(rootDir, resolved).replace(/\\/g, '/');
            localTargets.push(targetRel);
            const map = entry.isTest ? testImportersByRel : prodImportersByRel;
            const set = map.get(targetRel) ?? new Set();
            set.add(rel);
            map.set(targetRel, set);
          }
        } else {
          const pkgRoot = normalizePackageRoot(spec);
          if (pkgRoot) referencedPackages.add(pkgRoot);
        }
      }

      if (!entry.isTest) {
        parsedProdJs.push({
          rel,
          abs: entry.abs,
          rawContent,
          cleanLines: lexed.cleanLines,
          localTargets: [...new Set(localTargets)]
        });
      }
    }

    if (!entry.isTest) {
      const nonEmptyClean = lexed.cleanLines.filter((l) => l.trim().length > 0);
      const codeLineCount = nonEmptyClean.length;
      if (codeLineCount >= hotspotLines) {
        let branchCount = 0;
        for (const l of nonEmptyClean) {
          const matches = l.match(/\b(?:if|for|while|switch|catch|match)\b/g);
          if (matches) branchCount += matches.length;
        }
        hotspots.push({
          file: rel,
          codeLines: codeLineCount,
          branchCount
        });
      }
    }
  }

  const unusedDependencies = [];
  if (pkgJson) {
    const scriptsAndMetaBlob = JSON.stringify({
      scripts: pkgJson.scripts ?? {},
      bin: pkgJson.bin ?? {},
      eslintConfig: pkgJson.eslintConfig ?? {}
    });
    const combinedConfigText = `${scriptsAndMetaBlob}\n${auxWithoutPkgDeps}`;
    const allDeclared = [
      ...Object.keys(pkgJson.dependencies ?? {}).map((name) => ({ name, scope: 'dependencies' })),
      ...Object.keys(pkgJson.devDependencies ?? {}).map((name) => ({ name, scope: 'devDependencies' }))
    ];

    for (const dep of allDeclared) {
      if (referencedPackages.has(dep.name)) continue;
      if (dep.name === 'typescript' && (hasTsFilesOrConfig || /\btsc\b/.test(scriptsAndMetaBlob))) {
        continue;
      }
      if (dep.name.startsWith('@types/')) {
        const rawTypeTarget = dep.name.slice('@types/'.length);
        const basePkg = rawTypeTarget.includes('__')
          ? `@${rawTypeTarget.replace('__', '/')}`
          : rawTypeTarget;
        if (basePkg === 'node' || referencedPackages.has(basePkg) || hasTsFilesOrConfig) {
          continue;
        }
      }
      const shortBin = dep.name.includes('/') ? dep.name.split('/')[1] : dep.name;
      if (
        combinedConfigText.includes(dep.name)
        || new RegExp(`(?<![A-Za-z0-9_-])${escapeRegExp(shortBin)}(?![A-Za-z0-9_-])`).test(scriptsAndMetaBlob)
      ) {
        continue;
      }
      unusedDependencies.push(dep);
    }
  }

  const passThroughModules = [];
  const unreferencedModules = [];

  for (const mod of parsedProdJs) {
    const prodImporters = [...(prodImportersByRel.get(mod.rel) ?? [])].sort();
    const testImporters = [...(testImportersByRel.get(mod.rel) ?? [])].sort();
    const entrypoint = isLikelyEntrypoint(mod, mod.rawContent, pkgEntrypointRels, auxWithoutPkgDeps);

    if (!pkgEntrypointRels.has(mod.rel) && !(mod.rel === 'src/index.mjs' || mod.rel === 'src/index.js' || mod.rel === 'src/index.ts' || mod.rel === 'index.mjs' || mod.rel === 'index.js' || mod.rel === 'index.ts') && isPureReexportModule(mod.cleanLines)) {
      passThroughModules.push({
        file: mod.rel,
        kind: 'pure-reexport-barrel',
        prodImporterCount: prodImporters.length,
        targets: mod.localTargets,
        recommendation: 'Import the defining module directly from callers and remove the pass-through barrel file.'
      });
    }

    if (!entrypoint && prodImporters.length === 0) {
      unreferencedModules.push({
        file: mod.rel,
        testImporterCount: testImporters.length,
        recommendation: testImporters.length > 0
          ? 'Zero production callers; referenced only by tests. Confirm whether the feature is retired and remove module + tests together.'
          : 'Unreferenced by any production module or entrypoint; candidate orphan module for deletion.'
      });
    }
  }

  hotspots.sort((a, b) => b.codeLines - a.codeLines || b.branchCount - a.branchCount);

  const rotRiskCount = debtLedger.filter((d) => d.rotRisk).length;
  const bloatFindingCount = unusedDependencies.length + passThroughModules.length + unreferencedModules.length;

  return {
    rootDir,
    scannedFiles: allCodeFiles.filter((f) => !f.isTest).length,
    summary: {
      debtCount: debtLedger.length,
      rotRiskDebtCount: rotRiskCount,
      unusedDependencyCount: unusedDependencies.length,
      passThroughCount: passThroughModules.length,
      unreferencedModuleCount: unreferencedModules.length,
      hotspotCount: hotspots.length
    },
    debtLedger,
    unusedDependencies,
    passThroughModules,
    unreferencedModules,
    hotspots,
    actionableCount: rotRiskCount + bloatFindingCount
  };
}

function printHelp() {
  console.log('Usage: node slim.mjs [--json] [--strict] [--repo <target_dir>] [--hotspot-lines <lines>] [<target_dir>]');
}

function main() {
  let parsed;
  try {
    parsed = parseCliFlags(process.argv.slice(2), {
      booleans: ['json', 'help', 'strict'],
      allowedFlags: ['json', 'strict', 'repo', 'hotspot-lines']
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
    console.error(`Only one target directory may be specified: ${parsed.positional.join(' ')}`);
    process.exit(2);
  }

  const targetDir = parsed.flags.repo ?? parsed.positional[0] ?? process.cwd();

  try {
    const report = analyzeSlim(targetDir, {
      hotspotLines: parsed.flags['hotspot-lines']
    });

    if (parsed.flags.json) {
      console.log(JSON.stringify(report, null, 2));
    } else {
      const s = report.summary;
      console.log(
        `Slim audit complete (${report.scannedFiles} production files): debt=${s.debtCount} (rotRisk=${s.rotRiskDebtCount}), unusedDeps=${s.unusedDependencyCount}, passThrough=${s.passThroughCount}, unreferenced=${s.unreferencedModuleCount}, hotspots=${s.hotspotCount}`
      );
      for (const dep of report.unusedDependencies) {
        console.log(`  [delete:dep] package.json (${dep.scope}): ${dep.name}`);
      }
      for (const pt of report.passThroughModules) {
        console.log(`  [delete:barrel] ${pt.file} (importers=${pt.prodImporterCount})`);
      }
      for (const orph of report.unreferencedModules) {
        console.log(`  [delete:orphan] ${orph.file} (testImporters=${orph.testImporterCount})`);
      }
      for (const debt of report.debtLedger) {
        const flag = debt.rotRisk ? 'no-trigger' : `upgrade=${debt.upgradeTrigger}`;
        console.log(`  [debt:${debt.tag}] ${debt.file}:${debt.line} ${debt.ceiling} (${flag})`);
      }
      for (const hs of report.hotspots) {
        console.log(`  [shrink:hotspot] ${hs.file} (codeLines=${hs.codeLines}, branches=${hs.branchCount})`);
      }
    }

    if (parsed.flags.strict && report.actionableCount > 0) {
      process.exit(3);
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
