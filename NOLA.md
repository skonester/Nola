# Nola builds and releases

Nola is a fork of [Magnolia](https://github.com/chwair/magnolia). The original
logos, icons, installer artwork, licenses, extension credits, and upstream
source files are retained. Fork branding is applied only to the build checkout,
so it does not create recurring rename/version conflicts when syncing upstream.

## Publish a release

1. Push the Nola files to the fork's default branch and enable GitHub Actions
   for the fork if needed.
2. Open **Actions > Nola Release > Run workflow** and select the branch to build.
3. Enter an unused numeric version such as `2.3.1`. Leave **Save as a draft**
   unchecked to publish, or check it to review the release first. **Mark as a
   prerelease** excludes the release from the normal in-app update channel.
   For a beta release, enter a numeric version such as `2.3.1` and check
   **Mark as a prerelease**; do not enter `beta` in the version field.
4. After all three builds succeed, the workflow uploads these assets to this
   repository's GitHub Releases under `nola-v2.3.1`:

| Platform | Architecture | Asset |
| --- | --- | --- |
| Windows | x64 | Nola NSIS installer (`.exe`) |
| macOS | Apple Silicon / ARM64 | `Nola_2.3.1_arm64-macos.zip` containing `Nola.app` |
| Linux | x64 | Nola Debian package (`.deb`) |

This matches upstream's current platform coverage; Intel macOS, Windows ARM64,
and additional Linux package formats are not included. Native runtime-library
setup and packaging follow `.github/workflows/release.yml`. Windows and macOS
builds are unsigned, as in upstream; signing/notarization is not configured.
The workflow uses the automatic `GITHUB_TOKEN`; no personal token is needed.

The release tag points to the selected source commit. Versions are updated in
all manifests and lockfiles inside the runners, with no version-bump commits or
branch pushes. Existing Nola tags are rejected to avoid replacing a release
with binaries from a different commit. If publishing fails after creating a
release, rerun only the failed release job to finish uploading its assets.

Use **Nola Release** for this fork. The original **Release** workflow is retained
for upstream syncing and still builds Magnolia when run.

## Branding and local builds

`nola.json` defines the Nola version, product name, executable name, stable app identifier,
and default repository. `scripts/prepare_nola.mjs` applies these settings to the
Tauri configuration, visible About/onboarding text, HTML title, app-data font
path, and updater. CI sets the update repository to the current GitHub fork.
Nola uses its own `com.skonester.nola` data directory; existing Magnolia settings
are not migrated automatically. Keep this identifier stable across Nola releases.

To check compatibility with the current upstream source without editing it:

```sh
node scripts/prepare_nola.mjs --check
node --test scripts/prepare_nola.test.mjs
```

For a local Nola build, use a disposable checkout, run
`node scripts/prepare_nola.mjs`, then follow upstream's dependency/runtime setup
and `npm run tauri:build` instructions. Preparation defaults to the current
Nola version in `nola.json` and `skonester/Nola`; override these with `NOLA_VERSION` and
`NOLA_REPOSITORY` environment variables. The script intentionally edits tracked
files in that checkout: do not commit the generated changes. Ordinary upstream
build commands without preparation still build Magnolia.

The source archives attached automatically by GitHub also contain upstream
branding; run the preparation script with the release's `NOLA_VERSION` before
building them to reproduce Nola branding and versioning.

### Bundled VLC for Windows Live TV

Windows builds also carry VideoLAN's official VLC 3.0.24 x64 runtime. The
version, archive URL, SHA-256 digest, and corresponding source archive are
pinned in `scripts/vlc_runtime.mjs`. `npm run setup:libs` and the Windows
`run_tauri.mjs` build/dev wrapper install it automatically at build time.
`npm run setup:vlc` installs only VLC; add `-- --check` to verify it.
For an offline build, set `VLC_ARCHIVE_PATH` to the exact pinned official ZIP.

The complete DLL/plugin hierarchy, license notices, and a provenance manifest
are staged under `src-tauri/libs/vlc` (ignored by Git), copied to `target/<profile>/vlc`
for development, and embedded at `vlc/` in the Windows installation using
`tauri.vlc.windows.json`. Plugins must retain their subdirectories. Each DLL
is checked against the extracted inventory and x64 PE architecture before
bundling. Missing runtimes fail the Windows build; the installed app never
searches PATH, prompts users for DLLs, or downloads a runtime at launch.

The VLC player currently targets Windows x64. Other platform builds retain
mpv, and the VLC files are not included in their packages.

## Syncing Magnolia

Merge upstream into the fork normally. Nola's tracked additions are isolated in
`nola.json`, `NOLA.md`, `scripts/prepare_nola*.mjs`, and
`.github/workflows/nola-release.yml`. After syncing, run the checks above and
review any upstream changes to `.github/workflows/release.yml` for dependency,
runtime-library, or packaging changes that also belong in the Nola workflow.

The preparation script fails before writing if a required source-text anchor
has moved or changed. Update the affected anchor after reviewing the upstream
change; do not apply a repository-wide Magnolia-to-Nola replacement, which would
also change credits, asset paths, and service URLs.
