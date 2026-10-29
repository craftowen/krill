use super::*;

#[test]
fn account_handoff_rebinds_background_tabs_without_touching_remote_sessions() {
    let mut layout = Layout::default();
    layout.set_session(PaneId(0), SessionId(7));
    layout.new_tab();
    layout.set_session(PaneId(1), SessionId(7));
    layout.new_tab();
    layout.set_session(PaneId(2), SessionId(7));
    layout.set_host(PaneId(2), HostId::Remote("ssh".into()));
    assert_eq!(
        layout.rebind_session(&HostId::Local, SessionId(7), SessionId(9)),
        [PaneId(0), PaneId(1)]
    );
    assert_eq!(layout.session(PaneId(0)), Some(SessionId(9)));
    assert_eq!(layout.session(PaneId(1)), Some(SessionId(9)));
    assert_eq!(layout.session(PaneId(2)), Some(SessionId(7)));
    assert!(
        layout
            .rebind_session(&HostId::Local, SessionId(7), SessionId(9))
            .is_empty()
    );
}

fn temp_path(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("mk-layout-{}-{name}", std::process::id()));
    fs::remove_dir_all(&dir).ok();
    dir.join("state").join(FILE_NAME)
}

fn leaf_count(tree: &PaneTree) -> usize {
    let mut v = Vec::new();
    tree.leaves(&mut v);
    v.len()
}

/// 완료 조건 규모: 탭 10개, 한 탭에 분할 4번(칸 5개, 좌우·상하 섞음).
fn sample() -> Layout {
    let mut l = Layout::default();
    for _ in 0..9 {
        l.new_tab();
    }
    l.activate(3);
    l.split(Axis::Horizontal);
    l.split(Axis::Vertical);
    l.split(Axis::Horizontal);
    l.split(Axis::Vertical);
    l.set_ratio(&[], 0.3);
    l.set_ratio(&[Branch::Second], 0.7);
    l.sidebar_width = 199.0;
    l.panel_width = 321.0;
    l
}

#[test]
fn ten_tabs_four_splits() {
    let l = sample();
    assert_eq!(l.tabs().len(), 10);
    assert_eq!(l.active(), 3);
    assert_eq!(leaf_count(&l.active_tab().root), 5);
    assert!(l.is_valid());
    let PaneTree::Split { ratio, second, .. } = &l.active_tab().root else {
        panic!("root should be a split");
    };
    assert!((ratio - 0.3).abs() < f32::EPSILON);
    assert!(
        matches!(**second, PaneTree::Split { ratio, .. } if (ratio - 0.7).abs() < f32::EPSILON)
    );
}

#[test]
fn restart_restores_layout() {
    let path = temp_path("roundtrip");
    let l = sample();
    l.save(&path).unwrap();
    assert_eq!(Layout::load(&path).unwrap(), l);
    // 다시 저장해도(재시작 후 첫 저장) 같은 바이트.
    let first = fs::read(&path).unwrap();
    Layout::load(&path).unwrap().save(&path).unwrap();
    assert_eq!(fs::read(&path).unwrap(), first);
    assert!(!path.with_extension("tmp").exists());
    let mode = std::os::unix::fs::PermissionsExt::mode(&fs::metadata(&path).unwrap().permissions());
    assert_eq!(mode & 0o777, 0o600);
}

#[test]
fn close_pane_collapses_split_then_tab() {
    let mut l = Layout::default();
    l.new_tab();
    l.split(Axis::Horizontal); // 칸 1 | 칸 2, 포커스 2
    l.split(Axis::Vertical); // 칸 1 | (칸 2 / 칸 3), 포커스 3
    l.close_pane();
    assert_eq!(l.active_tab().focused, PaneId(2));
    assert_eq!(leaf_count(&l.active_tab().root), 2);
    l.close_pane();
    assert_eq!(l.active_tab().root, PaneTree::Leaf(PaneId(1)));
    l.close_pane(); // 마지막 칸 → 탭 닫힘
    assert_eq!(l.tabs().len(), 1);
    assert_eq!(l.active(), 0);
    l.close_pane(); // 마지막(기본) 탭도 닫힘 → 탭 0개
    assert!(l.tabs().is_empty());
    assert_eq!(l.active(), 0);
    assert_eq!(l.focused(), None);
    assert!(l.visible().is_empty());
    assert!(l.is_valid());

    // #30326: 왼쪽 형제가 다시 분할된 트리 `(칸 A | 칸 B) | 칸 C`에서 오른쪽 `칸 C`를 닫으면
    // 첫 잎(`칸 A`)이 아니라 인접한 이웃 잎(`칸 B`)으로 포커스가 이동해야 한다.
    l.new_tab(); // PaneId(4)
    let a = l.focused().unwrap();
    l.split(Axis::Horizontal); // a | c
    let c = l.focused().unwrap();
    l.focus(a);
    l.split(Axis::Vertical); // (a / b) | c
    let b = l.focused().unwrap();
    l.focus(c);
    l.close_pane();
    assert_eq!(l.focused(), Some(b));
}

#[test]
fn close_tab_keeps_active_in_range() {
    let mut l = Layout::default();
    l.new_tab();
    l.new_tab();
    l.close_tab(0); // 활성(2)보다 앞 → 활성 인덱스 당김
    assert_eq!(l.active(), 1);
    l.close_tab(1); // 활성이 마지막 → 앞으로
    assert_eq!(l.active(), 0);
    l.close_tab(5);
    assert_eq!(l.tabs().len(), 1);
    l.close_tab(0); // 기본 탭 닫기 → 빈 탭 목록
    assert!(l.tabs().is_empty());
    assert_eq!(l.active(), 0);
    assert!(l.is_valid());
    l.cycle(-1);
    assert_eq!(l.active(), 0);
    let path = temp_path("empty-tabs");
    l.save(&path).unwrap();
    assert_eq!(Layout::load(&path).unwrap(), l);
}

#[test]
fn drag_ratio_keeps_min_50px() {
    assert!((drag_ratio(150.0, 100.0, 400.0) - 0.125).abs() < 1e-6);
    assert!((drag_ratio(110.0, 100.0, 400.0) - 50.0 / 400.0).abs() < 1e-6);
    assert!((drag_ratio(9999.0, 100.0, 400.0) - 350.0 / 400.0).abs() < 1e-6);
    assert!((drag_ratio(0.0, 0.0, 80.0) - 0.5).abs() < f32::EPSILON);
    assert!((drag_ratio(0.0, 0.0, f32::NAN) - 0.5).abs() < f32::EPSILON);
    assert!((clamp_column(10.0, 1000.0, 280.0) - MIN_PANE_PX).abs() < f32::EPSILON);
    assert!((clamp_column(900.0, 1000.0, 280.0) - 670.0).abs() < f32::EPSILON);
}

