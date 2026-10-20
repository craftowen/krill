# Mobile Chat independent-review P1 fixes — 2026-10-20

Task `task_a40db2f669f0`, dispatch `ctx_f8eac9e15cf2`; target HEAD `e0a8a4e53a3c0265c4fb8345b54622a8e3bcd370` plus the existing shared working tree. This dispatch changed only `crates/krilld/src/mobile/assets/screens/chat.js`, `crates/krilld/src/mobile/pwa_rpc.rs`, `crates/krilld/src/agent_history/chat.rs`, and `crates/krilld/tests/mobile_chat.cjs`. Previous changes in these files were preserved; no other worker's source, gzip, harness, Cargo setup, commit, push, deployment or operating session was changed.

## P1 #1 — pending prompt completion after composer remount

- `chat.js:64` accepts `{version,text}` in the shared current-composer notifier. After a successful RPC deletes the exact matching shared draft, it sends that completion to the currently mounted composer (`chat.js:97`), which clears its textarea only when both version and text still match.
- Thus an unedited remount no longer leaves the successfully sent prompt available for duplicate submission. A newer edit survives even if the user changes the text and then returns to exactly the sent text. The existing shared submit lock and failed-send draft/photo retention remain.
- Regression cases cover delayed completion before an edit, remount with a later different edit, remount without edits followed by another submit (one prompt RPC total), and remount with newer edits that end in identical text.

## P1 #2 — observed short pages replaying as larger overlapping pages

- Chosen minimal contract: replay the original observed **byte range**, rather than replacing the wire DTO or attempting text-based deduplication. JSON `chat_poll` has an optional display-only `end_offset` (`pwa_rpc.rs:105`); normal polling remains unchanged.
- `loadEarlier` selects the cached `{start,next}` page and sends both boundaries (`chat.js:219`). It rejects reset/generation changes or a returned end different from the observed end before changing visible items. Page-local `start:index` IDs therefore retain the same partition on replay and are never used as approval authority.
- The server resolves the transcript through the existing Registry, then `chat::replay_page` (`chat.rs:83`) replays from that resolved file with the requested end. The underlying file reader is wrapped in `Read::take` (`chat.rs:161`), limiting read-ahead and oversized-line scanning as well as normal records. Existing record-count and read-byte limits remain.
- One-item pages are no longer marked clipped merely because an older page was removed from the 250-item view (`chat.js:183`). Loading earlier immediately prepends actual prior content. Pages without rendered items are not added as navigation stops.
- Deduplication continues to compare item IDs only; identical-text messages at different byte positions remain separate. Existing 250-item current/history views, live pause/latest restore, and retained-card scroll anchoring remain.
- OpenCode JSON files use synthetic offsets, so the replay helper never interprets those as byte boundaries; changed synthetic pages return an error. The existing OpenCode reset-on-change and parser limits remain.

## Verification executed by this dispatch

- `node --check crates/krilld/src/mobile/assets/screens/chat.js` — exit 0.
- `node --check crates/krilld/tests/mobile_chat.cjs` — exit 0.
- `node crates/krilld/tests/mobile_chat.cjs` — exit 0, eight PASS groups. Existing normal conversation, historical-card notes/DOM state, empty reset, polling errors/retry, photo preparation/send/failure, and current diff behavior still pass. Expanded history coverage includes 600 messages through all earlier pages; 300 single-record appends at actual simulated byte offsets; 50 consecutive earlier reads with exact ID/order assertions; two identical-text messages; the 250-item cap; current-card anchoring; history errors; and latest restore.
- `rustfmt --edition 2024 --check crates/krilld/src/mobile/pwa_rpc.rs crates/krilld/src/agent_history/chat.rs` — exit 0.
- Scoped tracked-file `git diff --check` — exit 0; no whitespace diagnostics for the three untracked owned files when checked with `git diff --no-index --check /dev/null <file>` (exit 1 indicates their added content).

## Rust checks owned by coordinator

No Cargo command was run by this dispatch, as explicitly instructed. The coordinator was notified that the final code was ready and requested to run the existing `mobile::pwa_rpc::tests` and `agent_history::chat::tests` filters plus clippy, including:

- `mobile::pwa_rpc::tests::mobile_chat_small_append_pages_replay_with_original_display_ids`: append and observe 300 individual real JSONL records, replay every original page after all appends, compare full mobile JSON page/ID, preserve two identical-text records, and decode the additive JSON boundary.
- `agent_history::chat::tests::mobile_replay_stops_at_observed_end_even_inside_a_record`: replay only the observed record after another identical record is appended, do not cross a partial-record byte boundary, and reject nonpositive ranges.

Coordinator owns Cargo results, gzip regeneration and integrated browser/device verification; this report does not claim those checks passed. The fix preserves the existing generation contract and does not add protection against arbitrary same-file in-place transcript rewriting.
