# Null Browser

<p align="center">
  <img src="https://img.shields.io/badge/Platform-Windows%2011%20x64-0078d4?style=for-the-badge&logo=windows11&logoColor=white" alt="Platform" />
  <img src="https://img.shields.io/badge/Rust-2024%20Edition-black?style=for-the-badge&logo=rust&logoColor=white" alt="Rust" />
  <img src="https://img.shields.io/badge/C%2B%2B-20%20%2F%20Direct3D%2011-00599c?style=for-the-badge&logo=cplusplus&logoColor=white" alt="C++" />
  <img src="https://img.shields.io/badge/Tests-73%20passed-2ea44f?style=for-the-badge" alt="Tests" />
  <img src="https://img.shields.io/badge/Code-17.3k%2B%20LOC-blueviolet?style=for-the-badge" alt="Lines of Code" />
  <img src="https://img.shields.io/badge/License-MIT-blue?style=for-the-badge" alt="License" />
</p>

An independent, lightweight web browser engine built from scratch for Windows 11 x64 in Rust and C++ (Direct3D 11, Direct2D, DirectWrite, Windows Media Foundation). It does not use Chromium, Gecko, WebKit, or WebView.

---

### Codebase Statistics

| Layer / Crate | Purpose | Files | Lines of Code |
|---|---|:---:|:---:|
| **`content`** | HTML5/CSS parser, DOM, Layout Engine, QuickJS runtime, AppContainer worker | 11 | **8,907** |
| **`browser`** | Win32 host, multi-tab strip, navigation history, compositor worker | 13 | **5,267** |
| **`gpu`** | D3D11 compositor, Motion Engine, Media Foundation player, DirectWrite text | 11 | **2,648** |
| **`net`** | HTTP(S) stack, TLS validation, SSRF protection, resource limiters | 1 | **385** |
| **`components`** | Browser Studio configuration model and settings persistence | 1 | **90** |
| **Total** | | **37** | **~17,297 LOC** |

* Language composition: **90.8% Rust**, **9.2% C++ / Direct3D 11 / Media Foundation**.

---

### Key Capabilities

* **Multi-Tab Interface**: Tab strip with adaptive width, tab lifecycle, per-tab history, scroll preservation, and shortcuts (`Ctrl+T`, `Ctrl+W`, `Ctrl+Tab`, `Ctrl+1..9`).
* **HTML5 Audio & Video**: Native playback for `<video>` and `<audio>` powered by Windows Media Foundation (`IMFSourceReader`) with direct D3D11 frame presentation, interactive play/pause controls overlay, and timeline scrubbing.
* **Interactive Forms**: `<form>`, `<input>` (text, search, password, submit, checkbox, radio), `<button>`, and `<textarea>`. Supports typing with Win32 `WM_CHAR`, focus outline, text caret, and query string submission.
* **Modern Web APIs**: JavaScript QuickJS engine with `fetch()` (Promises, `json()`, `text()`), `localStorage`, `sessionStorage`, and `console.log`.
* **Web Typography**: `@font-face` rule parsing, dynamic font resource downloading, and Windows DirectWrite in-memory font registration via `AddFontMemResourceEx`.
* **Security Sandbox**: Untrusted web pages execute inside a Windows AppContainer with strict Job Object memory and process limits.
* **GPU Compositing**: Direct3D 11 flip-model swapchain, DirectWrite text rasterization, and Motion Engine borderless fullscreen transitions.

---

### Building

Requirements: Rust stable, MSVC C++ tools, Windows SDK.

```powershell
cargo build --workspace --release --locked
.\target\release\browser_app.exe
cargo test --workspace --locked
```

Ensure `web_content_process.exe` is in the same directory as `browser_app.exe`.

---

### Navigation & Shortcuts

* **Ctrl+T** / **Ctrl+W**: Open new tab / close active tab.
* **Ctrl+Tab** / **Ctrl+Shift+Tab**: Switch to next / previous tab.
* **Ctrl+1** – **Ctrl+9**: Switch directly to tab by index.
* **Ctrl+L**: Select address bar.
* **Alt+Left** / **Alt+Right**: Navigate back / forward in history.
* **Ctrl+R** or **F5**: Reload active page.
* **Ctrl+O**: Open local HTML file.
* **F2**: Open Browser Studio (theme and UI customization).
* **F11** or **F** (in Motion Engine): Toggle borderless fullscreen.

---

### License

This project is licensed under the MIT License. See [LICENSE](LICENSE) for details.
