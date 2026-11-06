# Changelog

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [Unreleased]

### Added

- Demo GIF in the README, contributing guide, issue and PR templates, this changelog.

### Changed

- README rewritten. Speed section now has measurements that can be repeated.

## [0.2.0] - 2026-10-31

### Added

- Windows version: wheel as an OLE drop target, mouse hook for drags, status pop-up, trim and resize dialogs.
- One running copy on Windows, and Send To with click mode.
- Windows installer (Inno Setup) in CI and releases, with Ghostscript inside and the other converters through winget.
- `shiftdrop-cli.exe`, the command line on Windows.
- Windows manifest: per-monitor DPI and current dialog controls.
- Clippy and tests on Linux and Windows in CI.

### Changed

- Wheel logic, wheel geometry and the command line moved into the library, shared by both front ends.
- Platform differences moved to `sys.rs`.

### Fixed

- Windows: rebuild PATH from the registry at startup, so converters installed by winget are found.

## [0.1.0] - 2026-10-30

### Added

- Rust port of ShiftDrop.
- One-line install that downloads the latest release when not run from a checkout.
- `.deb`, tarball and extension zip built by a release workflow on each `v*` tag.
- MIT license.

### Changed

- Install a copy of the binary instead of linking into `target/`.
- Only offer a format when ffmpeg has its encoder.

### Fixed

- Cancel stops commands that print nothing.
- Hide the wheel and status windows on close instead of destroying them.

[Unreleased]: https://github.com/jayhemnani9910/shiftdrop-rust/compare/v0.2.0...HEAD
[0.2.0]: https://github.com/jayhemnani9910/shiftdrop-rust/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/jayhemnani9910/shiftdrop-rust/releases/tag/v0.1.0
