# ramtree

A memory usage viewer for your terminal, built on the same UI as [disktree](https://github.com/dmoore-dwmmholdings/disktree): see which processes are using your RAM, drill into process trees, and end what you don't need — without leaving the console.

```
 All processes  —  25.3 GB, 425 processes  (RAM 51.2 GB of 63.1 GB in use)  (148 unreadable)
      Memory      %             Processes Name
      7.9 GB  31.4% ███               226 wininit.exe (1976)\
      6.9 GB  27.4% ██▌                74 explorer.exe (12620)\
      4.5 GB  17.6% ██                 28 zen.exe (15328)\
   1000.1 MB   3.9% ▌                   6 Discord.exe (70152)\
 ↑↓ move  enter open  backspace up  space mark  c clear  d end process  D force kill  r refresh  q quit
```

- **Process tree**: processes are grouped under their parent, and each group's size includes everything it started, so a browser and its dozens of helpers show as one line
- **Marks and safe ending**: mark any number of processes, then ask them to close or force-kill them, always behind a confirmation that lists **where each one sits** in the tree
- Sizes update in place after a kill; `r` refreshes without losing your place
- Runs on Windows, macOS, and Linux

## Install

```bash
cargo install ramtree
```

Requires Rust 1.95 or newer.

## Keys

| Key | Action |
|---|---|
| `↑` `↓` or `k` `j` | Move selection |
| `PgUp` `PgDn` | Move by 20 |
| `Home` `End` or `g` `G` | Jump to first / last |
| `Enter` `→` `l` | Open a process group |
| `Backspace` `←` `h` | Go up |
| `Space` | Mark / unmark, then advance |
| `c` | Clear all marks |
| `d` | Ask marked (or selected) processes to **close** |
| `D` | **Force kill** them |
| `r` | Refresh |
| `q` | Quit |
| `Esc` | Ask to quit (`Esc` again quits, `Backspace` returns) |

A process that started other processes is a group, shown in cyan with a trailing `\`. Inside it, its own memory is the `[self]` row. Ending a group ends its whole subtree; ending its `[self]` row ends just that process.

On macOS, children of `launchd` appear at the top level so you can compare apps directly. Each app's own process subtree stays grouped. `launchd` itself shows only its own memory and cannot be ended by ramtree.

`d` sends SIGTERM on macOS and Linux. On Windows it asks the process's windows to close, as their close buttons would, so an app may stop to ask about unsaved work; processes with no window cannot be asked and need `D`. ramtree waits up to 3 seconds for them to exit and reports any still running. `D` is SIGKILL or `TerminateProcess`: immediate, and unsaved work is lost.

ramtree never ends a process it runs inside (its terminal, the terminal's parent, and so on), and checks each process's start time first, so a stale view cannot end an unrelated process that has reused a PID.

## Understanding the numbers

**Sizes are resident memory**: the working set on Windows (Task Manager's "Working set" column) and the resident set size on macOS and Linux (Activity Monitor's "Real Mem").

- **Shared pages count in every process that maps them.** System libraries and shared memory appear in each process's figure, so the per-process total can exceed the RAM actually in use.
- **Swapped or compressed memory is not counted**, because it is not resident.
- **The process total rarely matches "in use"**, because RAM also holds the kernel, drivers, file cache, and compressed memory, none of which belongs to one process.
- **Unreadable processes count as zero.** The header shows how many. Without elevation, Windows hides the memory of services and other users' processes, and macOS hides root's. Run elevated (`sudo` on macOS) to see more; protected Windows processes stay hidden even then.

## Contributing

Issues and pull requests are welcome. Before opening a PR:

```bash
cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test --all-targets && cargo test --doc
```

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or [MIT license](LICENSE-MIT), at your option.
