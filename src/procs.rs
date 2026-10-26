//! Reading the process table into a flat, index-addressed tree.
//!
//! [`scan`] reads every process and returns a [`Tree`]: a `Vec<Node>` arena in
//! which every node names its parent and its children by index. Memory and
//! process counts are aggregated bottom-up, so `tree.nodes[i].size` is already
//! the total for that whole subtree.
//!
//! See the [crate docs](crate) for what the sizes do and do not include.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};

/// Label of the root node, which stands for every process on the machine.
pub const ROOT_NAME: &str = "All processes";

/// Live counters a running [`scan`] publishes for a progress display.
#[derive(Default)]
pub struct ScanProgress {
    /// Processes counted so far.
    pub processes: AtomicU64,
    /// Resident bytes counted so far.
    pub bytes: AtomicU64,
}

/// One process as read from the OS, before it is placed in a [`Tree`].
#[derive(Clone)]
pub struct ProcInfo {
    /// Process ID.
    pub pid: u32,
    /// The parent's process ID, if the OS reports one.
    pub parent: Option<u32>,
    /// Executable name, e.g. `chrome.exe`.
    pub name: String,
    /// Resident memory in bytes: the working set on Windows, RSS on macOS.
    pub memory: u64,
    /// Start time in seconds since the epoch. Together with `pid` this
    /// identifies a process even after its PID is reused.
    pub start_time: u64,
}

/// One entry in the tree: the root, a process, or a group of processes.
///
/// A process with children becomes a group (`is_dir`). Its own memory is then
/// held by a leaf child flagged `is_self`, so that a group's size is exactly the
/// sum of its children, as a directory's is.
pub struct Node {
    /// Display label, e.g. `chrome.exe (812)`.
    pub name: String,
    /// The process this node stands for, or `None` for the root.
    pub pid: Option<u32>,
    /// See [`ProcInfo::start_time`].
    pub start_time: u64,
    /// Whether this node has children to open.
    pub is_dir: bool,
    /// Whether this leaf holds the own memory of its parent group's process.
    pub is_self: bool,
    /// Total resident bytes in this entry, including its whole subtree.
    pub size: u64,
    /// Total processes in this entry, including its whole subtree.
    pub n_procs: u64,
    /// Index of the containing group, or `None` for the root.
    pub parent: Option<usize>,
    /// Indices of the entries directly inside this one, in no particular order.
    pub children: Vec<usize>,
    /// Set by [`Tree::remove`]. Indices stay valid after a removal, so held
    /// indices (such as a set of marked rows) do not dangle; use
    /// [`Tree::is_gone`] to test whether one is still live.
    pub deleted: bool,
}

/// A snapshot of the process tree, stored as a flat arena.
pub struct Tree {
    /// Every entry. Indices into this are stable for the tree's lifetime.
    pub nodes: Vec<Node>,
    /// Index of the root node in [`Self::nodes`].
    pub root: usize,
    /// Physical memory installed, in bytes.
    pub total_ram: u64,
    /// Physical memory in use, in bytes, as the OS counts it.
    pub used_ram: u64,
    /// Processes whose memory could not be read and so count as zero.
    pub errors: u64,
    /// Wall-clock seconds the scan took.
    pub scan_secs: f64,
}

impl Tree {
    /// The breadcrumb from the root to a node, e.g.
    /// `All processes > explorer.exe (5123) > chrome.exe (812)`.
    pub fn path_of(&self, mut idx: usize) -> String {
        let mut parts: Vec<&str> = vec![&self.nodes[idx].name];
        while let Some(parent) = self.nodes[idx].parent {
            idx = parent;
            parts.push(&self.nodes[idx].name);
        }
        parts.reverse();
        parts.join(" > ")
    }

    /// Detach a node from the tree and subtract its size/counts from all ancestors.
    pub fn remove(&mut self, idx: usize) {
        self.nodes[idx].deleted = true;
        let size = self.nodes[idx].size;
        let n_procs = self.nodes[idx].n_procs;
        if let Some(parent) = self.nodes[idx].parent {
            self.nodes[parent].children.retain(|&c| c != idx);
            let mut ancestor = Some(parent);
            while let Some(i) = ancestor {
                self.nodes[i].size -= size;
                self.nodes[i].n_procs -= n_procs;
                ancestor = self.nodes[i].parent;
            }
        }
    }

