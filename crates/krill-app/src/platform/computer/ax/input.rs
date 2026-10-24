//! 입력 경로: 실행 직전 재검증 뒤의 AX 동작·값 쓰기·`CGEvent` 클릭/타이핑과 결과 축 판정.

use std::time::Instant;

use super::super::ffi::{self, AxError, Cf, CgPoint, ax};
use super::super::protocol::{Cleanup, Delivery, Observation, Op, Target, Verification};
use super::super::worker::{Backend, Dispatched, ReadResult, Refusal};
use super::{
    ALLOWED_ACTIONS, CAPABILITIES, MAX_ACTIONS, MacBackend, Prepared, Snapshot, TYPE_CHUNK_UTF16,
    UNSUPPORTED, ax_refusal,
};

/// AX 부작용 호출 결과를 전달 축으로 바꾼다. 시간 초과·일반 실패는 불명이며 좌표 대체나 재전송을 하지 않는다.
fn side_effect_delivery(error: AxError) -> Delivery {
    match error {
        ax::SUCCESS => Delivery::Submitted { issued_units: 1 },
        // 대상이 동작·속성을 거부했거나 클라이언트 쪽에서 막혔다: 효과 없음.
        ax::ACTION_UNSUPPORTED
        | ax::ATTRIBUTE_UNSUPPORTED
        | ax::ILLEGAL_ARGUMENT
        | ax::API_DISABLED => Delivery::NotDispatched {
            reason: ffi::ax_error_name(error).to_owned(),
        },
        ax::CANNOT_COMPLETE => Delivery::Unknown {
            reason: "ax_timeout".to_owned(),
        },
        _ => Delivery::Unknown {
            reason: ffi::ax_error_name(error).to_owned(),
        },
    }
}

/// 서로게이트 쌍을 나누지 않고 `max`단위 이하 조각으로 자른다.
fn chunk_utf16(text: &str, max: usize) -> Vec<Vec<u16>> {
    let mut chunks = Vec::new();
    let mut current = Vec::new();
    let mut units = [0u16; 2];
    for ch in text.chars() {
        let encoded = ch.encode_utf16(&mut units);
        if !current.is_empty() && current.len() + encoded.len() > max {
            chunks.push(std::mem::take(&mut current));
        }
        current.extend_from_slice(encoded);
    }
    if !current.is_empty() {
        chunks.push(current);
    }
    chunks
}

/// 각 발행 직전에 수신기를 검사한다. down 뒤 중단해도 대응 up은 내용 없는 정리로 발행한다.
/// 해제 검사 실패는 Partial이며 후속 down은 없다. `CGEventPost에는` 수신 ACK가 없다.
fn dispatch_pairs<'a, E: 'a>(
    pairs: impl IntoIterator<Item = (&'a E, &'a E)>,
    units_per_pair: u32,
    mut check: impl FnMut(bool, bool) -> Result<(), Refusal>,
    mut post: impl FnMut(&E),
) -> Dispatched {
    let mut issued = 0;
    let mut stopped = None;
    for (down, up) in pairs {
        if let Err(refusal) = check(issued == 0, false) {
            stopped = Some(refusal.code);
            break;
        }
        post(down);
        // 이미 발행한 down을 해제한다. 수신기를 강제로 재활성화하지 않는다.
        let release_check = check(false, true);
        post(up);
        issued += units_per_pair;
        if let Err(refusal) = release_check {
            stopped = Some(refusal.code);
            break;
        }
    }
    let delivery = match (issued, stopped) {
        (0, Some(reason)) => Delivery::NotDispatched {
            reason: reason.to_owned(),
        },
        (_, Some(reason)) => Delivery::Partial {
            issued_units: issued,
            reason: reason.to_owned(),
        },
        (_, None) => Delivery::Submitted {
            issued_units: issued,
        },
    };
    Dispatched {
        delivery,
        verification: Verification::NotRequested,
        cleanup: if issued == 0 {
            Cleanup::NotNeeded
        } else {
            Cleanup::Completed
        },
        data: None,
    }
}

