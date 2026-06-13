//! The egress-guarded HTTP client (native, rustls). Blocks loopback/private/
//! link-local addresses, re-checks every redirect hop, and strips credentials
//! on cross-host redirects. Ports v2's `httpclient.py`.

use crate::error::HttpError;
#[cfg(feature = "native")]
use crate::traits::{HttpClient, HttpRequest, HttpResponse};
#[cfg(feature = "native")]
use async_trait::async_trait;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, ToSocketAddrs};

pub const MAX_REDIRECTS: usize = 5;
#[cfg(feature = "native")]
const USER_AGENT: &str = "orchard/3.0 (+https://github.com/orchard-lang/orchard)";

/// Is an IPv4 address non-routable / special-purpose? Mirrors Python's
/// `ipaddress` `is_loopback|is_private|is_link_local|is_reserved` (the v2
/// guard), hand-rolled to avoid unstable std methods. Blocks RFC-1918,
/// loopback, link-local, CGNAT (100.64/10), 0.0.0.0/8, IETF protocol
/// assignments / documentation / benchmarking, multicast, and 240/4 reserved.
fn v4_blocked(v4: Ipv4Addr) -> bool {
    let [a, b, c, _d] = v4.octets();
    a == 0                                  // 0.0.0.0/8 "this network"
        || a == 10                          // 10/8 private
        || (a == 100 && (64..=127).contains(&b)) // 100.64/10 CGNAT
        || a == 127                         // 127/8 loopback
        || (a == 169 && b == 254)           // 169.254/16 link-local
        || (a == 172 && (16..=31).contains(&b)) // 172.16/12 private
        || (a == 192 && b == 0 && c == 0)   // 192.0.0.0/24 IETF protocol
        || (a == 192 && b == 0 && c == 2)   // 192.0.2.0/24 TEST-NET-1
        || (a == 192 && b == 168)           // 192.168/16 private
        || (a == 198 && (b == 18 || b == 19)) // 198.18/15 benchmarking
        || (a == 198 && b == 51 && c == 100) // 198.51.100.0/24 TEST-NET-2
        || (a == 203 && b == 0 && c == 113) // 203.0.113.0/24 TEST-NET-3
        || a >= 224 // 224/4 multicast + 240/4 reserved + 255.255.255.255 broadcast
}

/// Is an IPv6 address non-routable / special-purpose? Unwraps IPv4-mapped,
/// IPv4-compatible, and NAT64 (64:ff9b::/96) forms and re-applies [`v4_blocked`]
/// so an attacker can't tunnel a private v4 target through a v6 literal.
fn v6_blocked(v6: Ipv6Addr) -> bool {
    let seg = v6.segments();
    // IPv4-mapped (::ffff:a.b.c.d) and IPv4-compatible (::a.b.c.d)
    if let Some(v4) = v6.to_ipv4() {
        return v4_blocked(v4);
    }
    // NAT64 well-known prefix 64:ff9b::/96 → embedded v4 in the last 32 bits
    if seg[0] == 0x0064 && seg[1] == 0xff9b && seg[2..6].iter().all(|s| *s == 0) {
        let v4 = Ipv4Addr::new(
            (seg[6] >> 8) as u8,
            (seg[6] & 0xff) as u8,
            (seg[7] >> 8) as u8,
            (seg[7] & 0xff) as u8,
        );
        return v4_blocked(v4);
    }
    v6.is_loopback()
        || v6.is_unspecified()
        || (seg[0] & 0xfe00) == 0xfc00 // ULA fc00::/7
        || (seg[0] & 0xffc0) == 0xfe80 // link-local fe80::/10
        || (seg[0] & 0xff00) == 0xff00 // multicast ff00::/8
        || (seg[0] == 0x2001 && seg[1] == 0x0db8) // 2001:db8::/32 documentation
}

/// Whether a host resolves to a loopback/private/link-local/reserved address.
pub fn host_is_private(host: &str) -> bool {
    let addrs = match (host, 0u16).to_socket_addrs() {
        Ok(a) => a,
        Err(_) => return false, // unresolvable → let the request fail with a clear error
    };
    for sa in addrs {
        let blocked = match sa.ip() {
            IpAddr::V4(v4) => v4_blocked(v4),
            IpAddr::V6(v6) => v6_blocked(v6),
        };
        if blocked {
            return true;
        }
    }
    false
}

/// Validate a URL against the egress policy.
pub fn check_egress(
    url: &url::Url,
    allowed_domains: &[String],
    allow_local: bool,
) -> Result<(), HttpError> {
    let scheme = url.scheme();
    if scheme != "http" && scheme != "https" {
        return Err(HttpError::new(format!(
            "egress blocked: unsupported scheme '{scheme}'"
        )));
    }
    let host = match url.host_str() {
        Some(h) => h,
        None => return Err(HttpError::new("egress blocked: no host")),
    };
    if !allowed_domains.is_empty() {
        let ok = allowed_domains
            .iter()
            .any(|d| host == d || host.ends_with(&format!(".{d}")));
        if !ok {
            return Err(HttpError::new(format!(
                "egress blocked: '{host}' not in allowed_domains"
            )));
        }
    }
    if !allow_local && host_is_private(host) {
        return Err(HttpError::new(format!(
            "egress blocked: '{host}' resolves to a private/loopback address"
        )));
    }
    Ok(())
}

