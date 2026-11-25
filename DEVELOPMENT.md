# Developing InstaBox

Read this before you change the code, open a pull request, build a preview or cut a release. The README covers setup and running the app.

## Layout

- `src-tauri/` is the Rust core on Tauri 2.
  - `app.rs`: the app state and every action. `commands.rs`: the commands the UI calls.
  - `api/`: the instabox API client. `auth.rs`: device sign-in and token storage.
  - `sandbox/`: create, connect, sync and lifecycle of sandboxes.
  - `ade/`: the ADE registry (`mod.rs`) and one module per ADE (`t3.rs`, `cmux.rs`, `orca.rs`, `paradev.rs`).
  - `ssh_config.rs`: the SSH hosts InstaBox writes for Orca, and the one line it adds to `~/.ssh/config`.
  - `clipboard.rs`: copies a secret the user pastes once (Paradev's SSH address) and clears it a minute later.
  - `agents.rs`: signing Claude Code or Codex in on a sandbox. `tray.rs`: the menu bar popover. `login.rs`: start at login.
  - `updater.rs`: offers a new version of InstaBox and installs it when the user clicks Update (see [In-app updates](#in-app-updates)).
- `src/` is the React UI. `api.ts` wraps the Rust commands and `types.ts` mirrors the Rust state. `mode.ts` keeps the ADE the user picked.

The UI never calls the instabox API itself. Every action on a sandbox, the account or this computer is a Rust command, and is tested in Rust. The UI owns what it shows, such as the picked ADE, and is tested with Vitest.

## Run it

1. Quit any installed InstaBox. Only one instance runs at a time.
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

[How InstaBox works with each ADE](docs/ades.md) describes what each ADE does on the sandbox and on this computer. [Adding an ADE](#adding-an-ade) lists the files to touch. A sandbox's metadata marks which ADEs it has, and the mark must stay compatible with older releases:

- `instabox-ades` lists every ADE id on the sandbox, comma separated, in the order they were added, such as `t3,cmux`.
- `instabox-bridge` holds one id, the key v0.1.x reads and writes. It is `t3` whenever T3 is on the sandbox, so v0.1.x keeps treating the sandbox as connected. Else it is the first id of `instabox-ades`.
- InstaBox reads the ids in `instabox-ades`, then the `instabox-bridge` id when the list lacks it, since v0.1.x writes only that key.
- A sandbox with no mark, such as one made in plain InstaBox, has no ADEs, so each ADE offers Install on it. An action that must pick an ADE and was given none uses T3.
- An id this build does not know is kept when writing and never treated as T3.
- `paradev` joins the mark only once Paradev's host service runs on the sandbox, since the user sets the host up in Paradev.

## Adding an ADE

Pull requests that add an ADE are welcome. Read [How InstaBox works with each ADE](docs/ades.md) first to see how the four current ADEs differ.

- Registry: `src-tauri/src/ade/mod.rs` defines `Ade` and lists every ADE in `ALL`. Add a variant and fill in its match arms.
- ADE modules: `src-tauri/src/ade/t3.rs` finds T3 Code and reads its version, and holds the install, version and pairing scripts. `src-tauri/assets/t3-probe` is the idle probe. `src-tauri/src/ade/cmux.rs` finds cmux, reads its version from its app bundle, and builds its link. `src-tauri/src/ade/orca.rs` finds Orca and reads its version, from its app bundle on macOS and from `Orca.exe` on Windows. `src-tauri/src/ade/paradev.rs` finds Paradev like cmux, and holds its host check, host id and links.
- How it opens (`Open`): `Pair` for an app that pairs with a server on the sandbox (T3), `Link` for an app that reaches the sandbox over SSH from a link (cmux), `SshHost` for an app that imports the sandbox as a host from `~/.ssh/config` (Orca), `Host` for an app that runs the sandbox as one of its hosts, set up in the app (Paradev). `src-tauri/src/ssh_config.rs` writes the Orca hosts.
- Flow: `src-tauri/src/sandbox/connect.rs` sets the ADE up and adds its id to the sandbox's mark. `src-tauri/src/sandbox/sync.rs` keeps the versions in step for an ADE that pins them. `open_in_ade` in `src-tauri/src/app.rs` opens it.
- UI: `src/ades.tsx` holds what the app does with each ADE (its badge, names and text), `src/mode.ts` the ADE the user picked, and `src/ade.ts` with `src/hero/` each ADE's look (logo and background). The switcher is `src/AdeSwitcher.tsx`.
- Docs: add the ADE to the README's Supported ADEs and give it a section in `docs/ades.md`.

A pairing ADE needs an install script, a version check, a pairing step, and an idle probe that prints `{"busy": true}` or `{"busy": false}`. A link ADE needs a link and, if the sandbox needs one, a prep command. An SSH host ADE needs the text that tells the user how to connect the host. A host ADE needs a check that the host runs, the link to its host setup, the text that tells the user what to do there, and a link to the sandbox's host.

An ADE whose app gets an SSH token (in a link, a host or the clipboard) must check in `Ade::verify` that the app is the real one, as cmux, Orca and Paradev do.

## Preview builds

A pull request can get a signed Instabox Preview, which installs next to InstaBox with its own sign-in (identifier `sh.instabox.bridge.preview`).

1. Add the `preview-build` label. The pull request's author must be in `.github/PREVIEW_AUTHORS`.
2. **Preview build** (`.github/workflows/preview.yml`) builds the app from the pull request without secrets.
3. **Preview publish** (`.github/workflows/preview-publish.yml`) runs main's code only. It checks the author and the build, signs and notarizes the macOS app, takes the label off, and puts the files on the `instabox-preview` prerelease. The pull request gets a comment with the links.
4. To build a newer commit, add the label again. Closing the pull request deletes its files.

Preview publish runs from `main`, so a change to it takes effect only after it merges. The Windows installer is not signed yet.

## Releases

Repository admins cut releases.

1. Bump the version in `package.json`, `src-tauri/Cargo.toml`, `src-tauri/Cargo.lock` and `src-tauri/tauri.conf.json`, and merge that through a pull request.
2. Push a `vX.Y.Z` tag on `main`. Only admins may push `v*` tags.
3. **Release** (`.github/workflows/release.yml`) builds the signed and notarized `.dmg` files for Apple silicon and Intel and the Windows installer, checks the signatures, and uploads them to the GitHub release with the updater files.
4. When all three builds are done, it uploads `latest.json`. From then on, the app offers the release.

The signing secrets live in two environments: `release`, which only `v*` tags may use, and `preview`, which only `main` may use. Never add them at the repository level.

### In-app updates

The app reads `https://github.com/InsForge/instabox-bridge/releases/latest/download/latest.json` 15 seconds after launch and every 4 hours. A check never downloads. Only **Update** downloads, checks the signature, installs and restarts. **Later** hides the offer until the next launch.

- **Updater key**: the release build signs the updater files with the secrets `TAURI_SIGNING_PRIVATE_KEY` and `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` in the `release` environment. Without them it fails. The public key is `plugins.updater.pubkey` in `src-tauri/tauri.conf.json`. Keep the private key safe: installed apps accept only updates signed with it.
- **Updater files** on each release, each with a `.sig`: `Instabox_<version>_aarch64.app.tar.gz` and `Instabox_<version>_x64.app.tar.gz` hold the same signed, notarized and stapled app as the `.dmg` files. On Windows the updater runs `Instabox_<version>_x64-setup.exe` without questions (passive mode), then starts the app again.
- **`latest.json`** names the version, the release notes, the date and, for `darwin-aarch64`, `darwin-x86_64` and `windows-x86_64`, the file's URL on this release and the text of its `.sig`. The tag must match the app's version, since each signature names it.
- `latest` is the newest release that is not a prerelease. The `instabox-preview` prerelease must never become latest.
- Instabox Preview never updates: its config builds no updater files, so it needs no key, and the app checks only in a release build with InstaBox's identifier. CI builds the Windows installer with `--no-sign`, as it has no key either. Dev builds never check.
- 0.1.6 and earlier have no updater. Their users update by hand once, to the first release with it.
