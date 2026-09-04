use crate::app::{App, ModalCandidate, backend_short_badge, format_participation_badge};
use crate::backend::domain_supports_open_settings;
use crate::dsp::{
    DB_FLOOR, NUM_SPECTRUM_BANDS, SCOPE_WINDOW_FRAMES, VisMode, db_to_unit, heatmap_rgb,
    summarize_take_voice_bars,
};
use crate::hit_test::{ClickAction, HitMap, ModalTab};
use capture_system_or_process_audio::{
    CombineMode, CombineResolution, DefaultDeviceMode, DiscoveryDedupe, DiscoveryOrder, DitherMode,
    InputConversionPolicy, OnRootExit, Overflow, ParticipationState, PermissionEntry,
    PermissionKind, PermissionPolicy, PermissionReport, PermissionState, PlatformOs, ProcessScope,
    ProcessTreeStrategy, TargetKind, schema::TargetAcquisition,
};
use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    symbols::Marker,
    text::{Line, Span},
    widgets::{
        Block, BorderType, Borders, Clear, Paragraph, Wrap,
        canvas::{Canvas, Line as CanvasLine},
    },
};

pub fn render(frame: &mut Frame, app: &App, hit_map: &mut HitMap) {
    hit_map.clear();
    let area = frame.area();

    let top_panel_len = if area.height >= 36 { 11 } else { 10 };
    let root_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Length(top_panel_len),
            Constraint::Min(9),
            Constraint::Length(8),
            Constraint::Length(4),
            Constraint::Length(1),
        ])
        .split(area);

    let top_row = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(54), Constraint::Percentage(46)])
        .split(root_chunks[1]);
    let bottom_row = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(46), Constraint::Percentage(54)])
        .split(root_chunks[3]);

    let panels = [
        (top_row[0], 54, 10),
        (top_row[1], 46, 10),
        (root_chunks[2], 100, 9),
        (bottom_row[0], 46, 8),
        (bottom_row[1], 54, 8),
        (root_chunks[4], 100, 4),
    ];
    if panels
        .iter()
        .any(|&(panel, width, height)| panel_below_half(panel, width, height))
    {
        frame.render_widget(
            Paragraph::new("Expand terminal to show all panels")
                .style(
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                )
                .wrap(Wrap { trim: true }),
            area,
        );
        return;
    }

    render_header(frame, root_chunks[0], app, hit_map);
    render_staged_sources(frame, top_row[0], app, hit_map);
    render_config_and_plan(frame, top_row[1], app, hit_map);
    render_spectroscope(frame, root_chunks[2], app, hit_map);
    render_telemetry(frame, bottom_row[0], app);
    render_takes_and_playback(frame, bottom_row[1], app, hit_map);
    render_telegram_voice_strip(frame, root_chunks[4], app, hit_map);
    render_footer(frame, root_chunks[5], app);

    for (panel, width, height) in panels {
        if u32::from(panel.width) < width || u32::from(panel.height) < height {
            let warning = Rect::new(
                panel.x.saturating_add(1),
                panel.y.saturating_add(panel.height.saturating_sub(2)),
                panel.width.saturating_sub(2),
                1,
            );
            hit_map.remove_overlapping(warning);
            frame.render_widget(Clear, warning);
            frame.render_widget(
                Paragraph::new("Panel not fully fit").style(
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                ),
                warning,
            );
        }
    }

    if app.modal.open {
        render_add_source_modal(frame, area, app, hit_map);
    }
    if app.permissions_modal.open {
        hit_map.clear();
        render_permissions_modal(frame, area, app, hit_map);
    }
}

fn panel_below_half(panel: Rect, width: u32, height: u32) -> bool {
    u32::from(panel.width) * 2 < width || u32::from(panel.height) * 2 < height
}

fn summarize_header_permissions(app: &App) -> (&'static str, Style) {
    if app.permission_runner.is_in_flight() {
        return (
            "[Perms:ASK..(K)]",
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        );
    }
    let Ok(rep) = &app.permission_report else {
        return (
            "[Perms: ERR (K)]",
            Style::default()
                .fg(Color::LightRed)
                .add_modifier(Modifier::BOLD),
        );
    };

    let any_required = rep.entries().iter().any(|e| e.required);
    let is_relevant = |e: &PermissionEntry| {
        if any_required {
            e.required
        } else {
            e.kind != PermissionKind::ScreenRecording || rep.os == PlatformOs::MacOs
        }
    };

    if rep
        .entries()
        .iter()
        .filter(|e| is_relevant(e))
        .any(|e| e.state.is_denied_or_restricted())
    {
        return (
            "[Perms:DENY!(K)]",
            Style::default()
                .fg(Color::Black)
                .bg(Color::LightRed)
                .add_modifier(Modifier::BOLD),
        );
    }
    if rep
        .entries()
        .iter()
        .filter(|e| is_relevant(e))
        .any(|e| e.state == PermissionState::Granted && e.requires_relaunch)
    {
        return (
            "[Perms:RLNCH(K)]",
            Style::default()
                .fg(Color::Black)
                .bg(Color::LightMagenta)
                .add_modifier(Modifier::BOLD),
        );
    }
    if rep
        .entries()
        .iter()
        .filter(|e| is_relevant(e))
        .any(|e| e.state == PermissionState::NotDetermined)
    {
        return (
            "[Perms:ASK? (K)]",
            Style::default()
                .fg(Color::Black)
                .bg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        );
    }
    if rep
        .entries()
        .iter()
        .filter(|e| is_relevant(e))
        .any(|e| e.state == PermissionState::Unknown)
    {
        return (
            "[Perms:UNPRB(K)]",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        );
    }
    (
        "[Perms: OK  (K)]",
        Style::default()
            .fg(Color::LightGreen)
            .add_modifier(Modifier::BOLD),
    )
}

fn render_header(frame: &mut Frame, area: Rect, app: &App, hit_map: &mut HitMap) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Min(17),
            Constraint::Length(18),
            Constraint::Length(16),
            Constraint::Length(17),
            Constraint::Length(15),
            Constraint::Length(15),
        ])
        .split(inner);

    let state_span = if app.is_recording {
        Span::styled(
            format!(
                " ● REC Take #{} ({:.1}s | seg #{}) ",
                app.active_take_id,
                (app.live_frames as f32) / (app.live_format.rate().max(1) as f32),
                app.live_segments
            ),
            Style::default()
                .fg(Color::Black)
                .bg(Color::Red)
                .add_modifier(Modifier::BOLD),
        )
    } else if app.playback_status.active {
        Span::styled(
            format!(
                " ▶ PLAY Take #{} ({:.1}s/{:.1}s) ",
                app.playback_status.take_id.unwrap_or(0),
                app.playback_status.position_secs(),
                app.playback_status.duration_secs()
            ),
            Style::default()
                .fg(Color::Black)
                .bg(Color::Green)
                .add_modifier(Modifier::BOLD),
        )
    } else if app.live_monitor_enabled {
        Span::styled(
            " ◉ LIVE MONITOR (No Rec) ",
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )
    } else {
        Span::styled(
            " ■ IDLE (Monitor Off) ",
            Style::default()
                .fg(Color::Black)
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        )
    };

    let title_line = Line::from(vec![
        Span::styled(
            " CSPA Studio ",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        state_span,
    ]);
    frame.render_widget(Paragraph::new(title_line), cols[0]);

    let add_btn = Paragraph::new(Line::from(Span::styled(
        "[+ Add Source (A)]",
        Style::default()
            .fg(Color::Black)
            .bg(Color::LightCyan)
            .add_modifier(Modifier::BOLD),
    )))
    .alignment(Alignment::Center);
    frame.render_widget(add_btn, cols[1]);
    hit_map.register(cols[1], ClickAction::OpenAddSourceModal);

    let (rec_label, rec_bg) = if app.is_recording {
        ("[■ Stop Rec (R)]", Color::LightRed)
    } else {
        ("[● Record (R)]", Color::LightGreen)
    };
    let rec_btn = Paragraph::new(Line::from(Span::styled(
        rec_label,
        Style::default()
            .fg(Color::Black)
            .bg(rec_bg)
            .add_modifier(Modifier::BOLD),
    )))
    .alignment(Alignment::Center);
    frame.render_widget(rec_btn, cols[2]);
    hit_map.register(cols[2], ClickAction::ToggleRecordStop);

    let (perm_lbl, perm_style) = summarize_header_permissions(app);
    let perm_btn =
        Paragraph::new(Line::from(Span::styled(perm_lbl, perm_style))).alignment(Alignment::Center);
    frame.render_widget(perm_btn, cols[3]);
    hit_map.register(cols[3], ClickAction::OpenPermissionsModal);

    let (mon_label, mon_fg) = if app.live_monitor_enabled {
        ("[Mon: ON (M)]", Color::LightGreen)
    } else {
        ("[Mon: OFF (M)]", Color::DarkGray)
    };
    let mon_btn = Paragraph::new(Line::from(Span::styled(
        mon_label,
        Style::default().fg(mon_fg).add_modifier(Modifier::BOLD),
    )))
    .alignment(Alignment::Center);
    frame.render_widget(mon_btn, cols[4]);
    hit_map.register(cols[4], ClickAction::ToggleLiveMonitor);

    let (synth_label, synth_fg) = if app.synthetic_mode {
        ("[Synth: ON (T)]", Color::Yellow)
    } else {
        ("[Synth:OFF (T)]", Color::Gray)
    };
    let synth_btn = Paragraph::new(Line::from(Span::styled(
        synth_label,
        Style::default().fg(synth_fg).add_modifier(Modifier::BOLD),
    )))
    .alignment(Alignment::Center);
    frame.render_widget(synth_btn, cols[5]);
    hit_map.register(cols[5], ClickAction::ToggleSyntheticSignal);
}

