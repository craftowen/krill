//! macOS 실행기: AX 관찰·실행 직전 재검증. 입력 경로는 `input`, FFI는 [`super::ffi`], 정책은 [`super::worker`].
//!
//! 참조는 관찰 시점의 의미·값·부모 경로·창에 묶인다. 같은 CF 객체의 라벨·값·부모 변경도 거절한다.
//! 지문·전체 창 유일성을 상한 안에서 증명하지 못하면 재관찰을 요구한다. AX 조회와 이벤트 발행은
//! 원자적이지 않으므로 사용자 개입 검사는 best effort이며 OS 전체 입력 격리를 보장하지 않는다.

use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::ffi::{self, AttrBatch, AxError, Cf, ax};
use super::protocol::{
    Coverage, DEFAULT_OBSERVE_NODES, MAX_OBSERVE_NODES, MAX_TEXT_UTF16, Observation, Target,
};
use super::worker::{Identity, ReadResult, Refusal};
use crate::platform::permissions;

mod input;

const MAX_SNAPSHOTS: usize = 4;
const MAX_DEPTH: usize = 24;
const MAX_CHILDREN: usize = 200;
const MAX_WINDOWS: usize = 64;
const MAX_ACTIONS: usize = 24;
const MAX_ANCESTRY: usize = 64;
const MAX_APPS: usize = 256;
const PREVIEW_UTF16: usize = 500;
const LABEL_UTF16: usize = 128;
const IDENTITY_UTF16: usize = MAX_TEXT_UTF16;
const MAX_OBSERVE_BYTES: usize = 512 * 1024;
/// 남은 예산이 이보다 적으면 새 AX 호출을 시작하지 않는다.
const MARGIN: Duration = Duration::from_millis(50);
/// AX 호출 하나의 상한. 요청의 남은 시간과 이 값 중 작은 쪽을 쓴다.
const AX_CALL_CAP: Duration = Duration::from_secs(2);
/// 키 이벤트 하나에 싣는 UTF-16 단위.
// ponytail: 20단위 고정. 앱별로 긴 유니코드 이벤트를 자르는지 실측되면 조정한다.
const TYPE_CHUNK_UTF16: usize = 20;
/// 이 워커가 호출할 수 있는 AX 동작. 관찰된 동작 목록에도 있어야 한다.
const ALLOWED_ACTIONS: [&str; 8] = [
    "AXPress",
    "AXConfirm",
    "AXCancel",
    "AXRaise",
    "AXShowMenu",
    "AXIncrement",
    "AXDecrement",
    "AXPick",
];
const SELF_BUNDLE_PREFIX: &str = "dev.krill.app.";

const CAPABILITIES: [&str; 9] = [
    "capabilities",
    "permissions",
    "apps",
    "observe",
    "action",
    "set_value",
    "click",
    "type_text",
    "batch",
];
const UNSUPPORTED: [&str; 10] = [
    "screenshot",
    "clipboard",
    "key",
    "hotkey",
    "scroll",
    "drag",
    "activate",
    "restore_window",
    "observer",
    "delta",
];

// `AttrBatch` 순서.
const B_ROLE: usize = 0;
const B_SUBROLE: usize = 1;
const B_TITLE: usize = 2;
const B_DESCRIPTION: usize = 3;
const B_VALUE: usize = 4;
const B_IDENTIFIER: usize = 5;
const B_ENABLED: usize = 6;
const B_FOCUSED: usize = 7;
const B_POSITION: usize = 8;
const B_SIZE: usize = 9;

#[derive(Debug)]
struct Attrs {
    role: Cf,
    value: Cf,
    position: Cf,
    size: Cf,
    children: Cf,
    windows: Cf,
    focused_window: Cf,
    focused_element: Cf,
    parent: Cf,
    enabled: Cf,
    sheets: Cf,
    modal: Cf,
    batch: AttrBatch,
}

impl Attrs {
    fn new() -> Option<Self> {
        let s = ffi::cf_string;
        let role = s("AXRole")?;
        let subrole = s("AXSubrole")?;
        let title = s("AXTitle")?;
        let description = s("AXDescription")?;
        let value = s("AXValue")?;
        let identifier = s("AXIdentifier")?;
        let enabled = s("AXEnabled")?;
        let focused = s("AXFocused")?;
        let position = s("AXPosition")?;
        let size = s("AXSize")?;
        let batch = AttrBatch::new(&[
            &role,
            &subrole,
            &title,
            &description,
            &value,
            &identifier,
            &enabled,
            &focused,
            &position,
            &size,
        ])?;
        Some(Self {
            role,
            value,
            position,
            size,
            children: s("AXChildren")?,
            windows: s("AXWindows")?,
            focused_window: s("AXFocusedWindow")?,
            focused_element: s("AXFocusedUIElement")?,
            parent: s("AXParent")?,
            enabled,
            sheets: s("AXSheets")?,
            modal: s("AXModal")?,
            batch,
        })
    }
}

