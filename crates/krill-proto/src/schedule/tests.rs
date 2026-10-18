use super::*;

const TEST_CWD: &str = if cfg!(windows) { "C:/tmp" } else { "/tmp" };

fn parse_fixture(text: &str) -> io::Result<Schedules> {
    Schedules::parse(&text.replace("\"/tmp\"", &format!("\"{TEST_CWD}\"")))
}

#[test]
fn cron_lists_ranges_steps_and_day_or_semantics() {
    let cron = Cron::parse("*/15 9-17/2 1,15 1-12 1-5").unwrap();
    assert!(cron.matches([30, 11, 2, 10, 1]));
    assert!(cron.matches([30, 11, 15, 10, 6]));
    assert!(!cron.matches([31, 11, 15, 10, 1]));
    assert!(!cron.matches([30, 10, 15, 10, 1]));
    assert!(!cron.matches([30, 11, 2, 10, 6]));
    assert!(Cron::parse("0 0 * * 7").unwrap().matches([0, 0, 1, 1, 0]));
    assert!(!Cron::parse("0 0 * * 7").unwrap().matches([0, 0, 1, 1, 1]));
    assert!(Cron::parse("0 0 1 * *").unwrap().matches([0, 0, 1, 1, 6]));
    assert!(!Cron::parse("0 0 1 * *").unwrap().matches([0, 0, 2, 1, 6]));
}

#[test]
fn invalid_cron_and_configuration_are_rejected() {
    for text in [
        "",
        "* * * *",
        "* * * * * *",
        "60 * * * *",
        "* 24 * * *",
        "* * 0 * *",
        "* * * 13 *",
        "* * * * 8",
        "*/0 * * * *",
        "*/999 * * * *",
        "1/2 * * * *",
        "5-1 * * * *",
        ", * * * *",
        "* * * * MON",
    ] {
        assert!(Cron::parse(text).is_err(), "{text}");
    }
    let text = r#"[[schedule]]
id = "test"
cron = "* * * * *"
cwd = "/tmp"
command = ["/bin/echo"]
prompt = "한글 $(literal)"
"#;
    let value = parse_fixture(text).unwrap();
    let roundtrip = parse_fixture(&toml::to_string(&value).unwrap()).unwrap();
    assert_eq!(
        roundtrip.schedule.first().unwrap().prompt,
        "한글 $(literal)"
    );
    assert!(parse_fixture(&format!("{text}\n{text}")).is_err());
    assert!(parse_fixture(&text.replace("/tmp", "relative")).is_err());
    assert!(parse_fixture(&text.replace("test", "../escape")).is_err());
    assert!(parse_fixture(&text.replace("[\"/bin/echo\"]", "[]")).is_err());

    let extended = r#"[[schedule]]
id = "isolated-check"
cron = "0 9 * * 1-5"
cwd = "/tmp"
command = ["/bin/echo"]
prompt = "check"
precheck = "test -f ready"
workspace_mode = "isolate"
"#;
    let parsed = parse_fixture(extended).unwrap();
    let entry = parsed.schedule.first().unwrap();
    assert_eq!(entry.precheck.as_deref(), Some("test -f ready"));
    assert_eq!(entry.workspace_mode, WorkspaceMode::Isolate);
    assert!(parse_fixture(&extended.replace("test -f ready", "   ")).is_err());

    let empty_prompt = r#"[[schedule]]
id = "no-prompt"
cron = "0 0 1 1 *"
cwd = "/tmp"
command = ["/bin/echo", "two words"]
prompt = ""
"#;
    let parsed_empty = parse_fixture(empty_prompt).unwrap();
    assert_eq!(parsed_empty.schedule[0].prompt, "");
    let omitted_prompt = r#"[[schedule]]
id = "omitted-prompt"
cron = "0 0 1 1 *"
cwd = "/tmp"
command = ["/bin/echo", "two words"]
"#;
    assert_eq!(
        parse_fixture(omitted_prompt).unwrap().schedule[0].prompt,
        ""
    );
    assert!(
        parse_fixture(&empty_prompt.replace("prompt = \"\"", "prompt = \"a\\u0000b\"")).is_err()
    );
}

