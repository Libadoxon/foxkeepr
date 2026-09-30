//! Bookkeeping of which browser window sits on which workspace, and the
//! decision of when a newly opened window is a *restored* one that should be
//! put back.
//!
//! Two rules keep the daemon out of the user's way:
//!
//! * a window is only ever moved once, at the moment it is recognised as the
//!   continuation of a window from a previous session;
//! * once a window is resolved, every move the user makes is simply recorded.
//!
//! Restoring is armed only when the browser is absent (daemon start with no
//! windows, or the last window disappearing), so opening an extra window
//! during normal use never triggers a move.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use tracing::debug;

use crate::event::Event;
use crate::hypr::Client;
use crate::state::{Record, State};

#[derive(Debug, Clone)]
pub struct Config {
    /// Window classes treated as the browser (compared case-insensitively).
    pub classes: Vec<String>,
    /// How long after the first restored window appears we keep matching.
    pub grace: Duration,
    /// How long to let a window's title settle before giving up on an exact
    /// title match. Firefox opens windows titled just "Firefox" and fills the
    /// real title in a moment later.
    pub settle: Duration,
    /// How long the set of windows must hold still before we trust it as the
    /// layout worth persisting. This is what stops a crash — every window
    /// closing at once — from overwriting the layout with a partial one.
    pub stability: Duration,
    /// Fall back to matching leftover windows by position when titles differ.
    pub order_fallback: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            classes: DEFAULT_CLASSES.iter().map(|s| s.to_string()).collect(),
            grace: Duration::from_secs(60),
            settle: Duration::from_secs(8),
            stability: Duration::from_secs(5),
            order_fallback: true,
        }
    }
}

pub const DEFAULT_CLASSES: &[&str] = &[
    "firefox",
    "firefox-esr",
    "firefoxdeveloperedition",
    "floorp",
    "librewolf",
    "waterfox",
    "zen",
    "zen-alpha",
    "navigator",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Move { addr: String, workspace: String },
}

#[derive(Debug, Clone)]
struct Live {
    class: String,
    title: String,
    workspace: String,
    seq: Option<u64>,
    opened_at: Instant,
    resolved: bool,
}

pub struct Tracker {
    cfg: Config,
    live: HashMap<String, Live>,
    /// Last layout we are confident about; this is what gets persisted.
    session: Vec<Record>,
    next_seq: u64,
    /// Records still waiting for a window to claim them.
    pending: Vec<Record>,
    restore_deadline: Option<Instant>,
    membership_changed_at: Instant,
    dirty: bool,
    save_due: bool,
}

impl Tracker {
    pub fn new(cfg: Config, state: State, now: Instant) -> Self {
        let next_seq = state
            .next_seq
            .max(state.session.iter().map(|r| r.seq + 1).max().unwrap_or(0));
        Self {
            cfg,
            live: HashMap::new(),
            session: state.session,
            next_seq,
            pending: Vec::new(),
            restore_deadline: None,
            membership_changed_at: now,
            dirty: false,
            save_due: false,
        }
    }

    pub fn is_browser(&self, class: &str) -> bool {
        self.cfg
            .classes
            .iter()
            .any(|c| c.eq_ignore_ascii_case(class))
    }

    /// Take stock of the windows that already exist. Windows found here are
    /// adopted as-is and never moved: if the browser is running, this is a
    /// daemon restart, not a recovery.
    pub fn adopt(&mut self, clients: &[Client], now: Instant) {
        let browsers: Vec<&Client> = clients
            .iter()
            .filter(|c| self.is_browser(&c.class))
            .collect();

        if browsers.is_empty() {
            // Nothing running: whatever we have on disk is a layout to restore.
            self.arm();
            return;
        }

        let mut unclaimed = self.session.clone();
        unclaimed.sort_by_key(|r| r.seq);
        for client in browsers {
            let seq = match unclaimed.iter().position(|r| {
                r.title == client.title && r.class.eq_ignore_ascii_case(&client.class)
            }) {
                Some(i) => unclaimed.remove(i).seq,
                None => self.alloc_seq(),
            };
            self.live.insert(
                crate::event::normalize_addr(&client.address),
                Live {
                    class: client.class.clone(),
                    title: client.title.clone(),
                    workspace: client.workspace.name.clone(),
                    seq: Some(seq),
                    opened_at: now,
                    resolved: true,
                },
            );
        }
        self.membership_changed_at = now;
        self.dirty = true;
    }