#[derive(Debug, Clone)]
struct Node {
    element: Cf,
    parent: Option<usize>,
    identity: Option<Semantic>,
}

/// 표시용 미리보기와 독립적이다. 잘린 문자열·해석 못한 값은 지문이 되지 않는다.
#[derive(Debug, Clone, PartialEq)]
struct Semantic {
    role: String,
    subrole: Option<String>,
    title: Option<String>,
    description: Option<String>,
    identifier: Option<String>,
    value: SemanticValue,
    frame: Option<(ffi::CgPoint, ffi::CgSize)>,
}

#[derive(Debug, Clone, PartialEq)]
enum SemanticValue {
    Absent,
    Text(String),
    Number(f64),
    Flag(bool),
}

impl Semantic {
    fn from_values(values: &[Option<Cf>]) -> Option<Self> {
        let get = |index: usize| values.get(index).and_then(Option::as_ref);
        let text = |index: usize| -> Option<Option<String>> {
            match get(index) {
                None => Some(None),
                Some(value) => match ffi::string_value(value, IDENTITY_UTF16) {
                    Some((text, false)) => Some((!text.is_empty()).then_some(text)),
                    _ => None,
                },
            }
        };
        // AXValue를 전부 읽되 보존 상한을 넘으면 입력용 참조로 쓰지 않는다.
        let value = match get(B_VALUE) {
            None => SemanticValue::Absent,
            Some(value) => {
                if let Some((text, false)) = ffi::string_value(value, MAX_TEXT_UTF16) {
                    SemanticValue::Text(text)
                } else if let Some(flag) = ffi::bool_value(value) {
                    SemanticValue::Flag(flag)
                } else {
                    let number = ffi::number_value(value).filter(|n| n.is_finite())?;
                    SemanticValue::Number(number)
                }
            }
        };
        let frame = match (get(B_POSITION), get(B_SIZE)) {
            (None, None) => None,
            (Some(position), Some(size)) => {
                let point = ffi::point_value(position)?;
                let size = ffi::size_value(size)?;
                if ![point.x, point.y, size.width, size.height]
                    .iter()
                    .all(|n| n.is_finite())
                {
                    return None;
                }
                Some((point, size))
            }
            _ => return None,
        };
        Some(Self {
            role: text(B_ROLE)??,
            subrole: text(B_SUBROLE)?,
            title: text(B_TITLE)?,
            description: text(B_DESCRIPTION)?,
            identifier: text(B_IDENTIFIER)?,
            value,
            frame,
        })
    }

    fn bytes(&self) -> usize {
        self.role.len()
            + [
                &self.subrole,
                &self.title,
                &self.description,
                &self.identifier,
            ]
            .into_iter()
            .flatten()
            .map(String::len)
            .sum::<usize>()
            + match &self.value {
                SemanticValue::Text(text) => text.len(),
                _ => 0,
            }
    }

    fn stable_matches(&self, other: &Self) -> bool {
        self.role == other.role
            && self.subrole == other.subrole
            && self.title == other.title
            && self.description == other.description
            && self.identifier == other.identifier
            && self.frame == other.frame
    }

    fn matches(&self, other: &Self, compare_value: bool) -> bool {
        self.stable_matches(other) && (!compare_value || self.value == other.value)
    }

    /// 유일성 판정에서 위치는 제외한다. 같은 라벨 두 개를 좌표로 억지 구별하지 않는다.
    fn same_candidate(&self, other: &Self, compare_value: bool) -> bool {
        if let Some(identifier) = &self.identifier {
            return other.identifier.as_ref() == Some(identifier);
        }
        self.role == other.role
            && self.subrole == other.subrole
            && self.title == other.title
            && self.description == other.description
            && (!compare_value || self.value == other.value)
    }

    fn has_identity(&self) -> bool {
        self.identifier.is_some()
            || self.title.is_some()
            || self.description.is_some()
            || matches!(&self.value, SemanticValue::Text(value) if !value.is_empty())
    }
}

#[derive(Debug)]
struct Snapshot {
    generation: u64,
    pid: i32,
    witness: (u64, u64),
    window: Cf,
    nodes: Vec<Node>,
    complete: bool,
}

