# Client async independent review

Review date: 2026-10-20. Reviewer: independent read-only worker (`task_b72e6fe75ec0`, dispatch `ctx_734a6399d2f9`). Base HEAD: `e0a8a4e53a3c0265c4fb8345b54622a8e3bcd370`; the reviewed working tree contains substantial concurrent, uncommitted changes. **Final source pass completed after coordinator integration-ready signal (`msg_fd941b3ad7ef`).** The coordinator subsequently authorized the F12 sidebar startup-intent fix and remote-refresh coalescing; those sidebar changes are self-review, while all other modules remain independently reviewed.

## Method and limits

Read AGENTS.md and the normative CONVENTIONS, ARCHITECTURE and STYLEGUIDE. Enumerated every library module from `src/lib.rs`, binary startup/shell modules and the five requested vendor files. Searched filesystem, process, IPC/network, lock, join, sleep and blocking-runtime calls, then traced callers to their actual execution boundary. An `async` block on the foreground executor is not a background boundary; `background_executor().spawn`, a dedicated owner thread, or an existing worker is. Synchronous work already running there is classified safe for the event thread, with separate reliability caveats when necessary. Test-only calls were distinguished from production even where a `#[cfg(test)]` module occurs before later production code.

This is source analysis and focused sidebar regression testing, not a measured reproduction of the user's multi-minute stall. Severity describes reachable behavior, not measured duration. No Zed terminal source was read. The authorized follow-up edits only sidebar-owned production/test files. Cargo artifacts for the focused test use `/Volumes/krill-build/targets/krill`. Final formatting/whitespace and focused sidebar tests are recorded below; broad integration build/test execution belongs to the coordinator. Tests named below are source-inspected unless explicitly listed in Validation as run by this worker.

## Whole-module coverage

Paths in this table are relative to `crates/krill-app/src/`. Each row includes the relevant child modules. “Background” refers to the traced production caller, not merely the helper's name.

