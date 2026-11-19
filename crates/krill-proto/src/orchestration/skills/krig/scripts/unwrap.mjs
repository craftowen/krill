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
    reason: '특정 에이전트 호스트 이름에 결합되어 있습니다(호스트 어댑터 전용 섹션이면 --allow-host-specific 사용).'
  },
  {
    hostSpecific: false,
    test: (s) => /(사용자\s*지적|담당자\s*요청|방금\s*세션|이번\s*세션|어제\s*실수|\blast\s+session\b|\bthis\s+session\b|\byesterday'?s\s+bug\b)/i.test(s),
    reason: '일회성 세션 시점이나 개별 대화 맥락 표현이 남아 있습니다.'
  },
  {
    hostSpecific: false,
    test: (s) => hasTicketId(s) || /\bPR\s*#\d+\b/i.test(s),
    reason: '개별 티켓/PR 번호가 규칙 본문에 박혀 있습니다. 일반화된 불변 조건으로 바꾸세요.'
  }
];

const PATCH_MARKERS = [
  {
    pattern: /(추가\s*:|예외\s*:|업데이트\s*:|주의\s*:|\bUpdate\s*:|\bNote\s*:|\b202\d-\d{2}-\d{2}\b)/,
    reason: '패치 누적 흔적(날짜/추가/예외 꼬리표)이 있습니다. 원래부터 존재했던 v0 문단으로 통합 재작성하세요.'
  }
];

export function validateBackpropRule({ level, text, targetPath, allowHostSpecific = false } = {}) {
  const issues = [];
  if (!PYRAMID_LEVELS.includes(level)) {
    issues.push(`유효하지 않은 강제력 계층(${level})입니다. 허용값: ${PYRAMID_LEVELS.join(', ')}`);
    return { ok: false, rank: -1, canonicalTarget: null, issues };
  }

  const rank = PYRAMID_LEVELS.indexOf(level) + 1;
  const trimmed = String(text ?? '').trim();
  if (!trimmed) {
    issues.push('역전파할 규칙 또는 구조 변경 설명(text)이 비어 있습니다.');
  }

  let canonicalTarget = null;
  if (targetPath) {
    const absTarget = path.resolve(targetPath);
    if (!fs.existsSync(absTarget)) {
      issues.push(`정본 대상 파일이 존재하지 않습니다: ${targetPath}`);
    } else {
      const realTarget = realpathSync(absTarget);
      canonicalTarget = realTarget;
      if (fs.lstatSync(absTarget).isSymbolicLink()) {
        issues.push(`대상(${targetPath})은 심볼릭 링크 사본입니다. 단일 정본(${realTarget})을 직접 수정하세요.`);
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
  console.log('사용법:\n  node unwrap.mjs check-rule --level architecture|type|gate|test|doc --text "<규칙>" [--target <파일>] [--allow-host-specific] [--json]\n  node unwrap.mjs reflow <markdown_file> [--write]');
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
        console.log(`OK: 강제력 ${res.rank}순위(${res.level}) 검증 통과`);
      } else {
        for (const issue of res.issues) console.error(`위반: ${issue}`);
      }
      process.exit(res.ok ? 0 : 1);
    }

    if (subcmd === 'reflow') {
      const target = parsed.positional[1];
      if (!target) {
        console.error('마크다운 파일 경로가 필요합니다.');
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
          console.log(`정리 완료: ${target}`);
        }
      } else if (parsed.flags.json) {
        console.log(JSON.stringify({ target, changed, written: false, content: updated }, null, 2));
      } else {
        process.stdout.write(updated);
      }
      return;
    }

    console.error(`알 수 없는 하위 명령: ${subcmd}`);
    process.exit(2);
  } catch (err) {
    console.error(err.message);
    process.exit(2);
  }
}

if (isCliMain(import.meta.url)) {
  main();
}
