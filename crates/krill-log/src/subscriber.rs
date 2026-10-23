use std::fmt::{self, Write};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id, Record};
use tracing::{Event, Level, Metadata, Subscriber};

use crate::{RECORD_BYTES, RollingLog};

/// Event-only subscriber: no span trees, buffers, worker threads or shutdown flush to lose on SIGKILL.
#[derive(Debug)]
pub(crate) struct Logger {
    writer: RollingLog,
    next_span: AtomicU64,
}

impl Logger {
    pub(crate) const fn new(writer: RollingLog) -> Self {
        Self {
            writer,
            next_span: AtomicU64::new(1),
        }
    }
}

impl Subscriber for Logger {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        let target = metadata.target().split("::").next().unwrap_or_default();
        // `krill_log`: forwarded `log` warnings (gpui render path) keep their origin in a `source` field.
        metadata.is_event()
            && *metadata.level() <= Level::INFO
            && matches!(
                target,
                "krill_app" | "krilld" | "krill_holder" | "krill" | "krill_log"
            )
    }

    fn new_span(&self, _: &Attributes<'_>) -> Id {
        Id::from_u64(self.next_span.fetch_add(1, Ordering::Relaxed))
    }
    fn record(&self, _: &Id, _: &Record<'_>) {}
    fn record_follows_from(&self, _: &Id, _: &Id) {}
    fn enter(&self, _: &Id) {}
    fn exit(&self, _: &Id) {}

    fn event(&self, event: &Event<'_>) {
        if !self.enabled(event.metadata()) {
            return;
        }
        let millis = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_millis());
        let mut line = Line(String::with_capacity(RECORD_BYTES));
        write!(
            line,
            "{millis} {} pid={} {}",
            event.metadata().level(),
            std::process::id(),
            event.metadata().target()
        )
        .ok();
        event.record(&mut line);
        self.writer.append(&line.0).ok();
    }
}

struct Line(String);

impl Write for Line {
    fn write_str(&mut self, value: &str) -> fmt::Result {
        let remaining = (RECORD_BYTES - 1).saturating_sub(self.0.len());
        let end = value.floor_char_boundary(value.len().min(remaining));
        // Keep each event on one line, including fields containing control characters.
        for character in value.get(..end).unwrap_or_default().chars() {
            self.0.push(if character.is_control() {
                ' '
            } else {
                character
            });
        }
        Ok(())
    }
}

impl Visit for Line {
    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        write!(self, " {}={value:?}", field.name()).ok();
    }
}
