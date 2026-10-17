//! IPv4 인터페이스 선택. 가상 브리지를 제외하고 Tailscale, 기본 경로 LAN, 나머지 LAN, 수동 주소 순이다.
use krill_proto::mobile::{ConnectMode, NetworkInterface};
use std::io;
use std::net::{Ipv4Addr, SocketAddrV4};
use std::process::Command;

pub fn excluded(name: &str) -> bool {
    ["docker", "br-", "vmnet", "vboxnet", "veth", "bridge"]
        .iter()
        .any(|p| name.starts_with(p))
}
pub fn tailscale(ip: Ipv4Addr) -> bool {
    (u32::from(ip) & 0xffc0_0000) == 0x6440_0000
}

pub fn detect() -> io::Result<Vec<NetworkInterface>> {
    let default = Command::new("/sbin/route")
        .args(["-n", "get", "default"])
        .output()
        .ok()
        .and_then(|out| String::from_utf8(out.stdout).ok())
        .and_then(|text| {
            text.lines().find_map(|line| {
                line.trim()
                    .strip_prefix("interface:")
                    .map(|v| v.trim().to_owned())
            })
        });
    Ok(crate::sys::ipv4_interfaces()?
        .into_iter()
        .filter(|(name, ip)| !excluded(name) && (tailscale(*ip) || ip.is_private()))
        .map(|(name, ip)| NetworkInterface {
            default_route: default.as_ref() == Some(&name),
            name,
            address: ip.to_string(),
        })
        .collect())
}

/// 수동 선택은 별도로 보존한다. 자동 후보가 일시적으로 사라지면 저장된 주소로 돌아간다.
pub fn select(
    interfaces: &[NetworkInterface],
    mode: ConnectMode,
    custom: Option<&str>,
) -> io::Result<SocketAddrV4> {
    if mode == ConnectMode::Tunnel {
        return Ok(SocketAddrV4::new(
            Ipv4Addr::LOCALHOST,
            custom.map(parse).transpose()?.map_or(0, |s| s.port()),
        ));
    }
    let port = custom.map(parse).transpose()?.map_or(0, |s| s.port());
    let chosen = interfaces
        .iter()
        .filter(|i| !excluded(&i.name))
        .filter_map(|i| Some((i, i.address.parse::<Ipv4Addr>().ok()?)))
        .filter(|(_, ip)| tailscale(*ip) || (mode == ConnectMode::Lan && ip.is_private()))
        .min_by_key(|(i, ip)| {
            (
                if tailscale(*ip) {
                    0
                } else if i.default_route {
                    1
                } else {
                    2
                },
                i.name.clone(),
                *ip,
            )
        });
    if let Some((_, ip)) = chosen {
        return Ok(SocketAddrV4::new(ip, port));
    }
    custom.map(parse).transpose()?.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::AddrNotAvailable,
            "no mobile network interface",
        )
    })
}
fn parse(value: &str) -> io::Result<SocketAddrV4> {
    let address: SocketAddrV4 = value
        .parse()
        .or_else(|_| value.parse::<Ipv4Addr>().map(|ip| SocketAddrV4::new(ip, 0)))
        .map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "mobile address must be IPv4 or IPv4:port",
            )
        })?;
    if address.ip().is_unspecified() || address.ip().is_multicast() || address.ip().is_broadcast() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "mobile address must select one interface",
        ));
    }
    Ok(address)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn nic(name: &str, ip: &str, default_route: bool) -> NetworkInterface {
        NetworkInterface {
            name: name.into(),
            address: ip.into(),
            default_route,
        }
    }
    #[test]
    fn priority_exclusions_and_manual_fallback() {
        let mut all = vec![
            nic("docker0", "100.64.0.1", true),
            nic("en1", "192.168.1.2", false),
            nic("en0", "10.1.1.2", true),
            nic("utun5", "100.127.255.254", false),
        ];
        assert_eq!(
            select(&all, ConnectMode::Lan, None)
                .unwrap()
                .ip()
                .to_string(),
            "100.127.255.254"
        );
        all.pop();
        assert_eq!(
            select(&all, ConnectMode::Lan, None)
                .unwrap()
                .ip()
                .to_string(),
            "10.1.1.2"
        );
        assert!(select(&all, ConnectMode::Tailscale, None).is_err());
        for name in [
            "docker0",
            "br-test",
            "vmnet1",
            "vboxnet0",
            "veth123",
            "bridge100",
        ] {
            assert!(excluded(name));
        }
        for ip in ["100.64.0.0", "100.127.255.255"] {
            assert!(tailscale(ip.parse().unwrap()));
        }
        for ip in ["100.63.255.255", "100.128.0.0"] {
            assert!(!tailscale(ip.parse().unwrap()));
        }
        assert_eq!(
            select(&[], ConnectMode::Lan, Some("192.168.2.9:8765"))
                .unwrap()
                .to_string(),
            "192.168.2.9:8765"
        );
        assert!(select(&[], ConnectMode::Lan, Some("0.0.0.0:80")).is_err());
        assert_eq!(
            select(&all, ConnectMode::Tunnel, None).unwrap().ip(),
            &Ipv4Addr::LOCALHOST
        );
    }
}
