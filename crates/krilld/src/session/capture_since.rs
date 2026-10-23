//! Cheap CLI pattern probes. A stamp never substitutes for raw-stream capture/ACK semantics.
use super::{Tap, lock};
use krill_proto::control::{CaptureFormat, CaptureStamp, Reply};
use krill_proto::screen::{Revision, Seq};

impl Tap {
    pub(crate) fn capture_plain_since(
        &self,
        since: Option<CaptureStamp>,
    ) -> std::io::Result<Reply> {
        let live = lock(&self.0.live);
        let stamp = CaptureStamp {
            session: self.id(),
            revision: Revision(live.revision),
            seq: Seq(live.seq),
        };
        let bytes = if since == Some(stamp) {
            None
        } else {
            Some(live.engine.capture_cli(CaptureFormat::Plain)?)
        };
        Ok(Reply::CaptureSince { stamp, bytes })
    }
}
