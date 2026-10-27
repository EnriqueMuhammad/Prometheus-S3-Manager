//! Classifies addresses that plain HTTP may reach.

use std::net::IpAddr;

/// `true` for loopback, RFC 1918, link-local, RFC 6598 shared/CGNAT (VPN overlays), IPv6
/// unique-local / link-local, and IPv4-mapped forms of those.
pub fn is_private(address: IpAddr) -> bool {
    let address = match address {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map_or(address, IpAddr::V4),
        v4 => v4,
    };
    if address.is_loopback() {
        return true;
    }
    match address {
        IpAddr::V4(v4) => {
            let [a, b, ..] = v4.octets();
            a == 10
                || (a == 172 && (16..=31).contains(&b))
                || (a == 192 && b == 168)
                || (a == 169 && b == 254)
                || (a == 100 && (64..=127).contains(&b))
        }
        IpAddr::V6(v6) => {
            let [a, b, ..] = v6.octets();
            (a & 0xFE) == 0xFC || (a == 0xFE && (b & 0xC0) == 0x80)
        }
    }
}

/// Parses an IP literal, with or without IPv6 brackets.
pub fn parse_literal(host: &str) -> Option<IpAddr> {
    host.trim_start_matches('[')
        .trim_end_matches(']')
        .parse()
        .ok()
}

/// `true` when `host` is an IP literal in a private range.
pub fn is_private_literal(host: &str) -> bool {
    parse_literal(host).is_some_and(is_private)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> IpAddr {
        s.parse().expect("ip")
    }

    #[test]
    fn private_ranges() {
        for s in [
            "127.0.0.1",
            "10.1.2.3",
            "172.16.0.1",
            "172.31.255.255",
            "192.168.1.10",
            "169.254.1.1",
            "100.64.0.1",
            "100.127.255.255",
            "::1",
            "fd00::1",
            "fe80::1",
            "::ffff:10.0.0.1",
        ] {
            assert!(is_private(ip(s)), "{s}");
        }
    }

    #[test]
    fn public_ranges() {
        for s in [
            "8.8.8.8",
            "172.32.0.1",
            "100.128.0.1",
            "192.169.0.1",
            "2001:4860::8888",
            "::ffff:8.8.8.8",
        ] {
            assert!(!is_private(ip(s)), "{s}");
        }
    }

    #[test]
    fn literals() {
        assert!(is_private_literal("[fd00::5]"));
        assert!(!is_private_literal("minio.local"));
    }
}
