//! The one-row status bar: status message or per-view key hints, the pending
//! dispatch indicator, and the refresh age.

use crate::app::{App, Tab, View};
use crate::config;
use crate::folder_picker::PickerMode;
use crate::models;
use crate::ui::{agents, builds};
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

pub(super) fn render_status_bar(frame: &mut Frame, area: Rect, app: &App) {
    let elapsed = app.last_refresh.elapsed().as_secs();
    let refresh_text = if elapsed < 2 {
        "just now".to_string()
    } else {
        models::relative_age(elapsed)
    };

    let fresh_status = app
        .status_msg
        .as_ref()
        .filter(|(_, ts)| ts.elapsed() < config::get().ui.status_msg_ttl())
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
                // High-value verbs lead so they survive the right-edge
                // truncation at narrow widths (the status bar is one row, no
                // wrap); rare verbs trail. The Space chip is rendered
                // separately *ahead* of this string below so it is never the
                // first thing clipped.
                Tab::Tasks => "a/n:add  enter/f:focus agent  v:info  s:assign agent  S:agent in ~  h/l:col  j/k:task  H/L:move  /:filter  1-4:priority  t:tags  T:kind  r:rename  A:attach  p:paste note  x:delete  u:undo  c:clear done  tab:next  q:quit",
                Tab::Sessions => "enter/f:focus/resume  /:find any  n:new  h:handoff  R:respawn  A:default agent  N:new+model  p:new in…  i:info  r:rename  L:link task  o:shell  M:bookmarks  ←/j/k/l:nav  v:layout  x:close  H:inactive  tab:next  q:quit",
                Tab::Metrics => "enter:view transcript  j/k:select  r:refresh  tab:next  q:quit",
                Tab::Agents => agents::hints(app),
                Tab::Builds => builds::hints(app),
            },
            View::AgentDetail => agents::hints(app),
            View::BuildForm | View::BuildLog => builds::hints(app),
            View::Popup => "j/k:scroll  esc:close  q:close",
            View::LiveTail => "j/k:scroll  G:bottom  esc:close",
            View::ConfirmClose => "y:confirm  n/esc:cancel",
            View::ModelPicker if app
                .model_picker
                .as_ref()
                .is_some_and(|picker| picker.has_multiple_agents()) =>
            {
                "type:filter  ↑/↓:move  tab:agent  enter/space:start  esc:cancel"
            }
            View::ModelPicker => "type:filter  ↑/↓:move  enter/space:start  esc:cancel",
            View::AgentPicker => "j/k:move  enter/space:select default  esc:cancel",
            View::RespawnPicker => "j/k:move  enter/space:respawn  esc:cancel",
            View::TaskLinkPicker => "type:filter  ↑/↓:move  enter/space:link  esc:cancel",
            View::SessionFinder => "type:filter  ↑/↓:move  enter:open  esc:cancel",
            View::RenameSession => "edit title  enter:rename  esc:cancel",
            View::TmuxPane => "forwarding keys to tmux · F1: detach & close",
            View::FolderPicker => match app.folder_picker.as_ref().map(|p| p.mode) {
                Some(PickerMode::Bookmarks) => {
                    "j/k:move  enter/space:pick  m:unbookmark  esc:cancel"
                }
                Some(PickerMode::Places) => {
                    "type:filter  ↑/↓:move  enter/space:pick  tab:browse folders  esc:cancel"
                }
                _ if app.tasks.pending_assign.is_some() => {
                    "j/k:move  enter:descend  bksp:parent  space:pick  .:pick cwd  tab:places  esc:cancel"
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
                    "type task  tab:context  paste lands in context  enter:add  esc:cancel"
                }
            }
            View::TaskTags => "edit tags  space/comma separates  enter:save  esc:cancel",
            View::TaskKindPicker => "j/k:move  enter/space:set kind  esc:cancel",
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
        };
        // Render the Space chip *first* so the single highest-value verb
        // (proceed/done on Tasks, ack on Sessions) is never the first thing
        // clipped off the right edge of this one-row, no-wrap status bar.
        let space_verb = match (&app.view, app.current_tab) {
            // Space is status-aware on the Tasks board: it approves a
            // focused Planning card's plan, and toggles Done elsewhere.
            (View::Grid, Tab::Tasks) => Some(match app.selected_board_task().map(|t| t.status) {
                Some(crate::tasks::store::TaskStatus::Planning) => "proceed ",
                _ => "done ",
            }),
            (View::Grid, Tab::Sessions) => Some("ack "),
            (View::Grid, Tab::Builds) => builds::space_verb(app),
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
