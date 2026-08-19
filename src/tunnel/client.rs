use std::{
    collections::HashMap,
    net::SocketAddr,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use iroh::{
    Endpoint, NodeAddr, PublicKey, RelayMode,
    endpoint::{Connection, VarInt},
};
use tokio::{
    net::{TcpListener, UdpSocket},
    sync::{RwLock, mpsc},
};
use tokio_util::{sync::CancellationToken, task::TaskTracker};

use crate::{
    error::{Error, Result},
    forward::{tcp::bi_tcp_forward, udp::bi_udp_client_session},
    protocol::{ALPN, ConnReq, Hello, PROTOCOL_VERSION, write_frame},
    tunnel::TunnelProtocol,
};

/// Reconnection policy for a client tunnel.
#[derive(Debug, Clone)]
pub struct ReconnectPolicy {
    /// `None` = retry forever.
    pub max_attempts: Option<u32>,
    pub initial_backoff: Duration,
    pub max_backoff: Duration,
}

impl Default for ReconnectPolicy {
    fn default() -> Self {
        Self {
            max_attempts: None,
            initial_backoff: Duration::from_secs(1),
            max_backoff: Duration::from_secs(60),
        }
    }
}

impl ReconnectPolicy {
    /// Reconnection disabled.
    pub fn none() -> Self {
        Self {
            max_attempts: Some(0),
            initial_backoff: Duration::from_secs(0),
            max_backoff: Duration::from_secs(0),
        }
    }
}

pub struct ClientTunnel {
    name: String,
    bind: SocketAddr,
    protocol: TunnelProtocol,
    cancel: CancellationToken,
    tracker: TaskTracker,
    active: Arc<AtomicUsize>,
    connected: Arc<std::sync::atomic::AtomicBool>,
}

enum HostTarget {
    Key(PublicKey),
    Addr(NodeAddr),
}

pub struct ClientTunnelBuilder {
    host: Option<HostTarget>,
    bind: Option<SocketAddr>,
    protocol: Option<TunnelProtocol>,
    name: String,
    relay: RelayMode,
    discovery: bool,
    reconnect: ReconnectPolicy,
}

impl Default for ClientTunnelBuilder {
    fn default() -> Self {
        Self {
            host: None,
            bind: None,
            protocol: None,
            name: "client".into(),
            relay: RelayMode::Default,
            discovery: true,
            reconnect: ReconnectPolicy::default(),
        }
    }
}

impl ClientTunnelBuilder {
    /// Connect to a host by its public key.
    pub fn host_key(mut self, key: PublicKey) -> Self {
        self.host = Some(HostTarget::Key(key));
        self
    }

    /// Connect to a host by an explicit `NodeAddr`.
    pub fn host_addr(mut self, addr: NodeAddr) -> Self {
        self.host = Some(HostTarget::Addr(addr));
        self
    }

    pub fn bind(mut self, addr: SocketAddr) -> Self {
        self.bind = Some(addr);
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
    pub fn discovery(mut self, enable: bool) -> Self {
        self.discovery = enable;
        self
    }
    pub fn reconnect(mut self, policy: ReconnectPolicy) -> Self {
        self.reconnect = policy;
        self
    }

    pub async fn start(self) -> Result<ClientTunnel> {
        let host = self
            .host
            .ok_or_else(|| Error::Protocol("host required".into()))?;
        let bind = self
            .bind
            .ok_or_else(|| Error::Protocol("bind required".into()))?;
        let protocol = self
            .protocol
            .ok_or_else(|| Error::Protocol("protocol required".into()))?;

        // Bind the local listener up front so start() fails fast.
        let (local, actual_bind) = match protocol {
            TunnelProtocol::Tcp => {
                let l = TcpListener::bind(bind).await?;
                let addr = l.local_addr()?;
                (LocalSocket::Tcp(l), addr)
            }
            TunnelProtocol::Udp => {
                let s = UdpSocket::bind(bind).await?;
                let addr = s.local_addr()?;
                (LocalSocket::Udp(Arc::new(s)), addr)
            }
        };

        let cancel = CancellationToken::new();
        let tracker = TaskTracker::new();
        let active = Arc::new(AtomicUsize::new(0));
        let connected = Arc::new(std::sync::atomic::AtomicBool::new(false));

        let name = self.name.clone();
        let cancel_c = cancel.clone();
        let tracker_c = tracker.clone();
        let active_c = active.clone();
        let connected_c = connected.clone();
        let relay = self.relay;
        let discovery = self.discovery;
        let policy = self.reconnect;

        tracker.spawn(supervise(
            local,
            host,
            protocol,
            relay,
            discovery,
            policy,
            cancel_c,
            tracker_c,
            active_c,
            connected_c,
            name,
        ));
        tracker.close();

        Ok(ClientTunnel {
            name: self.name,
            bind: actual_bind,
            protocol,
            cancel,
            tracker,
            active,
            connected,
        })
    }
}

impl ClientTunnel {
    pub fn builder() -> ClientTunnelBuilder {
        ClientTunnelBuilder::default()
    }

    pub fn name(&self) -> &str {
        &self.name
    }
    pub fn bind(&self) -> SocketAddr {
        self.bind
    }
    pub fn protocol(&self) -> TunnelProtocol {
        self.protocol
    }
    pub fn active_connections(&self) -> usize {
        self.active.load(Ordering::Relaxed)
    }
    pub fn is_connected(&self) -> bool {
        self.connected.load(Ordering::Relaxed)
    }

    pub async fn shutdown(self) -> Result<()> {
        self.cancel.cancel();
        self.tracker.wait().await;
        tracing::info!("client tunnel \"{}\" stopped", self.name);
        Ok(())
    }
}

enum LocalSocket {
    Tcp(TcpListener),
    Udp(Arc<UdpSocket>),
}

#[allow(clippy::too_many_arguments)]
async fn supervise(
    local: LocalSocket,
    host: HostTarget,
    protocol: TunnelProtocol,
    relay: RelayMode,
    discovery: bool,
    policy: ReconnectPolicy,
    cancel: CancellationToken,
    tracker: TaskTracker,
    active: Arc<AtomicUsize>,
    connected: Arc<std::sync::atomic::AtomicBool>,
    tunnel_name: String,
) {
    let mut attempt: u32 = 0;
    let mut backoff = policy.initial_backoff;

    loop {
        if cancel.is_cancelled() {
            return;
        }

        // Build a fresh endpoint for this connection attempt.
        let mut ep_builder = Endpoint::builder().relay_mode(relay.clone());
        if discovery {
            ep_builder = ep_builder.discovery_n0();
        }
        let endpoint = match ep_builder.bind().await {
            Ok(e) => e,
            Err(e) => {
                tracing::warn!("client \"{tunnel_name}\": endpoint bind failed: {e}");
                if !should_retry(&policy, attempt, &cancel).await {
                    return;
                }
                sleep_backoff(&mut backoff, &policy, &cancel).await;
                attempt += 1;
                continue;
            }
        };

        // Attempt the connection.
        let conn_fut = async {
            match &host {
                HostTarget::Key(k) => endpoint.connect(*k, ALPN).await,
                HostTarget::Addr(a) => endpoint.connect(a.clone(), ALPN).await,
            }
        };
        let conn_result = tokio::select! {
            r = conn_fut => Some(r),
            _ = cancel.cancelled() => None,
        };
        let Some(conn_result) = conn_result else {
            endpoint.close().await;
            return;
        };
        let conn = match conn_result {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!("client \"{tunnel_name}\": connect failed: {e}");
                endpoint.close().await;
                if !should_retry(&policy, attempt, &cancel).await {
                    return;
                }
                sleep_backoff(&mut backoff, &policy, &cancel).await;
                attempt += 1;
                continue;
            }
        };

        // handshake
        let hello_result = tokio::select! {
            r = send_hello(&conn) => Some(r),
            _ = cancel.cancelled() => None,
        };
        let Some(hello_result) = hello_result else {
            conn.close(VarInt::from_u32(0), b"cancelled");
            endpoint.close().await;
            return;
        };
        if let Err(e) = hello_result {
            tracing::warn!("client \"{tunnel_name}\": hello failed: {e}");
            conn.close(VarInt::from_u32(1), b"hello failed");
            endpoint.close().await;
            if !should_retry(&policy, attempt, &cancel).await {
                return;
            }
            sleep_backoff(&mut backoff, &policy, &cancel).await;
            attempt += 1;
            continue;
        }

        // Connected
        attempt = 0;
        backoff = policy.initial_backoff;
        connected.store(true, Ordering::Relaxed);
        tracing::info!("client tunnel \"{tunnel_name}\" connected to host");

        let session_result = match protocol {
            TunnelProtocol::Tcp => {
                let LocalSocket::Tcp(ref listener) = local else {
                    unreachable!()
                };
                run_tcp_session(
                    listener,
                    conn.clone(),
                    tracker.clone(),
                    active.clone(),
                    cancel.clone(),
                )
                .await
            }
            TunnelProtocol::Udp => {
                let LocalSocket::Udp(ref sock) = local else {
                    unreachable!()
                };
                run_udp_session(
                    sock.clone(),
                    conn.clone(),
                    tracker.clone(),
                    active.clone(),
                    cancel.clone(),
                )
                .await
            }
        };

        connected.store(false, Ordering::Relaxed);
        conn.close(VarInt::from_u32(0), b"session ended");
        endpoint.close().await;

        if let Err(e) = session_result {
            tracing::warn!("client \"{tunnel_name}\": session ended with error: {e}");
        } else {
            tracing::debug!("client \"{tunnel_name}\": session ended cleanly");
        }

        if cancel.is_cancelled() {
            return;
        }
        if !should_retry(&policy, attempt, &cancel).await {
            return;
        }
        sleep_backoff(&mut backoff, &policy, &cancel).await;
        attempt += 1;
    }
}

async fn should_retry(policy: &ReconnectPolicy, attempt: u32, cancel: &CancellationToken) -> bool {
    if cancel.is_cancelled() {
        return false;
    }
    !matches!(policy.max_attempts, Some(max) if attempt >= max)
}

async fn sleep_backoff(
    backoff: &mut Duration,
    policy: &ReconnectPolicy,
    cancel: &CancellationToken,
) {
    let this = *backoff;
    tokio::select! {
        _ = cancel.cancelled() => {}
        _ = tokio::time::sleep(this) => {}
    }
    let next = (*backoff * 2).min(policy.max_backoff);
    *backoff = if next.is_zero() {
        policy.initial_backoff
    } else {
        next
    };
}

async fn send_hello(conn: &Connection) -> Result<()> {
    let mut s = conn.open_uni().await?;
    write_frame(
        &mut s,
        &Hello {
            version: PROTOCOL_VERSION,
            capabilities: 0,
        },
    )
    .await?;
    s.finish().map_err(|e| Error::Protocol(e.to_string()))?;
    Ok(())
}

async fn run_tcp_session(
    listener: &TcpListener,
    conn: Connection,
    tracker: TaskTracker,
    active: Arc<AtomicUsize>,
    cancel: CancellationToken,
) -> Result<()> {
    loop {
        tokio::select! {
            _ = cancel.cancelled() => return Ok(()),
            _ = conn.closed() => return Ok(()),
            r = listener.accept() => {
                let (stream, source) = match r {
                    Ok(x) => x,
                    Err(e) => {
                        tracing::warn!("client tcp accept: {e}");
                        tokio::time::sleep(Duration::from_millis(100)).await;
                        continue;
                    }
                };

                let (mut send, recv) = conn.open_bi().await?;
                write_frame(&mut send, &ConnReq { client_local_addr: source }).await?;
                let cancel_c = cancel.clone();
                let active_c = active.clone();
                tracker.spawn(async move {
                    active_c.fetch_add(1, Ordering::Relaxed);
                    if let Err(e) = bi_tcp_forward(stream, send, recv, cancel_c).await {
                        tracing::debug!("client tcp forwarder: {e}");
                    }
                    active_c.fetch_sub(1, Ordering::Relaxed);
                });
            }
        }
    }
}

async fn run_udp_session(
    socket: Arc<UdpSocket>,
    conn: Connection,
    tracker: TaskTracker,
    active: Arc<AtomicUsize>,
    cancel: CancellationToken,
) -> Result<()> {
    let sessions: Arc<RwLock<HashMap<SocketAddr, mpsc::Sender<Vec<u8>>>>> =
        Arc::new(RwLock::new(HashMap::new()));

    let mut buf = vec![0u8; 65_535];
    loop {
        tokio::select! {
            _ = cancel.cancelled() => return Ok(()),
            _ = conn.closed() => return Ok(()),
            r = socket.recv_from(&mut buf) => {
                let (n, source) = match r {
                    Ok(x) => x,
                    Err(e) => {
                        tracing::warn!("client udp recv: {e}");
                        tokio::time::sleep(Duration::from_millis(100)).await;
                        continue;
                    }
                };
                let payload = buf[..n].to_vec();

                let existing = { sessions.read().await.get(&source).cloned() };
                if let Some(tx) = existing {
                    if tx.send(payload).await.is_err() {
                        // Session closed
                        sessions.write().await.remove(&source);
                    }
                    continue;
                }

                // New source
                let (mut send, recv) = conn.open_bi().await?;
                write_frame(&mut send, &ConnReq { client_local_addr: source }).await?;

                let (tx, rx) = mpsc::channel::<Vec<u8>>(64);
                sessions.write().await.insert(source, tx.clone());

                let _ = tx.send(payload).await;

                let socket_c = socket.clone();
                let sessions_c = sessions.clone();
                let active_c = active.clone();
                let cancel_c = cancel.clone();
                tracker.spawn(async move {
                    active_c.fetch_add(1, Ordering::Relaxed);
                    if let Err(e) = bi_udp_client_session(socket_c, source, rx, send, recv, cancel_c).await {
                        tracing::debug!("client udp session {source}: {e}");
                    }
                    active_c.fetch_sub(1, Ordering::Relaxed);
                    sessions_c.write().await.remove(&source);
                });
            }
        }
    }
}