#[test]
fn run_now_queue_and_coalesced_journal_roundtrip() {
    let state = std::env::temp_dir().join(format!("mk186-proto-{}", std::process::id()));
    fs::remove_dir_all(&state).ok();
    fs::create_dir_all(&state).unwrap();

    let schedules = Schedules {
        schedule: vec![
            Schedule {
                id: "gated".into(),
                cron: "* * * * *".into(),
                cwd: TEST_CWD.into(),
                command: vec!["/bin/echo".into()],
                prompt: "should-not-run".into(),
                precheck: Some("echo 'no work' && exit 1".into()),
                workspace_mode: WorkspaceMode::Reuse,
            },
            Schedule {
                id: "isolated".into(),
                cron: "* * * * *".into(),
                cwd: TEST_CWD.into(),
                command: vec!["/bin/echo".into()],
                prompt: "isolated-ok".into(),
                precheck: Some("exit 0".into()),
                workspace_mode: WorkspaceMode::Isolate,
            },
        ],
    };
    schedules.save(&state).unwrap();

    assert!(request_run_now(&state, "unknown").is_err());
    request_run_now(&state, "gated").unwrap();
    request_run_now(&state, "isolated").unwrap();
    request_run_now(&state, "gated").unwrap();
    assert_eq!(
        take_run_now(&state).unwrap(),
        vec!["gated".to_owned(), "isolated".to_owned()]
    );
    assert!(take_run_now(&state).unwrap().is_empty());

    let records = [
        ScheduleRun {
            id: "gated".into(),
            minute: 100,
            phase: "started".into(),
            session: None,
            error: None,
            status: None,
            cwd: None,
            output: None,
        },
        ScheduleRun {
            id: "gated".into(),
            minute: 100,
            phase: "skipped".into(),
            session: None,
            error: Some("precheck exited with status 1".into()),
            status: Some(1),
            cwd: None,
            output: Some("no work".into()),
        },
        ScheduleRun {
            id: "isolated".into(),
            minute: 101,
            phase: "started".into(),
            session: None,
            error: None,
            status: None,
            cwd: None,
            output: None,
        },
        ScheduleRun {
            id: "isolated".into(),
            minute: 101,
            phase: "spawned".into(),
            session: Some(7),
            error: None,
            status: None,
            cwd: Some("/tmp/wt/schedule-isolated-101".into()),
            output: None,
        },
        ScheduleRun {
            id: "isolated".into(),
            minute: 101,
            phase: "finished".into(),
            session: Some(7),
            error: None,
            status: Some(0),
            cwd: None,
            output: Some("isolated-ok".into()),
        },
    ];
    let mut jsonl = String::new();
    for r in &records {
        jsonl.push_str(&serde_json::to_string(r).unwrap());
        jsonl.push('\n');
    }
    fs::write(state.join(RUNS), jsonl).unwrap();

    let coalesced = load_coalesced_runs(&state).unwrap();
    assert_eq!(coalesced.len(), 2);
    assert_eq!(coalesced[0].id, "isolated");
    assert_eq!(coalesced[0].phase, "finished");
    assert_eq!(
        coalesced[0].cwd.as_deref(),
        Some("/tmp/wt/schedule-isolated-101")
    );
    assert_eq!(coalesced[0].output.as_deref(), Some("isolated-ok"));
    assert_eq!(coalesced[1].id, "gated");
    assert_eq!(coalesced[1].phase, "skipped");
    assert_eq!(coalesced[1].status, Some(1));
    assert_eq!(coalesced[1].output.as_deref(), Some("no work"));

    fs::remove_dir_all(&state).ok();
}

#[test]
fn concurrent_request_run_now_and_take_run_now_never_lose_triggers() {
    use std::sync::{Arc, Barrier};

    let state = std::env::temp_dir().join(format!("owe754-proto-{}", std::process::id()));
    fs::remove_dir_all(&state).ok();
    fs::create_dir_all(&state).unwrap();

    let count = 32_usize;
    let schedules = Schedules {
        schedule: (0..count)
            .map(|i| Schedule {
                id: format!("job-{i}"),
                cron: "* * * * *".into(),
                cwd: TEST_CWD.into(),
                command: vec!["/bin/echo".into()],
                prompt: "ok".into(),
                precheck: None,
                workspace_mode: WorkspaceMode::Reuse,
            })
            .collect(),
    };
    schedules.save(&state).unwrap();

    let barrier = Arc::new(Barrier::new(2));
    let writer_dir = state.clone();
    let writer_barrier = Arc::clone(&barrier);
    let writer = std::thread::spawn(move || {
        writer_barrier.wait();
        for i in 0..count {
            request_run_now(&writer_dir, &format!("job-{i}")).unwrap();
        }
    });

    let reader_dir = state.clone();
    let reader_barrier = Arc::clone(&barrier);
    let reader = std::thread::spawn(move || {
        reader_barrier.wait();
        let mut collected = HashSet::new();
        for _ in 0..count.saturating_mul(2) {
            for id in take_run_now(&reader_dir).unwrap() {
                collected.insert(id);
            }
        }
        collected
    });

    writer.join().unwrap();
    let mut collected = reader.join().unwrap();
    for id in take_run_now(&state).unwrap() {
        collected.insert(id);
    }
    assert_eq!(collected.len(), count);
    for i in 0..count {
        assert!(collected.contains(&format!("job-{i}")), "missing job-{i}");
    }

    fs::remove_dir_all(&state).ok();
}

proptest::proptest! {
    #[test]
    fn parsers_never_panic(text in ".{0,2048}", values in proptest::array::uniform5(0_u32..100)) {
        if let Ok(cron) = Cron::parse(&text) { let _ = cron.matches(values); }
        drop(parse_fixture(&text));
    }
}