    /// True if this node or any of its ancestors has been removed.
    pub fn is_gone(&self, mut idx: usize) -> bool {
        loop {
            if self.nodes[idx].deleted {
                return true;
            }
            match self.nodes[idx].parent {
                Some(p) => idx = p,
                None => return false,
            }
        }
    }

    /// Every leaf in the subtree at `idx`, descendants before ancestors, so
    /// ending them in this order never orphans a child that is still to come.
    pub fn leaves_of(&self, idx: usize) -> Vec<usize> {
        let mut out = Vec::new();
        let mut stack = vec![idx];
        while let Some(i) = stack.pop() {
            if self.nodes[i].children.is_empty() {
                if self.nodes[i].pid.is_some() {
                    out.push(i);
                }
            } else {
                stack.extend(&self.nodes[i].children);
            }
        }
        // Children always have higher indices than their parents.
        out.sort_unstable_by(|a, b| b.cmp(a));
        out
    }
}

/// Read every process on the machine and return the tree.
pub fn scan(progress: &ScanProgress) -> Tree {
    let start = Instant::now();
    let mut sys = System::new();
    sys.refresh_memory();
    sys.refresh_processes_specifics(
        ProcessesToUpdate::All,
        true,
        ProcessRefreshKind::nothing().with_memory(),
    );

    let procs: Vec<ProcInfo> = sys
        .processes()
        .values()
        // Linux lists each thread as a task of its process; they share its memory.
        .filter(|p| {
            p.thread_kind()
                .is_none_or(|k| k == sysinfo::ThreadKind::Kernel)
        })
        .map(|p| {
            progress.processes.fetch_add(1, Ordering::Relaxed);
            progress.bytes.fetch_add(p.memory(), Ordering::Relaxed);
            ProcInfo {
                pid: p.pid().as_u32(),
                parent: p.parent().map(Pid::as_u32),
                name: p.name().to_string_lossy().into_owned(),
                memory: p.memory(),
                start_time: p.start_time(),
            }
        })
        .collect();

    let mut tree = build(procs, sys.total_memory(), sys.used_memory());
    tree.scan_secs = start.elapsed().as_secs_f64();
    tree
}

/// Arrange processes into a tree by parent PID.
///
/// A parent link is trusted only if the parent started no later than the
/// child: Windows reuses PIDs, so an orphan's recorded parent PID can belong to
/// an unrelated, newer process. Untrusted links make the process top-level.
pub fn build(procs: Vec<ProcInfo>, total_ram: u64, used_ram: u64) -> Tree {
    let by_pid: HashMap<u32, usize> = procs.iter().enumerate().map(|(i, p)| (p.pid, i)).collect();
    let parent_of = |i: usize| -> Option<usize> {
        let p = &procs[i];
        let &j = by_pid.get(&p.parent?)?;
        (j != i && procs[j].start_time <= p.start_time).then_some(j)
    };

    let mut kids: Vec<Vec<usize>> = vec![Vec::new(); procs.len()];
    let mut roots = Vec::new();
    for i in 0..procs.len() {
        match parent_of(i) {
            Some(j) => kids[j].push(i),
            None => roots.push(i),
        }
    }

    let mut nodes = vec![Node {
        name: ROOT_NAME.to_owned(),
        pid: None,
        start_time: 0,
        is_dir: true,
        is_self: false,
        size: 0,
        n_procs: 0,
        parent: None,
        children: Vec::new(),
        deleted: false,
    }];
    let mut visited = vec![false; procs.len()];
    let mut errors = 0;

    // Roots first so every process lands under its real parent. Equal start
    // times can still form a cycle with no root; those are attached at the top.
    for start in roots.into_iter().chain(0..procs.len()) {
        let mut stack = vec![(start, 0usize)];
        while let Some((i, parent)) = stack.pop() {
            if visited[i] {
                continue;
            }
            visited[i] = true;
            let p = &procs[i];
            // PID 0 is the idle process; it has no memory to read.
            if p.memory == 0 && p.pid != 0 {
                errors += 1;
            }
            let label = format!("{} ({})", p.name, p.pid);
            let pending: Vec<usize> = kids[i].iter().copied().filter(|&k| !visited[k]).collect();
            let leaf = |name: String, parent: usize, is_self: bool| Node {
                name,
                pid: Some(p.pid),
                start_time: p.start_time,
                is_dir: false,
                is_self,
                size: p.memory,
                n_procs: 1,
                parent: Some(parent),
                children: Vec::new(),
                deleted: false,
            };

            let idx = nodes.len();
            nodes[parent].children.push(idx);
            if pending.is_empty() {
                nodes.push(leaf(label, parent, false));
            } else {
                nodes.push(Node {
                    is_dir: true,
                    size: 0,
                    n_procs: 0,
                    ..leaf(label.clone(), parent, false)
                });
                nodes[idx].children.push(idx + 1);
                nodes.push(leaf(format!("{label} [self]"), idx, true));
                stack.extend(pending.into_iter().map(|k| (k, idx)));
            }
        }
    }

    // Children always have higher indices than their parents, so one reverse
    // pass rolls every leaf up into all of its ancestors.
    for i in (1..nodes.len()).rev() {
        let (size, n) = (nodes[i].size, nodes[i].n_procs);
        let parent = nodes[i].parent.expect("only the root has no parent");
        nodes[parent].size += size;
        nodes[parent].n_procs += n;
    }

    Tree {
        nodes,
        root: 0,
        total_ram,
        used_ram,
        errors,
        scan_secs: 0.0,
    }
}

