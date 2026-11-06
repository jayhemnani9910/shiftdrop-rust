# Contributing

I try to reply to issues within a few days.

## Setup

On Linux, with Rust stable:

```
sudo apt install libgtk-4-dev ffmpeg imagemagick ghostscript poppler-utils libreoffice-writer
cargo build
```

To try your build on the desktop:

```
./install.sh
pkill -f "shiftdrop --gapplication-service"
```

Changes to the app apply on the next drag after those two commands. Changes to `extension.js` need a logout:
the shell keeps the first copy it loaded.

## Tests

```
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
```

`tests/plans.rs` checks the planned commands. `tests/real_conversions.rs` runs real conversions on tiny generated
files and skips a case when its tool is missing. CI runs clippy and `tests/plans.rs` on Linux and Windows, so run
the real conversions yourself before a PR.

Windows code is in `src/win/` and only builds on Windows. CI builds it, so you'll see breakage there on the PR.

## Style

Keep it plain and small. New conversions go in `convert.rs`, new tools in `tools.rs`, and each gets a test.
`cargo clippy` has to pass with no warnings. Commit messages are one short line saying what changed, like
`Cancel stops commands that print nothing`.

## Branches and PRs

Branch from `main`, one change per PR. Say what changed and how you tested it. If you touched the drag or the
wheel, say which GNOME version or Windows version you tried it on.

## Bugs

Use the bug report template. For drag problems, turn on the debug log first (see "Debugging the drag" in the
README) and paste the lines from one drag.
