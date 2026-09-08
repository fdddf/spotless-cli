//! The interactive terminal UI — what `spotless` with no arguments opens.
//!
//! The TUI is a front end to exactly the same functions the subcommands call,
//! including the same safety guard: `c` on the Scan tab runs
//! [`crate::cmd::clean::execute`], not a second implementation of cleaning.
//!
//! Scans run on a worker thread and report back over a channel, so the list
//! stays responsive and a long walk of `~/Library/Caches` can be watched rather
//! than waited out. Only one worker runs at a time; while it does, the keys
//! that would start another are inert.

mod render;

use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::Duration;

use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use spotless_core::trash::TrashSummary;
use spotless_core::{AppInfo, DevArtifact, ScanTarget, TargetScan};

/// The four things the TUI can show. One tab, one question.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Scan,
    Dev,
    Apps,
    Trash,
}

impl Tab {
    const ALL: [Tab; 4] = [Tab::Scan, Tab::Dev, Tab::Apps, Tab::Trash];

    fn title(self) -> &'static str {
        match self {
            Tab::Scan => "Scan",
            Tab::Dev => "Developer",
            Tab::Apps => "Apps",
            Tab::Trash => "Trash",
        }
    }

    fn index(self) -> usize {
        Tab::ALL.iter().position(|t| *t == self).unwrap_or(0)
    }

    fn shifted(self, by: isize) -> Tab {
        let len = Tab::ALL.len() as isize;
        let next = (self.index() as isize + by).rem_euclid(len);
        Tab::ALL[next as usize]
    }
}

/// A list row that can be ticked.
pub struct Row<T> {
    pub item: T,
    pub selected: bool,
}

/// What a worker thread sends back.
enum Msg {
    Progress(String),
    ScanDone(Vec<TargetScan>),
    DevDone(Vec<DevArtifact>),
    AppsDone(Vec<AppInfo>),
    TrashDone(TrashSummary),
    /// A finished action, as the line to show the user.
    Finished(String),
}

/// A pending destructive action, waiting on y/n.
struct Confirm {
    question: String,
    action: Action,
}

enum Action {
    CleanScan,
    CleanDev,
    Uninstall,
    EmptyTrash,
}

pub struct App {
    tab: Tab,
    /// The ruleset, kept so a clean can re-approve the same roots the scan used.
    targets: Vec<ScanTarget>,
    scan: Vec<Row<TargetScan>>,
    dev: Vec<Row<DevArtifact>>,
    apps: Vec<AppInfo>,
    /// `None` until the Trash has actually been measured. Distinguishing
    /// that from a measured-and-empty Trash is the difference between the tab
    /// saying "measuring" and it asserting, wrongly, that there is nothing
    /// there.
    trash: Option<TrashSummary>,
    cursor: [usize; 4],
    /// The worker's latest progress line, and the fact that one is running.
    busy: Option<String>,
    status: String,
    confirm: Option<Confirm>,
    quit: bool,
    tx: Sender<Msg>,
    rx: Receiver<Msg>,
}

/// Open the TUI, and put the terminal back however it exits.
pub fn run() -> Result<()> {
    let mut terminal = ratatui::init();
    let mut app = App::new()?;
    app.start_scan();
    let result = app.event_loop(&mut terminal);
    ratatui::restore();
    result
}

impl App {
    fn new() -> Result<Self> {
        let (tx, rx) = mpsc::channel();
        Ok(Self {
            tab: Tab::Scan,
            targets: spotless_core::builtin_ruleset()?.targets,
            scan: Vec::new(),
            dev: Vec::new(),
            apps: Vec::new(),
            trash: None,
            cursor: [0; 4],
            busy: None,
            status: String::from("Scanning…"),
            confirm: None,
            quit: false,
            tx,
            rx,
        })
    }

    fn event_loop(&mut self, terminal: &mut ratatui::DefaultTerminal) -> Result<()> {
        while !self.quit {
            terminal.draw(|frame| render::draw(frame, self))?;

            // Drain everything the worker has said since the last frame, so a
            // fast scan does not need one frame per message to catch up.
            while let Ok(msg) = self.rx.try_recv() {
                self.handle(msg);
            }

            // The poll timeout is what makes progress visible: with no input,
            // this still wakes ten times a second to redraw the counters.
            if event::poll(Duration::from_millis(100))? {
                if let Event::Key(key) = event::read()? {
                    if key.kind == KeyEventKind::Press {
                        self.on_key(key.code, key.modifiers);
                    }
                }
            }
        }
        Ok(())
    }

