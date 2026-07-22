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

use crate::config::{Config, SortMode};
use crate::providers::{self, Provider, Usage};

struct App {
    providers: Vec<Arc<dyn Provider>>,
    states: Vec<ProviderState>,
    selected: usize,
    last_refresh_started: DateTime<Utc>,
    last_refresh_tick: Instant,
    refresh_interval: Duration,
    in_flight: usize,
    config: Config,
}

struct ProviderState {
    id: String,
    loading: bool,
    result: Option<Result<Usage, String>>,
}

impl App {
    fn new(providers: Vec<Arc<dyn Provider>>, config: Config, interval_secs: Option<u64>) -> Self {
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
            last_refresh_tick: Instant::now(),
            refresh_interval: Duration::from_secs(
                interval_secs.unwrap_or(config.refresh_interval_secs).max(5),
            ),
            in_flight: 0,
            config,
        }
    }

    fn spawn_refresh(&mut self) -> mpsc::UnboundedReceiver<(String, Result<Usage, String>)> {
        let (tx, rx) = mpsc::unbounded_channel();
        self.last_refresh_started = Utc::now();
        self.last_refresh_tick = Instant::now();
        self.in_flight = self.providers.len();
        let status_enabled = self.config.status_enabled;
        for s in &mut self.states {
            s.loading = true;
        }
        for p in &self.providers {
            let p = Arc::clone(p);
            let tx = tx.clone();
            tokio::spawn(async move {
                let id = p.id().to_string();
                let res = match p.fetch().await {
                    Ok(mut usage) => {
                        if status_enabled {
                            usage.status = providers::status::fetch_for_provider(&id).await;
                        }
                        Ok(usage)
                    }
                    Err(e) => Err(format!("{:#}", e)),
                };
                let _ = tx.send((id, res));
            });
        }
        rx
    }

    fn set_result(&mut self, id: &str, res: Result<Usage, String>) {
        let selected_id = self.states.get(self.selected).map(|s| s.id.clone());
        if let Some(s) = self.states.iter_mut().find(|s| s.id == id) {
            s.loading = false;
            s.result = Some(res);
            self.in_flight = self.in_flight.saturating_sub(1);
        }
        self.sort_for_display();
        if let Some(selected_id) = selected_id
            && let Some(pos) = self.states.iter().position(|s| s.id == selected_id)
        {
            self.selected = pos;
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

    fn sort_for_display(&mut self) {
        if self.config.sort == SortMode::Original {
            return;
        }
        let mut paired: Vec<_> = self
            .providers
            .drain(..)
            .zip(self.states.drain(..))
            .collect();
        match self.config.sort {
            SortMode::Risk => paired.sort_by(|a, b| {
                risk_score(&b.1)
                    .partial_cmp(&risk_score(&a.1))
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then_with(|| a.1.id.cmp(&b.1.id))
            }),
            SortMode::Name => paired.sort_by(|a, b| a.1.id.cmp(&b.1.id)),
            SortMode::Original => {}
        }
        let (providers, states): (Vec<_>, Vec<_>) = paired.into_iter().unzip();
        self.providers = providers;
        self.states = states;
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
    let config = Config::load();
    let mut app = App::new(providers, config, None);

    let mut terminal = init_terminal()?;
    let mut rx = app.spawn_refresh();

    let res = event_loop(&mut terminal, &mut app, &mut rx).await;

    restore_terminal(&mut terminal)?;
    res
}

pub async fn run_watch(provider: &str, interval: u64) -> Result<()> {
    // watch 模式 = 只渲染单 provider 的 TUI。复用 App，过滤 providers 列表。
    let filtered = providers::select(provider)?;
    let config = Config::load();
    let mut app = App::new(filtered, config, Some(interval));
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

        if app.in_flight == 0 && app.last_refresh_tick.elapsed() >= app.refresh_interval {
            *rx = app.spawn_refresh();
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
    let next = if app.in_flight == 0 {
        let left = app
            .refresh_interval
            .saturating_sub(app.last_refresh_tick.elapsed())
            .as_secs();
        format!(" · next {}s", left)
    } else {
        String::new()
    };
    let line = Line::from(vec![
        Span::styled(
            "aitop",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("  ·  "),
        Span::styled(
            format!("{}{}", refresh_label, next),
            Style::default().fg(Color::Gray),
        ),
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

    let start = visible_start(&card_heights, app.selected, area.height);
    let visible_heights: Vec<u16> = card_heights.iter().skip(start).copied().collect();
    let constraints: Vec<Constraint> = if total <= area.height {
        visible_heights
            .iter()
            .map(|h| Constraint::Length(*h))
            .chain(std::iter::once(Constraint::Min(0)))
            .collect()
    } else {
        visible_heights
            .iter()
            .map(|h| Constraint::Length(*h))
            .collect()
    };

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(area);

    for (chunk_i, (i, state)) in app.states.iter().enumerate().skip(start).enumerate() {
        if chunk_i >= chunks.len() {
            break;
        }
        render_card(f, chunks[chunk_i], state, i == app.selected, &app.config);
    }
}

fn visible_start(heights: &[u16], selected: usize, available: u16) -> usize {
    if heights.iter().sum::<u16>() <= available || heights.is_empty() {
        return 0;
    }
    let mut used: u16 = 0;
    let mut start = selected.min(heights.len() - 1);
    loop {
        let h = heights[start];
        if used.saturating_add(h) > available && start != selected {
            start += 1;
            break;
        }
        used = used.saturating_add(h);
        if start == 0 {
            break;
        }
        start -= 1;
    }
    start
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
            n += u.costs.len() as u16;
            n += u.sub_quotas.len() as u16;
            if u.note.is_some() {
                n += 1;
            }
            n
        }
    };
    body + 2 // 边框
}

fn render_card(f: &mut Frame, area: Rect, s: &ProviderState, selected: bool, config: &Config) {
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
        Some(Ok(u)) => render_usage(f, inner, u, config),
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

fn render_usage(f: &mut Frame, area: Rect, u: &Usage, config: &Config) {
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
    for _ in &u.costs {
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
    let meta = build_meta_line(u, config);
    f.render_widget(Paragraph::new(meta), chunks[idx]);
    idx += 1;

    if let Some(s) = &u.session {
        f.render_widget(build_window_gauge("session", s), chunks[idx]);
        idx += 1;
    }
    if let Some(w) = &u.weekly {
        f.render_widget(build_window_gauge("weekly ", w), chunks[idx]);
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
    for c in &u.costs {
        let line = Line::from(vec![
            Span::raw(format!("  {}: ", truncate(&c.label, 16))),
            Span::styled(
                match c.amount {
                    Some(amount) => format!("~{:.2} {}", amount, c.currency),
                    None => c.currency.clone(),
                },
                Style::default().fg(Color::Green),
            ),
            Span::raw(match c.tokens {
                Some(tokens) => format!(" · {}", fmt_tokens(tokens)),
                None => String::new(),
            }),
        ]);
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

fn build_meta_line<'a>(u: &'a Usage, config: &Config) -> Line<'a> {
    let mut spans = vec![
        Span::raw("  "),
        Span::styled(&u.source, Style::default().fg(Color::Blue)),
    ];
    if config.show_accounts
        && let Some(acc) = &u.account
    {
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
    if let Some(status) = &u.status {
        spans.push(Span::raw("  "));
        spans.push(Span::styled(
            status.message.as_str(),
            Style::default().fg(match status.level {
                crate::providers::StatusLevel::Operational => Color::Green,
                crate::providers::StatusLevel::Degraded => Color::Yellow,
                crate::providers::StatusLevel::Outage => Color::Red,
                crate::providers::StatusLevel::Unknown => Color::Gray,
            }),
        ));
    }
    Line::from(spans)
}

fn build_window_gauge(label: &str, window: &crate::providers::Window) -> Gauge<'static> {
    let pct = window.used_percent.clamp(0.0, 100.0);
    let color = pct_color(pct);
    Gauge::default()
        .block(Block::default())
        .gauge_style(Style::default().fg(color).bg(Color::Rgb(40, 40, 40)))
        .ratio(pct / 100.0)
        .label(format!(
            "  {} {:>5.1}% · {}",
            label,
            pct,
            window_detail(window)
        ))
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

fn window_detail(window: &crate::providers::Window) -> String {
    let left = (100.0 - window.used_percent).clamp(0.0, 100.0);
    let mut parts = vec![format!("{:.0}% left", left)];
    if let Some(reset) = window.resets_at {
        let secs = (reset - Utc::now()).num_seconds();
        parts.push(format!("resets in {}", fmt_duration_secs(secs)));
    }
    if let Some(pace) = pace_label(window) {
        parts.push(pace);
    }
    parts.join(" · ")
}

fn pace_label(window: &crate::providers::Window) -> Option<String> {
    let resets_at = window.resets_at?;
    let minutes = window.window_minutes?;
    if minutes == 0 {
        return None;
    }
    let now = Utc::now();
    let total = chrono::Duration::minutes(minutes as i64);
    let remaining = resets_at - now;
    if remaining <= chrono::Duration::zero() {
        return Some("reset due".to_string());
    }
    let elapsed = total - remaining;
    if elapsed <= chrono::Duration::zero() {
        return None;
    }
    let elapsed_frac = elapsed.num_seconds() as f64 / total.num_seconds() as f64;
    if elapsed_frac < 0.03 {
        return None;
    }
    let ideal_used = elapsed_frac * 100.0;
    let delta = ideal_used - window.used_percent;
    if delta.abs() < 3.0 {
        Some("on pace".to_string())
    } else if delta > 0.0 {
        Some(format!("{:.0}% reserve", delta))
    } else {
        let used_rate = window.used_percent / elapsed.num_seconds() as f64;
        let secs_to_empty = ((100.0 - window.used_percent) / used_rate).max(0.0) as i64;
        Some(format!("runs out in {}", fmt_duration_secs(secs_to_empty)))
    }
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

fn risk_score(s: &ProviderState) -> f64 {
    match &s.result {
        Some(Ok(u)) => {
            let mut score = 0.0_f64;
            if let Some(w) = &u.session {
                score = score.max(w.used_percent);
            }
            if let Some(w) = &u.weekly {
                score = score.max(w.used_percent);
            }
            for sq in &u.sub_quotas {
                score = score.max(sq.used_percent);
            }
            if matches!(
                u.status.as_ref().map(|s| &s.level),
                Some(crate::providers::StatusLevel::Outage)
            ) {
                score += 200.0;
            } else if matches!(
                u.status.as_ref().map(|s| &s.level),
                Some(crate::providers::StatusLevel::Degraded)
            ) {
                score += 100.0;
            }
            score
        }
        Some(Err(_)) => 300.0,
        None => 0.0,
    }
}

fn fmt_duration_secs(secs: i64) -> String {
    let secs = secs.max(0);
    let (d, h, m) = (secs / 86_400, (secs % 86_400) / 3_600, (secs % 3_600) / 60);
    match (d, h, m) {
        (0, 0, 0) => format!("{}s", secs),
        (0, 0, m) => format!("{}m", m),
        (0, h, 0) => format!("{}h", h),
        (0, h, m) => format!("{}h{}m", h, m),
        (d, 0, _) => format!("{}d", d),
        (d, h, _) => format!("{}d{}h", d, h),
    }
}

fn fmt_tokens(n: u64) -> String {
    if n >= 1_000_000_000 {
        format!("{:.1}B tokens", n as f64 / 1_000_000_000.0)
    } else if n >= 1_000_000 {
        format!("{:.1}M tokens", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}K tokens", n as f64 / 1_000.0)
    } else {
        format!("{} tokens", n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::{StatusLevel, Window};

    fn state_with_session(id: &str, used_percent: f64) -> ProviderState {
        ProviderState {
            id: id.to_string(),
            loading: false,
            result: Some(Ok(Usage {
                provider: id.to_string(),
                source: "test".to_string(),
                account: None,
                plan: None,
                session: Some(Window {
                    used_percent,
                    window_minutes: None,
                    resets_at: None,
                }),
                weekly: None,
                credits: None,
                sub_quotas: Vec::new(),
                costs: Vec::new(),
                status: None,
                updated_at: Utc::now(),
                note: None,
            })),
        }
    }

    #[test]
    fn visible_start_keeps_selected_card_in_view() {
        let heights = vec![3, 3, 3, 3, 3];
        assert_eq!(visible_start(&heights, 0, 6), 0);
        assert_eq!(visible_start(&heights, 3, 6), 2);
        assert_eq!(visible_start(&heights, 4, 6), 3);
    }

    #[test]
    fn risk_score_prefers_errors_then_high_usage() {
        let err = ProviderState {
            id: "err".to_string(),
            loading: false,
            result: Some(Err("boom".to_string())),
        };
        let low = state_with_session("low", 10.0);
        let high = state_with_session("high", 95.0);
        assert!(risk_score(&err) > risk_score(&high));
        assert!(risk_score(&high) > risk_score(&low));
    }

    #[test]
    fn risk_score_includes_status_severity() {
        let mut degraded = state_with_session("degraded", 10.0);
        if let Some(Ok(u)) = &mut degraded.result {
            u.status = Some(crate::providers::ProviderStatus {
                level: StatusLevel::Degraded,
                message: "degraded".to_string(),
                url: None,
            });
        }
        let high = state_with_session("high", 95.0);
        assert!(risk_score(&degraded) > risk_score(&high));
    }
}
