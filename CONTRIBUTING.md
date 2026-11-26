# Contributing to InstaADE

Thanks for your help. Most of InstaADE connects a sandbox to an ADE, so a new ADE or a new feature is often a small pull request.

## Ways to help

- **Add your favorite ADE.** If you use an ADE that InstaADE does not support yet, add it. See [Add an ADE](#add-an-ade).
- **Add a feature.** For example, a new sandbox action, a setting or a better flow in the popover.
- **Fix a bug.** Add a test that fails without the fix.
- **Report a bug or ask for an ADE.** Open an [issue](https://github.com/InsForge/instaADE/issues). For a bug, give your OS, the InstaADE version (in Settings), the ADE and its version, and the steps.

For a large change, open an issue first, so we can agree on the approach before you write the code.

## Set up

1. Install the [prerequisites](README.md#prerequisites): Bun, Rust, and the Xcode Command Line Tools on macOS or the Microsoft C++ Build Tools on Windows.
2. Run `bun install`, then `bun tauri dev`. To sign in, you need an [instabox](https://instabox.sh) account.
3. For UI work only, run `bun run dev` and open `http://localhost:1420`. It shows the popover with sample data.

[DEVELOPMENT.md](DEVELOPMENT.md) explains the code layout, the ADEs and the release flow. Coding agents load the same guide through the `instabox-development` skill.

## Add an ADE

An ADE fits InstaADE when it has a desktop app on macOS or Windows, or runs in a terminal, and can work on a remote Linux machine. InstaADE opens a sandbox in an ADE in one of five ways:

| Way | What the ADE does | Example |
| --- | --- | --- |
| Pair | Pairs with a server that InstaADE installs on the sandbox | T3 Code |
| Link | Opens a link from InstaADE and connects to the sandbox over SSH | cmux |
| SSH host | Imports the sandbox as a host from `~/.ssh/config` | Orca |
| Host | Runs the sandbox as one of its hosts, set up in the ADE | Paradev |
| Terminal | Runs on the sandbox, and InstaADE opens it in your terminal over SSH | Herdr |

1. Read [How InstaADE works with each ADE](docs/ades.md) to see how the ADEs differ.
2. Follow [Adding an ADE](DEVELOPMENT.md#adding-an-ade) for the files to change.
3. If the ADE gets an SSH token, check in `Ade::verify` that its app is the real one.
4. Add the ADE to the README's [Supported ADEs](README.md#supported-ades) and give it a section in `docs/ades.md`.
5. In the pull request, show the ADE working from **Install** to **Open**, in a short video or screenshots.

If the ADE cannot open a remote machine in any of these ways, open an issue. It may need a change in the ADE first.

## Pull requests

- Keep each pull request to one change.
- Run the checks in [Before a pull request](DEVELOPMENT.md#before-a-pull-request). CI runs them on macOS and also builds the Windows installer.
- Add a test with every fix: in Rust for an action, in Vitest for the UI.
- For a UI change, add a screenshot.
- In the description, say what changed and how you checked it.
- A maintainer reviews each pull request. `main` takes a change after one approval and a passing `check` job.

## License

By contributing, you agree that your contributions are licensed under the [MIT License](LICENSE).
