//! The interactive settings screen.
//!
//! Everything here is also reachable through flags and `config.toml`, so this is
//! not the only way to configure anything. It exists because the settings that
//! matter most — autosave, auto-recall — are the ones you want to *see* the
//! effect of: a toggle that says "on" while no hook is wired into any agent is
//! worse than no toggle at all. So the Agents pane reads the real config files
//! back, and flipping autosave rewrites them.
//!
//! Two panes:
//!
//! - **Settings** — toggles and numbers, saved on `s`.
//! - **Memories** — what is actually stored, newest first, searchable, with
//!   `x` to retract. Reading the store is the fastest way to find out whether
//!   autosave is keeping the right things.

use anyhow::{Context, Result};
use ratatui::crossterm::event::{self, Event as TermEvent, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::crossterm::ExecutableCommand;
use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Tabs, Wrap};
use rusqlite::Connection;
use std::time::{Duration, Instant};

use crate::config::Config;
use crate::embed::Embedder;
use crate::pack;
use crate::retrieve::{self, Query};
use crate::store::{self, Stats};
use crate::{db, install, scope};

pub fn run(cfg: Config) -> Result<()> {
    let mut app = App::new(cfg)?;
    let mut term = enter()?;
    let outcome = app.main_loop(&mut term);
    leave(&mut term)?;
    // Report after the screen is restored, or the message scrolls away with it.
    match &outcome {
        Ok(Some(msg)) => println!("{msg}"),
        Ok(None) => {}
        Err(_) => {}
    }
    outcome.map(|_| ())
}

type Term = Terminal<CrosstermBackend<std::io::Stdout>>;

fn enter() -> Result<Term> {
    enable_raw_mode().context("this terminal does not support raw mode")?;
    let mut out = std::io::stdout();
    out.execute(EnterAlternateScreen)?;
    Ok(Terminal::new(CrosstermBackend::new(out))?)
}

fn leave(term: &mut Term) -> Result<()> {
    disable_raw_mode()?;
    term.backend_mut().execute(LeaveAlternateScreen)?;
    term.show_cursor()?;
    Ok(())
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Pane {
    Settings,
    Memories,
}

/// One editable setting. Kept as an enum rather than boxed closures so the list,
/// the renderer and the key handler can never drift out of sync.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Item {
    Autosave,
    AutosaveScope,
    AutosaveMinChars,
    AutosaveFacts,
    Redact,
    Autorecall,
    AutorecallLimit,
    AutorecallBudget,
    Semantic,
    Fast,
    Budget,
}

const ITEMS: &[Item] = &[
    Item::Autosave,
    Item::AutosaveScope,
    Item::AutosaveMinChars,
    Item::AutosaveFacts,
    Item::Redact,
    Item::Autorecall,
    Item::AutorecallLimit,
    Item::AutorecallBudget,
    Item::Semantic,
    Item::Fast,
    Item::Budget,
];

impl Item {
    fn label(self) -> &'static str {
        match self {
            Item::Autosave => "autosave",
            Item::AutosaveScope => "  scope",
            Item::AutosaveMinChars => "  min length",
            Item::AutosaveFacts => "  derive facts",
            Item::Redact => "  redact secrets",
            Item::Autorecall => "auto-recall",
            Item::AutorecallLimit => "  memories",
            Item::AutorecallBudget => "  token budget",
            Item::Semantic => "semantic search",
            Item::Fast => "fast embed cache",
            Item::Budget => "recall budget",
        }
    }

    /// The config key an environment variable would lock, if any.
    fn key(self) -> &'static str {
        match self {
            Item::Autosave => "autosave.enabled",
            Item::Redact => "autosave.redact",
            Item::Autorecall => "autorecall.enabled",
            Item::Semantic => "semantic",
            Item::Fast => "fast",
            Item::Budget => "budget_tokens",
            _ => "",
        }
    }

    fn help(self) -> &'static str {
        match self {
            Item::Autosave => {
                "Store every prompt you send, without the agent having to call `remember`. \
                 Turning this on wires a hook into each agent that supports one."
            }
            Item::AutosaveScope => {
                "project: memories belong to the repo you are in. global: one shared pile."
            }
            Item::AutosaveMinChars => {
                "Prompts shorter than this are ignored — 'ok' and 'continue' are not memories."
            }
            Item::AutosaveFacts => {
                "Promote prompts that read like rules ('never force push', 'usamos pnpm') into \
                 the fact graph. Everything else is still kept as a raw episode."
            }
            Item::Redact => {
                "Replace anything that looks like a token, key or password with [redacted] \
                 before it is written."
            }
            Item::Autorecall => {
                "Search memory on every prompt and hand the results to the agent as context, \
                 so recall happens even when the model doesn't think to ask."
            }
            Item::AutorecallLimit => "How many memories may be injected per prompt.",
            Item::AutorecallBudget => "Hard cap on the size of that injection, in tokens.",
            Item::Semantic => {
                "Use embeddings alongside keyword search. Off is BM25 only — faster to \
                 install, worse at paraphrase."
            }
            Item::Fast => {
                "Load embeddings from the mmap'd cache: ~1 ms instead of ~206 ms per process. \
                 Verified against the real model before it is ever used."
            }
            Item::Budget => "Default size of a `recall` answer, in tokens.",
        }
    }
}