fn render_staged_sources(frame: &mut Frame, area: Rect, app: &App, hit_map: &mut HitMap) {
    let title = format!(
        " [1] Staged Audio Sources ({}/{} checked - live hot-swap) ",
        app.staged_sources.iter().filter(|s| s.enabled).count(),
        app.staged_sources.len()
    );
    let block = Block::default()
        .title(Span::styled(
            title,
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if inner.height < 2 {
        return;
    }
    let list_height = inner.height.saturating_sub(1) as usize;

    for (idx, src) in app.staged_sources.iter().enumerate().take(list_height) {
        let row_rect = Rect {
            x: inner.x,
            y: inner.y + idx as u16,
            width: inner.width,
            height: 1,
        };
        let is_selected = idx == app.selected_source_idx;
        let check_str = if src.enabled { "[x]" } else { "[ ]" };
        let check_color = if src.enabled {
            Color::LightGreen
        } else {
            Color::DarkGray
        };

        let name_width = row_rect.width.saturating_sub(39).max(12);
        let toggle_rect = Rect {
            x: row_rect.x,
            y: row_rect.y,
            width: name_width,
            height: 1,
        };
        hit_map.register(toggle_rect, ClickAction::ToggleStagedSource(idx));

        let perm_entry = app.target_permission_entry(&src.target);
        let perm_denied =
            src.enabled && perm_entry.is_some_and(|e| e.state.is_denied_or_restricted());
        let perm_relaunch = src.enabled
            && perm_entry
                .is_some_and(|e| e.state == PermissionState::Granted && e.requires_relaunch);

        let (badge_txt, badge_style) = if perm_denied {
            (
                format!("[{}!] ", src.kind_badge()),
                Style::default()
                    .fg(Color::LightRed)
                    .add_modifier(Modifier::BOLD),
            )
        } else if perm_relaunch {
            (
                format!("[{}~] ", src.kind_badge()),
                Style::default()
                    .fg(Color::LightMagenta)
                    .add_modifier(Modifier::BOLD),
            )
        } else {
            let badge_fg = match src.target.kind() {
                TargetKind::DefaultInput | TargetKind::InputDevice => Color::LightMagenta,
                _ => Color::Cyan,
            };
            (
                format!("[{}] ", src.kind_badge()),
                Style::default().fg(badge_fg),
            )
        };
        let spans = vec![
            Span::styled(
                if is_selected { "> " } else { "  " },
                Style::default().fg(Color::Yellow),
            ),
            Span::styled(
                format!("{check_str} "),
                Style::default()
                    .fg(check_color)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(badge_txt, badge_style),
            Span::styled(
                truncate_str(
                    &src.display_name(),
                    (name_width as usize).saturating_sub(12),
                ),
                Style::default().fg(if src.enabled {
                    Color::White
                } else {
                    Color::DarkGray
                }),
            ),
        ];
        frame.render_widget(Paragraph::new(Line::from(spans)), toggle_rect);

        if row_rect.width > name_width + 18 {
            let chips_x = row_rect.x + name_width;
            let chips_w = row_rect.width - name_width;
            let chip_cols = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([
                    Constraint::Length(8),
                    Constraint::Length(7),
                    Constraint::Length(8),
                    Constraint::Length(8),
                    Constraint::Length(5),
                    Constraint::Min(3),
                ])
                .split(Rect {
                    x: chips_x,
                    y: row_rect.y,
                    width: chips_w,
                    height: 1,
                });

            let bk_txt = format!("[{}]", src.backend_badge());
            let bk_color = if src.target.backend().is_some() {
                Color::LightCyan
            } else {
                Color::Gray
            };
            frame.render_widget(
                Paragraph::new(Span::styled(bk_txt, Style::default().fg(bk_color))),
                chip_cols[0],
            );
            hit_map.register(chip_cols[0], ClickAction::CycleStagedBackend(idx));

            let gain_txt = format!("[{:.2}x]", src.target.gain())
                .replace(".00x", ".0x")
                .replace(".50x", ".5x");
            let gain_color = if (src.target.gain() - 1.0).abs() < 1e-4 {
                Color::Gray
            } else {
                Color::LightYellow
            };
            frame.render_widget(
                Paragraph::new(Span::styled(gain_txt, Style::default().fg(gain_color))),
                chip_cols[1],
            );
            hit_map.register(chip_cols[1], ClickAction::CycleStagedGain(idx));

            match src.target.kind() {
                TargetKind::Application | TargetKind::Process => {
                    let scope_txt = match src.target.scope() {
                        ProcessScope::Tree => "[tree]",
                        ProcessScope::Exact => "[exact]",
                    };
                    frame.render_widget(
                        Paragraph::new(Span::styled(
                            scope_txt,
                            Style::default().fg(Color::LightBlue),
                        )),
                        chip_cols[2],
                    );
                    hit_map.register(chip_cols[2], ClickAction::CycleStagedScope(idx));

                    let exit_txt = match src.target.on_root_exit() {
                        OnRootExit::Stop => "[stop]",
                        OnRootExit::Wait { .. } => "[wait5s]",
                    };
                    frame.render_widget(
                        Paragraph::new(Span::styled(
                            exit_txt,
                            Style::default().fg(Color::LightMagenta),
                        )),
                        chip_cols[3],
                    );
                    hit_map.register(chip_cols[3], ClickAction::CycleStagedExitPolicy(idx));
                }
                TargetKind::DefaultOutput | TargetKind::DefaultInput => {
                    let mode_txt = match src.target.default_device_mode() {
                        DefaultDeviceMode::Follow => "[follow]",
                        DefaultDeviceMode::Pin => "[pin]",
                    };
                    frame.render_widget(
                        Paragraph::new(Span::styled(
                            mode_txt,
                            Style::default().fg(Color::LightBlue),
                        )),
                        chip_cols[2],
                    );
                    hit_map.register(chip_cols[2], ClickAction::CycleStagedDeviceMode(idx));
                }
                TargetKind::Device | TargetKind::InputDevice => {}
            }

            frame.render_widget(
                Paragraph::new(Span::styled("[+2x]", Style::default().fg(Color::Yellow))),
                chip_cols[4],
            );
            hit_map.register(chip_cols[4], ClickAction::DuplicateStagedSource(idx));

            frame.render_widget(
                Paragraph::new(Span::styled("[x]", Style::default().fg(Color::LightRed))),
                chip_cols[5],
            );
            hit_map.register(chip_cols[5], ClickAction::RemoveStagedSource(idx));
        }
    }

    let footer_y = inner.y + inner.height - 1;
    let btn_cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(16),
            Constraint::Length(10),
            Constraint::Length(11),
            Constraint::Min(8),
        ])
        .split(Rect {
            x: inner.x,
            y: footer_y,
            width: inner.width,
            height: 1,
        });

    frame.render_widget(
        Paragraph::new(Span::styled(
            "[+ Add Source]",
            Style::default()
                .fg(Color::LightCyan)
                .add_modifier(Modifier::BOLD),
        )),
        btn_cols[0],
    );
    hit_map.register(btn_cols[0], ClickAction::OpenAddSourceModal);

    frame.render_widget(
        Paragraph::new(Span::styled("[All On]", Style::default().fg(Color::Green))),
        btn_cols[1],
    );
    hit_map.register(btn_cols[1], ClickAction::EnableAllStaged);

    frame.render_widget(
        Paragraph::new(Span::styled("[All Off]", Style::default().fg(Color::Gray))),
        btn_cols[2],
    );
    hit_map.register(btn_cols[2], ClickAction::DisableAllStaged);

    frame.render_widget(
        Paragraph::new(Span::styled(
            "Click row to toggle",
            Style::default().fg(Color::DarkGray),
        )),
        btn_cols[3],
    );
}

fn render_config_and_plan(frame: &mut Frame, area: Rect, app: &App, hit_map: &mut HitMap) {
    let block = Block::default()
        .title(Span::styled(
            " [2] Capture Plan & Config (click chips to change) ",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if inner.height < 6 {
        return;
    }

    let rows = (0..inner.height)
        .map(|i| Rect {
            x: inner.x,
            y: inner.y + i,
            width: inner.width,
            height: 1,
        })
        .collect::<Vec<_>>();

    let wrap_options = inner.width < 43 && rows.len() >= 7;
    let opt_cols = if wrap_options {
        let first =
            Layout::horizontal([Constraint::Length(14), Constraint::Min(14)]).split(rows[0]);
        [first[0], first[1], rows[1]]
    } else {
        let columns = Layout::horizontal([
            Constraint::Length(14),
            Constraint::Length(15),
            Constraint::Min(14),
        ])
        .split(rows[0]);
        [columns[0], columns[1], columns[2]]
    };

    let comb_txt = match app.combine_mode {
        CombineMode::Auto => "[Comb:Auto]",
        CombineMode::NativeOnly => "[Comb:Native]",
        CombineMode::CspaMixer => "[Comb:Mixer]",
    };
    frame.render_widget(
        Paragraph::new(Span::styled(
            comb_txt,
            Style::default()
                .fg(Color::LightCyan)
                .add_modifier(Modifier::BOLD),
        )),
        opt_cols[0],
    );
    hit_map.register(opt_cols[0], ClickAction::CycleCombineMode);

    let tree_txt = match (
        app.tree_strategy.start_level,
        app.tree_strategy.fallback_limit,
    ) {
        (
            ProcessTreeStrategy::NativeKernelCapture,
            Some(ProcessTreeStrategy::SoftwareStreamMix),
        ) => "[Tree:Krn..Sw]",
        (ProcessTreeStrategy::NativeKernelCapture, Some(ProcessTreeStrategy::NativeGraphMix)) => {
            "[Tree:Krn..Gr]"
        }
        (ProcessTreeStrategy::NativeKernelCapture, None) => "[Tree:KrnOnly]",
        _ => "[Tree:SwOnly]",
    };
    frame.render_widget(
        Paragraph::new(Span::styled(
            tree_txt,
            Style::default().fg(Color::LightBlue),
        )),
        opt_cols[1],
    );
    hit_map.register(opt_cols[1], ClickAction::CycleTreeStrategy);

    let in_conv_txt = match app.input_conversion {
        InputConversionPolicy::AutoWhenMixing => "[In:AutoResmp]",
        InputConversionPolicy::StrictNoResample => "[In:NoResampl]",
    };
    frame.render_widget(
        Paragraph::new(Span::styled(
            in_conv_txt,
            Style::default().fg(
                if app.input_conversion == InputConversionPolicy::StrictNoResample {
                    Color::LightRed
                } else {
                    Color::LightGreen
                },
            ),
        )),
        opt_cols[2],
    );
    hit_map.register(opt_cols[2], ClickAction::ToggleInputConversion);

    let conv_cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(17),
            Constraint::Length(12),
            Constraint::Min(10),
        ])
        .split(rows[1 + usize::from(wrap_options)]);
    let conv_badge = if app.use_converter {
        "Converter:[ON ]"
    } else {
        "Converter:[OFF]"
    };
    frame.render_widget(
        Paragraph::new(Span::styled(
            conv_badge,
            Style::default().fg(if app.use_converter {
                Color::LightGreen
            } else {
                Color::Gray
            }),
        )),
        conv_cols[0],
    );
    hit_map.register(conv_cols[0], ClickAction::ToggleFormatConverter);

    frame.render_widget(
        Paragraph::new(Span::styled(
            format!("[{} Hz]", app.converter_rate),
            Style::default().fg(Color::LightYellow),
        )),
        conv_cols[1],
    );
    hit_map.register(conv_cols[1], ClickAction::CycleSampleRate);

    let ch_txt = if app.converter_channels == 1 {
        "[Mono]"
    } else {
        "[Stereo]"
    };
    frame.render_widget(
        Paragraph::new(Span::styled(
            ch_txt,
            Style::default().fg(Color::LightYellow),
        )),
        conv_cols[2],
    );
    hit_map.register(conv_cols[2], ClickAction::CycleChannels);

    let controls_row = 2 + usize::from(wrap_options);
    let wrap_controls = inner.width < 49 && rows.len() > controls_row + 1;
    let fmt_cols = if wrap_controls {
        let first = Layout::horizontal([Constraint::Length(8), Constraint::Min(13)])
            .split(rows[controls_row]);
        let second = Layout::horizontal([Constraint::Length(15), Constraint::Min(12)])
            .split(rows[controls_row + 1]);
        [first[0], first[1], second[0], second[1]]
    } else {
        let columns = Layout::horizontal([
            Constraint::Length(8),
            Constraint::Length(14),
            Constraint::Length(15),
            Constraint::Min(12),
        ])
        .split(rows[controls_row]);
        [columns[0], columns[1], columns[2], columns[3]]
    };

    frame.render_widget(
        Paragraph::new(Span::styled(
            format!("[{:?}]", app.converter_sample),
            Style::default().fg(Color::LightMagenta),
        )),
        fmt_cols[0],
    );
    hit_map.register(fmt_cols[0], ClickAction::CycleSampleFormat);

    let dither_txt = match app.converter_dither {
        DitherMode::Tpdf => "[Dither:TPDF]",
        DitherMode::None => "[Dither:None]",
    };
    frame.render_widget(
        Paragraph::new(Span::styled(dither_txt, Style::default().fg(Color::Cyan))),
        fmt_cols[1],
    );
    hit_map.register(fmt_cols[1], ClickAction::CycleDither);

    let sil_txt = if app.silence_on_idle {
        "[Idle:Silence]"
    } else {
        "[Idle:Park]"
    };
    frame.render_widget(
        Paragraph::new(Span::styled(sil_txt, Style::default().fg(Color::Green))),
        fmt_cols[2],
    );
    hit_map.register(fmt_cols[2], ClickAction::ToggleSilenceOnIdle);

    let ovf_txt = match app.overflow_policy {
        Overflow::DropOldest => "[DropOldest]",
        Overflow::DropNewest => "[DropNewest]",
        Overflow::Fail => "[Ovf:Fail]",
    };
    frame.render_widget(
        Paragraph::new(Span::styled(ovf_txt, Style::default().fg(Color::Yellow))),
        fmt_cols[3],
    );
    hit_map.register(fmt_cols[3], ClickAction::CycleOverflowPolicy);

    let plan_row = controls_row + if wrap_controls { 2 } else { 1 };
    match &app.current_plan {
        Ok(plan) => {
            let res_txt = match plan.combine_resolution {
                CombineResolution::PassThrough(b) => {
                    format!("PassThrough({})", backend_short_badge(Some(b)))
                }
                CombineResolution::SharedNativeGraphMix(b) => {
                    format!("NativeGraphMix({})", backend_short_badge(Some(b)))
                }
                CombineResolution::CspaMixer => "CspaMixer (48kHz Stereo F32)".to_owned(),
            };
            if rows.len() > plan_row {
                frame.render_widget(
                    Paragraph::new(Line::from(vec![
                        Span::styled("Combine : ", Style::default().fg(Color::Gray)),
                        Span::styled(
                            truncate_str(&res_txt, (inner.width as usize).saturating_sub(10)),
                            Style::default()
                                .fg(Color::LightGreen)
                                .add_modifier(Modifier::BOLD),
                        ),
                    ])),
                    rows[plan_row],
                );
            }
            if rows.len() > plan_row + 1 {
                let per_target: Vec<String> = plan
                    .target_plans
                    .iter()
                    .map(|tp| {
                        let acq = match tp.acquisition {
                            TargetAcquisition::DirectEndpoint => "Endpt",
                            TargetAcquisition::ProcessTree(
                                ProcessTreeStrategy::NativeKernelCapture,
                            ) => "Kernel",
                            TargetAcquisition::ProcessTree(ProcessTreeStrategy::NativeGraphMix) => {
                                "Graph"
                            }
                            TargetAcquisition::ProcessTree(
                                ProcessTreeStrategy::SoftwareStreamMix,
                            ) => "SwMix",
                        };
                        format!(
                            "#{}:{}:{acq}",
                            tp.target_index,
                            backend_short_badge(Some(tp.resolved_backend))
                        )
                    })
                    .collect();
                frame.render_widget(
                    Paragraph::new(Line::from(vec![
                        Span::styled("Targets : ", Style::default().fg(Color::Gray)),
                        Span::styled(
                            truncate_str(
                                &per_target.join(", "),
                                (inner.width as usize).saturating_sub(10),
                            ),
                            Style::default().fg(Color::White),
                        ),
                    ])),
                    rows[plan_row + 1],
                );
            }
            if rows.len() > plan_row + 2 {
                frame.render_widget(
                    Paragraph::new(Line::from(vec![
                        Span::styled("Budgets : ", Style::default().fg(Color::Gray)),
                        Span::styled(
                            truncate_str(
                                &format!(
                                    "{} str, {} KiB | {:?} (dup:{})",
                                    plan.estimated_active_streams,
                                    plan.estimated_pcm_bytes / 1024,
                                    plan.overlap_semantics,
                                    plan.duplicate_target_ids.len()
                                ),
                                (inner.width as usize).saturating_sub(10),
                            ),
                            Style::default().fg(Color::Cyan),
                        ),
                    ])),
                    rows[plan_row + 2],
                );
            }
        }
        Err(err) => {
            if rows.len() > plan_row {
                frame.render_widget(
                    Paragraph::new(Span::styled(
                        format!("Plan Reject : {err}"),
                        Style::default()
                            .fg(Color::LightRed)
                            .add_modifier(Modifier::BOLD),
                    )),
                    rows[plan_row],
                );
            }
        }
    }

    let perm_start_row = if app.current_plan.is_ok() {
        plan_row + 3
    } else {
        plan_row + 1
    };
    if rows.len() > perm_start_row {
        render_inline_permissions_rows(frame, &rows[perm_start_row..], app, hit_map);
    }
}

fn format_compact_domain_chip(os: PlatformOs, entry: PermissionEntry) -> Span<'static> {
    let name = match entry.kind {
        PermissionKind::Microphone => "Mic",
        PermissionKind::SystemAudio => "Sys",
        PermissionKind::ScreenRecording => "Scr",
    };
    if !entry.required && entry.kind == PermissionKind::ScreenRecording && os != PlatformOs::MacOs {
        return Span::styled("Scr:-- ", Style::default().fg(Color::DarkGray));
    }
    let (code, fg) = match entry.state {
        PermissionState::Granted if entry.requires_relaunch => ("RLN", Color::LightMagenta),
        PermissionState::Granted => (
            "OK",
            if entry.required {
                Color::LightGreen
            } else {
                Color::DarkGray
            },
        ),
        PermissionState::NotDetermined => (
            "ASK",
            if entry.required {
                Color::Yellow
            } else {
                Color::Gray
            },
        ),
        PermissionState::Unknown => (
            "UNK",
            if entry.required {
                Color::Yellow
            } else {
                Color::DarkGray
            },
        ),
        PermissionState::Denied => (
            "DNY",
            if entry.required {
                Color::LightRed
            } else {
                Color::Red
            },
        ),
        PermissionState::Restricted => (
            "RST",
            if entry.required {
                Color::LightRed
            } else {
                Color::DarkGray
            },
        ),
    };
    let star = if entry.required { "*" } else { "" };
    let mut style = Style::default().fg(fg);
    if entry.required {
        style = style.add_modifier(Modifier::BOLD);
    }
    Span::styled(format!("{name}:{code}{star} "), style)
}

