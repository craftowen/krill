#!/usr/bin/env node
import fs from 'node:fs';
import path from 'node:path';
import { isCliMain, parseCliFlags } from './lib/cli.mjs';

const VALID_KINDS = new Set(['deleted', 'kept', 'restored']);

export function readLedgerEvents(ledgerPath) {
  if (!fs.existsSync(ledgerPath)) return [];
  const raw = fs.readFileSync(ledgerPath, 'utf8').trim();
  if (!raw) return [];
  return raw.split(/\r?\n/).map((line, index) => {
    try {
      return JSON.parse(line);
    } catch {
      throw new Error(`원장 JSON 파싱 실패 (${ledgerPath}:${index + 1})`);
    }
  });
}

function buildItemStates(events) {
  const byId = new Map();
  for (const ev of events) {
    if (!ev || typeof ev !== 'object') continue;
    const id = String(ev.id ?? '').trim();
    const kind = String(ev.kind ?? '').trim();
    if (!id || !VALID_KINDS.has(kind)) continue;

    const prev = byId.get(id) ?? { status: null, wasDeleted: false, title: '', lastReason: '' };
    if (kind === 'deleted') {
      prev.status = 'deleted';
      prev.wasDeleted = true;
    } else if (kind === 'restored') {
      if (!prev.wasDeleted) continue;
      prev.status = 'restored';
    } else if (kind === 'kept') {
      prev.status = 'kept';
      prev.wasDeleted = false;
    }
    const title = String(ev.title ?? '').trim();
    if (title) prev.title = title;
    prev.lastReason = String(ev.reason ?? '').trim();
    byId.set(id, prev);
  }
  return byId;
}

export function summarizeLedger(events) {
  const byId = buildItemStates(events);

  let deletedCount = 0;
  let keptCount = 0;
  let restoredCount = 0;
  const activeDeletedIds = [];

  for (const [id, state] of byId.entries()) {
    if (state.status === 'deleted') {
      deletedCount += 1;
      activeDeletedIds.push(id);
    } else if (state.status === 'restored') {
      deletedCount += 1;
      restoredCount += 1;
    } else if (state.status === 'kept') {
      keptCount += 1;
    }
  }

  const addBackRate = deletedCount === 0 ? 0 : Number((restoredCount / deletedCount).toFixed(4));
  const underDeletionWarning = (deletedCount === 0 && keptCount > 0)
    || (deletedCount >= 5 && addBackRate < 0.1);

  return {
    totalItems: deletedCount + keptCount,
    deletedCount,
    keptCount,
    restoredCount,
    activeDeletedIds,
    addBackRate,
    underDeletionWarning
  };
}

export function recordLedgerEvent(ledgerPath, entry) {
  const kind = String(entry?.kind ?? '').trim();
  const id = String(entry?.id ?? '').trim();
  const title = String(entry?.title ?? '').trim();
  const source = String(entry?.source ?? 'unspecified').trim() || 'unspecified';
  const reason = String(entry?.reason ?? '').trim();

  if (!VALID_KINDS.has(kind)) {
    throw new Error(`유효하지 않은 kind(${kind})입니다. 허용값: deleted, kept, restored`);
  }
  if (!id || !reason) {
    throw new Error('공백이 아닌 id와 reason은 필수입니다.');
  }

  const events = readLedgerEvents(ledgerPath);
  const states = buildItemStates(events);
  const current = states.get(id);

  if (kind === 'restored' && (!current || !current.wasDeleted)) {
    throw new Error(`삭제된 이력이 없는 항목(${id})은 restored로 복구할 수 없습니다.`);
  }

  const lastForId = [...events].reverse().find((e) => String(e?.id ?? '').trim() === id);
  if (
    lastForId
    && String(lastForId.kind ?? '').trim() === kind
    && String(lastForId.reason ?? '').trim() === reason
  ) {
    return { appended: false, summary: summarizeLedger(events) };
  }

  const record = {
    ts: entry?.ts ? String(entry.ts).trim() : new Date().toISOString(),
    kind,
    id,
    title,
    source,
    reason
  };

  fs.mkdirSync(path.dirname(path.resolve(ledgerPath)), { recursive: true });
  fs.appendFileSync(ledgerPath, `${JSON.stringify(record)}\n`, 'utf8');
  return { appended: true, summary: summarizeLedger([...events, record]) };
}

function printHelp() {
  console.log('사용법:\n  node ledger.mjs record <ledger.jsonl> --kind deleted|kept|restored --id <id> --title "<제목>" --source "<출처>" --reason "<사유>" [--json]\n  node ledger.mjs summary <ledger.jsonl> [--json]');
}

function main() {
  let parsed;
  try {
    parsed = parseCliFlags(process.argv.slice(2), {
      booleans: ['json', 'help'],
      allowedFlags: ['json', 'kind', 'id', 'title', 'source', 'reason', 'ts']
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

  const ledgerPath = parsed.positional[1];
  if (!ledgerPath) {
    console.error('원장 파일 경로(<ledger.jsonl>)가 필요합니다.');
    process.exit(2);
  }

  try {
    if (subcmd === 'record') {
      const result = recordLedgerEvent(ledgerPath, parsed.flags);
      if (parsed.flags.json) {
        console.log(JSON.stringify(result, null, 2));
      } else {
        console.log(`기록 완료(appended=${result.appended}): 삭제=${result.summary.deletedCount}, 복구=${result.summary.restoredCount}, 복구율=${result.summary.addBackRate}`);
      }
      return;
    }

    if (subcmd === 'summary') {
      const summary = summarizeLedger(readLedgerEvents(ledgerPath));
      if (parsed.flags.json) {
        console.log(JSON.stringify(summary, null, 2));
      } else {
        console.log(`총 항목=${summary.totalItems}, 삭제=${summary.deletedCount}, 유지=${summary.keptCount}, 복구=${summary.restoredCount}, 복구율=${summary.addBackRate}, 과소삭제경고=${summary.underDeletionWarning}`);
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