impl MacBackend {
    fn require_post_events() -> Result<(), Refusal> {
        if ffi::post_event_preflight() {
            Ok(())
        } else {
            Err(Refusal::new(
                "post_event_not_granted",
                "this worker process may not post input events",
            ))
        }
    }

    fn require_frontmost(pid: i32) -> Result<(), Refusal> {
        let actual = ffi::foreground_pid().map_err(|status| {
            Refusal::new(
                "foreground_unavailable",
                format!("foreground process query failed: OSStatus {status}"),
            )
        })?;
        if actual == pid {
            Ok(())
        } else {
            Err(Refusal::new(
                "target_not_frontmost",
                format!(
                    "target pid {pid} is not frontmost (actual {actual}); input would go elsewhere"
                ),
            ))
        }
    }

    fn require_enabled(&self, element: &Cf) -> Result<(), Refusal> {
        let (attrs, _) = self.ax()?;
        match ffi::copy_attribute(element, &attrs.enabled) {
            Ok(value) if ffi::bool_value(&value) == Some(true) => Ok(()),
            Ok(value) if ffi::bool_value(&value) == Some(false) => {
                Err(Refusal::new("target_disabled", "target is disabled"))
            }
            Err(ax::NO_VALUE | ax::ATTRIBUTE_UNSUPPORTED | ax::NOT_IMPLEMENTED) => Ok(()),
            Ok(_) => Err(Refusal::new(
                "enabled_unknown",
                "target enabled state is unreadable",
            )),
            Err(error) => Err(ax_refusal(error, "enabled")),
        }
    }

    /// 네이티브 AX 동작도 배경 수신 계약을 입증하지 않았으므로 전경의 관찰 창만 허용한다.
    /// 모달/시트가 떠도 뒤 창은 `AXWindows에` 남는다. 창 존재 검사만으로는 입력할 수 없다.
    fn require_receiver_window(
        &self,
        snapshot: &Snapshot,
        app: &Cf,
        deadline: Instant,
    ) -> Result<(), Refusal> {
        let (attrs, _) = self.ax()?;
        self.budget(deadline)?;
        for window in self.windows(app, deadline)? {
            self.budget(deadline)?;
            if window != snapshot.window {
                match ffi::copy_attribute(&window, &attrs.modal) {
                    Ok(value) if ffi::bool_value(&value) == Some(true) => {
                        return Err(Refusal::new(
                            "modal_changed",
                            "another modal window is open; observe its receiver",
                        ));
                    }
                    Ok(value) if ffi::bool_value(&value) == Some(false) => {}
                    Err(ax::NO_VALUE | ax::ATTRIBUTE_UNSUPPORTED | ax::NOT_IMPLEMENTED) => {}
                    Ok(_) => {
                        return Err(Refusal::new("modal_unknown", "modal state is unreadable"));
                    }
                    Err(error) => return Err(ax_refusal(error, "modal")),
                }
            }
        }
        self.budget(deadline)?;
        match ffi::attribute_count(&snapshot.window, &attrs.sheets) {
            Ok(0) | Err(ax::NO_VALUE | ax::ATTRIBUTE_UNSUPPORTED | ax::NOT_IMPLEMENTED) => {}
            Ok(_) => {
                return Err(Refusal::new(
                    "modal_changed",
                    "observed window has an attached sheet; observe its receiver",
                ));
            }
            Err(error) => return Err(ax_refusal(error, "sheets")),
        }
        self.budget(deadline)?;
        let focused = ffi::copy_attribute(app, &attrs.focused_window)
            .map_err(|error| ax_refusal(error, "focused window"))?;
        if focused != snapshot.window {
            return Err(Refusal::new(
                "focused_window_changed",
                "focused window differs from the observed window",
            ));
        }
        Self::require_frontmost(snapshot.pid)
    }

