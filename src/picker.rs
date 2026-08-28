//! Popup TUI: pick file(s) to send. Type to filter, ↑/↓ move, Tab multi-select, Enter send, Esc cancel.

use std::collections::HashSet;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen};
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};
use ratatui::Terminal;
use regex::Regex;

use crate::herdr::{self, Ctx};
use crate::send::{self, SendOpts};
use crate::state::State;
use crate::util;

#[derive(Clone)]
struct Cand {
    path: PathBuf,
    rel: String,
    size: u64,
    mtime_ms: u64,
    is_dir: bool,
    origin: &'static str, // "sel" | "out" | "mtime" | "typed"
}

impl Cand {
    fn glyph(&self) -> &'static str {
        if self.is_dir {
            return "▸";
        }
        let m = util::mime_for(&self.rel);
        if m.starts_with("image/") {
            "🖼"
        } else if m == "application/pdf" {
            "📄"
        } else if self.rel.ends_with(".zip") || self.rel.ends_with(".tar.gz") || self.rel.ends_with(".tgz") {
            "📦"
        } else {
            " "
        }
    }
}

fn mtime_ms(p: &Path) -> u64 {
    std::fs::metadata(p)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn make_cand(base: &Path, p: PathBuf, origin: &'static str) -> Option<Cand> {
    let md = std::fs::metadata(&p).ok()?;
    let rel = p.strip_prefix(base).map(|r| r.display().to_string()).unwrap_or_else(|_| p.display().to_string());
    Some(Cand { rel, size: md.len(), mtime_ms: mtime_ms(&p), is_dir: md.is_dir(), path: p, origin })
}

fn gather(ctx: &Ctx, base: &Path) -> Vec<Cand> {
    let mut seen: HashSet<PathBuf> = HashSet::new();
    let mut out: Vec<Cand> = Vec::new();
    let mut push = |p: PathBuf, origin: &'static str, out: &mut Vec<Cand>| {
        let canon = p.canonicalize().unwrap_or(p);
        if canon == base || !seen.insert(canon.clone()) {
            return;
        }
        if let Some(c) = make_cand(base, canon, origin) {
            out.push(c);
        }
    };

    // 1. selection
    if let Some(sel) = ctx.selected_text() {
        for tok in sel.split_whitespace() {
            if let Some(p) = herdr::path_exists_rel(base, tok.trim_matches(|c| "\"'`,;:()[]<>".contains(c))) {
                push(p, "sel", &mut out);
            }
        }
    }
    // 2. paths mentioned in recent pane output
    if let Some(pane) = &ctx.pane_id {
        if let Ok(text) = ctx.pane_read(pane, 300) {
            let re = Regex::new(r"(?:~|\.{1,2})?/?[\w@%+=.\-]+(?:/[\w@%+=.\-]+)+|[\w@%+=\-]+\.[A-Za-z0-9]{1,6}").unwrap();
            let mut found: Vec<PathBuf> = Vec::new();
            for m in re.find_iter(&text) {
                let tok = m.as_str().trim_end_matches(|c| ".,;:)".contains(c));
                if tok.len() < 3 || tok.starts_with("http") {
                    continue;
                }
                if let Some(p) = herdr::path_exists_rel(base, tok) {
                    if p.is_file() || p.is_dir() {
                        found.push(p);
                    }
                }
            }
            // most recently mentioned first
            for p in found.into_iter().rev() {
                push(p, "out", &mut out);
            }
        }
    }
    // 3. recently modified under cwd (30 min, depth ≤ 4, gitignore-aware)
    let cutoff = util::now_ms().saturating_sub(30 * 60 * 1000);
    let mut recent: Vec<(u64, PathBuf)> = Vec::new();
    let walker = ignore::WalkBuilder::new(base)
        .max_depth(Some(4))
        .hidden(true)
        .git_ignore(true)
        .filter_entry(|e| {
            let n = e.file_name().to_string_lossy();
            n != "node_modules" && n != "target" && n != ".git"
        })
        .build();
    for e in walker.flatten() {
        if e.depth() == 0 || !e.file_type().map(|t| t.is_file()).unwrap_or(false) {
            continue;
        }
        let m = mtime_ms(e.path());
        if m >= cutoff {
            recent.push((m, e.path().to_path_buf()));
        }
        if recent.len() > 5000 {
            break;
        }
    }
    recent.sort_by(|a, b| b.0.cmp(&a.0));
    for (_, p) in recent.into_iter().take(500) {
        push(p, "mtime", &mut out);
    }
    out
}