#[test]
fn set_ratio_rejects_out_of_range() {
    let mut l = Layout::default();
    l.split(Axis::Horizontal);
    for bad in [0.0, 1.0, -1.0, f32::NAN] {
        l.set_ratio(&[], bad);
    }
    l.set_ratio(&[Branch::First], 0.2); // 잎이라 무시
    assert!(
        matches!(l.active_tab().root, PaneTree::Split { ratio, .. } if (ratio - 0.5).abs() < f32::EPSILON)
    );
}

#[test]
fn bad_files_are_rejected() {
    let path = temp_path("bad");
    assert_eq!(
        Layout::load(&path).unwrap_err().kind(),
        io::ErrorKind::NotFound
    );
    fs::create_dir_all(path.parent().unwrap()).unwrap();

    let mut good = vec![FORMAT_VERSION];
    good.extend(postcard::to_stdvec(&sample()).unwrap());
    let mut wrong_version = good.clone();
    wrong_version[0] = FORMAT_VERSION + 1;
    let mut big = vec![FORMAT_VERSION];
    big.resize(usize::try_from(MAX_FILE_BYTES).unwrap() + 1, 0);

    let mut active_out = sample();
    // `active == tabs.len()`은 보이는 탭 없는 빈 화면이라 유효하다. 그보다 크면 깨진 파일이다.
    active_out.active = active_out.tabs.len() + 1;
    let mut dup = Layout::default();
    dup.tabs.push(Tab::single(PaneId(0)));
    let mut bad_ratio = Layout::default();
    bad_ratio.split(Axis::Vertical);
    if let PaneTree::Split { ratio, .. } = &mut bad_ratio.tabs[0].root {
        *ratio = 1.5;
    }
    let mut bad_focus = Layout::default();
    bad_focus.tabs[0].focused = PaneId(7);
    bad_focus.next_pane = 8;
    let mut stray_session = Layout::default();
    stray_session.sessions.insert(PaneId(5), SessionId(1));
    let encode = |l: &Layout| {
        let mut v = vec![FORMAT_VERSION];
        v.extend(postcard::to_stdvec(l).unwrap());
        v
    };

    let cases = [
        vec![],
        wrong_version,
        big,
        good[..good.len() / 2].to_vec(),
        encode(&active_out),
        encode(&dup),
        encode(&bad_ratio),
        encode(&bad_focus),
        encode(&stray_session),
    ];
    for (i, bytes) in cases.iter().enumerate() {
        fs::write(&path, bytes).unwrap();
        let err = Layout::load(&path).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData, "case {i}");
    }
    fs::write(&path, &good).unwrap();
    assert!(Layout::load(&path).is_ok());
}

/// 크기 상한 안에서 가장 깊게 중첩한 파일도 스택을 넘지 않는다(2MiB 스레드, 앱 메인 스레드는 8MiB).
#[test]
fn deepest_nesting_within_cap_does_not_overflow() {
    let levels = (usize::try_from(MAX_FILE_BYTES).unwrap() - 34) / 8;
    let mut bytes = vec![FORMAT_VERSION, 1]; // 버전, 탭 1개
    for _ in 0..levels {
        bytes.extend([1, 0]); // Split, Horizontal
        bytes.extend(0.5f32.to_le_bytes());
    }
    bytes.extend([0, 0]); // 가장 안쪽 잎
    for _ in 0..levels {
        bytes.extend([0, 0]); // 각 단계의 second 잎(중복 id라 불변식 위반)
    }
    bytes.extend([0, 0, 0, 0, 0]); // focused, pinned, color, custom_title(None), active
    bytes.extend(240f32.to_le_bytes());
    bytes.extend(280f32.to_le_bytes());
    bytes.push(1); // next_pane
    bytes.push(0); // sessions
    bytes.push(0); // launches
    bytes.push(0); // files
    assert!(bytes.len() as u64 <= MAX_FILE_BYTES);

    let path = temp_path("deep");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, &bytes).unwrap();
    let result = std::thread::Builder::new()
        .stack_size(2 * 1024 * 1024)
        .spawn(move || Layout::load(&path).map(|_| ()).map_err(|e| e.to_string()))
        .unwrap()
        .join()
        .unwrap();
    // 역직렬화는 끝까지 성공하고 불변식 검사에서 거절돼야 한다(깊이가 실제로 쌓였다는 뜻).
    assert_eq!(result, Err("layout violates invariants".to_owned()));
}

/// OWE-490: 칸 ↔ 세션은 저장·복원되고, 닫힌 칸의 항목은 지워진다. v1 파일은 기본 레이아웃으로 시작한다.
#[test]
fn sessions_persist_and_prune() {
    let path = temp_path("sessions");
    let mut l = Layout::default();
    l.split(Axis::Horizontal);
    let [a, b] = l.visible()[..] else {
        panic!("two panes")
    };
    l.set_session(a, SessionId(7));
    l.set_session(b, SessionId(8));
    l.set_session(PaneId(99), SessionId(9)); // 없는 칸은 무시
    l.save(&path).unwrap();
    let back = Layout::load(&path).unwrap();
    assert_eq!(back.session(a), Some(SessionId(7)));
    assert_eq!(back.session(b), Some(SessionId(8)));
    assert_eq!(back.session(PaneId(99)), None);

    l.close_pane(); // 포커스 칸 b
    assert_eq!(l.session(b), None);
    assert_eq!(l.session(a), Some(SessionId(7)));
    l.new_tab();
    assert_eq!(l.visible().len(), 1);
    l.close_tab(0);
    assert_eq!(l.session(a), None);

    let mut v1 = fs::read(&path).unwrap();
    v1[0] = 1;
    fs::write(&path, v1).unwrap();
    assert_eq!(
        Layout::load(&path).unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );
}

/// MK-055: 폴더·프로그램은 복원되고 새 탭·분할은 폴더를 이어받는다.
#[test]
fn folder_launches_persist_inherit_and_prune() {
    let path = temp_path("folder");
    let mut l = Layout::default();
    let pane = l.active_tab().focused;
    let launch = krill_proto::control::Create {
        account: None,
        program: krill_proto::control::Program::Agent { id: "codex".into() },
        cwd: b"/tmp/folder with 'quotes'".to_vec(),
        cols: 80,
        rows: 24,
    };
    l.set_launch(pane, launch.clone());
    l.set_session(pane, SessionId(55));
    l.save(&path).unwrap();
    let mut l = Layout::load(&path).unwrap();
    assert_eq!(l.launch(pane), Some(&launch));
    assert_eq!(l.session(pane), Some(SessionId(55)));
    l.split(Axis::Horizontal);
    let split = l.active_tab().focused;
    assert_eq!(l.launch(split).unwrap().cwd, launch.cwd);
    assert_eq!(
        l.launch(split).unwrap().program,
        krill_proto::control::Program::Login
    );
    l.close_pane();
    assert!(l.launch(split).is_none());
    l.new_tab();
    assert_eq!(l.launch(l.active_tab().focused).unwrap().cwd, launch.cwd);
    l.close_tab(0);
    assert!(l.launch(pane).is_none());
}

