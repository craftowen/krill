//! Native AX worker path (`krill-app --computer-worker`, contract `krill-computer-worker/1`). Opt-in with
//! `KRILL_COMPUTER_ENGINE=native`; JXA stays the default.
//!
//! Worker snapshots live only in worker memory and every CLI call is a new process. Instead of a resident
//! worker (a new lifecycle, socket and TCC subject to validate) this path keeps the CLI's existing contract:
//! the caller-scoped `get-app-state` tree is the authority. An action spawns one worker that observes the
//! same window again, requires every node from the window down to the target to keep its role, labels,
//! frame, actions and value ([`same_path`]), and then acts on the fresh reference inside that same worker.
//! The worker proves target uniqueness over its own complete fresh census and rechecks ancestry, enabled
//! state and the frontmost receiver before any input.
//!
//! Unlike the JXA path this does not require the rest of the window to be unchanged: live apps move
//! unrelated nodes between calls (a scrolled sidebar was measured flipping offscreen rows every few
//! hundred milliseconds), and such changes cannot redirect a reference whose own path and frame held.

use std::io::{BufReader, Write as _};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::super::mcp::transport::{read_line, worker_path};
use super::{
    AppStateSnapshot, AppSummary, Cleanup, Delivery, ElementNode, LiveAction, LiveActionOutcome,
    LiveObservation, Rect, WindowSummary, element_path, node_at_path, same_element_identity,
};

const CONTRACT: &str = "krill-computer-worker/1";
/// Worker-side budget for one request; the worker itself caps requests at 30 s.
const DEADLINE_MS: u64 = 20_000;
/// Extra wait for the answer frame after the worker's own deadline.
const ANSWER_GRACE: Duration = Duration::from_secs(5);
/// Marks snapshots observed by this path; the worker witness format differs from JXA's on purpose.
const PROCESS_PREFIX: &str = "native:";
/// Window ids from this path are AX window indexes, never CG window numbers.
const WINDOW_PREFIX: &str = "ax:";

pub(super) fn enabled() -> bool {
    std::env::var_os("KRILL_COMPUTER_ENGINE").is_some_and(|engine| engine == "native")
        && worker_path(None).is_ok()
}

pub(super) fn is_native_window(id: &str) -> bool {
    id.starts_with(WINDOW_PREFIX)
}

pub(super) fn is_native_snapshot(snapshot: &AppStateSnapshot) -> bool {
    snapshot.process_start.starts_with(PROCESS_PREFIX)
}

struct Worker {
    child: Child,
    stdin: Option<ChildStdin>,
    frames: mpsc::Receiver<Value>,
    epoch: Value,
    context: Value,
    seq: u64,
}