| Module | Foreground entry and execution boundary inspected | Result / remaining concern |
|---|---|---|
| `accounts` | Settings refresh/file jobs call blocking request/local store on actual workers; installed detection uses `spawn_detect`; browser login and output capture use dedicated threads. UI outcome polling uses `try_lock`. | No active main-thread network/process/store path found in the normal production configuration; conditional settings local-store callers tracked below. |
| `add_project` | Dialog render/input builds values and emits events; shell/project registration owns filesystem work. | Memory-only dialog. |
| `agent_history` | Panel refresh/search spawns daemon request work and uses generation/open/busy guards; pending query coalesces. | Safe boundary; useful model for chat polling. |
| `alert` | AwakeWorker owns assertion acquisition/release and thread shutdown; notification probes execute in background. UI native badge/beep calls stay on the platform thread. | Single native UI calls require thread affinity; no worker join on event thread found. Startup respects loaded settings. |
| `appearance` | Shell invokes connect/sync through background work; live changes enqueue pane writer frames. | No blocking wire operation on UI found. |
| `commands` | Schema/matching/value projection is memory-only; shell captures external context on a worker. | Clipboard enqueue semantics covered below. |
| `continue_session` | Dialog selection and event emission only. | No external IO. |
| `crash` | `offer_report` loads pending reports, writes bundles and acknowledges on background executor; prompt/reveal is native UI. | No file walk/write on foreground completion. |
| `detached` | Panel refresh lists and prunes on workers; stored task fences replaced completions. Close action sends blocking request on worker. | Safe IO boundary. |
| `diff` | Panel polling/socket access, patch generation, Explorer disk operations and regex/rg/fallback searches execute in background. Foreground applies data and selection fences. | Identified pipe deadline/join holes migrated to bounded helper (F6). |
| `echo` | Input journal and prediction model are memory-only. | No external IO. |
| `editor` | Open/save/stamp helpers are synchronous but shell/file actions execute them in background. QuickOpen listing also runs on worker. | QuickOpen now has deadline, incremental output cap and one-scan admission (F7). |
| `file_format` | Shell `prepare_preview` parses notebook/CSV/SVG on background executor. GPUI image asset loading and markdown parsing have their own background path. | Notebook and CSV now use virtualized rows (F8). |
| `hosts` | Link registration/lease filesystem work is called on workers; shell owns connection/status snapshots. | Old shell HostManager UI access had a real drop/process/FS path; see F1 and final fix verification. |
| `i18n` | Compiled catalogs, atomic locale and memory cache. | No external IO. |
| `jump` | Palette input observer calls in-memory ranking/filtering synchronously. Vault retrieval is background. | CPU scales with candidate count and query length; no external IO. Large candidate benchmark/tick regression absent; see F9. |
| `keys` | Key encoding/matching only. | No external IO. |
| `layout` | Startup/restore loads run on worker; Saver owner thread serializes writes/fsync. | No foreground load/save. |
| `link` | State/status projection only. | No external IO. |
| `link_ui` | Actual runtime `block_on`, pair/connect/listen calls execute on background workers; busy flags, generation and initial-value checks fence UI completion. | Clipboard future now needs bounded vendor read; no foreground network found. |
| `menu_bar` | Source fetch runs on serial background polling task; UI timer updates menu/title from snapshot. | Native menu update is expected UI work. |
| `onboarding` | `Env::current` clones cached data; startup discovery, survey, fix commands and state write execute on workers. | login-path helper uses bounded parent wait; ignored nonblocking-fd setup failure may leave a reader thread, not event-thread wait. |
| `open_in` | Command construction/canonicalization/spawn executes inside `spawn_open` background boundary, including sidebar menu callbacks. | No foreground metadata lookup found. |
| `orca_import` | Discovery, preview and import use actual background tasks plus generations/operation guards. Render uses prepared models. | Import stdout/stderr now use deadline-aware capped pipe workers (F6). |
| `pane` | Blocking screen/control/socket helpers are called by shell workers. Dedicated Writer thread owns socket writes; UI submits frames to channel. | No direct UI socket read/write. |
| `platform` | Native draw/IME/Dock/SecureInput stay on platform thread. Sleep assertion moved to owner worker; permission/folder probes background. | Native thread-affine calls justified individually; no filesystem/process loop under UI platform lock found. |
| `process_io` (new internal module) | Reader/writer owner threads use nonblocking Unix IO, absolute deadlines and capped 16-worker admission; `PipeTask::finish` uses timed receive only on background callers. | All identified callers migrated; Windows retained-worker limitation documented. |
| `pull_request` | Git/gh Service mutex/process work is invoked by sidebar PR/diff background workers. | Both platforms use deadline-aware bounded pipe helper; Windows retention limit documented (F6). |
| `recording` | Recording file existence check happens during worker connection; UI warning is memory-only. | No foreground file check. |
| `resources` | Render uses snapshots; request function called by background poll/action worker. | No foreground IPC. |
| `search` | Search UI stores debounce/query state; actual search travels through daemon writer/background service. | No foreground full-text IO. |
| `security` | Pure paste planning/confirmation/link formatting; shell prepares clipboard image payload on background executor. | Terminal input ordering/timeout/close behavior tracked F2. |
| `settings` | Startup parser, ordered file IO, account refresh, services, environment probes, SSH and command changes run on workers with generation/state-directory guards. | Font enumeration, local-store writes and external stamp/reload completion now use actual workers (F5/F10). |
| `sidebar` | Poll captures snapshot then worker loads stores/import/config/icons; row/rebuild uses memory. Ordered disk queue owns prefs/lineage/hooks/import writes; generations reject stale scans. Menus defer construction until opened. | Startup-close owner fix present (F4); startup intent merge edge case (F12). |
| `ssh_hosts` | File/config/process helpers invoked by settings jobs or shell host refresh worker. Managed install owns runtime discovery/SSH command. | No active UI external IO found. |
| `star_nag` | Startup state load, serial persist and gh probe run in background; UI renders cached state. | Startup-aware flush fixed during review, F4. |
| `terminal` | Grid/selection/paint reads memory; writer owns IO. | Expected visible glyph/layout work remains; no external IO found. |
| `terminal_font` | Normal visible-font measurement is platform text work. Whole installed-font enumeration/filtering formerly menu-time foreground. | Settings enumeration moved to coalesced background cache, F5. |
| `ui` | Tokens/primitives/icons/status projection; bundled/file image reads use GPUI asset background loader. Theme file import/export handled by settings worker. | No custom foreground filesystem path found. |
| `updater` | CLI update path runs before application event loop; shell staging/poll/rollback acknowledgement and relaunch process run on workers. | Native prompt remains UI; no background claim inferred only from `async` syntax. |
| `webhook` | Config schema/redaction/test helpers; settings worker owns writes/probes. | No active UI external IO. |
| `worktree` | Dialog events memory-only; refs/hooks/create actions invoked by sidebar/shell workers. | `list_git_refs` uses bounded PR execution helper plus git count cap (F6). |
| `main.rs`, `shell.rs`, `shell/**` | Startup preloads before app or on background workers; ordered layout/settings/sidebar/star flushes participate in app quit. Session connections, PTY management, git/files, clipboard payload prep and host snapshots audited at callers. | Chat polling/QuickOpen/host refresh fixes verified; direct socket shutdown exception F15. PTY ownership preserved. |