fn legacy_tabs(l: &Layout) -> Vec<(&PaneTree, PaneId)> {
    l.tabs.iter().map(|t| (&t.root, t.focused)).collect()
}

#[test]
fn v2_layout_migrates_without_losing_session_ids() {
    let path = temp_path("v2");
    let mut l = Layout::default();
    l.set_session(l.active_tab().focused, SessionId(420));
    let tabs = legacy_tabs(&l);
    let mut old = vec![2];
    old.extend(
        postcard::to_stdvec(&(
            &tabs,
            l.active,
            l.sidebar_width,
            l.panel_width,
            l.next_pane,
            &l.sessions,
        ))
        .unwrap(),
    );
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, old).unwrap();
    assert_eq!(Layout::load(&path).unwrap(), l);
}

/// 폴더 설정이 커져도 로드할 수 없는 파일로 이전 세션 지도를 덮어쓰지 않는다.
#[test]
fn oversized_folder_settings_preserve_previous_layout() {
    let path = temp_path("folder-large");
    let mut l = Layout::default();
    l.set_session(l.active_tab().focused, SessionId(420));
    l.save(&path).unwrap();
    let before = l.clone();
    l.set_launch(
        l.active_tab().focused,
        krill_proto::control::Create {
            account: None,
            program: krill_proto::control::Program::Login,
            cwd: vec![b'x'; usize::try_from(MAX_FILE_BYTES).unwrap()],
            cols: 80,
            rows: 24,
        },
    );
    assert_eq!(
        l.save(&path).unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );
    assert_eq!(Layout::load(&path).unwrap(), before);
}

#[test]
fn reorder_preserves_active_session_and_split_focus() {
    let mut layout = Layout::default();
    layout.set_session(layout.active_tab().focused, SessionId(7));
    layout.split(Axis::Horizontal);
    let original = layout.active_tab().clone();
    layout.new_tab();
    layout.new_tab();
    layout.activate(0);
    layout.reorder_tab(0, 2);
    assert_eq!(layout.active(), 2);
    assert_eq!(layout.active_tab(), &original);
    assert!(layout.sessions().contains(&SessionId(7)));
    layout.reorder_tab(0, 2);
    assert_eq!(layout.active(), 1);
    layout.reorder_tab(2, 0);
    assert_eq!(layout.active(), 2);
    let before = layout.clone();
    layout.reorder_tab(99, 0);
    layout.reorder_tab(0, 99);
    assert_eq!(layout, before);
    assert!(layout.is_valid());
}

/// OWE-583: 손상·미래 버전 원본은 기본 레이아웃의 첫 저장 뒤에도 보존한다.
#[test]
fn corrupt_and_future_files_are_preserved_before_reset() {
    for (name, bytes) in [
        ("corrupt-preserve", vec![FORMAT_VERSION, 255]),
        ("future-preserve", vec![FORMAT_VERSION + 1, 42]),
    ] {
        let path = temp_path(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, &bytes).unwrap();
        assert_eq!(
            Layout::load(&path).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        assert_eq!(fs::read(path.with_extension("corrupt")).unwrap(), bytes);
        Layout::default().save(&path).unwrap();
        assert!(Layout::load(&path).is_ok());
        assert_eq!(fs::read(path.with_extension("corrupt")).unwrap(), bytes);
    }
}

#[test]
fn corrupt_backups_never_replace_previous_originals() {
    let path = temp_path("corrupt-collision");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path.with_extension("corrupt"), b"first original").unwrap();
    fs::write(&path, b"second original").unwrap();
    assert!(Layout::load(&path).is_err());
    assert_eq!(
        fs::read(path.with_extension("corrupt")).unwrap(),
        b"first original"
    );
    assert_eq!(
        fs::read(path.with_extension("1.corrupt")).unwrap(),
        b"second original"
    );
}

/// MK-119: 파일 칸은 같은 파일이면 다시 쓰고, 세션을 받지 않으며, 저장·복원되고, 닫으면 지워진다.
#[test]
fn file_panes_open_dedupe_persist_and_prune() {
    let mut l = Layout::default();
    let a = l.open_file(PathBuf::from("/r/a.rs"));
    assert_eq!(l.tabs().len(), 2);
    assert_eq!(l.file(a), Some(Path::new("/r/a.rs")));
    l.set_session(a, SessionId(9));
    assert_eq!(l.session(a), None, "file pane never gets a session");

    l.activate(0);
    assert_eq!(l.open_file(PathBuf::from("/r/a.rs")), a);
    assert_eq!((l.tabs().len(), l.active()), (2, 1));
    let b = l.open_file(PathBuf::from("/r/b.rs"));
    assert_eq!(l.tabs().len(), 3);

    let path = temp_path("files");
    l.save(&path).unwrap();
    let back = Layout::load(&path).unwrap();
    assert_eq!(back, l);

    l.close_tab(l.active());
    assert_eq!(l.file(b), None);
    assert_eq!(l.files().count(), 1);
}

#[test]
fn v3_layout_migrates_without_files() {
    let path = temp_path("v3");
    let mut l = Layout::default();
    l.set_session(l.active_tab().focused, SessionId(7));
    let tabs = legacy_tabs(&l);
    let mut old = vec![3];
    old.extend(
        postcard::to_stdvec(&(
            &tabs,
            l.active,
            l.sidebar_width,
            l.panel_width,
            l.next_pane,
            &l.sessions,
            &l.launches,
        ))
        .unwrap(),
    );
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, old).unwrap();
    assert_eq!(Layout::load(&path).unwrap(), l);
}

/// OWE-712가 `Create`에 `account`를 붙이기 전의 디스크 형식(v2~v4)은 실행 정보(launches)가 있어도 그대로 읽힌다.
/// postcard는 `#[serde(default)]`를 모르므로 새 필드가 옛 바이트를 밀어 레이아웃 전체가 깨졌었다.
#[test]
fn v4_layout_with_launches_survives_create_account_field() {
    #[derive(Serialize)]
    struct OldCreate<'a> {
        program: &'a Program,
        cwd: &'a [u8],
        cols: u16,
        rows: u16,
    }
    let path = temp_path("v4-launch");
    let mut l = Layout::default();
    let pane = l.active_tab().focused;
    l.set_session(pane, SessionId(7));
    let launch = Create {
        program: Program::Agent {
            id: "claude".into(),
        },
        cwd: b"/tmp/work".to_vec(),
        cols: 80,
        rows: 24,
        account: None,
    };
    l.set_launch(pane, launch.clone());
    let launches: BTreeMap<PaneId, OldCreate<'_>> = BTreeMap::from([(
        pane,
        OldCreate {
            program: &launch.program,
            cwd: &launch.cwd,
            cols: launch.cols,
            rows: launch.rows,
        },
    )]);
    let tabs = legacy_tabs(&l);
    let mut old = vec![4];
    old.extend(
        postcard::to_stdvec(&(
            &tabs,
            l.active,
            l.sidebar_width,
            l.panel_width,
            l.next_pane,
            &l.sessions,
            &launches,
            &l.files,
        ))
        .unwrap(),
    );
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, old).unwrap();
    assert_eq!(Layout::load(&path).unwrap(), l);
    // 새로 저장하면 새 형식으로 다시 읽힌다(계정 포함).
    let mut with_account = l.clone();
    with_account.set_launch(
        pane,
        Create {
            account: Some("claude-a".into()),
            ..launch
        },
    );
    with_account.save(&path).unwrap();
    assert_eq!(Layout::load(&path).unwrap(), with_account);
}

