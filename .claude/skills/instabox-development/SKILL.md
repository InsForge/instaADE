---
name: instabox-development
description: Use when working on the InstaADE app codebase itself (InsForge/instaADE, formerly Instabox and InsForge/instabox-bridge), such as changing the Rust core under src-tauri/ or the React UI under src/, adding an ADE, running the app in dev mode, preparing a pull request, building a preview of a pull request, or cutting a release. Covers the code layout, the checks CI runs, the sandbox mark rules, adding an ADE, preview builds and the release flow.
license: MIT
metadata:
  organization: InsForge
---

# InstaADE Development

See the canonical development guide at [DEVELOPMENT.md](../../../DEVELOPMENT.md) in the repo root. It covers:

1. **Layout**: every action on a sandbox, the account or this computer is a Rust command in `src-tauri/`, and the React UI in `src/` reaches the API only through those commands.
2. **Running it**: `bun tauri dev` with hot reload, and the API environment variables.
3. **Before a pull request**: the local checks, which cover CI's `check` job, tests with every fix, keeping `src/types.ts` in step with Rust.
4. **ADEs and the sandbox mark**: the `instabox-ades` and `instabox-bridge` keys and the compatibility rules.
5. **Adding an ADE**: the registry, the ADE modules, the four ways an ADE opens, the flow and UI files, and what each kind of ADE needs.
6. **Preview builds**: the `preview-build` label, `.github/PREVIEW_AUTHORS`, and the two stage signing flow.
7. **Releases**: the version bump, the `v*` tag, where the signing secrets live, and in-app updates (the updater key, the updater files and `latest.json`).

What each ADE does on the sandbox and on the user's computer is in [docs/ades.md](../../../docs/ades.md).

Always read `DEVELOPMENT.md` before editing files under `src-tauri/` or `src/`, changing a workflow, or shipping a release.