/// A handle on the live process table, for ending processes from a [`Tree`].
pub struct Killer {
    sys: System,
}

impl Default for Killer {
    fn default() -> Self {
        Self::new()
    }
}

impl Killer {
    /// Read the current process table.
    pub fn new() -> Self {
        let mut sys = System::new();
        sys.refresh_processes_specifics(
            ProcessesToUpdate::All,
            true,
            ProcessRefreshKind::nothing(),
        );
        Killer { sys }
    }

    /// True if the process that `pid` named at `start_time` is still running.
    ///
    /// The start time is what stops a stale tree from ending an unrelated
    /// process that has since been given the same PID. A zombie has exited and
    /// holds no memory; it stays listed only until its parent reaps it.
    pub fn is_alive(&self, pid: u32, start_time: u64) -> bool {
        self.sys.process(Pid::from_u32(pid)).is_some_and(|p| {
            p.start_time() == start_time && p.status() != sysinfo::ProcessStatus::Zombie
        })
    }

    /// Ask a process to exit, or with `force`, kill it outright.
    ///
    /// Returning `Ok` means the request was delivered, not that the process
    /// has exited; use [`Self::wait_for_exit`] for that.
    pub fn end(&self, pid: u32, force: bool) -> Result<(), String> {
        let Some(process) = self.sys.process(Pid::from_u32(pid)) else {
            return Ok(());
        };
        if force {
            return if process.kill() {
                Ok(())
            } else {
                Err("access denied".to_owned())
            };
        }
        graceful_end(process)
    }