#[test]
fn new_tab_after_inserts_adjacent_and_activates() {
    let mut l = Layout::default();
    let first = l.active_tab().focused;
    l.new_tab();
    let third = l.active_tab().focused;
    l.new_tab_after(0);
    assert_eq!(l.tabs().len(), 3);
    assert_eq!(l.active(), 1);
    assert_eq!(l.tabs()[0].focused, first);
    assert_eq!(l.tabs()[2].focused, third);
    assert_ne!(l.tabs()[1].focused, first);
    assert_ne!(l.tabs()[1].focused, third);
    // 삽입 뒤 MRU 인덱스도 밀린다: 새 탭(1) → 기존 탭 1이던 셋째(2) → 첫째(0).
    assert_eq!(l.mru_order(), vec![1, 2, 0]);
    assert!(l.is_valid());
}

/// Orca: 새 탭(⌘T)은 맨 끝에 열린다.
#[test]
fn new_tab_appends_after_last_tab() {
    let mut l = Layout::default();
    let first = l.active_tab().focused;
    l.new_tab();
    let last = l.active_tab().focused;
    l.activate(0);
    l.new_tab();
    assert_eq!(l.tabs().len(), 3);
    assert_eq!(l.active(), 2);
    assert_eq!(l.tabs()[0].focused, first);
    assert_eq!(l.tabs()[1].focused, last);
    assert!(l.is_valid());
}

#[test]
fn v5_layout_migrates_to_v6_with_default_pin_and_color() {
    let path = temp_path("v5");
    let mut l = Layout::default();
    let pane = l.active_tab().focused;
    l.set_session(pane, SessionId(9));
    l.set_launch(
        pane,
        Create {
            program: Program::Agent { id: "codex".into() },
            cwd: b"/tmp/v5".to_vec(),
            cols: 80,
            rows: 24,
            account: Some("acct-1".into()),
        },
    );
    let tabs = legacy_tabs(&l);
    let mut old = vec![5];
    old.extend(
        postcard::to_stdvec(&(
            &tabs,
            l.active,
            l.sidebar_width,
            l.panel_width,
            l.next_pane,
            &l.sessions,
            &l.launches,
            &l.files,
        ))
        .unwrap(),
    );
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, old).unwrap();
    let loaded = Layout::load(&path).unwrap();
    assert_eq!(loaded, l);
    assert!(!loaded.is_tab_pinned(0));
    assert_eq!(loaded.tab_color(0), None);
}

/// OWE-734: 탭 핀 고정 상태와 9가지 색상 태그는 `layout.bin`에 저장·복원된다.
#[test]
fn tab_pin_and_nine_colors_persist_in_layout_bin() {
    let path = temp_path("pin-colors");
    let mut l = Layout::default();
    for _ in 0..TabColor::ALL.len() {
        l.new_tab();
    }
    assert_eq!(l.tabs().len(), TabColor::ALL.len() + 1);
    for (i, color) in TabColor::ALL.into_iter().enumerate() {
        l.set_tab_color(i, Some(color));
        if i % 2 == 0 {
            assert!(l.toggle_pin_tab(i));
        }
    }
    l.set_tab_color(TabColor::ALL.len(), None);
    l.save(&path).unwrap();

    let loaded = Layout::load(&path).unwrap();
    assert_eq!(loaded, l);
    for (i, color) in TabColor::ALL.into_iter().enumerate() {
        assert_eq!(loaded.tab_color(i), Some(color));
        assert_eq!(loaded.is_tab_pinned(i), i % 2 == 0);
    }
    assert_eq!(loaded.tab_color(TabColor::ALL.len()), None);
}

/// OWE-734: 다른 탭 모두 닫기 / 오른쪽 탭 닫기 / 모든 에디터 탭 닫기는 핀 고정된 탭을 건너뛴다.
#[test]
fn bulk_close_actions_skip_pinned_tabs() {
    // 1. close_tabs_to_right: 기준 탭 오른쪽의 비고정 탭만 닫고 핀 고정 탭은 유지한다.
    let mut l = Layout::default(); // tab 0 (pane 0)
    l.new_tab(); // tab 1 (pane 1)
    l.new_tab(); // tab 2 (pane 2) - pinned
    l.new_tab(); // tab 3 (pane 3)
    l.toggle_pin_tab(2);
    l.close_tabs_to_right(1);
    let panes: Vec<PaneId> = l.tabs().iter().map(|t| t.focused).collect();
    assert_eq!(panes, vec![PaneId(0), PaneId(1), PaneId(2)]);
    assert!(l.is_tab_pinned(2));

    // 2. close_other_tabs: 기준 탭과 핀 고정 탭만 남긴다.
    let mut l = Layout::default(); // tab 0 (pane 0) - pinned
    l.new_tab(); // tab 1 (pane 1)
    l.new_tab(); // tab 2 (pane 2) - keep
    l.new_tab(); // tab 3 (pane 3)
    l.toggle_pin_tab(0);
    l.close_other_tabs(2);
    let panes: Vec<PaneId> = l.tabs().iter().map(|t| t.focused).collect();
    assert_eq!(panes, vec![PaneId(0), PaneId(2)]);
    assert_eq!(l.active(), 1);
    assert!(l.is_tab_pinned(0));

    // 3. close_all_editor_tabs: 비고정 파일 탭만 닫고 터미널 탭과 핀 고정 파일 탭은 남긴다.
    let mut l = Layout::default(); // tab 0 (terminal, pane 0)
    let f1 = l.open_file(PathBuf::from("/tmp/pinned.rs")); // tab 1 (pinned editor)
    l.toggle_pin_tab(1);
    let _f2 = l.open_file(PathBuf::from("/tmp/unpinned.rs")); // tab 2 (unpinned editor)
    l.new_tab(); // tab 3 (terminal, pane 3)
    assert!(l.is_editor_tab(1));
    assert!(l.is_editor_tab(2));
    assert!(!l.is_editor_tab(0));
    l.close_all_editor_tabs();
    let panes: Vec<PaneId> = l.tabs().iter().map(|t| t.focused).collect();
    assert_eq!(panes, vec![PaneId(0), f1, PaneId(3)]);
}

