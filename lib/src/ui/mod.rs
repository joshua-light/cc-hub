//! Top-level TUI render entry point and shared chrome (title bar, tab strip,
//! status bar). `render` is the hot-reload entry called by lib.rs's
//! `#[no_mangle]` shim. The per-tab bodies and overlays live in the sibling
//! modules; the band background and layout split are defined here so the tab
//! strip, project chip strip, and to-do panel all share one source of truth.

pub mod common;
pub mod metrics;
pub mod palette;
pub mod popups;
pub mod projects;
pub mod sessions;
pub mod sessions_list;
pub mod tasks;

// Items consumed by bin/src/main.rs keep their `cc_hub_lib::ui::X` paths.
pub use common::build_usage_line;
pub use popups::build_state_debug_content;

use crate::app::{status_msg_ttl, visible_tabs, App, Tab, View};
use crate::config;
use crate::folder_picker::PickerMode;
use crate::models;
use crate::ui::palette::{ACCENT_BLUE, SEP_GRAY};
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

/// Shared dark band background painted by the tab strip, project chip strip,
/// and contention strip — keeping these identical avoids a visible seam when
/// rows abut.
pub(crate) const BAND_BG: Color = Color::Rgb(20, 20, 28);

pub(crate) fn cell_height() -> u16 {
    config::get().ui.cell_height.max(1)
}

pub(crate) fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Top-level vertical split: title bar, tab strip, body, status bar. Shared
/// between `render` and overlays that anchor to the body region (e.g. the
/// to-do side panel) so the band heights are defined in exactly one place.
pub(crate) fn main_layout(area: Rect) -> std::rc::Rc<[Rect]> {
    Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(3),
            Constraint::Min(0),
            Constraint::Length(1),
        ])
        .split(area)
}

pub fn render(frame: &mut Frame, app: &mut App) {
    app.update_grid_cols(frame.area().width);

    let chunks = main_layout(frame.area());

    render_title_bar(frame, chunks[0], app);
    render_tab_strip(frame, chunks[1], app);
    match app.current_tab {
        Tab::Tasks => tasks::render_tasks_body(frame, chunks[2], app),
        Tab::Projects => projects::render_projects_body(frame, chunks[2], app),
        Tab::Sessions => sessions::render_sessions_body(frame, chunks[2], app),
        Tab::Metrics => metrics::render_metrics_body(frame, chunks[2], app),
    }
    render_status_bar(frame, chunks[3], app);

    match app.view {
        View::Popup => sessions::render_popup(frame, frame.area(), app),
        View::LiveTail => popups::render_live_tail(frame, frame.area(), app),
        View::ConfirmClose => popups::render_confirm_close(frame, frame.area(), app),
        View::StateDebug => popups::render_state_debug(frame, frame.area(), app),
        View::PromptInput => popups::render_prompt_input(frame, frame.area(), app),
        View::ModelPicker => popups::render_model_picker(frame, frame.area(), app),
        View::AgentPicker => popups::render_agent_picker(frame, frame.area(), app),
        View::TaskLinkPicker => popups::render_task_link_picker(frame, frame.area(), app),
        View::RenameSession => popups::render_rename_session(frame, frame.area(), app),
        View::TmuxPane => popups::render_tmux_pane(frame, frame.area(), app),
        View::FolderPicker => popups::render_folder_picker(frame, frame.area(), app),
        View::GhCreateInput => {
            popups::render_folder_picker(frame, frame.area(), app);
            popups::render_gh_create_input(frame, frame.area(), app);
        }
        View::ProjectsResult => projects::render_projects_result(frame, frame.area(), app),
        View::Backlog => projects::render_backlog(frame, frame.area(), app),
        View::TodoPanel => popups::render_todo_panel(frame, frame.area(), app),
        View::TaskInput => popups::render_task_input(frame, frame.area(), app),
        View::TaskTags => popups::render_task_tags(frame, frame.area(), app),
        View::TaskInfo => tasks::render_task_info(frame, frame.area(), app),
        View::TaskAttachInput => popups::render_task_attach_input(frame, frame.area(), app),
        // The filter bar lives inside the tasks body (already rendered
        // above), so filter-editing needs no overlay.
        View::TaskFilter => {}
        View::Grid => {}
    }
}

