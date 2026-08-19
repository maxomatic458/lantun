use std::{
    collections::HashMap,
    net::{SocketAddr, SocketAddrV4},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use iroh::{
    Endpoint, NodeAddr, PublicKey, RelayMode, SecretKey,
    endpoint::{Connection, VarInt},
};
use tokio::{
    net::{TcpSocket, UdpSocket},
    sync::RwLock,
};
use tokio_util::{sync::CancellationToken, task::TaskTracker};

use crate::{
    error::{Error, Result},
    forward::{tcp::bi_tcp_forward, udp::bi_udp_host},
    protocol::{ALPN, ConnReq, Hello, PROTOCOL_VERSION, read_frame},
    tunnel::{TunnelProtocol, any_addr},
};

/// A [`HostTunnel`] listens for incoming peer connections and forwards each tunneled
/// stream to a fixed local address.
pub struct HostTunnel {
    name: String,
    public_key: PublicKey,
    forward_to: SocketAddr,
    protocol: TunnelProtocol,
    endpoint: Endpoint,
    peers: Arc<RwLock<HashMap<PublicKey, Arc<PeerEntry>>>>,
    cancel: CancellationToken,
    /// Cancelled by the accept loop if it exits without `shutdown()`.
    dead: CancellationToken,
    tracker: TaskTracker,
}

#[derive(Default)]
struct PeerEntry {
    /// Number of currently-open forwarded streams from this peer.
    active: Arc<AtomicUsize>,
}

/// Max time a newly-accepted peer has to send its Hello frame.
const HELLO_TIMEOUT: Duration = Duration::from_secs(10);

pub struct HostTunnelBuilder {
    secret: Option<SecretKey>,
    forward_to: Option<SocketAddr>,
    protocol: Option<TunnelProtocol>,
    name: String,
    relay: RelayMode,
    discovery: bool,
    bind_v4: Option<SocketAddrV4>,
}

impl Default for HostTunnelBuilder {
    fn default() -> Self {
        Self {
            secret: None,
            forward_to: None,
            protocol: None,
            name: "host".into(),
            relay: RelayMode::Default,
            discovery: true,
            bind_v4: None,
        }
    }
}

impl HostTunnelBuilder {
    pub fn secret(mut self, s: SecretKey) -> Self {
        self.secret = Some(s);
        self
    }
    pub fn forward_to(mut self, addr: SocketAddr) -> Self {
        self.forward_to = Some(addr);
        self
    }
    pub fn protocol(mut self, p: TunnelProtocol) -> Self {
        self.protocol = Some(p);
        self
    }
    pub fn name(mut self, n: impl Into<String>) -> Self {
        self.name = n.into();
        self
    }
    pub fn relay(mut self, r: RelayMode) -> Self {
        self.relay = r;
        self
    }
    /// Disable n0 discovery.
    pub fn discovery(mut self, enable: bool) -> Self {
        self.discovery = enable;
        self
    }

    /// Bind the iroh endpoint's IPv4 socket to a specific address.
    pub fn bind_addr_v4(mut self, addr: SocketAddrV4) -> Self {
        self.bind_v4 = Some(addr);
        self
    }

    pub async fn start(self) -> Result<HostTunnel> {
        let secret = self
            .secret
            .ok_or_else(|| Error::Protocol("secret required".into()))?;
        let forward_to = self
            .forward_to
            .ok_or_else(|| Error::Protocol("forward_to required".into()))?;
        let protocol = self
            .protocol
            .ok_or_else(|| Error::Protocol("protocol required".into()))?;

        let mut builder = Endpoint::builder()
            .alpns(vec![ALPN.to_vec()])
            .secret_key(secret.clone())
            .relay_mode(self.relay);
        if self.discovery {
            builder = builder.discovery_n0();
        }
        if let Some(addr) = self.bind_v4 {
            builder = builder.bind_addr_v4(addr);
        }
        let endpoint = builder
            .bind()
            .await
            .map_err(|e| Error::Endpoint(e.to_string()))?;

        let public_key = secret.public();
        let peers: Arc<RwLock<HashMap<PublicKey, Arc<PeerEntry>>>> =
            Arc::new(RwLock::new(HashMap::new()));
        let cancel = CancellationToken::new();
        let dead = CancellationToken::new();
        let tracker = TaskTracker::new();

        let ep = endpoint.clone();
        let peers_c = peers.clone();
        let cancel_c = cancel.clone();
        let dead_c = dead.clone();
        let tracker_c = tracker.clone();
        let name = self.name.clone();
        tracker.spawn(accept_loop(
            ep, forward_to, protocol, peers_c, tracker_c, cancel_c, dead_c, name,
        ));
        tracker.close();

        tracing::info!("host tunnel \"{}\" listening for peers", self.name);

        Ok(HostTunnel {
            name: self.name,
            public_key,
            forward_to,
            protocol,
            endpoint,
            peers,
            cancel,
            dead,
            tracker,
        })
    }
}

impl HostTunnel {
    pub fn builder() -> HostTunnelBuilder {
        HostTunnelBuilder::default()
    }

    pub fn name(&self) -> &str {
        &self.name
    }
    pub fn public_key(&self) -> PublicKey {
        self.public_key
    }
    pub fn forward_to(&self) -> SocketAddr {
        self.forward_to
    }
    pub fn protocol(&self) -> TunnelProtocol {
        self.protocol
    }

    /// The full addressing info (node id + direct addrs + relay) needed to connect by explicit
    /// `NodeAddr`.
    pub async fn node_addr(&self) -> Result<NodeAddr> {
        self.endpoint
            .node_addr()
            .await
            .map_err(|e| Error::Endpoint(e.to_string()))
    }

    pub async fn connected_peers(&self) -> usize {
        self.peers.read().await.len()
    }

    pub async fn active_connections(&self) -> usize {
        let peers = self.peers.read().await;
        peers
            .values()
            .map(|p| p.active.load(Ordering::Relaxed))
            .sum()
    }

    /// Resolves when the tunnel's accept loop exits *without* `shutdown()` being called.
    pub async fn wait_dead(&self) {
        self.dead.cancelled().await;
    }

    pub async fn shutdown(self) -> Result<()> {
        self.cancel.cancel();
        self.endpoint.close().await;
        self.tracker.wait().await;
        tracing::info!("host tunnel \"{}\" stopped", self.name);
        Ok(())
    }
}

#[allow(clippy::too_many_arguments)]
async fn accept_loop(
    endpoint: Endpoint,
    forward_to: SocketAddr,
    protocol: TunnelProtocol,
    peers: Arc<RwLock<HashMap<PublicKey, Arc<PeerEntry>>>>,
    tracker: TaskTracker,
    cancel: CancellationToken,
    dead: CancellationToken,
    tunnel_name: String,
) {
    loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                tracing::debug!("host \"{tunnel_name}\" accept loop cancelled");
                return;
            }
            maybe_incoming = endpoint.accept() => {
                let Some(incoming) = maybe_incoming else {
                    // `None` from `Endpoint::accept()` means our local endpoint has been
                    // closed. If shutdown() did it, cancel is set and we exit quietly.
                    // Otherwise the endpoint died on its own and we fire the `dead` signal and we exit.
                    if cancel.is_cancelled() {
                        tracing::debug!("host \"{tunnel_name}\" endpoint closed, accept loop exiting");
                    } else {
                        tracing::error!("host \"{tunnel_name}\" endpoint died unexpectedly");
                        dead.cancel();
                    }
                    return;
                };
                let peers = peers.clone();
                let tracker2 = tracker.clone();
                let cancel2 = cancel.clone();
                let tname = tunnel_name.clone();
                tracker.spawn(async move {
                    let conn = match incoming.await {
                        Ok(c) => c,
                        Err(e) => {
                            tracing::debug!("host \"{tname}\" incoming failed: {e}");
                            return;
                        }
                    };
                    if let Err(e) = handle_peer(conn, forward_to, protocol, peers, tracker2, cancel2, &tname).await {
                        tracing::debug!("host \"{tname}\" peer session ended: {e}");
                    }
                });
            }
        }
    }
}