    fn require_focused_element(&self, app: &Cf, element: &Cf) -> Result<(), Refusal> {
        if crate::platform::is_system_secure_event_input_enabled() {
            return Err(Refusal::new(
                "secure_event_input",
                "Secure Keyboard Entry is active",
            ));
        }
        let (attrs, _) = self.ax()?;
        let focused = ffi::copy_attribute(app, &attrs.focused_element)
            .map_err(|error| ax_refusal(error, "focused element"))?;
        // 컨테이너 자손이라는 이유로 다른 편집 필드를 수신기로 허용하지 않는다.
        if focused == *element {
            Ok(())
        } else {
            Err(Refusal::new(
                "target_not_focused",
                "keyboard receiver differs from the observed element",
            ))
        }
    }

    fn require_hit(
        &self,
        element: &Cf,
        pid: i32,
        at: CgPoint,
        deadline: Instant,
    ) -> Result<(), Refusal> {
        let (_, system) = self.ax()?;
        let hit = ffi::element_at(system, at).map_err(|error| ax_refusal(error, "hit-test"))?;
        if ffi::element_pid(&hit) == Some(pid) && self.within(element, &hit, deadline)? {
            Ok(())
        } else {
            Err(Refusal::new(
                "hit_test_mismatch",
                "another element is on top of the target's center",
            ))
        }
    }

    fn prepare_click(
        &self,
        target: &Target,
        element: &Cf,
        pid: i32,
        deadline: Instant,
    ) -> Result<Prepared, Refusal> {
        Self::require_post_events()?;
        let (attrs, _) = self.ax()?;
        let origin = ffi::copy_attribute(element, &attrs.position)
            .map_err(|error| ax_refusal(error, "position"))?;
        let size =
            ffi::copy_attribute(element, &attrs.size).map_err(|error| ax_refusal(error, "size"))?;
        let (Some(origin), Some(size)) = (ffi::point_value(&origin), ffi::size_value(&size)) else {
            return Err(Refusal::new("no_frame", "element has no on-screen frame"));
        };
        let at = CgPoint {
            x: origin.x + size.width / 2.0,
            y: origin.y + size.height / 2.0,
        };
        if !(at.x.is_finite() && at.y.is_finite() && size.width > 0.0 && size.height > 0.0) {
            return Err(Refusal::new("no_frame", "element frame is empty"));
        }
        self.require_hit(element, pid, at, deadline)?;
        let (Some(down), Some(up)) = (ffi::mouse_event(true, at), ffi::mouse_event(false, at))
        else {
            return Err(Refusal::new(
                "event_create_failed",
                "could not create mouse events",
            ));
        };
        Ok(Prepared::Click {
            target: target.clone(),
            at,
            down,
            up,
        })
    }

    fn prepare_type(
        &self,
        target: &Target,
        element: &Cf,
        app: &Cf,
        text: &str,
    ) -> Result<Prepared, Refusal> {
        Self::require_post_events()?;
        let role = self
            .role_of(element)
            .map_err(|error| ax_refusal(error, "keyboard receiver role"))?;
        if !matches!(role.as_str(), "AXTextField" | "AXTextArea" | "AXComboBox") {
            return Err(Refusal::new(
                "not_text_receiver",
                "typing requires an observed editable text receiver",
            ));
        }
        if text.chars().any(char::is_control) {
            return Err(Refusal::new(
                "control_text",
                "control characters require a value write or explicit key operation",
            ));
        }
        self.require_focused_element(app, element)?;
        let mut chunks = Vec::new();
        for units in chunk_utf16(text, TYPE_CHUNK_UTF16) {
            let (Some(down), Some(up)) = (
                ffi::unicode_key_event(true, &units),
                ffi::unicode_key_event(false, &[]),
            ) else {
                return Err(Refusal::new(
                    "event_create_failed",
                    "could not create key events",
                ));
            };
            chunks.push((down, up));
        }
        Ok(Prepared::TypeText {
            target: target.clone(),
            chunks,
        })
    }