/// 관찰 그래프와 새 전체 census를 묶는다. `CFEqual`은 후보를 다시 찾는 조건 하나일 뿐이다.
/// 이 순수 검사는 `AXParent`를 읽는 별도 검사와 함께 사용한다.
fn validate_observed_graph(
    snapshot: &Snapshot,
    node: &Node,
    current: &[Node],
    compare_target_value: bool,
) -> Result<(), Refusal> {
    let expected = node
        .identity
        .as_ref()
        .ok_or_else(|| Refusal::new("identity_incomplete", "observed identity is unreadable"))?;
    if !snapshot.complete
        || !expected.has_identity()
        || (!compare_target_value
            && expected.identifier.is_none()
            && expected.title.is_none()
            && expected.description.is_none())
    {
        return Err(Refusal::new(
            "identity_ambiguous",
            "complete unique semantic identity is required; observe again",
        ));
    }
    if snapshot
        .nodes
        .iter()
        .chain(current)
        .any(|n| n.identity.is_none())
    {
        return Err(Refusal::new(
            "identity_incomplete",
            "current window identity census is incomplete",
        ));
    }
    for (nodes, compare_value) in [(&snapshot.nodes[..], true), (current, compare_target_value)] {
        let candidates = nodes
            .iter()
            .filter(|n| {
                n.identity
                    .as_ref()
                    .is_some_and(|id| expected.same_candidate(id, compare_value))
            })
            .count();
        if candidates != 1 {
            return Err(Refusal::new(
                "identity_ambiguous",
                "target semantic identity is not unique in the observed window",
            ));
        }
    }
    let mut observed = node;
    let mut actual = current
        .iter()
        .find(|n| n.element == observed.element)
        .ok_or_else(|| {
            Refusal::new(
                "element_detached",
                "target is not in the observed window tree",
            )
        })?;
    for _ in 0..MAX_ANCESTRY {
        let identity = actual
            .identity
            .as_ref()
            .ok_or_else(|| Refusal::new("identity_incomplete", "current identity is unreadable"))?;
        if !observed
            .identity
            .as_ref()
            .is_some_and(|id| id.matches(identity, compare_target_value))
        {
            return Err(Refusal::new(
                "element_changed",
                "semantic identity changed since observation",
            ));
        }
        match (observed.parent, actual.parent) {
            (None, None) if actual.element == snapshot.window => return Ok(()),
            (Some(expected), Some(parent)) => {
                let expected = snapshot.nodes.get(expected).ok_or_else(|| {
                    Refusal::new("identity_incomplete", "observed parent is missing")
                })?;
                let parent = current.get(parent).ok_or_else(|| {
                    Refusal::new("identity_incomplete", "current parent is missing")
                })?;
                if expected.element != parent.element {
                    return Err(Refusal::new(
                        "element_reparented",
                        "element moved to another parent; observe again",
                    ));
                }
                observed = expected;
                actual = parent;
            }
            _ => {
                return Err(Refusal::new(
                    "element_reparented",
                    "element ancestry no longer ends at the observed window",
                ));
            }
        }
    }
    Err(Refusal::new(
        "identity_incomplete",
        "ancestry exceeds membership bounds",
    ))
}

#[derive(Debug)]
pub enum Prepared {
    Action {
        target: Target,
        element: Cf,
        action: Cf,
    },
    SetValue {
        target: Target,
        element: Cf,
        value: Cf,
        text: String,
        verify: bool,
    },
    Click {
        target: Target,
        at: ffi::CgPoint,
        down: ffi::Event,
        up: ffi::Event,
    },
    TypeText {
        target: Target,
        chunks: Vec<(ffi::Event, ffi::Event)>,
    },
}

#[derive(Debug)]
pub struct MacBackend {
    identity: Identity,
    ax: Option<(Attrs, Cf)>,
    snapshots: VecDeque<Snapshot>,
    next_generation: u64,
    protected: Vec<i32>,
}

fn ax_refusal(error: AxError, what: &str) -> Refusal {
    match error {
        ax::API_DISABLED => Refusal::new(
            "accessibility_not_granted",
            "this worker process is not trusted for Accessibility",
        ),
        ax::CANNOT_COMPLETE => Refusal::new("ax_timeout", format!("{what}: target did not answer")),
        ax::INVALID_ELEMENT => Refusal::new("element_gone", format!("{what}: element is gone")),
        _ => Refusal::new("ax_error", format!("{what}: {}", ffi::ax_error_name(error))),
    }
}

fn parse_snapshot_id(id: &str) -> Option<(&str, &str, u64)> {
    let mut parts = id.split('/');
    let epoch = parts.next()?;
    let context = parts.next()?;
    let generation = parts.next()?.parse().ok()?;
    parts
        .next()
        .is_none()
        .then_some((epoch, context, generation))
}

fn parse_element_key(key: &str) -> Option<usize> {
    let digits = key.strip_prefix('e')?;
    if digits.is_empty()
        || !digits.bytes().all(|b| b.is_ascii_digit())
        || (digits.len() > 1 && digits.starts_with('0'))
    {
        return None;
    }
    digits.parse().ok()
}

