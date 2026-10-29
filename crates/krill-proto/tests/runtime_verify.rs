use krill_proto::SessionId;
use krill_proto::control::{AgentStatus, Reply, RootShell, SessionStatus, ShellOs};
use krill_proto::ipc::{MAX_FRAME, read_frame};
use krill_proto::screen::{self, Msg, Op, Revision, Seq, Update, View};

#[test]
fn independent_cursor_max_values_split_old_reader_and_malformed_markers() {
    for explicit in [None, Some(false), Some(true)] {
        let ops = vec![Op::Title("한".repeat(341)); 2300];
        let frames =
            screen::encode_update_with_cursor(Revision(u64::MAX), Seq(u64::MAX), ops, explicit)
                .unwrap();
        assert!(frames.len() > 1);
        let mut view = View::default();
        for f in frames {
            assert!(f.len() <= MAX_FRAME + 4);
            let (kind, body) = read_frame(&mut f.as_slice()).unwrap().unwrap();
            let old: Update = postcard::from_bytes(&body).unwrap();
            assert_eq!(old.cursor_explicit, None);
            let new = Update::decode(kind, &body).unwrap();
            assert_eq!(new.cursor_explicit, if new.done { explicit } else { None });
            assert_eq!(new.ops, old.ops);
            view.apply(new).unwrap();
        }
        assert_eq!(view.revision, Some(Revision(u64::MAX)));
        assert_eq!(view.screen.cursor_explicit, explicit.unwrap_or(false));
    }
    let update = Update {
        revision: Revision(1),
        seq: Seq(1),
        done: true,
        ops: vec![],
        cursor_explicit: None,
    };
    for invalid in 2..=255 {
        let mut b = postcard::to_stdvec(&update).unwrap();
        b.push(invalid);
        assert!(Update::decode(screen::kind::UPDATE, &b).is_err());
    }
}

#[test]
fn independent_status_multirow_root_tail_boundaries_and_truncation() {
    let mut rows: Vec<_> = (1..=3)
        .map(|id| SessionStatus {
            session: SessionId(id),
            status: AgentStatus::UNKNOWN,
        })
        .collect();
    for text in ["a".repeat(256), "한".repeat(85)] {
        rows[1].status.root_shell = Some(Box::new(RootShell {
            os: ShellOs::Windows,
            program: text,
        }));
        let expected = Reply::States(rows.clone());
        let frame = expected.encode().unwrap();
        let (kind, body) = read_frame(&mut frame.as_slice()).unwrap().unwrap();
        assert_eq!(Reply::decode(kind, &body).unwrap(), expected);
        let old: Reply = postcard::from_bytes(&body).unwrap();
        let Reply::States(old_rows) = old else {
            panic!()
        };
        assert!(old_rows.iter().all(|r| r.status.root_shell.is_none()));
        assert!(Reply::decode(kind, &body[..body.len() - 1]).is_err());
    }
}

#[test]
fn invalid_observed_root_name_cannot_poison_other_status_rows() {
    let observed = "sh\ncmd".to_owned();
    // A legacy producer can observe a legal Unix filename containing control characters.
    let root = Some(observed).filter(|name| !name.is_empty() && name.len() <= 256);
    assert!(root.is_some());
    let mut status = AgentStatus::UNKNOWN;
    status.root_shell = root.map(|program| {
        Box::new(RootShell {
            os: ShellOs::Unix,
            program,
        })
    });
    let reply = Reply::States(vec![
        SessionStatus {
            session: SessionId(1),
            status: AgentStatus::UNKNOWN,
        },
        SessionStatus {
            session: SessionId(2),
            status,
        },
    ]);
    let frame = reply.encode().unwrap();
    let (kind, body) = read_frame(&mut frame.as_slice()).unwrap().unwrap();
    let Reply::States(rows) = Reply::decode(kind, &body)
        .expect("invalid observed names must not poison valid status rows")
    else {
        panic!("expected states")
    };
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().all(|row| row.status.root_shell.is_none()));
}