    /// Re-read the given processes until all have exited or `timeout` passes,
    /// and return those still running.
    pub fn wait_for_exit(&mut self, pending: &[(u32, u64)], timeout: Duration) -> HashSet<u32> {
        let pids: Vec<Pid> = pending.iter().map(|&(p, _)| Pid::from_u32(p)).collect();
        let deadline = Instant::now() + timeout;
        loop {
            self.sys.refresh_processes_specifics(
                ProcessesToUpdate::Some(&pids),
                true,
                ProcessRefreshKind::nothing(),
            );
            let alive: HashSet<u32> = pending
                .iter()
                .filter(|&&(p, t)| self.is_alive(p, t))
                .map(|&(p, _)| p)
                .collect();
            if alive.is_empty() || Instant::now() >= deadline {
                return alive;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

/// Send SIGTERM, which a process can catch to save its state and exit.
#[cfg(not(windows))]
fn graceful_end(process: &sysinfo::Process) -> Result<(), String> {
    match process.kill_with(sysinfo::Signal::Term) {
        Some(true) => Ok(()),
        _ => Err("permission denied".to_owned()),
    }
}

/// Windows has no SIGTERM. `taskkill` without `/F` posts WM_CLOSE to the
/// process's windows, which is what clicking their close buttons does.
#[cfg(windows)]
fn graceful_end(process: &sysinfo::Process) -> Result<(), String> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    let out = std::process::Command::new("taskkill")
        .args(["/PID", &process.pid().as_u32().to_string()])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_err(|e| e.to_string())?;
    if out.status.success() {
        return Ok(());
    }
    let text = String::from_utf8_lossy(&out.stderr);
    if text.contains("forcefully") {
        Err("it has no window to close; use D to force kill".to_owned())
    } else {
        Err(text.trim().trim_start_matches("ERROR: ").to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(pid: u32, parent: Option<u32>, name: &str, memory: u64, start_time: u64) -> ProcInfo {
        ProcInfo {
            pid,
            parent,
            name: name.to_owned(),
            memory,
            start_time,
        }
    }

    fn child_by_name(tree: &Tree, parent: usize, name: &str) -> usize {
        *tree.nodes[parent]
            .children
            .iter()
            .find(|&&c| tree.nodes[c].name == name)
            .unwrap()
    }

    fn fixture() -> Tree {
        build(
            vec![
                info(1, None, "init", 100, 10),
                info(2, Some(1), "shell", 2048, 20),
                info(3, Some(2), "editor", 1, 30),
                info(4, None, "daemon", 500, 10),
            ],
            1 << 30,
            1 << 29,
        )
    }

    #[test]
    fn build_aggregates_sizes() {
        let tree = fixture();
        assert_eq!(tree.nodes[tree.root].size, 2649);
        assert_eq!(tree.nodes[tree.root].n_procs, 4);

        let init = child_by_name(&tree, tree.root, "init (1)");
        assert!(tree.nodes[init].is_dir);
        assert_eq!(tree.nodes[init].size, 2149);
        assert_eq!(tree.nodes[init].n_procs, 3);

        // A group's own memory sits in its [self] leaf.
        let own = child_by_name(&tree, init, "init (1) [self]");
        assert!(tree.nodes[own].is_self);
        assert_eq!(tree.nodes[own].size, 100);

        let shell = child_by_name(&tree, init, "shell (2)");
        let editor = child_by_name(&tree, shell, "editor (3)");
        assert!(!tree.nodes[editor].is_dir);
        assert_eq!(
            tree.path_of(editor),
            "All processes > init (1) > shell (2) > editor (3)"
        );
    }

    /// A parent PID that now belongs to a younger process was reused, so the
    /// child must not be filed under it.
    #[test]
    fn reused_parent_pid_is_not_trusted() {
        let tree = build(
            vec![
                info(7, None, "newcomer", 10, 500),
                info(8, Some(7), "orphan", 20, 100),
            ],
            0,
            0,
        );
        let top: Vec<&str> = tree.nodes[tree.root]
            .children
            .iter()
            .map(|&c| tree.nodes[c].name.as_str())
            .collect();
        assert!(
            top.contains(&"orphan (8)"),
            "orphan filed under a newer process: {top:?}"
        );
    }

    /// Processes whose parents form a cycle must still all appear exactly once.
    #[test]
    fn parent_cycle_is_broken() {
        let tree = build(
            vec![info(1, Some(2), "a", 10, 5), info(2, Some(1), "b", 20, 5)],
            0,
            0,
        );
        assert_eq!(tree.nodes[tree.root].n_procs, 2);
        assert_eq!(tree.nodes[tree.root].size, 30);
    }

    #[test]
    fn remove_propagates_up() {
        let mut tree = fixture();
        let init = child_by_name(&tree, tree.root, "init (1)");
        let shell = child_by_name(&tree, init, "shell (2)");
        let editor = child_by_name(&tree, shell, "editor (3)");
        tree.remove(editor);

        assert_eq!(tree.nodes[shell].size, 2048);
        assert_eq!(tree.nodes[shell].n_procs, 1);
        assert_eq!(tree.nodes[tree.root].size, 2648);
        assert_eq!(tree.nodes[tree.root].n_procs, 3);
        assert!(tree.is_gone(editor));
        assert!(!tree.is_gone(shell));
    }

    #[test]
    fn leaves_come_descendants_first() {
        let tree = fixture();
        let init = child_by_name(&tree, tree.root, "init (1)");
        let pids: Vec<u32> = tree
            .leaves_of(init)
            .iter()
            .map(|&l| tree.nodes[l].pid.unwrap())
            .collect();
        assert_eq!(pids.len(), 3);
        assert_eq!(
            pids.last(),
            Some(&1),
            "the group's own process must end last: {pids:?}"
        );
    }

    #[test]
    fn scan_finds_this_process() {
        let progress = ScanProgress::default();
        let tree = scan(&progress);
        let me = std::process::id();
        assert!(tree.nodes.iter().any(|n| n.pid == Some(me)));
        assert!(tree.total_ram > 0);
        assert!(tree.nodes[tree.root].size > 0);
    }
}
