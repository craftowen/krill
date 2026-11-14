#![deny(clippy::indexing_slicing, clippy::print_stdout, clippy::print_stderr)]
//! krilld: 세션 데몬. 세션마다 Term(핫 1,000행)과 PTY 리더 스레드를 둔다(ARCHITECTURE §4). 클라이언트는 UDS 하나로 붙는다([`ipc`]).

// 단위 테스트도 데몬과 같은 할당기에서 돌아, 힙 계측 테스트가 실제 크기 등급을 본다.
#[cfg(all(test, not(windows)))]
#[global_allocator]
static TEST_ALLOCATOR: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

pub mod agent;
mod agent_history;
pub mod alloc;
mod appearance;
pub mod attach;
mod child;
mod cli;
pub mod git;
pub mod holder;
pub mod ipc;
pub mod mobile;
pub mod monitor;
mod os;
pub mod registry;
pub mod rejoin;
pub mod remote_fs;
mod resources;
pub mod scope;
pub mod search;
mod session;
mod shell;
pub mod status;
pub mod store;
mod sys;
mod term;
mod upgrade;
pub mod usage;
mod watch;

pub use appearance::{Appearance, CellSize};
pub use session::{Gap, PARK_GRACE, Session};
#[cfg(windows)]
pub use shell::run_windows_shell;
pub use shell::{Launch, Shell};
pub use store::{Recorder, Store};
#[cfg(unix)]
pub use sys::notify_on_stop_signal;
#[cfg(windows)]
pub use sys::run_console_probe;
#[cfg(windows)]
pub use sys::set_hard_error_dialogs;
pub use term::HOT_LINES;

#[cfg(test)]
mod log_redaction_tests {
    use std::fmt::{self, Write as _};
    use std::fs;
    use std::sync::{Arc, Mutex};
    use tracing::field::{Field, Visit};
    use tracing::span::{Attributes, Id, Record};
    use tracing::{Event, Metadata, Subscriber};

    #[derive(Default)]
    struct CaptureSubscriber {
        events: Arc<Mutex<Vec<String>>>,
    }

    struct LineVisitor(String);

    impl Visit for LineVisitor {
        fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
            if !self.0.is_empty() {
                self.0.push(' ');
            }
            self.0.push_str(field.name());
            self.0.push('=');
            write!(self.0, "{value:?}").ok();
        }

        fn record_str(&mut self, field: &Field, value: &str) {
            if !self.0.is_empty() {
                self.0.push(' ');
            }
            self.0.push_str(field.name());
            self.0.push('=');
            self.0.push_str(value);
        }
    }

    impl Subscriber for CaptureSubscriber {
        fn enabled(&self, _metadata: &Metadata<'_>) -> bool {
            true
        }

        fn new_span(&self, _span: &Attributes<'_>) -> Id {
            Id::from_u64(1)
        }

        fn record(&self, _span: &Id, _values: &Record<'_>) {}

        fn record_follows_from(&self, _span: &Id, _follows: &Id) {}

        fn event(&self, event: &Event<'_>) {
            let mut visitor = LineVisitor(String::new());
            event.record(&mut visitor);
            if let Ok(mut guard) = self.events.lock() {
                guard.push(visitor.0);
            }
        }

        fn enter(&self, _span: &Id) {}

        fn exit(&self, _span: &Id) {}
    }

    #[test]
    fn trust_config_warnings_omit_paths() {
        let root =
            std::env::temp_dir().join(format!("krill-secret-home-marker-{}", std::process::id()));
        fs::remove_dir_all(&root).ok();
        let project = root.join("workspace");
        assert!(fs::create_dir_all(&project).is_ok());

        let events = Arc::new(Mutex::new(Vec::new()));
        let subscriber = CaptureSubscriber {
            events: Arc::clone(&events),
        };

        tracing::subscriber::with_default(subscriber, || {
            let config = root.join(".claude.json");
            assert!(fs::write(&config, b"{bad json").is_ok());
            assert!(matches!(
                crate::agent::pre_trust_claude_in_with_hook(&root, None, &project, None),
                Ok(false)
            ));

            assert!(fs::write(&config, [0xff_u8, 0xfe]).is_ok());
            assert!(matches!(
                crate::agent::pre_trust_claude_in_with_hook(&root, None, &project, None),
                Ok(false)
            ));
        });

        fs::remove_dir_all(&root).ok();
        let recorded = events.lock().map_or_else(|_| Vec::new(), |g| g.clone());
        assert_eq!(recorded.len(), 2);
        for line in &recorded {
            assert!(
                !line.contains("secret-home-marker") && !line.contains(".claude.json"),
                "path leaked into tracing event: {line}"
            );
        }
    }

    #[test]
    fn non_test_tracing_calls_avoid_paths_and_raw_error_formatting() {
        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut stack = vec![src];
        let mut violations = Vec::new();
        let forbidden = [
            ".display()",
            "%e,",
            "%e)",
            "%e ",
            "%error",
            "%err,",
            "%err)",
            "?e,",
            "?e)",
            "?error,",
            "?error)",
            "?error ",
            "?error\n",
            "?err,",
            "?err)",
            "?res,",
            "?res)",
            "?res\n",
            "{e}",
            "{error}",
            "{err}",
        ];
        while let Some(dir) = stack.pop() {
            let Ok(entries) = fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    if path.file_name().and_then(|n| n.to_str()) != Some("tests") {
                        stack.push(path);
                    }
                    continue;
                }
                if path.extension().and_then(|e| e.to_str()) != Some("rs")
                    || path.file_name().and_then(|n| n.to_str()) == Some("tests.rs")
                {
                    continue;
                }
                let Ok(text) = fs::read_to_string(&path) else {
                    continue;
                };
                // Skip unit test module trailing block in source files.
                let body = text.split("#[cfg(test)]").next().unwrap_or(&text);
                let mut offset = 0usize;
                while let Some(pos) = body.get(offset..).and_then(|s| s.find("tracing::")) {
                    let start = offset + pos;
                    let end = body
                        .get(start..)
                        .and_then(|s| s.find(");"))
                        .map_or(body.len(), |n| start + n + 2);
                    if let Some(call) = body.get(start..end)
                        && forbidden.iter().any(|tok| call.contains(tok))
                    {
                        let line = body.get(..start).map_or(1, |p| p.lines().count());
                        violations.push(format!("{}:{line}", path.display()));
                    }
                    offset = end;
                }
            }
        }
        assert!(
            violations.is_empty(),
            "tracing call sites violating krill-log privacy rule: {violations:?}"
        );
    }
}
