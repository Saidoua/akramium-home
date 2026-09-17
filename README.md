# Akramium Home

One small daemon for a household: a private drive (DeKave), documents (Doks) and mail
(Komail), served on one port with one sign-in, used from the Akramium browser. It runs
on the AkramiumOS PC itself or on a NAS.

Status: DeKave is being built first. Doks and Komail follow.

## Run it

```
cargo run -p akramium-home -- --data-dir ./data
```

The first start prints a one-time setup link (also written to `data/setup-link`). Open it,
create the admin account, and the drive is at `http://localhost:11720/drive/`.

Configuration lives in `home.toml` (see `home.example.toml`); `HOME_CONFIG`, `HOME_LISTEN`
and `HOME_DATA_DIR` override it. During development `HOME_ASSETS_DIR=$PWD` serves the pages
from disk so a CSS change needs no rebuild.

## Mount the drive (WebDAV)

The drive is also at `http://<host>:11720/dav/`, with your Akramium Home name and password.

- macOS: Finder, Go, Connect to Server, then that address.
- Linux: `dav://<host>:11720/dav/` in the file manager, or davfs2.
- Windows: Explorer refuses name-and-password sign-in over plain `http` unless
  `BasicAuthLevel` is set to 2 under `HKLM\SYSTEM\CurrentControlSet\Services\WebClient\Parameters`.
  Turning on `[tls]` avoids that.

Deleting through a mount moves the item to the drive's trash. Files desktop clients leave
behind (`._name`, `.DS_Store`, `~$name`, `Thumbs.db`) are kept for those clients, hidden from
the web pages, and skip the trash.

## Layout

- `home-core/`: config, accounts (Argon2id), sessions, the SQLite database, embedded pages,
  the security headers every response carries.
- `dekave/`: the drive. Files stay real files under `data/users/<id>/files/`; SQLite holds
  the index. Web UI at `/drive/`, JSON at `/api/drive/`.
- `akramium-home/`: the binary that mounts the enabled modules.

## On the network

Set `listen = "0.0.0.0:11720"` and the daemon announces itself: `http://akramium.local:11720`
opens it from any device in the house, and file managers list the drive under Network. If
the machine is reached by another name, add that name to `host_names`.

For https, set `[tls] enabled = true`. The install makes its own certificate authority
under `data_dir/tls` and serves `https://akramium.local:11743`. Each device trusts it once:
open `http://akramium.local:11720/home/ca.pem` and add it as a trusted authority. The
certificate covers the configured names and the machine's addresses, and renews itself.

## Security model

The daemon is meant for a home LAN.

- Sessions are hashed random tokens in an `HttpOnly` `SameSite=Strict` cookie, `Secure` over https.
- Requests for a host name the install does not know are refused (421), which stops DNS
  rebinding. Writes that a browser marks as coming from another site are refused (403).
- Five wrong passwords from one address mean a 30 second wait, doubling up to 15 minutes.
- Uploaded content is served with a `sandbox` content security policy and `nosniff`; HTML,
  SVG and XML always download instead of rendering.
- Every name passes one validator before it reaches the disk.
- `/dav` takes a name and password only; the pages' API takes the session cookie only.

## License

MIT or Apache-2.0, at your option.
