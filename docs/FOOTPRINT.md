# Footprint

Relay's claim is not that it is fast. It is that it is **small**, and the
reason is architectural rather than clever: it does not ship a browser.

An Electron application bundles its own Chromium and Node. Three Electron
apps on a machine means three Chromiums resident. Relay uses the webview
the operating system already has — WebKitGTK on Linux, WebView2 on
Windows, WKWebView on macOS — so its share of that engine is divided with
everything else using it.

## Measured

Same machine, same method, both idle shortly after launch, nothing loaded.
Taken with `./scripts/measure-memory.sh`.

| | Processes | RSS | PSS |
|---|---:|---:|---:|
| **Relay** | 3 | 415 MB | **221 MB** |
| **Postman 11** (Electron) | 8 | 1,584 MB | **1,067 MB** |

**Relay is about one fifth of Postman's resident memory** — 4.8× smaller
by PSS, 3.8× by RSS.

Per process:

| Relay | RSS | PSS |  | Postman | RSS | PSS |
|---|---:|---:|---|---|---:|---:|
| `relay-desktop` | 162 MB | 80 MB |  | renderer | 700 MB | 608 MB |
| `WebKitWebProcess` | 204 MB | 123 MB |  | main | 382 MB | 249 MB |
| `WebKitNetworkProcess` | 48 MB | 17 MB |  | GPU | 161 MB | 83 MB |
| | | |  | utility ×4 | 331 MB | 117 MB |
| | | |  | launcher | 7 MB | 6 MB |

Machine: Intel i7-12700H, 31 GB RAM, KDE neon 24.04 (Wayland).
Postman 11 for Linux x64, installed from the official tarball.

**PSS is the number to read.** RSS counts a shared library in full against
every process that maps it, so adding RSS across a process tree
double-counts everything shared. PSS divides each shared page by the
number of processes sharing it, which is what the machine actually gives
up to run the application.

The gap between Relay's own RSS and PSS — 415 against 221 — *is* the
sharing, visible. Postman's two figures sit much closer together (1,584
against 1,067) because a private Chromium cannot be shared with anything.

### What this does and does not say

- **Both were idle with nothing open.** Load a large collection into
  either and both will grow. Relay spools large response bodies to disk
  rather than into the webview specifically to bound that, but the loaded
  case has not been measured.
- **Postman does more.** It syncs, runs a local agent, and signs you in.
  Some of that 1 GB is work Relay simply does not do — which is rather
  the point, but it is not a like-for-like feature comparison.
- **A webview is still a browser engine.** 221 MB is not nothing. Anyone
  expecting a 30 MB native application will be disappointed.
- **One machine, one OS.** Windows and macOS have not been measured.

## Download and disk

This gap is larger than the memory one and needs no caveats — it is just
file sizes.

| | Download | Installed |
|---|---:|---:|
| **Relay** `.deb` / `.rpm` | **7.3 MB** | ~20 MB |
| **Relay** `.AppImage` | 82 MB | 82 MB |
| **Postman** tarball | 199 MB | **538 MB** |

**27× smaller to download, 27× smaller on disk.**

The `.deb` is small because it contains Relay and nothing else; it depends
on `libwebkit2gtk-4.1-0` and `libgtk-3-0`, which the system already has.
The AppImage is ten times larger for the opposite reason: it carries its
dependencies so it runs anywhere. An Electron application's installer
carries a browser whichever packaging it uses.

## Reproducing it

```sh
./scripts/measure-memory.sh              # Relay
./scripts/measure-memory.sh Postman      # or any other process name
```

Start the application, leave it idle for a minute, then measure. Memory
immediately after launch is still settling and reads low.

> On Linux, launching an Electron application from a terminal inside
> VS Code fails with `bad option: --no-sandbox`. The editor exports
> `ELECTRON_RUN_AS_NODE=1`, which any Electron binary inherits and
> obeys, starting as plain Node. Unset it first.

## Not yet measured

Stated plainly rather than estimated:

- Startup time, cold and warm
- Memory with a large collection loaded
- Memory while rendering a large response
- CPU while idle
- Windows and macOS
