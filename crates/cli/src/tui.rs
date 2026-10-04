//! `autoresolve tui`: a terminal view of a run's event log (.autoresolve/events.jsonl).
//! Live mode follows a run that is still going (start `fix` in another terminal), replay mode
//! plays a finished run back from the start. Read-only: it never calls a model or touches the repo.

use anyhow::Result;
use autoresolve_core::events::{self, Event};
use ratatui::{
    DefaultTerminal, Frame,
    crossterm::event::{self, Event as CEvent, KeyCode, KeyEventKind},
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap},
};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

struct App {
    path: PathBuf,
    run: Option<String>, // a run the user asked for; None = always the newest run in the log
    events: Vec<Event>,
    shown: usize, // events visible so far (replay reveals them gradually)
    sel: usize,
    follow: bool,
    replay: bool,
    paused: bool,
    speed: usize,
    show_summary: bool,
    last_len: u64,
    list_state: ListState,
    scroll: u16,
    fork: Option<String>,
    note: String,
}

pub fn run(root: &Path, run: Option<String>) -> Result<()> {
    let path = root.join(".autoresolve").join(events::LOG_FILE);
    let mut app = App {
        path,
        run,
        events: Vec::new(),
        shown: 0,
        sel: 0,
        follow: true,
        replay: false,
        paused: false,
        speed: 1,
        show_summary: false,
        last_len: u64::MAX,
        list_state: ListState::default(),
        scroll: 0,
        fork: None,
        note: String::new(),
    };
    app.reload();
    let mut terminal = ratatui::init();
    let res = app.main_loop(&mut terminal);
    ratatui::restore();
    if let Some(cmd) = &app.fork {
        println!("{cmd}");
    }
    res
}

impl App {
    fn main_loop(&mut self, terminal: &mut DefaultTerminal) -> Result<()> {
        let mut last_poll = Instant::now();
        loop {
            terminal.draw(|f| self.draw(f))?;
            if event::poll(Duration::from_millis(100))? {
                if let CEvent::Key(k) = event::read()? {
                    if k.kind == KeyEventKind::Press && self.key(k.code) {
                        return Ok(());
                    }
                }
            }
            self.tick();
            if last_poll.elapsed() >= Duration::from_millis(500) {
                self.reload();
                last_poll = Instant::now();
            }
        }
    }

    /// Re-read the log when it grew. Without --run the newest run is shown.
    fn reload(&mut self) {
        let Ok(meta) = std::fs::metadata(&self.path) else { return };
        if meta.len() == self.last_len {
            return;
        }
        self.last_len = meta.len();
        let Ok(all) = events::read_events(&self.path, None) else { return };
        let run = match &self.run {
            Some(r) => r.clone(),
            None => match all.last() {
                Some(e) => e.run.clone(),
                None => return,
            },
        };
        self.events = all.into_iter().filter(|e| e.run == run).collect();
        if !self.replay {
            self.shown = self.events.len();
        }
        self.shown = self.shown.min(self.events.len());
        if self.follow {
            self.sel = self.shown.saturating_sub(1);
        }
        self.sel = self.sel.min(self.shown.saturating_sub(1));
    }

    fn tick(&mut self) {
        if self.replay && !self.paused && self.shown < self.events.len() {
            self.shown = (self.shown + self.speed).min(self.events.len());
            if self.follow {
                self.sel = self.shown.saturating_sub(1);
                self.scroll = 0;
            }
        }
    }

    fn mv(&mut self, d: i64) {
        let max = self.shown.saturating_sub(1) as i64;
        self.sel = (self.sel as i64 + d).clamp(0, max) as usize;
        self.follow = false;
        self.scroll = 0;
    }

    fn jump_issue(&mut self, forward: bool) {
        let top = self.shown;
        let found = if forward {
            (self.sel + 1..top).find(|&i| self.events[i].kind == "issue_start")
        } else {
            (0..self.sel).rev().find(|&i| self.events[i].kind == "issue_start")
        };
        if let Some(i) = found {
            self.sel = i;
            self.follow = false;
            self.scroll = 0;
        }
    }

