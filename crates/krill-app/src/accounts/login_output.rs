//! OAuth links may arrive on either pipe; Codex does not consistently use BROWSER.
//! Reference: Orca src/main/codex-accounts/codex-login-session.ts and
//! src/main/claude-accounts/claude-login-session.ts, Copyright (c) 2026 Lovecast Inc., MIT.

use std::io::Read;
use std::process::Child;
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};

use krill_proto::usage::ProviderKind;

pub(super) fn watch(child: &mut Child, provider: ProviderKind) -> Receiver<String> {
    let (tx, rx) = sync_channel(4);
    if let Some(stdout) = child.stdout.take() {
        drain(stdout, provider, tx.clone());
    }
    if let Some(stderr) = child.stderr.take() {
        drain(stderr, provider, tx);
    }
    rx
}

fn drain(mut pipe: impl Read + Send + 'static, provider: ProviderKind, tx: SyncSender<String>) {
    std::thread::spawn(move || {
        let mut bytes = [0; 4096];
        let mut pending = Vec::new();
        let mut published = false;
        loop {
            let count = match pipe.read(&mut bytes) {
                Ok(0) | Err(_) => break,
                Ok(n) => n,
            };
            if published {
                continue;
            } // Always drain both pipes, including after finding the link.
            pending.extend_from_slice(bytes.get(..count).unwrap_or_default());
            if let Some(url) = link(provider, &pending) {
                tx.try_send(url).ok();
                published = true;
                pending.clear();
            } else if pending.len() > 16 * 1024 {
                pending.drain(..pending.len().saturating_sub(8 * 1024));
            }
        }
    });
}

fn link(provider: ProviderKind, bytes: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(bytes);
    for (start, _) in text.match_indices("https://") {
        let tail = text.get(start..)?;
        // A chunk ending halfway through the query must never open a truncated OAuth URL.
        let end = tail.find(|c: char| {
            c.is_whitespace() || c.is_control() || matches!(c, '\'' | '"' | '<' | '>')
        })?;
        let candidate = tail.get(..end)?;
        if let Some(url) = super::authorize_url(provider, candidate) {
            return Some(url.to_owned());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    proptest::proptest! {
        #[test]
        fn arbitrary_cli_output_does_not_produce_untrusted_links(bytes in proptest::collection::vec(proptest::prelude::any::<u8>(), 0..4096)) {
            for provider in ProviderKind::ALL {
                if let Some(url) = link(provider, &bytes) {
                    proptest::prop_assert!(super::super::authorize_url(provider, &url).is_some());
                }
            }
        }
    }
    #[test]
    fn printed_links_wait_for_complete_url_and_reject_untrusted_hosts() {
        let partial = b"Open browser: https://auth.openai.com/authorize?client_id=fixture&state=";
        assert!(link(ProviderKind::Codex, partial).is_none());
        let mut full = partial.to_vec();
        full.extend_from_slice(b"complete\n");
        assert!(
            link(ProviderKind::Codex, &full)
                .unwrap()
                .ends_with("state=complete")
        );
        assert!(
            link(
                ProviderKind::Codex,
                b"https://auth.openai.com.evil.test/auth\n"
            )
            .is_none()
        );
    }

    #[test]
    fn claude_uses_the_local_browser_callback_instead_of_the_printed_code_link() {
        let manual = "https://claude.com/cai/oauth/authorize?redirect_uri=https%3A%2F%2Fplatform.claude.com%2Foauth%2Fcode%2Fcallback&state=s\n";
        assert!(link(ProviderKind::Claude, manual.as_bytes()).is_none());
        let local = "https://claude.com/cai/oauth/authorize?redirect_uri=http%3A%2F%2Flocalhost%3A5000%2Fcallback&state=s\n";
        assert_eq!(
            link(ProviderKind::Claude, local.as_bytes()).as_deref(),
            Some(local.trim())
        );
        assert!(
            link(
                ProviderKind::Claude,
                local.replace("localhost", "attacker.test").as_bytes()
            )
            .is_none()
        );
        assert!(
            link(
                ProviderKind::Claude,
                local
                    .replace("claude.com/", "claude.com.evil.test/")
                    .as_bytes()
            )
            .is_none()
        );
    }

    #[test]
    #[cfg(unix)]
    fn oauth_stdin_stays_open_and_both_output_streams_are_drained() {
        use std::io::Write as _;
        use std::process::{Command, Stdio};
        use std::time::Duration;
        let mut child = Command::new("/bin/sh").args(["-c", "printf 'https://auth.openai.com/authorize?state=fixture\\n' >&2; read answer; [ \"$answer\" = finish ]"])
            .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
        let links = watch(&mut child, ProviderKind::Codex);
        assert!(
            links
                .recv_timeout(Duration::from_secs(5))
                .unwrap()
                .ends_with("state=fixture")
        );
        assert!(
            child.try_wait().unwrap().is_none(),
            "EOF cancels real Claude login"
        );
        child.stdin.take().unwrap().write_all(b"finish\n").unwrap();
        assert!(child.wait().unwrap().success());
    }
}