## Requested vendor coverage

| Requested file | Final reviewed execution boundary / semantics |
|---|---|
| `vendor/gpui-pre-macos/src/clipboard_worker.rs` | One native owner thread, cap2 commands, one pending read, Arc snapshots, accepted-copy revision ordering, no native work/deep clone under shared snapshot mutex. Provider stalls retain only its single owner; reads return by API deadline. |
| `vendor/gpui-pre-macos/src/platform.rs:1232-1260` | Brief platform lock only clones worker handle; lock is released before cache/read/write API. Async read delegates to background 2s deadline. General/find synchronous reads return local snapshot only. |
| `vendor/gpui-pre-windows/src/clipboard_worker.rs` | Same queue/admission/revision/Arc boundary; native clipboard module calls occur only on owner thread. Windows worker native tests were not run on this macOS worker. |
| `vendor/gpui-pre-windows/src/platform.rs:1026-1045` | UI calls cache or nonwaiting command enqueue; async read uses background 2s deadline. |
| `vendor/gpui-base/src/input/base/state.rs:2793-2836` | Fresh async paste, WeakEntity/window completion, editable/current-focus/document revision/selection target checks. No synchronous native read. Focus away and back without edit is still current-target-equal; intent regression gap remains. |

## Final dispositions

The descriptions below retain the original defect/caller evidence; line numbers labeled original refer to the pre-fix working tree. The final pass re-read each corrected execution boundary and the five requested vendor files. No remaining concrete unbounded main-thread file/process/network/IPC request or wait was found in these audited paths. Pure memory operations and native thread-affine single calls remain, as do the specifically qualified resource/compatibility limits below. This is not a claim that all latency or memory use has been measured.

