#!/usr/bin/env node
import fs, { realpathSync } from 'node:fs';
import path from 'node:path';
import { isCliMain, parseCliFlags } from './lib/cli.mjs';
import { reflowMarkdown } from './lib/markdown.mjs';
import { hasTicketId } from './route.mjs';

export { reflowMarkdown };

const PYRAMID_LEVELS = ['architecture', 'type', 'gate', 'test', 'doc'];

const COUPLED_TERMS = [
  {
    hostSpecific: true,
    test: (s) => /\b(?:Claude Code|Codex CLI|Antigravity|Cursor)\b/i.test(s),
    reason: 'Coupled to a specific agent host name (use --allow-host-specific for host-adapter sections).'
  },
  {
    hostSpecific: false,
    test: (s) => /(사용자\s*지적|담당자\s*요청|방금\s*세션|이번\s*세션|어제\s*실수|\blast\s+session\b|\bthis\s+session\b|\byesterday'?s\s+bug\b)/i.test(s),
    reason: 'Contains one-off session timing or individual conversation context.'
  },
  {
    hostSpecific: false,
    test: (s) => hasTicketId(s) || /\bPR\s*#\d+\b/i.test(s),
    reason: 'Embeds a specific ticket or PR number in the rule text. Replace with a generalized invariant.'
  }
];

const PATCH_MARKERS = [
  {
    pattern: /(추가\s*:|예외\s*:|업데이트\s*:|주의\s*:|\bUpdate\s*:|\bNote\s*:|\b202\d-\d{2}-\d{2}\b)/,
    reason: 'Contains patch-accumulation markers (date/addendum/exception tags). Rewrite cleanly as a cohesive v0 paragraph.'
  }
];

export function validateBackpropRule({ level, text, targetPath, allowHostSpecific = false } = {}) {
  const issues = [];
  if (!PYRAMID_LEVELS.includes(level)) {
    issues.push(`Invalid enforcement level (${level}). Allowed: ${PYRAMID_LEVELS.join(', ')}`);
    return { ok: false, rank: -1, canonicalTarget: null, issues };
  }

  const rank = PYRAMID_LEVELS.indexOf(level) + 1;
  const trimmed = String(text ?? '').trim();
  if (!trimmed) {
    issues.push('Rule text or structural change description (text) is empty.');
  }

  let canonicalTarget = null;
  if (targetPath) {
    const absTarget = path.resolve(targetPath);
    if (!fs.existsSync(absTarget)) {
      issues.push(`Canonical target file does not exist: ${targetPath}`);
    } else {
      const realTarget = realpathSync(absTarget);
      canonicalTarget = realTarget;
      if (fs.lstatSync(absTarget).isSymbolicLink()) {
        issues.push(`Target (${targetPath}) is a symlink copy. Edit the single canonical source (${realTarget}) directly.`);
      }
    }
  }

  if (level === 'doc') {
    for (const rule of COUPLED_TERMS) {
      if (allowHostSpecific && rule.hostSpecific) continue;
      if (rule.test(trimmed)) {
        issues.push(rule.reason);
      }
    }
    for (const marker of PATCH_MARKERS) {
      if (marker.pattern.test(trimmed)) {
        issues.push(marker.reason);
      }
    }
  }

  return {
    ok: issues.length === 0,
    level,
    rank,
    canonicalTarget,
    issues
  };
}

function printHelp() {
  console.log('Usage:\n  node unwrap.mjs check-rule --level architecture|type|gate|test|doc --text "<rule>" [--target <file>] [--allow-host-specific] [--json]\n  node unwrap.mjs reflow <markdown_file> [--write]');
}

function main() {
  let parsed;
  try {
    parsed = parseCliFlags(process.argv.slice(2), {
      booleans: ['json', 'help', 'write', 'allow-host-specific'],
      allowedFlags: ['json', 'write', 'allow-host-specific', 'level', 'text', 'target']
    });
  } catch (err) {
    console.error(err.message);
    process.exit(2);
  }

  const subcmd = parsed.positional[0];
  if (!subcmd || parsed.flags.help) {
    printHelp();
    process.exit(0);
  }

  try {
    if (subcmd === 'check-rule') {
      const res = validateBackpropRule({
        level: parsed.flags.level,
        text: parsed.flags.text,
        targetPath: parsed.flags.target,
        allowHostSpecific: Boolean(parsed.flags['allow-host-specific'])
      });
      if (parsed.flags.json) {
        console.log(JSON.stringify(res, null, 2));
      } else if (res.ok) {
        console.log(`OK: passed enforcement level ${res.rank} (${res.level})`);
      } else {
        for (const issue of res.issues) console.error(`Violation: ${issue}`);
      }
      process.exit(res.ok ? 0 : 1);
    }

    if (subcmd === 'reflow') {
      const target = parsed.positional[1];
      if (!target) {
        console.error('Markdown file path is required.');
        process.exit(2);
      }
      const original = fs.readFileSync(target, 'utf8');
      const updated = reflowMarkdown(original);
      const changed = updated !== original;
      if (parsed.flags.write) {
        fs.writeFileSync(target, updated, 'utf8');
        if (parsed.flags.json) {
          console.log(JSON.stringify({ target, changed, written: true }, null, 2));
        } else {
          console.log(`Reflowed: ${target}`);
        }
      } else if (parsed.flags.json) {
        console.log(JSON.stringify({ target, changed, written: false, content: updated }, null, 2));
      } else {
        process.stdout.write(updated);
      }
      return;
    }

    console.error(`Unknown subcommand: ${subcmd}`);
    process.exit(2);
  } catch (err) {
    console.error(err.message);
    process.exit(2);
  }
}

if (isCliMain(import.meta.url)) {
  main();
}