    /// Returns true to quit.
    fn key(&mut self, code: KeyCode) -> bool {
        let last = self.shown.saturating_sub(1);
        self.note.clear();
        match code {
            KeyCode::Char('q') | KeyCode::Esc => return true,
            KeyCode::Down | KeyCode::Char('j') => self.mv(1),
            KeyCode::Up | KeyCode::Char('k') => self.mv(-1),
            KeyCode::PageDown => self.mv(15),
            KeyCode::PageUp => self.mv(-15),
            KeyCode::Home | KeyCode::Char('g') => {
                self.sel = 0;
                self.follow = false;
                self.scroll = 0;
            }
            KeyCode::End | KeyCode::Char('G') => {
                self.sel = last;
                self.follow = true;
                self.scroll = 0;
            }
            KeyCode::Char('f') => {
                self.follow = !self.follow;
                if self.follow {
                    self.sel = last;
                }
            }
            KeyCode::Char('r') => {
                self.replay = true;
                self.paused = false;
                self.follow = true;
                self.shown = self.events.len().min(1);
                self.sel = 0;
                self.scroll = 0;
            }
            KeyCode::Char('l') => {
                self.replay = false;
                self.shown = self.events.len();
                self.sel = self.shown.saturating_sub(1);
            }
            KeyCode::Char(' ') => self.paused = !self.paused,
            KeyCode::Char('+') | KeyCode::Char('=') => self.speed = (self.speed * 2).min(64),
            KeyCode::Char('-') => self.speed = (self.speed / 2).max(1),
            KeyCode::Tab => self.show_summary = !self.show_summary,
            KeyCode::Char('n') => self.jump_issue(true),
            KeyCode::Char('p') => self.jump_issue(false),
            KeyCode::Char('J') => self.scroll = self.scroll.saturating_add(3),
            KeyCode::Char('K') => self.scroll = self.scroll.saturating_sub(3),
            KeyCode::Char('x') => self.make_fork(),
            _ => {}
        }
        false
    }

    /// "Fork": build the command that re-runs the issue under the cursor from its bug report.
    fn make_fork(&mut self) {
        if self.shown == 0 {
            return;
        }
        match fork_command(&self.events[..self.shown], self.sel.min(self.shown - 1)) {
            Some(cmd) => {
                self.note = "fork command ready; it prints when you quit (q)".into();
                self.fork = Some(cmd);
            }
            None => self.note = "no issue at or before the cursor".into(),
        }
    }

