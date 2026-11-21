<div align="center">
  <a href="https://instabox.sh">
    <img src="src-tauri/icons/128x128@2x.png" alt="Instabox" width="96">
  </a>

  <h1>Instabox</h1>

  <p>Run your ADE (agentic development environment) on an instabox sandbox, from your menu bar or system tray.</p>

  <p>
    <a href="LICENSE"><img src="https://img.shields.io/badge/License-MIT-yellow.svg" alt="License: MIT"></a>
    <a href="https://github.com/InsForge/instabox-bridge/releases/latest"><img src="https://img.shields.io/github/v/release/InsForge/instabox-bridge" alt="Latest release"></a>
    <img src="https://img.shields.io/badge/platform-macOS%20%7C%20Windows-lightgrey.svg" alt="Platform: macOS | Windows">
  </p>
</div>

<p align="center">
  <img src="docs/images/list.png" alt="Sandbox list" width="250">
  <img src="docs/images/menu.png" alt="Sandbox actions" width="250">
  <img src="docs/images/details.png" alt="Sandbox details" width="250">
</p>

## Getting Started

You need macOS or Windows (x64) and an [instabox](https://instabox.sh) account. To open sandboxes in an ADE, you also need a supported one: the [T3 Code](https://github.com/pingdotgg/t3code/releases/latest) desktop app, or [cmux](https://github.com/manaflow-ai/cmux/releases/latest) on macOS.

1. Download the latest release from [Releases](https://github.com/InsForge/instabox-bridge/releases/latest):
   - macOS: the `.dmg`, `aarch64` for Apple silicon, `x64` for Intel.
   - Windows: the `x64-setup.exe` installer.
2. Install it:
   - macOS: drag Instabox to Applications and open it. It is signed and notarized, so it opens normally.
   - Windows: run the installer. It is not signed yet, so Windows SmartScreen asks once: click **More info**, then **Run anyway**.
3. Click the Instabox icon in the menu bar (macOS) or the system tray (Windows; at first it may be under the **^** arrow).
4. Sign in with GitHub or Google.
5. To open sandboxes in an ADE, pick it in the menu at the top (**Instabox** keeps to sandboxes only). If you used T3 Code with Instabox before, it starts in T3 Code.
6. Click **New sandbox**, pick a size, then click **Create sandbox**.
7. Confirm the new environment in T3 Code, or the workspace in cmux.

## Features

- Create a sandbox or connect one you already have
- Pick an ADE at the top: each sandbox gets **Open in** it, or **Install** it when the sandbox does not have it yet
- Set up your ADE on the sandbox and pair it with the app on your computer
- Pause, resume, resize and set auto sleep
- Copy the SSH command or open it in a terminal
- Sign in to Claude Code or Codex on the sandbox
- Starts at login (turn it off in Settings)

## Supported ADEs

### T3 Code

Instabox installs T3 at the same version as your T3 Code desktop app, then pairs them. On the sandbox it adds:

- T3 under `/opt/t3`
- A `t3-instabox` service that runs the T3 server
- An activity probe, so the sandbox sleeps only when T3 is idle

### cmux (macOS)

cmux connects over SSH by itself and uploads its own remote part, at the version of the cmux app, so Instabox installs nothing and no versions must match. On the sandbox it runs one command, `mkdir -p ~/.local/state && chmod go-w ~/.local ~/.local/state`, because cmux refuses a group-writable state directory. **Open in cmux** opens a `cmux://ssh` link with an SSH token of the sandbox. The link holds the token, so Instabox opens it only in the cmux app it found in Applications, never in another handler of `cmux://` links and never through cmux.com. Without that app, nothing opens. cmux has no idle probe yet, so the sandbox sleeps by its terminals and CPU.

A sandbox can have several ADEs. Its metadata marks them with two keys:

- `instabox-ades` lists every ADE id, comma separated, in the order they were added, such as `t3,cmux`.
- `instabox-bridge` holds one id, the key v0.1.x reads and writes. It is `t3` whenever T3 is on the sandbox, so v0.1.x keeps treating it as connected. Else it is the first id of the list.

Instabox reads the ids in `instabox-ades`, then the `instabox-bridge` id when the list lacks it. A sandbox with neither key counts as T3.

## Roadmap

- More ADEs: Orca, Superset and others

## Adding an ADE

- Registry: `src-tauri/src/ade/mod.rs` defines `Ade` and lists every ADE in `ALL`. Add a variant and fill in its match arms.
- ADE modules: `src-tauri/src/ade/t3.rs` finds T3 Code and reads its version, and holds the install, version and pairing scripts. `src-tauri/assets/t3-probe` is the idle probe. `src-tauri/src/ade/cmux.rs` finds cmux, reads its version from its app bundle, and builds its link.
- How it opens (`Open`): `Pair` for an app that pairs with a server on the sandbox (T3), `Link` for an app that reaches the sandbox over SSH from a link (cmux).
- Flow: `src-tauri/src/sandbox/connect.rs` sets the ADE up and adds its id to the sandbox's mark. `src-tauri/src/sandbox/sync.rs` keeps the versions in step for an ADE that pins them. `open_in_ade` in `src-tauri/src/app.rs` opens it.
- UI: `src/ades.tsx` holds each ADE's colors, badge and text, and `src/mode.ts` the ADE the user picked. The switcher is `src/AdeSwitcher.tsx`.
- A pairing ADE needs an install script, a version check, a pairing step, and an idle probe that prints `{"busy": true}` or `{"busy": false}`. A link ADE needs a link and, if the sandbox needs one, a prep command.

Pull requests that add an ADE are welcome.

## Development

### Prerequisites

- macOS or Windows
- [Bun](https://bun.sh)
- Rust via [rustup](https://rustup.rs)
- macOS: Xcode Command Line Tools (`xcode-select --install`)
- Windows: Microsoft C++ Build Tools

### Setup

```bash
bun install
```

### Run with hot reload

1. Quit any installed copy of Instabox. Only one instance runs at a time.
2. Run `bun tauri dev`.
3. Click the Instabox icon in the menu bar or system tray.
4. Edit the code. UI changes reload at once, and Rust changes rebuild the app.

**Tips:**

- Set `INSTABOX_API_URL` to point the app at another API.
- Set `INSTABOX_API_KEY` to sign in with an API key instead of the browser.
- You do not need an installer to see a change.
- A dev build never turns on Start at login by itself.

### Tests

```bash
bun run test
cd src-tauri && cargo test
```

### Build an installer

Only needed to test installing.

```bash
# macOS: src-tauri/target/release/bundle/dmg/
bun tauri build --bundles dmg

# Windows: src-tauri/target/release/bundle/nsis/
bun tauri build --bundles nsis
```

## Contributing

**Pull requests**: Welcome. To add an ADE, start with [Adding an ADE](#adding-an-ade).

**Issues**: Open an [issue](https://github.com/InsForge/instabox-bridge/issues) for bugs or ideas.

## License

This project is licensed under the MIT License. See the [LICENSE](LICENSE) file for details.
