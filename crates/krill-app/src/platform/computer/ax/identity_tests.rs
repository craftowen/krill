//! Adversarial frozen-observation fixtures. `CFStrings` stand in for retained handles so no AX/TCC is needed.
use super::*;

fn semantic(role: &str, title: &str, identifier: Option<&str>) -> Semantic {
    Semantic {
        role: role.into(),
        subrole: None,
        title: Some(title.into()),
        description: None,
        identifier: identifier.map(str::to_owned),
        value: SemanticValue::Absent,
        frame: Some((
            ffi::CgPoint { x: 10.0, y: 20.0 },
            ffi::CgSize {
                width: 100.0,
                height: 30.0,
            },
        )),
    }
}

fn node(handle: &str, parent: Option<usize>, identity: Semantic) -> Node {
    Node {
        element: ffi::cf_string(handle).unwrap(),
        parent,
        identity: Some(identity),
    }
}

fn fixture(identifier: Option<&str>) -> Snapshot {
    let nodes = vec![
        node(
            "window",
            None,
            semantic("AXWindow", "Fixture", Some("window-id")),
        ),
        node(
            "group-a",
            Some(0),
            semantic("AXGroup", "Group A", Some("group-a-id")),
        ),
        node(
            "save-handle",
            Some(1),
            semantic("AXButton", "Save fixture", identifier),
        ),
        node(
            "group-b",
            Some(0),
            semantic("AXGroup", "Group B", Some("group-b-id")),
        ),
        node(
            "cancel-handle",
            Some(3),
            semantic("AXButton", "Cancel fixture", Some("cancel-id")),
        ),
    ];
    Snapshot {
        generation: 1,
        pid: 1234,
        witness: (1, 0),
        window: nodes[0].element.clone(),
        nodes,
        complete: true,
    }
}

fn refusal(snapshot: &Snapshot, current: &[Node]) -> &'static str {
    validate_observed_graph(snapshot, &snapshot.nodes[2], current, true)
        .unwrap_err()
        .code
}

#[test]
fn same_retained_handle_and_role_do_not_authorize_changed_label_or_subrole() {
    let observed = fixture(Some("save-id"));
    let mut current = observed.nodes.clone();
    current[2].identity.as_mut().unwrap().title = Some("Delete fixture".into());
    assert_eq!(current[2].element, observed.nodes[2].element);
    assert_eq!(refusal(&observed, &current), "element_changed");
    current = observed.nodes.clone();
    current[2].identity.as_mut().unwrap().subrole = Some("AXDestructiveButton".into());
    assert_eq!(refusal(&observed, &current), "element_changed");
}

#[test]
fn reparent_and_detach_fail_even_with_same_identifier_title_and_geometry() {
    let observed = fixture(Some("save-id"));
    let mut current = observed.nodes.clone();
    current[2].parent = Some(3);
    assert_eq!(refusal(&observed, &current), "element_reparented");
    current[2].parent = None;
    assert_eq!(refusal(&observed, &current), "element_reparented");
    current = observed.nodes.clone();
    current[1].element = ffi::cf_string("replacement-group").unwrap();
    assert_eq!(refusal(&observed, &current), "element_reparented");
    current = observed.nodes.clone();
    current[0].element = ffi::cf_string("replacement-window").unwrap();
    assert_eq!(refusal(&observed, &current), "element_reparented");
}

#[test]
fn reordered_traversal_never_rebinds_a_snapshot_key_to_its_new_index() {
    let observed = fixture(Some("save-id"));
    let current = vec![
        observed.nodes[0].clone(),
        observed.nodes[3].clone(),
        Node {
            parent: Some(1),
            ..observed.nodes[4].clone()
        },
        observed.nodes[1].clone(),
        Node {
            parent: Some(3),
            ..observed.nodes[2].clone()
        },
    ];
    assert_ne!(current[2].element, observed.nodes[2].element);
    assert!(validate_observed_graph(&observed, &observed.nodes[2], &current, true).is_ok());
    let mut moved = current;
    moved[4]
        .identity
        .as_mut()
        .unwrap()
        .frame
        .as_mut()
        .unwrap()
        .0
        .y += 50.0;
    assert_eq!(refusal(&observed, &moved), "element_changed");
}

#[test]
fn identical_unidentified_labels_are_ambiguous_even_at_different_positions_or_parents() {
    let observed = fixture(None);
    let mut current = observed.nodes.clone();
    let mut duplicate = current[2].clone();
    duplicate.element = ffi::cf_string("duplicate-save-handle").unwrap();
    duplicate.parent = Some(3);
    duplicate
        .identity
        .as_mut()
        .unwrap()
        .frame
        .as_mut()
        .unwrap()
        .0
        .x += 200.0;
    current.push(duplicate.clone());
    assert_eq!(refusal(&observed, &current), "identity_ambiguous");
    // A later unique census cannot retroactively disambiguate an originally ambiguous observation.
    let mut ambiguous_observation = fixture(None);
    ambiguous_observation.nodes.push(duplicate);
    assert_eq!(
        refusal(&ambiguous_observation, &observed.nodes),
        "identity_ambiguous"
    );
}

#[test]
fn duplicate_identifiers_are_also_rejected() {
    let observed = fixture(Some("save-id"));
    let mut current = observed.nodes.clone();
    current[4].identity.as_mut().unwrap().identifier = Some("save-id".into());
    assert_eq!(refusal(&observed, &current), "identity_ambiguous");
}