    pub fn on_event(&mut self, ev: Event, now: Instant) -> Vec<Action> {
        match ev {
            Event::Open {
                addr,
                workspace,
                class,
                title,
            } => {
                if !self.is_browser(&class) {
                    return Vec::new();
                }
                let restoring = !self.pending.is_empty();
                if restoring && self.restore_deadline.is_none() {
                    self.restore_deadline = Some(now + self.cfg.grace);
                }
                let seq = if restoring {
                    None
                } else {
                    Some(self.alloc_seq())
                };
                self.live.insert(
                    addr.clone(),
                    Live {
                        class,
                        title,
                        workspace,
                        seq,
                        opened_at: now,
                        resolved: !restoring,
                    },
                );
                self.membership_changed_at = now;
                self.dirty = true;
                self.resolve(&addr, false).into_iter().collect()
            }
            Event::Close { addr } => {
                if self.live.remove(&addr).is_some() {
                    self.membership_changed_at = now;
                    self.dirty = true;
                    if self.live.is_empty() {
                        // The browser is gone. Keep the layout we already
                        // persisted and wait for the windows to come back.
                        self.arm();
                    }
                }
                Vec::new()
            }
            Event::Move { addr, workspace } => {
                // Covers the user dragging a window as well as our own move.
                if let Some(live) = self.live.get_mut(&addr)
                    && live.workspace != workspace
                {
                    live.workspace = workspace;
                    self.dirty = true;
                }
                Vec::new()
            }
            Event::Title { addr, title } => {
                let mut unresolved = false;
                if let Some(live) = self.live.get_mut(&addr) {
                    if live.title != title {
                        live.title = title;
                        self.dirty = true;
                    }
                    unresolved = !live.resolved;
                }
                if unresolved {
                    self.resolve(&addr, false).into_iter().collect()
                } else {
                    Vec::new()
                }
            }
        }
    }

    pub fn tick(&mut self, now: Instant) -> Vec<Action> {
        let mut actions = Vec::new();

        if self.restore_deadline.is_some_and(|dl| now >= dl) {
            debug!(
                unclaimed = self.pending.len(),
                "restore window closed; no longer matching"
            );
            self.pending.clear();
            self.restore_deadline = None;
        }

        let mut waiting: Vec<(Instant, String)> = self
            .live
            .iter()
            .filter(|(_, l)| !l.resolved)
            .map(|(addr, l)| (l.opened_at, addr.clone()))
            .collect();
        waiting.sort();
        for (opened_at, addr) in waiting {
            let force = opened_at + self.cfg.settle <= now || self.pending.is_empty();
            actions.extend(self.resolve(&addr, force));
        }

        self.snapshot(now);
        actions
    }

    /// Match a window against the pending layout. With `force`, take the first
    /// leftover record (position matching) or, failing that, accept the window
    /// as new.
    fn resolve(&mut self, addr: &str, force: bool) -> Option<Action> {
        let (class, title, workspace) = match self.live.get(addr) {
            Some(l) if !l.resolved => (l.class.clone(), l.title.clone(), l.workspace.clone()),
            _ => return None,
        };

        let by_title = self
            .pending
            .iter()
            .position(|r| r.title == title && r.class.eq_ignore_ascii_case(&class));
        let matched_on = if by_title.is_some() {
            "title"
        } else {
            "position"
        };
        let pick = match by_title {
            Some(i) => Some(i),
            None if force && self.cfg.order_fallback => self
                .pending
                .iter()
                .position(|r| r.class.eq_ignore_ascii_case(&class)),
            None => None,
        };

        match pick {
            Some(i) => {
                let record = self.pending.remove(i);
                debug!(
                    %addr,
                    %title,
                    %matched_on,
                    from = %workspace,
                    to = %record.workspace,
                    "recognised a restored window"
                );
                let live = self.live.get_mut(addr)?;
                live.seq = Some(record.seq);
                live.resolved = true;
                self.dirty = true;
                if record.workspace != workspace {
                    live.workspace = record.workspace.clone();
                    return Some(Action::Move {
                        addr: addr.to_string(),
                        workspace: record.workspace,
                    });
                }
                None
            }
            None if force => {
                debug!(%addr, %title, workspace = %workspace, "new window, left where it is");
                let seq = self.alloc_seq();
                let live = self.live.get_mut(addr)?;
                live.seq = Some(seq);
                live.resolved = true;
                self.dirty = true;
                None
            }
            None => None,
        }
    }

