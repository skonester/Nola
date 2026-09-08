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

`nola.json` defines the product name, executable name, stable app identifier,
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
package version and `skonester/Nola`; override these with `NOLA_VERSION` and
`NOLA_REPOSITORY` environment variables. The script intentionally edits tracked
files in that checkout: do not commit the generated changes. Ordinary upstream
build commands without preparation still build Magnolia.

The source archives attached automatically by GitHub also contain upstream
branding; run the preparation script with the release's `NOLA_VERSION` before
building them to reproduce Nola branding and versioning.

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
