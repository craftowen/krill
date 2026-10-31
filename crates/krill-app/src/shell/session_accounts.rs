//! The sidebar menu's target is independent of focused panes and global defaults.

use super::{Context, Link, Shell, sidebar};
use krill_proto::usage::UsageRequest;

impl Shell {
    pub(super) fn switch_sidebar_session_account(
        &mut self,
        target: sidebar::SwitchSessionAccount,
        cx: &mut Context<Self>,
    ) {
        let host = target.host.clone();
        if self.defer_account_mutation(&host) {
            let source = self.target_for_host(&host);
            self.account_deferred.push((
                source,
                super::account_input::DeferredMutation::Sidebar(target),
            ));
            return;
        }
        let daemon = if host.is_local() {
            self.paths
                .clone()
                .filter(|_| self.link == Link::Up)
                .map(krill_app::hosts::HostTarget::local)
        } else {
            self.target_for_host(&host)
        };
        let Some(daemon) = daemon else {
            self.sidebar.update(cx, |s, cx| {
                s.finish_session_account_switch(&host, target.session, cx);
            });
            self.notice = Some(
                krill_app::i18n::catalog::settings::ACCOUNTS_DAEMON_REQUIRED
                    .get()
                    .into(),
            );
            cx.notify();
            return;
        };
        self.begin_account_mutation(&host);
        let stamp = self.account_operation_stamp(&host);
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    krill_app::accounts::request_target_with_features(
                        &daemon,
                        &UsageRequest::SwitchSessionAccount {
                            session: target.session,
                            provider: target.provider,
                            account_id: target.account_id,
                        },
                    )
                })
                .await;
            this.update(cx, |s, cx| {
                let current = s.account_operation_is_current(&stamp);
                s.finish_account_mutation(&host, cx);
                if current {
                    match result {
                        Ok((snapshot, features)) => {
                            s.sidebar.update(cx, |sidebar, cx| {
                                sidebar.set_session_account_features(host.clone(), features, cx);
                            });
                            s.apply_accounts_snapshot_for_host(host.clone(), snapshot, cx);
                        }
                        Err(error) => {
                            s.notice = Some(error);
                            cx.notify();
                        }
                    }
                }
                // Apply the daemon stage before releasing the API stage so the
                // profile badge keeps its original one-second grace deadline.
                s.sidebar.update(cx, |sidebar, cx| {
                    sidebar.finish_session_account_switch(&host, target.session, cx);
                });
            })
            .ok();
        })
        .detach();
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use gpui::TestAppContext;
    use krill_proto::SessionId;
    use krill_proto::ipc::{self, ClientHello, ServerHello};
    use krill_proto::screen::Msg as _;
    use krill_proto::usage::{
        AccountsSnapshot, FEAT_PROVIDER_DETAILS, FEAT_SESSION_ACCOUNT_SWITCH, FEAT_USAGE,
        ProviderKind, UsageReply,
    };
    use std::io::Write as _;
    use std::sync::{Arc, Mutex};

    #[gpui::test]
    fn sidebar_switch_sends_clicked_remote_session_while_active_pane_stays_local(
        cx: &mut TestAppContext,
    ) {
        let root = std::env::temp_dir().join(format!("profile-target-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let socket = root.join("daemon.sock");
        let listener = krill_proto::os::net::UnixListener::bind(&socket).unwrap();
        let captured = Arc::new(Mutex::new(None));
        let capture = captured.clone();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                .unwrap();
            let hello = ServerHello {
                proto_min: ipc::PROTO_MIN,
                proto_max: ipc::PROTO_MAX,
                features: FEAT_USAGE | FEAT_PROVIDER_DETAILS | FEAT_SESSION_ACCOUNT_SWITCH,
                build: "synthetic".into(),
                instance_id: 9997,
            };
            stream.write_all(&hello.encode().unwrap()).unwrap();
            let (kind, body) = ipc::read_frame(&mut stream).unwrap().unwrap();
            let client = ClientHello::decode(kind, &body).unwrap();
            let negotiated = ipc::negotiate(&hello, &client).unwrap();
            let (kind, body) = ipc::read_frame(&mut stream).unwrap().unwrap();
            *capture.lock().unwrap() = Some(UsageRequest::decode(kind, &body).unwrap());
            let reply =
                UsageReply::Accounts(AccountsSnapshot::default()).for_features(negotiated.features);
            stream.write_all(&reply.encode().unwrap()).unwrap();
        });
        let (shell, _) = super::super::tests::open(
            cx,
            super::super::tests::state_path("sidebar-profile-target"),
        );
        let host = krill_app::hosts::HostId::Remote("profile-studio".into());
        shell.update(cx, |s, cx| {
            let pane = s.layout.focused().unwrap();
            s.layout.set_host(pane, krill_app::hosts::HostId::Local);
            s.layout.set_session(pane, SessionId(10));
            s.host_targets.insert(
                host.clone(),
                krill_app::hosts::HostTarget::remote("profile-studio", socket),
            );
            s.switch_sidebar_session_account(
                sidebar::SwitchSessionAccount {
                    host: host.clone(),
                    session: SessionId(20),
                    provider: ProviderKind::Claude,
                    account_id: "other".into(),
                },
                cx,
            );
        });
        cx.run_until_parked();
        server.join().unwrap();
        cx.run_until_parked();
        assert_eq!(
            *captured.lock().unwrap(),
            Some(UsageRequest::SwitchSessionAccount {
                session: SessionId(20),
                provider: ProviderKind::Claude,
                account_id: "other".into(),
            })
        );
        shell.read_with(cx, |s, _| {
            let pane = s.layout.focused().unwrap();
            assert_eq!(s.layout.host(pane), &krill_app::hosts::HostId::Local);
            assert_eq!(s.layout.session(pane), Some(SessionId(10)));
        });
        std::fs::remove_dir_all(root).unwrap();
    }
}