| Finding | Final disposition and current source evidence |
|---|---|
| F1 host getters | Fixed: `shell.rs:1922-2015` refresh/coalesce actual background manager work, generation+Paths fence; `target_for_host:2024`, `shell/new_session.rs:262`, `host_name_for_pane:4456` read memory. Sidebar remote reads also coalesced. |
| F2 terminal clipboard queue | Fixed: `shell/security.rs` 3s full input slot expiration, per-pane progress, session/focus/close cancellation and late-result rejection. `clipboard_paste_payload:570` has backend-owned two-operation admission; native worker read itself has 2s deadline/single-read admission. |
| F3 chat polling | Fixed: `shell/chat.rs:156-213` one in-flight task, release on completion, generation/host/session/offset fences and follow-up when active session changed. Slow coalescing regression at `:904`. |
| F4 startup/flush | Fixed: `star_nag.rs:134-150` startup watch then ordered tail; `sidebar/async_state.rs:64,317-361` startup barrier, finite strong owner and detached writer. Explicit quit in `shell/files.rs::quit_now` awaits editor/settings/sidebar/star/layout barriers while event loop remains alive. |
| F5 installed fonts | Fixed: `settings.rs:2841-2875` coalesced background enumeration/filtering/sort, WeakEntity publication; menu reads cached names. Individual text-system cache access/native visible-font layout remains normal bounded UI work. |
| F6 inherited pipes | Fixed at identified callers: shared `process_io.rs` absolute deadline, nonblocking Unix read/write and max16 native workers; diff patch/discard/rg/git-ignore, sidebar usage, import, QuickOpen and PR execution migrated. Worktree refs uses PR helper with `--count=50`. Windows may retain at most16 blocked native pipe workers; further calls fail admission. |
| F7 QuickOpen | Fixed: `editor/quick_open.rs:43-118` global one-scan admission, 3s absolute deadline, 8MiB incremental stdout bound, bounded fallback scan. Shell rechecks generation/root before asynchronous admission retries. Filesystem calls already entered may outlast the nominal scan deadline, but only one scan retains admission. |
| F8 notebook | Fixed: `file_format/view.rs:369-389` worker-prepared Arc row model; `:462-502` variable-height `gpui::list`, only visible rows construct cells/outputs. Shell prepares preview off-thread. |
| F9 Jump | Retained bounded-by-input CPU/capacity risk; no IO or measured freeze. Regression/measurement gap remains. |
| F10 settings | Fixed: local account/orphan operations use ordered `file_job` (e.g. `settings.rs:3515,3849`); `notify_external_change:590-597` only invalidates memory and requests coalesced background reload. |
| F11 clipboard snapshot | Fixed deep-copy lock hazard: both snapshot/command payloads use Arc; large cloning/destruction outside short mutex. Native calls only on owning thread; MacPlatform lock released before snapshot/native work. Legacy cached by-value API and full-queue copy rejection remain documented compatibility behavior. |
| F12 startup intents | Fixed by this worker (self-review): `sidebar/startup.rs`, explicit mutator marks, ordered initial migration and delayed-load regression. 76 sidebar tests passed. |
| F13 write accumulation | Remote refresh accumulation fixed/coalesced in both shell and sidebar. Replaceable preference snapshots still use an unbounded ordered chain under a held backend; resource limitation retained, not a foreground wait. |
| F14 onboarding tasks | Fixed: operation-keyed `tasks`/`fix_tasks` preserve unrelated completions and busy cleanup. |
| F15 socket teardown | Retained single local shutdown/wakeup syscall, no explicit wait; documented exception, not a request/reply or PTY close. |

## Original defect evidence and regression criteria

Unless explicitly introduced as current, the following descriptions explain the defect when first found; the final disposition table above is authoritative for whether it remains.

### F1 — HostManager UI getters can run process cleanup (P1, fixed)

Original active callers were `shell.rs::target_for_host`, `shell/new_session.rs::execution_locations` and `shell.rs::host_name_for_pane`. They synchronously called `HostManager::target_for/statuses` from UI menu, badge and session actions. `crates/krill-hosts/src/manager.rs:245-270` takes manager/slot locks and calls `tunnel.is_alive`; a dead tunnel is removed there. `crates/krill-hosts/src/ssh.rs:615-620` `Tunnel::drop` kills/waits/removes a socket on that same caller. This proves an active UI process/filesystem path, independently of a long-held lock hypothesis.

**Correction of an early escalation:** this source does **not** hold the host slot lock across SSH connect. `manager.rs:404-405` drops the guard before `Tunnel::start`, then reacquires it at 417. The initial message suggesting a network-long slot lock was retracted to the coordinator. No minute-scale duration or user-stall attribution is established by this review.

Shell owner reports replacing UI getters with `host_targets`/`host_statuses` snapshots and serial background host operations fenced by generation and Paths identity. Current `shell.rs:2024` uses `self.host_targets.get(host).cloned()`. Final pass verified the remaining manager methods are inside the coalesced worker; UI getters read snapshots. Missing protection at first report: lock-held/dead-tunnel fake backend while UI tick advances, plus generation change during refresh.

### F2 — Unresolved clipboard paste blocked ordinary terminal input (P1, fixed)

