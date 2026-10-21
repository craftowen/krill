# Messaging and decision gates

Krill messages are addressed to worker names, not Orca terminal or Delivery IDs.
Use distinct names across active Runs. For lifecycle messages always include the
assigned Run/Task/Dispatch and send to the coordinator, never to a group.

`inbox --to <name> --run <run-id> --json` inspects pending mail with a Run filter.
`check --to <name> --unread --wait --timeout-ms 10000 --json` waits without
acknowledging. It has no Run filter. Match each returned message to the owned Run
and authoritative dispatch before deciding what to do. Group addresses such as
`@all`, `@active`, and `@agy` currently span matching dispatches in the store;
they are not safe Run-scoped broadcasts. Prefer explicit recipients.

`check --ack` marks matching mail acknowledged when it is returned, not after
your reasoning. It is not Orca's replayable Delivery acknowledgment and accepts no
Delivery ID. If acknowledging a batch, preserve its full returned receipt and
handle every row it returns, including newly arrived messages. Do not assume it
acknowledged only an earlier inspection. Avoid bulk acknowledgment in a shared
coordinator inbox when another active Run owns matching mail; leave it unacked
and coordinate ownership instead. On restart reconcile the receipt and task
state rather than assuming acknowledged means successfully processed.

Worker questions use `ask`; timeout leaves a durable pending question. Re-wait
with its message ID. Answer with `reply <message-id> --body "<answer>" --json`.
Do not create a second gate merely to answer the same question.

Use a decision gate for an actual user/coordinator decision:
`gate-create --from <worker> --run <run-id> --task <task-id> --question "<decision>"
--options "approve,reject" --json`. Resolve with
`gate-resolve <gate-id> --decision approve --rationale "<reason>" --json`.
Use `gate-list --pending --json` and inspect ownership. Gate resolution must come
from the authorized decision maker; time passing is not approval. A gate object
does not by itself stop an external CLI from executing commands.
