# Final independent revalidation — Chat / connection P1

Task `task_e5532a99dfdf`, Dispatch `ctx_ce743f6b36c2`. Target HEAD `e0a8a4e53a3c0265c4fb8345b54622a8e3bcd370` plus the current shared uncommitted corrections, observed 2026-10-20 KST. This supersedes the open-P1 status in `review.md` for the three scenarios below, within the stated source/Node scope.

**Conclusion: all three original P1 counterexamples pass after correction. The additional historical-view/live-poll race also passes after coordinator correction. No remaining blocker was reproduced in the final tested Node scope.** Rust reader/parser tests were reviewed, not executed by this worker; coordinator owns their execution and release/integration validation.

## Independently executed scenarios

`node /tmp/krill-mobile-fix-20261020/review-repro.cjs` exits **0**, all **six groups pass**. Durable output: `/tmp/krill-mobile-fix-20261020/review-final-repro.log`.

| Group | Independent acceptance / final result |
|---|---|
| `remount` | Pending submission holds shared lock across unmount/remount; unedited successful completion clears current input and shared draft, and another Submit produces no duplicate RPC. Different new draft survives; edit away and back to the same text survives; failed send retains the draft. All four cases pass. |
| `history` | Exact original 300 one-record append pattern, 50 consecutive earlier reads, expected entire 250-row chronological window checked at every step. Every earlier request sends the original end; latest restores Message 50 through Message 299. Pass. |
| `byteHistory` | 360 variable-length UTF-8 records, observed batches cycling 7/1/13/4/2 records, page-local request-start IDs. Exercises clipped-page replay and all prior pages to the beginning. Full IDs compared against original observed partitions; two distinct records with identical text remain separate; view stays at 250 rows. Pass. |
| `historyRace` | Earlier page response held, screen update attempted during its wait, historical view established, delayed live response then released. Final view remains Message 49 through Message 298 rather than previously drifting to 59 through 309. Pass after coordinator guard patch. |
| `locate` | Delayed lookup false/true × current replacement connecting/ready, four combinations. Old completion emits no new status/fallback and closes no replacement; another Reconnect can still create a new attempt. Pass. |
| `discoverySideEffects` | Real bootstrap endpoint; delayed fetch/JSON × reconnect/setPair/close, six combinations. Aborted fetch is deliberately permitted to return success late. Old completion changes neither endpoint routing, saved credential, endpoint credential nor navigation, emits no current status, and closes no current socket. Pass. |

The Node harness uses the actual current product modules and existing fixture definitions. The independent script never changes source, starts a daemon or creates a browser session.

## Why the updated fake history server is valid

The original numeric-offset counterexample now honors the additive contract exactly:

```js
const start = request.offset;
const end = Math.min(total, start + 128, request.end_offset ?? Infinity);
```

It still assigns IDs from `start:index`; it does **not** replace them with globally stable synthetic IDs, drop duplicate text, or suppress the counterexample's incremental observations. The byte-offset group converts actual variable UTF-8 record boundaries to record indices, applies the same range cap, and returns the end byte offset.

Source correspondence was checked in `agent_history/chat.rs:83-111` and `:158-181`: replay uses the Registry-resolved transcript, validates a positive range, and the JSONL reader wraps the underlying file with `file.take(end_offset-start_offset)` before buffering. Thus it cannot read beyond a recorded boundary, even through read-ahead or oversized-line scanning. Normal 128-item/byte caps remain. A partial record is not consumed. OpenCode JSON synthetic offsets take a separate path and changed synthetic pages reject replay.

`pwa_rpc.rs:339-352` decodes optional `end_offset`, resolves the existing snapshot, invokes bounded replay and then applies `chat_page_value`; the requested end reaches the actual reader. `chat.js:219-230` sends the original cached `{start,next}`, rejects a changed end/generation before applying records, and deduplicates by page-local ID, preserving distinct identical text. This is a real additive server contract, not a weakened mock.

## Rust evidence reviewed; execution belongs to main