struct MemoryRow {
    id: i64,
    scope: String,
    kind: String,
    statement: String,
    when: String,
}

struct App {
    cfg: Config,
    saved: Config,
    conn: Connection,
    emb: Option<Embedder>,
    pane: Pane,
    sel: usize,
    mem_sel: usize,
    memories: Vec<MemoryRow>,
    /// `Some` while the user is typing a search.
    search: Option<String>,
    filter: Option<String>,
    stats: Stats,
    status: String,
    flash_until: Option<Instant>,
    show_help: bool,
}

impl App {
    fn new(cfg: Config) -> Result<Self> {
        let conn = db::open(&cfg.db_path())?;
        let stats = store::stats(&conn)?;
        let emb = if cfg.semantic {
            Embedder::load_if_cached(&cfg)
        } else {
            None
        };
        let mut app = Self {
            saved: cfg.clone(),
            cfg,
            conn,
            emb,
            pane: Pane::Settings,
            sel: 0,
            mem_sel: 0,
            memories: Vec::new(),
            search: None,
            filter: None,
            stats,
            status: String::new(),
            flash_until: None,
            show_help: false,
        };
        app.reload_memories()?;
        Ok(app)
    }

    fn dirty(&self) -> bool {
        let a = &self.cfg;
        let b = &self.saved;
        a.autosave != b.autosave
            || a.autosave_scope != b.autosave_scope
            || a.autosave_min_chars != b.autosave_min_chars
            || a.autosave_facts != b.autosave_facts
            || a.redact != b.redact
            || a.autorecall != b.autorecall
            || a.autorecall_limit != b.autorecall_limit
            || a.autorecall_budget != b.autorecall_budget
            || a.semantic != b.semantic
            || a.fast != b.fast
            || a.budget_tokens != b.budget_tokens
    }

    fn flash(&mut self, msg: impl Into<String>) {
        self.status = msg.into();
        self.flash_until = Some(Instant::now() + Duration::from_secs(6));
    }

    /// Newest live facts, or the results of a search when one is active.
    fn reload_memories(&mut self) -> Result<()> {
        self.memories.clear();
        match &self.filter {
            Some(q) if !q.trim().is_empty() => {
                let sc = scope::resolve(&self.conn, None, &std::env::current_dir()?)?;
                let ids = scope::read_set(&self.conn, &sc)?;
                let r = retrieve::recall(
                    &self.conn,
                    &ids,
                    self.emb.as_ref(),
                    &Query {
                        text: q.clone(),
                        limit: 60,
                        ..Default::default()
                    },
                    None,
                )?;
                for hit in r.hits {
                    self.memories.push(MemoryRow {
                        id: hit.fact.id,
                        scope: sc.label.clone(),
                        kind: hit.via.join("+"),
                        when: pack::ymd(hit.fact.recorded_at),
                        statement: hit.fact.statement,
                    });
                }
            }
            _ => {
                let mut st = self.conn.prepare(
                    "SELECT f.id, s.label, f.rel, f.statement, f.recorded_at
                     FROM facts f JOIN scopes s ON s.id = f.scope_id
                     WHERE f.invalidated_at IS NULL
                     ORDER BY f.recorded_at DESC LIMIT 200",
                )?;
                let rows = st.query_map([], |r| {
                    Ok(MemoryRow {
                        id: r.get(0)?,
                        scope: r.get(1)?,
                        kind: r.get(2)?,
                        statement: r.get(3)?,
                        when: pack::ymd(r.get(4)?),
                    })
                })?;
                for row in rows {
                    self.memories.push(row?);
                }
            }
        }
        self.mem_sel = self.mem_sel.min(self.memories.len().saturating_sub(1));
        Ok(())
    }

