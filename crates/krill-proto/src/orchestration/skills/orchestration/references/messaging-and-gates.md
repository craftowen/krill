# Messaging and decision gates

Krill messages are addressed to worker names, not Orca terminal or Delivery IDs.
Use distinct names across active Runs. For lifecycle messages always include the
assigned Run/Task/Dispatch and send to the coordinator, never to a group.

`inbox --to <name> --run <run-id> --json` inspects pending mail with a Run filter.
`check --to <name> --run <run-id> --unread --wait --timeout-ms 60000 --json` waits
without acknowledging (plain `--wait` defaults to 60s, max 300s, with a short
arrival coalescing window so simultaneous completions arrive in one batch). Match
each returned message to the owned Run and authoritative dispatch before deciding
what to do. Group addresses such as `@all`, `@active`, and `@agy` currently span
matching dispatches in the store; they are not safe Run-scoped broadcasts. Prefer
explicit recipients.

`check --ack` marks matching mail acknowledged when it is returned, not after
your reasoning. Prefer inspecting with `check --unread --wait` first and then
acknowledging specific handled message IDs with
`check --to <name> --run <run-id> --ack-ids <id1,id2,...> --json` (`--ack-ids`
cannot be combined with `--wait`). If acknowledging a batch with `--ack`, preserve
its full returned receipt and handle every row it returns, including newly arrived
messages. Avoid bulk `--ack` without `--run` in a shared coordinator inbox when
another active Run owns matching mail. On restart reconcile the receipt and task
state rather than assuming acknowledged means successfully processed.

Worker questions use `ask`; timeout leaves a durable pending question. Re-wait
with its message ID. Answer with `reply <message-id> --body "<answer>" --json`.
Do not create a second gate merely to answer the same question.

Questions to `coordinator` or another worker stay in the internal inbox. When
human input is actually required, the coordinator may ask `--to user` (or
`--to mobile`) with the owning `--run` and `--task`. These are the explicit
human inbox addresses shown under **Confirmation requests** on mobile. Write a
short question in the user's language and put the situation and consequences in
the body; resolve worker coordination internally. Pending decision gates are
also shown there. Completed/archived Runs, completed Tasks and ended Dispatches
are excluded without deleting or acknowledging their stored history. Requests
without an owner cannot be retired with its lifecycle, so always attach one.

Use a decision gate for an actual user/coordinator decision:
`gate-create --from <worker> --run <run-id> --task <task-id> --question "<decision>"
--options "approve,reject" --json`. Resolve with
`gate-resolve <gate-id> --decision approve --rationale "<reason>" --json`.
Use `gate-list --pending --json` and inspect ownership. Gate resolution must come
from the authorized decision maker; time passing is not approval. A gate object
does not by itself stop an external CLI from executing commands.
