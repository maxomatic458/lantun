//! Helpers for tests that spawn the compiled `lantun` binary as a subprocess.
//!
//! These tests intentionally use real n0 discovery — on the same machine both endpoints
//! find each other's loopback direct addresses quickly, so the relay is not on the data
//! path. Network access is required.

#![cfg(unix)]
#![allow(dead_code)]

use std::{
    net::SocketAddr,
    path::{Path, PathBuf},
    process::{ExitStatus, Stdio},
    time::Duration,
};

use lantun::{SecretKey, gen_secret};
use tempfile::TempDir;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream, UdpSocket},
    process::{Child, Command},
    task::JoinHandle,
    time::timeout,
};

/// Compile-time path to the freshly-built lantun binary.
pub const LANTUN_BIN: &str = env!("CARGO_BIN_EXE_lantun");

/// Overall deadline for "the tunnel should be ready" polling.
pub const READY_TIMEOUT: Duration = Duration::from_secs(30);

/// Single-operation deadline for basic round-trips once the tunnel is ready.
pub const OP_TIMEOUT: Duration = Duration::from_secs(10);

/// A running `lantun` subprocess. Its config directory lives inside the returned TempDir.
/// Killed on Drop as a safety net; call `sigint_and_wait` for a clean shutdown test.
pub struct LantunProc {
    pub child: Child,
    pub config_dir: TempDir,
    pub config_path: PathBuf,
    pub pid: u32,
    pub label: String,
}

impl LantunProc {
    pub async fn sigint_and_wait(mut self) -> ExitStatus {
        send_sigint(self.pid);
        // Give it a generous window — SIGINT triggers the async ctrl_c handler which then
        // shuts down each tunnel (each tunnel closes its endpoint, waits for tasks).
        timeout(Duration::from_secs(15), self.child.wait())
            .await
            .unwrap_or_else(|_| panic!("{} did not exit within 15s of SIGINT", self.label))
            .expect("wait failed")
    }

    pub async fn kill_and_wait(mut self) {
        let _ = self.child.start_kill();
        let _ = self.child.wait().await;
    }
}

/// Send SIGINT via the system `kill` binary — avoids adding libc/nix as a dev-dep.
fn send_sigint(pid: u32) {
    let _ = std::process::Command::new("kill")
        .arg("-INT")
        .arg(pid.to_string())
        .status();
}

/// Build a host config file. Returns (proc handle, host public key hex).
pub async fn spawn_host(label: &str, entries: Vec<HostSpec>) -> (LantunProc, Vec<String>) {
    let dir = TempDir::new().unwrap();
    let cfg_path = dir.path().join("lantun.toml");

    let mut toml = String::new();
    let mut public_keys = Vec::new();
    for e in &entries {
        public_keys.push(hex::encode(e.secret.public()));
        toml.push_str(&format!(
            "[[host_tunnels]]\nname = \"{}\"\nlocal = \"{}\"\nprotocol = \"{}\"\nsecret_key = \"{}\"\nenabled = {}\n\n",
            e.name,
            e.backend,
            e.protocol,
            hex::encode(e.secret.to_bytes()),
            e.enabled,
        ));
    }
    std::fs::write(&cfg_path, toml).unwrap();

    let child = spawn_lantun(&cfg_path, label).await;
    (
        LantunProc {
            pid: child.id().expect("pid"),
            child,
            config_dir: dir,
            config_path: cfg_path,
            label: format!("host[{label}]"),
        },
        public_keys,
    )
}

pub async fn spawn_client(label: &str, entries: Vec<ClientSpec>) -> LantunProc {
    let dir = TempDir::new().unwrap();
    let cfg_path = dir.path().join("lantun.toml");

    let mut toml = String::new();
    for e in &entries {
        toml.push_str(&format!(
            "[[client_tunnels]]\nname = \"{}\"\nlocal = \"{}\"\nprotocol = \"{}\"\nhost_key = \"{}\"\nenabled = {}\n\n",
            e.name, e.bind, e.protocol, e.host_key, e.enabled,
        ));
    }
    std::fs::write(&cfg_path, toml).unwrap();

    let child = spawn_lantun(&cfg_path, label).await;
    LantunProc {
        pid: child.id().expect("pid"),
        child,
        config_dir: dir,
        config_path: cfg_path,
        label: format!("client[{label}]"),
    }
}