fn render_inline_permissions_rows(
    frame: &mut Frame,
    perm_rows: &[Rect],
    app: &App,
    hit_map: &mut HitMap,
) {
    let row0 = perm_rows[0];
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Min(20), Constraint::Length(12)])
        .split(row0);

    let summary_line = match &app.permission_report {
        Ok(rep) => Line::from(vec![
            Span::styled("Perm: ", Style::default().fg(Color::Gray)),
            format_compact_domain_chip(rep.os, rep.microphone()),
            format_compact_domain_chip(rep.os, rep.system_audio()),
            format_compact_domain_chip(rep.os, rep.screen_recording()),
        ]),
        Err(err) => Line::from(Span::styled(
            truncate_str(&format!("Perm Err: {err}"), cols[0].width as usize),
            Style::default().fg(Color::LightRed),
        )),
    };
    frame.render_widget(Paragraph::new(summary_line), cols[0]);
    hit_map.register(cols[0], ClickAction::OpenPermissionsModal);

    let (pol_lbl, pol_fg) = match app.permission_policy {
        PermissionPolicy::AutoRequest => ("[Pol:AutoRq]", Color::LightGreen),
        PermissionPolicy::CheckOnly => ("[Pol:ChkOnl]", Color::Yellow),
    };
    frame.render_widget(
        Paragraph::new(Span::styled(
            pol_lbl,
            Style::default().fg(pol_fg).add_modifier(Modifier::BOLD),
        )),
        cols[1],
    );
    hit_map.register(cols[1], ClickAction::TogglePermissionPolicy);

    if perm_rows.len() > 1 {
        let act_cols = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Length(11),
                Constraint::Length(14),
                Constraint::Min(14),
            ])
            .split(perm_rows[1]);

        frame.render_widget(
            Paragraph::new(Span::styled("[Re-Check]", Style::default().fg(Color::Cyan))),
            act_cols[0],
        );
        hit_map.register(act_cols[0], ClickAction::RefreshPermissions);

        let can_ask_any = app.permission_report.as_ref().is_ok_and(|rep| {
            rep.entries().iter().any(|e| {
                e.required
                    && (e.can_prompt
                        || (rep.os == PlatformOs::MacOs
                            && e.kind == PermissionKind::SystemAudio
                            && e.state == PermissionState::Unknown))
            })
        });
        let (ask_lbl, ask_style) = if app.permission_runner.is_in_flight() {
            (
                "[Asking OS..]",
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )
        } else if can_ask_any {
            (
                "[Ask Needed]",
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            )
        } else {
            ("[Ask Needed]", Style::default().fg(Color::Gray))
        };
        frame.render_widget(
            Paragraph::new(Span::styled(ask_lbl, ask_style)),
            act_cols[1],
        );
        hit_map.register(act_cols[1], ClickAction::RequestRequiredPermissions);

        let needs_attention = app
            .permission_report
            .as_ref()
            .is_ok_and(|r| r.has_denial() || r.requires_relaunch());
        let ui_style = if needs_attention {
            Style::default()
                .fg(Color::LightRed)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::LightCyan)
        };
        frame.render_widget(
            Paragraph::new(Span::styled("[Perms UI (K)]", ui_style)),
            act_cols[2],
        );
        hit_map.register(act_cols[2], ClickAction::OpenPermissionsModal);
    }
}

fn render_spectroscope(frame: &mut Frame, area: Rect, app: &App, hit_map: &mut HitMap) {
    let block = Block::default()
        .title(Span::styled(
            format!(
                " [3] Live Spectroscope - {} (Press V or click tab) ",
                app.vis_mode.label()
            ),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if inner.height < 4 || inner.width < 20 {
        return;
    }

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(3),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .split(inner);

    let tab_cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(24),
            Constraint::Length(24),
            Constraint::Length(24),
            Constraint::Min(4),
        ])
        .split(chunks[0]);

    for (idx, mode) in [
        VisMode::FftSpectrum,
        VisMode::Waterfall,
        VisMode::Oscilloscope,
    ]
    .into_iter()
    .enumerate()
    {
        let active = app.vis_mode == mode;
        let style = if active {
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::Gray)
        };
        let label = match mode {
            VisMode::FftSpectrum => " [1] FFT Spectrum ",
            VisMode::Waterfall => " [2] Waterfall Map ",
            VisMode::Oscilloscope => " [3] Oscilloscope ",
        };
        frame.render_widget(Paragraph::new(Span::styled(label, style)), tab_cols[idx]);
        hit_map.register(tab_cols[idx], ClickAction::SelectVisMode(mode));
    }

    let (mon_tab_txt, mon_tab_color) = if app.live_monitor_enabled {
        (" [◉ Idle Monitor: ON (M)] ", Color::LightGreen)
    } else {
        (" [○ Idle Monitor: OFF (M)] ", Color::DarkGray)
    };
    frame.render_widget(
        Paragraph::new(Span::styled(
            mon_tab_txt,
            Style::default()
                .fg(mon_tab_color)
                .add_modifier(Modifier::BOLD),
        ))
        .alignment(Alignment::Right),
        tab_cols[3],
    );
    hit_map.register(tab_cols[3], ClickAction::ToggleLiveMonitor);

    match app.vis_mode {
        VisMode::FftSpectrum => render_fft_bars(frame, chunks[1], app),
        VisMode::Waterfall => render_waterfall_heatmap(frame, chunks[1], app),
        VisMode::Oscilloscope => render_braille_oscilloscope(frame, chunks[1], app),
    }

    render_frequency_ruler(frame, chunks[2]);

    render_stereo_meters(frame, chunks[3], app);
}

fn render_frequency_ruler(frame: &mut Frame, area: Rect) {
    let width = usize::from(area.width);
    if width < 9 || area.height == 0 {
        return;
    }
    let labels = [
        (20.0_f64, "20Hz"),
        (20_000.0, "20kHz"),
        (1_000.0, "1kHz"),
        (50.0, "50Hz"),
        (100.0, "100Hz"),
        (250.0, "250Hz"),
        (500.0, "500Hz"),
        (2_000.0, "2kHz"),
        (4_000.0, "4kHz"),
        (8_000.0, "8kHz"),
        (16_000.0, "16kHz"),
    ];
    let mut occupied = vec![false; width];
    for (frequency, label) in labels {
        let center =
            ((frequency / 20.0).ln() / 1000.0_f64.ln() * (width - 1) as f64).round() as usize;
        let start = center
            .saturating_sub(label.len() / 2)
            .min(width - label.len());
        let end = start + label.len();
        let padded_start = start.saturating_sub(1);
        let padded_end = (end + 1).min(width);
        if occupied[padded_start..padded_end].iter().any(|used| *used) {
            continue;
        }
        occupied[start..end].fill(true);
        frame.render_widget(
            Paragraph::new(Span::styled(label, Style::default().fg(Color::DarkGray))),
            Rect::new(area.x + start as u16, area.y, label.len() as u16, 1),
        );
    }
}

