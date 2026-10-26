# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Fixed

- On macOS, show launchd's children at the top level instead of hiding unrelated
  apps inside one large launchd group. Preserve app subtrees and keep launchd
  protected from termination.

## [0.1.0] - 2026-10-25

### Added

- First release, forked from [disktree](https://github.com/dmoore-dwmmholdings/disktree)
  0.2.1 with the same UI. Browses running processes as a tree by parent process,
  sized by resident memory, on Windows, macOS, and Linux.
- `d` asks marked (or selected) processes to close; `D` force-kills them. Both
  act on a group's whole subtree, confirm first, and skip ramtree's own
  ancestors.
- `r` refreshes in place, keeping the open group, selection, and marks.
