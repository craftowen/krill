---
name: computer-use
description: Drive a visible local macOS app through krill computer using immutable accessibility snapshots, guarded actions, and explicit delivery and verification results. Prefer shell, filesystem, git, HTTP, or an existing CLI when they can complete the task.
---

# Krill computer use

This guide describes `computer/2`. Read `krill computer capabilities --json` for the bundled guide hash, backend, supported actions, and service features. A different installed guide hash means the instructions may be stale; read `krill skills get computer-use` from the same binary.

Use the current JXA backend for visible macOS app windows. Native execution is experimental and must not be inferred from a capability name. GUI execution is separate from terminal session survival.

## Observe, act, inspect

1. Check `krill computer capabilities --json` and `krill computer permissions --json`. To open the relevant Settings page, explicitly use `permissions --id accessibility` or `permissions --id screenshots`.
2. Discover the exact process/window with `krill computer list-apps --json` and `krill computer list-windows --app pid:123 --json`. Use a PID when names are ambiguous.
3. Observe with `krill computer get-app-state --app pid:123 --window-id 456 --json`.
4. Select a reference from that result and bind the action explicitly: `krill computer click --snapshot <snapshotId> --element 2 --json`.
5. Read all result axes and the returned observation before choosing another action. A fresh observation supplies its own snapshot ID. Never reuse a reference number from a different snapshot.

JSON is compact by default and screenshots are **off by default**. Use `--screenshot` only when visual evidence is needed. `--no-screenshot` remains accepted. Use `--format full` to inspect the full bounded tree. Coverage describes observed nodes and truncation; an unobserved element is not proof that it disappeared.

Completed screenshot files have separate bounded retention: at most 32 images and 64 MiB per capture directory. When another capture completes, older captures beyond 300 seconds and files exceeding those count/byte limits are removed; a single oversized capture is refused. A previously returned image path may therefore expire independently of snapshot history. Capture fresh evidence when the file is gone or the scene has changed; an old screenshot never refreshes a target reference.

For additional elements already captured in the same immutable observation, use `krill computer get-app-state --snapshot <snapshotId> --offset 40 --limit 40 --json`. Follow `coverage.nextOffset`. Paging never refreshes the desktop or expands an unobserved subtree. Reference actions are refused when the fresh bounded census is incomplete: a partial tree cannot prove target uniqueness. Paging the saved prefix does not repair that missing coverage. Use a fresh observation or a different supported tool.

Snapshots expire for input after 120 seconds. They are private to the actual calling process context; `--session`, `--worktree`, and environment claims do not grant access to another caller. The parent-process cache namespace is a convenience boundary within one OS user, not authenticated session authorization. Separate invocations must share a long-lived parent process to reuse this namespace; independent shell wrappers may not share it. Use one persistent caller or the experimental MCP session below. Missing, stale, reordered, ambiguous, disabled, or replaced targets are refused. Observe again and reassess; do not substitute a new index for an old target automatically.

## Actions

All actions accept `--snapshot <snapshotId>`, `--json`, and optional `--screenshot`. The default numeric reference resolves only against the caller's last returned snapshot. An `--app` or window selector must agree with that original observation.

- `krill computer click --element 2` or `krill computer click --x 100 --y 200`; optional `--button left|right|middle`, `--click-count 1..16`, and `--modifiers cmd,shift`.
- `krill computer perform-secondary-action --element 2 --action AXShowMenu`; the requested action must be advertised by the actual AX element.
- `krill computer set-value --element 1 --value-stdin` (or `--value <text>`). Verification compares the full value on the same resolved object, not its preview.
- `krill computer type-text --element 1 --text-stdin` (or `--text <text>`). Requires an already focused editable receiver; newline/tab sequences are refused. Prefer set-value or paste-text for such text.
- `krill computer paste-text --element 1 --text-stdin` (or `--text <text>`). Requires a focused editable receiver, an unambiguous selected-text range, and bounded preservation of all readable clipboard item types. Consumption is confirmed only when the full receiver value equals the expected edit.
- `krill computer press-key --key return --modifiers cmd --repeat 1`; repeat is bounded to 32.
- `krill computer hotkey --keys cmd+shift+p`.
- `krill computer scroll --element 3 --direction down --pages 1`, or a point and `--delta-x` / `--delta-y`.
- `krill computer drag --from-element 2 --to-element 3`, or explicit `--from-x`, `--from-y`, `--to-x`, `--to-y`.

The target app and window must already be foreground. Text input requires its observed receiver to remain focused. Unexpected foreground/window/receiver changes stop the action; do not force focus back after user intervention. `get-app-state --restore-window` explicitly allows guarded unhide/unminimization and foreground activation with a checked postcondition; it does not authorize targeting Krill itself. Coordinates use the observed desktop logical points, not screenshot pixels. A screenshot carries measured dimensions and capture bounds; changed geometry or occlusion requires a new observation.

## Read results honestly

`delivery` reports `not_dispatched`, `submitted`, `partial`, or `unknown`. Submitted means input was issued, not that the application completed the task. `observation` independently reports the post-action snapshot or its failure. `verification.status=verified` only means the specific reported predicate was observed (currently full set-value equality or confirmed paste receiver contents). Clicks, keys, scrolls, and drags are generally unverified even when successfully submitted. `cleanup` reports `not_needed`, `completed`, or `incomplete`; read clipboard `preserved` and `restored` separately.

Nonzero exit status can accompany a JSON result: retain that evidence. Post-action observation failure does not erase submitted input. An error or lost response after dispatch is unknown; **never replay it automatically**, switch backends, or fall back from an AX mutation to coordinates. Observe the target before deciding on any new action. A returned snapshot is an observation, not proof of business completion or causality.