fn render_fft_bars(frame: &mut Frame, area: Rect, app: &App) {
    let width = area.width as usize;
    let height = area.height as usize;
    if width == 0 || height == 0 {
        return;
    }
    let buf = frame.buffer_mut();
    let bar_chars = [' ', '▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];

    for col in 0..width {
        let band_idx = (col * NUM_SPECTRUM_BANDS / width).min(NUM_SPECTRUM_BANDS - 1);
        let db = app.spectroscope.smoothed_bands_db[band_idx];
        let peak_db = app.spectroscope.peak_hold_db[band_idx];

        let unit = db_to_unit(db);
        let peak_unit = db_to_unit(peak_db);

        let total_eighths = (unit * (height as f32) * 8.0).round() as usize;
        let peak_row_from_bottom = ((peak_unit * ((height.saturating_sub(1)) as f32)).round()
            as usize)
            .min(height.saturating_sub(1));

        for row_from_bottom in 0..height {
            let y = area.y + (height - 1 - row_from_bottom) as u16;
            let x = area.x + col as u16;
            let row_eighths_start = row_from_bottom * 8;

            let row_ratio = (row_from_bottom as f32) / (height.max(1) as f32);
            let fg_color = if row_ratio > 0.82 {
                Color::LightRed
            } else if row_ratio > 0.58 {
                Color::Yellow
            } else if row_ratio > 0.30 {
                Color::LightGreen
            } else {
                Color::Cyan
            };

            if total_eighths > row_eighths_start {
                let fill = (total_eighths - row_eighths_start).min(8);
                let ch = bar_chars[fill];
                if let Some(cell) = buf.cell_mut((x, y)) {
                    cell.set_char(ch).set_fg(fg_color);
                }
            } else if row_from_bottom == peak_row_from_bottom
                && peak_db > DB_FLOOR + 2.0
                && let Some(cell) = buf.cell_mut((x, y))
            {
                cell.set_char('▔').set_fg(Color::White);
            }
        }
    }
}

fn render_waterfall_heatmap(frame: &mut Frame, area: Rect, app: &App) {
    let width = area.width as usize;
    let height = area.height as usize;
    if width == 0 || height == 0 {
        return;
    }
    let buf = frame.buffer_mut();
    let rows_available = app.spectroscope.waterfall_rows.len();

    for ry in 0..height {
        let top_slice_idx = (ry * 2).min(rows_available.saturating_sub(1));
        let bot_slice_idx = (ry * 2 + 1).min(rows_available.saturating_sub(1));
        let top_row = &app.spectroscope.waterfall_rows[top_slice_idx];
        let bot_row = &app.spectroscope.waterfall_rows[bot_slice_idx];

        for col in 0..width {
            let band_idx = (col * NUM_SPECTRUM_BANDS / width).min(NUM_SPECTRUM_BANDS - 1);
            let (tr, tg, tb) = heatmap_rgb(db_to_unit(top_row[band_idx]));
            let (br, bg, bb) = heatmap_rgb(db_to_unit(bot_row[band_idx]));
            let x = area.x + col as u16;
            let y = area.y + ry as u16;
            if let Some(cell) = buf.cell_mut((x, y)) {
                cell.set_char('▀')
                    .set_fg(Color::Rgb(tr, tg, tb))
                    .set_bg(Color::Rgb(br, bg, bb));
            }
        }
    }
}

fn render_braille_oscilloscope(frame: &mut Frame, area: Rect, app: &App) {
    let samples = app.spectroscope.scope_triggered.clone();
    let len = samples.len().max(2);
    let canvas = Canvas::default()
        .marker(Marker::Braille)
        .x_bounds([0.0, (len - 1) as f64])
        .y_bounds([-1.05, 1.05])
        .paint(move |ctx| {
            ctx.draw(&CanvasLine {
                x1: 0.0,
                y1: 0.0,
                x2: (len - 1) as f64,
                y2: 0.0,
                color: Color::DarkGray,
            });
            for i in 0..samples.len().saturating_sub(1) {
                let (l0, r0) = samples[i];
                let (l1, r1) = samples[i + 1];
                ctx.draw(&CanvasLine {
                    x1: i as f64,
                    y1: f64::from(r0.clamp(-1.0, 1.0)),
                    x2: (i + 1) as f64,
                    y2: f64::from(r1.clamp(-1.0, 1.0)),
                    color: Color::LightMagenta,
                });
                ctx.draw(&CanvasLine {
                    x1: i as f64,
                    y1: f64::from(l0.clamp(-1.0, 1.0)),
                    x2: (i + 1) as f64,
                    y2: f64::from(l1.clamp(-1.0, 1.0)),
                    color: Color::Cyan,
                });
            }
        });
    let _ = SCOPE_WINDOW_FRAMES;
    frame.render_widget(canvas, area);
}

fn render_stereo_meters(frame: &mut Frame, area: Rect, app: &App) {
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(area);

    for (ch_idx, ch_name) in [(0usize, "L"), (1usize, "R")] {
        let peak = app.spectroscope.peak_dbfs[ch_idx];
        let rms = app.spectroscope.rms_dbfs[ch_idx];
        let unit = db_to_unit(peak);
        let bar_w = cols[ch_idx].width.saturating_sub(22) as usize;
        let filled = ((unit * (bar_w as f32)).round() as usize).min(bar_w);
        let empty = bar_w.saturating_sub(filled);
        let bar_color = if peak > -1.0 {
            Color::LightRed
        } else if peak > -12.0 {
            Color::Yellow
        } else {
            Color::LightGreen
        };
        let line = Line::from(vec![
            Span::styled(
                format!(" {ch_name} ["),
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled("█".repeat(filled), Style::default().fg(bar_color)),
            Span::styled("░".repeat(empty), Style::default().fg(Color::DarkGray)),
            Span::styled(
                format!("] {:+5.1}dB (rms {:+5.1})", peak, rms),
                Style::default().fg(Color::Gray),
            ),
        ]);
        frame.render_widget(Paragraph::new(line), cols[ch_idx]);
    }
}

fn render_telemetry(frame: &mut Frame, area: Rect, app: &App) {
    let block = Block::default()
        .title(Span::styled(
            " [4] Active Stream & Mixer Telemetry ",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let mut lines = Vec::new();
    let skew_txt = match (
        app.live_metadata.skew_estimate_ns,
        app.live_metadata.skew_uncertainty_ns,
    ) {
        (Some(ns), Some(unc)) => format!(
            "Skew {:.2}ms (±{:.2}ms)",
            (ns as f64) / 1_000_000.0,
            (unc as f64) / 1_000_000.0
        ),
        (Some(ns), None) => format!("Skew {:.2}ms", (ns as f64) / 1_000_000.0),
        _ => "Skew cross-clock".to_owned(),
    };

    lines.push(Line::from(vec![
        Span::styled("Session : ", Style::default().fg(Color::Gray)),
        Span::styled(
            format!(
                "{} frames ({:.1} KiB) | Segs: {} | Mask: 0x{:X}",
                app.live_frames,
                (app.live_bytes as f64) / 1024.0,
                app.live_segments,
                app.live_metadata.contributing_mask
            ),
            Style::default().fg(Color::White),
        ),
    ]));
    lines.push(Line::from(vec![
        Span::styled("CspaMixer: ", Style::default().fg(Color::Gray)),
        Span::styled(
            format!(
                "{skew_txt} | Clips:{} | Slips:{} | Late:{}",
                app.live_metadata.clipped_samples,
                app.live_metadata.drift_slips,
                app.live_metadata.late_inputs
            ),
            Style::default().fg(Color::Cyan),
        ),
    ]));

    if app.live_inputs.is_empty() {
        lines.push(Line::from(Span::styled(
            "Inputs  : Idle (press Record to monitor per-input normalizer & participation)",
            Style::default().fg(Color::DarkGray),
        )));
    } else {
        for row in app.live_inputs.iter().take(4) {
            let snap = &row.snapshot;
            let badge = format_participation_badge(snap.participation_state);
            let badge_color = match snap.participation_state {
                ParticipationState::Active => Color::LightGreen,
                ParticipationState::WaitingRelaunch => Color::Yellow,
                ParticipationState::Failed(_) => Color::LightRed,
                _ => Color::Gray,
            };
            let norm_badge = row.normalizer_badge();
            let norm_color = if row.is_resampled || !row.is_identity_bypass {
                Color::LightMagenta
            } else {
                Color::LightCyan
            };
            lines.push(Line::from(vec![
                Span::styled(
                    format!("#{:<1} [{}] ", snap.input_index, badge),
                    Style::default()
                        .fg(badge_color)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(format!("[{norm_badge}] "), Style::default().fg(norm_color)),
                Span::styled(
                    format!(
                        "{} (q:{} slp:{})",
                        truncate_str(&snap.target_id, 14),
                        snap.queued_packets,
                        snap.drift_slips
                    ),
                    Style::default().fg(Color::White),
                ),
            ]));
        }
    }

    frame.render_widget(Paragraph::new(lines), inner);
}

fn render_takes_and_playback(frame: &mut Frame, area: Rect, app: &App, hit_map: &mut HitMap) {
    let block = Block::default()
        .title(Span::styled(
            format!(" [5] In-Memory Takes ({}) & Playback ", app.takes.len()),
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if inner.height < 3 {
        return;
    }

    let list_rows = inner.height.saturating_sub(2) as usize;
    if app.takes.is_empty() {
        frame.render_widget(
            Paragraph::new(Span::styled(
                "  No takes recorded yet. Press [Record (R)] -> [Stop (R)] to capture into RAM.",
                Style::default().fg(Color::DarkGray),
            )),
            Rect {
                x: inner.x,
                y: inner.y,
                width: inner.width,
                height: 1,
            },
        );
    } else {
        for (idx, take) in app.takes.iter().enumerate().take(list_rows) {
            let row_rect = Rect {
                x: inner.x,
                y: inner.y + idx as u16,
                width: inner.width,
                height: 1,
            };
            let is_sel = idx == app.selected_take_idx;
            let is_playing_this =
                app.playback_status.active && app.playback_status.take_id == Some(take.id);

            let storage_badge = if take.saved_path.is_some() {
                Span::styled("[SAVED WAV]", Style::default().fg(Color::LightGreen))
            } else {
                Span::styled("[RAM ONLY ]", Style::default().fg(Color::LightYellow))
            };

            let line = Line::from(vec![
                Span::styled(
                    if is_sel { "> " } else { "  " },
                    Style::default().fg(Color::Yellow),
                ),
                Span::styled(
                    if is_playing_this { "▶ " } else { "  " },
                    Style::default().fg(Color::Green),
                ),
                Span::styled(
                    format!(
                        "Take #{:02} ({:5.1}s | {}Hz {}ch {:?} | segs:{}) ",
                        take.id,
                        take.duration_secs(),
                        take.format.rate(),
                        take.format.channels(),
                        take.format.sample(),
                        take.segments.len()
                    ),
                    Style::default().fg(if is_sel { Color::White } else { Color::Gray }),
                ),
                storage_badge,
            ]);
            frame.render_widget(Paragraph::new(line), row_rect);
            hit_map.register(row_rect, ClickAction::SelectTake(idx));
        }
    }

    let seek_y = inner.y + inner.height.saturating_sub(2);
    let seek_rect = Rect {
        x: inner.x,
        y: seek_y,
        width: inner.width,
        height: 1,
    };
    let ratio = app.playback_status.progress_ratio();
    let bar_w = seek_rect.width.saturating_sub(20) as usize;
    let pos_chars = ((ratio * (bar_w as f32)).round() as usize).min(bar_w);
    let rem_chars = bar_w.saturating_sub(pos_chars);
    let seek_line = Line::from(vec![
        Span::styled("Seek: [", Style::default().fg(Color::Gray)),
        Span::styled(
            "━".repeat(pos_chars),
            Style::default().fg(Color::LightGreen),
        ),
        Span::styled("●", Style::default().fg(Color::White)),
        Span::styled(
            "─".repeat(rem_chars.saturating_sub(1)),
            Style::default().fg(Color::DarkGray),
        ),
        Span::styled(
            format!(
                "] {:04.1}s/{:04.1}s",
                app.playback_status.position_secs(),
                app.playback_status.duration_secs()
            ),
            Style::default().fg(Color::Cyan),
        ),
    ]);
    frame.render_widget(Paragraph::new(seek_line), seek_rect);
    hit_map.register(
        Rect {
            x: seek_rect.x + 7,
            y: seek_rect.y,
            width: (bar_w as u16).max(1),
            height: 1,
        },
        ClickAction::SeekPlaybackBar {
            bar_x: seek_rect.x + 7,
            bar_width: (bar_w as u16).max(1),
        },
    );

    let btn_y = inner.y + inner.height - 1;
    let btn_cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(17),
            Constraint::Length(13),
            Constraint::Length(18),
            Constraint::Min(10),
        ])
        .split(Rect {
            x: inner.x,
            y: btn_y,
            width: inner.width,
            height: 1,
        });

    let play_txt = if app.playback_status.active && !app.playback_status.paused {
        "[■ Pause (P)]"
    } else {
        "[▶ Play RAM (P)]"
    };
    frame.render_widget(
        Paragraph::new(Span::styled(
            play_txt,
            Style::default()
                .fg(Color::LightGreen)
                .add_modifier(Modifier::BOLD),
        )),
        btn_cols[0],
    );
    hit_map.register(btn_cols[0], ClickAction::PlayPauseSelectedTake);

    frame.render_widget(
        Paragraph::new(Span::styled(
            "[■ Stop (S)]",
            Style::default().fg(Color::Yellow),
        )),
        btn_cols[1],
    );
    hit_map.register(btn_cols[1], ClickAction::StopPlayback);

    frame.render_widget(
        Paragraph::new(Span::styled(
            "[💾 Save WAV (W)]",
            Style::default()
                .fg(Color::Black)
                .bg(Color::LightYellow)
                .add_modifier(Modifier::BOLD),
        )),
        btn_cols[2],
    );
    hit_map.register(btn_cols[2], ClickAction::SaveSelectedTakeWav);

    frame.render_widget(
        Paragraph::new(Span::styled(
            "[Delete]",
            Style::default().fg(Color::LightRed),
        )),
        btn_cols[3],
    );
    hit_map.register(btn_cols[3], ClickAction::DeleteSelectedTake);
}

fn render_telegram_voice_strip(frame: &mut Frame, area: Rect, app: &App, hit_map: &mut HitMap) {
    let pill_bg = Color::Rgb(18, 25, 38);
    let border_color = if app.is_recording {
        Color::LightRed
    } else if app.playback_status.active {
        Color::LightGreen
    } else {
        Color::Rgb(58, 96, 138)
    };

    let strip_title = if app.is_recording {
        " 💬 Live Recording Voice Strip (Thick Capsule Waveform) "
    } else if app.playback_status.active || !app.takes.is_empty() {
        " 💬 Voice Strip (Click waveform to seek playback) "
    } else {
        " 💬 Live Monitor Voice Strip (Thick Capsule Waveform) "
    };

    let block = Block::default()
        .title(Span::styled(
            strip_title,
            Style::default()
                .fg(Color::Cyan)
                .bg(pill_bg)
                .add_modifier(Modifier::BOLD),
        ))
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(border_color).bg(pill_bg))
        .style(Style::default().bg(pill_bg));

    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.height < 2 || inner.width < 24 {
        return;
    }

    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(9),
            Constraint::Min(10),
            Constraint::Length(28),
        ])
        .split(inner);

    let (btn_top, btn_bot, btn_fg, btn_action) = if app.is_recording {
        (
            "  ■ REC ",
            "  Stop  ",
            Color::LightRed,
            ClickAction::ToggleRecordStop,
        )
    } else if app.playback_status.active && !app.playback_status.paused {
        (
            "  ■     ",
            "  Pause ",
            Color::LightGreen,
            ClickAction::PlayPauseSelectedTake,
        )
    } else if !app.takes.is_empty() {
        (
            "  ▶     ",
            "  Play  ",
            Color::LightCyan,
            ClickAction::PlayPauseSelectedTake,
        )
    } else {
        (
            "  ●     ",
            "  Rec   ",
            Color::LightRed,
            ClickAction::ToggleRecordStop,
        )
    };

    frame.render_widget(
        Paragraph::new(vec![
            Line::from(Span::styled(
                btn_top,
                Style::default()
                    .fg(btn_fg)
                    .bg(pill_bg)
                    .add_modifier(Modifier::BOLD),
            )),
            Line::from(Span::styled(
                btn_bot,
                Style::default()
                    .fg(btn_fg)
                    .bg(pill_bg)
                    .add_modifier(Modifier::BOLD),
            )),
        ]),
        cols[0],
    );
    hit_map.register(cols[0], btn_action);

    let wave_area = cols[1];
    let wave_w = wave_area.width as usize;

    let selected_take = if !app.is_recording {
        app.takes.get(app.selected_take_idx)
    } else {
        None
    };

    let bars: Vec<f32> = if let Some(take) = selected_take {
        summarize_take_voice_bars(&take.preview_stereo_f32, wave_w)
    } else {
        let hist_len = app.spectroscope.voice_pill_history.len();
        if hist_len >= wave_w {
            app.spectroscope
                .voice_pill_history
                .iter()
                .skip(hist_len - wave_w)
                .copied()
                .collect()
        } else {
            let mut v = vec![0.0; wave_w - hist_len];
            v.extend(app.spectroscope.voice_pill_history.iter().copied());
            v
        }
    };

    let progress_ratio = if selected_take.is_some() {
        if app.playback_status.active {
            app.playback_status.progress_ratio()
        } else {
            0.0
        }
    } else {
        1.0
    };
    let played_cols = ((progress_ratio * (wave_w as f32)).round() as usize).min(wave_w);

    let top_glyphs = [' ', '⡀', '⡄', '⡆', '⡇', '▍'];
    let bot_glyphs = ['·', '⠁', '⠃', '⠇', '⡇', '▍'];

    let buf = frame.buffer_mut();
    for (col_idx, &unit) in bars.iter().enumerate().take(wave_w) {
        let level = ((unit.clamp(0.0, 1.0) * 5.0).round() as usize).min(5);
        let top_ch = top_glyphs[level];
        let bot_ch = bot_glyphs[level];

        let is_playhead = selected_take.is_some()
            && app.playback_status.active
            && col_idx == played_cols.saturating_sub(1);
        let is_played = col_idx < played_cols;

        let fg = if app.is_recording {
            if level >= 4 {
                Color::Rgb(255, 110, 90)
            } else if level >= 2 {
                Color::Rgb(255, 75, 75)
            } else {
                Color::Rgb(165, 65, 75)
            }
        } else if is_playhead {
            Color::White
        } else if selected_take.is_some() {
            if is_played {
                if level >= 4 {
                    Color::Rgb(95, 242, 214)
                } else {
                    Color::Rgb(42, 171, 238)
                }
            } else if level == 0 {
                Color::Rgb(58, 74, 96)
            } else {
                Color::Rgb(78, 108, 142)
            }
        } else if level >= 4 {
            Color::Rgb(95, 242, 214)
        } else if level >= 1 {
            Color::Rgb(42, 171, 238)
        } else {
            Color::Rgb(58, 74, 96)
        };

        let x = wave_area.x + col_idx as u16;
        if let Some(c_top) = buf.cell_mut((x, wave_area.y)) {
            c_top.set_char(top_ch).set_fg(fg).set_bg(pill_bg);
        }
        if let Some(c_bot) = buf.cell_mut((x, wave_area.y + 1)) {
            c_bot.set_char(bot_ch).set_fg(fg).set_bg(pill_bg);
        }
    }

    if selected_take.is_some() && wave_area.width > 0 {
        hit_map.register(
            wave_area,
            ClickAction::SeekPlaybackBar {
                bar_x: wave_area.x,
                bar_width: wave_area.width,
            },
        );
    }

    let (meta_line1, meta_line2) = if app.is_recording {
        let secs = (app.live_frames as f32) / (app.live_format.rate().max(1) as f32);
        (
            Line::from(vec![
                Span::styled(
                    " ● REC ",
                    Style::default()
                        .fg(Color::LightRed)
                        .bg(pill_bg)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!("{secs:05.1}s (Take #{:02})", app.active_take_id),
                    Style::default().fg(Color::White).bg(pill_bg),
                ),
            ]),
            Line::from(Span::styled(
                format!(
                    " Seg #{} · {:+5.1} dBFS ",
                    app.live_segments, app.spectroscope.peak_dbfs[0]
                ),
                Style::default().fg(Color::Gray).bg(pill_bg),
            )),
        )
    } else if let Some(take) = selected_take {
        let pos_s = if app.playback_status.active && app.playback_status.take_id == Some(take.id) {
            app.playback_status.position_secs()
        } else {
            0.0
        };
        let dur_s = take.duration_secs();
        let badge = if take.saved_path.is_some() {
            "[SAVED]"
        } else {
            "[💾 WAV]"
        };
        (
            Line::from(Span::styled(
                format!(" {pos_s:05.1}s / {dur_s:05.1}s "),
                Style::default()
                    .fg(Color::White)
                    .bg(pill_bg)
                    .add_modifier(Modifier::BOLD),
            )),
            Line::from(vec![
                Span::styled(
                    format!(" Take #{:02}·{}kHz ", take.id, take.format.rate() / 1000),
                    Style::default().fg(Color::Cyan).bg(pill_bg),
                ),
                Span::styled(
                    badge,
                    Style::default()
                        .fg(Color::LightYellow)
                        .bg(pill_bg)
                        .add_modifier(Modifier::BOLD),
                ),
            ]),
        )
    } else {
        (
            Line::from(Span::styled(
                format!(" Peak {:+5.1} dBFS ", app.spectroscope.peak_dbfs[0]),
                Style::default().fg(Color::Cyan).bg(pill_bg),
            )),
            Line::from(Span::styled(
                " Click ● to Record Take ",
                Style::default().fg(Color::Gray).bg(pill_bg),
            )),
        )
    };

    frame.render_widget(
        Paragraph::new(vec![meta_line1, meta_line2]).alignment(Alignment::Right),
        cols[2],
    );
    if selected_take.is_some() {
        hit_map.register(cols[2], ClickAction::SaveSelectedTakeWav);
    }
}

fn render_footer(frame: &mut Frame, area: Rect, app: &App) {
    let line = if let Some(ref err) = app.error_banner {
        Line::from(vec![
            Span::styled(
                " ERROR ",
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::LightRed)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(format!(" {err}"), Style::default().fg(Color::LightRed)),
        ])
    } else {
        Line::from(vec![
            Span::styled(
                " STATUS ",
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!(" {} ", app.status_message),
                Style::default().fg(Color::White),
            ),
        ])
    };
    frame.render_widget(Paragraph::new(line), area);
}

fn render_add_source_modal(frame: &mut Frame, full_area: Rect, app: &App, hit_map: &mut HitMap) {
    let modal_w = (full_area.width * 84 / 100).clamp(40, full_area.width.saturating_sub(2));
    let modal_h = (full_area.height * 80 / 100).clamp(14, full_area.height.saturating_sub(2));
    let modal_rect = Rect {
        x: full_area.x + (full_area.width.saturating_sub(modal_w)) / 2,
        y: full_area.y + (full_area.height.saturating_sub(modal_h)) / 2,
        width: modal_w,
        height: modal_h,
    };

    frame.render_widget(Clear, modal_rect);
    let block = Block::default()
        .title(Span::styled(
            " Add Capture Sources (Live Discovery Subscription) ",
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan));
    let inner = block.inner(modal_rect);
    frame.render_widget(block, modal_rect);

    if inner.height < 7 {
        return;
    }

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Min(3),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .split(inner);

    let tab_cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(15),
            Constraint::Length(13),
            Constraint::Length(15),
            Constraint::Length(15),
            Constraint::Min(2),
            Constraint::Length(9),
        ])
        .split(rows[0]);

    for (idx, tab) in [
        ModalTab::Applications,
        ModalTab::Processes,
        ModalTab::OutputDevices,
        ModalTab::Microphones,
    ]
    .into_iter()
    .enumerate()
    {
        let active = app.modal.tab == tab;
        let style = if active {
            Style::default()
                .fg(Color::Black)
                .bg(Color::Yellow)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::Gray)
        };
        frame.render_widget(
            Paragraph::new(Span::styled(format!(" {} ", tab.label()), style)),
            tab_cols[idx],
        );
        hit_map.register(tab_cols[idx], ClickAction::SwitchModalTab(tab));
    }
    frame.render_widget(
        Paragraph::new(Span::styled(
            "[Close X]",
            Style::default()
                .fg(Color::LightRed)
                .add_modifier(Modifier::BOLD),
        )),
        tab_cols[5],
    );
    hit_map.register(tab_cols[5], ClickAction::CloseModal);

    let filter_cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Min(20),
            Constraint::Length(8),
            Constraint::Length(17),
            Constraint::Length(18),
            Constraint::Length(11),
        ])
        .split(rows[1]);

    let search_style = if app.modal.search_focused {
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::White)
    };
    frame.render_widget(
        Paragraph::new(Span::styled(
            format!("Search (/): [ {}_ ]", app.modal.search_query),
            search_style,
        )),
        filter_cols[0],
    );
    hit_map.register(filter_cols[0], ClickAction::FocusModalSearch);

    frame.render_widget(
        Paragraph::new(Span::styled("[Clear]", Style::default().fg(Color::Gray))),
        filter_cols[1],
    );
    hit_map.register(filter_cols[1], ClickAction::ClearModalSearch);

    let audio_chk = if app.modal.audio_only {
        "[x] AudioOnly(F)"
    } else {
        "[ ] AudioOnly(F)"
    };
    frame.render_widget(
        Paragraph::new(Span::styled(audio_chk, Style::default().fg(Color::Cyan))),
        filter_cols[2],
    );
    hit_map.register(filter_cols[2], ClickAction::ToggleModalAudioOnly);

    let play_chk = if app.modal.playing_only {
        "[x] PlayingOnly(P)"
    } else {
        "[ ] PlayingOnly(P)"
    };
    frame.render_widget(
        Paragraph::new(Span::styled(
            play_chk,
            Style::default().fg(Color::LightGreen),
        )),
        filter_cols[3],
    );
    hit_map.register(filter_cols[3], ClickAction::ToggleModalPlayingOnly);

    frame.render_widget(
        Paragraph::new(Span::styled(
            "[Refresh]",
            Style::default().fg(Color::Yellow),
        )),
        filter_cols[4],
    );
    hit_map.register(filter_cols[4], ClickAction::RefreshModalDiscovery);

    let query_cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(22),
            Constraint::Length(26),
            Constraint::Min(22),
        ])
        .split(rows[2]);

    let bk_filter_lbl = match app.modal.backend_filter {
        None => "[Backend: All (B)]".to_owned(),
        Some(b) => format!("[Backend: {} (B)]", backend_short_badge(Some(b))),
    };
    frame.render_widget(
        Paragraph::new(Span::styled(
            bk_filter_lbl,
            Style::default().fg(Color::LightCyan),
        )),
        query_cols[0],
    );
    hit_map.register(query_cols[0], ClickAction::CycleModalBackendFilter);

    let dedupe_lbl = match app.modal.dedupe {
        DiscoveryDedupe::ByLogicalTarget => "[Dedupe: Logical (U)]",
        DiscoveryDedupe::PerBackend => "[Dedupe: PerBackend (U)]",
    };
    frame.render_widget(
        Paragraph::new(Span::styled(
            dedupe_lbl,
            Style::default().fg(Color::LightMagenta),
        )),
        query_cols[1],
    );
    hit_map.register(query_cols[1], ClickAction::ToggleModalDedupe);

    let order_lbl = match app.modal.order {
        DiscoveryOrder::KindThenBackend => "[Order: Kind->Bk (O)]",
        DiscoveryOrder::BackendThenKind => "[Order: Bk->Kind (O)]",
    };
    frame.render_widget(
        Paragraph::new(Span::styled(
            order_lbl,
            Style::default().fg(Color::LightYellow),
        )),
        query_cols[2],
    );
    hit_map.register(query_cols[2], ClickAction::ToggleModalOrder);

    frame.render_widget(
        Paragraph::new(Span::styled(
            "  Sel  Audio State   Backend   Name / Target                 PID     Identifier / Path",
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        )),
        rows[3],
    );

    let list_area = rows[4];
    let visible_rows = list_area.height as usize;
    let scroll = if app.modal.cursor_idx >= visible_rows {
        app.modal.cursor_idx + 1 - visible_rows
    } else {
        0
    };
    let mut hovered_row_info: Option<(u16, u16, &ModalCandidate)> = None;

    if app.modal.candidates.is_empty() {
        frame.render_widget(
            Paragraph::new(Span::styled(
                "  No matching targets found. Toggle [AudioOnly(F)] off or switch Backend/Tab.",
                Style::default().fg(Color::DarkGray),
            )),
            list_area,
        );
    } else {
        for (vis_idx, (cand_idx, cand)) in app
            .modal
            .candidates
            .iter()
            .enumerate()
            .skip(scroll)
            .take(visible_rows)
            .enumerate()
        {
            let r_rect = Rect {
                x: list_area.x,
                y: list_area.y + vis_idx as u16,
                width: list_area.width,
                height: 1,
            };
            let is_cursor = cand_idx == app.modal.cursor_idx;
            let is_hovered = app
                .modal
                .hovered_candidate
                .is_some_and(|(h_idx, _, _)| h_idx == cand_idx);
            if let Some((h_idx, mouse_col, _)) = app.modal.hovered_candidate
                && h_idx == cand_idx
            {
                hovered_row_info = Some((r_rect.y, mouse_col, cand));
            }
            let sel_box = if cand.selected { "[x]" } else { "[ ]" };
            let (audio_badge, badge_color) = match cand.target.kind() {
                TargetKind::DefaultOutput => ("🔊 DEFAULT", Color::LightCyan),
                TargetKind::DefaultInput => ("🎤 DEFAULT", Color::LightMagenta),
                TargetKind::Device => ("🔈 ENDPOINT", Color::Cyan),
                TargetKind::InputDevice => ("🎙 MIC IN ", Color::Magenta),
                TargetKind::Application | TargetKind::Process => match cand.target.active_audio() {
                    Some(true) => ("🔊 PLAYING", Color::LightGreen),
                    Some(false) => ("🔈 IDLE   ", Color::Yellow),
                    None => ("   silent ", Color::DarkGray),
                },
            };
            let bk_tag = if let Some(b) = cand.target.backend() {
                backend_short_badge(Some(b)).to_owned()
            } else if !cand.target.available_backends().is_empty() {
                cand.target
                    .available_backends()
                    .iter()
                    .map(|&b| backend_short_badge(Some(b)))
                    .collect::<Vec<_>>()
                    .join("+")
            } else {
                "AUTO".to_owned()
            };
            let name = match cand.target.kind() {
                TargetKind::DefaultOutput => "Default System Output",
                TargetKind::DefaultInput => "Default Microphone",
                _ => cand
                    .target
                    .name()
                    .map_or_else(|| cand.target.id().as_ref(), |n| n.as_ref()),
            };
            let pid_str = cand
                .target
                .pid()
                .map_or_else(|| "-".to_owned(), |p| p.to_string());
            let path_str = cand
                .target
                .executable_path()
                .map(|s| s.as_ref())
                .or_else(|| cand.target.clean_device_id())
                .unwrap_or(cand.target.id().as_ref());

            let row_bg = if is_hovered {
                Color::Rgb(24, 38, 52)
            } else {
                Color::Reset
            };
            let row_line = Line::from(vec![
                Span::styled(
                    if is_cursor {
                        "> "
                    } else if is_hovered {
                        "• "
                    } else {
                        "  "
                    },
                    Style::default()
                        .fg(if is_cursor {
                            Color::Yellow
                        } else {
                            Color::LightCyan
                        })
                        .bg(row_bg),
                ),
                Span::styled(
                    format!("{sel_box}  "),
                    Style::default()
                        .fg(if cand.selected {
                            Color::LightCyan
                        } else {
                            Color::Gray
                        })
                        .bg(row_bg),
                ),
                Span::styled(
                    format!("{audio_badge}   "),
                    Style::default().fg(badge_color).bg(row_bg),
                ),
                Span::styled(
                    format!("{:<8}  ", truncate_str(&bk_tag, 8)),
                    Style::default().fg(Color::LightCyan).bg(row_bg),
                ),
                Span::styled(
                    format!("{:<26}  ", truncate_str(name, 26)),
                    Style::default()
                        .fg(if is_hovered {
                            Color::LightYellow
                        } else {
                            Color::White
                        })
                        .bg(row_bg),
                ),
                Span::styled(
                    format!("{:<6}  ", pid_str),
                    Style::default().fg(Color::Cyan).bg(row_bg),
                ),
                Span::styled(
                    truncate_str(path_str, 26),
                    Style::default()
                        .fg(if is_hovered {
                            Color::Gray
                        } else {
                            Color::DarkGray
                        })
                        .bg(row_bg),
                ),
            ]);
            frame.render_widget(
                Paragraph::new(row_line).style(Style::default().bg(row_bg)),
                r_rect,
            );
            hit_map.register(r_rect, ClickAction::ToggleModalCandidate(cand_idx));
        }
    }

    let status_cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Min(24), Constraint::Length(26)])
        .split(rows[5]);

    frame.render_widget(
        Paragraph::new(Span::styled(
            truncate_str(
                &format!(
                    "  Live Discovery ({} items) | {}",
                    app.modal.candidates.len(),
                    app.modal.last_event_note
                ),
                status_cols[0].width as usize,
            ),
            Style::default().fg(Color::Gray),
        )),
        status_cols[0],
    );

    if let Ok(rep) = &app.permission_report {
        let tab_kind = app.modal_tab_primary_permission_kind();
        let entry = rep.entry(tab_kind);
        let short_kind = match tab_kind {
            PermissionKind::Microphone => "Mic",
            PermissionKind::SystemAudio => "SysAudio",
            PermissionKind::ScreenRecording => "ScreenRec",
        };
        let (pill_txt, pill_style, pill_action) = if entry.can_prompt
            || (rep.os == PlatformOs::MacOs
                && tab_kind == PermissionKind::SystemAudio
                && entry.state == PermissionState::Unknown)
        {
            (
                format!("[Grant {short_kind} (G)]"),
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
                ClickAction::RequestDomainPermission(tab_kind),
            )
        } else if entry.state.is_denied_or_restricted()
            && domain_supports_open_settings(rep.os, tab_kind)
        {
            (
                format!("[{short_kind}:Settings (G)]"),
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::LightRed)
                    .add_modifier(Modifier::BOLD),
                ClickAction::OpenDomainSettings(tab_kind),
            )
        } else if entry.state.is_denied_or_restricted() {
            (
                format!("[{short_kind}:RESTRICTED(K)]"),
                Style::default()
                    .fg(Color::LightRed)
                    .add_modifier(Modifier::BOLD),
                ClickAction::OpenPermissionsModal,
            )
        } else if entry.state == PermissionState::Granted && entry.requires_relaunch {
            (
                format!("[{short_kind}:RELAUNCH (K)]"),
                Style::default()
                    .fg(Color::LightMagenta)
                    .add_modifier(Modifier::BOLD),
                ClickAction::OpenPermissionsModal,
            )
        } else {
            (
                format!("[Perm {short_kind}: OK (K)]"),
                Style::default().fg(Color::LightGreen),
                ClickAction::OpenPermissionsModal,
            )
        };
        frame.render_widget(
            Paragraph::new(Span::styled(pill_txt, pill_style)).alignment(Alignment::Right),
            status_cols[1],
        );
        hit_map.register(status_cols[1], pill_action);
    }

    let selected_count = app.modal.candidates.iter().filter(|c| c.selected).count();
    let bot_cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Min(10),
            Constraint::Length(18),
            Constraint::Length(26),
        ])
        .split(rows[6]);

    frame.render_widget(
        Paragraph::new(Span::styled(
            "[ Cancel (Esc) ]",
            Style::default().fg(Color::Gray),
        ))
        .alignment(Alignment::Center),
        bot_cols[1],
    );
    hit_map.register(bot_cols[1], ClickAction::CloseModal);

    frame.render_widget(
        Paragraph::new(Span::styled(
            format!("[ + Add {selected_count} Selected (Enter) ]"),
            Style::default()
                .fg(Color::Black)
                .bg(Color::LightGreen)
                .add_modifier(Modifier::BOLD),
        ))
        .alignment(Alignment::Center),
        bot_cols[2],
    );
    hit_map.register(bot_cols[2], ClickAction::ConfirmAddModalCandidates);

    if let Some((hovered_row_y, mouse_col, cand)) = hovered_row_info {
        render_modal_candidate_hover_popup(
            frame,
            modal_rect,
            list_area,
            hovered_row_y,
            mouse_col,
            cand,
            app,
        );
    }
}

