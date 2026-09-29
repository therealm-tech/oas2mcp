//! The `healthcheck` subcommand: a liveness probe the binary runs against
//! itself, since the runtime image has no shell or HTTP client to probe with.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::time::Duration;

use anyhow::Context as _;
use tokio::net::TcpStream;

/// How long a connection attempt may take before the server counts as down.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);

/// Succeed when something accepts TCP connections on `bind`.
pub async fn probe(bind: SocketAddr) -> anyhow::Result<()> {
    let target = probe_target(bind);
    tracing::debug!(%target, "probing the MCP server");
    tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect(target))
        .await
        .with_context(|| format!("no answer from {target} within {CONNECT_TIMEOUT:?}"))?
        .with_context(|| format!("connecting to {target}"))?;
    Ok(())
}

/// The address to connect to for a server bound to `bind`. A wildcard bind
/// address listens on loopback too, but cannot itself be connected to.
fn probe_target(bind: SocketAddr) -> SocketAddr {
    let ip = match bind.ip() {
        IpAddr::V4(ip) if ip.is_unspecified() => IpAddr::V4(Ipv4Addr::LOCALHOST),
        IpAddr::V6(ip) if ip.is_unspecified() => IpAddr::V6(Ipv6Addr::LOCALHOST),
        ip => ip,
    };
    SocketAddr::new(ip, bind.port())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_wildcard_bind_address_is_probed_on_loopback() {
        for (bind, target) in [
            ("0.0.0.0:8000", "127.0.0.1:8000"),
            ("[::]:8000", "[::1]:8000"),
            ("10.1.2.3:8000", "10.1.2.3:8000"),
        ] {
            assert_eq!(
                probe_target(bind.parse().unwrap()),
                target.parse().unwrap(),
                "{bind}"
            );
        }
    }

    #[tokio::test]
    async fn a_listening_server_is_healthy() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let bind = SocketAddr::new(
            IpAddr::V4(Ipv4Addr::UNSPECIFIED),
            listener.local_addr().unwrap().port(),
        );
        probe(bind).await.expect("the listener accepts connections");
    }

    #[tokio::test]
    async fn a_closed_port_is_unhealthy() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let bind = listener.local_addr().unwrap();
        drop(listener);
        probe(bind).await.expect_err("nothing listens any more");
    }
}
