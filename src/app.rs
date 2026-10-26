use std::collections::{HashMap, HashSet};
use std::time::Duration;

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::widgets::TableState;

use crate::procs::{Killer, Tree};

pub enum Mode {
    Browse,
    Confirm { force: bool, targets: Vec<usize> },
    Ending { count: u64 },
    ConfirmQuit,
}

/// What identifies a node across two scans: the process, and whether the node
/// is that process's `[self]` leaf rather than its group.
type NodeKey = (Option<u32>, u64, bool);

pub struct App {
    pub tree: Tree,
    /// Node index of the group currently being viewed.
    pub cwd: usize,
    /// Children of `cwd`, sorted by size descending.
    pub rows: Vec<usize>,
    pub selected: usize,
    /// Index of the first row drawn. Only the visible slice is ever built into
    /// widgets, so render cost is independent of how many children `cwd` has.
    pub offset: usize,
    pub marked: HashSet<usize>,
    pub mode: Mode,
    pub status: Option<String>,
    pub table_state: TableState,
    /// Set when a kill is confirmed; executed by the main loop after one
    /// "Ending…" frame has been drawn.
    pub pending_kill: Option<(Vec<usize>, bool)>,
    pub quit: bool,
    pub rescan: bool,
}

impl App {
    pub fn new(tree: Tree) -> Self {
        let status = Some(format!(
            "Read {} processes ({}) in {:.1} s",
            format_count(tree.nodes[tree.root].n_procs),
            format_size(tree.nodes[tree.root].size),
            tree.scan_secs,
        ));
        let mut app = App {
            cwd: tree.root,
            tree,
            rows: Vec::new(),
            selected: 0,
            offset: 0,
            marked: HashSet::new(),
            mode: Mode::Browse,
            status,
            table_state: TableState::default(),
            pending_kill: None,
            quit: false,
            rescan: false,
        };
        app.refresh_rows();
        app
    }

    /// Swap in a fresh scan, keeping the open group, selection, and marks for
    /// every process that still exists.
    pub fn reload(&mut self, tree: Tree) {
        let key = |t: &Tree, i: usize| -> NodeKey {
            let n = &t.nodes[i];
            (n.pid, n.start_time, n.is_self)
        };
        let mut chain = vec![key(&self.tree, self.cwd)];
        let mut at = self.cwd;
        while let Some(p) = self.tree.nodes[at].parent {
            chain.push(key(&self.tree, p));
            at = p;
        }
        let selected = self.rows.get(self.selected).map(|&i| key(&self.tree, i));
        let marked: Vec<NodeKey> = self.marked.iter().map(|&i| key(&self.tree, i)).collect();

        let index: HashMap<NodeKey, usize> =
            (0..tree.nodes.len()).map(|i| (key(&tree, i), i)).collect();
        // The innermost open group that still exists; a process that lost its
        // children is a leaf now and cannot be opened.
        self.cwd = chain
            .iter()
            .filter_map(|k| index.get(k).copied())
            .find(|&i| tree.nodes[i].is_dir)
            .unwrap_or(tree.root);
        self.marked = marked
            .iter()
            .filter_map(|k| index.get(k).copied())
            .collect();
        self.status = Some(format!(
            "Refreshed: {} processes ({})",
            format_count(tree.nodes[tree.root].n_procs),
            format_size(tree.nodes[tree.root].size),
        ));
        self.tree = tree;
        self.refresh_rows();
        if let Some(pos) = selected.and_then(|k| {
            let &i = index.get(&k)?;
            self.rows.iter().position(|&r| r == i)
        }) {
            self.selected = pos;
        }
    }

    fn refresh_rows(&mut self) {
        self.rows = self.tree.nodes[self.cwd].children.clone();
        self.rows.sort_unstable_by(|&a, &b| {
            let (na, nb) = (&self.tree.nodes[a], &self.tree.nodes[b]);
            nb.size.cmp(&na.size).then_with(|| na.name.cmp(&nb.name))
        });
        if self.selected >= self.rows.len() {
            self.selected = self.rows.len().saturating_sub(1);
        }
    }