/// OWE-734: `Ctrl+Tab` / `Ctrl+Shift+Tab`은 최근 포커스 순서(MRU)대로 탭을 순환하고 확정 시 MRU를 갱신한다.
#[test]
fn ctrl_tab_mru_cycles_and_commits_recent_order() {
    let mut l = Layout::default(); // tab 0
    l.new_tab(); // tab 1
    l.new_tab(); // tab 2
    l.new_tab(); // tab 3
    // 포커스 이력: 0 -> 1 -> 2 -> 3 -> 1 -> 3 -> 0
    l.activate(1);
    l.activate(3);
    l.activate(0);
    assert_eq!(l.mru_order(), vec![0, 3, 1, 2]);
    assert!(!l.mru_switching());

    // 첫 Ctrl+Tab: 직전 탭(3)으로 이동
    l.cycle_mru(1);
    assert!(l.mru_switching());
    assert_eq!(l.active(), 0);
    assert_eq!(l.mru_cursor(), Some(1));

    // Ctrl을 누른 채 한 번 더 Ctrl+Tab: 그 이전 탭(1)으로 이동
    l.cycle_mru(1);
    assert_eq!(l.active(), 0);
    assert_eq!(l.mru_cursor(), Some(2));

    // Ctrl+Shift+Tab: 다시 탭 3으로 역방향 이동
    l.cycle_mru(-1);
    assert_eq!(l.active(), 0);
    assert_eq!(l.mru_cursor(), Some(1));

    // Ctrl 해제(확정): 탭 3이 MRU 최상단으로 올라간다([3, 0, 1, 2]).
    assert!(l.commit_mru());
    assert!(!l.mru_switching());
    assert_eq!(l.mru_order(), vec![3, 0, 1, 2]);

    // 다시 Ctrl+Tab 한 번 누르고 확정하면 탭 0으로 토글된다.
    l.cycle_mru(1);
    assert_eq!(l.active(), 3);
    l.cancel_mru();
    assert_eq!(l.active(), 3);
    assert_eq!(l.mru_order(), vec![3, 0, 1, 2]);
    l.cycle_mru(1);
    assert!(l.commit_mru());
    assert_eq!(l.active(), 0);
    assert_eq!(l.mru_order(), vec![0, 3, 1, 2]);
}

/// OWE-869: 탭이 0개인 상태에서 분할이나 파일 열기를 요청하면 패닉 없이 첫 탭을 만든다.
#[test]
fn empty_tabs_split_and_open_file_create_first_tab() {
    let mut l = Layout::default();
    l.close_tab(0);
    assert!(l.tabs().is_empty());

    l.split(Axis::Horizontal);
    assert_eq!(l.tabs().len(), 1);
    assert_eq!(l.visible().len(), 1);
    assert!(l.is_valid());

    l.close_tab(0);
    assert!(l.tabs().is_empty());
    let file_pane = l.open_file(PathBuf::from("/tmp/empty-open.rs"));
    assert_eq!(l.tabs().len(), 1);
    assert_eq!(l.focused(), Some(file_pane));
    assert_eq!(l.file(file_pane), Some(Path::new("/tmp/empty-open.rs")));
    assert!(l.is_valid());
}

/// OWE-849: 단일 클릭 미리보기는 같은 칸을 재사용하고, 고정 열기·탭 닫기로 슬롯이 비워진다.
#[test]
fn preview_tab_reuses_pane_until_pinned_or_closed() {
    let mut l = Layout::default(); // tab 0 (terminal)
    let a = PathBuf::from("/tmp/preview-a.rs");
    let b = PathBuf::from("/tmp/preview-b.rs");
    let p1 = l.open_file_preview(a.clone());
    assert_eq!(l.tabs().len(), 2);
    // 두 번째 미리보기는 같은 칸을 재사용한다.
    let p2 = l.open_file_preview(b.clone());
    assert_eq!(p1, p2);
    assert_eq!(l.tabs().len(), 2);
    assert_eq!(l.file(p2), Some(b.as_path()));
    // 이미 열린 파일을 미리보기로 열면 포커스만 옮긴다.
    assert_eq!(l.open_file_preview(b.clone()), p2);
    // 고정으로 열면 슬롯이 비워지고 같은 탭에 머문다.
    assert_eq!(l.open_file(b.clone()), p2);
    assert_eq!(l.tabs().len(), 2);
    // 다음 미리보기는 슬롯이 비었으므로 새 탭을 연다.
    let p4 = l.open_file_preview(a.clone());
    assert_ne!(p4, p2);
    assert_eq!(l.tabs().len(), 3);
    // 미리보기 탭을 닫으면 슬롯이 함께 비워진다.
    l.activate(2);
    l.close_tab(2);
    let p5 = l.open_file_preview(a.clone());
    assert_ne!(p5, p4);
    assert_eq!(l.tabs().len(), 3);
}

/// OWE-842: 창 크기/위치/모드, 사이드바/우측 패널 접힘·활성 탭, 파일 탭 커서/편집 모드가 `layout.bin`에 저장·복원되며,
/// 꼬리(`LayoutTail`)가 없는 기존 v6 `layout.bin`도 그대로 읽힌다.
#[test]
fn owe_842_layout_tail_persists_window_sidebar_panel_and_file_states() {
    let mut l = Layout::default();
    let file_pane = l.open_file(PathBuf::from("/repo/src/lib.rs"));
    let bounds = SavedWindowBounds {
        x: 120.0,
        y: 80.0,
        width: 1440.0,
        height: 900.0,
        mode: SavedWindowMode::Fullscreen,
    };
    assert!(bounds.is_valid());
    assert!(
        !(SavedWindowBounds {
            width: MIN_WINDOW_WIDTH - 1.0,
            ..bounds
        })
        .is_valid()
    );
    assert!(l.set_window_bounds(bounds));
    assert!(!l.set_window_bounds(bounds));
    l.sidebar_collapsed = true;
    l.panel_collapsed = false;
    assert!(l.set_panel_tab(crate::diff::PanelTab::Search));
    l.set_file_state(
        file_pane,
        SavedFileState {
            line: 42,
            column: 7,
            preview: true,
        },
    );

    let path = temp_path("owe-842-tail");
    l.save(&path).unwrap();
    assert_eq!(Layout::peek_window_bounds(&path), Some(bounds));

    let loaded = Layout::load(&path).unwrap();
    assert_eq!(loaded, l);
    assert_eq!(loaded.window_bounds(), Some(bounds));
    assert!(loaded.sidebar_collapsed);
    assert!(!loaded.panel_collapsed);
    assert_eq!(loaded.panel_tab(), crate::diff::PanelTab::Search);
    assert_eq!(
        loaded.file_state(file_pane),
        Some(SavedFileState {
            line: 42,
            column: 7,
            preview: true,
        })
    );

    // 파일 탭을 닫으면 해당 파일 칸의 SavedFileState도 정리된다.
    let mut closed = loaded.clone();
    closed.close_tab(1);
    assert_eq!(closed.file_state(file_pane), None);

    // 꼬리 없는 기존 v6(9-튜플) 바이트는 기본 꼬리로 하위 호환 복원된다.
    let v6_no_tail_path = temp_path("owe-842-v6-no-tail");
    let base = Layout::default();
    let mut raw = vec![FORMAT_VERSION];
    raw.extend(
        postcard::to_stdvec(&(
            &base.tabs,
            base.active,
            base.sidebar_width,
            base.panel_width,
            base.next_pane,
            &base.sessions,
            &base.launches,
            &base.files,
            &base.hosts,
        ))
        .unwrap(),
    );
    fs::create_dir_all(v6_no_tail_path.parent().unwrap()).unwrap();
    fs::write(&v6_no_tail_path, raw).unwrap();
    assert_eq!(Layout::load(&v6_no_tail_path).unwrap(), base);
}

