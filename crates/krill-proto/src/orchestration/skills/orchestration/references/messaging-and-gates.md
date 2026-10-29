# Messaging and decision gates

Krill messages are addressed to worker names, not Orca terminal or Delivery IDs.
Use distinct names across active Runs. For lifecycle messages always include the
assigned Run/Task/Dispatch and send to the coordinator, never to a group.

A successful `send` records mail; it does not prove the recipient read it or
accepted new work. An idle-session notification is only a pointer to that mail.
For a follow-up, inspect `worker-show <dispatch-id> --json`, then send to its
unique worker name with the owning `--run`. Do not reuse worker names for later
attempts. `--dispatch` is message metadata, not a `dispatch:<id>` mailbox address;
Krill does not implement Orca's Dispatch-address routing.

`inbox --to <name> --run <run-id> --json` inspects pending mail with a Run filter.
`check --to <name> --run <run-id> --unread --wait --timeout-ms 60000 --json` waits
without acknowledging (plain `--wait` defaults to 60s, max 300s).
JSON checks return a `delivery` receipt and `messages` array. Match
each returned message to the owned Run and authoritative dispatch before deciding
what to do. Group addresses such as `@all`, `@active`, and `@agy` are scoped by
the supplied Run and exclude failed, abandoned, or released workers. Always supply `--run` for a group
message; without one, matching workers across Runs can be selected. Prefer explicit recipients.

Inspect with `check --unread --wait` first and acknowledge the returned immutable
batch with `check --to <name> --run <run-id> --ack <delivery-id> --json` after handling
it. Mail arriving later stays unread. You can also acknowledge specific handled message IDs with
`check --to <name> --run <run-id> --ack-ids <id1,id2,...> --json` (`--ack-ids`
cannot be combined with `--wait`). A bare `check --ack` acknowledges matching mail
at read time and returns an array. If using bare `--ack`, preserve
its full returned receipt and handle every row it returns, including newly arrived
messages. Avoid bulk `--ack` without `--run` in a shared coordinator inbox when
another active Run owns matching mail. On restart reconcile the receipt and task
state rather than assuming acknowledged means successfully processed.

Workers use the same read-process-ack discipline. `inbox` is useful for inspection;
`check --to <worker> --run <run-id> --unread --json` gives a batch receipt for
processing. Handle its messages, acknowledge that exact `delivery.id`, then read
again until no unread messages remain. Krill's named ACK returns the acknowledged
batch, not the next batch. Do not keep replaying it as new guidance.
Avoid type filters in the normal supervision loop so questions and redirects are
not missed; a filtered Krill check selects messages, not merely wake-up conditions.

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