/// Spawn a lantun binary with inline `--host-tunnels` / `--client-tunnels` JSON args.
/// Points `--config` at a non-existent path in a fresh tempdir; the CLI should NOT
/// create it (the whole point of inline mode).
pub async fn spawn_lantun_inline(
    label: &str,
    hosts_json: Option<String>,
    clients_json: Option<String>,
) -> (LantunProc, PathBuf) {
    let dir = TempDir::new().unwrap();
    let cfg_path = dir.path().join("should-not-exist.toml");

    let mut cmd = Command::new(LANTUN_BIN);
    // Point --config at a path in a scratch dir. If inline mode is doing its job, this
    // file must remain nonexistent after startup.
    cmd.arg("--config").arg(&cfg_path);
    if let Some(json) = &hosts_json {
        cmd.arg("--host-tunnels").arg(json);
    }
    if let Some(json) = &clients_json {
        cmd.arg("--client-tunnels").arg(json);
    }
    cmd.env("LANTUN_RECONNECT_INITIAL_MS", "100");
    cmd.env("LANTUN_RECONNECT_MAX_MS", "500");
    cmd.env("RUST_LOG", "lantun=info");
    cmd.env("LANTUN_LABEL", label);
    cmd.kill_on_drop(true);
    cmd.stdin(Stdio::null());
    cmd.stdout(Stdio::inherit());
    cmd.stderr(Stdio::inherit());
    let child = cmd.spawn().expect("spawn lantun");

    (
        LantunProc {
            pid: child.id().expect("pid"),
            child,
            config_dir: dir,
            config_path: cfg_path.clone(),
            label: format!("inline[{label}]"),
        },
        cfg_path,
    )
}

/// Spawn a lantun binary. Inherits stdio so failures print into the test output.
async fn spawn_lantun(config: &Path, label: &str) -> Child {
    let mut cmd = Command::new(LANTUN_BIN);
    cmd.arg("--config").arg(config);
    // Tight reconnect backoff so restart / disconnect tests aren't waiting on the default
    // 1s→60s exponential.
    cmd.env("LANTUN_RECONNECT_INITIAL_MS", "100");
    cmd.env("LANTUN_RECONNECT_MAX_MS", "500");
    // Test-friendly logging: only lantun itself, info level.
    cmd.env("RUST_LOG", "lantun=info");
    cmd.env("LANTUN_LABEL", label);
    cmd.kill_on_drop(true);
    cmd.stdin(Stdio::null());
    // Inherit stdout/stderr so tests print any diagnostic output on failure.
    cmd.stdout(Stdio::inherit());
    cmd.stderr(Stdio::inherit());
    cmd.spawn().expect("spawn lantun")
}

pub struct HostSpec {
    pub name: String,
    pub backend: SocketAddr,
    pub protocol: &'static str,
    pub secret: SecretKey,
    pub enabled: bool,
}

pub struct ClientSpec {
    pub name: String,
    pub bind: SocketAddr,
    pub protocol: &'static str,
    pub host_key: String,
    pub enabled: bool,
}

impl HostSpec {
    pub fn tcp(name: &str, backend: SocketAddr) -> Self {
        Self {
            name: name.into(),
            backend,
            protocol: "tcp",
            secret: gen_secret(),
            enabled: true,
        }
    }
    pub fn udp(name: &str, backend: SocketAddr) -> Self {
        Self {
            name: name.into(),
            backend,
            protocol: "udp",
            secret: gen_secret(),
            enabled: true,
        }
    }
    /// Use a caller-provided secret. Needed for restart tests where multiple host
    /// processes must share one iroh identity so the client's stored public key still
    /// resolves after a restart.
    pub fn tcp_with_secret(name: &str, backend: SocketAddr, secret: SecretKey) -> Self {
        Self {
            name: name.into(),
            backend,
            protocol: "tcp",
            secret,
            enabled: true,
        }
    }
}

