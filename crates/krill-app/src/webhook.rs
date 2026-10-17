//! MK-086 endpoint settings and transport shared with the session daemon (MK-126).
#[path = "../../krilld/src/webhook/transport.rs"]
mod transport;
#[cfg(test)]
use transport::validated_url;
pub use transport::{Endpoint, deliver, deliver_transition};

#[cfg(test)]
pub(crate) mod tests;
