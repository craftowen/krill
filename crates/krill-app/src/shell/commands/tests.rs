use std::fmt::Write as _;

use gpui::{ClipboardItem, TestAppContext};
use krill_proto::input::Input;
use krill_proto::screen::{
    Cell, HistoryRequest, HistoryRows, Msg as _, Op, Revision, Seq, encode_update,
};

use crate::shell::tests::{layout_of, open, state_path};
use crate::shell::{Duration, UnixStream, ipc};

#[gpui::test]
fn quick_command_click_path_and_context_clipboard(cx: &mut TestAppContext) {
    let (view, cx) = open(cx, state_path("commands"));
    let (ours, mut theirs) = UnixStream::pair().unwrap();
    theirs
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let pane = layout_of(&view, cx).active_tab().focused;
    let d = state_path("commands-config")
        .parent()
        .unwrap()
        .to_path_buf();
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(
        d.join("settings.toml"),
        "copy_context_lines = 3\n[[quick_commands]]\nname = \"상태\"\ntext = \"git status\"\n",
    )
    .unwrap();
    cx.update(|_, cx| krill_app::settings::init_at(Some(d.clone()), cx));
    cx.update(|_, cx| {
        view.update(cx, |s, cx| {
            s.attach_test(pane, krill_proto::SessionId(3), ours, cx);
            let row = |t: &str| {
                t.chars()
                    .map(|ch| Cell {
                        ch,
                        ..Cell::default()
                    })
                    .collect()
            };
            let ops = vec![
                Op::Reset { cols: 20, lines: 3 },
                Op::Line {
                    line: 0,
                    cells: row("현재"),
                },
                Op::Line {
                    line: 1,
                    cells: row("출력"),
                },
            ];
            for f in encode_update(Revision(1), Seq(0), ops).unwrap() {
                let (k, b) = ipc::read_frame(&mut &f[..]).unwrap().unwrap();
                assert!(s.on_frame(pane, k, &b, cx));
            }
        });
    });
    cx.run_until_parked();
    let bounds = cx.debug_bounds("quick-commands").unwrap();
    cx.simulate_click(bounds.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    // 저장된 항목을 메뉴에서 선택한다. 실행용 Enter는 PTY에 덧붙이지 않는다.
    cx.simulate_keystrokes("down enter");
    cx.run_until_parked();
    let (k, body) = ipc::read_frame(&mut theirs).unwrap().unwrap();
    assert_eq!(Input::decode(k, &body).unwrap().bytes, b"git status");
    cx.write_to_clipboard(ClipboardItem::new_string("이전".into()));
    cx.run_until_parked();
    let bounds = cx.debug_bounds("copy-context").unwrap();
    cx.simulate_click(bounds.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    assert_eq!(
        cx.read_from_clipboard().unwrap().text().as_deref(),
        Some("이전")
    );
    let (k, body) = ipc::read_frame(&mut theirs).unwrap().unwrap();
    let req = HistoryRequest::decode(k, &body).unwrap();
    let rows = HistoryRows {
        req: req.req,
        skip: 0,
        total: 1,
        pages: 0,
        rows: vec![
            "과거"
                .chars()
                .map(|ch| Cell {
                    ch,
                    ..Cell::default()
                })
                .collect(),
        ],
    };
    let f = rows.encode().unwrap();
    let (k, body) = ipc::read_frame(&mut &f[..]).unwrap().unwrap();
    cx.update(|_, cx| {
        view.update(cx, |s, cx| {
            assert!(s.on_frame(pane, k, &body, cx));
        });
    });
    assert_eq!(
        cx.read_from_clipboard().unwrap().text().as_deref(),
        Some("과거\n현재\n출력")
    );
    std::fs::remove_dir_all(d).unwrap();
}

#[gpui::test]
fn commands_large_menu_filters_and_injects_original_command(cx: &mut TestAppContext) {
    let (view, cx) = open(cx, state_path("commands-large"));
    let (ours, mut theirs) = UnixStream::pair().unwrap();
    theirs
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let pane = layout_of(&view, cx).active_tab().focused;
    let dir = state_path("commands-large-config")
        .parent()
        .unwrap()
        .to_path_buf();
    std::fs::create_dir_all(&dir).unwrap();
    let config = (0..100).fold(String::new(), |mut config, i| {
        writeln!(
            config,
            "[[quick_commands]]\nname = \"command {i}\"\ntext = \"echo {i}\""
        )
        .unwrap();
        config
    });
    std::fs::write(dir.join("settings.toml"), config).unwrap();
    cx.update(|_, cx| {
        krill_app::settings::init_at(Some(dir.clone()), cx);
        view.update(cx, |shell, cx| {
            shell.attach_test(pane, krill_proto::SessionId(3), ours, cx);
        });
    });
    cx.run_until_parked();
    let trigger = cx.debug_bounds("quick-commands").unwrap();
    cx.simulate_click(trigger.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    assert!(cx.debug_bounds("quick-command-search-list").is_some());
    assert!(
        cx.debug_bounds("quick-command-99").is_none(),
        "offscreen commands must not render"
    );
    cx.simulate_input("command 99");
    cx.run_until_parked();
    let result = cx.debug_bounds("quick-command-99").unwrap();
    assert!(cx.debug_bounds("quick-command-0").is_none());
    cx.simulate_click(result.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    let (kind, body) = ipc::read_frame(&mut theirs).unwrap().unwrap();
    assert_eq!(Input::decode(kind, &body).unwrap().bytes, b"echo 99");
    std::fs::remove_dir_all(dir).unwrap();
}