    fn draw(&mut self, f: &mut Frame) {
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(4), Constraint::Min(5), Constraint::Length(2)])
            .split(f.area());

        // ---- header
        let ev = &self.events[..self.shown];
        let issues = ev.iter().filter(|e| e.kind == "issue_start").count();
        let turns = ev.iter().filter(|e| e.kind == "model_turn").count();
        let flag = |e: &Event, k: &str| e.data[k].as_bool().unwrap_or(false);
        let pass = ev.iter().filter(|e| e.kind == "check" && flag(e, "passed")).count();
        let fail = ev.iter().filter(|e| e.kind == "check" && !flag(e, "passed")).count();
        let verified = ev.iter().filter(|e| e.kind == "outcome" && flag(e, "verified")).count();
        let proven = ev.iter().filter(|e| e.kind == "outcome" && flag(e, "proven")).count();
        let done = ev.iter().any(|e| e.kind == "run_end");
        let mode = if self.replay {
            format!("REPLAY x{}{}", self.speed, if self.paused { " (paused)" } else { "" })
        } else if done {
            "FINISHED".to_string()
        } else {
            "LIVE".to_string()
        };
        let elapsed = match (ev.first(), ev.last()) {
            (Some(a), Some(b)) => b.ts_ms.saturating_sub(a.ts_ms) / 1000,
            _ => 0,
        };
        let run_id = ev.first().map(|e| e.run.clone()).unwrap_or_else(|| "-".into());
        let head = vec![
            Line::from(vec![
                Span::styled(" AutoResolve ", Style::default().fg(Color::Black).bg(Color::Cyan).add_modifier(Modifier::BOLD)),
                Span::raw(format!("  run {run_id}   ")),
                Span::styled(mode, Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
                Span::raw(format!("   {}/{} events   {elapsed}s", self.shown, self.events.len())),
            ]),
            Line::from(format!(
                " issues {issues}   model turns {turns}   checks {pass} pass / {fail} fail   verified {verified}   proven {proven}"
            )),
        ];
        f.render_widget(Paragraph::new(head).block(Block::default().borders(Borders::BOTTOM)), rows[0]);

        // ---- event list (left) and detail or per-role summary (right)
        let cols = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(42), Constraint::Percentage(58)])
            .split(rows[1]);
        let t0 = self.events.first().map(|e| e.ts_ms).unwrap_or(0);
        let items: Vec<ListItem> = ev
            .iter()
            .map(|e| {
                ListItem::new(Line::from(vec![
                    Span::styled(
                        format!("{:>4} {:>6.1}s ", e.seq, e.ts_ms.saturating_sub(t0) as f64 / 1000.0),
                        Style::default().fg(Color::DarkGray),
                    ),
                    Span::styled(format!("{:<10} ", e.role), Style::default().fg(role_color(&e.role))),
                    Span::styled(summary(e), kind_style(e)),
                ]))
            })
            .collect();
        self.list_state.select(if self.shown == 0 { None } else { Some(self.sel.min(self.shown - 1)) });
        let list = List::new(items)
            .block(Block::default().borders(Borders::ALL).title(" events "))
            .highlight_style(Style::default().bg(Color::DarkGray).add_modifier(Modifier::BOLD))
            .highlight_symbol("> ");
        f.render_stateful_widget(list, cols[0], &mut self.list_state);

        let (title, lines): (&str, Vec<Line>) = if self.show_summary {
            (" per-role summary ", events::summarize(ev).lines().map(|l| Line::from(l.to_string())).collect())
        } else if self.shown == 0 {
            (
                " waiting ",
                vec![Line::from("no events yet. Start `autoresolve fix <file>` in another terminal; this view follows it live.")],
            )
        } else {
            (" detail ", detail_lines(&self.events[self.sel.min(self.shown - 1)]))
        };
        let detail = Paragraph::new(lines)
            .block(Block::default().borders(Borders::ALL).title(title))
            .wrap(Wrap { trim: false })
            .scroll((self.scroll, 0));
        f.render_widget(detail, cols[1]);

        // ---- footer
        let keys = " q quit  j/k move  n/p issue  g/G top/end  f follow  r replay  l live  space pause  +/- speed  tab summary  J/K scroll  x fork";
        let foot = vec![
            Line::from(Span::styled(keys, Style::default().fg(Color::DarkGray))),
            Line::from(Span::styled(format!(" {}", self.note), Style::default().fg(Color::Yellow))),
        ];
        f.render_widget(Paragraph::new(foot), rows[2]);
    }
}

fn role_color(role: &str) -> Color {
    let base = role.split('_').next().unwrap_or(role);
    match base {
        "reviewer" => Color::Cyan,
        "skeptic" => Color::Magenta,
        "tester" => Color::Yellow,
        "guard" => Color::Blue,
        "fixer" => Color::Green,
        "gate" => Color::LightRed,
        "still" => Color::LightMagenta,
        _ => Color::Gray,
    }
}

fn kind_style(e: &Event) -> Style {
    let flag = |k: &str| e.data[k].as_bool().unwrap_or(false);
    match e.kind.as_str() {
        "check" => Style::default().fg(if flag("passed") { Color::Green } else { Color::Red }),
        "outcome" => {
            if flag("proven") {
                Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)
            } else if flag("verified") {
                Style::default().fg(Color::Yellow)
            } else {
                Style::default().fg(Color::Red)
            }
        }
        "issue_start" => Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
        "model_error" | "retry" => Style::default().fg(Color::Red),
        "model_turn" | "tool_call" | "paced" => Style::default().fg(Color::Gray),
        "terminal" | "terminal_forced" => Style::default().add_modifier(Modifier::BOLD),
        _ => Style::default(),
    }
}

