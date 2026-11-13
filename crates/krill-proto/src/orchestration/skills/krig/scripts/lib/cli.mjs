import { realpathSync } from 'node:fs';
import { fileURLToPath } from 'node:url';

export function isCliMain(metaUrl, argv1 = process.argv[1]) {
  if (!argv1) return false;
  try {
    return realpathSync(argv1) === realpathSync(fileURLToPath(metaUrl));
  } catch {
    return false;
  }
}

export function parseCliFlags(argv, options = {}) {
  const booleans = new Set(options.booleans ?? ['json', 'help', 'list', 'write', 'stdin', 'allow-host-specific']);
  const allowed = options.allowedFlags ? new Set([...options.allowedFlags, 'help']) : null;
  const allowSingleDashPositional = Boolean(options.allowSingleDashPositional);
  const flags = {};
  const positional = [];
  let passthrough = false;

  for (let i = 0; i < argv.length; i += 1) {
    const arg = argv[i];
    if (passthrough) {
      positional.push(arg);
      continue;
    }
    if (arg === '--') {
      passthrough = true;
      continue;
    }
    if (arg === '-h' || arg === '--help') {
      flags.help = true;
      continue;
    }
    if (arg.startsWith('--')) {
      const eqIdx = arg.indexOf('=');
      if (eqIdx !== -1) {
        const key = arg.slice(2, eqIdx);
        if (allowed && !allowed.has(key)) {
          throw new Error(`알 수 없는 옵션입니다: --${key}`);
        }
        const val = arg.slice(eqIdx + 1);
        flags[key] = booleans.has(key) ? val !== 'false' : val;
        continue;
      }
      const key = arg.slice(2);
      if (allowed && !allowed.has(key)) {
        throw new Error(`알 수 없는 옵션입니다: --${key}`);
      }
      if (booleans.has(key)) {
        flags[key] = true;
        continue;
      }
      const next = argv[i + 1];
      const nextLooksLikeKnownFlag = typeof next === 'string' && (
        next === '--'
        || next === '-h'
        || next === '--help'
        || (
          next.startsWith('--')
          && allowed
          && allowed.has(next.includes('=') ? next.slice(2, next.indexOf('=')) : next.slice(2))
        )
      );
      if (next === undefined || nextLooksLikeKnownFlag) {
        throw new Error(`옵션 --${key} 에 값이 필요합니다.`);
      }
      flags[key] = next;
      i += 1;
      continue;
    }
    if (arg.startsWith('-') && arg !== '-' && !allowSingleDashPositional) {
      throw new Error(`알 수 없는 옵션입니다: ${arg}`);
    }
    positional.push(arg);
  }

  return { flags, positional };
}