/// Subsequence fuzzy match; lower score = better. None = no match.
fn fuzzy(q: &str, s: &str) -> Option<u32> {
    if q.is_empty() {
        return Some(0);
    }
    let s_l = s.to_lowercase();
    let q_l = q.to_lowercase();
    if let Some(i) = s_l.find(&q_l) {
        return Some(i as u32);
    }
    let mut score = 100;
    let mut it = s_l.chars();
    for qc in q_l.chars() {
        loop {
            match it.next() {
                Some(c) if c == qc => break,
                Some(_) => score += 1,
                None => return None,
            }
        }
    }
    Some(score)
}

struct App {
    all: Vec<Cand>,
    shown: Vec<usize>,
    query: String,
    cursor: usize,
    selected: HashSet<PathBuf>,
    base: PathBuf,
    typed: Option<Cand>,
}

impl App {
    fn refilter(&mut self) {
        let mut scored: Vec<(u32, usize)> =
            self.all.iter().enumerate().filter_map(|(i, c)| fuzzy(&self.query, &c.rel).map(|s| (s, i))).collect();
        scored.sort_by(|a, b| a.0.cmp(&b.0).then(self.all[b.1].mtime_ms.cmp(&self.all[a.1].mtime_ms)));
        self.shown = scored.into_iter().map(|(_, i)| i).collect();
        self.typed = if self.shown.is_empty() && !self.query.trim().is_empty() {
            herdr::path_exists_rel(&self.base, self.query.trim()).and_then(|p| make_cand(&self.base, p, "typed"))
        } else {
            None
        };
        self.cursor = 0;
    }

    fn current(&self) -> Option<Cand> {
        if let Some(t) = &self.typed {
            return Some(t.clone());
        }
        self.shown.get(self.cursor).map(|&i| self.all[i].clone())
    }

    fn targets(&self) -> Vec<PathBuf> {
        if !self.selected.is_empty() {
            let mut v: Vec<PathBuf> = self.selected.iter().cloned().collect();
            v.sort();
            return v;
        }
        self.current().map(|c| vec![c.path]).unwrap_or_default()
    }
}

pub fn run(state: &State, ctx: &Ctx, list_only: bool) -> Result<u8> {
    let base = ctx.pane_cwd();
    let all = gather(ctx, &base);
    if list_only {
        println!("base: {}", base.display());
        for c in &all {
            println!("{}\t{}\t{}\t{}", c.origin, c.rel, if c.is_dir { "dir".into() } else { util::human_size(c.size) }, util::human_age(c.mtime_ms));
        }
        return Ok(0);
    }
    let mut app = App { all, shown: vec![], query: String::new(), cursor: 0, selected: HashSet::new(), base: base.clone(), typed: None };
    app.refilter();

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    crossterm::execute!(stdout, EnterAlternateScreen)?;
    let mut term = Terminal::new(ratatui::backend::CrosstermBackend::new(stdout))?;
    let result = ui_loop(&mut term, &mut app);
    disable_raw_mode()?;
    crossterm::execute!(term.backend_mut(), LeaveAlternateScreen)?;
    term.show_cursor()?;

    let targets = match result? {
        Some(t) if !t.is_empty() => t,
        _ => return Ok(0),
    };
    let opts = SendOpts { note: None, clip: false, force: false, allow_outside_home: false };
    match send::send(state, ctx, &targets, &opts) {
        Ok(ids) => {
            for (p, id) in targets.iter().zip(ids) {
                println!("queued {} → {id}", p.display());
            }
            std::thread::sleep(Duration::from_millis(700));
            Ok(0)
        }
        Err(e) => {
            eprintln!("herdr-ferry: {e:#}");
            eprintln!("(press any key)");
            let _ = enable_raw_mode();
            let _ = event::read();
            let _ = disable_raw_mode();
            Ok(1)
        }
    }
}

type Term = Terminal<ratatui::backend::CrosstermBackend<io::Stdout>>;