Original `shell/security.rs::begin_clipboard_paste` inserted an unresolved paste slot; `queue_input_behind_paste` buffered subsequent bytes; `complete_clipboard_paste` drained only until the first unresolved slot of a **global** queue. `paste_clipboard` awaited native clipboard completion without a deadline. A stalled owner could therefore suppress subsequent ordinary terminal keys indefinitely, and a closed pane's unresolved head could delay another pane's paste. This was not event-loop IO, but directly reproduced the visible “terminal does not accept input” failure class by control flow.

Current source introduces `PASTE_PREPARATION_TIMEOUT = 3s`, late-completion rejection, session/pane fencing, per-pane blocked tracking, focus/window cancellation and close cleanup. Vendor reads add a separate 2s native deadline. These changes require final review and tests for never-completing native read, never-completing payload preparation, closed head pane, independent pane progress, and late payload after timeout. A timer that only releases input while leaving unlimited synchronous payload tasks alive would still need resource admission; inspect that separately from input liveness.

### F3 — Chat polling cancels UI completion while synchronous work continues (P2, fixed)

Original `shell/chat.rs:125-148` starts active-chat polling at 800ms intervals. `poll_session_chat:151-182` replaces `self.chat.task` for each request, while its actual daemon request executes synchronously in `background_executor().spawn`. Replacing the outer foreground task does not interrupt an already-running synchronous request. If replies take longer than the interval, overlapping backend calls can continue while successful completions are repeatedly canceled; offset is only advanced after completion. No in-flight/pending guard analogous to AgentHistory was present at inspection.

Required protection: one in-flight poll per relevant session/target, coalesced refresh intent, release busy on every result, generation/session identity checks, slow (>800ms) request test proving UI ticks, bounded call count and eventual visible update. This is background resource/result starvation, not direct UI IPC.

### F4 — Startup and shutdown save races (P2, fixed)

`star_nag.rs` originally returned from flush after existing pending saves only. Agent starts before initial state read were counted in memory but not yet enqueued; immediate quit could finish flush first. Current `flush:134-150` waits on a startup watch receiver, then inserts/awaits an ordered write barrier. `init:~173-213` merges starts, persists, then sends startup completion. Regression added by coordinator: init → record start → immediate flush.

`sidebar/async_state.rs` originally canceled the only startup future when the Sidebar entity closed, even if pre-load edits had merely marked `disk.dirty`. Current `ensure_external_polling:302-361` holds a strong owner for finite initialization, submits dirty state to detached ordered writer, signals initialization and drops the owner before entering the weak polling loop. New regression `closing_sidebar_before_initial_load_still_persists_edits:771` complements the existing blocked initialization/flush cases. A backend that never returns can retain the finite-startup owner; this is a bounded-number retention risk, not a main-thread wait. Final pass verified `shell/files.rs::quit_now` awaits these explicit startup-aware barriers before `cx.quit()`.

### F5 — Installed-font enumeration/filtering on UI menu open (P2, fixed)

Original `settings.rs::render_terminal_tab` dropdown callback called `text_system().all_font_names`, then `terminal_font::usable_terminal_fonts`, sorting all names. The filter resolves/measures each font with seven glyph probes. The macOS implementation (`vendor/gpui-pre-apple/src/text_system.rs:108-143`) enumerates CoreText descriptors and takes the memory-font RwLock. Work scales with installed fonts on the UI callback; no duration was measured.

Current `settings.rs:2841-2875::refresh_font_names` captures the text-system handle, enumerates/filters/sorts inside actual background spawn, and publishes an Arc snapshot via WeakEntity completion. `fonts_pending` coalesces and one-minute freshness bounds repeats. Final pass verified menu render only reads snapshot and completion uses WeakEntity. Missing regression at initial report: slow enumeration plus UI tick, repeated menu opens produce one job.

### F6 — Process deadline does not bound inherited pipe drain (P2, fixed at identified callers)

These production helpers are already invoked off the UI thread; they must not be mislabeled main-thread blocking. They can nevertheless pin workers or defeat advertised timeouts:

