mod utils;
mod control;
use std::net::SocketAddr;

/// The state of the LanTun application.
/// This tracks both the host and client state,
/// so a user can use other tunnels as well as create their own.
struct LanTunState {
    /// The state of the tunnels hosted by the user.
    host_state: HostState,
    /// The state of the tunnels this user is connected to.
    client_state: ClientState,
} 

/// The host state.
struct HostState {
    tunnels: Vec<HostTunnel>,
    host_endpoint: iroh::endpoint::Endpoint,
    control_channel: ()
}

/// The client state.
struct ClientState {
    tunnels: Vec<ClientTunnel>,
    client_endpoint: iroh::endpoint::Endpoint,
}

struct HostTunnel {
    /// The secret allows others to connect to this tunnel.
    /// This value is unique for every existing tunnel.
    secret: String,
    /// The local socket address this tunnel is bound to.
    host_addr: SocketAddr,
}

struct ClientTunnel {
    /// The secret of the tunnel this client is connected to.
    /// This value is unique for every existing tunnel.
    secret: String,
    /// The remote socket address (of the host) this tunnel is connected to.
    host_addr: SocketAddr,
    /// The local address which the traffic of the tunnel will be forwarded to.
    client_addr: SocketAddr,
}

