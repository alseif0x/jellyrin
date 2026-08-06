//! Small HTTPS CONNECT relay for the isolated MAGSTV egress container.
//!
//! The process intentionally does not know about MAGSTV credentials, portal
//! payloads, or signing. It only turns a local HTTP proxy connection into a
//! raw TCP tunnel. The container's WireGuard default route supplies the
//! Mexico egress; the provider is the only caller configured to use it.

use std::{env, io, net::IpAddr, time::Duration};

use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream, lookup_host},
    signal,
    time::timeout,
};
use tracing::{info, warn};

const DEFAULT_LISTEN: &str = "0.0.0.0:18080";
const MAX_PROXY_HEADER_BYTES: usize = 16 * 1024;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const IDLE_COPY_TIMEOUT: Duration = Duration::from_secs(30 * 60);

#[derive(Debug, PartialEq, Eq)]
struct ConnectTarget {
    host: String,
    port: u16,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            env::var("RUST_LOG").unwrap_or_else(|_| "jellyrin_magstv_egress=info".to_string()),
        )
        .with_target(false)
        .init();

    let listen = env::var("MAGSTV_EGRESS_LISTEN")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_LISTEN.to_string());
    let listener = TcpListener::bind(&listen).await?;
    info!(listen = %listen, "MAGSTV isolated CONNECT relay ready");
    let mut shutdown = Box::pin(signal::ctrl_c());

    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let (stream, peer) = accepted?;
                tokio::spawn(async move {
                    if let Err(error) = handle_connection(stream).await {
                        warn!(peer = %peer, ?error, "MAGSTV egress connection closed");
                    }
                });
            }
            _ = &mut shutdown => {
                info!("MAGSTV isolated CONNECT relay stopping");
                break;
            }
        }
    }
    Ok(())
}

async fn handle_connection(mut client: TcpStream) -> io::Result<()> {
    let request = read_proxy_head(&mut client).await?;
    let target = match parse_connect_target(&request) {
        Ok(target) => target,
        Err(status) => {
            client.write_all(status.as_bytes()).await?;
            client.shutdown().await?;
            return Ok(());
        }
    };

    let mut upstream = connect_target(&target).await?;
    client
        .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
        .await?;

    // The stream itself may be long-lived, but an idle tunnel should not keep
    // a tuner/VPN socket forever after the player has gone away.
    timeout(
        IDLE_COPY_TIMEOUT,
        tokio::io::copy_bidirectional(&mut client, &mut upstream),
    )
    .await
    .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "idle CONNECT tunnel timeout"))??;
    Ok(())
}

async fn read_proxy_head(stream: &mut TcpStream) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::with_capacity(1024);
    let mut chunk = [0_u8; 1024];
    while bytes.len() < MAX_PROXY_HEADER_BYTES {
        let read = stream.read(&mut chunk).await?;
        if read == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "proxy request ended before headers",
            ));
        }
        bytes.extend_from_slice(&chunk[..read]);
        if bytes.windows(4).any(|window| window == b"\r\n\r\n") {
            return Ok(bytes);
        }
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidData,
        "proxy request headers exceed limit",
    ))
}

fn parse_connect_target(request: &[u8]) -> Result<ConnectTarget, &'static str> {
    let request = std::str::from_utf8(request).map_err(|_| BAD_REQUEST)?;
    let request_line = request.lines().next().ok_or(BAD_REQUEST)?;
    let mut fields = request_line.split_whitespace();
    let method = fields.next().ok_or(BAD_REQUEST)?;
    let authority = fields.next().ok_or(BAD_REQUEST)?;
    let version = fields.next().ok_or(BAD_REQUEST)?;
    if method != "CONNECT" || !matches!(version, "HTTP/1.0" | "HTTP/1.1") {
        return Err(METHOD_NOT_ALLOWED);
    }
    if authority.is_empty()
        || authority.bytes().any(|byte| byte.is_ascii_control())
        || authority.contains('/')
        || authority.contains('@')
    {
        return Err(BAD_REQUEST);
    }

    let parsed = url::Url::parse(&format!("https://{authority}")).map_err(|_| BAD_REQUEST)?;
    if !parsed.username().is_empty()
        || parsed.password().is_some()
        || !parsed.path().is_empty() && parsed.path() != "/"
        || parsed.query().is_some()
        || parsed.fragment().is_some()
    {
        return Err(BAD_REQUEST);
    }
    let host = parsed.host_str().ok_or(BAD_REQUEST)?;
    let port = parsed.port().unwrap_or(443);
    if port != 443 {
        return Err(BAD_REQUEST);
    }
    if host.parse::<IpAddr>().is_ok() {
        // The sidecar should resolve the rotating MAGSTV hostnames through
        // the VPN resolver; refusing literals also avoids an easy localhost
        // or private-address tunnel from this local-only relay.
        return Err(BAD_REQUEST);
    }
    Ok(ConnectTarget {
        host: host.to_string(),
        port,
    })
}

async fn connect_target(target: &ConnectTarget) -> io::Result<TcpStream> {
    let addresses = timeout(
        CONNECT_TIMEOUT,
        lookup_host((target.host.as_str(), target.port)),
    )
    .await
    .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "target DNS timeout"))??;
    let addresses = addresses.collect::<Vec<_>>();
    if addresses.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "target has no addresses",
        ));
    }
    let mut last_error = None;
    for address in addresses {
        match timeout(CONNECT_TIMEOUT, TcpStream::connect(address)).await {
            Ok(Ok(stream)) => return Ok(stream),
            Ok(Err(error)) => last_error = Some(error),
            Err(_) => {
                last_error = Some(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "target connect timeout",
                ));
            }
        }
    }
    Err(last_error.unwrap_or_else(|| io::Error::other("target unavailable")))
}

const BAD_REQUEST: &str =
    "HTTP/1.1 400 Bad Request\r\nConnection: close\r\nContent-Length: 0\r\n\r\n";
const METHOD_NOT_ALLOWED: &str =
    "HTTP/1.1 405 Method Not Allowed\r\nConnection: close\r\nContent-Length: 0\r\n\r\n";

#[cfg(test)]
mod tests {
    use super::{ConnectTarget, parse_connect_target};

    #[test]
    fn accepts_https_connect_hostname() {
        assert_eq!(
            parse_connect_target(
                b"CONNECT portal.example:443 HTTP/1.1\r\nHost: portal.example\r\n\r\n"
            ),
            Ok(ConnectTarget {
                host: "portal.example".to_string(),
                port: 443,
            })
        );
    }

    #[test]
    fn rejects_plain_http_non_tls_and_ip_literals() {
        for request in [
            b"GET https://portal.example/ HTTP/1.1\r\n\r\n".as_slice(),
            b"CONNECT portal.example:80 HTTP/1.1\r\n\r\n".as_slice(),
            b"CONNECT 127.0.0.1:443 HTTP/1.1\r\n\r\n".as_slice(),
            b"CONNECT user:password@portal.example:443 HTTP/1.1\r\n\r\n".as_slice(),
        ] {
            assert!(parse_connect_target(request).is_err());
        }
    }
}
