//! Memory usage by process for a terminal UI: find what is large, then end it.
//!
//! This crate backs the `ramtree` binary. The reusable part is [`procs`], which
//! reads the process table and returns it as a flat arena of
//! [`Node`](procs::Node)s addressed by index, arranged by parent process, with
//! memory and process counts already rolled up into every group.
//!
//! ```
//! use ramtree::procs::{ProcInfo, build};
//!
//! let p = |pid, parent, name: &str, memory| ProcInfo {
//!     pid,
//!     parent,
//!     name: name.to_owned(),
//!     memory,
//!     start_time: 0,
//! };
//! let tree = build(
//!     vec![p(1, None, "shell", 100), p(2, Some(1), "browser", 4096)],
//!     1 << 30,
//!     1 << 29,
//! );
//!
//! // The root aggregates every process.
//! let root = &tree.nodes[tree.root];
//! assert_eq!(root.n_procs, 2);
//! assert_eq!(root.size, 4196);
//!
//! // A process with children is a group; its own memory is a `[self]` leaf.
//! let shell = root.children[0];
//! assert!(tree.nodes[shell].is_dir);
//! let mut kids = tree.nodes[shell].children.clone();
//! kids.sort_by_key(|&i| std::cmp::Reverse(tree.nodes[i].size));
//! assert_eq!(tree.nodes[kids[0]].name, "browser (2)");
//! assert_eq!(tree.nodes[kids[1]].name, "shell (1) [self]");
//! ```
//!
//! # What the numbers mean
//!
//! Sizes are **resident** memory: the working set on Windows and the resident
//! set size on macOS and Linux — what Task Manager's "Working set" and Activity
//! Monitor's "Real Mem" columns show. Pages shared between processes (system
//! libraries, shared memory) count once in every process that maps them, so
//! the total can exceed the RAM actually in use. Memory that is swapped or
//! compressed out is not resident and is not counted.
//!
//! Processes whose memory the OS will not report, usually for lack of
//! permission, count as zero and are tallied in [`Tree::errors`](procs::Tree::errors).

#![warn(missing_docs)]

pub mod procs;

// The UI layer is an implementation detail of the binary: its signatures name
// `ratatui` types, so exposing it as documented API would make every ratatui
// version bump a breaking change for this crate.
#[doc(hidden)]
pub mod app;
#[doc(hidden)]
pub mod ui;