fn render_modal_candidate_hover_popup(
    frame: &mut Frame,
    modal_rect: Rect,
    list_area: Rect,
    hovered_row_y: u16,
    mouse_col: u16,
    cand: &ModalCandidate,
    app: &App,
) {
    if modal_rect.width < 30 || modal_rect.height < 8 {
        return;
    }

    let popup_w = modal_rect
        .width
        .saturating_sub(6)
        .clamp(42, 76)
        .min(modal_rect.width.saturating_sub(2));
    let inner_w = popup_w.saturating_sub(2) as usize;
    let label_col_w = 12usize;
    let val_col_w = inner_w.saturating_sub(label_col_w).max(12);

    let (header_label, border_color) = match cand.target.kind() {
        TargetKind::DefaultOutput => (" 🔊 DEFAULT SYSTEM OUTPUT ", Color::LightCyan),
        TargetKind::DefaultInput => (" 🎤 DEFAULT MICROPHONE ", Color::LightMagenta),
        TargetKind::Device => (" 🔈 OUTPUT ENDPOINT ", Color::Cyan),
        TargetKind::InputDevice => (" 🎙 INPUT ENDPOINT ", Color::Magenta),
        TargetKind::Application => match cand.target.active_audio() {
            Some(true) => (" 🔊 APPLICATION · PLAYING ", Color::LightGreen),
            Some(false) => (" 🔈 APPLICATION · IDLE ", Color::Yellow),
            None => (" APPLICATION · SILENT ", Color::Gray),
        },
        TargetKind::Process => match cand.target.active_audio() {
            Some(true) => (" 🔊 PROCESS · PLAYING ", Color::LightGreen),
            Some(false) => (" 🔈 PROCESS · IDLE ", Color::Yellow),
            None => (" PROCESS · SILENT ", Color::Gray),
        },
    };

    let full_name = match cand.target.kind() {
        TargetKind::DefaultOutput => "Default System Output",
        TargetKind::DefaultInput => "Default Microphone",
        _ => cand
            .target
            .name()
            .map_or_else(|| cand.target.id().as_ref(), |n| n.as_ref()),
    };

    let mut lines: Vec<Line<'static>> = Vec::new();
    append_wrapped_kv_lines(
        &mut lines,
        "Name",
        full_name,
        val_col_w,
        Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD),
    );
    append_wrapped_kv_lines(
        &mut lines,
        "Target ID",
        cand.target.id().as_ref(),
        val_col_w,
        Style::default().fg(Color::Cyan),
    );

    let bk_detail = format!(
        "Pinned: {} · Available: [{}]",
        backend_short_badge(cand.target.backend()),
        cand.target
            .available_backends()
            .iter()
            .map(|&b| backend_short_badge(Some(b)))
            .collect::<Vec<_>>()
            .join(", ")
    );
    append_wrapped_kv_lines(
        &mut lines,
        "Backends",
        &bk_detail,
        val_col_w,
        Style::default().fg(Color::LightCyan),
    );

    if let Some(app_id) = cand.target.app_id().map(|s| s.as_ref())
        && !app_id.is_empty()
    {
        append_wrapped_kv_lines(
            &mut lines,
            "App ID",
            app_id,
            val_col_w,
            Style::default().fg(Color::LightYellow),
        );
    }

    if let Some(dev_id) = cand.target.clean_device_id()
        && !dev_id.is_empty()
    {
        append_wrapped_kv_lines(
            &mut lines,
            "Device ID",
            dev_id,
            val_col_w,
            Style::default().fg(Color::LightCyan),
        );
    }

    if let Some(pid) = cand.target.pid() {
        let pid_detail = match cand.target.process_start_time() {
            Some(start_ts) => format!("{pid}  (start_time: {start_ts})"),
            None => pid.to_string(),
        };
        append_wrapped_kv_lines(
            &mut lines,
            "PID / Gen",
            &pid_detail,
            val_col_w,
            Style::default().fg(Color::LightGreen),
        );
    }

    if let Some(path) = cand.target.executable_path().map(|s| s.as_ref())
        && !path.is_empty()
    {
        append_wrapped_path_lines(&mut lines, "Path", path, val_col_w);
    }

    let policy_str = match cand.target.kind() {
        TargetKind::Application | TargetKind::Process => format!(
            "Scope: {:?} · OnExit: {:?} · Gain: {:.0}%",
            cand.target.scope(),
            cand.target.on_root_exit(),
            cand.target.gain() * 100.0
        ),
        TargetKind::DefaultOutput
        | TargetKind::DefaultInput
        | TargetKind::Device
        | TargetKind::InputDevice => format!(
            "DeviceMode: {:?} · Gain: {:.0}%",
            cand.target.default_device_mode(),
            cand.target.gain() * 100.0
        ),
    };
    append_wrapped_kv_lines(
        &mut lines,
        "Defaults",
        &policy_str,
        val_col_w,
        Style::default().fg(Color::Gray),
    );

    if let Some(perm_entry) = app.target_permission_entry(&cand.target) {
        let relaunch_tag = if perm_entry.requires_relaunch {
            " (relaunch needed)"
        } else {
            ""
        };
        let perm_txt = format!(
            "{} · {}{relaunch_tag}",
            perm_entry.kind.as_str(),
            perm_entry.state.as_str()
        );
        let perm_fg = match perm_entry.state {
            PermissionState::Granted if perm_entry.requires_relaunch => Color::LightMagenta,
            PermissionState::Granted => Color::LightGreen,
            PermissionState::NotDetermined | PermissionState::Unknown => Color::Yellow,
            PermissionState::Denied | PermissionState::Restricted => Color::LightRed,
        };
        append_wrapped_kv_lines(
            &mut lines,
            "Permission",
            &perm_txt,
            val_col_w,
            Style::default().fg(perm_fg),
        );
    }

    let max_popup_h = modal_rect.height.saturating_sub(2).max(4);
    let popup_h = ((lines.len() as u16) + 2).min(max_popup_h);

    let list_mid_y = list_area.y + list_area.height / 2;
    let space_below = (modal_rect.y + modal_rect.height)
        .saturating_sub(hovered_row_y + 1)
        .saturating_sub(1);
    let space_above = hovered_row_y.saturating_sub(modal_rect.y + 1);

    let popup_y =
        if space_below >= popup_h && (hovered_row_y <= list_mid_y || space_below >= space_above) {
            hovered_row_y + 1
        } else if space_above >= popup_h {
            hovered_row_y.saturating_sub(popup_h)
        } else if space_below >= space_above {
            (hovered_row_y + 1)
                .min((modal_rect.y + modal_rect.height).saturating_sub(popup_h.saturating_add(1)))
        } else {
            hovered_row_y
                .saturating_sub(popup_h)
                .max(modal_rect.y.saturating_add(1))
        };

    let min_x = modal_rect.x.saturating_add(2);
    let max_x = (modal_rect.x + modal_rect.width)
        .saturating_sub(popup_w.saturating_add(1))
        .max(min_x);
    let desired_x = mouse_col.saturating_sub(popup_w / 3);
    let popup_x = desired_x.clamp(min_x, max_x);

    let popup_rect = Rect {
        x: popup_x,
        y: popup_y,
        width: popup_w,
        height: popup_h,
    };

    let status_badge = if cand.selected {
        " [Selected ✓] "
    } else {
        " [Click to Toggle] "
    };

    frame.render_widget(Clear, popup_rect);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(border_color))
        .style(Style::default().bg(Color::Rgb(14, 20, 28)))
        .title(
            Line::from(Span::styled(
                header_label,
                Style::default()
                    .fg(Color::Black)
                    .bg(border_color)
                    .add_modifier(Modifier::BOLD),
            ))
            .alignment(Alignment::Left),
        )
        .title(
            Line::from(Span::styled(
                status_badge,
                Style::default()
                    .fg(if cand.selected {
                        Color::LightCyan
                    } else {
                        Color::Gray
                    })
                    .add_modifier(Modifier::BOLD),
            ))
            .alignment(Alignment::Right),
        );
    let inner = block.inner(popup_rect);
    frame.render_widget(block, popup_rect);
    frame.render_widget(
        Paragraph::new(lines).style(Style::default().bg(Color::Rgb(14, 20, 28))),
        inner,
    );
}