/// OWE-842: 재스폰용 최신 cwd는 폴더(`launches`)와 분리 보관되어 `layout.bin` 꼬리로 영속되고,
/// 칸을 닫으면 함께 정리된다.
#[test]
fn owe_842_respawn_cwd_persists_separately_and_prunes_with_pane() {
    let mut l = Layout::default();
    let pane = l.active_tab().focused;
    l.set_session(pane, SessionId(11));
    l.set_launch(
        pane,
        Create {
            program: Program::Login,
            cwd: b"/repo/tree".to_vec(),
            cols: 80,
            rows: 24,
            account: None,
        },
    );
    // 같은 cwd면 저장 불필요, 빈 cwd·없는 칸이면 무시.
    assert!(l.set_respawn_cwd(pane, b"/repo/tree/src".to_vec()));
    assert!(!l.set_respawn_cwd(pane, b"/repo/tree/src".to_vec()));
    assert!(!l.set_respawn_cwd(pane, Vec::new()));
    assert!(!l.set_respawn_cwd(PaneId(999), b"/tmp/x".to_vec()));
    assert_eq!(l.respawn_cwd(pane), Some(&b"/repo/tree/src"[..]));

    let path = temp_path("owe-842-respawn");
    l.save(&path).unwrap();
    let loaded = Layout::load(&path).unwrap();
    assert_eq!(loaded, l);
    // 폴더는 워크트리 루트 그대로, 재스폰 cwd는 최신 cwd로 각각 복원.
    assert_eq!(
        loaded.launch(pane).map(|c| c.cwd.as_slice()),
        Some(&b"/repo/tree"[..])
    );
    assert_eq!(loaded.respawn_cwd(pane), Some(&b"/repo/tree/src"[..]));

    // 칸을 닫으면(탭 닫기) 재스폰 보관도 정리된다.
    let mut closed = loaded.clone();
    closed.close_tab(0);
    assert!(closed.tabs().is_empty());
    assert_eq!(closed.respawn_cwd(pane), None);
    assert!(closed.is_valid());
}

/// OWE-842: `respawn_cwds`가 붙기 전 중간 커밋의 5-필드 꼬리도 읽힌다(재스폰 보관만 비어 있음).
#[test]
fn owe_842_legacy_five_field_tail_loads_with_empty_respawn() {
    #[derive(Serialize)]
    struct OldTail<'a> {
        window_bounds: &'a Option<SavedWindowBounds>,
        sidebar_collapsed: &'a bool,
        panel_collapsed: &'a bool,
        panel_tab: &'a crate::diff::PanelTab,
        file_states: &'a BTreeMap<PaneId, SavedFileState>,
    }
    let mut l = Layout::default();
    let file_pane = l.open_file(PathBuf::from("/repo/src/lib.rs"));
    l.sidebar_collapsed = true;
    l.set_file_state(
        file_pane,
        SavedFileState {
            line: 1,
            column: 2,
            preview: false,
        },
    );

    let path = temp_path("owe-842-legacy-tail");
    let panel_tab = l.panel_tab();
    let file_states = BTreeMap::from([(file_pane, l.file_state(file_pane).expect("file state"))]);
    let mut raw = vec![FORMAT_VERSION];
    raw.extend(
        postcard::to_stdvec(&(
            &l.tabs,
            l.active,
            l.sidebar_width,
            l.panel_width,
            l.next_pane,
            &l.sessions,
            &l.launches,
            &l.files,
            &l.hosts,
        ))
        .unwrap(),
    );
    raw.extend(
        postcard::to_stdvec(&OldTail {
            window_bounds: &l.window_bounds,
            sidebar_collapsed: &l.sidebar_collapsed,
            panel_collapsed: &l.panel_collapsed,
            panel_tab: &panel_tab,
            file_states: &file_states,
        })
        .unwrap(),
    );
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, raw).unwrap();

    let loaded = Layout::load(&path).unwrap();
    assert_eq!(loaded, l);
    assert!(loaded.sidebar_collapsed);
    assert_eq!(loaded.respawn_cwd(file_pane), None);
}

/// OWE-870: 왼쪽 탭 닫기는 기준 탭 왼쪽의 비고정 탭만 닫고 핀 고정 탭은 유지한다.
#[test]
fn close_tabs_to_left_skips_pinned_tabs() {
    let mut l = Layout::default(); // tab 0 (pane 0)
    l.new_tab(); // tab 1 (pane 1) - pinned
    l.new_tab(); // tab 2 (pane 2) - keep
    l.new_tab(); // tab 3 (pane 3)
    l.toggle_pin_tab(1);
    l.close_tabs_to_left(2);
    let panes: Vec<PaneId> = l.tabs().iter().map(|t| t.focused).collect();
    assert_eq!(panes, vec![PaneId(1), PaneId(2), PaneId(3)]);
    assert!(l.is_tab_pinned(0));
    assert!(l.is_valid());
    // 범위를 벗어나면 아무 일도 없다.
    let before = l.clone();
    l.close_tabs_to_left(99);
    assert_eq!(l, before);
}

/// OWE-870: 탭 사용자 지정 제목은 다듬고·비우면 지우며 `layout.bin`에 저장·복원된다.
#[test]
fn tab_custom_title_normalizes_and_persists() {
    let mut l = Layout::default();
    assert_eq!(l.tab_title(0), None);
    l.set_tab_title(0, Some("  서버 로그  "));
    assert_eq!(l.tab_title(0), Some("서버 로그"));
    l.set_tab_title(0, Some("   "));
    assert_eq!(l.tab_title(0), None);
    l.set_tab_title(0, Some("a\u{0}b"));
    assert_eq!(l.tab_title(0), Some("ab"));
    l.set_tab_title(99, Some("무시"));
    assert_eq!(l.tab_title(99), None);

    let path = temp_path("custom-title");
    l.set_tab_title(0, Some("배포 탭"));
    l.save(&path).unwrap();
    let loaded = Layout::load(&path).unwrap();
    assert_eq!(loaded, l);
    assert_eq!(loaded.tab_title(0), Some("배포 탭"));
}