impl MacBackend {
    pub fn new(identity: Identity, protected: Vec<i32>) -> Self {
        let ax = Attrs::new().zip(ffi::system_wide());
        Self {
            identity,
            ax,
            snapshots: VecDeque::new(),
            next_generation: 1,
            protected,
        }
    }

    fn ax(&self) -> Result<(&Attrs, &Cf), Refusal> {
        self.ax
            .as_ref()
            .map(|(attrs, system)| (attrs, system))
            .ok_or_else(|| Refusal::new("unsupported", "accessibility runtime is unavailable"))
    }

    /// 남은 예산으로 이 프로세스의 AX 호출 상한을 정한다.
    fn budget(&self, deadline: Instant) -> Result<(), Refusal> {
        let (_, system) = self.ax()?;
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining <= MARGIN {
            return Err(Refusal::new(
                "deadline_exceeded",
                "not enough time left for an accessibility call",
            ));
        }
        ffi::set_messaging_timeout(system, remaining.min(AX_CALL_CAP).as_secs_f32());
        Ok(())
    }

    fn require_trust() -> Result<(), Refusal> {
        if permissions::probe_accessibility() {
            Ok(())
        } else {
            Err(Refusal::new(
                "accessibility_not_granted",
                "this worker process is not trusted for Accessibility",
            ))
        }
    }

    /// 실행기·부모(제어 프로세스)·지정 PID·Krill 번들은 대상이 될 수 없다.
    fn guard(&self, pid: i32) -> Result<(), Refusal> {
        let me = i32::try_from(std::process::id()).unwrap_or(-1);
        let parent = rustix::process::Pid::as_raw(rustix::process::getppid());
        if pid == me || pid == parent || self.protected.contains(&pid) {
            return Err(Refusal::new(
                "self_target",
                "refusing to drive the executor or its controller",
            ));
        }
        if ffi::bundle_id_of(pid).is_some_and(|id| {
            id == krill_proto::daemon::BUNDLE_ID || id.starts_with(SELF_BUNDLE_PREFIX)
        }) {
            return Err(Refusal::new(
                "self_target",
                "refusing to drive Krill itself",
            ));
        }
        Ok(())
    }

    fn role_of(&self, element: &Cf) -> Result<String, AxError> {
        let (attrs, _) = self.ax().map_err(|_| ax::FAILURE)?;
        let value = ffi::copy_attribute(element, &attrs.role)?;
        Ok(ffi::string_value(&value, LABEL_UTF16)
            .map(|(role, _)| role)
            .unwrap_or_default())
    }

    /// `AXWindows`. 비어 있으면 앱 자식 중 `AXWindow` 역할만(앱 자신 제외) 쓴다.
    fn windows(&self, app: &Cf, deadline: Instant) -> Result<Vec<Cf>, Refusal> {
        let (attrs, _) = self.ax()?;
        self.budget(deadline)?;
        match ffi::attribute_count(app, &attrs.windows) {
            Ok(count) if count > MAX_WINDOWS => {
                return Err(Refusal::new(
                    "identity_incomplete",
                    "window list exceeds receiver bounds",
                ));
            }
            Ok(0) | Err(ax::NO_VALUE | ax::ATTRIBUTE_UNSUPPORTED | ax::NOT_IMPLEMENTED) => {}
            Ok(count) => {
                self.budget(deadline)?;
                let list = ffi::copy_attribute_values(app, &attrs.windows, count)
                    .map_err(|error| ax_refusal(error, "windows"))?;
                if !list.is_empty() {
                    return Ok(list);
                }
            }
            Err(error) => return Err(ax_refusal(error, "windows")),
        }
        self.budget(deadline)?;
        let count = ffi::attribute_count(app, &attrs.children)
            .map_err(|error| ax_refusal(error, "app children"))?;
        if count > MAX_WINDOWS {
            return Err(Refusal::new(
                "identity_incomplete",
                "app child list exceeds window membership bounds",
            ));
        }
        if count == 0 {
            return Ok(Vec::new());
        }
        self.budget(deadline)?;
        let children = ffi::copy_attribute_values(app, &attrs.children, count)
            .map_err(|error| ax_refusal(error, "app children"))?;
        let mut windows = Vec::new();
        for child in children {
            if child != *app {
                self.budget(deadline)?;
                let role = self
                    .role_of(&child)
                    .map_err(|error| ax_refusal(error, "window role"))?;
                if role == "AXWindow" {
                    windows.push(child);
                }
            }
        }
        Ok(windows)
    }

