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
        "5-1 * * * *",
        ", * * * *",
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
                enabled: true,
                ..Default::default()
            },
            Schedule {
                id: "isolated".into(),
                cron: "* * * * *".into(),
                cwd: TEST_CWD.into(),
                command: vec!["/bin/echo".into()],
                prompt: "isolated-ok".into(),
                precheck: Some("exit 0".into()),
                workspace_mode: WorkspaceMode::Isolate,
                enabled: true,
                ..Default::default()
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
            ..Default::default()
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
            ..Default::default()
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
            ..Default::default()
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
            ..Default::default()
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
            ..Default::default()
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
                enabled: true,
                ..Default::default()
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
        drop(tokenize_command_line(&text));
        drop(parse_schedule_to_cron(&text));
        let _ = classify_schedule(&text);
    }
}

#[test]
fn rrule_intervals_must_divide_the_cron_field_cycle() {
    for (expr, expected) in [
        ("FREQ=MINUTELY;INTERVAL=15", "*/15 * * * *"),
        ("FREQ=MINUTELY;INTERVAL=60", "0 * * * *"),
        ("FREQ=HOURLY;INTERVAL=6;BYMINUTE=0", "0 */6 * * *"),
        ("FREQ=HOURLY;INTERVAL=1;BYMINUTE=15", "15 * * * *"),
    ] {
        assert_eq!(parse_schedule_to_cron(expr).unwrap(), expected, "{expr}");
    }
    for expr in [
        "FREQ=MINUTELY;INTERVAL=7",
        "FREQ=MINUTELY;INTERVAL=61",
        "FREQ=HOURLY;INTERVAL=7;BYMINUTE=0",
        "FREQ=HOURLY;INTERVAL=25;BYMINUTE=0",
    ] {
        assert!(parse_schedule_to_cron(expr).is_err(), "{expr}");
    }
}

#[test]
fn presets_rrules_weekday_aliases_and_occurrences() {
    for (expr, expected, preset) in [
        (
            "FREQ=HOURLY;BYMINUTE=15",
            "15 * * * *",
            SchedulePreset::Hourly,
        ),
        (
            "RRULE:FREQ=DAILY;BYHOUR=9;BYMINUTE=30",
            "30 9 * * *",
            SchedulePreset::Daily,
        ),
        (
            "FREQ=WEEKLY;BYDAY=MO,TU,WE,TH,FR;BYHOUR=9",
            "0 9 * * 1,2,3,4,5",
            SchedulePreset::Weekdays,
        ),
        (
            "FREQ=WEEKLY;BYDAY=MO;BYHOUR=9",
            "0 9 * * 1",
            SchedulePreset::Weekly,
        ),
        (
            "FREQ=MINUTELY;INTERVAL=15",
            "*/15 * * * *",
            SchedulePreset::Custom,
        ),
    ] {
        assert_eq!(parse_schedule_to_cron(expr).unwrap(), expected);
        assert_eq!(classify_schedule(expr).preset, preset);
        assert!(Cron::parse(expr).is_ok());
    }
    for expr in [
        "FREQ=YEARLY",
        "FREQ=DAILY;INTERVAL=2",
        "FREQ=WEEKLY;BYDAY=BOGUS",
        "FREQ=DAILY;COUNT=2",
        "FREQ=DAILY;BYHOUR=24",
    ] {
        assert!(parse_schedule_to_cron(expr).is_err(), "{expr}");
    }
    assert!(
        Cron::parse("5/15 0 * * mon-fri")
            .unwrap()
            .matches([35, 0, 1, 1, 1])
    );
    assert!(
        !Cron::parse("5/15 0 * * mon-fri")
            .unwrap()
            .matches([30, 0, 1, 1, 1])
    );
    assert_eq!(classify_schedule("0 9 * * SUN").day_of_week, 0);
    assert_eq!(
        build_preset_cron(SchedulePreset::Weekly, 9, 30, 1).unwrap(),
        "30 9 * * 1"
    );
    assert!(build_preset_cron(SchedulePreset::Daily, 24, 0, 1).is_err());
    assert!(build_preset_cron(SchedulePreset::Custom, 9, 0, 1).is_err());
    assert_eq!(
        format_schedule_summary("30 9 * * 1-5", true),
        "평일 09:30 (UTC)"
    );
    assert_eq!(
        format_schedule_summary("30 9 * * 7", false),
        "Weekly on Sun at 09:30 (UTC)"
    );
    assert_eq!(utc_calendar_from_epoch_minute(0), (1970, 1, 1, 0, 0, 4));
    assert_eq!(
        utc_calendar_from_epoch_minute(28_486_834),
        (2024, 2, 29, 12, 34, 4)
    );
    let cron = Cron::parse("35 12 29 2 *").unwrap();
    assert_eq!(cron.next_minute_after(28_486_834), Some(28_486_835));
    let next = cron.next_minute_after(28_486_835).unwrap();
    assert_eq!(
        utc_calendar_from_epoch_minute(next),
        (2028, 2, 29, 12, 35, 2)
    );
    assert_eq!(
        cron.latest_minute_at_or_before(28_486_840, 5),
        Some(28_486_835)
    );
    assert_eq!(cron.latest_minute_at_or_before(28_486_840, 4), None);
    assert_eq!(
        Cron::parse("0 0 31 2 *").unwrap().next_minute_after(0),
        None
    );
    assert_eq!(cron.next_minute_after(u64::MAX), None);
}