/// OWE-870: v6 레이아웃(핀·색상 있음, 제목 없음)은 핀·색상을 유지하고 제목 없이 읽힌다.
#[test]
fn v6_layout_migrates_with_pin_color_and_no_title() {
    #[derive(Serialize)]
    struct OldTab<'a> {
        root: &'a PaneTree,
        focused: PaneId,
        pinned: bool,
        color: Option<TabColor>,
    }
    let path = temp_path("v6");
    let mut l = Layout::default();
    l.new_tab();
    l.toggle_pin_tab(0);
    l.set_tab_color(1, Some(TabColor::Blue));
    let tabs: Vec<OldTab<'_>> = l
        .tabs
        .iter()
        .map(|t| OldTab {
            root: &t.root,
            focused: t.focused,
            pinned: t.pinned,
            color: t.color,
        })
        .collect();
    let mut old = vec![6];
    old.extend(
        postcard::to_stdvec(&(
            &tabs,
            l.active,
            l.sidebar_width,
            l.panel_width,
            l.next_pane,
            &l.sessions,
            &l.launches,
            &l.files,
        ))
        .unwrap(),
    );
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, old).unwrap();
    let loaded = Layout::load(&path).unwrap();
    assert_eq!(loaded, l);
    assert!(loaded.is_tab_pinned(0));
    assert_eq!(loaded.tab_color(1), Some(TabColor::Blue));
    assert_eq!(loaded.tab_title(0), None);
    assert_eq!(loaded.tab_title(1), None);
}

/// OWE-871: 기하 방향 이동은 분할 배치대로 포커스를 옮기고, 없는 쪽이면 그대로 둔다.
#[test]
fn focus_direction_moves_geometrically() {
    let mut l = Layout::default(); // pane 0
    l.split(Axis::Horizontal); // pane 1이 오른쪽, 포커스는 1
    assert_eq!(l.focused(), Some(PaneId(1)));
    l.focus_direction(Direction::Left);
    assert_eq!(l.focused(), Some(PaneId(0)));
    l.focus_direction(Direction::Left);
    assert_eq!(l.focused(), Some(PaneId(0)));
    l.focus_direction(Direction::Up);
    assert_eq!(l.focused(), Some(PaneId(0)));
    l.focus_direction(Direction::Right);
    assert_eq!(l.focused(), Some(PaneId(1)));

    // 왼쪽 칸을 상하로 가른다: root H[ V[0, 2], 1 ].
    l.focus(PaneId(0));
    l.split(Axis::Vertical); // pane 2가 아래, 포커스는 2
    assert_eq!(l.focused(), Some(PaneId(2)));
    l.focus_direction(Direction::Up);
    assert_eq!(l.focused(), Some(PaneId(0)));
    l.focus_direction(Direction::Right);
    assert_eq!(l.focused(), Some(PaneId(1)));
    l.focus_direction(Direction::Down);
    assert_eq!(l.focused(), Some(PaneId(2)));
    assert!(l.is_valid());
}

/// 마지막 앱 생존 시각은 꼬리 끝 필드로 저장·복원되고, 그 필드가 없는 6-필드 꼬리(이전 버전)는
/// 나머지 꼬리를 그대로 읽되 시각은 `None`이다.
#[test]
fn last_seen_round_trips_and_old_tail_has_none() {
    let mut l = Layout::default();
    let pane = l.active_tab().focused;
    assert!(l.set_respawn_cwd(pane, b"/repo/sub".to_vec()));
    assert_eq!(l.last_seen_unix_ns(), None);
    l.set_last_seen_unix_ns(1_760_000_000_000_000_000);
    let path = temp_path("last-seen");
    l.save(&path).unwrap();
    let loaded = Layout::load(&path).unwrap();
    assert_eq!(loaded.last_seen_unix_ns(), Some(1_760_000_000_000_000_000));
    assert_eq!(loaded, l);

    // 시각 0은 varint 한 바이트(0x00)라 마지막 바이트를 떼면 이전 버전의 6-필드 꼬리가 된다.
    l.set_last_seen_unix_ns(0);
    l.save(&path).unwrap();
    let mut bytes = std::fs::read(&path).unwrap();
    assert_eq!(bytes.pop(), Some(0));
    std::fs::write(&path, &bytes).unwrap();
    let old = Layout::load(&path).unwrap();
    assert_eq!(old.last_seen_unix_ns(), None);
    assert_eq!(old.respawn_cwd(pane), Some(&b"/repo/sub"[..]));
    std::fs::remove_file(&path).ok();
}

#[test]
fn deactivate_hides_tabs_and_survives_close_and_restore() {
    let path = temp_path("deactivate");
    let mut layout = sample();
    let tabs = layout.tabs.len();
    layout.deactivate();
    assert_eq!(layout.focused(), None);
    assert!(layout.visible().is_empty());
    layout.close_tab(0);
    assert_eq!(layout.active(), tabs - 1);
    assert_eq!(layout.focused(), None);
    layout.save(&path).unwrap();
    let restored = Layout::load(&path).unwrap();
    assert_eq!(restored.focused(), None);
    assert_eq!(restored.tabs().len(), tabs - 1);
    layout.new_tab();
    assert_eq!(layout.active(), tabs - 1);
    assert!(layout.focused().is_some());
}

#[test]
fn file_tabs_and_previews_keep_identical_paths_on_different_hosts_separate() {
    let mut layout = Layout::default();
    let path = PathBuf::from("/repo/shared.rs");
    let remote = HostId::Remote("remote-a".into());
    let local_pane = layout.open_file_on(HostId::Local, path.clone());
    let remote_pane = layout.open_file_on(remote.clone(), path.clone());
    assert_ne!(local_pane, remote_pane);
    assert_eq!(layout.open_file_on(HostId::Local, path.clone()), local_pane);
    assert_eq!(layout.open_file_on(remote.clone(), path), remote_pane);
    let preview = PathBuf::from("/repo/preview.rs");
    let tabs: Vec<_> = (0..layout.tabs().len()).collect();
    let local_preview = layout.open_file_preview_on(HostId::Local, preview.clone(), &tabs);
    let tabs: Vec<_> = (0..layout.tabs().len()).collect();
    let remote_preview = layout.open_file_preview_on(remote.clone(), preview.clone(), &tabs);
    assert_ne!(local_preview, remote_preview);
    assert_eq!(layout.file(local_preview), Some(preview.as_path()));
    assert_eq!(layout.host(local_preview), &HostId::Local);
    assert_eq!(layout.host(remote_preview), &remote);
    assert!(layout.is_valid());
}

