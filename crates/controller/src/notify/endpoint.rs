//! The SSRF guard for Web Push endpoints.
//!
//! A push endpoint is a URL the browser hands us, and the server POSTs to it. So that an
//! authenticated user can't make the server reach internal services, endpoints must be
//! `https` URLs on the default port, with a DNS host name (no IP literal, no `localhost`), and
//! no credentials. At send time the host must resolve to public addresses only
//! ([`PublicResolver`]), and redirects are never followed.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

use reqwest::Url;
use reqwest::dns::{Addrs, Name, Resolve, Resolving};

/// Checks a push endpoint URL; the error says what's wrong.
pub(crate) fn check(endpoint: &str) -> Result<Url, String> {
    let url = Url::parse(endpoint).map_err(|e| format!("not a URL: {e}"))?;
    if url.scheme() != "https" {
        return Err("push endpoints must be https URLs".into());
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err("push endpoints can't carry credentials".into());
    }
    if url.port().is_some() {
        return Err("push endpoints must use the default https port".into());
    }
    // `domain()` is `None` for IP literals.
    let Some(host) = url.domain() else {
        return Err("push endpoints must name a host, not an IP address".into());
    };
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    if !host.contains('.') || host == "localhost" || host.ends_with(".localhost") {
        return Err(format!("{host} isn't a public push service"));
    }
    Ok(url)
}

/// Whether `ip` is a public unicast address (not loopback, private, link-local, shared,
/// multicast, unspecified or reserved).
pub(crate) fn is_public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_public_v4(v4),
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => is_public_v4(v4),
            None => is_public_v6(v6),
        },
    }
}

fn is_public_v4(ip: Ipv4Addr) -> bool {
    let [a, b, ..] = ip.octets();
    !(ip.is_unspecified()
        || ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local()
        || ip.is_multicast()
        || ip.is_broadcast()
        || ip.is_documentation()
        || a == 0
        // Shared address space (RFC 6598) and reserved (240/4).
        || (a == 100 && (64..128).contains(&b))
        || a >= 240
        // IETF protocol assignments (192.0.0.0/24) and benchmarking (198.18.0.0/15).
        || (a == 192 && b == 0 && ip.octets()[2] == 0)
        || (a == 198 && (b == 18 || b == 19)))
}

fn is_public_v6(ip: Ipv6Addr) -> bool {
    let first = ip.segments()[0];
    !(ip.is_unspecified()
        || ip.is_loopback()
        || ip.is_multicast()
        // Unique local fc00::/7, link-local fe80::/10, documentation 2001:db8::/32.
        || (first & 0xfe00) == 0xfc00
        || (first & 0xffc0) == 0xfe80
        || (first == 0x2001 && ip.segments()[1] == 0x0db8)
        // IPv4-compatible and NAT64 well-known prefixes could reach private v4 space.
        || (first == 0 && ip.segments()[1..6].iter().all(|s| *s == 0))
        || (first == 0x0064 && ip.segments()[1] == 0xff9b))
}

/// A DNS resolver that only returns public addresses, so a push endpoint whose name points
/// into the internal network is refused at connect time.
pub(crate) struct PublicResolver;

impl Resolve for PublicResolver {
    fn resolve(&self, name: Name) -> Resolving {
        Box::pin(async move {
            let host = name.as_str().to_owned();
            let addrs: Vec<SocketAddr> = tokio::net::lookup_host((host.as_str(), 0))
                .await?
                .filter(|a| is_public(a.ip()))
                .collect();
            if addrs.is_empty() {
                return Err(format!("{host} has no public address").into());
            }
            let addrs: Addrs = Box::new(addrs.into_iter());
            Ok(addrs)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoints_must_be_public_https_names() {
        for ok in [
            "https://push.example.com/send/1",
            "https://fcm.example.com:443/x",
            "https://PUSH.example.com./x",
        ] {
            assert!(check(ok).is_ok(), "{ok}");
        }
        for bad in [
            "http://push.example.com/1",
            "https://127.0.0.1/1",
            "https://[::1]/1",
            "https://10.0.0.8/1",
            "https://localhost/1",
            "https://api.localhost/1",
            "https://intranet/1",
            "https://push.example.com:8443/1",
            "https://alice:secret@push.example.com/1",
            "file:///etc/passwd",
            "not a url",
        ] {
            assert!(check(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn only_public_addresses_pass() {
        for public in [
            "93.184.216.34",
            "2606:2800:220:1::1",
            "::ffff:93.184.216.34",
        ] {
            assert!(is_public(public.parse().unwrap()), "{public}");
        }
        for private in [
            "127.0.0.1",
            "10.1.2.3",
            "172.16.0.1",
            "192.168.1.1",
            "169.254.169.254",
            "100.64.0.1",
            "0.0.0.0",
            "255.255.255.255",
            "224.0.0.1",
            "192.0.0.8",
            "198.18.0.1",
            "::1",
            "::",
            "fd00::1",
            "fe80::1",
            "ff02::1",
            "::ffff:10.0.0.1",
            "::ffff:127.0.0.1",
            "64:ff9b::a00:1",
            "2001:db8::1",
        ] {
            assert!(!is_public(private.parse().unwrap()), "{private}");
        }
    }
}
