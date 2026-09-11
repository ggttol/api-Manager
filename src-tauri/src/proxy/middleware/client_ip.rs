use axum::extract::{ConnectInfo, Request};
use std::net::{IpAddr, SocketAddr};

/// Resolves the client identity from the transport peer. Forwarded headers are
/// considered only when the direct peer is explicitly configured as trusted.
pub(crate) fn resolve_client_ip(request: &Request, trusted_proxies: &[String]) -> Option<IpAddr> {
    let peer = request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()?
        .0
        .ip();
    if !trusted_proxies
        .iter()
        .filter_map(|value| value.trim().parse::<IpAddr>().ok())
        .any(|trusted| trusted == peer)
    {
        return Some(peer);
    }

    let forwarded = request
        .headers()
        .get("x-forwarded-for")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| {
            value
                .split(',')
                .map(str::trim)
                .map(str::parse::<IpAddr>)
                .collect::<Result<Vec<_>, _>>()
                .ok()
        })
        .filter(|chain| !chain.is_empty());

    if let Some(chain) = forwarded {
        // XFF is client-to-proxy ordered. Walk backwards through trusted hops
        // to find the first untrusted address, which is the client identity.
        return chain
            .iter()
            .rev()
            .copied()
            .find(|ip| {
                !trusted_proxies
                    .iter()
                    .any(|value| value.trim().parse::<IpAddr>().ok() == Some(*ip))
            })
            .or_else(|| chain.first().copied());
    }

    request
        .headers()
        .get("x-real-ip")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.trim().parse().ok())
        .or(Some(peer))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;

    fn request(peer: &str, xff: Option<&str>) -> Request {
        let mut request = Request::builder().uri("/").body(Body::empty()).unwrap();
        request
            .extensions_mut()
            .insert(ConnectInfo(peer.parse::<SocketAddr>().unwrap()));
        if let Some(xff) = xff {
            request
                .headers_mut()
                .insert("x-forwarded-for", xff.parse().unwrap());
        }
        request
    }

    #[test]
    fn direct_peer_cannot_spoof_forwarded_client() {
        let request = request("203.0.113.4:1234", Some("198.51.100.10"));
        assert_eq!(
            resolve_client_ip(&request, &[]).unwrap().to_string(),
            "203.0.113.4"
        );
    }

    #[test]
    fn trusted_proxy_chain_resolves_first_untrusted_hop() {
        let request = request("10.0.0.2:1234", Some("198.51.100.10, 10.0.0.1"));
        assert_eq!(
            resolve_client_ip(&request, &["10.0.0.2".into(), "10.0.0.1".into()])
                .unwrap()
                .to_string(),
            "198.51.100.10"
        );
    }
}