fn render_permissions_modal(frame: &mut Frame, full_area: Rect, app: &App, hit_map: &mut HitMap) {
    let modal_w = (full_area.width * 84 / 100).clamp(48, full_area.width.saturating_sub(2));
    let modal_h = 20u16.min(full_area.height.saturating_sub(2)).max(12);
    let modal_rect = Rect {
        x: full_area.x + (full_area.width.saturating_sub(modal_w)) / 2,
        y: full_area.y + (full_area.height.saturating_sub(modal_h)) / 2,
        width: modal_w,
        height: modal_h,
    };

    let os_label = match app.effective_permission_os() {
        PlatformOs::Windows => "Windows",
        PlatformOs::Linux => "Linux",
        PlatformOs::MacOs => "macOS",
    };

    frame.render_widget(Clear, modal_rect);
    let block = Block::default()
        .title(Span::styled(
            format!(" OS Recording Permissions & Consent ({os_label}) "),
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ))
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::Cyan));
    let inner = block.inner(modal_rect);
    frame.render_widget(block, modal_rect);

    if inner.height < 8 || inner.width < 36 {
        return;
    }

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(3),
            Constraint::Length(1),
            Constraint::Min(3),
            Constraint::Length(1),
        ])
        .split(inner);

    let top_cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(27),
            Constraint::Min(22),
            Constraint::Length(13),
        ])
        .split(rows[0]);

    let (pol_txt, pol_fg) = match app.permission_policy {
        PermissionPolicy::AutoRequest => ("[Policy: AutoRequest (P)]", Color::LightGreen),
        PermissionPolicy::CheckOnly => ("[Policy: CheckOnly   (P)]", Color::Yellow),
    };
    frame.render_widget(
        Paragraph::new(Span::styled(
            pol_txt,
            Style::default().fg(pol_fg).add_modifier(Modifier::BOLD),
        )),
        top_cols[0],
    );
    hit_map.register(top_cols[0], ClickAction::TogglePermissionPolicy);

    let set_txt = if app.open_settings_if_denied {
        "[x] Open OS Settings on Denial (O)"
    } else {
        "[ ] Open OS Settings on Denial (O)"
    };
    frame.render_widget(
        Paragraph::new(Span::styled(set_txt, Style::default().fg(Color::Cyan))),
        top_cols[1],
    );
    hit_map.register(top_cols[1], ClickAction::ToggleOpenSettingsFallback);

    frame.render_widget(
        Paragraph::new(Span::styled(
            "[Close (Esc)]",
            Style::default()
                .fg(Color::LightRed)
                .add_modifier(Modifier::BOLD),
        ))
        .alignment(Alignment::Right),
        top_cols[2],
    );
    hit_map.register(top_cols[2], ClickAction::ClosePermissionsModal);

    let act_cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(22), Constraint::Min(24)])
        .split(rows[1]);

    frame.render_widget(
        Paragraph::new(Span::styled(
            "[Re-Check States (C)]",
            Style::default()
                .fg(Color::LightCyan)
                .add_modifier(Modifier::BOLD),
        )),
        act_cols[0],
    );
    hit_map.register(act_cols[0], ClickAction::RefreshPermissions);

    let (req_all_lbl, req_all_style) = if app.permission_runner.is_in_flight() {
        (
            "[ Requesting OS Consent... ]",
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )
    } else {
        (
            "[ Request Required for Staged (R / Enter) ]",
            Style::default()
                .fg(Color::Black)
                .bg(Color::LightGreen)
                .add_modifier(Modifier::BOLD),
        )
    };
    frame.render_widget(
        Paragraph::new(Span::styled(req_all_lbl, req_all_style)),
        act_cols[1],
    );
    hit_map.register(act_cols[1], ClickAction::RequestRequiredPermissions);

    frame.render_widget(
        Paragraph::new(Span::styled(
            " #  Domain            Required    State                Prompt  Relaunch  Action",
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        )),
        rows[3],
    );

    match &app.permission_report {
        Ok(rep) => {
            for (idx, entry) in rep.entries().iter().copied().enumerate() {
                if idx as u16 >= rows[4].height {
                    break;
                }
                let r_rect = Rect {
                    x: rows[4].x,
                    y: rows[4].y + idx as u16,
                    width: rows[4].width,
                    height: 1,
                };
                let row_cols = Layout::default()
                    .direction(Direction::Horizontal)
                    .constraints([Constraint::Min(30), Constraint::Length(14)])
                    .split(r_rect);

                let dom_name = match entry.kind {
                    PermissionKind::Microphone => "Microphone      ",
                    PermissionKind::SystemAudio => "SystemAudio     ",
                    PermissionKind::ScreenRecording => "ScreenRecording ",
                };
                let (req_txt, req_style) = if entry.required {
                    (
                        "* REQUIRED  ",
                        Style::default()
                            .fg(Color::Yellow)
                            .add_modifier(Modifier::BOLD),
                    )
                } else {
                    ("  optional  ", Style::default().fg(Color::DarkGray))
                };
                let (state_txt, state_fg) = match entry.state {
                    PermissionState::Granted => ("GRANTED             ", Color::LightGreen),
                    PermissionState::NotDetermined => ("NOT DETERMINED      ", Color::Yellow),
                    PermissionState::Denied => ("DENIED              ", Color::LightRed),
                    PermissionState::Restricted => ("RESTRICTED          ", Color::LightRed),
                    PermissionState::Unknown => ("UNKNOWN (Unprobed)  ", Color::Yellow),
                };
                let prompt_txt = if entry.can_prompt {
                    "Yes     "
                } else {
                    "No      "
                };
                let (rel_txt, rel_fg) = if entry.requires_relaunch {
                    ("YES     ", Color::LightMagenta)
                } else {
                    ("No      ", Color::DarkGray)
                };

                let left_line = Line::from(vec![
                    Span::styled(format!(" {}  ", idx + 1), Style::default().fg(Color::Cyan)),
                    Span::styled(dom_name, Style::default().fg(Color::White)),
                    Span::styled(req_txt, req_style),
                    Span::styled(
                        state_txt,
                        Style::default().fg(state_fg).add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(prompt_txt, Style::default().fg(Color::Gray)),
                    Span::styled(rel_txt, Style::default().fg(rel_fg)),
                ]);
                frame.render_widget(Paragraph::new(left_line), row_cols[0]);

                let key_num = idx + 1;
                if entry.can_prompt {
                    let lbl = format!("[Ask OS ({key_num})]");
                    frame.render_widget(
                        Paragraph::new(Span::styled(
                            lbl,
                            Style::default()
                                .fg(Color::Black)
                                .bg(Color::Yellow)
                                .add_modifier(Modifier::BOLD),
                        ))
                        .alignment(Alignment::Right),
                        row_cols[1],
                    );
                    hit_map.register(
                        row_cols[1],
                        ClickAction::RequestDomainPermission(entry.kind),
                    );
                } else if rep.os == PlatformOs::MacOs
                    && entry.kind == PermissionKind::SystemAudio
                    && entry.state == PermissionState::Unknown
                {
                    let lbl = format!("[Probe  ({key_num})]");
                    frame.render_widget(
                        Paragraph::new(Span::styled(
                            lbl,
                            Style::default()
                                .fg(Color::Black)
                                .bg(Color::Cyan)
                                .add_modifier(Modifier::BOLD),
                        ))
                        .alignment(Alignment::Right),
                        row_cols[1],
                    );
                    hit_map.register(
                        row_cols[1],
                        ClickAction::RequestDomainPermission(entry.kind),
                    );
                } else if entry.state.is_denied_or_restricted()
                    && domain_supports_open_settings(rep.os, entry.kind)
                {
                    let lbl = format!("[Settgs ({key_num})]");
                    frame.render_widget(
                        Paragraph::new(Span::styled(
                            lbl,
                            Style::default()
                                .fg(Color::Black)
                                .bg(Color::LightRed)
                                .add_modifier(Modifier::BOLD),
                        ))
                        .alignment(Alignment::Right),
                        row_cols[1],
                    );
                    hit_map.register(row_cols[1], ClickAction::OpenDomainSettings(entry.kind));
                } else {
                    frame.render_widget(
                        Paragraph::new(Span::styled(
                            "[    --    ]",
                            Style::default().fg(Color::DarkGray),
                        ))
                        .alignment(Alignment::Right),
                        row_cols[1],
                    );
                }
            }

            let diag_lines = build_permission_diagnostics(rep, rows[6].width as usize);
            frame.render_widget(Paragraph::new(diag_lines), rows[6]);
        }
        Err(err) => {
            frame.render_widget(
                Paragraph::new(Span::styled(
                    format!(" Failed to inspect OS permissions: {err}"),
                    Style::default().fg(Color::LightRed),
                )),
                rows[4],
            );
        }
    }

    frame.render_widget(
        Paragraph::new(Span::styled(
            truncate_str(
                &format!(" Note: {}", app.permissions_modal.last_action_note),
                rows[7].width as usize,
            ),
            Style::default().fg(Color::Cyan),
        )),
        rows[7],
    );
}