#[test]
fn full_value_and_full_label_are_independent_of_the_display_previews() {
    let prefix = "a".repeat(PREVIEW_UTF16);
    let old_value = format!("{prefix}old");
    let new_value = format!("{prefix}new");
    let label_prefix = "L".repeat(LABEL_UTF16);
    let read = |text: &str, title: &str| {
        let mut values = vec![None; B_SIZE + 1];
        values[B_ROLE] = ffi::cf_string("AXTextField");
        values[B_TITLE] = ffi::cf_string(title);
        values[B_IDENTIFIER] = ffi::cf_string("fixture-input");
        values[B_VALUE] = ffi::cf_string(text);
        Semantic::from_values(&values).unwrap()
    };
    let old = read(&old_value, &format!("{label_prefix}old"));
    let changed_value = read(&new_value, &format!("{label_prefix}old"));
    assert!(!old.matches(&changed_value, true));
    assert!(old.matches(&changed_value, false));
    let changed_label = read(&old_value, &format!("{label_prefix}new"));
    assert!(!old.matches(&changed_label, false));
    let mut observed = fixture(Some("save-id"));
    observed.nodes[2].identity = Some(old.clone());
    let mut current = observed.nodes.clone();
    current[2].identity = Some(changed_value);
    assert_eq!(refusal(&observed, &current), "element_changed");
    // Comparing after our own dispatch does not rewrite the immutable old snapshot.
    assert_eq!(observed.nodes[2].identity.as_ref().unwrap(), &old);
    assert_eq!(refusal(&observed, &current), "element_changed");
}

#[test]
fn overlong_or_unreadable_values_cannot_become_identity_prefixes() {
    let mut values = vec![None; B_SIZE + 1];
    values[B_ROLE] = ffi::cf_string("AXTextField");
    values[B_IDENTIFIER] = ffi::cf_string("fixture-input");
    values[B_VALUE] = ffi::cf_string(&"x".repeat(MAX_TEXT_UTF16 + 1));
    assert!(Semantic::from_values(&values).is_none());
    values[B_VALUE] = ffi::cf_string("");
    assert!(Semantic::from_values(&values).is_some());
    values[B_TITLE] = ffi::cf_string(&"x".repeat(IDENTITY_UTF16 + 1));
    assert!(Semantic::from_values(&values).is_none());
}

#[test]
fn ancestor_control_flags_are_not_target_enabled_or_settable_state() {
    let mut values = vec![None; B_SIZE + 1];
    values[B_ROLE] = ffi::cf_string("AXGroup");
    values[B_TITLE] = ffi::cf_string("container");
    let initial = Semantic::from_values(&values).unwrap();
    // These attributes belong to the container; semantic ancestry must not treat them as target gates.
    values[B_ENABLED] = ffi::cf_string("false");
    values[B_FOCUSED] = ffi::cf_string("false");
    assert_eq!(Semantic::from_values(&values), Some(initial));
}

#[test]
fn incomplete_census_cannot_prove_uniqueness_and_new_observation_can_accept_deliberate_change() {
    let mut observed = fixture(Some("save-id"));
    observed.complete = false;
    assert_eq!(refusal(&observed, &observed.nodes), "identity_ambiguous");
    observed.complete = true;
    let mut current = observed.nodes.clone();
    current[4].identity = None;
    assert_eq!(refusal(&observed, &current), "identity_incomplete");
    current = observed.nodes.clone();
    current[2].identity.as_mut().unwrap().value =
        SemanticValue::Text("deliberate new value".into());
    assert_eq!(refusal(&observed, &current), "element_changed");
    observed.nodes = current;
    assert!(validate_observed_graph(&observed, &observed.nodes[2], &observed.nodes, true).is_ok());
}

#[test]
fn later_chunks_recheck_uniqueness_without_rewriting_the_old_value() {
    let mut observed = fixture(None);
    observed.nodes[2].identity.as_mut().unwrap().role = "AXTextField".into();
    observed.nodes[2].identity.as_mut().unwrap().value = SemanticValue::Text("old".into());
    let mut current = observed.nodes.clone();
    current[2].identity.as_mut().unwrap().value =
        SemanticValue::Text("old plus issued text".into());
    assert!(validate_observed_graph(&observed, &observed.nodes[2], &current, false).is_ok());
    assert_eq!(refusal(&observed, &current), "identity_ambiguous");
    let mut duplicate = current[2].clone();
    duplicate.element = ffi::cf_string("new-duplicate-input").unwrap();
    duplicate.identity.as_mut().unwrap().value = SemanticValue::Text("different value".into());
    current.push(duplicate);
    assert_eq!(
        validate_observed_graph(&observed, &observed.nodes[2], &current, false)
            .unwrap_err()
            .code,
        "identity_ambiguous"
    );
}

#[test]
fn ancestor_context_value_change_requires_new_observation() {
    let mut observed = fixture(Some("save-id"));
    observed.nodes[1].identity.as_mut().unwrap().value = SemanticValue::Text("record-a".into());
    let mut current = observed.nodes.clone();
    current[1].identity.as_mut().unwrap().value = SemanticValue::Text("record-b".into());
    assert_eq!(refusal(&observed, &current), "element_changed");
}
