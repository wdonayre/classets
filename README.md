# CLASSETS

Your files, on every device you trust.

CLASSETS is a desktop app for **Windows, macOS and Linux** that syncs your files to your own
Cloudflare R2 bucket, or straight to your other devices. A separate web app decides who can
see which folder and which devices may hold it; this repo is the desktop app and its sync
engine.

> Status: early scaffold. Nothing syncs yet.

## Stack

- **Tauri 2** desktop shell (`src-tauri/`), React 19 + TypeScript + Vite UI (`src/`)
- **Rust** sync engine (`crates/core`, crate `classets-core`)
- Supported: Windows 10+, macOS 11+, Ubuntu 22.04+ (WebKitGTK 4.1)

## Layout

```
src/            React UI
src-tauri/      Tauri shell (commands, windows, tray, updater)
crates/core/    sync engine library
.githooks/      commit guards (see Contributing)
```

## Develop

Needs Rust (pinned in `rust-toolchain.toml`), Node 22 and pnpm. On Linux also
`libwebkit2gtk-4.1-dev`, `libgtk-3-dev`, `librsvg2-dev` and `build-essential`.

```bash
pnpm install
pnpm tauri dev          # run the app
cargo test --workspace  # engine tests
pnpm tauri build        # installers for the current OS
```

## Security model (short)

- Devices never hold the R2 keys. For each upload or download the server hands out a
  one-file link that expires in minutes.
- Each device has its own key pair and must be approved on the web before it gets files.
- Direct device-to-device transfers are encrypted end to end with those device keys.

## Contributing

Run `git config core.hooksPath .githooks` after cloning. The hooks keep commits limited to
the maintainer's account and refuse co-author or generated-by trailers.
