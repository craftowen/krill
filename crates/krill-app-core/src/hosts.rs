//! MK-131: 앱 계층 호스트 타깃·원격 연결 관리자 브리지.
//!
//! UI·패인·사이드바·패널 계층은 SSH 전용 타입 대신 전송 무관 [`HostId`]·[`HostTarget`]·[`HostStatus`]·
//! [`HostManager`]만 사용한다.

pub use krill_hosts::{
    HostId, HostManager, HostStatus, HostTarget, RemoteConnector, RemoteHostSpec,
};

use crate::ssh_hosts::SshHost;

/// 로컬 `hosts.toml`의 [`SshHost`] 목록을 전송 무관 [`RemoteHostSpec`] 목록으로 변환한다.
pub fn specs_from_ssh_hosts(hosts: &[SshHost]) -> Vec<RemoteHostSpec> {
    hosts
        .iter()
        .filter(|h| !h.id.trim().is_empty() && !h.destination.trim().is_empty())
        .map(|h| {
            let label = if h.label.trim().is_empty() {
                h.destination.clone()
            } else {
                h.label.trim().to_owned()
            };
            RemoteHostSpec {
                id: h.id.clone(),
                label,
                address: h.destination.clone(),
                port: h.port,
                identity_file: h.identity_file.clone(),
                remote_socket: h.remote_socket.clone(),
            }
        })
        .collect()
}

pub mod link;