    fn handle(&mut self, msg: Msg) {
        match msg {
            Msg::Progress(line) => self.busy = Some(line),
            Msg::ScanDone(targets) => {
                let mut rows: Vec<Row<TargetScan>> = targets
                    .into_iter()
                    .filter(|t| t.total_bytes > 0)
                    .map(|t| Row {
                        // Safe targets are pre-ticked; anything that costs time
                        // to rebuild is left for the user to choose.
                        selected: t.target.safety == spotless_core::SafetyTier::Safe,
                        item: t,
                    })
                    .collect();
                rows.sort_by_key(|r| std::cmp::Reverse(r.item.total_bytes));
                self.scan = rows;
                self.finish("space selects · c cleans");
            }
            Msg::DevDone(artifacts) => {
                self.dev = artifacts
                    .into_iter()
                    .map(|item| Row {
                        item,
                        selected: false,
                    })
                    .collect();
                self.finish("space selects · c cleans");
            }
            Msg::AppsDone(apps) => {
                self.apps = apps;
                self.finish("u uninstalls the highlighted app");
            }
            Msg::TrashDone(summary) => {
                self.trash = Some(summary);
                self.finish("e empties the Trash");
            }
            Msg::Finished(line) => {
                self.finish(&line);
                // What is on screen described the disk as it was before the
                // removal, so it is now wrong; re-measure rather than leave a
                // list that offers to clean what is already gone.
                self.refresh();
            }
        }
    }

    fn finish(&mut self, status: &str) {
        self.busy = None;
        self.status = status.to_string();
        self.clamp_cursor();
    }

    fn on_key(&mut self, code: KeyCode, modifiers: KeyModifiers) {
        // A pending confirmation swallows every key but its own answer: no
        // navigating away from a question about deleting things.
        if self.confirm.is_some() {
            match code {
                KeyCode::Char('y') | KeyCode::Char('Y') => self.commit(),
                _ => {
                    self.confirm = None;
                    self.status = "Cancelled.".into();
                }
            }
            return;
        }

        match code {
            KeyCode::Char('q') | KeyCode::Esc => self.quit = true,
            KeyCode::Char('c') if modifiers.contains(KeyModifiers::CONTROL) => self.quit = true,
            KeyCode::Tab | KeyCode::Right | KeyCode::Char('l') => self.switch(self.tab.shifted(1)),
            KeyCode::BackTab | KeyCode::Left | KeyCode::Char('h') => {
                self.switch(self.tab.shifted(-1))
            }
            KeyCode::Down | KeyCode::Char('j') => self.move_cursor(1),
            KeyCode::Up | KeyCode::Char('k') => self.move_cursor(-1),
            KeyCode::Char(' ') => self.toggle(),
            KeyCode::Char('a') => self.toggle_all(),
            KeyCode::Char('r') => self.refresh(),
            KeyCode::Char('c') => self.ask_clean(),
            KeyCode::Char('u') => self.ask_uninstall(),
            KeyCode::Char('e') => self.ask_empty(),
            _ => {}
        }
    }

    fn switch(&mut self, tab: Tab) {
        self.tab = tab;
        // Each tab loads on first visit rather than all at once at startup:
        // measuring every app on disk before the user has asked to see them is
        // seconds of work they did not ask for.
        let empty = match tab {
            Tab::Scan => self.scan.is_empty(),
            Tab::Dev => self.dev.is_empty(),
            Tab::Apps => self.apps.is_empty(),
            Tab::Trash => self.trash.is_none(),
        };
        if empty && self.busy.is_none() {
            self.refresh();
        }
    }

    fn rows(&self) -> usize {
        match self.tab {
            Tab::Scan => self.scan.len(),
            Tab::Dev => self.dev.len(),
            Tab::Apps => self.apps.len(),
            Tab::Trash => 0,
        }
    }

    fn cursor(&self) -> usize {
        self.cursor[self.tab.index()]
    }

    fn clamp_cursor(&mut self) {
        let last = self.rows().saturating_sub(1);
        let slot = self.tab.index();
        self.cursor[slot] = self.cursor[slot].min(last);
    }

    fn move_cursor(&mut self, by: isize) {
        let rows = self.rows();
        if rows == 0 {
            return;
        }
        let slot = self.tab.index();
        let next = (self.cursor[slot] as isize + by).clamp(0, rows as isize - 1);
        self.cursor[slot] = next as usize;
    }

    fn toggle(&mut self) {
        let at = self.cursor();
        match self.tab {
            Tab::Scan => {
                if let Some(row) = self.scan.get_mut(at) {
                    row.selected = !row.selected;
                }
            }
            Tab::Dev => {
                if let Some(row) = self.dev.get_mut(at) {
                    row.selected = !row.selected;
                }
            }
            _ => {}
        }
    }

