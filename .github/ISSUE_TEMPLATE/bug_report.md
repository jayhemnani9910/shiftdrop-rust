---
name: Bug report
about: Something doesn't work
labels: bug
---

**Steps**

1.
2.

**Expected**

**Actual**

**Version**

ShiftDrop version or commit, and how you installed it (install script, .deb, source, Windows installer).

**OS**

Distribution and GNOME version, or Windows version.

**Debug log**

For drag problems: `touch ~/.cache/shiftdrop-debug`, do one drag, then paste what
`journalctl --user -o cat | grep shiftdrop` shows. On Windows: `%LOCALAPPDATA%\shiftdrop\shiftdrop.log`.
