# limuxd-remote (Go)

Go remote daemon for `limux ssh` bootstrap, capability negotiation, and remote proxy RPC. It is not in the terminal keystroke hot path.

## Commands

1. `limuxd-remote version`
2. `limuxd-remote serve --stdio`
3. `limuxd-remote cli <command> [args...]` — relay limux commands to the local app over the reverse SSH forward

When invoked as `limux` (via wrapper/symlink installed during bootstrap), the binary auto-dispatches to the `cli` subcommand. This is busybox-style argv[0] detection.

## RPC methods (newline-delimited JSON over stdio)

1. `hello`
2. `ping`
3. `proxy.open`
4. `proxy.close`
5. `proxy.write`
6. `proxy.stream.subscribe`
7. async `proxy.stream.data` / `proxy.stream.eof` / `proxy.stream.error` events
8. `session.open`
9. `session.close`
10. `session.attach`
11. `session.resize`
12. `session.detach`
13. `session.status`

Current integration in limux:
1. `workspace.remote.configure` bootstraps this binary over SSH when missing.
2. Client sends `hello` before enabling remote proxy transport.
3. Local workspace proxy broker serves SOCKS5 + HTTP CONNECT and tunnels stream traffic through `proxy.*` RPC over `serve --stdio`, using daemon-pushed stream events instead of polling reads.
4. Daemon status/capabilities are exposed in `workspace.remote.status -> remote.daemon` (including `session.resize.min`).

`workspace.remote.configure` contract notes:
1. `port` / `local_proxy_port` accept integer values and numeric strings; explicit `null` clears each field.
2. Out-of-range values and invalid types return `invalid_params`.
3. `local_proxy_port` is an internal deterministic test hook used by bind-conflict regressions.
4. SSH option precedence checks are case-insensitive; user overrides for `StrictHostKeyChecking` and control-socket keys prevent default injection.

## Distribution

Release builds publish prebuilt `limuxd-remote` binaries on GitHub Releases for:
1. `darwin/arm64`
2. `darwin/amd64`
3. `linux/arm64`
4. `linux/amd64`

The app fetches a manifest at runtime from `RyantHults/limux` Releases (tag shape `limuxd-remote-vX.Y.Z`) that contains:
1. exact release asset URLs per `(GOOS, GOARCH)`
2. pinned SHA-256 digests
3. release tag and checksums asset URL

The Rust app downloads and caches the matching binary locally, verifies its SHA-256, then SCPs it to the remote host if needed. Dev builds can opt into a local `go build` fallback with `LIMUX_REMOTE_DAEMON_ALLOW_LOCAL_BUILD=1`.

## CLI relay

The `cli` subcommand (or `limux` wrapper/symlink) connects to the local limux app through an SSH reverse forward and relays commands. It supports both v1 text protocol and v2 JSON-RPC commands.

Socket discovery order:
1. `--socket <path>` flag
2. `LIMUX_SOCKET_PATH` environment variable
3. `~/.limux/socket_addr` file (written by the app after the reverse relay establishes)

For TCP addresses, the CLI dials once and only refreshes `~/.limux/socket_addr` a single time if the first address was stale. Relay metadata is published only after the reverse forward is ready, so steady-state use does not rely on polling.

Authenticated relay details:
1. Each SSH workspace gets its own relay ID and relay token.
2. The app runs a local loopback relay server that requires an HMAC-SHA256 challenge-response (protocol id `limux-relay-auth`) before forwarding a command to the real local Unix socket.
3. The remote shell never gets direct access to the local app socket. It only gets the reverse-forwarded relay port plus `~/.limux/relay/<port>.auth`, which is written with `0600` permissions and removed when the relay stops.

Integration additions for the relay path:

1. Bootstrap installs `~/.limux/bin/limux` wrapper and keeps a default daemon target (`~/.limux/bin/limuxd-remote-current`).
2. A background `ssh -N -R` process reverse-forwards a TCP port to the authenticated local relay server. The relay address is written to `~/.limux/socket_addr` on the remote.
3. Relay startup writes `~/.limux/relay/<port>.daemon_path` so the wrapper can route each shell to the correct daemon binary when multiple local limux instances or versions coexist.
4. Relay startup writes `~/.limux/relay/<port>.auth` with the relay ID and token needed for HMAC authentication.