pub(crate) fn render_tab_strip(frame: &mut Frame, area: Rect, app: &App) {
    if area.height == 0 {
        return;
    }
    let bg = Style::default().bg(BAND_BG);

    // Paint the full band (top padding + tabs row + bottom padding) so the
    // background colour reads as a continuous header strip.
    frame.render_widget(Paragraph::new("").style(bg), area);

    let tabs = visible_tabs();
    let mut spans: Vec<Span<'static>> = vec![Span::styled("  ", bg)];
    for (i, tab) in tabs.iter().enumerate() {
        let is_active = *tab == app.current_tab;
        let (fg, bgc, modi) = if is_active {
            (Color::Black, ACCENT_BLUE, Modifier::BOLD)
        } else {
            (
                Color::Rgb(170, 170, 190),
                Color::Rgb(40, 40, 52),
                Modifier::empty(),
            )
        };
        spans.push(Span::styled(
            format!(" {} ", tab.label()),
            Style::default().fg(fg).bg(bgc).add_modifier(modi),
        ));
        if i + 1 < tabs.len() {
            spans.push(Span::styled(" ", bg));
        }
    }
    spans.push(Span::styled(
        "   ⇥/K next · ⇧⇥/J prev tab",
        Style::default().fg(Color::Rgb(80, 80, 95)).bg(BAND_BG),
    ));

    // Tabs go on the visual middle row (or first row if the band is shorter).
    let row_y = area.y + area.height / 2;
    let row_area = Rect::new(area.x, row_y, area.width, 1);
    frame.render_widget(Paragraph::new(Line::from(spans)).style(bg), row_area);
}

pub(crate) fn render_title_bar(frame: &mut Frame, area: Rect, app: &App) {
    let total = app.session_count();
    let attention = app.attention_count();

    let mut left_spans = vec![
        Span::styled(
            " 󰚩 cc-hub ",
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("{} sessions", total),
            Style::default().fg(Color::DarkGray),
        ),
    ];

    if attention > 0 {
        left_spans.push(Span::styled(
            format!("  󰂞 {} need attention", attention),
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ));
    }

    let bg = Style::default().bg(Color::Rgb(30, 30, 40)).fg(Color::White);
    let left_line = Line::from(left_spans);

    let mut right_spans = build_session_count_spans(&app.session_counts);
    let usage_spans = app.usage_line.spans.iter().cloned();
    if !right_spans.is_empty() && app.usage_line.width() > 0 {
        right_spans.push(Span::styled(" │ ", Style::default().fg(SEP_GRAY)));
    }
    right_spans.extend(usage_spans);
    let right_line = Line::from(right_spans);
    let right_w = right_line.width() as u16;
    let left_w = left_line.width() as u16;

    // If usage would overflow, fall back to just the left line.
    if right_w == 0 || left_w + right_w > area.width {
        frame.render_widget(Paragraph::new(left_line).style(bg), area);
        return;
    }

    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Min(0), Constraint::Length(right_w)])
        .split(area);

    frame.render_widget(Paragraph::new(left_line).style(bg), chunks[0]);
    frame.render_widget(
        Paragraph::new(right_line)
            .style(bg)
            .alignment(Alignment::Right),
        chunks[1],
    );
}

