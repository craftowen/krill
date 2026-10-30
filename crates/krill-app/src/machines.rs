//! Machine management groups registered routes without changing their session authority.

use std::collections::BTreeMap;

use krill_proto::ipc::MachineIdentity;

use crate::hosts::{HostId, HostStatus};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum MachineKey {
    Known(MachineIdentity),
    Unknown(HostId),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouteKind {
    Local,
    Ssh,
    Invite,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MachineRoute {
    pub host: HostId,
    pub label: String,
    pub kind: RouteKind,
    pub status: HostStatus,
    pub identity: Option<MachineIdentity>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Machine {
    pub key: MachineKey,
    pub routes: Vec<MachineRoute>,
}

impl Machine {
    #[must_use]
    pub fn is_local(&self) -> bool {
        self.routes.iter().any(|r| r.kind == RouteKind::Local)
    }

    #[must_use]
    pub fn label(&self) -> &str {
        self.routes.first().map_or("", |r| r.label.as_str())
    }

    #[must_use]
    pub fn status(&self) -> HostStatus {
        if self
            .routes
            .iter()
            .any(|r| r.status == HostStatus::Connected)
        {
            HostStatus::Connected
        } else if self
            .routes
            .iter()
            .any(|r| r.status == HostStatus::Connecting)
        {
            HostStatus::Connecting
        } else {
            HostStatus::Disconnected
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddMethod {
    Invite,
    Ssh,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum MachinePage {
    #[default]
    Overview,
    Details(HostId),
    Add(AddMethod),
    ThisComputer,
}

#[must_use]
pub fn project(routes: impl IntoIterator<Item = MachineRoute>) -> Vec<Machine> {
    let mut groups: BTreeMap<MachineKey, Vec<MachineRoute>> = BTreeMap::new();
    for route in routes {
        let key = route.identity.map_or_else(
            || MachineKey::Unknown(route.host.clone()),
            MachineKey::Known,
        );
        groups.entry(key).or_default().push(route);
    }
    let mut machines: Vec<_> = groups
        .into_iter()
        .map(|(key, mut routes)| {
            routes
                .sort_by(|a, b| (!a.host.is_local(), &a.host).cmp(&(!b.host.is_local(), &b.host)));
            Machine { key, routes }
        })
        .collect();
    machines.sort_by(|a, b| {
        (!a.is_local(), a.label(), &a.key).cmp(&(!b.is_local(), b.label(), &b.key))
    });
    machines
}

#[cfg(test)]
mod tests {
    use super::*;

    fn route(id: &str, identity: Option<u8>, status: HostStatus) -> MachineRoute {
        MachineRoute {
            host: HostId::Remote(id.into()),
            label: "Studio".into(),
            kind: if id.starts_with("iroh:") {
                RouteKind::Invite
            } else {
                RouteKind::Ssh
            },
            status,
            identity: identity.map(|n| MachineIdentity([n; 16])),
        }
    }

    #[test]
    fn equal_verified_identity_keeps_both_connection_methods() {
        let machines = project([
            route("ssh", Some(7), HostStatus::Disconnected),
            route("iroh:peer", Some(7), HostStatus::Connected),
        ]);
        assert_eq!(machines.len(), 1);
        assert_eq!(machines[0].routes.len(), 2);
        assert_eq!(machines[0].status(), HostStatus::Connected);
        assert!(
            machines[0]
                .routes
                .iter()
                .any(|r| r.kind == RouteKind::Invite)
        );
        assert!(machines[0].routes.iter().any(|r| r.kind == RouteKind::Ssh));
    }

    #[test]
    fn identical_names_without_equal_identity_remain_separate() {
        for ids in [[None, None], [Some(1), None], [Some(1), Some(2)]] {
            assert_eq!(
                project([
                    route("ssh", ids[0], HostStatus::Connected),
                    route("iroh:peer", ids[1], HostStatus::Connected),
                ])
                .len(),
                2
            );
        }
    }

    #[test]
    fn removing_one_route_preserves_the_machine_and_remaining_route() {
        let machines = project([route("iroh:peer", Some(7), HostStatus::Connected)]);
        assert_eq!(machines[0].key, MachineKey::Known(MachineIdentity([7; 16])));
        assert_eq!(
            machines[0].routes[0].host,
            HostId::Remote("iroh:peer".into())
        );
    }

    #[test]
    fn this_computer_is_first_and_can_include_a_verified_loopback_route() {
        let machines = project([
            route("ssh", Some(7), HostStatus::Connected),
            MachineRoute {
                host: HostId::Local,
                label: "This computer".into(),
                kind: RouteKind::Local,
                status: HostStatus::Connected,
                identity: Some(MachineIdentity([7; 16])),
            },
            route("other", None, HostStatus::Disconnected),
        ]);
        assert_eq!(machines.len(), 2);
        assert!(machines[0].is_local());
        assert_eq!(machines[0].label(), "This computer");
        assert_eq!(machines[0].routes.len(), 2);
    }
}
