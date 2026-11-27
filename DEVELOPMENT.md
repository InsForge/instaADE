# Developing InstaADE

Read this before you change the code, open a pull request, build a preview or cut a release. The README covers setup and running the app.

## Layout

- `src-tauri/` is the Rust core on Tauri 2.
  - `app.rs`: the app state and every action. `commands.rs`: the commands the UI calls.
  - `api/`: the instabox API client. `auth.rs`: device sign-in and token storage.
  - `sandbox/`: create, connect, sync and lifecycle of sandboxes.
  - `ade/`: the ADE registry (`mod.rs`) and one module per ADE (`t3.rs`, `cmux.rs`, `orca.rs`, `paradev.rs`, `paseo.rs`, `herdr.rs`).
  - `ssh.rs`: SSH tokens and the login commands, and the terminal that Open in Terminal and Herdr open. `ssh_config.rs`: the SSH hosts InstaADE writes for Orca and Paseo, and the one line it adds to `~/.ssh/config`.
  - `clipboard.rs`: copies a secret the user pastes once (Paradev's SSH address) and clears it a minute later, and copies Paseo's host address, which holds no secret and stays.
  - `old_app.rs`: finds a copy of the app from before it was InstaADE, and moves it to the Trash (see [The old app](#the-old-app)).
  - `agents.rs`: signing Claude Code or Codex in on a sandbox. `tray.rs`: the menu bar popover. `login.rs`: start at login.
  - `updater.rs`: checks for a new version of InstaADE and installs it when the user clicks Update in Settings (see [In-app updates](#in-app-updates)).
- `src/` is the React UI. `api.ts` wraps the Rust commands and `types.ts` mirrors the Rust state. `mode.ts` keeps the ADE the user picked.

The UI never calls the instabox API itself. Every action on a sandbox, the account or this computer is a Rust command, and is tested in Rust. The UI owns what it shows, such as the picked ADE, and is tested with Vitest.

## Run it

1. Quit any installed InstaADE, or Instabox (its old name). Only one instance runs at a time.
2. `bun install`, then `bun tauri dev`.
3. UI changes reload at once. Rust changes rebuild and restart the app.

- On a Mac with a notch, a full menu bar hides the icon behind the notch. Make room in the menu bar, or use the popover that opens on launch.
- `INSTABOX_API_URL` points the app at another API. `INSTABOX_API_KEY` signs in with an API key instead of the browser.

## Before a pull request

Run these locally. They cover the `check` job in CI, with clippy also over the tests. CI builds the Windows installer as well.

```bash
bun install --frozen-lockfile
bun run typecheck
bun run test
bun run build
cd src-tauri
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

- Add a test with every fix, in Rust for an action and in Vitest for the UI.
- Keep `src/types.ts` in step with the Rust state. Rust serializes it in camelCase.
- `main` takes changes only through pull requests. One approval and the `check` job are required.

## ADEs and the sandbox mark

[How InstaADE works with each ADE](docs/ades.md) describes what each ADE does on the sandbox and on this computer. [Adding an ADE](#adding-an-ade) lists the files to touch. A sandbox's metadata marks which ADEs it has, and the mark must stay compatible with older releases:

- `instabox-ades` lists every ADE id on the sandbox, comma separated, in the order they were added, such as `t3,cmux`.
- `instabox-bridge` holds one id, the key v0.1.x reads and writes. It is `t3` whenever T3 is on the sandbox, so v0.1.x keeps treating the sandbox as connected. Else it is the first id of `instabox-ades`.
- InstaADE reads the ids in `instabox-ades`, then the `instabox-bridge` id when the list lacks it, since v0.1.x writes only that key.
- A sandbox with no mark, such as one made in plain InstaBox, has no ADEs, so each ADE offers Install on it. An action that must pick an ADE and was given none uses T3.
- An id this build does not know is kept when writing and never treated as T3.
- `paradev` joins the mark only once Paradev's host service runs on the sandbox, since the user sets the host up in Paradev.

## Adding an ADE

Pull requests that add an ADE are welcome. Read [How InstaADE works with each ADE](docs/ades.md) first to see how the current ADEs differ.

- Registry: `src-tauri/src/ade/mod.rs` defines `Ade` and lists every ADE in `ALL`. Add a variant and fill in its match arms.
- ADE modules: `src-tauri/src/ade/t3.rs` finds T3 Code and reads its version, and holds the install, version and pairing scripts. `src-tauri/assets/t3-probe` is the idle probe. `src-tauri/src/ade/cmux.rs` finds cmux, reads its version from its app bundle, and builds its link. `src-tauri/src/ade/orca.rs` finds Orca and reads its version, from its app bundle on macOS and from `Orca.exe` on Windows. `src-tauri/src/ade/paradev.rs` finds Paradev like cmux, as `Paradev.app` or `Superset.app`, and holds its host check, host id and links. `src-tauri/src/ade/paseo.rs` finds Paseo like Orca (it uses Orca's reader for `.exe` files), and holds its daemon setup, host address and notice. `src-tauri/assets/paseo-probe` is its idle probe. `src-tauri/src/ade/herdr.rs` finds the OpenSSH client and reads its version, and holds Herdr's install script and version check. When an app can be installed more than once, its module picks the newest real copy with `ade::newest_app`, so the version shown is the copy that opens.
- How it opens (`Open`): `Pair` for an app that pairs with a server on the sandbox (T3), `Link` for an app that reaches the sandbox over SSH from a link (cmux), `SshHost` for an app that reaches the sandbox through a host in `~/.ssh/config`, which it imports (Orca) or the user adds by its address (Paseo), `Host` for an app that runs the sandbox as one of its hosts, set up in the app (Paradev), `Terminal` for an ADE that runs on the sandbox and opens in the user's terminal over SSH (Herdr). `src-tauri/src/ssh_config.rs` writes the hosts of Orca and Paseo, and `ssh::open_in_terminal` opens the terminal.
- Flow: `src-tauri/src/sandbox/connect.rs` sets the ADE up and adds its id to the sandbox's mark. `src-tauri/src/sandbox/sync.rs` keeps the versions in step for an ADE that pins them. `open_in_ade` in `src-tauri/src/app.rs` opens it.
- UI: `src/ades.tsx` holds what the app does with each ADE (its badge, names and text), `src/mode.ts` the ADE the user picked, and `src/ade.ts` with `src/hero/` each ADE's look (logo and background). The switcher is `src/AdeSwitcher.tsx`.
- Docs: add the ADE to the README's Supported ADEs and give it a section in `docs/ades.md`.

A pairing ADE needs an install script, a version check, a pairing step, and an idle probe that prints `{"busy": true}` or `{"busy": false}`. A link ADE needs a link and, if the sandbox needs one, a prep command. An SSH host ADE needs the text that tells the user how to connect the host. If the user adds the host by hand, it also needs the address to copy, and if its app needs a server on the sandbox, a setup that runs it and an idle probe for the agents it runs outside a terminal (Paseo's daemon). A host ADE needs a check that the host runs, the link to its host setup, the text that tells the user what to do there, and a link to the sandbox's host. A terminal ADE needs an install script that prints its version, a version check, and the command the SSH session runs. An ADE with a probe whose Open runs nothing on the sandbox can return it from `Ade::probe_on_open` (Paseo does): Open then installs or updates the probe in the background, once per sandbox and probe hash while the app runs (`Core::refresh_probe`).

An ADE whose app gets an SSH token (in a link, a host or the clipboard) must check in `Ade::verify` that the app is the real one, as cmux, Orca, Paradev and Paseo do. A terminal ADE gives the token to the OpenSSH client only, on its command line, as Open in Terminal does, so it has no app to check. It runs the client `locate` found, by its path, and builds the arguments with `ssh::terminal_args`, which refuses a host or token with anything a shell could run. `ssh::open_in_terminal` checks every argument again before any of it reaches a terminal.

## The old app

InstaADE was called Instabox until v0.1.6. The bundle identifier `sh.instabox.bridge`, the executable `instabox-bridge`, the sandbox mark and the keychain entries keep that name, so an update keeps the sign-in and the sandboxes. On macOS, installing InstaADE from its `.dmg` leaves `Instabox.app` next to it.

- At launch, a release build with that identifier looks for `Instabox.app` in `/Applications` and `~/Applications`. It counts only a folder, not a link, whose `Info.plist` has the identifier and that the app does not run from. The popover then offers **Move to Trash** and **Not now**. Not now lasts until the next launch.
- **Move to Trash** checks each copy again and moves nothing while one runs. When the app starts at login, it turns the login item off before the move, while the old copy can still be recognized, and on again after it, so the item names InstaADE and not the copy in the Trash.
- The LaunchAgent that v0.1.5 and earlier wrote keeps its label, `Instabox` (`login::agent_label`), so it still moves to SMAppService on macOS 13 and later, and stays the login item before macOS 13.
- Only one copy runs at a time, since both have the same identifier, and the single-instance plugin hands a launch to the copy that runs. So a launch first asks a running `Instabox.app` to quit the normal way (`old_app::quit_running`, before the plugins in `lib.rs`) and waits up to 5 seconds. When it still runs, an alert tells the user to quit it, and the launch goes to it as before. Another launch of InstaADE itself is never asked to quit.
- On Windows, the installer would leave the Instabox install next to InstaADE, since it finds an install by the product name. So in the release build, the installer hook `src-tauri/windows/hooks.nsh` first runs Instabox's uninstaller, silently. It does so only when the `Instabox` uninstall key has this app's publisher (`instabox`) and executable. Silent, the uninstaller closes a running Instabox and never deletes the app data. When Instabox started at login (its `Instabox` Run entry, not turned off in Task Manager), InstaADE gets its own `InstaADE` Run entry. Without an Instabox install, the hook does nothing.

## Preview builds

A pull request can get a signed InstaADE Preview, which installs next to InstaADE with its own sign-in (identifier `sh.instabox.bridge.preview`).

1. Add the `preview-build` label. The pull request's author must be in `.github/PREVIEW_AUTHORS`.
2. **Preview build** (`.github/workflows/preview.yml`) builds the app from the pull request without secrets.
3. **Preview publish** (`.github/workflows/preview-publish.yml`) runs main's code only. It checks the author and the build, signs and notarizes the macOS app, takes the label off, and puts the files on the `instabox-preview` prerelease. The pull request gets a comment with the links.
4. To build a newer commit, add the label again. Closing the pull request deletes its files.

Preview publish runs from `main`, so a change to it takes effect only after it merges. The Windows installer is not signed yet.

## Releases

Repository admins cut releases.

1. Bump the version in `package.json`, `src-tauri/Cargo.toml`, `src-tauri/Cargo.lock` and `src-tauri/tauri.conf.json`, and merge that through a pull request.
2. Push a `vX.Y.Z` tag on `main`. Only admins may push `v*` tags.
3. **Release** (`.github/workflows/release.yml`) makes a draft GitHub release. It builds the signed and notarized `.dmg` files for Apple silicon and Intel and the Windows installer, checks the signatures, and uploads them to the draft with the updater files.
4. When all three builds are done, it uploads `latest.json` and publishes the release. From then on, the app offers the release. When a build fails, the release stays a draft and the app keeps offering the release before. Re-run the failed jobs to finish it.

The signing secrets live in two environments: `release`, which only `v*` tags may use, and `preview`, which only `main` may use. Never add them at the repository level.

### In-app updates

The app reads `https://github.com/InsForge/instaADE/releases/latest/download/latest.json` 15 seconds after launch, every 4 hours, and when the user clicks **Check for updates** in Settings. A check never downloads, and only one runs at a time. A check fails after 30 seconds and a download after 15 minutes, and Settings then offers to try again. A newer version puts a dot on the Settings button and **Update** in Settings, under Updates. Signed out, the app has no Settings, so the sign-in page shows the version on offer and **Update** under the sign-in buttons, and an app whose sign-in fails can still update. Only **Update** downloads, checks the signature, installs and restarts.

- **Updater key**: the release build signs the updater files with the secrets `TAURI_SIGNING_PRIVATE_KEY` and `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` in the `release` environment. Without them it fails. The public key is `plugins.updater.pubkey` in `src-tauri/tauri.conf.json`. Keep the private key safe: installed apps accept only updates signed with it.
- **Updater files** on each release: `InstaADE_<version>_aarch64.app.tar.gz` and `InstaADE_<version>_x64.app.tar.gz` hold the same signed, notarized and stapled app as the `.dmg` files. On Windows the updater runs `InstaADE_<version>_x64-setup.exe` without questions (passive mode), then starts the app again. The build signs each of them (`.sig`), and the signature's text goes into `latest.json`. The `.sig` files are not uploaded, since the app reads only `latest.json`.
- **Release notes** start with which file to download for each computer, then GitHub's generated list of changes.
- **`latest.json`** names the version, the release notes, the date and, for `darwin-aarch64`, `darwin-x86_64` and `windows-x86_64`, the file's URL on this release and the text of its `.sig`. The tag must match the app's version, since each signature names it. The app installs an update only when its signature names the version that `latest.json` announces (`requireSignedVersion`), and the release checks each signature before it uploads `latest.json`.
- `latest` is the newest release that is not a prerelease. The `instabox-preview` prerelease must never become latest.
- InstaADE Preview never updates: its config builds no updater files, so it needs no key, and the app checks only in a release build with InstaADE's identifier. CI builds the Windows installer with `--no-sign`, as it has no key either. Dev builds never check. In both, Settings shows the version and says updates are only for released builds.
- 0.1.6 and earlier have no updater. Their users update by hand once, to the first release with it.
