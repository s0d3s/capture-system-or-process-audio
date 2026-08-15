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