fn build_permission_diagnostics(report: &PermissionReport, max_width: usize) -> Vec<Line<'static>> {
    let mut raw_notes: Vec<(Color, String)> = Vec::new();
    match report.os {
        PlatformOs::Windows => {
            let mic = report.microphone();
            match mic.state {
                PermissionState::Granted => raw_notes.push((
                    Color::LightGreen,
                    "Microphone: Allowed in Windows Privacy ConsentStore (WASAPI eCapture ready)."
                        .to_owned(),
                )),
                PermissionState::Denied => raw_notes.push((
                    Color::LightRed,
                    "Microphone: Denied in Windows Settings -> Privacy & security -> Microphone. Click [Settgs (1)] for ms-settings:privacy-microphone."
                        .to_owned(),
                )),
                PermissionState::Restricted => raw_notes.push((
                    Color::LightRed,
                    "Microphone: Restricted by Group Policy / MDM (LetAppsAccessMicrophone=2) or OS build floor."
                        .to_owned(),
                )),
                other => raw_notes.push((
                    Color::Yellow,
                    format!("Microphone: state={}.", other.as_str()),
                )),
            }
            raw_notes.push((
                Color::Gray,
                "SystemAudio / ScreenRecording: WASAPI loopback & process-tree capture require no separate privacy toggle on Windows."
                    .to_owned(),
            ));
        }
        PlatformOs::MacOs => {
            let mic = report.microphone();
            match mic.state {
                PermissionState::Granted => raw_notes.push((
                    Color::LightGreen,
                    "Microphone (AVMediaTypeAudio): TCC consent granted.".to_owned(),
                )),
                PermissionState::NotDetermined => raw_notes.push((
                    Color::Yellow,
                    "Microphone (AVMediaTypeAudio): Not yet requested; click [Ask OS (1)] to trigger macOS TCC prompt."
                        .to_owned(),
                )),
                PermissionState::Denied => raw_notes.push((
                    Color::LightRed,
                    "Microphone (AVMediaTypeAudio): Denied in TCC. Click [Settgs (1)] -> Privacy & Security -> Microphone."
                        .to_owned(),
                )),
                PermissionState::Restricted => raw_notes.push((
                    Color::LightRed,
                    "Microphone (AVMediaTypeAudio): Restricted (SSH session, MDM, or .app missing NSMicrophoneUsageDescription)."
                        .to_owned(),
                )),
                PermissionState::Unknown => raw_notes.push((
                    Color::Yellow,
                    "Microphone (AVMediaTypeAudio): Status unknown.".to_owned(),
                )),
            }

            let sys = report.system_audio();
            match sys.state {
                PermissionState::Granted => raw_notes.push((
                    Color::LightGreen,
                    "SystemAudio (CoreAudio CATap 14.2+): Verified granted ('wat?'/'!hog' probe clean)."
                        .to_owned(),
                )),
                PermissionState::Unknown => raw_notes.push((
                    Color::Yellow,
                    "SystemAudio (CoreAudio CATap 14.2+): Unprobed (never faked as Granted). Click [Probe (2)] or open stream."
                        .to_owned(),
                )),
                PermissionState::Denied => raw_notes.push((
                    Color::LightRed,
                    "SystemAudio (CoreAudio CATap 14.2+): Denied ('wat?'/'!hog'). Click [Settgs (2)] -> System Audio Recording Only."
                        .to_owned(),
                )),
                PermissionState::Restricted => raw_notes.push((
                    Color::LightRed,
                    "SystemAudio (CoreAudio CATap): Restricted (macOS < 14.2 or .app missing NSAudioCaptureUsageDescription)."
                        .to_owned(),
                )),
                PermissionState::NotDetermined => raw_notes.push((
                    Color::Yellow,
                    "SystemAudio (CoreAudio CATap 14.2+): Click [Ask OS (2)] to request consent."
                        .to_owned(),
                )),
            }

            let scr = report.screen_recording();
            if scr.state == PermissionState::Granted && scr.requires_relaunch {
                raw_notes.push((
                    Color::LightMagenta,
                    "ScreenRecording (ScreenCaptureKit): Newly granted! Relaunch app for SCShareableContent/SCK audio to activate."
                        .to_owned(),
                ));
            } else {
                match scr.state {
                    PermissionState::Granted => raw_notes.push((
                        Color::LightGreen,
                        "ScreenRecording (ScreenCaptureKit): TCC consent granted.".to_owned(),
                    )),
                    PermissionState::NotDetermined => raw_notes.push((
                        Color::Yellow,
                        "ScreenRecording (ScreenCaptureKit): Not yet requested; click [Ask OS (3)] (granting requires relaunch)."
                            .to_owned(),
                    )),
                    PermissionState::Denied => raw_notes.push((
                        Color::LightRed,
                        "ScreenRecording (ScreenCaptureKit): Denied in TCC. Click [Settgs (3)] -> Screen & System Audio Recording, then relaunch."
                            .to_owned(),
                    )),
                    PermissionState::Restricted => raw_notes.push((
                        Color::LightRed,
                        "ScreenRecording (ScreenCaptureKit): Restricted (macOS < 13.0 or headless session)."
                            .to_owned(),
                    )),
                    PermissionState::Unknown => raw_notes.push((
                        Color::Yellow,
                        "ScreenRecording (ScreenCaptureKit): Status unknown.".to_owned(),
                    )),
                }
            }
        }
        PlatformOs::Linux => {
            let sys = report.system_audio();
            match sys.state {
                PermissionState::Granted => raw_notes.push((
                    Color::LightGreen,
                    "PipeWire / PulseAudio: Audio server UNIX socket reachable (no interactive TCC prompt required)."
                        .to_owned(),
                )),
                PermissionState::Restricted => raw_notes.push((
                    Color::LightRed,
                    "Linux Sandbox / Socket Denied: EACCES or Flatpak missing --socket=pulseaudio / --filesystem=xdg-run/pipewire-0."
                        .to_owned(),
                )),
                PermissionState::Unknown => raw_notes.push((
                    Color::Yellow,
                    "Audio Server Unreachable: Neither PipeWire nor PulseAudio socket responded in XDG_RUNTIME_DIR."
                        .to_owned(),
                )),
                other => raw_notes.push((
                    Color::Yellow,
                    format!("Linux Audio Socket: state={}.", other.as_str()),
                )),
            }
        }
    }

    let mut lines = Vec::new();
    let wrap_w = max_width.saturating_sub(4).max(16);
    for (color, note) in raw_notes {
        for (idx, chunk) in wrap_text_chunks(&note, wrap_w).into_iter().enumerate() {
            let bullet = if idx == 0 { " * " } else { "   " };
            lines.push(Line::from(vec![
                Span::styled(bullet, Style::default().fg(color)),
                Span::styled(chunk, Style::default().fg(color)),
            ]));
        }
    }
    lines
}