    /// Persist settings, and make the world match them: toggling autosave also
    /// writes or removes the agent hooks, because a setting that doesn't take
    /// effect is a lie.
    fn save(&mut self) -> Result<()> {
        let path = self.cfg.save()?;
        let hooks_wanted = self.cfg.autosave || self.cfg.autorecall;
        let home = dirs::home_dir().context("cannot find your home directory")?;
        let opts = install::Options {
            command: install::self_command(),
            global: true,
            project: None,
            only: None,
            instructions: false,
            hooks: true,
            dry_run: false,
        };
        let changes = install::apply_hooks(&home, &opts, !hooks_wanted)?;
        let touched: Vec<String> = changes
            .iter()
            .filter(|c| {
                matches!(
                    c.what,
                    install::What::WriteHooks | install::What::RemoveHooks
                )
            })
            .map(|c| c.agent.to_string())
            .collect();

        self.saved = self.cfg.clone();
        let where_ = path.display().to_string();
        if touched.is_empty() {
            self.flash(format!("saved to {where_}"));
        } else {
            self.flash(format!(
                "saved to {where_} · hooks {} for {}",
                if hooks_wanted { "wired" } else { "removed" },
                touched.join(", ")
            ));
        }
        Ok(())
    }

    fn toggle(&mut self) {
        let item = ITEMS[self.sel];
        if !item.key().is_empty() && self.cfg.is_locked(item.key()) {
            self.flash(format!(
                "{} is pinned by an environment variable",
                item.label().trim()
            ));
            return;
        }
        match item {
            Item::Autosave => self.cfg.autosave = !self.cfg.autosave,
            Item::AutosaveFacts => self.cfg.autosave_facts = !self.cfg.autosave_facts,
            Item::Redact => self.cfg.redact = !self.cfg.redact,
            Item::Autorecall => self.cfg.autorecall = !self.cfg.autorecall,
            Item::Semantic => self.cfg.semantic = !self.cfg.semantic,
            Item::Fast => self.cfg.fast = !self.cfg.fast,
            Item::AutosaveScope => {
                self.cfg.autosave_scope = if self.cfg.autosave_scope == "global" {
                    "project".into()
                } else {
                    "global".into()
                }
            }
            _ => self.adjust(1),
        }
    }

    fn adjust(&mut self, delta: i64) {
        let item = ITEMS[self.sel];
        if !item.key().is_empty() && self.cfg.is_locked(item.key()) {
            return;
        }
        let bump = |v: usize, step: i64, lo: usize, hi: usize| -> usize {
            let next = v as i64 + step;
            next.clamp(lo as i64, hi as i64) as usize
        };
        match item {
            Item::AutosaveMinChars => {
                self.cfg.autosave_min_chars = bump(self.cfg.autosave_min_chars, delta, 0, 400)
            }
            Item::AutorecallLimit => {
                self.cfg.autorecall_limit = bump(self.cfg.autorecall_limit, delta, 1, 50)
            }
            Item::AutorecallBudget => {
                self.cfg.autorecall_budget =
                    bump(self.cfg.autorecall_budget, delta * 100, 64, 8_000)
            }
            Item::Budget => {
                self.cfg.budget_tokens = bump(self.cfg.budget_tokens, delta * 100, 64, 20_000)
            }
            _ => self.toggle(),
        }
    }

    fn value_of(&self, item: Item) -> String {
        let on = |b: bool| if b { "on" } else { "off" }.to_string();
        match item {
            Item::Autosave => on(self.cfg.autosave),
            Item::AutosaveScope => self.cfg.autosave_scope.clone(),
            Item::AutosaveMinChars => format!("{} chars", self.cfg.autosave_min_chars),
            Item::AutosaveFacts => on(self.cfg.autosave_facts),
            Item::Redact => on(self.cfg.redact),
            Item::Autorecall => on(self.cfg.autorecall),
            Item::AutorecallLimit => self.cfg.autorecall_limit.to_string(),
            Item::AutorecallBudget => format!("{} tokens", self.cfg.autorecall_budget),
            Item::Semantic => on(self.cfg.semantic),
            Item::Fast => on(self.cfg.fast),
            Item::Budget => format!("{} tokens", self.cfg.budget_tokens),
        }
    }