| Helper | Proof / trigger | Missing regression |
|---|---|---|
| `diff.rs:343-348` patch command | After deadline, kills/waits direct child, then unconditionally `reader.join()`. Descendant inherits stdout without closing it: direct child termination does not close all writers. | Fake child forks holder, direct child times out/exits, patch returns by deadline. |
| `diff/search.rs:~1009`, `1384-1387` | rg reader join and git-ignore stdin writer/stdout reader joins occur after only direct child kill/wait. Descendant can retain either pipe. | Inherited stdin/stdout independently; bounded method return and bounded reader lifetime. |
| `sidebar/ipc.rs:534` Claude usage | Child timeout is followed by unconditional stdout reader join. | Descendant-held pipe with timed-out direct child. |
| `orca_import.rs:~835-843` `krill import orca` CLI helper | Timeout error returns before reader joins (threads may remain); successful direct child exit then joins both readers (descendant may hold pipe forever). | Success-exit plus retained stdout/stderr; timeout leaves no accumulating threads. |
| `worktree.rs:175-213` refs | Waits direct child before draining stdout; large refs can fill pipe and force 1.5s timeout/empty results. After exit, `wait_with_output` can still wait inherited pipes. | Pipe-capacity-exceeding refs and inherited descriptor. |
| `pull_request.rs::execute` Windows branch | Blocking stdout drain is documented in source as able to delay deadline. Unix branch uses nonblocking reads, so does not have the same indefinite join. | Platform Windows inherited-pipe/deadline test. |

Use nonblocking/bounded reader lifetime or equivalent process-tree ownership where allowed; preserving PTY/session ownership is mandatory. Do not simply call `join` after killing one direct process and label it bounded.

### F7 — Repeated QuickOpen can accumulate unbounded git workers (P2, fixed)

Original `shell/files.rs:~1822-1848` correctly fences stale results by generation, but cancel/reopen starts another detached background `editor/quick_open.rs:22-35` git `ls-files .output()` without an in-flight bound or child deadline. `MAX_FILES` truncates parsed output only **after** `.output()` has collected the complete child output. A delayed git/filesystem can leave many requests alive even though the UI dialog closed. AgentHistory's busy/pending pattern is a safer bounded model. Needed tests: repeat reopen against held listing, call count bounded, latest query eventually publishes, output capped during read.

### F8 — Notebook view does unbounded per-cell render projection on UI (P2, fixed)

Original `file_format/view.rs:369-417::notebook` walks all cells and outputs and creates all elements/TextViews on every render. Its comment explicitly defers virtualization. `file_format/ipynb.rs:45-61` has no independent cell/output count cap; upstream 8 MiB text limit still permits many tiny cells. Background parsing is correct, but does not move this repeated projection/allocation off the event thread. No measured freeze duration is asserted. Required: visible-range rendering or bounded visible model, plus a many-cell UI tick test preserving existing notebook outputs.

### F9 — Jump local ranking remains foreground CPU (P3 / capacity risk)

`jump.rs:650-660` input observer synchronously calls `recompute_all_matches:904`, which ranks item/worktree/action/command/vault collections. `rank:345` scores and sorts candidates and several paths reconstruct lowercase character vectors. These are memory operations, not hidden IO. Their cost grows with candidate count/query length; no evidence presently proves multi-minute cost. Missing protection is a realistic large-candidate benchmark/tick test and, if justified by measurement, cached search keys/coalesced background ranking with query-generation fencing. This is not grounds for rewriting all small in-memory UI operations.

### F10 — Conditional settings helpers retain foreground filesystem access (P2, fixed)

`settings.rs::set_account_rule`, `clear_account_rule`, and orphan purge had direct `accounts::request_local/orphans_local` branches when `is_daemon_state_dir` is false. Default production startup uses daemon state, but public/custom `init_at` can make the other branch reachable; these are not all `cfg(test)`. Local store lock/IO inside a UI handler violates the boundary in that configuration. Settings owner is editing these paths; recheck final source.