    /// `CGEvent` 각각의 직전에도 실행한다. 내용은 down에만 싣고 up은 대응하는 해제다.
    fn event_receiver(
        &self,
        target: &Target,
        at: Option<CgPoint>,
        compare_value: bool,
        release: bool,
        deadline: Instant,
    ) -> Result<(), Refusal> {
        Self::require_trust()?;
        Self::require_post_events()?;
        let (snapshot, node) = self.resolve(target)?;
        let app = self.validate_instance(snapshot, deadline)?;
        if !release {
            // 각 내용 청크도 유일성을 다시 검사한다. 앞 청크가 쓴 값은 현재 census에서만 제외하며
            // 저장한 관찰은 갱신하지 않는다. 해제는 별도의 읽기/정리 검사 뒤 반드시 발행한다.
            self.validate_chunk_identity(snapshot, node, compare_value, deadline)?;
        }
        self.validate_path(snapshot, node, compare_value, deadline)?;
        self.require_enabled(&node.element)?;
        self.require_receiver_window(snapshot, &app, deadline)?;
        self.budget(deadline)?;
        if let Some(at) = at {
            self.require_hit(&node.element, snapshot.pid, at, deadline)?;
        } else {
            self.require_focused_element(&app, &node.element)?;
        }
        // 중간 AX IPC 동안 전환할 수 있으므로 마지막에 창·전경을 다시 본다. 원자적이지 않다.
        self.budget(deadline)?;
        let (attrs, _) = self.ax()?;
        let focused_window = ffi::copy_attribute(&app, &attrs.focused_window)
            .map_err(|error| ax_refusal(error, "focused window"))?;
        if focused_window != snapshot.window {
            return Err(Refusal::new(
                "focused_window_changed",
                "focused window changed before event emission",
            ));
        }
        Self::require_frontmost(snapshot.pid)?;
        self.budget(deadline)
    }

    fn verify_value(
        &self,
        target: &Target,
        element: &Cf,
        expected: &str,
        deadline: Instant,
    ) -> Verification {
        let predicate = "target_value_equals".to_owned();
        let identity = || -> Result<(), Refusal> {
            let (snapshot, node) = self.resolve(target)?;
            self.validate_instance(snapshot, deadline)?;
            self.validate_path(snapshot, node, false, deadline)?;
            self.budget(deadline)
        };
        if let Err(refusal) = identity() {
            return Verification::Unknown {
                reason: refusal.code.to_owned(),
            };
        }
        let Ok((attrs, _)) = self.ax() else {
            return Verification::Unknown {
                reason: "unsupported".to_owned(),
            };
        };
        // 전용 조회: 관찰의 500자 미리보기가 아니라 기대값 길이+1까지 읽어 더 긴 값도 가린다.
        let limit = expected.encode_utf16().count() + 1;
        match ffi::copy_attribute(element, &attrs.value) {
            Ok(value) => match ffi::string_value(&value, limit) {
                Some((actual, false)) if actual == expected => {
                    Verification::Satisfied { predicate }
                }
                Some(_) => Verification::Unsatisfied { predicate },
                None => Verification::Unknown {
                    reason: "value_not_text".to_owned(),
                },
            },
            Err(ax::NO_VALUE) if expected.is_empty() => Verification::Satisfied { predicate },
            Err(error) => Verification::Unknown {
                reason: ffi::ax_error_name(error).to_owned(),
            },
        }
    }
}

impl Backend for MacBackend {
    type Prepared = Prepared;

    fn supported(&self) -> bool {
        self.ax.is_some()
    }