fn ui_loop(term: &mut Term, app: &mut App) -> Result<Option<Vec<PathBuf>>> {
    let mut list_state = ListState::default();
    loop {
        list_state.select(if app.shown.is_empty() { None } else { Some(app.cursor) });
        term.draw(|f| {
            let [top, mid, bot] = Layout::vertical([Constraint::Length(1), Constraint::Min(1), Constraint::Length(1)]).areas(f.area());
            let base = app.base.display().to_string();
            let header = Line::from(vec![
                Span::styled("ferry ", Style::default().add_modifier(Modifier::BOLD)),
                Span::styled(base, Style::default().fg(Color::DarkGray)),
                Span::raw("  › "),
                Span::raw(app.query.clone()),
                Span::styled("▏", Style::default().fg(Color::Cyan)),
            ]);
            f.render_widget(Paragraph::new(header), top);

            let items: Vec<ListItem> = if let Some(t) = &app.typed {
                vec![row(t, false, "typed")]
            } else if app.shown.is_empty() {
                vec![ListItem::new(Span::styled("  no matches — type a path", Style::default().fg(Color::DarkGray)))]
            } else {
                app.shown
                    .iter()
                    .map(|&i| {
                        let c = &app.all[i];
                        row(c, app.selected.contains(&c.path), c.origin)
                    })
                    .collect()
            };
            let list = List::new(items)
                .block(Block::default().borders(Borders::TOP).border_style(Style::default().fg(Color::DarkGray)))
                .highlight_style(Style::default().bg(Color::Blue).fg(Color::White));
            f.render_stateful_widget(list, mid, &mut list_state);

            let n = app.selected.len();
            let help = if n > 0 {
                format!("Enter send {n} selected  Tab toggle  Esc cancel")
            } else {
                "Enter send  Tab multi-select  ↑↓ move  Esc cancel".to_string()
            };
            f.render_widget(Paragraph::new(Span::styled(help, Style::default().fg(Color::DarkGray))), bot);
        })?;

        if !event::poll(Duration::from_millis(250))? {
            continue;
        }
        if let Event::Key(k) = event::read()? {
            if k.kind != KeyEventKind::Press {
                continue;
            }
            match (k.code, k.modifiers) {
                (KeyCode::Esc, _) | (KeyCode::Char('c'), KeyModifiers::CONTROL) => return Ok(None),
                (KeyCode::Enter, _) => {
                    let t = app.targets();
                    if !t.is_empty() {
                        return Ok(Some(t));
                    }
                }
                (KeyCode::Tab, _) => {
                    if let Some(c) = app.current() {
                        if !app.selected.remove(&c.path) {
                            app.selected.insert(c.path);
                        }
                        if app.cursor + 1 < app.shown.len() {
                            app.cursor += 1;
                        }
                    }
                }
                (KeyCode::Up, _) | (KeyCode::Char('p'), KeyModifiers::CONTROL) => app.cursor = app.cursor.saturating_sub(1),
                (KeyCode::Down, _) | (KeyCode::Char('n'), KeyModifiers::CONTROL) => {
                    if app.cursor + 1 < app.shown.len() {
                        app.cursor += 1;
                    }
                }
                (KeyCode::Backspace, _) => {
                    app.query.pop();
                    app.refilter();
                }
                (KeyCode::Char('u'), KeyModifiers::CONTROL) => {
                    app.query.clear();
                    app.refilter();
                }
                (KeyCode::Char(ch), m) if !m.contains(KeyModifiers::CONTROL) => {
                    app.query.push(ch);
                    app.refilter();
                }
                _ => {}
            }
        }
    }
}

fn row<'a>(c: &Cand, selected: bool, origin: &str) -> ListItem<'a> {
    let mark = if selected { "●" } else { " " };
    let tag = match origin {
        "sel" => "selection",
        "out" => "mentioned",
        "typed" => "typed",
        _ => "",
    };
    let size = if c.is_dir { "dir".to_string() } else { util::human_size(c.size) };
    ListItem::new(Line::from(vec![
        Span::raw(format!("{mark} {} ", c.glyph())),
        Span::raw(c.rel.clone()),
        Span::styled(format!("  {size}  {}  ", util::human_age(c.mtime_ms)), Style::default().fg(Color::DarkGray)),
        Span::styled(tag.to_string(), Style::default().fg(Color::Yellow)),
    ]))
}
