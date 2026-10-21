# Footprint

Relay's claim is not that it is fast. It is that it is **small**, and the
reason is architectural rather than clever: it does not ship a browser.

An Electron application bundles its own copy of Chromium and Node. Three
Electron apps on a machine means three Chromiums resident. Relay uses the
webview the operating system already has — WebKitGTK on Linux, WebView2
on Windows, WKWebView on macOS — so its share of that engine is divided
with everything else using it.

## Measured

Same machine, same method, all idle. Taken with
`./scripts/measure-memory.sh`.

| Application | Processes | RSS | PSS |
|---|---:|---:|---:|
| **Relay** | 3 | 415 MB | **251 MB** |
| Slack (Electron) | 10 | 686 MB | 501 MB |
| VS Code (Electron) | 53 | 6,935 MB | 5,518 MB |

Machine: Intel i7-12700H, 31 GB RAM, KDE neon 24.04 (Wayland).

**PSS is the number to read.** RSS counts a shared library in full against
every process that maps it, so adding RSS across a process tree
double-counts everything shared. PSS divides each shared page by the
number of processes sharing it, which is what the machine actually gives
up to run the application.

The gap between Relay's own RSS and PSS — 415 against 251 — *is* the
sharing, visible. An Electron app's private Chromium cannot be shared with
anything, so its two figures sit much closer together.

### What this does and does not say

- **Relay is about half Slack's size.** Slack is the fair comparison: one
  purpose, one window, Electron. VS Code is in the table for scale, not as
  a comparison — it is running extensions and language servers and is
  doing far more than either.
- **It is not twenty times smaller.** A webview is still a browser engine,
  even a shared one. Anyone expecting a 30 MB native application will be
  disappointed; the honest figure is "half an Electron app", not "a
  rounding error".
- **Relay here is idle with an empty workspace.** A large collection open
  and a 10 MB JSON response rendered will cost more. Large responses spool
  to disk rather than into the webview precisely to bound that.
- **Reported Postman figures are not in this table.** A 1 GB reading on a
  different machine, a different OS and a different Electron version is
  not something this project measured, so it is not quoted as if it were.

## Download size

This gap is larger and easier to verify than the memory one.

| | Size |
|---|---:|
| Relay `.deb` / `.rpm` | **7.3 MB** |
| Relay `.AppImage` | 82 MB |
| Relay binary, stripped | 18 MB |

The `.deb` is small because it contains Relay and nothing else — it
depends on `libwebkit2gtk-4.1-0` and `libgtk-3-0`, which the system
already has. The AppImage is ten times larger for exactly the opposite
reason: it carries its dependencies so it can run anywhere.

An Electron application's installer carries a browser whatever the
packaging, which is why they start around 100 MB and go up.

## Reproducing it

```sh
./scripts/measure-memory.sh              # Relay
./scripts/measure-memory.sh slack        # anything else
```

Start the application, leave it idle for ten seconds or so, then measure.
Memory right after launch is still settling and reads low.

## Not yet measured

Stated plainly rather than estimated:

- Startup time, cold and warm
- Memory with a large collection loaded
- Memory while rendering a large response
- CPU while idle
- Comparable figures on Windows and macOS

Until those exist, the README says nothing about them.
