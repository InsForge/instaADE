---
name: instabox-development
description: Use when working on the Instabox app codebase itself (InsForge/instabox-bridge), such as changing the Rust core under src-tauri/ or the React UI under src/, adding an ADE, running the app in dev mode, preparing a pull request, building a preview of a pull request, or cutting a release. Covers the code layout, the checks CI runs, the sandbox mark rules, preview builds and the release flow.
license: MIT
metadata:
  organization: InsForge
---

# Instabox Development

See the canonical development guide at [DEVELOPMENT.md](../../../DEVELOPMENT.md) in the repo root. It covers:

1. **Layout**: the Rust core in `src-tauri/` holds all behavior, and the React UI in `src/` calls it only through Rust commands.
2. **Running it**: `bun tauri dev` with hot reload, and the API environment variables.
3. **Before a pull request**: the exact checks CI runs, tests with every fix, keeping `src/types.ts` in step with Rust.
4. **ADEs and the sandbox mark**: the `instabox-ades` and `instabox-bridge` keys and the compatibility rules.
5. **Preview builds**: the `preview-build` label, `.github/PREVIEW_AUTHORS`, and the two stage signing flow.
6. **Releases**: the version bump, the `v*` tag, and where the signing secrets live.

Always read `DEVELOPMENT.md` before editing files under `src-tauri/` or `src/`, changing a workflow, or shipping a release.