    /// Persist the live layout once it has held still, and never while the
    /// browser is absent — the layout of "no windows" is not worth keeping.
    fn snapshot(&mut self, now: Instant) {
        if !self.dirty || self.live.is_empty() {
            return;
        }
        if now < self.membership_changed_at + self.cfg.stability {
            return;
        }
        if self.live.values().any(|l| !l.resolved) {
            return;
        }
        let mut records: Vec<Record> = self
            .live
            .values()
            .filter_map(|l| {
                Some(Record {
                    seq: l.seq?,
                    class: l.class.clone(),
                    title: l.title.clone(),
                    workspace: l.workspace.clone(),
                })
            })
            .collect();
        records.sort_by_key(|r| r.seq);
        if records != self.session {
            debug!(windows = records.len(), "layout changed; saving");
            self.session = records;
            self.save_due = true;
        }
        self.dirty = false;
    }

    fn arm(&mut self) {
        self.pending = self.session.clone();
        self.pending.sort_by_key(|r| r.seq);
        self.restore_deadline = None;
        debug!(
            records = self.pending.len(),
            "browser absent; armed for restore"
        );
    }

    fn alloc_seq(&mut self) -> u64 {
        let seq = self.next_seq;
        self.next_seq += 1;
        seq
    }

    pub fn state(&self) -> State {
        State {
            next_seq: self.next_seq,
            session: self.session.clone(),
        }
    }

    pub fn take_save(&mut self) -> bool {
        std::mem::take(&mut self.save_due)
    }

    pub fn pending_len(&self) -> usize {
        self.pending.len()
    }
}