fn summary(e: &Event) -> String {
    let d = &e.data;
    let s = |k: &str| d[k].as_str().unwrap_or("").to_string();
    match e.kind.as_str() {
        "model_turn" => {
            let tools: Vec<String> = d["tool_calls"]
                .as_array()
                .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
                .unwrap_or_default();
            format!("turn {} ({} ms) {}", d["step"], d["ms"], tools.join(","))
        }
        "tool_call" => format!("{}{}", s("name"), if d["repeated"].as_bool().unwrap_or(false) { " (repeat)" } else { "" }),
        "terminal" | "terminal_forced" => format!("submit: {}", s("tool")),
        "check" => format!("{} {}", if d["passed"].as_bool().unwrap_or(false) { "PASS" } else { "FAIL" }, s("name")),
        "issue_start" => format!("ISSUE {}", s("title")),
        "outcome" => format!("outcome verified={} proven={}", d["verified"], d["proven"]),
        "repro" | "guard" => format!("{} ok={}", e.kind, d["ok"]),
        _ => e.kind.clone(),
    }
}

fn push_text(out: &mut Vec<Line<'static>>, text: &str, style: Style) {
    for l in text.lines() {
        out.push(Line::from(Span::styled(l.to_string(), style)));
    }
}

fn bold() -> Style {
    Style::default().add_modifier(Modifier::BOLD)
}

fn diff_lines(args: &Value, out: &mut Vec<Line<'static>>) {
    if let Some(s) = args["summary"].as_str() {
        push_text(out, s, Style::default().add_modifier(Modifier::ITALIC));
        out.push(Line::from(""));
    }
    for ed in args["edits"].as_array().into_iter().flatten() {
        let file = ed["file"].as_str().unwrap_or("?");
        out.push(Line::from(Span::styled(format!("--- {file}"), Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD))));
        for l in ed["search"].as_str().unwrap_or("").lines() {
            out.push(Line::from(Span::styled(format!("- {l}"), Style::default().fg(Color::Red))));
        }
        for l in ed["replace"].as_str().unwrap_or("").lines() {
            out.push(Line::from(Span::styled(format!("+ {l}"), Style::default().fg(Color::Green))));
        }
        out.push(Line::from(""));
    }
}

fn terminal_lines(d: &Value, out: &mut Vec<Line<'static>>) {
    let tool = d["tool"].as_str().unwrap_or("?");
    let args = &d["args"];
    out.push(Line::from(Span::styled(format!("submitted: {tool}"), bold())));
    out.push(Line::from(""));
    match tool {
        "submit_patch" => diff_lines(args, out),
        "submit_test" | "submit_guard" => {
            if let Some(s) = args["description"].as_str() {
                push_text(out, s, Style::default().add_modifier(Modifier::ITALIC));
            }
            if let Some(s) = args["expected_failure"].as_str() {
                push_text(out, &format!("expected failure: {s}"), Style::default().fg(Color::Yellow));
            }
            out.push(Line::from(""));
            push_text(out, args["code"].as_str().unwrap_or(""), Style::default().fg(Color::Gray));
        }
        "submit_findings" => {
            for f in args["findings"].as_array().into_iter().flatten() {
                let head = format!(
                    "[{}] {}:{}  {}",
                    f["severity"].as_str().unwrap_or("?").to_uppercase(),
                    f["file"].as_str().unwrap_or("?"),
                    f["line"],
                    f["title"].as_str().unwrap_or("")
                );
                out.push(Line::from(Span::styled(head, Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD))));
                push_text(out, f["explanation"].as_str().unwrap_or(""), Style::default());
                push_text(out, &format!("fix: {}", f["fix"].as_str().unwrap_or("")), Style::default().fg(Color::Green));
                out.push(Line::from(""));
            }
        }
        _ => {
            if let Some(obj) = args.as_object() {
                for (k, v) in obj {
                    match v.as_bool() {
                        Some(b) => {
                            // gate answers: true is a problem, except for the skeptic's plain verdict text
                            let color = if b { Color::Red } else { Color::Green };
                            out.push(Line::from(vec![
                                Span::styled(format!("{k}: "), bold()),
                                Span::styled(b.to_string(), Style::default().fg(color)),
                            ]));
                        }
                        None => {
                            let text = v.as_str().map(String::from).unwrap_or_else(|| v.to_string());
                            out.push(Line::from(Span::styled(format!("{k}:"), bold())));
                            push_text(out, &text, Style::default());
                        }
                    }
                }
            }
        }
    }
}