async fn handle_peer(
    conn: Connection,
    forward_to: SocketAddr,
    protocol: TunnelProtocol,
    peers: Arc<RwLock<HashMap<PublicKey, Arc<PeerEntry>>>>,
    tracker: TaskTracker,
    cancel: CancellationToken,
    tunnel_name: &str,
) -> Result<()> {
    let peer_key = conn
        .remote_node_id()
        .map_err(|e| Error::Protocol(format!("no remote node id: {e}")))?;

    let hello: Hello = tokio::time::timeout(HELLO_TIMEOUT, async {
        let mut s = conn.accept_uni().await?;
        read_frame::<Hello>(&mut s).await
    })
    .await
    .map_err(|_| Error::Protocol("hello timed out".into()))??;

    if hello.version != PROTOCOL_VERSION {
        conn.close(VarInt::from_u32(1), b"version mismatch");
        return Err(Error::VersionMismatch {
            ours: PROTOCOL_VERSION,
            peer: hello.version,
        });
    }

    // Register new peer.
    let entry = Arc::new(PeerEntry::default());
    peers.write().await.insert(peer_key, entry.clone());
    let active = entry.active.clone();
    tracing::debug!("host \"{tunnel_name}\" peer {peer_key} connected");

    // Cleanup guard: only removes the map entry if it still matches the one we inserted.
    // Prevents fast connect/disconnect from removing a new peer's entry.
    struct PeerGuard {
        peers: Arc<RwLock<HashMap<PublicKey, Arc<PeerEntry>>>>,
        key: PublicKey,
        ours: Arc<PeerEntry>,
    }
    impl Drop for PeerGuard {
        fn drop(&mut self) {
            let peers = self.peers.clone();
            let key = self.key;
            let ours = self.ours.clone();
            tokio::spawn(async move {
                let mut peers = peers.write().await;
                if let Some(existing) = peers.get(&key)
                    && Arc::ptr_eq(existing, &ours)
                {
                    peers.remove(&key);
                }
            });
        }
    }
    let _guard = PeerGuard {
        peers: peers.clone(),
        key: peer_key,
        ours: entry,
    };

    loop {
        tokio::select! {
            _ = cancel.cancelled() => return Ok(()),
            _ = conn.closed() => {
                tracing::debug!("host \"{tunnel_name}\" peer {peer_key} disconnected");
                return Ok(());
            }
            r = conn.accept_bi() => {
                let (send, mut recv) = r?;
                let req: ConnReq = read_frame(&mut recv).await?;
                let cancel3 = cancel.clone();
                let active3 = active.clone();
                let tname = tunnel_name.to_string();
                tracker.spawn(async move {
                    active3.fetch_add(1, Ordering::Relaxed);
                    let result = handle_stream(protocol, forward_to, req, send, recv, cancel3).await;
                    active3.fetch_sub(1, Ordering::Relaxed);
                    if let Err(e) = result {
                        tracing::debug!("host \"{tname}\" stream ended: {e}");
                    }
                });
            }
        }
    }
}

async fn handle_stream(
    protocol: TunnelProtocol,
    forward_to: SocketAddr,
    req: ConnReq,
    send: iroh::endpoint::SendStream,
    recv: iroh::endpoint::RecvStream,
    cancel: CancellationToken,
) -> Result<()> {
    tracing::debug!(
        "host: opening backend stream for client {} -> {}",
        req.client_local_addr,
        forward_to
    );
    match protocol {
        TunnelProtocol::Tcp => {
            let socket = if forward_to.is_ipv4() {
                TcpSocket::new_v4()?
            } else {
                TcpSocket::new_v6()?
            };
            socket.bind(any_addr(forward_to.is_ipv4()))?;
            let tcp = socket.connect(forward_to).await?;
            bi_tcp_forward(tcp, send, recv, cancel).await
        }
        TunnelProtocol::Udp => {
            let socket = UdpSocket::bind(any_addr(forward_to.is_ipv4())).await?;
            socket.connect(forward_to).await?;
            bi_udp_host(socket, send, recv, cancel).await
        }
    }
}