Original `settings.rs:592::notify_external_change` calls `file_stamps` (stats seven files) on its caller. A fresh whole-app caller pass proves active foreground completion/event paths: `link_ui.rs:1380/1384` pairing subscriptions, `orca_import.rs` import completion, `shell.rs` host-change completion and many settings `cx.defer` callbacks. The earlier interim dormant classification is withdrawn. `defer` keeps execution on the UI thread; it is not a background boundary. This active path requires async invalidation/reload and a slow-stat UI-tick regression. `canonical_repo_key` canonicalization is now only used inside service/file jobs and is safe at those production callers.

### F11 — Clipboard snapshot large-copy mutex and compatibility semantics (P2 hazard fixed; compatibility limits retained)

Both vendor workers now use `sync_channel(2)` plus nonwaiting `try_send`, and atomic single-read admission. Read futures are deadline-wrapped at platform API; worker owns all native general/read/write calls. A stalled native owner cannot hold MacPlatform's UI lock. Revision comparison prevents an older read from replacing the cache after a newer accepted copy. Rejected/full-queue writes now leave cache unchanged and log failure; this is a deliberate bounded-admission tradeoff, not a successful copy.

Original residual, now corrected by Arc snapshot/command payloads and cloning/replaced-value drops outside the mutex: mac `clipboard_worker.rs:89-94,132-140,167` and Windows `:66,105-107,45` clone ClipboardItem under shared snapshot mutex. `ClipboardItem` owns a Vec of entries and Image owns `Vec<u8>` (`vendor/gpui-pre/src/platform.rs`), so this can allocate/copy a large image inside the lock and on the UI caller. Native IO itself is outside the snapshot lock. Move deep preparation outside the critical section or use cheap immutable handles/nonwaiting snapshot access; small-string queue tests do not cover this contention.

Legacy synchronous clipboard reads now return only the last local snapshot, not external system freshness. Current application paste paths (`shell/security`, `link_ui`, GPUI base input paste) use fresh async API. Mac find-pasteboard cached read has no active application consumer found. `vendor/gpui-component/src/input/input.rs::on_paste` still uses synchronous cache, but no application `.on_paste` registration was found. These are compatibility limitations, not active UI IO. Base input paste (`vendor/gpui-base/src/input/base/state.rs:~2794-2827`) uses WeakEntity/window completion and checks editable/focus/document revision/selections; stale edits are rejected. Test gaps: real delayed-provider cancellation, focus-away-and-back intent, huge image snapshot contention, and Windows queue ordering/admission tests.

### F12 — Startup snapshot confuses default values with untouched intent (P2, fixed; self-review)

Original `sidebar/async_state.rs:137-165::Startup::apply` chooses disk options when current options equal Default, unions persisted collapsed/expanded sets, and restores selected worktree when current is None. This is not an adequate intent fence. With initial read held: persisted group mode is None; user selects another mode and then explicitly returns to Repo (Default); eventual disk completion changes it back to None. Similarly `sidebar.rs:974::fold_host(host, Some(true))` explicitly opens a host, but later `collapsed_hosts.extend` recloses it. `select_worktree(None)` is indistinguishable from an untouched initial None. Existing positive-name/edit tests do not cover negative/default-valued edits. Need per-field/set touched intent or delayed interactions, with blocked-load round-trip/clear/open regressions. Coordinator authorized implementation: `sidebar/startup.rs` now records per-field/per-key edits and explicit negative/default-valued intents. `write_state` records intermediate transitions so a round trip to default remains edited; untouched fields still merge from current disk data. Initial read/legacy lineage migration also enters the ordered queue, preventing migration from restoring a parent removed concurrently. Delayed startup regression covers UI tick, external file change, default group mode, explicit card property, filter clear, host/folder open, collapsed card round trip, selection clear, wake, parent clear and rename preserving an untouched saved pin. Existing startup-owner and close/flush tests remain.

### F13 — Ordered snapshot writes have no admission/coalescing bound (P3 / resource risk)