fn append_wrapped_kv_lines(
    lines: &mut Vec<Line<'static>>,
    label: &str,
    value: &str,
    val_col_w: usize,
    val_style: Style,
) {
    let chunks = wrap_text_chunks(value, val_col_w);
    for (idx, chunk) in chunks.into_iter().enumerate() {
        let prefix = if idx == 0 {
            format!(" {:<10} ", label)
        } else {
            "            ".to_owned()
        };
        lines.push(Line::from(vec![
            Span::styled(prefix, Style::default().fg(Color::DarkGray)),
            Span::styled(chunk, val_style),
        ]));
    }
}

fn append_wrapped_path_lines(
    lines: &mut Vec<Line<'static>>,
    label: &str,
    path: &str,
    val_col_w: usize,
) {
    let sep_pos = path.rfind(['\\', '/']);
    if let Some(pos) = sep_pos {
        let (dir_part, file_part) = path.split_at(pos + 1);
        if !file_part.is_empty() {
            let dir_chunks = wrap_text_chunks(dir_part, val_col_w);
            for (idx, chunk) in dir_chunks.into_iter().enumerate() {
                let prefix = if idx == 0 {
                    format!(" {:<10} ", label)
                } else {
                    "            ".to_owned()
                };
                lines.push(Line::from(vec![
                    Span::styled(prefix, Style::default().fg(Color::DarkGray)),
                    Span::styled(chunk, Style::default().fg(Color::Gray)),
                ]));
            }
            let file_chunks = wrap_text_chunks(file_part, val_col_w.saturating_sub(2).max(8));
            for (idx, chunk) in file_chunks.into_iter().enumerate() {
                let arrow = if idx == 0 { "↳ " } else { "  " };
                lines.push(Line::from(vec![
                    Span::styled("            ", Style::default().fg(Color::DarkGray)),
                    Span::styled(arrow, Style::default().fg(Color::Cyan)),
                    Span::styled(
                        chunk,
                        Style::default()
                            .fg(Color::White)
                            .add_modifier(Modifier::BOLD),
                    ),
                ]));
            }
            return;
        }
    }
    append_wrapped_kv_lines(
        lines,
        label,
        path,
        val_col_w,
        Style::default().fg(Color::White),
    );
}

fn wrap_text_chunks(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let chars: Vec<char> = text.chars().collect();
    if chars.is_empty() {
        return vec!["-".to_owned()];
    }
    chars
        .chunks(width)
        .map(|chunk| chunk.iter().collect())
        .collect()
}

fn truncate_str(s: &str, max_chars: usize) -> String {
    if max_chars == 0 {
        return String::new();
    }
    let count = s.chars().count();
    if count <= max_chars {
        s.to_owned()
    } else if max_chars <= 3 {
        s.chars().take(max_chars).collect()
    } else {
        let prefix: String = s.chars().take(max_chars - 3).collect();
        format!("{prefix}...")
    }
}

#[cfg(test)]
mod layout_regressions {
    use super::*;
    use ratatui::{Terminal, backend::TestBackend};

    #[test]
    fn panel_fit_half_threshold_checks_both_dimensions() {
        assert!(!panel_below_half(Rect::new(0, 0, 50, 5), 100, 10));
        assert!(panel_below_half(Rect::new(0, 0, 49, 5), 100, 10));
        assert!(panel_below_half(Rect::new(0, 0, 50, 4), 100, 10));
        assert!(!panel_below_half(Rect::new(0, 0, 50, 5), 100, 9));
        assert!(panel_below_half(Rect::new(0, 0, 50, 4), 100, 9));
    }

    #[test]
    fn narrow_config_chips_remain_visible_and_clickable() {
        for width in [38, 44, 60] {
            let mut terminal = Terminal::new(TestBackend::new(width, 10)).unwrap();
            let app = App::new();
            let mut hit_map = HitMap::default();
            terminal
                .draw(|frame| render_config_and_plan(frame, frame.area(), &app, &mut hit_map))
                .unwrap();
            for (action, label) in [
                (ClickAction::CycleCombineMode, "[Comb:Auto]"),
                (ClickAction::CycleTreeStrategy, "[Tree:Krn..Sw]"),
                (ClickAction::ToggleInputConversion, "[In:AutoResmp]"),
            ] {
                let (rect, _) = hit_map
                    .regions()
                    .iter()
                    .find(|(_, candidate)| *candidate == action)
                    .unwrap();
                assert!(
                    usize::from(rect.width) >= label.len(),
                    "width {width}: {label}"
                );
                let rendered: String = (0..label.len())
                    .map(|offset| {
                        terminal.backend().buffer()[(rect.x + offset as u16, rect.y)].symbol()
                    })
                    .collect();
                assert_eq!(rendered, label, "width {width}");
                assert_eq!(hit_map.resolve(rect.x, rect.y), Some(action));
            }
        }
    }

    #[test]
    fn spectrum_ruler_spans_resized_plot() {
        for width in [42, 100, 160] {
            let mut terminal = Terminal::new(TestBackend::new(width, 10)).unwrap();
            let app = App::new();
            let mut hit_map = HitMap::default();
            terminal
                .draw(|frame| render_spectroscope(frame, frame.area(), &app, &mut hit_map))
                .unwrap();
            let ruler: String = (1..width - 1)
                .map(|x| terminal.backend().buffer()[(x, 7)].symbol())
                .collect();
            assert!(ruler.starts_with("20Hz"), "width {width}: {ruler}");
            assert!(ruler.ends_with("20kHz"), "width {width}: {ruler}");
            let position = ruler.find("1kHz").unwrap();
            let expected =
                ((f64::from(width - 3)) * (50.0_f64.ln() / 1000.0_f64.ln())).round() as usize;
            assert!(
                (position as isize + 2 - expected as isize).abs() <= 1,
                "width {width}: {ruler}"
            );
        }
    }
}