    fn capabilities(&self) -> Vec<&'static str> {
        if self.supported() {
            CAPABILITIES.to_vec()
        } else {
            vec!["capabilities"]
        }
    }

    fn unsupported(&self) -> Vec<&'static str> {
        UNSUPPORTED.to_vec()
    }

    fn read(&mut self, op: &Op, deadline: Instant) -> Result<ReadResult, Refusal> {
        self.ax()?;
        let data = match op {
            Op::Permissions => Self::permissions_data(),
            Op::Apps => self.apps_data(),
            Op::Observe { pid, window, limit } => {
                return self.observe(*pid, *window, *limit, deadline);
            }
            _ => return Err(Refusal::new("unsupported", "not a read operation")),
        };
        Ok(ReadResult {
            observation: Observation::NotRequested,
            data,
        })
    }

    fn prepare(&mut self, op: &Op, deadline: Instant) -> Result<Prepared, Refusal> {
        Self::require_trust()?;
        self.budget(deadline)?;
        let (Op::Action { target, .. }
        | Op::SetValue { target, .. }
        | Op::Click { target }
        | Op::TypeText { target, .. }) = op
        else {
            return Err(Refusal::new("unsupported", "not an input operation"));
        };
        if let Op::Action { name, .. } = op
            && !ALLOWED_ACTIONS.contains(&name.as_str())
        {
            return Err(Refusal::new(
                "action_not_allowed",
                "action is not in the worker's allowlist",
            ));
        }
        let (snapshot, node) = self.resolve(target)?;
        let app = self.revalidate(snapshot, node, deadline)?;
        self.require_enabled(&node.element)?;
        self.require_receiver_window(snapshot, &app, deadline)?;
        let element = node.element.clone();
        let pid = snapshot.pid;
        let (attrs, _) = self.ax()?;
        match op {
            Op::Action { name, .. } => {
                let names = ffi::action_names(&element, MAX_ACTIONS)
                    .map_err(|error| ax_refusal(error, "actions"))?;
                if !names.iter().any(|n| n == name) {
                    return Err(Refusal::new(
                        "action_unsupported",
                        "element does not offer that action now",
                    ));
                }
                let action = ffi::cf_string(name)
                    .ok_or_else(|| Refusal::new("invalid_request", "bad action name"))?;
                Ok(Prepared::Action {
                    target: target.clone(),
                    element,
                    action,
                })
            }
            Op::SetValue { text, verify, .. } => {
                if !ffi::is_settable(&element, &attrs.value)
                    .map_err(|error| ax_refusal(error, "value"))?
                {
                    return Err(Refusal::new(
                        "value_not_settable",
                        "element value is not settable",
                    ));
                }
                let value = ffi::cf_string(text)
                    .ok_or_else(|| Refusal::new("invalid_request", "text is not encodable"))?;
                Ok(Prepared::SetValue {
                    target: target.clone(),
                    element,
                    value,
                    text: text.clone(),
                    verify: *verify,
                })
            }
            Op::Click { .. } => self.prepare_click(target, &element, pid, deadline),
            Op::TypeText { text, .. } => self.prepare_type(target, &element, &app, text),
            _ => Err(Refusal::new("unsupported", "not an input operation")),
        }
    }

    fn dispatch(&mut self, prepared: Prepared, deadline: Instant) -> Dispatched {
        let simple = |delivery| Dispatched {
            delivery,
            verification: Verification::NotRequested,
            cleanup: Cleanup::NotNeeded,
            data: None,
        };
        // prepare와 dispatch 사이에도 수신기가 바뀔 수 있어 전체를 다시 검증한다.
        let target = match &prepared {
            Prepared::Action { target, .. }
            | Prepared::SetValue { target, .. }
            | Prepared::Click { target, .. }
            | Prepared::TypeText { target, .. } => target,
        };
        let validate = || -> Result<(), Refusal> {
            Self::require_trust()?;
            let (snapshot, node) = self.resolve(target)?;
            let app = self.revalidate(snapshot, node, deadline)?;
            self.require_enabled(&node.element)?;
            self.require_receiver_window(snapshot, &app, deadline)
        };
        if let Err(refusal) = validate() {
            return simple(Delivery::NotDispatched {
                reason: refusal.code.to_owned(),
            });
        }
        match prepared {
            Prepared::Action {
                target,
                element,
                action,
            } => {
                let check = || -> Result<(), Refusal> {
                    let name = ffi::string_value(&action, 128)
                        .ok_or_else(|| Refusal::new("invalid_request", "action is not text"))?
                        .0;
                    let names = ffi::action_names(&element, MAX_ACTIONS)
                        .map_err(|error| ax_refusal(error, "actions"))?;
                    if !names.contains(&name) {
                        return Err(Refusal::new("action_unsupported", "target action changed"));
                    }
                    let (snapshot, node) = self.resolve(&target)?;
                    let app = self.validate_instance(snapshot, deadline)?;
                    self.validate_path(snapshot, node, true, deadline)?;
                    self.require_enabled(&element)?;
                    self.require_receiver_window(snapshot, &app, deadline)?;
                    self.budget(deadline)
                };
                if let Err(refusal) = check() {
                    return simple(Delivery::NotDispatched {
                        reason: refusal.code.to_owned(),
                    });
                }
                simple(side_effect_delivery(ffi::perform_action(&element, &action)))
            }
            Prepared::SetValue {
                target,
                element,
                value,
                text,
                verify,
            } => {
                let check = || -> Result<(), Refusal> {
                    let (attrs, _) = self.ax()?;
                    if !ffi::is_settable(&element, &attrs.value)
                        .map_err(|error| ax_refusal(error, "value"))?
                    {
                        return Err(Refusal::new(
                            "value_not_settable",
                            "target value is not settable",
                        ));
                    }
                    let (snapshot, node) = self.resolve(&target)?;
                    let app = self.validate_instance(snapshot, deadline)?;
                    self.validate_path(snapshot, node, true, deadline)?;
                    self.require_enabled(&element)?;
                    self.require_receiver_window(snapshot, &app, deadline)?;
                    self.budget(deadline)
                };
                if let Err(refusal) = check() {
                    return simple(Delivery::NotDispatched {
                        reason: refusal.code.to_owned(),
                    });
                }
                let Ok((attrs, _)) = self.ax() else {
                    return simple(Delivery::NotDispatched {
                        reason: "unsupported".to_owned(),
                    });
                };
                let delivery =
                    side_effect_delivery(ffi::set_attribute(&element, &attrs.value, &value));
                // 전달·검증은 독립 축이다. 불명인 AX 부작용을 좌표 입력으로 바꾸지 않는다.
                let verification = if verify && !matches!(delivery, Delivery::NotDispatched { .. })
                {
                    self.verify_value(&target, &element, &text, deadline)
                } else {
                    Verification::NotRequested
                };
                Dispatched {
                    delivery,
                    verification,
                    cleanup: Cleanup::NotNeeded,
                    data: None,
                }
            }
            Prepared::Click {
                target,
                at,
                down,
                up,
            } => dispatch_pairs(
                std::iter::once((&down, &up)),
                2,
                |compare_value, release| {
                    self.event_receiver(&target, Some(at), compare_value, release, deadline)
                },
                ffi::post,
            ),
            Prepared::TypeText { target, chunks } => dispatch_pairs(
                chunks.iter().map(|(down, up)| (down, up)),
                1,
                |compare_value, release| {
                    self.event_receiver(&target, None, compare_value, release, deadline)
                },
                ffi::post,
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunks_never_split_surrogate_pairs() {
        let text = format!("{}😀{}", "a".repeat(19), "b".repeat(3));
        let chunks = chunk_utf16(&text, 20);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].len(), 19);
        assert_eq!(chunks[1].len(), 5);
        let joined: Vec<u16> = chunks.concat();
        assert_eq!(String::from_utf16(&joined).unwrap(), text);
        assert!(chunk_utf16("", 20).is_empty());
        assert_eq!(chunk_utf16("한글", 1).len(), 2);
    }

    #[test]
    fn side_effect_errors_never_claim_success_or_allow_fallback() {
        assert_eq!(
            side_effect_delivery(ax::SUCCESS),
            Delivery::Submitted { issued_units: 1 }
        );
        assert!(matches!(
            side_effect_delivery(ax::CANNOT_COMPLETE),
            Delivery::Unknown { .. }
        ));
        assert!(matches!(
            side_effect_delivery(ax::FAILURE),
            Delivery::Unknown { .. }
        ));
        assert!(matches!(
            side_effect_delivery(ax::INVALID_ELEMENT),
            Delivery::Unknown { .. }
        ));
        assert!(matches!(
            side_effect_delivery(ax::ACTION_UNSUPPORTED),
            Delivery::NotDispatched { .. }
        ));
    }
    #[test]
    fn receiver_is_checked_before_first_event_and_each_pair_has_release_cleanup() {
        use std::cell::{Cell, RefCell};
        let trace = RefCell::new(Vec::new());
        let calls = Cell::new(0);
        let result = dispatch_pairs(
            [(&"down-1", &"up-1"), (&"down-2", &"up-2")],
            1,
            |compare_value, _release| {
                calls.set(calls.get() + 1);
                trace.borrow_mut().push(if compare_value {
                    "check-original"
                } else {
                    "check-receiver"
                });
                Ok(())
            },
            |event| trace.borrow_mut().push(*event),
        );
        assert_eq!(calls.get(), 4);
        assert_eq!(
            *trace.borrow(),
            [
                "check-original",
                "down-1",
                "check-receiver",
                "up-1",
                "check-receiver",
                "down-2",
                "check-receiver",
                "up-2"
            ]
        );
        assert_eq!(result.delivery, Delivery::Submitted { issued_units: 2 });
        assert_eq!(result.cleanup, Cleanup::Completed);
    }

    #[test]
    fn intervention_on_any_event_checkpoint_stops_new_content_and_preserves_issued_units() {
        use std::cell::{Cell, RefCell};
        for reason in [
            "target_not_frontmost",
            "focused_window_changed",
            "target_not_focused",
            "modal_changed",
            "hit_test_mismatch",
            "deadline_exceeded",
        ] {
            for stop_at in 1..=4 {
                let calls = Cell::new(0);
                let posted = RefCell::new(Vec::new());
                let result = dispatch_pairs(
                    [(&"down-1", &"up-1"), (&"down-2", &"up-2")],
                    1,
                    |_, _| {
                        calls.set(calls.get() + 1);
                        if calls.get() == stop_at {
                            Err(Refusal::new(reason, "injected receiver change"))
                        } else {
                            Ok(())
                        }
                    },
                    |event| posted.borrow_mut().push(*event),
                );
                let expected_pairs = match stop_at {
                    1 => 0,
                    2 | 3 => 1,
                    _ => 2,
                };
                assert_eq!(
                    posted.borrow().len(),
                    expected_pairs * 2,
                    "{reason} checkpoint {stop_at}"
                );
                let expected_units = u32::try_from(expected_pairs).unwrap();
                if stop_at == 1 {
                    assert_eq!(
                        result.delivery,
                        Delivery::NotDispatched {
                            reason: reason.into()
                        }
                    );
                    assert_eq!(result.cleanup, Cleanup::NotNeeded);
                } else {
                    assert_eq!(
                        result.delivery,
                        Delivery::Partial {
                            issued_units: expected_units,
                            reason: reason.into()
                        }
                    );
                    assert_eq!(result.cleanup, Cleanup::Completed);
                    assert_eq!(
                        posted.borrow().last(),
                        Some(&if expected_pairs == 1 { "up-1" } else { "up-2" })
                    );
                }
                assert_eq!(calls.get(), stop_at);
                assert_eq!(result.verification, Verification::NotRequested);
            }
        }
    }
}