    /// Which agents are wired, and whether autosave reached them. Read from the
    /// files themselves rather than from our own settings.
    fn agent_lines(&self) -> Vec<String> {
        let Some(home) = dirs::home_dir() else {
            return vec!["(no home directory)".into()];
        };
        let mut out = Vec::new();
        for agent in install::AGENTS {
            if !install::detect(agent, &home) {
                continue;
            }
            let mcp = agent
                .global_mcp
                .iter()
                .map(|p| home.join(p))
                .find(|p| p.exists())
                .and_then(|p| std::fs::read_to_string(p).ok())
                .map(|t| t.contains(install::SERVER_NAME))
                .unwrap_or(false);
            let hooked = agent
                .hooks
                .map(|(rel, _)| home.join(rel))
                .and_then(|p| std::fs::read_to_string(p).ok())
                .map(|t| t.contains("fuckmemory hook") || t.contains("fuckmemory\" hook"))
                .unwrap_or(false);
            let mark = |b: bool| if b { "✓" } else { "·" };
            out.push(format!(
                "{} tools {}  autosave {}",
                format_args!("{:<13}", agent.id),
                mark(mcp),
                if agent.hooks.is_some() {
                    mark(hooked).to_string()
                } else {
                    "—".to_string()
                }
            ));
        }
        if out.is_empty() {
            out.push("(no agents detected)".into());
        }
        out
    }

    fn forget_selected(&mut self) -> Result<()> {
        let Some(row) = self.memories.get(self.mem_sel) else {
            return Ok(());
        };
        let id = row.id;
        let n = self.conn.execute(
            "UPDATE facts SET invalidated_at = ?2, valid_to = COALESCE(valid_to, ?2)
             WHERE id = ?1 AND invalidated_at IS NULL",
            rusqlite::params![id, crate::config::now()],
        )?;
        if n > 0 {
            self.flash(format!("retracted #{id} — still visible in `timeline`"));
            self.stats = store::stats(&self.conn)?;
            self.reload_memories()?;
        }
        Ok(())
    }

    fn rebuild_cache(&mut self) {
        match crate::embed::build_cache(&self.cfg, true) {
            Ok(rows) => {
                self.emb = Embedder::load_if_cached(&self.cfg);
                self.flash(format!("cache rebuilt — {rows} tokens, cold start ~1 ms"));
            }
            Err(e) => self.flash(format!("cache build failed: {e:#}")),
        }
    }

    fn consolidate(&mut self) {
        let cfg = self.cfg.clone();
        match crate::consolidate::run(&mut self.conn, &cfg, self.emb.as_ref(), 500) {
            Ok(r) => {
                self.stats = store::stats(&self.conn).unwrap_or(self.stats.clone());
                let _ = self.reload_memories();
                self.flash(format!(
                    "consolidated {} episode(s), merged {} duplicate(s)",
                    r.episodes_processed, r.facts_merged
                ));
            }
            Err(e) => self.flash(format!("consolidate failed: {e:#}")),
        }
    }