Clipboard preservation is bounded to 4 MiB. Unsupported promised data or oversize contents cause refusal before writing. If another process changes the clipboard, its contents are retained and restoration is reported incomplete. If paste consumption cannot be confirmed, the temporary clipboard may remain for a delayed receiver; do not claim restoration or force previous contents over a user's copy. Previous clipboard text is never included in results. Use stdin flags for sensitive text; live payloads travel in private operation files rather than child argv.

The input lease serializes cooperating Krill executors through the entire command, including restore and clipboard cleanup. It does not exclude the user or unrelated automation tools. Timeouts do not prove that an already submitted OS action was cancelled. Short batches, request deduplication, status, and cancel are not supported unless the running binary explicitly advertises them; do not invent a service protocol or LLM API.

## Experimental persistent native MCP

`krill computer serve --experimental [--worker /absolute/path/to/krill-app]` explicitly selects an isolated native worker for one stdio MCP session. The default CLI commands above still use JXA. This experimental path has not passed every native permission, hang isolation, update, footprint, and end-to-end gate. Do not infer permission inheritance from the worker being the app executable.

The proxy supports **MCP 2025-11-25 only**, using `initialize`, then `notifications/initialized`. It reports that exact version, accepts `ping`, and returns `-32601` to `server/discover` so clients that support the documented legacy fallback can initialize. This is not a claim of support for the newer discovery/per-request metadata protocol. Messages are UTF-8 JSON-RPC lines on stdin/stdout; diagnostics go to stderr. The command neither registers an MCP server nor changes configuration, accounts, providers, or remote services.

The worker is discovered beside the current `krill` executable, including a sibling `Krill.app/Contents/MacOS/krill-app`; use the absolute `--worker` override for a development or test binary. A missing, unsupported, or incompatible worker fails before desktop dispatch. The only worker transport is inherited private stdin/stdout pipes. No socket, daemon, or desktop work runs in the CLI proxy.

After the handshake, discover `tools/list` and call:

- `computer_session {}` returns the actual native hello: contract, epoch, context, capabilities, unsupported operations, and limits. These capabilities apply to this worker, separately from the default JXA capabilities.
- `computer_execute {"seq":1,"deadlineMs":5000,"op":{"kind":"apps"}}` submits one operation. `seq` is required, positive, caller-owned, and monotonically increasing within the advertised gap. `deadlineMs` includes queueing and execution and cannot exceed native limits. The returned tool schema lists the supported native op shapes. For example, observe with `{"kind":"observe","pid":123,"window":0,"limit":100}` and mutate only with an observed native target such as `{"kind":"click","target":{"snapshot":"E/C/1","element":"e2"}}`. Native window indices and element references are zero-based (`window:0` and `e0` are valid); copy them exactly from native observations.
- `computer_status {"seq":1}` queries the original native request without issuing desktop input. `expired` or `unseen` never authorizes replay.
- `computer_cancel {"seq":1}` attempts cancellation before dispatch. MCP `notifications/cancelled` also forwards cancellation of a pending execute request. `too_late` cannot undo already posted input; inspect all result axes.

The proxy captures epoch/context from native hello; callers cannot replace them. A mutation target must contain a snapshot from that same native epoch/context. JXA snapshots and snapshots from another worker cannot be used. A restarted session always requires a fresh observation and a new assessment of the desktop.

When the worker advertises `batch` and `limits.maxBatchSteps`, `computer_execute` also accepts `{"kind":"batch","steps":[...]}` with 1..4 mutation steps, bounded by the advertised maximum. Steps may be `action`, `set_value`, `click`, or `type_text`; reads and nested batches are refused. The batch has one seq and one deadline. Inspect each step and aggregate `issuedSteps`/`issuedUnits` (`issuedSteps` is null when dispatch is unknown): a known submitted prefix followed by a refused step reports Partial and `isError` without implying that prior input was undone. Aggregate verification is satisfied only if every step satisfied its explicit predicate; unverified input leaves it not requested. A known Partial result with completed cleanup keeps the session usable; Unknown, incomplete cleanup, or quiescing closes it.

Retain the original `seq` and identical payload when checking or retrying a request within this live session. The proxy does not allocate sequences. Duplicate JSON-RPC request IDs with identical payloads and duplicate execute sequences do not issue multiple effects; a changed payload under a retained ID or seq is rejected. Always choose a new JSON-RPC ID for a new request. Completed request/payload/result entries roll out under count and byte limits, while pending entries are protected. The seq highwater survives eviction: an old seq returns `expired` without a native write and never becomes permission to replay. `computer_session.proxy` advertises these retention and pending/output bounds. A limit rejection before dispatch permits no claim of input delivery.

Native result data appears once in `structuredContent`, with a short text summary. Read delivery, observation, verification, and cleanup independently. `isError` includes refused, partial, unknown, failed observation, and unsatisfied/unknown verification results; an error never erases submitted input. If the MCP envelope would exceed the frame cap, observation data may be omitted with `output_too_large` while delivery evidence remains.

Native observations use one compact `treeText` by default, preserving native references, identity, and coverage. Set the execute argument `"format":"full"` to retain the native element array when its additional detail is needed; compact output does not also duplicate that array. Format is part of the retained execute payload, so retries of a seq must keep it identical. Format selects representation and does not refresh an earlier observation or grant new target identity.

After submission, worker EOF, a deadline without a result, or transport failure leaves the **original seq unknown**. Native Unknown/quiescing or incomplete cleanup also closes the MCP session: the proxy terminates and reaps its worker with bounded waiting and reports uncertainty when output is still available. It never respawns, replays, switches to JXA, or converts termination into proof that posted effects were cancelled. If the client stops reading, output may be incomplete. Observe the desktop afresh through a separately chosen tool before deciding whether any new action is appropriate. Operations not advertised by both the worker and this proxy fail closed.