    /// `start`가 `target`이거나 그 자손인지(`AXParent`를 따라 올라간다).
    fn within(&self, target: &Cf, start: &Cf, deadline: Instant) -> Result<bool, Refusal> {
        let (attrs, _) = self.ax()?;
        let mut current = start.clone();
        for _ in 0..MAX_ANCESTRY {
            if current == *target {
                return Ok(true);
            }
            self.budget(deadline)?;
            match ffi::copy_attribute(&current, &attrs.parent) {
                Ok(parent) if parent != current => current = parent,
                Ok(_) | Err(ax::NO_VALUE | ax::ATTRIBUTE_UNSUPPORTED) => return Ok(false),
                Err(error) => return Err(ax_refusal(error, "hit-test ancestry")),
            }
        }
        Ok(false)
    }

    fn permissions_data() -> Value {
        json!({
            "subject": {
                "pid": std::process::id(),
                "parentPid": rustix::process::Pid::as_raw(rustix::process::getppid()),
                "bundleId": permissions::bundle_id(),
            },
            "accessibility": permissions::probe_accessibility(),
            "screenCapture": permissions::probe_screen_recording(),
            "postEvent": ffi::post_event_preflight(),
            "inheritance": "not_assumed",
            "note": "probes describe this worker process's own TCC subject; grants seen by Krill.app or a terminal are not assumed to apply",
        })
    }

    fn apps_data(&self) -> Value {
        ffi::pump_run_loop();
        let apps: Vec<Value> = ffi::running_apps(MAX_APPS)
            .into_iter()
            .map(|app| {
                json!({
                    "pid": app.pid,
                    "name": app.name,
                    "bundleId": app.bundle_id,
                    "active": app.active,
                    "hidden": app.hidden,
                    "protected": self.guard(app.pid).is_err(),
                    "startWitness": ffi::process_start(app.pid).map(|(s, us)| format!("{s}.{us:06}")),
                })
            })
            .collect();
        json!({ "apps": apps, "freshness": "unverified_outside_appkit_run_loop" })
    }

    fn observe(
        &mut self,
        pid: i32,
        window: Option<usize>,
        limit: Option<usize>,
        deadline: Instant,
    ) -> Result<ReadResult, Refusal> {
        Self::require_trust()?;
        self.guard(pid)?;
        let witness = ffi::process_start(pid)
            .ok_or_else(|| Refusal::new("no_such_process", "target process is not running"))?;
        self.budget(deadline)?;
        let app = ffi::application(pid)
            .ok_or_else(|| Refusal::new("ax_error", "could not create application element"))?;
        let windows = self.windows(&app, deadline)?;
        let (index, root) = if let Some(index) = window {
            let root = windows.get(index).cloned().ok_or_else(|| {
                Refusal::new(
                    "no_such_window",
                    format!(
                        "window {index} not found; app has {} windows",
                        windows.len()
                    ),
                )
            })?;
            (Some(index), root)
        } else {
            let (attrs, _) = self.ax()?;
            let focused = match ffi::copy_attribute(&app, &attrs.focused_window) {
                Ok(focused) => focused,
                Err(ax::NO_VALUE | ax::ATTRIBUTE_UNSUPPORTED) => {
                    return Err(Refusal::new(
                        "no_focused_window",
                        format!(
                            "app has no focused window; pass an explicit window index (app has {} windows)",
                            windows.len()
                        ),
                    ));
                }
                Err(error) => return Err(ax_refusal(error, "focused window")),
            };
            (windows.iter().position(|w| *w == focused), focused)
        };
        let limit = limit.unwrap_or(DEFAULT_OBSERVE_NODES);
        let (nodes, elements, coverage) = self.walk(&root, limit, deadline)?;
        let generation = self.next_generation;
        self.next_generation += 1;
        let id = format!(
            "{}/{}/{generation}",
            self.identity.epoch, self.identity.context
        );
        self.snapshots.push_back(Snapshot {
            generation,
            pid,
            witness,
            window: root,
            nodes,
            complete: !coverage.truncated,
        });
        while self.snapshots.len() > MAX_SNAPSHOTS {
            self.snapshots.pop_front();
        }
        Ok(ReadResult {
            observation: Observation::Observed {
                snapshot: id.clone(),
                coverage,
            },
            data: json!({
                "snapshot": id,
                "app": {
                    "pid": pid,
                    "startWitness": format!("{}.{:06}", witness.0, witness.1),
                    "bundleId": ffi::bundle_id_of(pid),
                },
                "window": { "index": index },
                "elements": elements,
            }),
        })
    }