    fn toggle_all(&mut self) {
        match self.tab {
            Tab::Scan => {
                let on = self.scan.iter().any(|r| !r.selected);
                self.scan.iter_mut().for_each(|r| r.selected = on);
            }
            Tab::Dev => {
                let on = self.dev.iter().any(|r| !r.selected);
                self.dev.iter_mut().for_each(|r| r.selected = on);
            }
            _ => {}
        }
    }

    /// Selected bytes on the current tab — the number in the header.
    fn selected_bytes(&self) -> u64 {
        match self.tab {
            Tab::Scan => self
                .scan
                .iter()
                .filter(|r| r.selected)
                .map(|r| r.item.total_bytes)
                .sum(),
            Tab::Dev => self
                .dev
                .iter()
                .filter(|r| r.selected)
                .map(|r| r.item.size_bytes)
                .sum(),
            Tab::Apps => self
                .apps
                .get(self.cursor())
                .and_then(|a| a.size_bytes)
                .unwrap_or(0),
            Tab::Trash => self.trash.as_ref().map_or(0, |t| t.bytes),
        }
    }

    // --- workers -----------------------------------------------------------

    fn refresh(&mut self) {
        if self.busy.is_some() {
            return;
        }
        match self.tab {
            Tab::Scan => self.start_scan(),
            Tab::Dev => self.start_dev(),
            Tab::Apps => self.start_apps(),
            Tab::Trash => self.start_trash(),
        }
    }

    fn start_scan(&mut self) {
        self.busy = Some("scanning…".into());
        let targets = self.targets.clone();
        let tx = self.tx.clone();
        thread::spawn(move || {
            let mut done = Vec::new();
            let report = spotless_core::scanner::scan_targets_with(
                &targets,
                |scan| {
                    let _ = tx.send(Msg::Progress(format!("scanning {}", scan.target.name)));
                },
                &|| false,
            );
            done.extend(report.targets);
            let _ = tx.send(Msg::ScanDone(done));
        });
    }

    fn start_dev(&mut self) {
        self.busy = Some("looking for build artifacts…".into());
        let tx = self.tx.clone();
        thread::spawn(move || {
            let Some(home) = spotless_core::paths::home_dir() else {
                let _ = tx.send(Msg::DevDone(Vec::new()));
                return;
            };
            let artifacts = spotless_core::devscan::scan_dev_artifacts_with(
                &home,
                spotless_core::DevScanOptions::default(),
                &|_| {},
                &|progress| {
                    let _ = tx.send(Msg::Progress(format!(
                        "{} folders · {} found · {}",
                        progress.dirs_scanned,
                        progress.found,
                        crate::ui::bytes(progress.bytes_found)
                    )));
                },
                &|| false,
            );
            let _ = tx.send(Msg::DevDone(artifacts));
        });
    }

    fn start_apps(&mut self) {
        self.busy = Some("measuring applications…".into());
        let tx = self.tx.clone();
        thread::spawn(move || {
            use spotless_core::apps;
            let mut installed = apps::list_apps(&apps::default_app_dirs());
            let paths: Vec<_> = installed.iter().map(|a| a.path.clone()).collect();
            let sizes: std::collections::HashMap<_, _> =
                apps::app_sizes(&paths).into_iter().collect();
            for app in &mut installed {
                app.size_bytes = sizes.get(&app.path).copied();
            }
            installed.sort_by_key(|a| std::cmp::Reverse(a.size_bytes.unwrap_or(0)));
            let _ = tx.send(Msg::AppsDone(installed));
        });
    }

    fn start_trash(&mut self) {
        self.busy = Some("measuring the Trash…".into());
        let tx = self.tx.clone();
        thread::spawn(move || {
            let _ = tx.send(Msg::TrashDone(spotless_core::trash::summary()));
        });
    }

    // --- destructive actions ----------------------------------------------

    fn ask_clean(&mut self) {
        if self.busy.is_some() {
            return;
        }
        let bytes = self.selected_bytes();
        match self.tab {
            Tab::Scan if self.scan.iter().any(|r| r.selected) => {
                self.confirm = Some(Confirm {
                    question: format!("Move {} to the Trash?", crate::ui::bytes(bytes)),
                    action: Action::CleanScan,
                });
            }
            Tab::Dev if self.dev.iter().any(|r| r.selected) => {
                self.confirm = Some(Confirm {
                    question: format!(
                        "Move {} of build artifacts to the Trash?",
                        crate::ui::bytes(bytes)
                    ),
                    action: Action::CleanDev,
                });
            }
            Tab::Scan | Tab::Dev => self.status = "Nothing selected — press Space first.".into(),
            _ => {}
        }
    }