    /// Clamp `offset` so the selection is on screen, and return the range of
    /// rows to draw.
    ///
    /// Called every frame rather than only on key events, because a terminal
    /// resize changes `height` without any key being pressed.
    pub fn visible_range(&mut self, height: usize) -> std::ops::Range<usize> {
        let len = self.rows.len();
        if len == 0 || height == 0 {
            self.offset = 0;
            return 0..0;
        }
        if self.selected >= len {
            self.selected = len - 1;
        }
        // Scroll just far enough to bring the selection back into view.
        if self.selected < self.offset {
            self.offset = self.selected;
        } else if self.selected >= self.offset + height {
            self.offset = self.selected - height + 1;
        }
        // Never leave blank rows below a full list.
        self.offset = self.offset.min(len.saturating_sub(height));
        self.offset..(self.offset + height).min(len)
    }

    pub fn on_key(&mut self, key: KeyEvent) {
        match self.mode {
            Mode::Browse => self.on_key_browse(key),
            Mode::Confirm { .. } => self.on_key_confirm(key),
            Mode::Ending { .. } => {}
            Mode::ConfirmQuit => match key.code {
                KeyCode::Esc => self.quit = true,
                KeyCode::Backspace => self.mode = Mode::Browse,
                _ => {}
            },
        }
    }