fn detail_lines(e: &Event) -> Vec<Line<'static>> {
    let mut out: Vec<Line<'static>> = Vec::new();
    out.push(Line::from(Span::styled(format!("#{}  {}  {}", e.seq, e.role, e.kind), bold())));
    out.push(Line::from(""));
    let d = &e.data;
    match e.kind.as_str() {
        "terminal" | "terminal_forced" => terminal_lines(d, &mut out),
        "check" => {
            let ok = d["passed"].as_bool().unwrap_or(false);
            out.push(Line::from(Span::styled(
                if ok { "PASS" } else { "FAIL" },
                Style::default().fg(if ok { Color::Green } else { Color::Red }).add_modifier(Modifier::BOLD),
            )));
            push_text(&mut out, d["name"].as_str().unwrap_or(""), bold());
            push_text(&mut out, d["detail"].as_str().unwrap_or(""), Style::default());
        }
        "model_turn" => {
            push_text(&mut out, &format!("step {}   {} ms", d["step"], d["ms"]), Style::default().fg(Color::DarkGray));
            push_text(&mut out, d["text"].as_str().unwrap_or(""), Style::default());
        }
        "tool_call" => {
            push_text(&mut out, &format!("{}({})", d["name"].as_str().unwrap_or(""), d["args"]), bold());
            out.push(Line::from(""));
            push_text(&mut out, d["result"].as_str().unwrap_or(""), Style::default().fg(Color::Gray));
        }
        _ => {
            let pretty = serde_json::to_string_pretty(d).unwrap_or_default();
            push_text(&mut out, &pretty, Style::default());
        }
    }
    out
}

fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// The command that re-runs the issue at or before `at` from its original bug report.
fn fork_command(events: &[Event], at: usize) -> Option<String> {
    let ix = events[..=at].iter().rposition(|e| e.kind == "issue_start")?;
    let d = &events[ix].data;
    let title = d["title"].as_str().unwrap_or("");
    let file = d["file"].as_str().unwrap_or("");
    let explanation = events[..ix]
        .iter()
        .rev()
        .filter(|e| e.kind == "terminal" && e.data["tool"] == "submit_findings")
        .flat_map(|e| e.data["args"]["findings"].as_array().cloned().unwrap_or_default())
        .find(|f| f["title"].as_str() == Some(title))
        .and_then(|f| f["explanation"].as_str().map(String::from))
        .unwrap_or_default();
    let report = if explanation.is_empty() { title.to_string() } else { format!("{title}: {explanation}") };
    Some(format!("autoresolve fix {} --issue {}", shell_quote(file), shell_quote(&report)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ev(seq: u64, role: &str, kind: &str, data: Value) -> Event {
        Event { seq, ts_ms: seq * 1000, run: "r".into(), role: role.into(), kind: kind.into(), data }
    }

    #[test]
    fn fork_command_rebuilds_the_bug_report_and_quotes_it_safely() {
        let events = vec![
            ev(1, "reviewer", "terminal", json!({"tool": "submit_findings", "args": {"findings": [
                {"title": "Bare except", "explanation": "it's too broad", "file": "p.py", "line": 5, "severity": "low", "fix": "x"}]}})),
            ev(2, "controller", "issue_start", json!({"title": "Bare except", "file": "p.py"})),
            ev(3, "fixer", "model_turn", json!({})),
        ];
        let cmd = fork_command(&events, 2).unwrap();
        assert!(cmd.starts_with("autoresolve fix 'p.py' --issue 'Bare except: it'\\''s too broad'"));
        assert!(fork_command(&events, 0).is_none()); // before any issue
    }

    #[test]
    fn a_patch_is_drawn_as_a_diff_and_summaries_are_short() {
        let e = ev(1, "fixer", "terminal", json!({"tool": "submit_patch", "args": {"summary": "s", "edits": [
            {"file": "a.py", "search": "except:", "replace": "except ValueError:"}]}}));
        let text: Vec<String> = detail_lines(&e).iter().map(|l| l.spans.iter().map(|s| s.content.to_string()).collect::<String>()).collect();
        assert!(text.iter().any(|l| l == "- except:") && text.iter().any(|l| l == "+ except ValueError:"));
        assert_eq!(summary(&e), "submit: submit_patch");
        let c = ev(2, "controller", "check", json!({"name": "lint", "passed": false}));
        assert_eq!(summary(&c), "FAIL lint");
    }
}
