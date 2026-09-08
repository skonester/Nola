# Nola

![Nola logo, retained from Magnolia](src/media/magnolia.png)

Nola is an experimental fork of [Magnolia](https://github.com/chwair/magnolia), a desktop media client. This fork is a place to try new features, explore changes, and gather feedback while keeping it practical to sync improvements from upstream.

Features and behavior may change as experiments develop. Nola keeps Magnolia's original logos and artwork, with credit to the upstream project and its contributors.

## Features

- Video playback powered by mpv
- Extensions for media sources and subtitles
- Subtitle imports for individual files or a full series
- Watch progress tracking and personal lists
- Media discovery and recommendations
- Desktop builds for Windows, macOS, and Linux

## Downloads

Visit [Nola Releases](https://github.com/skonester/Nola/releases) for available builds, prereleases, and release notes.

| Platform | Architecture | Package |
| --- | --- | --- |
| Windows | x64 | Installer (`.exe`) |
| macOS | Apple Silicon / ARM64 | Application ZIP (`.zip`) |
| Linux | x64 | Debian package (`.deb`) |

Experimental builds may be marked as prereleases. Check each release's notes for changes and known issues.

## Building locally

### Prerequisites

- Node.js 22 and npm
- Rust stable
- Platform-specific Tauri build dependencies; see the [Nola build workflow](.github/workflows/nola-release.yml) for each platform's setup

### Setup

Use a disposable checkout for a Nola build. The branding step updates files in that checkout; keep those generated changes out of commits intended for upstream syncing.

```sh
git clone https://github.com/skonester/Nola.git
cd Nola

# Install JavaScript dependencies and runtime libraries
npm ci
npm run setup:libs

# Apply Nola branding
node scripts/prepare_nola.mjs

# Start the development app
npm run tauri:dev
```

To create a production build:

```sh
npm run tauri:build
```

See [Nola builds and releases](NOLA.md) for release publishing, versioning, branding, and upstream-sync instructions.

## Feedback and contributions

Use [GitHub Issues](https://github.com/skonester/Nola/issues) to report bugs or suggest experiments. Include your operating system, Nola version, steps to reproduce, and relevant logs when reporting a problem.

## Acknowledgments

- [Magnolia](https://github.com/chwair/magnolia) and its contributors
- [TMDB](https://www.themoviedb.org/)
- [Soia](https://github.com/FengZeng/soia)
- [rqbit](https://github.com/ikatson/rqbit)
- [mpv](https://github.com/mpv-player/mpv)
- [Tauri](https://tauri.app/)

## License

Nola retains the upstream license files and attribution. See [LICENSE-MIT](LICENSE-MIT) and [LICENSE-GPL](LICENSE-GPL) for the license terms.