    fn on_key_browse(&mut self, key: KeyEvent) {
        self.status = None;
        match key.code {
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.quit = true;
            }
            KeyCode::Char('q') => self.quit = true,
            KeyCode::Esc => self.mode = Mode::ConfirmQuit,
            KeyCode::Up | KeyCode::Char('k') => self.move_selection(-1),
            KeyCode::Down | KeyCode::Char('j') => self.move_selection(1),
            KeyCode::PageUp => self.move_selection(-20),
            KeyCode::PageDown => self.move_selection(20),
            KeyCode::Home | KeyCode::Char('g') => self.selected = 0,
            KeyCode::End | KeyCode::Char('G') => {
                self.selected = self.rows.len().saturating_sub(1);
            }
            KeyCode::Enter | KeyCode::Right | KeyCode::Char('l') => self.enter_dir(),
            KeyCode::Backspace | KeyCode::Left | KeyCode::Char('h') => self.go_up(),
            KeyCode::Char(' ') => self.toggle_mark(),
            KeyCode::Char('c') => {
                self.marked.clear();
                self.status = Some("Cleared all marks".to_owned());
            }
            KeyCode::Char('d') => self.request_kill(false),
            KeyCode::Char('D') => self.request_kill(true),
            KeyCode::Char('r') => self.rescan = true,
            _ => {}
        }
    }

    fn on_key_confirm(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => {
                if let Mode::Confirm { force, targets } =
                    std::mem::replace(&mut self.mode, Mode::Browse)
                {
                    self.mode = Mode::Ending {
                        count: targets.iter().map(|&t| self.tree.nodes[t].n_procs).sum(),
                    };
                    self.pending_kill = Some((targets, force));
                }
            }
            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc | KeyCode::Char('q') => {
                self.mode = Mode::Browse;
            }
            _ => {}
        }
    }

    fn move_selection(&mut self, delta: isize) {
        if self.rows.is_empty() {
            return;
        }
        let max = self.rows.len() as isize - 1;
        self.selected = (self.selected as isize + delta).clamp(0, max) as usize;
    }

    fn enter_dir(&mut self) {
        if let Some(&idx) = self.rows.get(self.selected)
            && self.tree.nodes[idx].is_dir
        {
            self.cwd = idx;
            self.selected = 0;
            self.refresh_rows();
        }
    }

    fn go_up(&mut self) {
        if let Some(parent) = self.tree.nodes[self.cwd].parent {
            let old = self.cwd;
            self.cwd = parent;
            self.refresh_rows();
            self.selected = self.rows.iter().position(|&i| i == old).unwrap_or(0);
        }
    }

    fn toggle_mark(&mut self) {
        if let Some(&idx) = self.rows.get(self.selected) {
            if !self.marked.remove(&idx) {
                self.marked.insert(idx);
            }
            self.move_selection(1);
        }
    }

    fn request_kill(&mut self, force: bool) {
        let mut targets: Vec<usize> = if self.marked.is_empty() {
            self.rows.get(self.selected).copied().into_iter().collect()
        } else {
            self.marked.iter().copied().collect()
        };

        // Drop targets whose ancestor is also targeted — ending the ancestor's
        // group ends them anyway, and a second attempt would just fail.
        let set: HashSet<usize> = targets.iter().copied().collect();
        targets.retain(|&t| {
            let mut ancestor = self.tree.nodes[t].parent;
            while let Some(i) = ancestor {
                if set.contains(&i) {
                    return false;
                }
                ancestor = self.tree.nodes[i].parent;
            }
            true
        });

        if targets.is_empty() {
            return;
        }
        targets.sort_unstable_by(|&a, &b| self.tree.nodes[b].size.cmp(&self.tree.nodes[a].size));
        self.mode = Mode::Confirm { force, targets };
    }

    /// End every process under `targets`, wait briefly for them to exit, and
    /// drop the ones that did from the tree.
    pub fn perform_kill(&mut self, targets: &[usize], force: bool) {
        // Ending a process ramtree runs inside (its terminal, that terminal's
        // parent, …) would end ramtree partway through the list.
        let mut protected: HashSet<u32> = HashSet::new();
        // The display omits parent links to launchd, but it is still an ancestor.
        #[cfg(target_os = "macos")]
        protected.insert(1);
        let me = std::process::id();
        if let Some(mut at) = self.tree.nodes.iter().position(|n| n.pid == Some(me)) {
            loop {
                protected.extend(self.tree.nodes[at].pid);
                match self.tree.nodes[at].parent {
                    Some(p) => at = p,
                    None => break,
                }
            }
        }

        let mut killer = Killer::new();
        let mut vanished = Vec::new();
        let mut sent = Vec::new();
        let mut skipped = 0usize;
        let mut failed = 0usize;
        let mut first_err: Option<String> = None;

        for leaf in targets.iter().flat_map(|&t| self.tree.leaves_of(t)) {
            let node = &self.tree.nodes[leaf];
            let pid = node.pid.expect("leaves are processes");
            if protected.contains(&pid) {
                skipped += 1;
            } else if !killer.is_alive(pid, node.start_time) {
                // The tree can be stale, and the PID may now be someone else's.
                vanished.push(leaf);
            } else {
                match killer.end(pid, force) {
                    Ok(()) => sent.push(leaf),
                    Err(e) => {
                        failed += 1;
                        first_err.get_or_insert(e);
                    }
                }
            }
        }

        // A force kill is immediate; a polite request may wait on the app
        // finishing up, or on a "save changes?" prompt nobody has answered.
        let timeout = Duration::from_millis(if force { 1000 } else { 3000 });
        let pending: Vec<(u32, u64)> = sent
            .iter()
            .map(|&l| {
                (
                    self.tree.nodes[l].pid.unwrap(),
                    self.tree.nodes[l].start_time,
                )
            })
            .collect();
        let alive = killer.wait_for_exit(&pending, timeout);

        let mut ended = 0usize;
        let mut freed = 0u64;
        let mut still_running = 0usize;
        for &leaf in &sent {
            if alive.contains(&self.tree.nodes[leaf].pid.unwrap()) {
                still_running += 1;
            } else {
                ended += 1;
                freed += self.tree.nodes[leaf].size;
                self.remove_leaf(leaf);
            }
        }
        for &leaf in &vanished {
            self.remove_leaf(leaf);
        }

        self.marked.retain(|&m| !self.tree.is_gone(m));
        while self.tree.is_gone(self.cwd) {
            match self.tree.nodes[self.cwd].parent {
                Some(p) => self.cwd = p,
                None => break,
            }
        }
        self.refresh_rows();

        let verb = if force { "Killed" } else { "Ended" };
        let mut msg = format!(
            "{verb} {ended} process(es), {} released",
            format_size(freed)
        );
        if !vanished.is_empty() {
            msg.push_str(&format!(" — {} already gone", vanished.len()));
        }
        if failed > 0 {
            msg.push_str(&format!(
                " — {failed} failed ({})",
                first_err.unwrap_or_default()
            ));
        }
        if still_running > 0 {
            msg.push_str(&format!(
                " — {still_running} still running; it may be asking to save, or press D to force"
            ));
        }
        if skipped > 0 {
            msg.push_str(&format!(" — skipped {skipped} that ramtree runs inside"));
        }
        self.status = Some(msg);
        self.mode = Mode::Browse;
    }

    /// Remove a process leaf, then any group it leaves empty.
    fn remove_leaf(&mut self, leaf: usize) {
        self.tree.remove(leaf);
        let mut at = self.tree.nodes[leaf].parent;
        while let Some(group) = at {
            let node = &self.tree.nodes[group];
            if node.parent.is_none() || !node.children.is_empty() || node.deleted {
                break;
            }
            at = node.parent;
            self.tree.remove(group);
        }
    }
}

