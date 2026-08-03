# lantun

lantun is a networking tool that makes it possible to expose local ports over a secure peer-to-peer tunnel to the internet. It is **not** a reverse proxy — both peers need to run the `lantun` binary.

Under the hood it uses [iroh](https://github.com/n0-computer/iroh) for NAT-traversing QUIC connections. Traffic is end-to-end encrypted between peers; a relay is used only when a direct connection isn't reachable.

## Install

```
cargo install lantun
```

## Tunnel model

A host exposes a local service by creating a `HostTunnel`, identified by a 32-byte public key (shown as 64 hex chars). Anyone who knows the public key can create a `ClientTunnel` that binds a local port on their machine and forwards traffic through the encrypted tunnel to the host's service.

Both TCP and UDP are supported. Multiple clients can connect to the same host, and each client can open multiple concurrent connections through its local tunnel port.

Example: host runs a Minecraft server on `127.0.0.1:25565/tcp` → clients bind `127.0.0.1:25565/tcp` locally and connect their Minecraft client to `127.0.0.1:25565`.

## CLI usage

Host side:

```
$ lantun add-host 127.0.0.1:25565 tcp mc-server
Created host tunnel "mc-server" on 127.0.0.1:25565/tcp
Public key: e929b2f2cc170c71ec7b3e71cc78b041c29545095ee0c1d74baba545c5091b02
Peers can connect with:
  lantun add-client e929b2f2cc170c71ec7b3e71cc78b041c29545095ee0c1d74baba545c5091b02 <local> tcp <name>
```

Client side:

```
$ lantun add-client e929b2f2cc170c71ec7b3e71cc78b041c29545095ee0c1d74baba545c5091b02 127.0.0.1:25565 tcp mc-client
```

Run the tunnels on both machines:

```
$ lantun
```

Ctrl-C to stop.

Other subcommands: `list`, `remove <name>`, `enable <name>`, `disable <name>`. Disabled tunnels are skipped when `lantun` is run without arguments. See `lantun --help` for the full listing.

### Running without a config file (`--host-tunnels` / `--client-tunnels`)

```
lantun \
  --host-tunnels '[{"name":"mc","local":"127.0.0.1:25565","protocol":"tcp","secret_key":"HEX..."}]' \
  --client-tunnels '[{"name":"remote-mc","local":"127.0.0.1:25565","protocol":"tcp","host_key":"HEX..."}]'
```

(`host_key` on a client entry is the host's public key — the same hex string `lantun add-host` prints.)

## Config file location

- Linux: `~/.config/lantun/lantun.toml`
- macOS: `~/Library/Application Support/lantun/lantun.toml`
- Windows: `%APPDATA%\lantun\lantun.toml`
