# Linux user-level systemd

This document describes a Linux **user** service for the currently available HTTP server. It uses the installed `memoro` executable and does not depend on an installation script.

The unit runs:

```text
memoro serve --transport http --host 127.0.0.1
```

The server listens on `127.0.0.1:8000` by default. Its health endpoint is `/health`, and the MCP streamable HTTP endpoint is mounted at `/mcp`.

## Prerequisites

- Install the `memoro` executable and make sure it is available at a stable absolute path. The examples below use `/usr/local/bin/memoro`; replace it with the output of `command -v memoro` if needed.
- Decide where to keep the bearer token. The example uses `~/.config/memoro/memoro.env`, readable only by the user.

Create the environment file:

```sh
mkdir -p ~/.config/memoro
chmod 700 ~/.config/memoro
printf 'MEMORO_TOKEN=%s\n' 'replace-with-a-long-random-token' > ~/.config/memoro/memoro.env
chmod 600 ~/.config/memoro/memoro.env
```

`MEMORO_TOKEN` is read by `memoro serve` and protects the `/mcp` endpoint with an HTTP `Authorization: Bearer ...` header. Keep the token secret. `/health` remains available as a health check without bearer authentication.

## Install the user unit

Create `~/.config/systemd/user/memoro.service`:

```ini
[Unit]
Description=Memoro MCP server
After=default.target

[Service]
ExecStart=/usr/local/bin/memoro serve --transport http --host 127.0.0.1
EnvironmentFile=%h/.config/memoro/memoro.env
Restart=on-failure

[Install]
WantedBy=default.target
```

If the executable is installed somewhere else, update `ExecStart` to its absolute path. Then reload the user manager and enable the service:

```sh
systemctl --user daemon-reload
systemctl --user enable memoro.service
```

This is a manual unit installation; the project does not provide an installation script.

## Start

```sh
systemctl --user start memoro.service
```

To start it automatically when the user manager starts, use `enable --now` instead of separate `enable` and `start` commands:

```sh
systemctl --user enable --now memoro.service
```

## Check status

```sh
systemctl --user status memoro.service
curl http://127.0.0.1:8000/health
```

A healthy server returns a JSON response containing `"status":"ok"`. To call the MCP endpoint, send the token from the environment file in the request's bearer authorization header.

## Stop

```sh
systemctl --user stop memoro.service
```

Stopping the unit does not delete the Memoro home or any Git-backed memory data.

## Uninstall

Disable the unit, stop it if necessary, remove the manually created unit and environment file, then reload systemd:

```sh
systemctl --user disable --now memoro.service
rm ~/.config/systemd/user/memoro.service
rm ~/.config/memoro/memoro.env
systemctl --user daemon-reload
```

The default uninstall leaves `~/.memoro` in place. That directory contains the local configuration, per-space Git repositories, Markdown memories, and lock files, so keeping it preserves data for a later reinstall.

**Warning:** deleting `~/.memoro` is a separate, explicit data-destruction step. Only do it after confirming that the data is backed up and no longer needed:

```sh
rm -rf ~/.memoro
```

Do not run that command as part of ordinary service uninstallation.