    /// 창에서 시작하는 전위 순회. 노드·깊이·자식·바이트·시간 상한과 방문 집합(`CFEqual`)으로 순환을 끊는다.
    fn walk(
        &self,
        root: &Cf,
        limit: usize,
        deadline: Instant,
    ) -> Result<(Vec<Node>, Vec<Value>, Coverage), Refusal> {
        let (attrs, _) = self.ax()?;
        let mut nodes: Vec<Node> = Vec::new();
        let mut elements: Vec<Value> = Vec::new();
        let mut coverage = Coverage::default();
        let mut visited: HashMap<usize, Vec<usize>> = HashMap::new();
        let mut bytes = 0usize;
        let mut stack: Vec<(Cf, Option<usize>, usize)> = vec![(root.clone(), None, 0)];
        let mut root_error = None;
        while let Some((element, parent, depth)) = stack.pop() {
            if nodes.len() >= limit {
                coverage.truncate("nodes");
                break;
            }
            if Instant::now() + MARGIN >= deadline {
                coverage.truncate("time");
                break;
            }
            let hash = element.hash_code();
            if visited
                .get(&hash)
                .is_some_and(|seen| seen.iter().any(|&i| nodes[i].element == element))
            {
                coverage.truncate("cycle");
                continue;
            }
            self.budget(deadline)?;
            let values = match ffi::copy_multiple(&element, &attrs.batch) {
                Ok(values) => values,
                Err(error) => {
                    if depth == 0 {
                        root_error = Some(error);
                    }
                    coverage.truncate(match error {
                        ax::CANNOT_COMPLETE => "ax_timeout",
                        ax::INVALID_ELEMENT => "element_gone",
                        _ => "ax_error",
                    });
                    continue;
                }
            };
            let get = |index: usize| values.get(index).and_then(Option::as_ref);
            let text =
                |index: usize, max: usize| get(index).and_then(|v| ffi::string_value(v, max));
            let role = text(B_ROLE, LABEL_UTF16)
                .map(|(role, _)| role)
                .unwrap_or_default();
            // 실제 앱에서 창 아래 자식이 다시 AXApplication으로 돌아오는 순환이 관찰됐다.
            if depth > 0 && role == "AXApplication" {
                coverage.truncate("cycle");
                continue;
            }
            let key = nodes.len();
            let mut node = json!({
                "key": format!("e{key}"),
                "parent": parent.map(|p| format!("e{p}")),
                "depth": depth,
                "role": role,
            });
            for (field, index) in [
                ("subrole", B_SUBROLE),
                ("title", B_TITLE),
                ("description", B_DESCRIPTION),
                ("identifier", B_IDENTIFIER),
            ] {
                if let Some((value, _)) = text(index, LABEL_UTF16).filter(|(v, _)| !v.is_empty()) {
                    node[field] = json!(value);
                }
            }
            if let Some(value) = get(B_VALUE) {
                if let Some((preview, truncated)) = ffi::string_value(value, PREVIEW_UTF16) {
                    node["value"] = json!(preview);
                    if truncated {
                        node["valueTruncated"] = json!(true);
                    }
                } else if let Some(number) = ffi::number_value(value) {
                    node["value"] = json!(number);
                } else if let Some(flag) = ffi::bool_value(value) {
                    node["value"] = json!(flag);
                }
                self.budget(deadline)?;
                if let Ok(settable) = ffi::is_settable(&element, &attrs.value) {
                    node["valueSettable"] = json!(settable);
                }
            }
            for (field, index) in [("enabled", B_ENABLED), ("focused", B_FOCUSED)] {
                if let Some(flag) = get(index).and_then(ffi::bool_value) {
                    node[field] = json!(flag);
                }
            }
            if let (Some(origin), Some(size)) = (
                get(B_POSITION).and_then(ffi::point_value),
                get(B_SIZE).and_then(ffi::size_value),
            ) {
                node["frame"] = json!({
                    "x": origin.x, "y": origin.y, "width": size.width, "height": size.height,
                });
            }
            self.budget(deadline)?;
            if let Ok(actions) = ffi::action_names(&element, MAX_ACTIONS)
                && !actions.is_empty()
            {
                node["actions"] = json!(actions);
            }
            if depth >= MAX_DEPTH {
                node["unexpanded"] = json!(true);
                coverage.truncate("depth");
            } else {
                self.budget(deadline)?;
                match ffi::attribute_count(&element, &attrs.children) {
                    Ok(0) | Err(ax::NO_VALUE | ax::ATTRIBUTE_UNSUPPORTED) => {}
                    Ok(count) => {
                        let take = count.min(MAX_CHILDREN);
                        self.budget(deadline)?;
                        if let Ok(children) =
                            ffi::copy_attribute_values(&element, &attrs.children, take)
                        {
                            node["childCount"] = json!(count);
                            if count > take {
                                node["childrenTruncated"] = json!(true);
                                coverage.truncate("children");
                            }
                            for child in children.into_iter().rev() {
                                stack.push((child, Some(key), depth + 1));
                            }
                        } else {
                            node["unexpanded"] = json!(true);
                            coverage.truncate("ax_error");
                        }
                    }
                    Err(_) => {
                        node["unexpanded"] = json!(true);
                        coverage.truncate("ax_error");
                    }
                }
            }
            let identity = Semantic::from_values(&values);
            if identity.is_none() {
                node["inputRefusal"] = json!("identity_incomplete");
            }
            bytes += serde_json::to_vec(&node).map_or(0, |b| b.len())
                + identity.as_ref().map_or(0, Semantic::bytes);
            visited.entry(hash).or_default().push(key);
            nodes.push(Node {
                element,
                parent,
                identity,
            });
            elements.push(node);
            if bytes > MAX_OBSERVE_BYTES {
                coverage.truncate("bytes");
                break;
            }
        }
        if !stack.is_empty() && !coverage.truncated {
            coverage.truncate("nodes");
        }
        if nodes.is_empty() {
            return Err(root_error.map_or_else(
                || Refusal::new("observe_failed", "window could not be read"),
                |error| ax_refusal(error, "window"),
            ));
        }
        coverage.nodes = nodes.len();
        Ok((nodes, elements, coverage))
    }