    fn main_loop(&mut self, term: &mut Term) -> Result<Option<String>> {
        loop {
            if self
                .flash_until
                .map(|t| Instant::now() > t)
                .unwrap_or(false)
            {
                self.status.clear();
                self.flash_until = None;
            }
            term.draw(|f| self.draw(f))?;

            if !event::poll(Duration::from_millis(250))? {
                continue;
            }
            let TermEvent::Key(key) = event::read()? else {
                continue;
            };
            if key.kind != KeyEventKind::Press {
                continue;
            }

            // Search input swallows everything except Esc and Enter.
            if let Some(buf) = self.search.as_mut() {
                match key.code {
                    KeyCode::Esc => self.search = None,
                    KeyCode::Enter => {
                        self.filter = self.search.take().filter(|s| !s.trim().is_empty());
                        self.mem_sel = 0;
                        self.reload_memories()?;
                    }
                    KeyCode::Backspace => {
                        buf.pop();
                    }
                    KeyCode::Char(c) => buf.push(c),
                    _ => {}
                }
                continue;
            }

            if self.show_help {
                self.show_help = false;
                continue;
            }

            match key.code {
                KeyCode::Char('q') | KeyCode::Esc => {
                    if self.dirty() {
                        self.flash("unsaved changes — press s to save, or Q to discard");
                        continue;
                    }
                    return Ok(None);
                }
                KeyCode::Char('Q') => return Ok(Some("left without saving".into())),
                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    return Ok(None)
                }
                KeyCode::Tab | KeyCode::Right if self.pane == Pane::Memories => {
                    self.pane = Pane::Settings
                }
                KeyCode::Tab => {
                    self.pane = if self.pane == Pane::Settings {
                        Pane::Memories
                    } else {
                        Pane::Settings
                    }
                }
                KeyCode::Char('?') => self.show_help = true,
                KeyCode::Char('s') => self.save()?,
                KeyCode::Char('r') => self.rebuild_cache(),
                KeyCode::Char('C') => self.consolidate(),
                KeyCode::Down | KeyCode::Char('j') => match self.pane {
                    Pane::Settings => self.sel = (self.sel + 1) % ITEMS.len(),
                    Pane::Memories => {
                        if !self.memories.is_empty() {
                            self.mem_sel = (self.mem_sel + 1) % self.memories.len();
                        }
                    }
                },
                KeyCode::Up | KeyCode::Char('k') => match self.pane {
                    Pane::Settings => {
                        self.sel = (self.sel + ITEMS.len() - 1) % ITEMS.len();
                    }
                    Pane::Memories => {
                        if !self.memories.is_empty() {
                            self.mem_sel =
                                (self.mem_sel + self.memories.len() - 1) % self.memories.len();
                        }
                    }
                },
                KeyCode::Char(' ') | KeyCode::Enter if self.pane == Pane::Settings => self.toggle(),
                KeyCode::Right | KeyCode::Char('+') | KeyCode::Char('l')
                    if self.pane == Pane::Settings =>
                {
                    self.adjust(1)
                }
                KeyCode::Left | KeyCode::Char('-') | KeyCode::Char('h')
                    if self.pane == Pane::Settings =>
                {
                    self.adjust(-1)
                }
                KeyCode::Char('/') if self.pane == Pane::Memories => {
                    self.search = Some(String::new())
                }
                KeyCode::Char('x') if self.pane == Pane::Memories => self.forget_selected()?,
                KeyCode::Char('a') if self.pane == Pane::Memories => {
                    self.filter = None;
                    self.reload_memories()?;
                }
                _ => {}
            }
        }
    }

    fn draw(&self, f: &mut Frame) {
        let area = f.area();
        let rows = Layout::vertical([
            Constraint::Length(3),
            Constraint::Min(6),
            Constraint::Length(2),
        ])
        .split(area);

        let tabs = Tabs::new(vec!["Settings", "Memories"])
            .select(if self.pane == Pane::Settings { 0 } else { 1 })
            .highlight_style(Style::new().fg(Color::Black).bg(Color::Cyan))
            .block(Block::default().borders(Borders::ALL).title(format!(
                " fuckmemory — {} facts · {} scopes · {:.0} KiB{} ",
                self.stats.facts_live,
                self.stats.scopes,
                self.stats.db_bytes as f64 / 1024.0,
                if self.dirty() { " · unsaved" } else { "" }
            )));
        f.render_widget(tabs, rows[0]);

        match self.pane {
            Pane::Settings => self.draw_settings(f, rows[1]),
            Pane::Memories => self.draw_memories(f, rows[1]),
        }

        let keys = match self.pane {
            Pane::Settings => "↑↓ move · space toggle · ←→ adjust · s save · r rebuild cache · C consolidate · tab memories · q quit",
            Pane::Memories => "↑↓ move · / search · a all · x retract · tab settings · q quit",
        };
        let footer = Paragraph::new(vec![
            Line::from(Span::styled(
                self.status.clone(),
                Style::new().fg(Color::Yellow),
            )),
            Line::from(Span::styled(keys, Style::new().fg(Color::DarkGray))),
        ]);
        f.render_widget(footer, rows[2]);

        if self.show_help {
            self.draw_help(f, area);
        }
    }

    fn draw_settings(&self, f: &mut Frame, area: Rect) {
        let cols = Layout::horizontal([Constraint::Percentage(58), Constraint::Percentage(42)])
            .split(area);

        let items: Vec<ListItem> = ITEMS
            .iter()
            .map(|item| {
                let locked = !item.key().is_empty() && self.cfg.is_locked(item.key());
                let value = self.value_of(*item);
                let value_style = match value.as_str() {
                    "on" => Style::new().fg(Color::Green),
                    "off" => Style::new().fg(Color::DarkGray),
                    _ => Style::new().fg(Color::White),
                };
                let mut spans = vec![
                    Span::raw(format!("{:<20}", item.label())),
                    Span::styled(format!("{value:<14}"), value_style),
                ];
                if locked {
                    spans.push(Span::styled(
                        "env",
                        Style::new().fg(Color::Magenta).add_modifier(Modifier::DIM),
                    ));
                }
                ListItem::new(Line::from(spans))
            })
            .collect();

        let mut state = ListState::default();
        state.select(Some(self.sel));
        let list = List::new(items)
            .block(Block::default().borders(Borders::ALL).title(" settings "))
            .highlight_style(
                Style::new()
                    .bg(Color::DarkGray)
                    .add_modifier(Modifier::BOLD),
            )
            .highlight_symbol("▸ ");
        f.render_stateful_widget(list, cols[0], &mut state);

        let side = Layout::vertical([Constraint::Min(6), Constraint::Length(9)]).split(cols[1]);
        let explain = Paragraph::new(ITEMS[self.sel].help())
            .wrap(Wrap { trim: true })
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(format!(" {} ", ITEMS[self.sel].label().trim())),
            );
        f.render_widget(explain, side[0]);

        let agents = Paragraph::new(
            self.agent_lines()
                .into_iter()
                .map(Line::from)
                .collect::<Vec<_>>(),
        )
        .block(Block::default().borders(Borders::ALL).title(" agents "));
        f.render_widget(agents, side[1]);
    }

    fn draw_memories(&self, f: &mut Frame, area: Rect) {
        let title = match (&self.search, &self.filter) {
            (Some(buf), _) => format!(" search: {buf}▏"),
            (None, Some(q)) => format!(" search: {q} (a for all) "),
            (None, None) => format!(" newest {} memories ", self.memories.len()),
        };
        let items: Vec<ListItem> = self
            .memories
            .iter()
            .map(|m| {
                ListItem::new(Line::from(vec![
                    Span::styled(format!("{:<11}", m.when), Style::new().fg(Color::DarkGray)),
                    Span::styled(
                        format!("{:<14}", truncate(&m.scope, 13)),
                        Style::new().fg(Color::Blue),
                    ),
                    Span::styled(
                        format!("{:<13}", truncate(&m.kind, 12)),
                        Style::new().fg(Color::Cyan),
                    ),
                    Span::raw(m.statement.replace('\n', " ")),
                ]))
            })
            .collect();
        let mut state = ListState::default();
        state.select(Some(self.mem_sel));
        let list = List::new(items)
            .block(Block::default().borders(Borders::ALL).title(title))
            .highlight_style(
                Style::new()
                    .bg(Color::DarkGray)
                    .add_modifier(Modifier::BOLD),
            )
            .highlight_symbol("▸ ");
        f.render_stateful_widget(list, area, &mut state);
    }

    fn draw_help(&self, f: &mut Frame, area: Rect) {
        let text = "\
fuckmemory keys

  tab          switch between settings and memories
  ↑ ↓ / j k    move
  space        toggle the selected setting
  ← →          adjust a number
  s            save settings (and wire or unwire agent hooks)
  r            rebuild the fast embedding cache
  C            consolidate: merge duplicates, compact indexes
  /            search memories        x  retract the selected one
  q            quit (Q discards unsaved changes)

Settings marked `env` are pinned by a FUCKMEMORY_* variable and
cannot be changed here.";
        let w = 62u16.min(area.width.saturating_sub(4));
        let h = 16u16.min(area.height.saturating_sub(2));
        let popup = Rect {
            x: area.x + (area.width.saturating_sub(w)) / 2,
            y: area.y + (area.height.saturating_sub(h)) / 2,
            width: w,
            height: h,
        };
        f.render_widget(Clear, popup);
        f.render_widget(
            Paragraph::new(text).block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" help — any key to close "),
            ),
            popup,
        );
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let cut: String = s.chars().take(max.saturating_sub(1)).collect();
    format!("{cut}…")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_item_has_a_label_and_help() {
        for item in ITEMS {
            assert!(!item.label().trim().is_empty());
            assert!(
                item.help().len() > 20,
                "{} needs real help text",
                item.label()
            );
        }
    }

    #[test]
    fn truncate_keeps_it_within_the_column() {
        assert_eq!(truncate("short", 10), "short");
        assert_eq!(truncate("a-very-long-scope-name", 10), "a-very-lo…");
        assert_eq!(truncate("ñññññññññññ", 4), "ñññ…");
    }
}