pub fn format_size(bytes: u64) -> String {
    const UNITS: [&str; 6] = ["B", "KB", "MB", "GB", "TB", "PB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    format!("{value:.1} {}", UNITS[unit])
}

pub fn format_count(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::procs::{ProcInfo, build};

    #[test]
    fn format_size_units() {
        assert_eq!(format_size(0), "0 B");
        assert_eq!(format_size(1023), "1023 B");
        assert_eq!(format_size(1024), "1.0 KB");
        assert_eq!(format_size(1536), "1.5 KB");
        assert_eq!(format_size(1024 * 1024 * 1024), "1.0 GB");
    }

    #[test]
    fn format_count_separators() {
        assert_eq!(format_count(0), "0");
        assert_eq!(format_count(999), "999");
        assert_eq!(format_count(1000), "1,000");
        assert_eq!(format_count(1234567), "1,234,567");
    }

    fn info(pid: u32, parent: Option<u32>, name: &str, memory: u64) -> ProcInfo {
        ProcInfo {
            pid,
            parent,
            name: name.to_owned(),
            memory,
            start_time: 1,
        }
    }

    /// Two same-named helpers under different parents. The PIDs are far above
    /// anything an OS hands out, so no real process can match them.
    fn same_name_fixture() -> App {
        App::new(build(
            vec![
                info(4_000_000_001, None, "alpha", 10),
                info(4_000_000_002, None, "beta", 10),
                info(4_000_000_003, Some(4_000_000_001), "helper", 300),
                info(4_000_000_004, Some(4_000_000_002), "helper", 400),
            ],
            1 << 30,
            1 << 29,
        ))
    }

    fn find_node(app: &App, pid: u32) -> usize {
        (0..app.tree.nodes.len())
            .find(|&i| app.tree.nodes[i].pid == Some(pid) && !app.tree.nodes[i].is_self)
            .expect("node not found")
    }

    /// The confirm dialog must say where each target sits, or the user is
    /// agreeing to end something they cannot identify.
    #[test]
    fn confirm_dialog_shows_full_paths_for_same_named_targets() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let mut app = same_name_fixture();
        let a = find_node(&app, 4_000_000_003);
        let b = find_node(&app, 4_000_000_004);
        app.marked.insert(a);
        app.marked.insert(b);
        app.mode = Mode::Confirm {
            force: false,
            targets: vec![a, b],
        };

        let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
        terminal.draw(|f| crate::ui::draw(f, &mut app)).unwrap();
        let buf = terminal.backend().buffer().clone();
        let screen: String = (0..24)
            .map(|y| {
                (0..100)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");

        assert!(
            screen.contains("alpha") && screen.contains("beta"),
            "dialog does not distinguish the two targets:\n{screen}"
        );
    }

    /// A target that exited between scan and confirm must reconcile the tree
    /// rather than report a phantom kill or leave a stale size behind.
    #[test]
    fn kill_reconciles_target_that_vanished() {
        let mut app = same_name_fixture();
        let a = find_node(&app, 4_000_000_003);
        let before = app.tree.nodes[app.tree.root].size;

        app.perform_kill(&[a], true);

        assert!(app.tree.is_gone(a), "vanished target still in tree");
        assert_eq!(app.tree.nodes[app.tree.root].size, before - 300);
        let status = app.status.clone().unwrap_or_default();
        assert!(status.contains("already gone"), "status was {status:?}");
    }

    /// Ending a group's only child process leaves just the `[self]` leaf; ending
    /// the whole group takes the group out with it.
    #[test]
    fn emptied_groups_are_removed() {
        let mut app = same_name_fixture();
        let alpha = find_node(&app, 4_000_000_001);
        app.perform_kill(&[alpha], true);
        assert!(app.tree.is_gone(alpha));
        assert_eq!(app.tree.nodes[app.tree.root].n_procs, 2);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn launchd_is_protected_without_a_display_parent_link() {
        // Deliberately stale start time: a regression can only remove the
        // synthetic row, never send a signal to the real launchd.
        for force in [false, true] {
            let tree = build(vec![info(1, None, "launchd", 100)], 1 << 30, 1 << 29);
            let mut app = App::new(tree);
            let launchd = find_node(&app, 1);

            app.perform_kill(&[launchd], force);

            assert!(!app.tree.is_gone(launchd));
            assert_eq!(app.tree.nodes[app.tree.root].size, 100);
            assert!(app.status.as_deref().unwrap().contains("skipped 1"));
        }
    }

    /// A long-running child process this test owns and can safely kill.
    fn spawn_sleeper() -> std::process::Child {
        use std::process::{Command, Stdio};
        #[cfg(windows)]
        let mut cmd = Command::new("ping");
        #[cfg(windows)]
        cmd.args(["-n", "60", "127.0.0.1"]);
        #[cfg(not(windows))]
        let mut cmd = Command::new("sleep");
        #[cfg(not(windows))]
        cmd.arg("60");
        cmd.stdout(Stdio::null()).spawn().expect("spawn sleeper")
    }

    fn app_with_sleeper() -> (std::process::Child, App, usize) {
        let child = spawn_sleeper();
        let app = App::new(crate::procs::scan(&Default::default()));
        let idx = find_node(&app, child.id());
        (child, app, idx)
    }

    #[test]
    fn force_kill_ends_a_real_process() {
        let (mut child, mut app, idx) = app_with_sleeper();
        app.perform_kill(&[idx], true);

        let status = app.status.clone().unwrap_or_default();
        assert!(
            status.starts_with("Killed 1 process"),
            "status was {status:?}"
        );
        assert!(app.tree.is_gone(idx));
        assert!(child.try_wait().unwrap().is_some(), "process survived");
    }

    /// SIGTERM ends a process that does not catch it.
    #[cfg(not(windows))]
    #[test]
    fn graceful_end_ends_a_real_process() {
        let (mut child, mut app, idx) = app_with_sleeper();
        app.perform_kill(&[idx], false);

        let status = app.status.clone().unwrap_or_default();
        assert!(
            status.starts_with("Ended 1 process"),
            "status was {status:?}"
        );
        assert!(child.wait().is_ok());
    }

    /// A windowless process cannot be asked to close on Windows; the message
    /// must say what to do instead of echoing taskkill.
    #[cfg(windows)]
    #[test]
    fn graceful_end_of_windowless_process_suggests_force() {
        let (mut child, mut app, idx) = app_with_sleeper();
        app.perform_kill(&[idx], false);

        let status = app.status.clone().unwrap_or_default();
        assert!(
            status.contains("use D to force kill"),
            "status was {status:?}"
        );
        assert!(!app.tree.is_gone(idx));
        child.kill().unwrap();
    }

    #[test]
    fn reload_keeps_view_and_marks() {
        let mut app = same_name_fixture();
        let alpha = find_node(&app, 4_000_000_001);
        app.cwd = alpha;
        app.refresh_rows();
        let helper = find_node(&app, 4_000_000_003);
        app.marked.insert(helper);

        app.reload(same_name_fixture().tree);

        assert_eq!(app.tree.nodes[app.cwd].pid, Some(4_000_000_001));
        let marked: Vec<_> = app.marked.iter().map(|&m| app.tree.nodes[m].pid).collect();
        assert_eq!(marked, vec![Some(4_000_000_003)]);
    }
}