/// One-shot matching used by the `restore` subcommand.
pub fn plan_restore(cfg: &Config, session: &[Record], clients: &[Client]) -> Vec<Action> {
    let mut unclaimed: Vec<Record> = session.to_vec();
    unclaimed.sort_by_key(|r| r.seq);

    let mut windows: Vec<&Client> = clients
        .iter()
        .filter(|c| cfg.classes.iter().any(|k| k.eq_ignore_ascii_case(&c.class)))
        .collect();
    windows.sort_by(|a, b| a.address.cmp(&b.address));

    let mut actions = Vec::new();
    let mut leftovers = Vec::new();

    for client in windows {
        match unclaimed
            .iter()
            .position(|r| r.title == client.title && r.class.eq_ignore_ascii_case(&client.class))
        {
            Some(i) => {
                let record = unclaimed.remove(i);
                if record.workspace != client.workspace.name {
                    actions.push(Action::Move {
                        addr: crate::event::normalize_addr(&client.address),
                        workspace: record.workspace,
                    });
                }
            }
            None => leftovers.push(client),
        }
    }

    if cfg.order_fallback {
        for client in leftovers {
            let Some(i) = unclaimed
                .iter()
                .position(|r| r.class.eq_ignore_ascii_case(&client.class))
            else {
                continue;
            };
            let record = unclaimed.remove(i);
            if record.workspace != client.workspace.name {
                actions.push(Action::Move {
                    addr: crate::event::normalize_addr(&client.address),
                    workspace: record.workspace,
                });
            }
        }
    }

    actions
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> Config {
        Config {
            classes: vec!["floorp".into()],
            ..Config::default()
        }
    }

    fn rec(seq: u64, title: &str, ws: &str) -> Record {
        Record {
            seq,
            class: "floorp".into(),
            title: title.into(),
            workspace: ws.into(),
        }
    }

    fn saved(records: Vec<Record>) -> State {
        State {
            next_seq: records.iter().map(|r| r.seq + 1).max().unwrap_or(0),
            session: records,
        }
    }

    fn open(addr: &str, ws: &str, title: &str) -> Event {
        Event::Open {
            addr: addr.into(),
            workspace: ws.into(),
            class: "floorp".into(),
            title: title.into(),
        }
    }

    #[test]
    fn restores_windows_to_their_saved_workspaces_after_a_crash() {
        let t0 = Instant::now();
        let state = saved(vec![rec(0, "Mail", "2"), rec(1, "Docs", "special:mail")]);
        let mut tr = Tracker::new(cfg(), state, t0);
        tr.adopt(&[], t0); // browser not running -> arm restore

        // Firefox restores both windows onto workspace 1.
        let a = tr.on_event(open("0xa", "1", "Mail"), t0);
        let b = tr.on_event(open("0xb", "1", "Docs"), t0);

        assert_eq!(
            a,
            vec![Action::Move {
                addr: "0xa".into(),
                workspace: "2".into()
            }]
        );
        assert_eq!(
            b,
            vec![Action::Move {
                addr: "0xb".into(),
                workspace: "special:mail".into()
            }]
        );
    }

    #[test]
    fn waits_for_the_title_to_settle_before_matching() {
        let t0 = Instant::now();
        let mut tr = Tracker::new(cfg(), saved(vec![rec(0, "Docs — Floorp", "7")]), t0);
        tr.adopt(&[], t0);

        // Opens with the placeholder title Firefox uses before restoring a tab.
        assert!(tr.on_event(open("0xa", "1", "Floorp"), t0).is_empty());
        assert!(tr.tick(t0 + Duration::from_secs(1)).is_empty());

        let acted = tr.on_event(
            Event::Title {
                addr: "0xa".into(),
                title: "Docs — Floorp".into(),
            },
            t0 + Duration::from_secs(2),
        );
        assert_eq!(
            acted,
            vec![Action::Move {
                addr: "0xa".into(),
                workspace: "7".into()
            }]
        );
    }

    #[test]
    fn falls_back_to_position_when_the_title_changed() {
        let t0 = Instant::now();
        let mut tr = Tracker::new(cfg(), saved(vec![rec(0, "Old title", "9")]), t0);
        tr.adopt(&[], t0);
        assert!(
            tr.on_event(open("0xa", "1", "Brand new title"), t0)
                .is_empty()
        );

        let acted = tr.tick(t0 + Duration::from_secs(9));
        assert_eq!(
            acted,
            vec![Action::Move {
                addr: "0xa".into(),
                workspace: "9".into()
            }]
        );
    }

    #[test]
    fn position_fallback_hands_out_records_oldest_window_first() {
        let t0 = Instant::now();
        let mut tr = Tracker::new(
            cfg(),
            saved(vec![rec(0, "was first", "8"), rec(1, "was second", "9")]),
            t0,
        );
        tr.adopt(&[], t0);

        // 0xb opens first, so it must claim the lower-seq record even though
        // 0xa sorts earlier by address.
        tr.on_event(open("0xb", "1", "unrecognised"), t0);
        tr.on_event(
            open("0xa", "1", "also unrecognised"),
            t0 + Duration::from_secs(1),
        );

        let acted = tr.tick(t0 + Duration::from_secs(10));
        assert_eq!(
            acted,
            vec![
                Action::Move {
                    addr: "0xb".into(),
                    workspace: "8".into()
                },
                Action::Move {
                    addr: "0xa".into(),
                    workspace: "9".into()
                },
            ]
        );
    }

    #[test]
    fn windows_opened_together_fall_back_in_address_order() {
        let t0 = Instant::now();
        let mut tr = Tracker::new(
            cfg(),
            saved(vec![rec(0, "first", "8"), rec(1, "second", "9")]),
            t0,
        );
        tr.adopt(&[], t0);
        tr.on_event(open("0xb", "1", "unrecognised"), t0);
        tr.on_event(open("0xa", "1", "also unrecognised"), t0);

        let acted = tr.tick(t0 + Duration::from_secs(10));
        assert_eq!(
            acted,
            vec![
                Action::Move {
                    addr: "0xa".into(),
                    workspace: "8".into()
                },
                Action::Move {
                    addr: "0xb".into(),
                    workspace: "9".into()
                },
            ]
        );
    }

    #[test]
    fn position_fallback_can_be_disabled() {
        let t0 = Instant::now();
        let cfg = Config {
            order_fallback: false,
            ..cfg()
        };
        let mut tr = Tracker::new(cfg, saved(vec![rec(0, "Old title", "9")]), t0);
        tr.adopt(&[], t0);
        tr.on_event(open("0xa", "1", "Brand new title"), t0);
        assert!(tr.tick(t0 + Duration::from_secs(9)).is_empty());
    }

    #[test]
    fn a_window_opened_during_normal_use_is_never_moved() {
        let t0 = Instant::now();
        let existing = Client {
            address: "0xa".into(),
            class: "floorp".into(),
            title: "Mail".into(),
            workspace: crate::hypr::Workspace { name: "2".into() },
        };
        let mut tr = Tracker::new(cfg(), saved(vec![rec(0, "Mail", "2")]), t0);
        tr.adopt(&[existing], t0); // browser already running -> no restore armed

        let acted = tr.on_event(open("0xb", "5", "Something else"), t0);
        assert!(acted.is_empty());
        assert!(tr.tick(t0 + Duration::from_secs(30)).is_empty());
    }

    #[test]
    fn the_user_moving_a_window_is_recorded_not_undone() {
        let t0 = Instant::now();
        let existing = Client {
            address: "0xa".into(),
            class: "floorp".into(),
            title: "Mail".into(),
            workspace: crate::hypr::Workspace { name: "2".into() },
        };
        let mut tr = Tracker::new(cfg(), saved(vec![rec(0, "Mail", "2")]), t0);
        tr.adopt(&[existing], t0);

        let acted = tr.on_event(
            Event::Move {
                addr: "0xa".into(),
                workspace: "6".into(),
            },
            t0,
        );
        assert!(acted.is_empty(), "the daemon must not fight the user");

        tr.tick(t0 + Duration::from_secs(10));
        assert_eq!(tr.state().session[0].workspace, "6");
    }

    #[test]
    fn a_crash_does_not_overwrite_the_layout() {
        let t0 = Instant::now();
        let clients: Vec<Client> = [("0xa", "Mail", "2"), ("0xb", "Docs", "4")]
            .iter()
            .map(|(addr, title, ws)| Client {
                address: (*addr).into(),
                class: "floorp".into(),
                title: (*title).into(),
                workspace: crate::hypr::Workspace { name: (*ws).into() },
            })
            .collect();
        let mut tr = Tracker::new(cfg(), State::default(), t0);
        tr.adopt(&clients, t0);
        tr.tick(t0 + Duration::from_secs(6));
        assert_eq!(tr.state().session.len(), 2);

        // Both windows vanish at once.
        let t1 = t0 + Duration::from_secs(10);
        tr.on_event(Event::Close { addr: "0xa".into() }, t1);
        tr.on_event(Event::Close { addr: "0xb".into() }, t1);
        tr.tick(t1 + Duration::from_secs(60));

        let session = tr.state().session;
        assert_eq!(session.len(), 2, "layout must survive the crash");
        assert_eq!(tr.pending_len(), 2, "and be armed for restore");
    }

    #[test]
    fn closing_one_window_deliberately_drops_it_from_the_layout() {
        let t0 = Instant::now();
        let clients: Vec<Client> = [("0xa", "Mail", "2"), ("0xb", "Docs", "4")]
            .iter()
            .map(|(addr, title, ws)| Client {
                address: (*addr).into(),
                class: "floorp".into(),
                title: (*title).into(),
                workspace: crate::hypr::Workspace { name: (*ws).into() },
            })
            .collect();
        let mut tr = Tracker::new(cfg(), State::default(), t0);
        tr.adopt(&clients, t0);
        tr.tick(t0 + Duration::from_secs(6));

        let t1 = t0 + Duration::from_secs(10);
        tr.on_event(Event::Close { addr: "0xb".into() }, t1);
        tr.tick(t1 + Duration::from_secs(6));

        let session = tr.state().session;
        assert_eq!(session.len(), 1);
        assert_eq!(session[0].title, "Mail");
    }

    #[test]
    fn restore_stops_after_the_grace_period() {
        let t0 = Instant::now();
        let mut tr = Tracker::new(cfg(), saved(vec![rec(0, "Mail", "2")]), t0);
        tr.adopt(&[], t0);
        tr.on_event(open("0xa", "1", "Mail"), t0);

        // A window opened long after the restore burst keeps its workspace.
        let late = t0 + Duration::from_secs(120);
        tr.tick(late);
        let acted = tr.on_event(open("0xc", "3", "Mail"), late);
        assert!(acted.is_empty());
    }

    #[test]
    fn plan_restore_matches_by_title_then_position() {
        let clients: Vec<Client> = [("0xa", "Mail", "1"), ("0xb", "Untitled", "1")]
            .iter()
            .map(|(addr, title, ws)| Client {
                address: (*addr).into(),
                class: "floorp".into(),
                title: (*title).into(),
                workspace: crate::hypr::Workspace { name: (*ws).into() },
            })
            .collect();
        let session = vec![rec(0, "Docs", "8"), rec(1, "Mail", "2")];
        let actions = plan_restore(&cfg(), &session, &clients);
        assert_eq!(
            actions,
            vec![
                Action::Move {
                    addr: "0xa".into(),
                    workspace: "2".into()
                },
                Action::Move {
                    addr: "0xb".into(),
                    workspace: "8".into()
                },
            ]
        );
    }
}