    fn resolve(&self, target: &Target) -> Result<(&Snapshot, &Node), Refusal> {
        let (epoch, context, generation) = parse_snapshot_id(&target.snapshot)
            .ok_or_else(|| Refusal::new("invalid_target", "snapshot id is malformed"))?;
        if epoch != self.identity.epoch {
            return Err(Refusal::new(
                "stale_epoch",
                "snapshot belongs to another executor instance; observe again",
            ));
        }
        if context != self.identity.context {
            return Err(Refusal::new(
                "foreign_context",
                "snapshot was issued to another caller",
            ));
        }
        let snapshot = self
            .snapshots
            .iter()
            .find(|s| s.generation == generation)
            .ok_or_else(|| {
                Refusal::new(
                    "stale_snapshot",
                    "snapshot was evicted or never issued; observe again",
                )
            })?;
        let node = parse_element_key(&target.element)
            .and_then(|index| snapshot.nodes.get(index))
            .ok_or_else(|| Refusal::new("unknown_element", "element is not in that snapshot"))?;
        Ok((snapshot, node))
    }

    /// 전체 창을 다시 읽어 의미 지문·유일성·실제 양방향 부모 경로를 확인한다.
    fn revalidate(
        &self,
        snapshot: &Snapshot,
        node: &Node,
        deadline: Instant,
    ) -> Result<Cf, Refusal> {
        let app = self.validate_instance(snapshot, deadline)?;
        self.validate_chunk_identity(snapshot, node, true, deadline)?;
        self.validate_path(snapshot, node, true, deadline)?;
        Ok(app)
    }

    fn validate_chunk_identity(
        &self,
        snapshot: &Snapshot,
        node: &Node,
        compare_value: bool,
        deadline: Instant,
    ) -> Result<(), Refusal> {
        let (current, _, coverage) = self.walk(&snapshot.window, MAX_OBSERVE_NODES, deadline)?;
        if coverage.truncated {
            return Err(Refusal::new(
                "identity_incomplete",
                "current window identity census is incomplete",
            ));
        }
        validate_observed_graph(snapshot, node, &current, compare_value)
    }

    fn validate_instance(&self, snapshot: &Snapshot, deadline: Instant) -> Result<Cf, Refusal> {
        self.guard(snapshot.pid)?;
        if ffi::process_start(snapshot.pid) != Some(snapshot.witness) {
            return Err(Refusal::new(
                "app_instance_changed",
                "target process exited or its pid was reused",
            ));
        }
        self.budget(deadline)?;
        let app = ffi::application(snapshot.pid)
            .ok_or_else(|| Refusal::new("ax_error", "could not create application element"))?;
        if !self.windows(&app, deadline)?.contains(&snapshot.window) {
            return Err(Refusal::new(
                "window_gone",
                "observed window is no longer open",
            ));
        }
        Ok(app)
    }