`sidebar.rs:1788-1826::write_state` clones the full preference/options/lineage/UI snapshot per persist. `sidebar/async_state.rs:472-509::enqueue_disk_write_with_completion` creates a detached background future, predecessor oneshot and UI completion for every write. Ordering and view-close survival are correct, but a held first file lock allows arbitrarily many later preference-only snapshots/futures to accumulate as the UI continues accepting fold/filter changes. Sidebar remote reads are now coalesced to one in-flight plus one follow-up; the held-backend regression submits 25 refreshes and expects only two starts. `settings.rs:945::background_file_io` uses a similar serial predecessor chain for heterogeneous jobs; settings edit changes must not be casually dropped. No direct UI blocking or measured memory overrun is claimed. Consider coalescing replaceable same-file snapshots between noncommuting operations, and test queue growth under a held backend; preserve operation order and explicit flush semantics.

### F14 — Onboarding shared task cancels unrelated completion (P2, fix observed)

Original `onboarding/welcome.rs:175,209,277,394,422` assigned collect/recheck/fix-all/notification authorization/folder probe to one `self.task`, while busy admission was per operation. Starting a different action canceled the first completion while its synchronous worker continued; its busy/checks_loading flag then remained set. Window activation could similarly replace a permission completion with recheck. Current owner fix stores separate operation-keyed `tasks` and `fix_tasks`, preserving unrelated completions. Required regression: hold notification action, run folder/recheck action, release and prove both flags clear and UI ticks. Final test status belongs in integration results.

### F15 — Remaining single socket teardown syscall (P3 / explicit exception)

At inspection, `shell.rs::Live::cut` calls `UnixStream::shutdown(Shutdown::Both)` directly from UI teardown. It is a single cancellation/wakeup syscall with no explicit read/write/wait loop, used to wake the background reader and release a screen connection; it does not terminate the PTY session. This is materially different from synchronous IPC request/reply. It is nevertheless movable off-thread under a strict zero-external-syscall policy and was sent to shell owner for final disposition. No measured latency or stall is attributed to it.

## Validation and final disposition

- Review scope: all 41 public library modules, the added internal `process_io` module, `main.rs` and shell children; the exact public count is derived from current `lib.rs` (module additions remain included). Requested vendor platform/worker/base paste paths were re-read after fixes.
- Focused command: `CARGO_TARGET_DIR=/Volumes/krill-build/targets/krill cargo nextest run -p krill-app --locked -E 'test(sidebar::)' --no-fail-fast` → exit 0, **76 passed, 882 skipped**. Log: `/tmp/krill-sidebar-intent-tests.log`. This includes the two added delayed-startup/remote-coalescing regressions and the retained initial-close/flush tests.
- `rustfmt --edition 2024 --config skip_children=true` applied only to owned sidebar Rust files. `git diff --check` on changed sidebar/report files → exit 0.
- Base HEAD remains `e0a8a4e53a3c0265c4fb8345b54622a8e3bcd370`; validation applies to this dirty working tree, not a clean release HEAD. The coordinator owns final all-target clippy, Windows and broad app regression verification; none is claimed here. A linker unwind-table warning occurred during the focused build; tests completed successfully.
- Changed by this dispatch after coordinator authorization: `crates/krill-app/src/sidebar.rs`, `sidebar/async_state.rs`, new `sidebar/startup.rs`, `sidebar/options.rs`, `sidebar/lineage.rs`, `sidebar/tests.rs`, and this report. Previous dirty edits, root startup-owner fixes, and other workers' changes were preserved.
- Remaining practical limits: native clipboard provider can retain its single owner thread after UI deadline; Windows native pipe calls can retain bounded workers; synchronous filesystem calls already on workers are not interruptible by Rust task cancellation; ordered durable snapshot queues can retain pending snapshots while a backend lock never returns. Legacy cached clipboard API returns local snapshot and can reject copy when queue is full. None of these remaining limits is presented as a measured explanation of the original multi-minute stall.

Coordinator received all actionable findings immediately, plus the explicit correction to the early SSH lock-duration hypothesis. Final source dispositions above are verified from implementation, not merely owner claims. No PTY/session ownership was changed by the authorized sidebar edits.