- `mobile_chat_small_append_pages_replay_with_original_display_ids`: appends/observes 300 actual JSONL records one at a time, replays each original byte range after all appends, compares full mobile JSON/IDs, proves two identical texts have different IDs, and checks optional boundary deserialization.
- `mobile_replay_stops_at_observed_end_even_inside_a_record`: identical adjacent Claude records, exact replay equality, partial-record boundary remains unconsumed, nonpositive ranges rejected.
- `bounded_history_pages_can_be_replayed_and_generation_survives_append`: normal bounded pages and unchanged generation on append.
- `codex_agy_and_opencode_normalize_into_unified_chat_items`: existing dual-representation compatibility, plus coordinator-added repeated identical Codex representation-pair test expecting two equal-text items.

During this dispatch the coordinator reported that the first bounded-reader Rust regression exposed old global text-based `push_item` deduplication. Coordinator removed global text-only deduplication and retained Codex dual-wrapper handling locally. Independent source review then found a chained-merge edge: `response_item(A),event_msg(A),response_item(A),event_msg(A)` collapsed two representation pairs into one item because every subsequent wrapper was opposite to the previous one. It was immediately escalated with durable raw records in `/tmp/krill-mobile-fix-20261020/review-codex-identical-pairs.jsonl`.

The final source consumes a matched wrapper pair by setting `previous_codex_record=None`, and the normalization test now repeats the original two wrapper records twice and asserts two items with equal text. Final source/test inspection confirms this correction. **This worker did not execute Rust, so no independent Rust pass is claimed.** This preserves the distinction between directly executed Node evidence and reviewed Rust assertions.

## Additional historical-view race found and closed

Before the final guard patch, `pollNow` could begin while `loadEarlier` was pending. Resolving earlier first made history 49..298, then resolving live offset 300 with messages 300..309 changed it to 59..309 and omitted 299. This was independently executed and immediately reported; coordinator added `earlierPending` before polling and `earlierPending || entry.live` after response. `loadEarlier` already rejects an in-flight poll. The preserved independent `historyRace` fixture now passes.

## Node suites executed after the P1 patches

All exited 0:

- `node crates/krilld/tests/mobile_chat.cjs` — original conversation/photo/error/DOM cases plus unchanged-remount, equal-text edit-back and short-page replay.
- `node crates/krilld/tests/mobile_screens.cjs`.
- `node crates/krilld/tests/mobile_terminal.cjs`.
- `node crates/krilld/tests/mobile_session.cjs` — current VM fixture includes AbortController required by final transport.
- `node --test crates/krilld/tests/mobile_transport.cjs cloud/mobile/test/sw.test.js` — **25 passed, 0 failed** (21 transport + 4 service-worker tests).
- `node /tmp/krill-mobile-fix-20261020/review-repro.cjs` — six independent groups pass, final output preserved.

No source file was written by this dispatch. Artifact-only output: updated independent reproduction script/log, Codex raw counterexample and this report. No Cargo/build/gzip/browser, harness, operational session, commit/push/deployment or paid action was performed.

## Final inspected source identity

The HEAD alone does not identify the dirty corrections; SHA-256 below records the inspected file contents. Later source changes need appropriately scoped reruns.

| File | SHA-256 |
|---|---|
| `crates/krilld/src/mobile/assets/screens/chat.js` | `33089a9fddc17e156bc96663bb6fe44b6de0b8a63b4e842c70a96d2c60f6a86d` |
| `crates/krilld/src/mobile/assets/client.js` | `02c182ec90b1d520df1180571ddc9d6c62df512c85663a308c8ad98b56ca1687` |
| `crates/krilld/src/mobile/pwa_rpc.rs` | `0101c8f140f288a405f68cc6ad230c7a1e3bb13dcc73d93202a2a167a795fbfe` |
| `crates/krilld/src/agent_history/chat.rs` | `1e16fc9fe2caed9c72a6ab5dd23a8146b1fce67cca1cbd1e8630d43cf7e62302` |

Remaining scope boundaries: real browser/history/focus/IME, mobile installation, live transport/PTY, memory measurements, Cargo validation, gzip synchronization and release checks remain coordinator-owned. The earlier review's policy/device candidates are not promoted to confirmed bugs, and the Node pass is not reported as a full release pass.