/// The default native HTTP client (reqwest + rustls).
#[cfg(feature = "native")]
pub struct ReqwestClient {
    client: reqwest::Client,
}

#[cfg(feature = "native")]
impl Default for ReqwestClient {
    fn default() -> Self {
        ReqwestClient::new()
    }
}

#[cfg(feature = "native")]
impl ReqwestClient {
    pub fn new() -> Self {
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .user_agent(USER_AGENT)
            .build()
            .expect("reqwest client builds");
        ReqwestClient { client }
    }
}

#[cfg(feature = "native")]
#[async_trait]
impl HttpClient for ReqwestClient {
    async fn request(&self, req: HttpRequest) -> Result<HttpResponse, HttpError> {
        let original =
            url::Url::parse(&req.url).map_err(|e| HttpError::new(format!("bad url: {e}")))?;
        let original_host = original.host_str().unwrap_or("").to_string();
        let mut url = original;
        let mut method = req.method.to_uppercase();
        let mut body = req.body.clone();

        for _ in 0..=MAX_REDIRECTS {
            if req.enforce_egress {
                check_egress(&url, &req.allowed_domains, req.allow_local)?;
            }
            let cross_host = url.host_str().unwrap_or("") != original_host;
            let m = reqwest::Method::from_bytes(method.as_bytes()).unwrap_or(reqwest::Method::GET);
            let mut builder = self.client.request(m, url.clone());
            for (k, v) in &req.headers {
                // strip credentials on cross-host redirects
                let lk = k.to_ascii_lowercase();
                if cross_host
                    && (lk == "authorization" || lk == "cookie" || lk == "proxy-authorization")
                {
                    continue;
                }
                builder = builder.header(k, v);
            }
            if let Some(b) = &body {
                builder = builder.body(b.clone());
            }
            let timeout = std::time::Duration::from_secs(req.timeout_secs.max(1));
            let resp = builder
                .timeout(timeout)
                .send()
                .await
                .map_err(|e| HttpError::new(format!("request failed: {e}")))?;
            let status = resp.status().as_u16();
            if (300..400).contains(&status) {
                if let Some(loc) = resp.headers().get("location").and_then(|v| v.to_str().ok()) {
                    let next = url
                        .join(loc)
                        .map_err(|e| HttpError::new(format!("bad redirect: {e}")))?;
                    if status == 303 || (matches!(status, 301 | 302) && method == "POST") {
                        method = "GET".into();
                        body = None;
                    }
                    url = next;
                    continue;
                }
            }
            let headers: Vec<(String, String)> = resp
                .headers()
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or("").to_string()))
                .collect();
            let bytes = resp
                .bytes()
                .await
                .map_err(|e| HttpError::new(format!("read body failed: {e}")))?;
            return Ok(HttpResponse {
                status,
                headers,
                body: bytes.to_vec(),
            });
        }
        Err(HttpError::new("too many redirects"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    fn v6(s: &str) -> bool {
        v6_blocked(Ipv6Addr::from_str(s).unwrap())
    }
    fn v4(s: &str) -> bool {
        v4_blocked(Ipv4Addr::from_str(s).unwrap())
    }

    #[test]
    fn ipv4_mapped_v6_is_blocked() {
        // SSRF: a private/loopback/metadata v4 tunneled through a v6 literal.
        assert!(v6("::ffff:127.0.0.1"));
        assert!(v6("::ffff:169.254.169.254")); // cloud metadata
        assert!(v6("::ffff:10.0.0.1"));
        assert!(v6("64:ff9b::7f00:1")); // NAT64 → 127.0.0.1
        assert!(v6("::ffff:192.168.1.1"));
        // a genuinely public mapped address is allowed
        assert!(!v6("::ffff:8.8.8.8"));
    }

    #[test]
    fn ipv4_reserved_ranges_blocked() {
        assert!(v4("100.64.0.1")); // CGNAT
        assert!(v4("0.0.0.0"));
        assert!(v4("240.0.0.1")); // reserved
        assert!(v4("255.255.255.255")); // broadcast
        assert!(v4("192.0.2.5")); // TEST-NET-1
        assert!(v4("198.18.0.1")); // benchmarking
        assert!(v4("169.254.1.1")); // link-local
        assert!(v4("10.1.2.3"));
        assert!(v4("172.16.0.1"));
        assert!(!v4("8.8.8.8"));
        assert!(!v4("1.1.1.1"));
    }

    #[test]
    fn ipv6_special_blocked() {
        assert!(v6("::1"));
        assert!(v6("fc00::1")); // ULA
        assert!(v6("fe80::1")); // link-local
        assert!(v6("ff02::1")); // multicast
        assert!(v6("2001:db8::1")); // documentation
        assert!(!v6("2606:4700::1111")); // public
    }
}
