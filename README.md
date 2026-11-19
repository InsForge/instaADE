# Instabox Bridge

Instabox Bridge connects instabox cloud sandboxes to the T3 Code desktop app. You create an instabox sandbox or pick one you have. The app puts T3 on it at your T3 Code's version and pairs it for you. You then manage the sandbox from the app window and the menu bar.

## Requirements

- macOS
- T3 Code desktop app
- An instabox account

## Download

Get the latest `.dmg` from [GitHub Releases](../../releases). Pick the aarch64 (Apple silicon) file or the x64 (Intel) file.

## First run on an unsigned build

The builds are not signed yet. macOS blocks the first launch.

1. Open the app once. macOS shows a warning and does not start it.
2. Open System Settings → Privacy & Security.
3. Scroll to the message about Instabox Bridge and click Open Anyway.

Right-click → Open no longer works on macOS 15.

## What it does on your sandbox

- Installs T3 under `/opt/t3`.
- Adds a `t3-instabox` service.
- Adds an activity probe, so the sandbox sleeps only when T3 is idle.

## Known limits

- A sandbox with T3 Code connected does not auto-sleep.
- cmux, Superset and Orca come later.
- Windows comes later.

## Development

```sh
bun install
bun tauri dev
bun run test
cd src-tauri && cargo test
```

Set `INSTABOX_API_KEY` to use a dev key. Set `INSTABOX_API_URL` to point the app at another API.

## Credits

Parts are adapted from t3boat by Pau Kraft (MIT). See [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).

## License

MIT
