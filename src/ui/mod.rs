//! TUI 层 — 基于 ratatui + crossterm。
//!
//! 布局：
//! ┌ header: 标题 · 最后刷新时间 · 键位提示 ────────────────┐
//! ├ body: 垂直堆叠的 provider 卡片（带边框），选中高亮 ─────┤
//! └ footer: 错误/提示条（全局状态时可用） ──────────────────┘
//!
//! 键位：q/Ctrl-C 退出 · r 刷新 · ↑↓/jk 切换 · g/G 跳首尾

use std::io;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Frame;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Gauge, Paragraph, Wrap};
use tokio::sync::mpsc;

use rust_i18n::t;

use crate::providers::{self, Provider, Usage};

struct App {
    providers: Vec<Arc<dyn Provider>>,
    states: Vec<ProviderState>,
    selected: usize,
    last_refresh_started: DateTime<Utc>,
    in_flight: usize,
}

struct ProviderState {
    id: String,
    loading: bool,
    result: Option<Result<Usage, String>>,
}

impl App {
    fn new(providers: Vec<Arc<dyn Provider>>) -> Self {
        let states = providers
            .iter()
            .map(|p| ProviderState {
                id: p.id().to_string(),
                loading: true,
                result: None,
            })
            .collect();
        Self {
            providers,
            states,
            selected: 0,
            last_refresh_started: Utc::now(),
            in_flight: 0,
        }
    }

    fn spawn_refresh(&mut self) -> mpsc::UnboundedReceiver<(String, Result<Usage, String>)> {
        let (tx, rx) = mpsc::unbounded_channel();
        self.last_refresh_started = Utc::now();
        self.in_flight = self.providers.len();
        for s in &mut self.states {
            s.loading = true;
        }
        for p in &self.providers {
            let p = Arc::clone(p);
            let tx = tx.clone();
            tokio::spawn(async move {
                let id = p.id().to_string();
                let res = p.fetch().await.map_err(|e| format!("{:#}", e));
                let _ = tx.send((id, res));
            });
        }
        rx
    }

    fn set_result(&mut self, id: &str, res: Result<Usage, String>) {
        if let Some(s) = self.states.iter_mut().find(|s| s.id == id) {
            s.loading = false;
            s.result = Some(res);
            self.in_flight = self.in_flight.saturating_sub(1);
        }
    }

    fn select_next(&mut self) {
        if !self.states.is_empty() {
            self.selected = (self.selected + 1) % self.states.len();
        }
    }

    fn select_prev(&mut self) {
        if !self.states.is_empty() {
            self.selected = (self.selected + self.states.len() - 1) % self.states.len();
        }
    }
}

pub async fn run_tui(show_all: bool) -> Result<()> {
    let providers = if show_all {
        providers::all_providers()
    } else {
        let avail = providers::available_providers();
        if avail.is_empty() {
            eprintln!("{}", t!("no_detected_providers"));
            providers::all_providers()
        } else {
            avail
        }
    };
    if providers.is_empty() {
        eprintln!("{}", t!("no_providers"));
        return Ok(());
    }
    let mut app = App::new(providers);

    let mut terminal = init_terminal()?;
    let mut rx = app.spawn_refresh();

    let res = event_loop(&mut terminal, &mut app, &mut rx).await;

    restore_terminal(&mut terminal)?;
    res
}

pub async fn run_watch(provider: &str, _interval: u64) -> Result<()> {
    // watch 模式 = 只渲染单 provider 的 TUI。复用 App，过滤 providers 列表。
    let filtered = providers::select(provider)?;
    let mut app = App::new(filtered);
    let mut terminal = init_terminal()?;
    let mut rx = app.spawn_refresh();
    let res = event_loop(&mut terminal, &mut app, &mut rx).await;
    restore_terminal(&mut terminal)?;
    res
}

type Term = Terminal<CrosstermBackend<io::Stdout>>;

fn init_terminal() -> Result<Term> {
    enable_raw_mode().context("无法进入 raw mode")?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen).context("无法进入 alternate screen")?;
    let backend = CrosstermBackend::new(stdout);
    let terminal = Terminal::new(backend).context("构建 terminal 失败")?;
    Ok(terminal)
}

fn restore_terminal(terminal: &mut Term) -> Result<()> {
    disable_raw_mode().ok();
    execute!(terminal.backend_mut(), LeaveAlternateScreen).ok();
    terminal.show_cursor().ok();
    Ok(())
}

