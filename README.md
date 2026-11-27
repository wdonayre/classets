# CLASSETS

Your files, on every device you trust.

CLASSETS is a desktop app for **Windows, macOS and Linux** that syncs your files to your own
Cloudflare R2 bucket, or straight to your other devices. A separate web app decides who can
see which folder and which devices may hold it; this repo is the desktop app and its sync
engine.

> Status: working first version. Desktop sync (upload, download, edits, deletes, conflict
> copies, revoke with wipe) works end to end against `classets-web`. The same app runs on
> iOS and Android as a companion: browse, open, upload from the phone, approve devices.
> Not built yet: direct device-to-device sync, "on demand" placeholders, signed installers.

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

### Try it against a local web app

Run `classets-web` locally (`php artisan serve`), create an account, then start the app,
enter `http://127.0.0.1:8000` as the server, and approve the code it shows at `/approve`.
On macOS and Windows the app keeps its device key in the system keychain. An unsigned local
build counts as a new app after every rebuild, so the keychain asks again each time; for local
testing, start it with the keys in a file instead:

```bash
open --env CLASSETS_SECRET_STORE=file target/debug/bundle/macos/CLASSETS.app
```

Without the GUI, the dev CLI does the same:

```bash
cargo run -p classets-core --example classets_dev -- pair --server http://127.0.0.1:8000 --root ~/CLASSETS-test --data ~/.classets-test --name TEST-PC
cargo run -p classets-core --example classets_dev -- sync --data ~/.classets-test
```

### Mobile (iOS and Android)

The phone app is the same Tauri project (`src-tauri/gen/apple`, `src-tauri/gen/android`).
Phones never keep folders in sync; they browse, open, upload and approve.

```bash
pnpm tauri ios dev                      # iOS Simulator (needs Xcode + CocoaPods)
pnpm tauri ios build --debug --target aarch64-sim
pnpm tauri android dev                  # needs ANDROID_HOME and NDK_HOME
```

## Security model (short)

- Devices never hold the R2 keys. For each upload or download the server hands out a
  one-file link that expires in minutes.
- Each device has its own key pair and must be approved on the web before it gets files.
- Direct device-to-device transfers are encrypted end to end with those device keys.

## Contributing

Run `git config core.hooksPath .githooks` after cloning. The hooks keep commits limited to
the maintainer's account and refuse co-author or generated-by trailers.