    /// 관찰한 부모 순서에 `AXParent`와 `AXChildren` 양쪽으로 속해야 한다.
    /// 값 비교 면제는 한 `type_text` 요청에서 이미 문자를 발행한 뒤뿐이다. 저장 스냅샷은 바꾸지 않는다.
    fn validate_path(
        &self,
        snapshot: &Snapshot,
        node: &Node,
        compare_value: bool,
        deadline: Instant,
    ) -> Result<(), Refusal> {
        let (attrs, _) = self.ax()?;
        let mut current = node;
        for _ in 0..MAX_ANCESTRY {
            self.budget(deadline)?;
            if ffi::element_pid(&current.element) != Some(snapshot.pid) {
                return Err(Refusal::new(
                    "element_changed",
                    "element process identity changed",
                ));
            }
            let values = ffi::copy_multiple(&current.element, &attrs.batch)
                .map_err(|error| ax_refusal(error, "identity"))?;
            let identity = Semantic::from_values(&values).ok_or_else(|| {
                Refusal::new(
                    "identity_incomplete",
                    "current identity cannot be read in full",
                )
            })?;
            let expected = current.identity.as_ref().ok_or_else(|| {
                Refusal::new(
                    "identity_incomplete",
                    "observed ancestor identity is incomplete",
                )
            })?;
            if !expected.matches(&identity, compare_value) {
                return Err(Refusal::new(
                    "element_changed",
                    "element semantic, value or geometry changed since observation",
                ));
            }
            let Some(parent_index) = current.parent else {
                if current.element == snapshot.window {
                    return Ok(());
                }
                return Err(Refusal::new(
                    "element_detached",
                    "observed ancestry does not end at the window",
                ));
            };
            let parent = snapshot
                .nodes
                .get(parent_index)
                .ok_or_else(|| Refusal::new("identity_incomplete", "observed parent is missing"))?;
            self.budget(deadline)?;
            let actual_parent = ffi::copy_attribute(&current.element, &attrs.parent)
                .map_err(|error| ax_refusal(error, "parent"))?;
            if actual_parent != parent.element {
                return Err(Refusal::new(
                    "element_reparented",
                    "element moved to another parent; observe again",
                ));
            }
            self.budget(deadline)?;
            let count = ffi::attribute_count(&parent.element, &attrs.children)
                .map_err(|error| ax_refusal(error, "parent children"))?;
            if count > MAX_CHILDREN {
                return Err(Refusal::new(
                    "identity_incomplete",
                    "parent children exceed membership bounds",
                ));
            }
            let children = if count == 0 {
                Vec::new()
            } else {
                ffi::copy_attribute_values(&parent.element, &attrs.children, count)
                    .map_err(|error| ax_refusal(error, "parent children"))?
            };
            if !children.contains(&current.element) {
                return Err(Refusal::new(
                    "element_detached",
                    "parent no longer contains the observed element",
                ));
            }
            current = parent;
        }
        Err(Refusal::new(
            "identity_incomplete",
            "ancestry exceeds membership bounds",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_and_element_ids_are_strict() {
        assert_eq!(parse_snapshot_id("E/C/3"), Some(("E", "C", 3)));
        for bad in ["E/C", "E/C/x", "E/C/3/4", "", "E/C/-1"] {
            assert_eq!(parse_snapshot_id(bad), None, "{bad}");
        }
        assert_eq!(parse_element_key("e0"), Some(0));
        assert_eq!(parse_element_key("e12"), Some(12));
        for bad in ["", "e", "e01", "x1", "e-1", "e+1", "1"] {
            assert_eq!(parse_element_key(bad), None, "{bad}");
        }
    }

    fn backend() -> MacBackend {
        MacBackend::new(
            Identity {
                epoch: "E".into(),
                context: "C".into(),
            },
            Vec::new(),
        )
    }

    #[test]
    fn stale_or_foreign_references_are_refused_before_any_ax_call() {
        let backend = backend();
        let target = |snapshot: &str, element: &str| Target {
            snapshot: snapshot.into(),
            element: element.into(),
        };
        let code = |t: Target| backend.resolve(&t).unwrap_err().code;
        assert_eq!(code(target("OLD/C/1", "e0")), "stale_epoch");
        assert_eq!(code(target("E/OTHER/1", "e0")), "foreign_context");
        assert_eq!(code(target("E/C/1", "e0")), "stale_snapshot");
        assert_eq!(code(target("E/C", "e0")), "invalid_target");
    }

    #[test]
    fn executor_and_parent_are_never_targets() {
        let backend = backend();
        let me = i32::try_from(std::process::id()).unwrap();
        assert_eq!(backend.guard(me).unwrap_err().code, "self_target");
        let parent = rustix::process::Pid::as_raw(rustix::process::getppid());
        assert_eq!(backend.guard(parent).unwrap_err().code, "self_target");
        let protected = MacBackend::new(
            Identity {
                epoch: "E".into(),
                context: "C".into(),
            },
            vec![4242],
        );
        assert_eq!(protected.guard(4242).unwrap_err().code, "self_target");
    }

    #[test]
    fn unsupported_list_is_disjoint_from_capabilities() {
        for name in UNSUPPORTED {
            assert!(!CAPABILITIES.contains(&name), "{name}");
        }
    }
}

#[cfg(test)]
#[path = "ax/identity_tests.rs"]
mod identity_tests;