impl ClientSpec {
    pub fn tcp(name: &str, bind: SocketAddr, host_key: &str) -> Self {
        Self {
            name: name.into(),
            bind,
            protocol: "tcp",
            host_key: host_key.into(),
            enabled: true,
        }
    }
    pub fn udp(name: &str, bind: SocketAddr, host_key: &str) -> Self {
        Self {
            name: name.into(),
            bind,
            protocol: "udp",
            host_key: host_key.into(),
            enabled: true,
        }
    }
}

// ------------------------- Echo servers (test backends) -------------------------

pub async fn spawn_tcp_echo() -> (SocketAddr, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = tokio::spawn(async move {
        loop {
            let (mut sock, _) = match listener.accept().await {
                Ok(x) => x,
                Err(_) => break,
            };
            tokio::spawn(async move {
                let mut buf = vec![0u8; 65_536];
                loop {
                    let n = match sock.read(&mut buf).await {
                        Ok(0) | Err(_) => return,
                        Ok(n) => n,
                    };
                    if sock.write_all(&buf[..n]).await.is_err() {
                        return;
                    }
                }
            });
        }
    });
    (addr, handle)
}

pub async fn spawn_udp_echo() -> (SocketAddr, JoinHandle<()>) {
    let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let addr = socket.local_addr().unwrap();
    let handle = tokio::spawn(async move {
        let mut buf = vec![0u8; 65_535];
        loop {
            let (n, src) = match socket.recv_from(&mut buf).await {
                Ok(x) => x,
                Err(_) => return,
            };
            let _ = socket.send_to(&buf[..n], src).await;
        }
    });
    (addr, handle)
}

// ------------------------- Readiness / port helpers -------------------------

/// Pick a free port by binding to `:0`, releasing, and returning the number. Small race but
/// fine for tests that immediately hand the port to the lantun subprocess.
pub async fn pick_free_port_tcp() -> u16 {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let p = l.local_addr().unwrap().port();
    drop(l);
    p
}

pub async fn pick_free_port_udp() -> u16 {
    let s = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let p = s.local_addr().unwrap().port();
    drop(s);
    p
}

/// Poll a TCP tunnel until an echo round-trip succeeds. Panics on `READY_TIMEOUT`.
/// This is the strongest "the whole pipe works" signal.
pub async fn wait_tcp_ready(addr: SocketAddr) {
    let start = std::time::Instant::now();
    let probe: [u8; 4] = *b"rdy?";
    loop {
        let attempt = async {
            let mut s = TcpStream::connect(addr).await?;
            s.write_all(&probe).await?;
            let mut buf = [0u8; 4];
            s.read_exact(&mut buf).await?;
            Ok::<_, std::io::Error>(buf == probe)
        };
        if let Ok(Ok(true)) = timeout(Duration::from_secs(2), attempt).await {
            return;
        }
        if start.elapsed() > READY_TIMEOUT {
            panic!("tcp tunnel at {addr} not ready within {READY_TIMEOUT:?}");
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// Poll a UDP tunnel until an echo round-trip succeeds. Panics on `READY_TIMEOUT`.
pub async fn wait_udp_ready(addr: SocketAddr) {
    let start = std::time::Instant::now();
    let probe: [u8; 4] = *b"udp?";
    loop {
        let sock = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        sock.connect(addr).await.unwrap();
        let attempt = async {
            sock.send(&probe).await?;
            let mut buf = [0u8; 4];
            let n = sock.recv(&mut buf).await?;
            Ok::<_, std::io::Error>(n == 4 && buf == probe)
        };
        if let Ok(Ok(true)) = timeout(Duration::from_millis(500), attempt).await {
            return;
        }
        if start.elapsed() > READY_TIMEOUT {
            panic!("udp tunnel at {addr} not ready within {READY_TIMEOUT:?}");
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}
