use super::*;

#[test]
fn chat_choices_preserve_host_identity_focus_and_sessions_across_restart() {
    let mut layout = Layout::default();
    let local = layout.focused().unwrap();
    layout.set_session(local, SessionId(41));
    layout.set_chat_view(local, true);
    layout.new_tab();
    let remote = layout.focused().unwrap();
    layout.set_host(remote, HostId::Remote("windows-pc".into()));
    layout.set_session(remote, SessionId(41));
    layout.set_chat_view(remote, false);
    layout.split(Axis::Horizontal);
    let split = layout.focused().unwrap();
    layout.set_session(split, SessionId(42));
    layout.set_chat_view(split, true);
    let path = std::env::temp_dir().join(format!("krill-chat-layout-{}.bin", std::process::id()));
    layout.save(&path).unwrap();
    let mut loaded = Layout::load(&path).unwrap();
    assert_eq!(loaded, layout);
    assert_eq!(loaded.chat_view(local), Some(true));
    assert_eq!(loaded.chat_view(remote), Some(false));
    assert_eq!(loaded.host(remote), &HostId::Remote("windows-pc".into()));
    assert_eq!(loaded.focused(), Some(split));
    loaded.rebind_session(&HostId::Local, SessionId(41), SessionId(49));
    assert_eq!(loaded.chat_view(local), Some(true));
    loaded.close_pane();
    assert_eq!(loaded.chat_view(split), None);
    loaded.close_pane();
    assert_eq!(loaded.chat_view(remote), None);
    assert_eq!(loaded.chat_view(local), Some(true));
    loaded.set_session(local, SessionId(50));
    assert_eq!(
        loaded.chat_view(local),
        None,
        "unrelated terminal inherited an old view choice"
    );
    loaded.set_chat_view(local, true);
    loaded.clear_session(local);
    loaded.set_session(local, SessionId(51));
    assert_eq!(
        loaded.chat_view(local),
        Some(true),
        "explicit cold restart lost the pane choice"
    );
    std::fs::remove_file(path).unwrap();
}

#[test]
fn optional_chat_preferences_keep_existing_v7_readers_and_corrupt_preference_recovery() {
    let mut layout = Layout::default();
    let pane = layout.focused().unwrap();
    layout.set_session(pane, SessionId(51));
    layout.set_chat_view(pane, true);
    let path = std::env::temp_dir().join(format!("krill-chat-v7-{}.bin", std::process::id()));
    layout.save(&path).unwrap();
    let bytes = fs::read(&path).unwrap();
    // The old v7 decoder reads its original layout, hosts, tail and focus exactly.
    let (old, rest) = postcard::take_from_bytes::<Layout>(&bytes[1..]).unwrap();
    let (_, rest) = postcard::take_from_bytes::<BTreeMap<PaneId, HostId>>(rest).unwrap();
    let (_, rest) = postcard::take_from_bytes::<LayoutTail>(rest).unwrap();
    let focus = postcard::from_bytes::<FocusTail>(rest).unwrap();
    assert_eq!(old.session(pane), Some(SessionId(51)));
    assert_eq!(focus.order, vec![0]);
    let (_, preferences) = postcard::take_from_bytes::<FocusTail>(rest).unwrap();
    assert!(preferences.starts_with(CHAT_VIEWS_TAG));
    let mut legacy = bytes[..bytes.len() - preferences.len()].to_vec();
    let loaded = Layout::decode(&legacy).unwrap();
    assert_eq!(loaded.session(pane), Some(SessionId(51)));
    assert_eq!(loaded.chat_view(pane), None);
    // A corrupt/unknown preference cannot erase the valid tabs and PTY bindings.
    legacy.extend_from_slice(CHAT_VIEWS_TAG);
    legacy.push(0xff);
    let loaded = Layout::decode(&legacy).unwrap();
    assert_eq!(loaded.session(pane), Some(SessionId(51)));
    assert_eq!(loaded.chat_view(pane), None);
    fs::remove_file(path).unwrap();
}