async fn event_loop(
    terminal: &mut Term,
    app: &mut App,
    rx: &mut mpsc::UnboundedReceiver<(String, Result<Usage, String>)>,
) -> Result<()> {
    let tick = Duration::from_millis(120);
    let mut last_tick = Instant::now();
    loop {
        // 先排空已到达的 fetch 结果（非阻塞）
        while let Ok((id, res)) = rx.try_recv() {
            app.set_result(&id, res);
        }

        terminal.draw(|f| render(f, app))?;

        let timeout = tick.saturating_sub(last_tick.elapsed());
        if event::poll(timeout)?
            && let Event::Key(key) = event::read()?
        {
            if key.kind != KeyEventKind::Press {
                continue;
            }
            match key.code {
                KeyCode::Char('q') => break,
                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => break,
                KeyCode::Char('r') => {
                    *rx = app.spawn_refresh();
                }
                KeyCode::Down | KeyCode::Char('j') => app.select_next(),
                KeyCode::Up | KeyCode::Char('k') => app.select_prev(),
                KeyCode::Char('g') => app.selected = 0,
                KeyCode::Char('G') => app.selected = app.states.len().saturating_sub(1),
                _ => {}
            }
        }

        if last_tick.elapsed() >= tick {
            last_tick = Instant::now();
        }
    }
    Ok(())
}

fn render(f: &mut Frame, app: &App) {
    let area = f.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .split(area);

    render_header(f, chunks[0], app);
    render_body(f, chunks[1], app);
    render_footer(f, chunks[2], app);
}