impl Worker {
    fn spawn() -> Result<Self, String> {
        let mut child = Command::new(worker_path(None)?)
            .arg("--computer-worker")
            .arg(format!("--computer-protect-pid={}", std::process::id()))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|error| format!("start native computer worker: {error}"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or("native worker stdout unavailable")?;
        let (sender, frames) = mpsc::channel();
        // A reader thread lets every wait carry a timeout; a hung worker cannot hang the CLI.
        std::thread::spawn(move || {
            let (mut stdout, mut bytes) = (BufReader::new(stdout), Vec::new());
            while let Ok(true) = read_line(&mut stdout, &mut bytes) {
                let Ok(frame) = serde_json::from_slice::<Value>(&bytes) else {
                    break;
                };
                if sender.send(frame).is_err() {
                    break;
                }
            }
        });
        let mut worker = Self {
            stdin: child.stdin.take(),
            child,
            frames,
            epoch: Value::Null,
            context: Value::Null,
            seq: 0,
        };
        let hello = worker.next_frame(ANSWER_GRACE)?;
        if hello["type"] != "hello" || hello["contract"] != CONTRACT || hello["supported"] != true {
            return Err(
                "native computer worker speaks another contract or is unsupported".to_owned(),
            );
        }
        worker.epoch = hello["epoch"].clone();
        worker.context = hello["context"].clone();
        Ok(worker)
    }

    fn next_frame(&self, wait: Duration) -> Result<Value, String> {
        self.frames
            .recv_timeout(wait)
            .map_err(|_| "native computer worker stopped answering".to_owned())
    }

    /// `Err` before the frame is written means nothing was sent; afterwards the caller must treat the
    /// outcome of a mutation as unknown.
    fn request(&mut self, op: &Value) -> Result<Value, RequestError> {
        self.seq += 1;
        let frame = json!({
            "type": "request",
            "epoch": self.epoch,
            "context": self.context,
            "seq": self.seq,
            "deadlineMs": DEADLINE_MS,
            "op": op,
        });
        let stdin = self
            .stdin
            .as_mut()
            .ok_or_else(|| RequestError::NotSent("native worker input closed".to_owned()))?;
        writeln!(stdin, "{frame}")
            .and_then(|()| stdin.flush())
            .map_err(|error| {
                RequestError::NotSent(format!("write native worker request: {error}"))
            })?;
        let until = Instant::now() + Duration::from_millis(DEADLINE_MS) + ANSWER_GRACE;
        loop {
            let frame = self
                .next_frame(until.saturating_duration_since(Instant::now()))
                .map_err(RequestError::Sent)?;
            match frame["type"].as_str() {
                Some("result") if frame["seq"] == self.seq => return Ok(frame),
                // Receipt rejections never reserve or run the request.
                Some("rejected") => {
                    return Err(RequestError::NotSent(format!(
                        "native worker rejected request: {}",
                        frame["message"].as_str().unwrap_or("no reason")
                    )));
                }
                Some("protocol_error") => {
                    return Err(RequestError::Sent(format!(
                        "native worker protocol error: {}",
                        frame["message"].as_str().unwrap_or("no reason")
                    )));
                }
                _ => {}
            }
        }
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        // EOF ends the worker; a worker that does not exit promptly is killed.
        drop(self.stdin.take());
        let until = Instant::now() + Duration::from_secs(1);
        while Instant::now() < until {
            if self.child.try_wait().is_ok_and(|status| status.is_some()) {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        drop(self.child.kill());
        drop(self.child.wait());
    }
}

#[derive(Debug)]
enum RequestError {
    NotSent(String),
    Sent(String),
}

impl RequestError {
    fn into_message(self) -> String {
        match self {
            Self::NotSent(message) | Self::Sent(message) => message,
        }
    }
}

/// A fresh observation; worker references follow the tree's pre-order.
struct Observed {
    snapshot: String,
    window: WindowSummary,
    observation: LiveObservation,
}

fn parse_window(id: Option<&str>) -> Result<Option<usize>, String> {
    id.map(|id| {
        id.strip_prefix(WINDOW_PREFIX)
            .and_then(|index| index.parse().ok())
            .ok_or_else(|| format!("window id is not a native AX window: {id}"))
    })
    .transpose()
}

fn observe_with(
    worker: &mut Worker,
    app: &AppSummary,
    window_id: Option<&str>,
) -> Result<Observed, String> {
    let window = parse_window(window_id)?;
    let frame = worker
        .request(&json!({"kind": "observe", "pid": app.pid, "window": window}))
        .map_err(RequestError::into_message)?;
    if frame["observation"]["state"] != "observed" {
        return Err(format!(
            "native observation refused: {}",
            error_text(&frame)
        ));
    }
    let data = &frame["data"];
    let elements = data["elements"]
        .as_array()
        .ok_or("native observation has no elements")?;
    let root = build_tree(elements)?;
    let index = data["window"]["index"]
        .as_u64()
        .and_then(|index| usize::try_from(index).ok())
        .ok_or("observed window is not in the app's window list; pass an explicit window")?;
    let witness = data["app"]["startWitness"]
        .as_str()
        .filter(|witness| !witness.is_empty())
        .ok_or("process start witness unavailable")?;
    let window = WindowSummary {
        id: format!("{WINDOW_PREFIX}{index}"),
        index: index + 1,
        title: root.title.clone().unwrap_or_default(),
        app_name: app.name.clone(),
        bundle_id: app.bundle_id.clone(),
        pid: app.pid,
        bounds: root.bounds.ok_or("observed window has no frame")?,
        is_focused: window_id.is_none(),
        is_main: false,
        is_minimized: false,
    };
    Ok(Observed {
        snapshot: data["snapshot"]
            .as_str()
            .ok_or("native observation has no snapshot id")?
            .to_owned(),
        window,
        observation: LiveObservation {
            root,
            process_start: format!("{PROCESS_PREFIX}{witness}"),
            coverage_truncated: frame["observation"]["coverage"]["truncated"] != false,
            geometry: None,
            geometry_error: None,
        },
    })
}

pub(super) fn observe(
    app: &AppSummary,
    window_id: Option<&str>,
) -> Result<(WindowSummary, LiveObservation), String> {
    let observed = observe_with(&mut Worker::spawn()?, app, window_id)?;
    Ok((observed.window, observed.observation))
}

/// Worker elements arrive in pre-order with `parent` keys; the tree keeps that order.
fn build_tree(elements: &[Value]) -> Result<ElementNode, String> {
    let mut nodes: Vec<Option<ElementNode>> = Vec::with_capacity(elements.len());
    let mut parents = Vec::with_capacity(elements.len());
    for (index, element) in elements.iter().enumerate() {
        if element["key"] != format!("e{index}") {
            return Err("native observation keys are not in pre-order".to_owned());
        }
        let parent = match element["parent"].as_str() {
            None if index == 0 => None,
            Some(key) => Some(
                key.strip_prefix('e')
                    .and_then(|n| n.parse::<usize>().ok())
                    .filter(|&n| n < index)
                    .ok_or("native observation parent is not an earlier element")?,
            ),
            None => return Err("native observation has more than one root".to_owned()),
        };
        parents.push(parent);
        nodes.push(Some(element_node(element)));
    }
    // Children have larger indexes, so walking backwards attaches complete subtrees.
    for index in (1..nodes.len()).rev() {
        let mut node = nodes[index].take().ok_or("native tree assembly failed")?;
        node.children.reverse();
        let parent = parents[index].ok_or("native tree assembly failed")?;
        nodes[parent]
            .as_mut()
            .ok_or("native tree assembly failed")?
            .children
            .push(node);
    }
    let mut root = nodes
        .first_mut()
        .and_then(Option::take)
        .ok_or("native observation is empty")?;
    root.children.reverse();
    Ok(root)
}

fn element_node(element: &Value) -> ElementNode {
    let text = |field: &str| element[field].as_str().map(str::to_owned);
    let frame = &element["frame"];
    ElementNode {
        reference_id: None,
        role: text("role").unwrap_or_default(),
        subrole: text("subrole"),
        title: text("title"),
        description: text("description"),
        value: match &element["value"] {
            Value::String(value) => Some(value.clone()),
            Value::Number(value) => Some(value.to_string()),
            Value::Bool(value) => Some(value.to_string()),
            _ => None,
        },
        value_settable: element["valueSettable"].as_bool(),
        bounds: (|| {
            Some(Rect {
                x: frame["x"].as_f64()?,
                y: frame["y"].as_f64()?,
                width: frame["width"].as_f64()?,
                height: frame["height"].as_f64()?,
            })
        })(),
        enabled: element["enabled"].as_bool().unwrap_or(true),
        focused: element["focused"].as_bool().unwrap_or(false),
        selected: false,
        actions: element["actions"]
            .as_array()
            .map(|actions| {
                actions
                    .iter()
                    .filter_map(|action| action.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default(),
        children: Vec::new(),
    }
}

/// Every node from the root down to `path` keeps its identity, frame, actions and value.
fn same_path(old: &ElementNode, new: &ElementNode, path: &[usize]) -> bool {
    (0..=path.len()).all(|depth| {
        let prefix = path.get(..depth).unwrap_or_default();
        node_at_path(old, prefix)
            .zip(node_at_path(new, prefix))
            .is_some_and(|(old, new)| same_element_identity(old, new) && old.value == new.value)
    })
}

/// Pre-order position of the node at `path`, which is its worker reference number.
fn preorder_index(root: &ElementNode, path: &[usize]) -> Option<usize> {
    fn size(node: &ElementNode) -> usize {
        1 + node.children.iter().map(size).sum::<usize>()
    }
    let mut node = root;
    let mut index = 0;
    for &child in path {
        index += 1 + node.children.get(..child)?.iter().map(size).sum::<usize>();
        node = node.children.get(child)?;
    }
    Some(index)
}

fn not_dispatched(reason: String) -> LiveActionOutcome {
    LiveActionOutcome {
        issued_units: Some(0),
        delivery: Delivery::NotDispatched,
        cleanup: Cleanup::NotNeeded,
        clipboard: None,
        ok: false,
        status: "unverified".to_owned(),
        reason: Some(reason),
        note: Some("path=native; no backend fallback".to_owned()),
    }
}

/// Element click, secondary action and set-value only. Everything else is refused before any input:
/// the JXA action script cannot validate against a native tree, so there is no silent fallback.
pub(super) fn execute(
    base: &AppStateSnapshot,
    app: &AppSummary,
    action: &LiveAction,
) -> LiveActionOutcome {
    let (target_ref, op) = match action {
        LiveAction::Click {
            target_ref: Some(target),
            button,
            click_count: 1,
            modifiers,
            ..
        } if button == "left" && modifiers.is_empty() => {
            (*target, json!({"kind": "action", "name": "AXPress"}))
        }
        LiveAction::SecondaryAction {
            target_ref,
            action_name,
            ..
        } => (*target_ref, json!({"kind": "action", "name": action_name})),
        LiveAction::SetValue {
            target_ref, value, ..
        } => (
            *target_ref,
            json!({"kind": "set_value", "text": value, "verify": true}),
        ),
        _ => {
            return not_dispatched(
                "this action needs the JXA engine; observe again without KRILL_COMPUTER_ENGINE=native"
                    .to_owned(),
            );
        }
    };
    match prepare_and_run(base, app, target_ref, op) {
        Ok(outcome) => outcome,
        Err(RequestError::NotSent(reason)) => not_dispatched(reason),
        Err(RequestError::Sent(reason)) => {
            let mut outcome = LiveActionOutcome::unknown(reason);
            outcome.note = Some("path=native; no mutation replay".to_owned());
            outcome
        }
    }
}

fn prepare_and_run(
    base: &AppStateSnapshot,
    app: &AppSummary,
    target_ref: usize,
    mut op: Value,
) -> Result<LiveActionOutcome, RequestError> {
    let not_sent = |reason: &str| RequestError::NotSent(reason.to_owned());
    let base_root = base
        .root
        .as_ref()
        .ok_or_else(|| not_sent("original AX root unavailable"))?;
    let path = element_path(base_root, target_ref)
        .ok_or_else(|| not_sent("element is not in the original snapshot"))?;
    if op["kind"] == "action"
        && node_at_path(base_root, &path)
            .is_none_or(|node| !node.actions.iter().any(|name| op["name"] == name.as_str()))
    {
        return Err(not_sent(
            "requested AX action unavailable; no coordinate fallback",
        ));
    }
    let mut worker = Worker::spawn().map_err(RequestError::NotSent)?;
    let window_id = base.window.as_ref().map(|window| window.id.as_str());
    let fresh = observe_with(&mut worker, app, window_id).map_err(RequestError::NotSent)?;
    if fresh.observation.process_start != base.process_start {
        return Err(not_sent("process instance changed; observe again"));
    }
    if !same_path(base_root, &fresh.observation.root, &path) {
        return Err(not_sent(
            "target or an ancestor changed since observation; observe again",
        ));
    }
    let key = preorder_index(&fresh.observation.root, &path)
        .ok_or_else(|| not_sent("target is missing from the fresh observation"))?;
    op["target"] = json!({"snapshot": fresh.snapshot, "element": format!("e{key}")});
    let frame = worker.request(&op)?;
    Ok(outcome_from(&frame))
}

fn error_text(frame: &Value) -> String {
    let error = &frame["error"];
    match (error["code"].as_str(), error["message"].as_str()) {
        (Some(code), Some(message)) => format!("{code}: {message}"),
        _ => frame["delivery"]["reason"]
            .as_str()
            .unwrap_or("no reason")
            .to_owned(),
    }
}

fn outcome_from(frame: &Value) -> LiveActionOutcome {
    let delivery = &frame["delivery"];
    let reason = (delivery["state"] != "submitted").then(|| error_text(frame));
    let issued_units = delivery["issuedUnits"]
        .as_u64()
        .and_then(|units| u32::try_from(units).ok());
    let (delivery, issued_units) = match delivery["state"].as_str() {
        Some("submitted") => (Delivery::Submitted, issued_units),
        Some("partial") => (Delivery::Partial, issued_units),
        Some("not_dispatched") => (Delivery::NotDispatched, Some(0)),
        _ => (Delivery::Unknown, None),
    };
    let verified = frame["verification"]["state"] == "satisfied";
    LiveActionOutcome {
        issued_units,
        delivery,
        cleanup: match frame["cleanup"]["state"].as_str() {
            Some("completed") => Cleanup::Completed,
            Some("not_needed") => Cleanup::NotNeeded,
            _ => Cleanup::Incomplete,
        },
        clipboard: None,
        ok: delivery == Delivery::Submitted,
        status: if verified { "verified" } else { "unverified" }.to_owned(),
        reason: reason.or_else(|| frame["verification"]["reason"].as_str().map(str::to_owned)),
        note: Some("path=native".to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn element(key: usize, parent: Option<usize>, role: &str) -> Value {
        json!({"key": format!("e{key}"), "parent": parent.map(|p| format!("e{p}")), "role": role,
            "frame": {"x": 0.0, "y": 0.0, "width": 1.0, "height": 1.0}})
    }

    #[test]
    fn pre_order_elements_round_trip_to_tree_and_worker_keys() {
        // window → [group → [button, text], close]
        let elements = [
            element(0, None, "AXWindow"),
            element(1, Some(0), "AXGroup"),
            element(2, Some(1), "AXButton"),
            element(3, Some(1), "AXStaticText"),
            element(4, Some(0), "AXButton"),
        ];
        let root = build_tree(&elements).unwrap();
        let roles: Vec<_> = root.children.iter().map(|n| n.role.as_str()).collect();
        assert_eq!(roles, ["AXGroup", "AXButton"]);
        assert_eq!(root.children[0].children[1].role, "AXStaticText");
        assert_eq!(preorder_index(&root, &[]), Some(0));
        assert_eq!(preorder_index(&root, &[0, 1]), Some(3));
        assert_eq!(preorder_index(&root, &[1]), Some(4));
        assert_eq!(preorder_index(&root, &[2]), None);
    }

    #[test]
    fn only_the_target_path_must_hold() {
        let elements = [
            element(0, None, "AXWindow"),
            element(1, Some(0), "AXGroup"),
            element(2, Some(1), "AXButton"),
            element(3, Some(0), "AXList"),
        ];
        let old = build_tree(&elements).unwrap();
        let mut new = old.clone();
        // An unrelated branch scrolls: still the same target.
        new.children[1].bounds = Some(Rect {
            x: 0.0,
            y: -19.0,
            width: 1.0,
            height: 1.0,
        });
        assert!(same_path(&old, &new, &[0, 0]));
        // Target value, an ancestor label, the window frame, or the target's place all refuse.
        let mut changed = old.clone();
        changed.children[0].children[0].value = Some("other".into());
        assert!(!same_path(&old, &changed, &[0, 0]));
        let mut changed = old.clone();
        changed.children[0].title = Some("Other group".into());
        assert!(!same_path(&old, &changed, &[0, 0]));
        let mut changed = old.clone();
        changed.bounds = None;
        assert!(!same_path(&old, &changed, &[0, 0]));
        let mut changed = old.clone();
        changed.children[0].children.clear();
        assert!(!same_path(&old, &changed, &[0, 0]));
    }

    #[test]
    fn malformed_worker_trees_are_refused() {
        let forward = [element(0, None, "AXWindow"), element(1, Some(1), "AXGroup")];
        assert!(build_tree(&forward).is_err());
        let two_roots = [element(0, None, "AXWindow"), element(1, None, "AXWindow")];
        assert!(build_tree(&two_roots).is_err());
        let gap = [element(0, None, "AXWindow"), element(2, Some(0), "AXGroup")];
        assert!(build_tree(&gap).is_err());
        assert!(build_tree(&[]).is_err());
        assert_eq!(parse_window(Some("ax:2")), Ok(Some(2)));
        assert!(parse_window(Some("12345")).is_err());
    }

    #[test]
    fn worker_outcomes_never_claim_more_than_reported() {
        let frame = |delivery: Value| {
            json!({"delivery": delivery, "cleanup": {"state": "not_needed"},
            "verification": {"state": "not_requested"}})
        };
        let submitted = outcome_from(&frame(json!({"state": "submitted", "issuedUnits": 1})));
        assert!(submitted.ok);
        assert_eq!(submitted.status, "unverified");
        let refused = outcome_from(&frame(json!({"state": "not_dispatched", "reason": "x"})));
        assert_eq!(refused.delivery, Delivery::NotDispatched);
        assert!(!refused.ok);
        let odd = outcome_from(&frame(json!({"state": "mystery"})));
        assert_eq!(odd.delivery, Delivery::Unknown);
        assert_eq!(odd.issued_units, None);
    }
}