/// APP-RUNTIME-004: 시작 복원이 끝나기 전 임시 기본 레이아웃에서 연 탭은 원래 `PaneId`를 그대로 유지한 채 복원한
/// 레이아웃 뒤에 붙고, 저장된 탭·분할·세션·호스트·파일 매핑은 충돌 없는 새 `PaneId`로 옮겨 의미 그대로 보존된다.
/// 아무 의도 없는 placeholder 탭은 버리고, 런타임/입력 의도가 걸린 탭·확대·미리보기·열 너비는 유지한다.
#[test]
fn startup_edits_are_appended_to_the_restored_layout() {
    let mut saved = Layout::default();
    saved.set_session(PaneId(0), SessionId(7));
    saved.new_tab();
    saved.split(Axis::Horizontal);
    saved.set_zoomed_pane(Some(PaneId(2)));
    saved.set_host(PaneId(2), HostId::Remote("box".into()));
    saved.set_tab_title(1, Some("work"));
    let _saved_file = saved.open_file(PathBuf::from("/tmp/notes.txt"));
    let expected_tab_count = saved.tabs().len();

    let baseline = Layout::default();
    let mut edited = baseline.clone();
    // 로딩 중 Cmd+T + 분할 + 확대 + 단일 클릭 미리보기 + 열 너비 조절.
    edited.new_tab();
    let early_first = edited.focused().unwrap();
    assert_eq!(early_first, PaneId(1));
    edited.split(Axis::Vertical);
    let early_second = edited.focused().unwrap();
    edited.set_zoomed_pane(Some(early_second));
    edited.set_launch(
        early_first,
        Create {
            account: None,
            program: Program::Login,
            cwd: b"/work".to_vec(),
            cols: 80,
            rows: 24,
        },
    );
    edited.set_host(early_first, HostId::Remote("box".into()));
    let preview_pane = edited.open_file_preview(PathBuf::from("/tmp/preview-a.rs"));
    edited.sidebar_collapsed = true;
    edited.sidebar_width = 312.0;
    edited.panel_width = 428.0;

    let empty_panes = BTreeSet::new();
    let empty_tabs = BTreeSet::new();
    let mut merged = saved.clone();
    let tab_map = merged.absorb_startup_edits(&baseline, &edited, &empty_panes, &empty_tabs);
    assert!(merged.is_valid());
    assert_eq!(merged.tabs().len(), expected_tab_count + 2);
    assert_eq!(tab_map.get(&1).copied(), Some(expected_tab_count));
    assert_eq!(tab_map.get(&2).copied(), Some(expected_tab_count + 1));
    assert!(
        !tab_map.contains_key(&0),
        "untouched placeholder is dropped"
    );

    // 저장된 탭들의 구조·세션·호스트·확대·사용자 제목·파일 경로가 그대로 살아남는다.
    let restored_tab0 = merged.tab_panes(0);
    assert_eq!(restored_tab0.len(), 1);
    assert_eq!(merged.session(restored_tab0[0]), Some(SessionId(7)));

    let restored_tab1 = merged.tab_panes(1);
    assert_eq!(restored_tab1.len(), 2);
    assert_eq!(merged.tabs()[1].focused, restored_tab1[1]);
    assert_eq!(merged.host(restored_tab1[1]), &HostId::Remote("box".into()));
    assert_eq!(merged.tab_title(1), Some("work"));

    let restored_tab2 = merged.tab_panes(2);
    assert_eq!(restored_tab2.len(), 1);
    assert_eq!(
        merged.file(restored_tab2[0]),
        Some(Path::new("/tmp/notes.txt"))
    );

    // 초기 편집 탭의 PaneId는 바뀌지 않고 저장본과도 겹치지 않는다.
    let restored_all: BTreeSet<PaneId> = (0..expected_tab_count)
        .flat_map(|idx| merged.tab_panes(idx))
        .collect();
    assert!(!restored_all.contains(&early_first));
    assert!(!restored_all.contains(&early_second));
    assert!(!restored_all.contains(&preview_pane));
    assert_eq!(
        merged.tab_panes(expected_tab_count),
        vec![early_first, early_second]
    );
    assert_eq!(
        merged.launch(early_first).map(|c| c.cwd.as_slice()),
        Some(&b"/work"[..])
    );
    assert_eq!(merged.host(early_first), &HostId::Remote("box".into()));
    assert_eq!(merged.active(), expected_tab_count + 1);
    assert_eq!(
        merged.file(preview_pane),
        Some(Path::new("/tmp/preview-a.rs"))
    );
    // 단일 클릭 미리보기 슬롯도 유지되어 다음 미리보기가 같은 칸을 재사용한다.
    let reused = merged.open_file_preview(PathBuf::from("/tmp/preview-b.rs"));
    assert_eq!(reused, preview_pane);
    assert!(merged.sidebar_collapsed);
    assert!((merged.sidebar_width - 312.0).abs() < f32::EPSILON);
    assert!((merged.panel_width - 428.0).abs() < f32::EPSILON);

    merged.activate(1);
    assert_eq!(merged.zoomed_pane(), Some(restored_tab1[1]));
    merged.activate(expected_tab_count);
    assert_eq!(merged.zoomed_pane(), Some(early_second));

    // 아무것도 하지 않았으면(placeholder 탭만) 저장본이 그대로다.
    let mut untouched = saved.clone();
    let map = untouched.absorb_startup_edits(&baseline, &baseline, &empty_panes, &empty_tabs);
    assert!(map.is_empty());
    assert_eq!(untouched, saved);

    // placeholder 탭에 초기 입력/런타임 의도가 걸려 있으면 버리지 않고 원래 PaneId(0) 그대로 뒤에 남긴다.
    let retained_panes = BTreeSet::from([PaneId(0)]);
    let mut with_input = saved.clone();
    let map = with_input.absorb_startup_edits(&baseline, &baseline, &retained_panes, &empty_tabs);
    assert!(with_input.is_valid());
    assert_eq!(map.get(&0).copied(), Some(expected_tab_count));
    assert_eq!(with_input.tab_panes(expected_tab_count), vec![PaneId(0)]);
    assert_eq!(with_input.active(), expected_tab_count);

    // placeholder 탭을 닫기만 한 경우(보이는 탭 없음)도 저장본의 활성 탭을 유지한다.
    let mut closed = baseline.clone();
    closed.close_tab(0);
    let mut kept = saved.clone();
    kept.absorb_startup_edits(&baseline, &closed, &empty_panes, &empty_tabs);
    assert_eq!(kept.tabs(), saved.tabs());
    assert_eq!(kept.active(), saved.active());
}