fn render_header(f: &mut Frame, area: Rect, app: &App) {
    let elapsed = (Utc::now() - app.last_refresh_started).num_seconds();
    let refresh_label = if app.in_flight > 0 {
        t!("status_refreshing", remaining = app.in_flight).into_owned()
    } else {
        t!("status_updated_ago", seconds = elapsed.max(0)).into_owned()
    };
    let line = Line::from(vec![
        Span::styled(
            "aitop",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("  ·  "),
        Span::styled(refresh_label, Style::default().fg(Color::Gray)),
        Span::raw("  ·  "),
        Span::styled("q", Style::default().fg(Color::Yellow)),
        Span::raw(format!(" {}  ", t!("hint_quit"))),
        Span::styled("r", Style::default().fg(Color::Yellow)),
        Span::raw(format!(" {}  ", t!("hint_refresh"))),
        Span::styled("↑↓/jk", Style::default().fg(Color::Yellow)),
        Span::raw(format!(" {}", t!("hint_nav"))),
    ]);
    f.render_widget(Paragraph::new(line), area);
}

fn render_footer(f: &mut Frame, area: Rect, app: &App) {
    let (ok, err, loading) = tally(app);
    let line = Line::from(vec![
        Span::raw("["),
        Span::styled(format!("{}", ok), Style::default().fg(Color::Green)),
        Span::raw(" ok / "),
        Span::styled(format!("{}", err), Style::default().fg(Color::Red)),
        Span::raw(" err / "),
        Span::styled(format!("{}", loading), Style::default().fg(Color::Yellow)),
        Span::raw(" loading]"),
    ]);
    f.render_widget(Paragraph::new(line), area);
}

fn tally(app: &App) -> (usize, usize, usize) {
    let mut ok = 0;
    let mut err = 0;
    let mut loading = 0;
    for s in &app.states {
        if s.loading {
            loading += 1;
        } else {
            match &s.result {
                Some(Ok(_)) => ok += 1,
                Some(Err(_)) => err += 1,
                None => loading += 1,
            }
        }
    }
    (ok, err, loading)
}

fn render_body(f: &mut Frame, area: Rect, app: &App) {
    // 每个卡片的高度根据内容动态估算（标题行 + 账号行 + 每个 gauge 1 行 + note 1 行 + 边框）
    let card_heights: Vec<u16> = app.states.iter().map(card_height).collect();
    let total: u16 = card_heights.iter().sum();

    // 如果总高度 > 可用区域，裁剪至可用区域；暂不做滚动
    let constraints: Vec<Constraint> = if total <= area.height {
        card_heights
            .iter()
            .map(|h| Constraint::Length(*h))
            .chain(std::iter::once(Constraint::Min(0)))
            .collect()
    } else {
        card_heights
            .iter()
            .map(|h| Constraint::Length(*h))
            .collect()
    };

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(area);

    for (i, state) in app.states.iter().enumerate() {
        if i >= chunks.len() {
            break;
        }
        render_card(f, chunks[i], state, i == app.selected);
    }
}

fn card_height(s: &ProviderState) -> u16 {
    // 边框占 2 行；其余：meta 1 + gauges/note
    let body = match &s.result {
        None => 1, // loading
        Some(Err(_)) => 2,
        Some(Ok(u)) => {
            let mut n = 1; // meta 行
            if u.session.is_some() {
                n += 1;
            }
            if u.weekly.is_some() {
                n += 1;
            }
            if u.credits.is_some() {
                n += 1;
            }
            n += u.sub_quotas.len() as u16;
            if u.note.is_some() {
                n += 1;
            }
            n
        }
    };
    body + 2 // 边框
}

fn render_card(f: &mut Frame, area: Rect, s: &ProviderState, selected: bool) {
    let title_color = match &s.result {
        _ if s.loading => Color::Yellow,
        Some(Ok(_)) => Color::Green,
        Some(Err(_)) => Color::Red,
        None => Color::Gray,
    };
    let border_style = if selected {
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    let prefix = if selected { "▶ " } else { "  " };
    let title = Line::from(vec![
        Span::styled(prefix, Style::default().fg(Color::Cyan)),
        Span::styled(
            provider_display_name(&s.id),
            Style::default()
                .fg(title_color)
                .add_modifier(Modifier::BOLD),
        ),
    ]);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(border_style)
        .title(title);
    let inner = block.inner(area);
    f.render_widget(block, area);

    if s.loading && s.result.is_none() {
        let p = Paragraph::new(Line::from(Span::styled(
            "  loading…",
            Style::default().fg(Color::Yellow),
        )));
        f.render_widget(p, inner);
        return;
    }

    match &s.result {
        None => {}
        Some(Err(msg)) => render_err(f, inner, msg),
        Some(Ok(u)) => render_usage(f, inner, u),
    }
}

fn render_err(f: &mut Frame, area: Rect, msg: &str) {
    let text = vec![
        Line::from(Span::styled(
            "  ⚠ error",
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(
            format!("  {}", msg),
            Style::default().fg(Color::Red),
        )),
    ];
    f.render_widget(Paragraph::new(text).wrap(Wrap { trim: true }), area);
}

fn render_usage(f: &mut Frame, area: Rect, u: &Usage) {
    // 逐行布局：meta 1 行 → gauges → note
    let mut constraints: Vec<Constraint> = vec![Constraint::Length(1)];
    if u.session.is_some() {
        constraints.push(Constraint::Length(1));
    }
    if u.weekly.is_some() {
        constraints.push(Constraint::Length(1));
    }
    if u.credits.is_some() {
        constraints.push(Constraint::Length(1));
    }
    for _ in &u.sub_quotas {
        constraints.push(Constraint::Length(1));
    }
    if u.note.is_some() {
        constraints.push(Constraint::Length(1));
    }
    constraints.push(Constraint::Min(0));
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(area);

    let mut idx = 0;

    // meta
    let meta = build_meta_line(u);
    f.render_widget(Paragraph::new(meta), chunks[idx]);
    idx += 1;

    if let Some(s) = &u.session {
        f.render_widget(build_gauge("session", s.used_percent), chunks[idx]);
        idx += 1;
    }
    if let Some(w) = &u.weekly {
        f.render_widget(build_gauge("weekly ", w.used_percent), chunks[idx]);
        idx += 1;
    }
    if let Some(c) = &u.credits {
        let line = match c.total {
            Some(total) => Line::from(vec![
                Span::raw("  credits: "),
                Span::styled(
                    format!("{:.2}", c.remaining),
                    Style::default()
                        .fg(Color::Green)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(format!(" / {:.2} {}", total, c.unit)),
            ]),
            None => Line::from(vec![
                Span::raw("  credits: "),
                Span::styled(
                    format!("{:.2} {}", c.remaining, c.unit),
                    Style::default().fg(Color::Green),
                ),
                Span::raw("  (no cap)"),
            ]),
        };
        f.render_widget(Paragraph::new(line), chunks[idx]);
        idx += 1;
    }
    for sq in &u.sub_quotas {
        let label = format!("  {:<22}", truncate(&sq.label, 22));
        f.render_widget(build_gauge(&label, sq.used_percent), chunks[idx]);
        idx += 1;
    }
    if let Some(n) = &u.note {
        let line = Line::from(Span::styled(
            format!(
                "  note: {}",
                truncate(n, area.width.saturating_sub(8) as usize)
            ),
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::ITALIC),
        ));
        f.render_widget(Paragraph::new(line), chunks[idx]);
    }
}

fn build_meta_line(u: &Usage) -> Line<'_> {
    let mut spans = vec![
        Span::raw("  "),
        Span::styled(&u.source, Style::default().fg(Color::Blue)),
    ];
    if let Some(acc) = &u.account {
        spans.push(Span::raw("  "));
        spans.push(Span::styled(
            acc.as_str(),
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ));
    }
    if let Some(plan) = &u.plan {
        spans.push(Span::raw("  ["));
        spans.push(Span::styled(
            plan.as_str(),
            Style::default().fg(Color::Magenta),
        ));
        spans.push(Span::raw("]"));
    }
    Line::from(spans)
}

fn build_gauge(label: &str, used_percent: f64) -> Gauge<'_> {
    let pct = used_percent.clamp(0.0, 100.0);
    let color = pct_color(pct);
    Gauge::default()
        .block(Block::default())
        .gauge_style(Style::default().fg(color).bg(Color::Rgb(40, 40, 40)))
        .ratio(pct / 100.0)
        .label(format!("  {} {:>5.1}%", label, pct))
}

fn pct_color(pct: f64) -> Color {
    if pct >= 80.0 {
        Color::Red
    } else if pct >= 50.0 {
        Color::Yellow
    } else {
        Color::Green
    }
}

fn provider_display_name(id: &str) -> String {
    match id {
        "openrouter" => "OpenRouter".to_string(),
        "gemini" => "Gemini".to_string(),
        "codex" => "Codex".to_string(),
        "claude" => "Claude".to_string(),
        "copilot" => "Copilot".to_string(),
        "kiro" => "Kiro".to_string(),
        other => other.to_string(),
    }
}

fn truncate(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max_chars.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}