#[test]
fn optional_fields_validate_and_legacy_defaults_roundtrip() {
    let legacy =
        "id = 'job'\ncron = '* * * * *'\ncwd = '/tmp'\ncommand = ['claude']\nprompt = ''\n";
    let entry: Schedule = toml::from_str(legacy).unwrap();
    let serialized = toml::to_string(&entry).unwrap();
    for key in [
        "name",
        "agent =",
        "extra_agent_args",
        "precheck_timeout_secs",
        "base_branch",
        "reuse_session",
        "missed_run_grace_minutes",
    ] {
        assert!(!serialized.contains(key), "{serialized}");
    }
    assert!(entry.enabled);
    assert_eq!(entry.effective_grace_minutes(), 0);
    assert_eq!(entry.effective_precheck_timeout_secs(), 60);
    assert_eq!(entry.display_name(), "job");
    assert_eq!(entry.resolved_agent(), Some("claude"));
    let entry = Schedule {
        cwd: TEST_CWD.into(),
        ..entry
    };
    entry.validate().unwrap();
    for bad in [
        Schedule {
            name: Some("bad\nname".into()),
            ..entry.clone()
        },
        Schedule {
            agent: Some(" ".into()),
            ..entry.clone()
        },
        Schedule {
            extra_agent_args: Some("positional".into()),
            ..entry.clone()
        },
        Schedule {
            extra_agent_args: Some("--model 'oops".into()),
            ..entry.clone()
        },
        Schedule {
            extra_agent_args: Some("--model x --".into()),
            ..entry.clone()
        },
        Schedule {
            extra_agent_args: Some("--model x".into()),
            reuse_session: true,
            ..entry.clone()
        },
        Schedule {
            workspace_mode: WorkspaceMode::Isolate,
            reuse_session: true,
            ..entry.clone()
        },
        Schedule {
            precheck_timeout_secs: Some(0),
            ..entry.clone()
        },
        Schedule {
            precheck_timeout_secs: Some(601),
            ..entry.clone()
        },
        Schedule {
            missed_run_grace_minutes: Some(10_081),
            ..entry.clone()
        },
        Schedule {
            base_branch: Some("-bad".into()),
            ..entry.clone()
        },
        Schedule {
            base_branch: Some("bad branch".into()),
            ..entry.clone()
        },
    ] {
        assert!(bad.validate().is_err(), "{bad:?}");
    }
    let extended = Schedule {
        name: Some("  Daily review  ".into()),
        extra_agent_args: Some("--model 'two words'".into()),
        precheck_timeout_secs: Some(123),
        missed_run_grace_minutes: Some(720),
        base_branch: Some("dev".into()),
        ..entry
    };
    extended.validate().unwrap();
    assert_eq!(extended.display_name(), "Daily review");
    assert_eq!(
        toml::from_str::<Schedule>(&toml::to_string(&extended).unwrap()).unwrap(),
        extended
    );
    assert_eq!(
        tokenize_command_line("cmd 'two words' \"\" one\\ two").unwrap(),
        ["cmd", "two words", "", "one two"]
    );
    assert!(tokenize_command_line("abc\\").is_err());
    assert_eq!(
        slugify_schedule_id("  Hello, 세계! World  ", &["hello-world".into()]),
        "hello-world-2"
    );
    assert_eq!(slugify_schedule_id("한글", &[]), "automation");
}

#[test]
fn coalescing_keeps_metadata_and_separates_reused_session_runs() {
    let first = ScheduleRun {
        id: "a".into(),
        minute: 7,
        phase: "started".into(),
        trigger: Some("manual".into()),
        run_number: Some(1),
        duration_ms: Some(20),
        timed_out: Some(false),
        ..Default::default()
    };
    let spawned = ScheduleRun {
        phase: "spawned".into(),
        session: Some(9),
        ..first.clone()
    };
    let second = ScheduleRun {
        run_number: Some(2),
        ..first.clone()
    };
    let second_spawned = ScheduleRun {
        run_number: Some(2),
        ..spawned.clone()
    };
    let finish = ScheduleRun {
        id: "a".into(),
        minute: 7,
        phase: "finished".into(),
        session: Some(9),
        run_number: Some(2),
        ..Default::default()
    };
    let runs = coalesce_runs(&[first, spawned, second, second_spawned, finish]);
    assert_eq!(runs.len(), 2);
    assert_eq!(runs[0].phase, "finished");
    assert_eq!(runs[0].trigger.as_deref(), Some("manual"));
    assert_eq!(runs[0].duration_ms, Some(20));
    assert_eq!(runs[0].timed_out, Some(false));
    assert_eq!(runs[1].run_number, Some(1));
}