pub(crate) fn build_session_count_spans(
    c: &crate::session_count::SessionCounts,
) -> Vec<Span<'static>> {
    if c.today == 0 && c.week == 0 {
        return Vec::new();
    }
    let label_style = Style::default().fg(Color::DarkGray);
    let num_style = Style::default()
        .fg(Color::White)
        .add_modifier(Modifier::BOLD);
    let sep_style = Style::default().fg(SEP_GRAY);
    vec![
        Span::styled(" today ", label_style),
        Span::styled(c.today.to_string(), num_style),
        Span::styled(" │ ", sep_style),
        Span::styled("this wk ", label_style),
        Span::styled(c.week.to_string(), num_style),
    ]
}

pub(crate) fn render_status_bar(frame: &mut Frame, area: Rect, app: &App) {
    let elapsed = app.last_refresh.elapsed().as_secs();
    let refresh_text = if elapsed < 2 {
        "just now".to_string()
    } else {
        models::relative_age(elapsed)
    };

    let fresh_status = app
        .status_msg
        .as_ref()
        .filter(|(_, ts)| ts.elapsed() < status_msg_ttl())
        .map(|(msg, _)| msg.as_str());

    let mut spans: Vec<Span> = Vec::new();

    if let Some(msg) = fresh_status {
        spans.push(Span::styled(
            format!(" {} ", msg),
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        ));
    } else {
        let keybinds: &str = match app.view {
            View::Grid => match app.current_tab {
                // High-value Review verbs lead so they survive the right-edge
                // truncation at narrow widths (the status bar is one row, no
                // wrap); rare project-management verbs trail. The Space:approve
                // chip is rendered separately *ahead* of this string below so
                // it is never the first thing clipped.
                Tab::Tasks => "a/n:add  enter/f:focus agent  v:info  s:assign agent  S:agent in ~  h/l:col  j/k:task  H/L:move  /:filter  1-4:priority  t:tags  r:rename  A:attach  p:paste note  x:delete  u:undo  c:clear done  tab:next  q:quit",
                Tab::Projects => "enter:focus orch  n:new task  r:result  f:agent terminal/resurrect  R:restart  b:backlog  h/l:col  j/k:task  H/L:project  N:register project  c:copy id  x:delete task  X:remove project  tab:next  q:quit",
                Tab::Sessions => "enter/f:focus/resume  n:new  A:default agent  N:new+model  p:new in…  i:info  r:rename  L:link task  t:to-do  o:shell  M:bookmarks  D:why?  h/j/k/l:nav  v:layout  x:close  H:inactive  W:workers  tab:next  q:quit",
                Tab::Metrics => "enter:view transcript  j/k:select  r:refresh  tab:next  q:quit",
            },
            View::Popup => "j/k:scroll  esc:close  q:close",
            View::LiveTail => "j/k:scroll  G:bottom  esc:close",
            View::ConfirmClose => "y:confirm  n/esc:cancel",
            View::StateDebug => "j/k:scroll  esc:close  q:close",
            View::PromptInput => "type prompt  enter:create task  esc:cancel",
            View::ModelPicker if app
                .model_picker
                .as_ref()
                .is_some_and(|picker| picker.has_multiple_agents()) =>
            {
                "type:filter  ↑/↓:move  tab:agent  enter/space:start  esc:cancel"
            }
            View::ModelPicker => "type:filter  ↑/↓:move  enter/space:start  esc:cancel",
            View::AgentPicker => "j/k:move  enter/space:select default  esc:cancel",
            View::TaskLinkPicker => "type:filter  ↑/↓:move  enter/space:link  esc:cancel",
            View::RenameSession => "edit title  enter:rename  esc:cancel",
            View::TodoPanel => {
                if app.todo.adding {
                    "type task  enter:add  esc:cancel"
                } else {
                    "j/k:move  space/enter:toggle  a:add  d:delete  c:clear done  t/esc:close"
                }
            }
            View::TmuxPane => "forwarding keys to tmux · F1: detach & close",
            View::FolderPicker => match app.folder_picker.as_ref().map(|p| p.mode) {
                Some(PickerMode::Bookmarks) => {
                    "j/k:move  enter/space:pick  m:unbookmark  esc:cancel"
                }
                Some(PickerMode::Places) => {
                    "type:filter  ↑/↓:move  enter/space:pick  tab:browse folders  esc:cancel"
                }
                _ if app.tasks.pending_assign.is_some() => {
                    "j/k:move  enter:descend  bksp:parent  space:pick  .:pick cwd  tab:projects  esc:cancel"
                }
                _ => {
                    "j/k:move  enter:descend  bksp:parent  space:pick  .:pick cwd  m:bookmark  c/C:gh new (pub/priv)  esc:cancel"
                }
            },
            View::GhCreateInput => "type name  tab:toggle public/private  enter:create  esc:cancel",
            View::TaskInput => {
                if app.tasks.renaming.is_some() {
                    "edit task  enter:rename  esc:cancel"
                } else {
                    "type task  enter:add  esc:cancel"
                }
            }
            View::TaskTags => "edit tags  space/comma separates  enter:save  esc:cancel",
            View::TaskInfo => {
                "j/k:attachment  a:attach  p:paste note  c:copy path  o:open  x:remove  PgUp/PgDn:scroll  esc/v:close"
            }
            View::TaskAttachInput => {
                if app.tasks.attach_note {
                    "type note  tab:file/URL  enter:attach  esc:cancel"
                } else {
                    "paste path or URL  tab:note  enter:attach  esc:cancel"
                }
            }
            View::TaskFilter => "type to filter (text or #tag)  enter:apply  esc:clear",
            View::ProjectsResult => "j/k:artifact  e:expand  PgUp/PgDn:scroll  c:copy path  o:xdg-open  esc/r:close",
            View::Backlog => "j/k:select  s/enter:start  x:delete  esc/q:close",
        };
        // Render the Space chip *first* so the single highest-value verb
        // (approve on Projects, ack on Sessions) is never the first thing
        // clipped off the right edge of this one-row, no-wrap status bar.
        let space_verb = match (&app.view, app.current_tab) {
            // Space is status-aware on the Tasks board: it approves a
            // focused Planning card's plan, and toggles Done elsewhere.
            (View::Grid, Tab::Tasks) => Some(match app.selected_board_task().map(|t| t.status) {
                Some(crate::orchestrator::TaskStatus::Planning) => "proceed ",
                _ => "done ",
            }),
            (View::Grid, Tab::Projects) => Some("approve "),
            (View::Grid, Tab::Sessions) => Some("ack "),
            _ => None,
        };
        if let Some(verb) = space_verb {
            spans.push(Span::styled(
                " Space ",
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ));
            spans.push(Span::styled(
                verb,
                Style::default()
                    .fg(Color::LightCyan)
                    .add_modifier(Modifier::BOLD),
            ));
        }
        spans.push(Span::styled(
            format!(" {} ", keybinds),
            Style::default().fg(Color::DarkGray),
        ));
    }

    // Pending dispatch indicator — visible when a freshly-spawned session
    // has a queued prompt that hasn't fired yet. Without this the user has
    // no way to tell that "session sitting there empty" actually has a
    // dispatch in flight, or that it's about to time out.
    if let Some(target) = app.pending_dispatch_target() {
        let age = app.pending_dispatch_age().map(|d| d.as_secs()).unwrap_or(0);
        let queued = app.pending_dispatch_count();
        let suffix = if queued > 1 {
            format!(" +{}", queued - 1)
        } else {
            String::new()
        };
        spans.push(Span::styled(
            format!(" ↻ dispatch waiting [{}{}] {}s ", target, suffix, age),
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ));
    }

    spans.push(Span::styled(
        format!("refreshed {} ", refresh_text),
        Style::default().fg(Color::DarkGray),
    ));

    frame.render_widget(
        Paragraph::new(Line::from(spans)).style(Style::default().bg(Color::Rgb(30, 30, 30))),
        area,
    );
}