    fn ask_uninstall(&mut self) {
        if self.tab != Tab::Apps || self.busy.is_some() {
            return;
        }
        let Some(app) = self.apps.get(self.cursor()) else {
            return;
        };
        self.confirm = Some(Confirm {
            question: format!("Uninstall {} and its leftovers?", app.name),
            action: Action::Uninstall,
        });
    }

    fn ask_empty(&mut self) {
        if self.tab != Tab::Trash {
            return;
        }
        // Nothing measured yet, or nothing there: either way there is no
        // question worth asking.
        let Some(trash) = self.trash.as_ref().filter(|t| t.items > 0) else {
            return;
        };
        self.confirm = Some(Confirm {
            question: format!(
                "Permanently delete {} in the Trash? This cannot be undone.",
                crate::ui::bytes(trash.bytes)
            ),
            action: Action::EmptyTrash,
        });
    }

    /// Carry out the confirmed action on a worker thread.
    fn commit(&mut self) {
        let Some(confirm) = self.confirm.take() else {
            return;
        };
        self.busy = Some("removing…".into());
        let tx = self.tx.clone();

        match confirm.action {
            Action::CleanScan => {
                let targets = self.targets.clone();
                let items: Vec<_> = self
                    .scan
                    .iter()
                    .filter(|r| r.selected)
                    .flat_map(|r| r.item.items.iter().cloned())
                    .collect();
                thread::spawn(move || {
                    let report = crate::cmd::clean::execute(&targets, &items, false, false);
                    let _ = tx.send(Msg::Finished(removed_line(&report)));
                });
            }
            Action::CleanDev => {
                let artifacts: Vec<_> = self
                    .dev
                    .iter()
                    .filter(|r| r.selected)
                    .map(|r| r.item.clone())
                    .collect();
                thread::spawn(move || {
                    let report = crate::cmd::dev::clean(&artifacts);
                    let _ = tx.send(Msg::Finished(removed_line(&report)));
                });
            }
            Action::Uninstall => {
                let Some(app) = self.apps.get(self.cursor()).cloned() else {
                    // Nothing under the cursor any more (the list was replaced
                    // while the question stood). Drop back out of "busy" rather
                    // than leaving a spinner nothing will ever clear.
                    self.finish("The app is no longer in the list.");
                    return;
                };
                thread::spawn(move || {
                    let Some(home) = spotless_core::paths::home_dir() else {
                        let _ = tx.send(Msg::Finished("cannot locate your home directory".into()));
                        return;
                    };
                    let plan = spotless_core::apps::plan_uninstall(app, &home);
                    let report = crate::cmd::apps::remove(&plan);
                    let _ = tx.send(Msg::Finished(removed_line(&report)));
                });
            }
            Action::EmptyTrash => {
                thread::spawn(move || {
                    let report = spotless_core::trash::empty();
                    let _ = tx.send(Msg::Finished(format!(
                        "Emptied {} from {}.",
                        crate::ui::bytes(report.bytes_reclaimed),
                        crate::ui::count(report.removed, "item", "items")
                    )));
                });
            }
        }
    }
}

/// The one-line summary of a finished removal.
fn removed_line(report: &spotless_core::CleanReport) -> String {
    let mut line = format!(
        "Reclaimed {} from {}.",
        crate::ui::bytes(report.bytes_reclaimed),
        crate::ui::count(report.removed.len(), "item", "items")
    );
    // Refusals and failures are the half of the outcome a user most needs to
    // see, so they go in the same line rather than into a log nobody opens.
    if !report.refused.is_empty() {
        line.push_str(&format!(" {} refused.", report.refused.len()));
    }
    if !report.failed.is_empty() {
        line.push_str(&format!(" {} failed.", report.failed.len()));
    }
    line
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tabs_wrap_in_both_directions() {
        assert_eq!(Tab::Scan.shifted(-1).title(), "Trash");
        assert_eq!(Tab::Trash.shifted(1).title(), "Scan");
    }

    #[test]
    fn removed_line_mentions_refusals() {
        let report = spotless_core::CleanReport {
            bytes_reclaimed: 1000,
            refused: vec![spotless_core::RefusedItem {
                path: "/x".into(),
                reason: "nope".into(),
            }],
            ..Default::default()
        };
        let line = removed_line(&report);
        assert!(line.contains("1.0 KB"), "{line}");
        assert!(line.contains("1 refused"), "{line}");
    }
}
