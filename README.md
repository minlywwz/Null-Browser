# Null Browser

A standalone, independent web browser prototype for Windows 11 x64 built from scratch without Chromium, Gecko, WebKit, or WebView. The address bar opens HTTP(S) URLs and domains (auto-completing HTTPS). A custom Rust engine constructs the DOM, resolves CSS styling, calculates layout, and dispatches a lightweight Display List to a high-performance C++ DirectX 11 (D3D11) compositor via the DXGI flip model.

## Building

Building requires Rust stable, MSVC with C++ build tools, and the Windows SDK. From the project root:

```powershell
cargo build --workspace --release --locked
.\target\release\browser_app.exe
cargo test --workspace --locked
```

Ensure `web_content_process.exe` is placed alongside `browser_app.exe`. It executes inside a Windows AppContainer sandbox with Job Object resource constraints for untrusted web pages and selected local HTML files, parsing HTML/CSS, executing scripts via QuickJS, and decoding images.

## Navigation & Shortcuts

- **Tabs**:
  - **Ctrl+T**: Open a new tab.
  - **Ctrl+W**: Close current tab.
  - **Ctrl+Tab** / **Ctrl+Shift+Tab**: Switch to next / previous tab.
  - **Ctrl+1** .. **Ctrl+9**: Jump directly to tab by index.
  - Click on tabs in the top tab strip to switch, click `×` to close, or click `+` to create a new tab.
- **Address Bar**: **Ctrl+L** focuses and selects the address bar. Enter a domain like `example.com` or a full URL. Use explicit `http://` for unencrypted HTTP. The `--url <address>` command-line option opens an address on startup.
- **Navigation**: **Alt+←** and **Alt+→** navigate back and forward through tab history. **Ctrl+R** or **F5** reloads the active page.
- **Local Files**: **⋯ → Open HTML** or **Ctrl+O** opens a local `.html` or `.htm` file encoded in UTF-8. You can also pass the file path as the first startup argument.
- **Browser Studio**: **⋯ → Browser Studio** or **F2** opens the browser customizer (toolbar positioning, accent color themes, animation speeds). Configuration is saved to `%LOCALAPPDATA%\Browser\browser-studio.ini`. Use `--profile <path>` for isolated configurations.
- **Motion Engine**: **⋯ → Motion Engine** (or `--motion-demo`) showcases layer transitions on a synthetic GPU procedural texture. **F** expands into borderless fullscreen, **Esc** returns to windowed mode.
- **Diagnostics & Probing**:
  - `--log <file>` records frame present timings, page layout latencies, and Motion Engine transitions.
  - `--warp` forces Windows Advanced Rasterization Platform (WARP) software D3D11 rendering for diagnostics.
  - `--motion-probe 30 --log motion.log` executes 30 automated windowed-to-fullscreen transitions, logs frame dispatch intervals, and exits cleanly.

## Engine Capabilities

The engine renders static documents, server-side HTML, and interactive pages:
- **Layout & CSS**: Block layout, inline text runs with mixed styling and UTF-16 ranges, tables, lists, basic Flexbox (`display: flex`, axis alignments), and basic Grid (fixed tracks, percentages, `fr`, and numeric `repeat()`). CSS selectors support tags, classes, IDs, attribute selectors, `!important` declarations, cascade inheritance, and basic media queries.
- **Media & Images**: Decodes PNG, JPEG, WebP, GIF, and static SVG from remote URLs or inline base64 `data:` URIs.
- **JavaScript**: QuickJS executes embedded and external scripts with lightweight DOM bindings (`click` handlers, DOM mutations, timers). Lightweight TypeScript annotations are stripped on the fly.
- **Security & Network**: Network stack enforces TLS validation, limits payload sizes (documents up to 2 MiB, stylesheets 256 KiB, scripts 1 MiB, images 2 MiB), and prevents SSRF by blocking requests to local, loopback, and private IP ranges.

## Architecture

Source code is organized into modular layers:
- `browser`: Win32 window host, tab strip UI, and GPU compositor coordinator (`browser_app`, `browser_core`).
- `content`: HTML/CSS parser, DOM, style & layout engine, QuickJS runtime (`web_engine`), and isolated worker process (`web_content_process`).
- `gpu`: Abstract display list (`display_list`), D3D11 / Direct2D renderer and Motion Engine compositor (`rendering_engine`, `rendering_ffi`).
- `net`: Network stack, SSRF protection, URL validation, and HTTP/TLS fetcher.
- `components`: Browser Studio configuration model (`browser_studio`).
- `testing`: Fixtures and integration tests.

For detailed module boundaries, IPC contracts, and pipeline flow, see [ARCHITECTURE.md](ARCHITECTURE.md).
