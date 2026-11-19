//! Which addresses the server may reach for a member, such as for `connections.test`.
//!
//! An address inside the server's own networks is refused unless the installation allows it with
//! `ROOKERY_OUTBOUND_ALLOW`: loopback, link-local (where clouds keep their metadata service), private
//! and shared ranges, multicast, and the addresses of the server's own interfaces. Otherwise a member
//! could make the server call its neighbours and learn what they answer.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

/// A range of addresses: `10.0.0.0/8`, `fd00::/8`, or one address.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AddrRange {
    addr: IpAddr,
    prefix: u8,
}

impl AddrRange {
    pub fn parse(text: &str) -> Result<AddrRange, String> {
        let text = text.trim();
        let (addr, prefix) = match text.split_once('/') {
            Some((addr, prefix)) => (addr, Some(prefix)),
            None => (text, None),
        };
        let addr: IpAddr = addr
            .parse()
            .map_err(|_| format!("{text:?} is not an address or a range such as 10.0.0.0/8"))?;
        let max = if addr.is_ipv4() { 32 } else { 128 };
        let prefix = match prefix {
            Some(p) => p
                .parse::<u8>()
                .ok()
                .filter(|p| *p <= max)
                .ok_or_else(|| format!("{text:?} has a prefix longer than {max}"))?,
            None => max,
        };
        Ok(AddrRange { addr, prefix })
    }

    /// Ranges apart by commas.
    pub fn parse_list(text: &str) -> Result<Vec<AddrRange>, String> {
        text.split(',')
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .map(AddrRange::parse)
            .collect()
    }

    pub fn contains(&self, ip: IpAddr) -> bool {
        match (self.addr, canonical(ip)) {
            (IpAddr::V4(net), IpAddr::V4(ip)) => {
                let mask = u32::MAX.checked_shl(32 - self.prefix as u32).unwrap_or(0);
                u32::from(net) & mask == u32::from(ip) & mask
            }
            (IpAddr::V6(net), IpAddr::V6(ip)) => {
                let mask = u128::MAX.checked_shl(128 - self.prefix as u32).unwrap_or(0);
                u128::from(net) & mask == u128::from(ip) & mask
            }
            _ => false,
        }
    }
}

impl std::fmt::Display for AddrRange {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{}", self.addr, self.prefix)
    }
}

/// An IPv4 address written as IPv6 (`::ffff:10.0.0.1`) is the IPv4 address.
fn canonical(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v6) => v6
            .to_ipv4_mapped()
            .map(IpAddr::V4)
            .unwrap_or(IpAddr::V6(v6)),
        v4 => v4,
    }
}

fn internal_v4(ip: Ipv4Addr) -> bool {
    let [a, b, ..] = ip.octets();
    ip.is_unspecified()
        || ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local()
        || ip.is_multicast()
        || ip.is_broadcast()
        // 0.0.0.0/8, "this network"
        || a == 0
        // 100.64.0.0/10, shared address space, such as a tailnet's
        || (a == 100 && (64..128).contains(&b))
        // 192.0.0.0/24, protocol assignments
        || (a == 192 && b == 0 && ip.octets()[2] == 0)
        // 198.18.0.0/15, benchmarking, used inside networks
        || (a == 198 && (b == 18 || b == 19))
        // 240.0.0.0/4, reserved
        || a >= 240
}

fn internal_v6(ip: Ipv6Addr) -> bool {
    let first = ip.segments()[0];
    ip.is_unspecified()
        || ip.is_loopback()
        || ip.is_multicast()
        // fc00::/7, unique local
        || (first & 0xfe00) == 0xfc00
        // fe80::/10, link-local
        || (first & 0xffc0) == 0xfe80
        // fec0::/10, site-local, deprecated but routed inside some networks
        || (first & 0xffc0) == 0xfec0
        // 64:ff9b:1::/48, local NAT64
        || (first == 0x64 && ip.segments()[1] == 0xff9b && ip.segments()[2] == 1)
}

/// Whether `ip` is inside the server's networks: loopback, link-local, private, shared or reserved.
pub fn is_internal(ip: IpAddr) -> bool {
    match canonical(ip) {
        IpAddr::V4(v4) => internal_v4(v4),
        IpAddr::V6(v6) => {
            internal_v6(v6)
                // An IPv4-compatible address (`::10.0.0.1`) is that IPv4 address.
                || v6.to_ipv4().is_some_and(|v4| v6.segments()[..6] == [0; 6] && internal_v4(v4))
        }
    }
}

/// Whether `ip` is an address of one of this machine's interfaces: one a socket can be bound to.
pub fn is_own(ip: IpAddr) -> bool {
    std::net::UdpSocket::bind((canonical(ip), 0)).is_ok()
}

/// Why the server does not reach `ip` for a member, or `None` when it may.
pub fn refusal(ip: IpAddr, allow: &[AddrRange]) -> Option<String> {
    if allow.iter().any(|range| range.contains(ip)) {
        return None;
    }
    let why = if is_internal(ip) {
        "an internal address"
    } else if is_own(ip) {
        "an address of the server itself"
    } else {
        return None;
    };
    Some(format!(
        "{ip} is {why}; an admin allows it with ROOKERY_OUTBOUND_ALLOW"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(text: &str) -> IpAddr {
        text.parse().unwrap()
    }

    #[test]
    fn internal_addresses_are_refused_unless_allowed() {
        for internal in [
            "127.0.0.1",
            "10.1.2.3",
            "172.16.0.9",
            "192.168.1.1",
            "169.254.169.254",
            "100.100.1.1",
            "0.0.0.0",
            "::1",
            "fe80::1",
            "fd12::3",
            "::ffff:127.0.0.1",
            "::ffff:10.0.0.1",
        ] {
            assert!(refusal(ip(internal), &[]).is_some(), "{internal}");
        }
        for outside in ["192.0.2.10", "198.51.100.7", "2001:db8::1"] {
            assert_eq!(refusal(ip(outside), &[]), None, "{outside}");
        }
        let allow = AddrRange::parse_list("10.0.0.0/8, 127.0.0.1").unwrap();
        assert_eq!(refusal(ip("10.9.8.7"), &allow), None);
        assert_eq!(refusal(ip("127.0.0.1"), &allow), None);
        assert_eq!(refusal(ip("::ffff:127.0.0.1"), &allow), None);
        assert!(refusal(ip("127.0.0.2"), &allow).is_some());
        assert!(refusal(ip("192.168.0.1"), &allow).is_some());
    }

    #[test]
    fn the_servers_own_addresses_are_its_interfaces() {
        assert!(is_own(ip("127.0.0.1")));
        assert!(!is_own(ip("192.0.2.10")));
    }

    #[test]
    fn ranges_parse_or_say_why_not() {
        assert_eq!(
            AddrRange::parse("fd00::/8").unwrap().to_string(),
            "fd00::/8"
        );
        assert!(AddrRange::parse("10.0.0.0/33").is_err());
        assert!(AddrRange::parse("intranet").is_err());
        assert!(
            AddrRange::parse("0.0.0.0/0")
                .unwrap()
                .contains(ip("192.0.2.1"))
        );
    }
}
