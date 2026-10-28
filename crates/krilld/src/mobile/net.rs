//! IPv4 선택. 신규 직접 연결은 Tailscale만 허용하고 loopback은 격리 QA에 쓴다.
use krill_proto::mobile::{ConnectMode, NetworkInterface};
use std::io;
use std::net::{Ipv4Addr, SocketAddrV4};
#[cfg(unix)]
use std::process::Command;

pub fn excluded(name: &str) -> bool {
    ["docker", "br-", "vmnet", "vboxnet", "veth", "bridge"]
        .iter()
        .any(|p| name.starts_with(p))
}
pub fn tailscale(ip: Ipv4Addr) -> bool {
    (u32::from(ip) & 0xffc0_0000) == 0x6440_0000
}

#[cfg(unix)]
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
        .filter(|(name, ip)| !excluded(name) && tailscale(*ip))
        .map(|(name, ip)| NetworkInterface {
            default_route: default.as_ref() == Some(&name),
            name,
            address: ip.to_string(),
        })
        .collect())
}

#[cfg(windows)]
pub fn detect() -> io::Result<Vec<NetworkInterface>> {
    Ok(crate::sys::ipv4_interfaces()?
        .into_iter()
        .filter(|(name, ip, _)| !excluded(name) && tailscale(*ip))
        .map(|(name, address, default_route)| NetworkInterface {
            name,
            address: address.to_string(),
            default_route,
        })
        .collect())
}

/// 수동 주소가 지정되면 자동 인터페이스 후보보다 우선한다.
pub fn select(
    interfaces: &[NetworkInterface],
    mode: ConnectMode,
    custom: Option<&str>,
) -> io::Result<SocketAddrV4> {
    if mode == ConnectMode::Tunnel {
        let port = custom.and_then(|c| parse(c).ok()).map_or(0, |s| s.port());
        return Ok(SocketAddrV4::new(Ipv4Addr::LOCALHOST, port));
    }
    let custom = custom.map(parse).transpose()?;
    if let Some(address) = custom {
        if (mode == ConnectMode::Tailscale && !tailscale(*address.ip()))
            || (mode == ConnectMode::Lan && !address.ip().is_loopback())
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "select a Tailscale address",
            ));
        }
        return Ok(address);
    }
    let chosen = interfaces
        .iter()
        .filter(|i| !excluded(&i.name))
        .filter_map(|i| Some((i, i.address.parse::<Ipv4Addr>().ok()?)))
        .filter(|(_, ip)| mode == ConnectMode::Tailscale && tailscale(*ip))
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
        return Ok(SocketAddrV4::new(ip, 0));
    }
    Err(io::Error::new(
        io::ErrorKind::AddrNotAvailable,
        "no mobile network interface",
    ))
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
    fn only_tailscale_or_explicit_loopback_can_select_direct_address() {
        let all = vec![
            nic("en0", "192.168.1.2", true),
            nic("utun5", "100.64.1.2", false),
        ];
        assert_eq!(
            select(&all, ConnectMode::Tailscale, None)
                .unwrap()
                .ip()
                .to_string(),
            "100.64.1.2"
        );
        assert!(select(&all, ConnectMode::Lan, None).is_err());
        assert!(select(&all, ConnectMode::Lan, Some("192.168.1.2:8765")).is_err());
        assert!(select(&all, ConnectMode::Tailscale, Some("192.168.1.2:8765")).is_err());
        assert!(select(&all, ConnectMode::Tailscale, Some("127.0.0.1:8765")).is_err());
        assert_eq!(
            select(&all, ConnectMode::Lan, Some("127.0.0.1:8765"))
                .unwrap()
                .port(),
            8765
        );
        assert_eq!(
            select(&all, ConnectMode::Tunnel, None).unwrap().ip(),
            &Ipv4Addr::LOCALHOST
        );
        assert!(excluded("docker0"));
        assert!(tailscale("100.127.255.255".parse().unwrap()));
        assert!(!tailscale("100.128.0.0".parse().unwrap()));
    }
}
