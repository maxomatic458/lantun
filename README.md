# lantun

lantun is a networking tool that makes it possible to expose local ports over a secure tunnel to the internet.
lantun is **not** a reverse proxy, so both the host and client need to run the lantun client.

## Tunnel system
A host can expose a port over the internet by creating a `HostTunnel`, which is identified
by a 64 character long ID. Everyone who knows this ID can create a `ClientTunnel` to connect to the host.

A `HostTunnel` can be bound to a specific IP address and port like `127.0.0.1:8080/tcp`.
The `ClientTunnel` can expose the tunneled connection on a IP address and port on the client network like `127.0.0.1:5000/tcp`.

This would establish a tunneled connection between client and host.


Host: `127.0.0.1:8080/tcp` <-> Client: `127.0.0.1:5000/tcp`

So the client can now essentially access the hosts service on port 8080 over their own local port 5000.

## Usage

Lets say the Host wants to expose a minecraft server running on port 25565.

On the host side run:
```
$ lantun add-host 127.0.0.1:25565 tcp mc-server 

Tunnel "mc-server" with local addr 127.0.0.1:25565/tcp created.
The public secret is "e929b2f2cc170c71ec7b3e71cc78b041c29545095ee0c1d74baba545c5091b02"

To add a client tunnel on the client side, use:
  lantun add-client e929b2f2cc170c71ec7b3e71cc78b041c29545095ee0c1d74baba545c5091b02 127.0.0.1:25565 tcp <name>
```

On the client side run:
```
$ lantun add-client e929b2f2cc170c71ec7b3e71cc78b041c29545095ee0c1d74baba545c5091b02 127.0.0.1:25565 tcp <name>
```

Now start the lantun program on both sides and keep it running to tunnel the connection:
```
$ lantun
```

see `lantun --help` for more options.

### Details
Under the hood lantun uses [iroh](https://github.com/n0-computer/iroh) to establish peer-to-peer connections. If a direct connection is not possible a relay server will be used.
Tunnel traffic is encrypted using the private and public keys of the tunnel.

Location of the lantun config file:
- Linux: `~/.config/lantun/config.toml`
- Windows: `%APPDATA%/lantun/config.toml`
- MacOS: `~/Library/Application Support/lantun/config.toml`