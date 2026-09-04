#![forbid(unsafe_code)]

pub mod app;
pub mod backend;
pub mod capture_worker;
pub mod dsp;
pub mod hit_test;
pub mod playback;
pub mod ui;
pub mod wav;

use app::App;
use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyEventKind},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use hit_test::HitMap;
use ratatui::{Terminal, backend::CrosstermBackend};
use std::io;
use std::time::{Duration, Instant};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--smoke-test") {
        return run_headless_smoke_test();
    }

    let original_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |panic_info| {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen, DisableMouseCapture);
        original_hook(panic_info);
    }));

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let res = run_tui_loop(&mut terminal);

    let _ = disable_raw_mode();
    let _ = execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    );
    let _ = terminal.show_cursor();

    res
}

fn run_tui_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut app = App::new();
    let mut hit_map = HitMap::default();
    let tick_rate = Duration::from_millis(33);
    let mut last_tick = Instant::now();
    let mut needs_redraw = true;

    while !app.should_quit {
        if needs_redraw {
            terminal.draw(|frame| ui::render(frame, &app, &mut hit_map))?;
            needs_redraw = false;
        }

        let timeout = tick_rate.saturating_sub(last_tick.elapsed());
        if event::poll(timeout)? {
            loop {
                match event::read()? {
                    Event::Key(key) if key.kind == KeyEventKind::Press => {
                        app.handle_key_event(key);
                        needs_redraw = true;
                    }
                    Event::Mouse(mouse) => {
                        if !matches!(mouse.kind, crossterm::event::MouseEventKind::Moved)
                            || app.modal.open
                        {
                            app.handle_mouse_event(mouse, &hit_map);
                            needs_redraw = true;
                        }
                    }
                    Event::Resize(_, _) => {
                        needs_redraw = true;
                    }
                    _ => {}
                }
                if app.should_quit || !event::poll(Duration::ZERO)? {
                    break;
                }
            }
        }

        if last_tick.elapsed() >= tick_rate {
            app.on_tick();
            last_tick = Instant::now();
            needs_redraw = true;
        }
    }

    Ok(())
}

fn run_headless_smoke_test() -> Result<(), Box<dyn std::error::Error>> {
    use crate::hit_test::{ClickAction, ModalTab};
    use capture_system_or_process_audio::{CombineResolution, DefaultDeviceMode, DiscoveryQuery};
    use ratatui::backend::TestBackend;

    let backend = TestBackend::new(120, 36);
    let mut terminal = Terminal::new(backend)?;
    let mut app = App::new();
    let mut hit_map = HitMap::default();

    let catalog = app
        .discovery
        .discover_query(&DiscoveryQuery::default())
        .map_err(|e| format!("DiscoveryQuery failed: {e}"))?;
    let native_req = capture_system_or_process_audio::CaptureRequest::single(
        capture_system_or_process_audio::CaptureTarget::default_output(DefaultDeviceMode::Pin),
    );
    let perm_rep = crate::backend::inspect_request_permissions(&native_req)?;
    if perm_rep.has_denial() {
        return Err("unexpected permission denial on default_output".into());
    }
    let mut native_sess = crate::backend::PlatformCaptureSession::open_native(
        native_req,
        capture_system_or_process_audio::Overflow::DropOldest,
    )?;
    let _ = native_sess.read_next(256, Duration::from_millis(60))?;
    native_sess.stop_and_cleanup(capture_system_or_process_audio::Stop::Abort);

    app.synthetic_mode = true;
    app.open_add_source_modal();
    app.handle_click_action(ClickAction::SwitchModalTab(ModalTab::Microphones));
    terminal.draw(|f| ui::render(f, &app, &mut hit_map))?;
    if !app.modal.candidates.is_empty() {
        app.handle_click_action(ClickAction::ToggleModalCandidate(0));
        app.handle_click_action(ClickAction::ConfirmAddModalCandidates);
    } else {
        app.handle_click_action(ClickAction::CloseModal);
    }

    let plan = app
        .current_plan
        .as_ref()
        .map_err(|e| format!("smoke plan failed: {e}"))?;
    if app.staged_sources.len() >= 2 && plan.combine_resolution != CombineResolution::CspaMixer {
        return Err(
            "expected CombineResolution::CspaMixer for [DefaultOutput + Microphone]".into(),
        );
    }

    terminal.draw(|f| ui::render(f, &app, &mut hit_map))?;
    app.start_recording();
    for _ in 0..6 {
        std::thread::sleep(Duration::from_millis(15));
        app.on_tick();
        terminal.draw(|f| ui::render(f, &app, &mut hit_map))?;
    }
    if app.staged_sources.len() >= 2 {
        app.handle_click_action(ClickAction::ToggleStagedSource(1));
        for _ in 0..4 {
            std::thread::sleep(Duration::from_millis(15));
            app.on_tick();
        }
        app.handle_click_action(ClickAction::ToggleStagedSource(1));
        for _ in 0..4 {
            std::thread::sleep(Duration::from_millis(15));
            app.on_tick();
        }
    }
    app.stop_recording();
    for _ in 0..12 {
        std::thread::sleep(Duration::from_millis(15));
        app.on_tick();
        if !app.takes.is_empty() {
            break;
        }
    }
    if app.takes.is_empty() {
        return Err("expected recorded MemoryTake in RAM after smoke-test".into());
    }
    println!(
        "cspa-tui-demo smoke-test PASS: takes_in_ram={} segments={} discovered={} hit_regions={}",
        app.takes.len(),
        app.takes[0].segments.len(),
        catalog.targets.len(),
        hit_map.regions().len()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dsp::VisMode;
    use crate::hit_test::ClickAction;
    use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
    use ratatui::backend::TestBackend;

    #[test]
    fn panel_fit_warnings_follow_terminal_resize() {
        let app = App::new();
        let mut hit_map = HitMap::default();
        let mut terminal = Terminal::new(TestBackend::new(100, 36)).expect("terminal");
        for (width, height, expected) in [
            (100, 36, "normal"),
            (80, 36, "partial"),
            (50, 36, "partial"),
            (49, 36, "severe"),
            (40, 36, "severe"),
            (100, 30, "partial"),
            (100, 12, "severe"),
            (100, 36, "normal"),
        ] {
            terminal.backend_mut().resize(width, height);
            terminal
                .draw(|frame| ui::render(frame, &app, &mut hit_map))
                .expect("draw");
            let buffer = terminal.backend().buffer();
            let text: String = (0..height)
                .map(|row| {
                    (0..width)
                        .map(|column| buffer[(column, row)].symbol())
                        .collect::<String>()
                })
                .collect::<Vec<_>>()
                .join("\n");
            assert_eq!(
                text.contains("Expand terminal to show all panels"),
                expected == "severe",
                "{width}x{height}"
            );
            assert_eq!(
                text.contains("Panel not fully fit"),
                expected == "partial",
                "{width}x{height}"
            );
            for row in 0..height {
                for column in 0..width.saturating_sub(18) {
                    let label: String = (column..column + 19)
                        .map(|x| buffer[(x, row)].symbol())
                        .collect();
                    if label == "Panel not fully fit" {
                        for x in column..column + 19 {
                            assert_eq!(
                                hit_map.resolve(x, row),
                                None,
                                "warning row at {width}x{height}"
                            );
                        }
                    }
                }
            }
            if expected == "severe" {
                assert!(hit_map.regions().is_empty());
            }
        }
        terminal.backend_mut().resize(1, 1);
        terminal
            .draw(|frame| ui::render(frame, &app, &mut hit_map))
            .expect("tiny terminal");
        assert!(hit_map.regions().is_empty());
    }

    #[test]
    fn config_controls_are_separated_and_clickable() {
        let mut app = App::new();
        app.converter_dither = capture_system_or_process_audio::DitherMode::None;

        for width in [100, 120] {
            let backend = TestBackend::new(width, 36);
            let mut terminal = Terminal::new(backend).expect("test terminal");
            let mut hit_map = HitMap::default();
            terminal
                .draw(|frame| ui::render(frame, &app, &mut hit_map))
                .expect("draw settings");

            let controls = [
                (ClickAction::CycleCombineMode, "[Comb:Auto]"),
                (ClickAction::CycleTreeStrategy, "[Tree:Krn..Sw]"),
                (ClickAction::ToggleInputConversion, "[In:AutoResmp]"),
                (ClickAction::CycleSampleFormat, "[F32]"),
                (ClickAction::CycleDither, "[Dither:None]"),
                (ClickAction::ToggleSilenceOnIdle, "[Idle:Silence]"),
                (ClickAction::CycleOverflowPolicy, "[DropOldest]"),
            ];
            let buffer = terminal.backend().buffer();
            for (action, label) in controls {
                let (rect, _) = hit_map
                    .regions()
                    .iter()
                    .find(|(_, candidate)| *candidate == action)
                    .expect("control hitbox");
                assert!(usize::from(rect.width) >= label.len());
                let rendered: String = (0..label.len())
                    .map(|offset| buffer[(rect.x + offset as u16, rect.y)].symbol())
                    .collect();
                assert_eq!(rendered, label);
                assert_eq!(hit_map.resolve(rect.x, rect.y), Some(action));
            }
        }
    }

    #[test]
    fn headless_ui_renders_all_modes_modal_and_handles_mouse_clicks() {
        let backend = TestBackend::new(120, 36);
        let mut terminal = Terminal::new(backend).expect("test terminal");
        let mut app = App::new();
        let mut hit_map = HitMap::default();

        terminal
            .draw(|f| ui::render(f, &app, &mut hit_map))
            .expect("initial draw");
        assert!(!hit_map.regions().is_empty());

        let (add_rect, _) = hit_map
            .regions()
            .iter()
            .find(|(_, act)| *act == ClickAction::OpenAddSourceModal)
            .expect("OpenAddSourceModal hitbox");
        let click = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: add_rect.x + 1,
            row: add_rect.y,
            modifiers: crossterm::event::KeyModifiers::NONE,
        };
        app.handle_mouse_event(click, &hit_map);
        assert!(app.modal.open, "clicking Add Source must open the modal");

        terminal
            .draw(|f| ui::render(f, &app, &mut hit_map))
            .expect("modal draw");
        let (close_rect, _) = hit_map
            .regions()
            .iter()
            .find(|(_, act)| *act == ClickAction::CloseModal)
            .expect("CloseModal hitbox");
        let close_click = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: close_rect.x + 1,
            row: close_rect.y,
            modifiers: crossterm::event::KeyModifiers::NONE,
        };
        app.handle_mouse_event(close_click, &hit_map);
        assert!(!app.modal.open, "clicking Close must close the modal");

        for mode in [
            VisMode::FftSpectrum,
            VisMode::Waterfall,
            VisMode::Oscilloscope,
        ] {
            app.handle_click_action(ClickAction::SelectVisMode(mode));
            assert_eq!(app.vis_mode, mode);
            terminal
                .draw(|f| ui::render(f, &app, &mut hit_map))
                .expect("draw vis mode");
        }

        app.synthetic_mode = true;
        app.handle_click_action(ClickAction::ToggleRecordStop);
        assert!(app.is_recording);
        for _ in 0..5 {
            std::thread::sleep(Duration::from_millis(12));
            app.on_tick();
        }
        app.handle_click_action(ClickAction::DuplicateStagedSource(0));
        for _ in 0..6 {
            std::thread::sleep(Duration::from_millis(12));
            app.on_tick();
        }
        app.handle_click_action(ClickAction::ToggleRecordStop);
        for _ in 0..10 {
            std::thread::sleep(Duration::from_millis(12));
            app.on_tick();
            if !app.takes.is_empty() {
                break;
            }
        }
        assert_eq!(app.takes.len(), 1);
        assert!(app.takes[0].saved_path.is_none());
        assert!(app.takes[0].segments.len() >= 2);

        assert!(app.live_monitor_enabled);
        let (mon_rect, _) = hit_map
            .regions()
            .iter()
            .find(|(_, act)| *act == ClickAction::ToggleLiveMonitor)
            .expect("ToggleLiveMonitor hitbox");
        let mon_click = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: mon_rect.x + 1,
            row: mon_rect.y,
            modifiers: crossterm::event::KeyModifiers::NONE,
        };
        app.handle_mouse_event(mon_click, &hit_map);
        assert!(!app.live_monitor_enabled);

        app.handle_click_action(ClickAction::ToggleLiveMonitor);
        assert!(app.live_monitor_enabled);
        for _ in 0..5 {
            std::thread::sleep(Duration::from_millis(12));
            app.on_tick();
        }
        assert!(app.live_frames > 0);
        assert_eq!(app.takes.len(), 1);
    }

    #[test]
    fn device_modal_tab_displays_endpoint_badge_and_propagates_live_rename() {
        use crate::app::StagedSource;
        use crate::hit_test::ModalTab;
        use capture_system_or_process_audio::{CaptureTarget, DiscoveryEvent, DiscoveryEventKind};

        let backend = TestBackend::new(120, 36);
        let mut terminal = Terminal::new(backend).expect("test terminal");
        let mut app = App::new();
        let mut hit_map = HitMap::default();

        let dev_target = CaptureTarget::device("endpoint-guid-01")
            .expect("device target")
            .with_name(Some("Speakers (Realtek Audio)".into()));
        assert!(dev_target.active_audio().is_none());

        app.staged_sources.push(StagedSource {
            enabled: true,
            target: dev_target.clone(),
        });
        assert_eq!(
            app.staged_sources[1].display_name(),
            "Speakers (Realtek Audio)"
        );

        app.open_add_source_modal();
        app.handle_click_action(ClickAction::SwitchModalTab(ModalTab::OutputDevices));
        terminal
            .draw(|f| ui::render(f, &app, &mut hit_map))
            .expect("draw devices tab");

        let renamed = dev_target.with_name(Some("Studio Headphones (Realtek)".into()));
        app.apply_discovery_event(&DiscoveryEvent {
            revision: 42,
            kind: DiscoveryEventKind::StateChanged(renamed),
        });

        assert_eq!(
            app.staged_sources[1].display_name(),
            "Studio Headphones (Realtek)"
        );
        assert!(
            app.modal
                .last_event_note
                .contains("Device renamed -> Studio Headphones (Realtek)")
        );
    }

    #[test]
    fn cycling_channels_to_mono_builds_known_layout_converter_and_passes_admission() {
        use capture_system_or_process_audio::{
            ByteOrder, Channel, Format, Layout, SampleType, manager::MultiInputManager,
        };
        use std::sync::Arc;

        let mut app = App::new();
        app.handle_click_action(ClickAction::CycleChannels);
        assert!(app.use_converter);
        assert_eq!(app.converter_channels, 1);

        let req = app.build_capture_request().expect("capture request");
        let conv = req.output_converter().expect("converter enabled");
        assert_eq!(
            conv.target_format.layout(),
            &Layout::Known(Arc::from([Channel::Mono]))
        );
        assert!(
            req.inspect().is_ok(),
            "inspect() must accept mono converter"
        );

        let native_pcm = Arc::new(
            Format::new(
                SampleType::F32,
                ByteOrder::Little,
                48_000,
                2,
                Layout::Known(Arc::from([Channel::FrontLeft, Channel::FrontRight])),
            )
            .expect("native stereo format"),
        );
        let multi = MultiInputManager::new(req.clone(), native_pcm, 4096)
            .expect("MultiInputManager admission must succeed for Stereo -> Mono converter");
        assert_eq!(multi.targets().len(), 1);

        let mut sess =
            crate::backend::PlatformCaptureSession::open_synthetic(&req).expect("open synthetic");
        assert_eq!(sess.targets().len(), 1);
        app.handle_click_action(ClickAction::DuplicateStagedSource(0));
        let req2 = app.build_capture_request().expect("two-target request");
        sess.hot_swap(req2, capture_system_or_process_audio::Overflow::DropOldest)
            .expect("hot_swap synthetic");
        assert_eq!(sess.targets().len(), 2);
    }

    #[test]
    fn modal_candidate_mouse_hover_renders_full_target_details_popup() {
        use crate::app::ModalCandidate;
        use capture_system_or_process_audio::{
            CaptureTarget, OnRootExit, ProcessIdentity, ProcessScope,
        };
        use crossterm::event::{KeyModifiers, MouseEvent, MouseEventKind};

        let backend = TestBackend::new(120, 36);
        let mut terminal = Terminal::new(backend).expect("test terminal");
        let mut app = App::new();
        let mut hit_map = HitMap::default();

        app.open_add_source_modal();
        let long_name = "UltraLongApplicationDisplayName_ForAudioMasteringSuite_2026";
        let long_path =
            r"C:\Program Files\AudioMasteringSuite2026\SubFolder\Binaries\AudioMasteringEngine.exe";
        let target = CaptureTarget::application(
            ProcessIdentity {
                pid: 42424,
                generation: 9876543210,
            },
            Some("com.example.AudioMasteringSuite2026".into()),
            Some(long_name.into()),
            Some(long_path.into()),
            ProcessScope::Tree,
            OnRootExit::Stop,
        )
        .expect("valid application target")
        .with_active_audio(Some(true));

        app.modal.candidates = vec![ModalCandidate {
            selected: false,
            target,
        }];
        app.modal.cursor_idx = 0;

        terminal
            .draw(|f| ui::render(f, &app, &mut hit_map))
            .expect("draw modal before hover");

        let (cand_rect, _) = hit_map
            .regions()
            .iter()
            .find(|(_, act)| *act == ClickAction::ToggleModalCandidate(0))
            .expect("ToggleModalCandidate(0) hitbox");

        app.handle_mouse_event(
            MouseEvent {
                kind: MouseEventKind::Moved,
                column: cand_rect.x + 4,
                row: cand_rect.y,
                modifiers: KeyModifiers::NONE,
            },
            &hit_map,
        );
        assert_eq!(
            app.modal.hovered_candidate,
            Some((0, cand_rect.x + 4, cand_rect.y))
        );

        terminal
            .draw(|f| ui::render(f, &app, &mut hit_map))
            .expect("draw modal with hover popup");

        let buf = terminal.backend().buffer();
        let mut rendered_text = String::new();
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                rendered_text.push_str(buf[(x, y)].symbol());
            }
            rendered_text.push('\n');
        }

        assert!(
            rendered_text.contains("APPLICATION · PLAYING"),
            "popup title badge must be rendered"
        );
        assert!(
            rendered_text.contains("AudioMasteringEngine.exe"),
            "full executable filename must be visible in popup"
        );
        assert!(
            rendered_text.contains("com.example.AudioMasteringSuite2026"),
            "full app_id must be visible in popup"
        );
        assert!(
            rendered_text.contains("42424"),
            "PID must be visible in popup"
        );

        app.handle_mouse_event(
            MouseEvent {
                kind: MouseEventKind::Moved,
                column: 1,
                row: 1,
                modifiers: KeyModifiers::NONE,
            },
            &hit_map,
        );
        assert!(app.modal.hovered_candidate.is_none());
    }

    #[test]
    fn modal_state_changed_updates_in_place_without_swapping_cursor_or_hover_rows() {
        use crate::app::ModalCandidate;
        use capture_system_or_process_audio::{
            CaptureTarget, DiscoveryEvent, DiscoveryEventKind, OnRootExit, ProcessIdentity,
            ProcessScope,
        };

        let mut app = App::new();
        app.synthetic_mode = true;
        app.modal.open = true;
        app.modal.playing_only = false;

        let chrome = CaptureTarget::application(
            ProcessIdentity {
                pid: 30208,
                generation: 100,
            },
            Some("com.google.Chrome".into()),
            Some("Google Chrome".into()),
            Some("/Applications/Google Chrome.app/Contents/MacOS/Google Chrome".into()),
            ProcessScope::Tree,
            OnRootExit::Stop,
        )
        .unwrap()
        .with_active_audio(Some(true));

        let canary = CaptureTarget::application(
            ProcessIdentity {
                pid: 73325,
                generation: 200,
            },
            Some("com.google.Chrome.canary".into()),
            Some("Google Chrome Canary".into()),
            Some(
                "/Applications/Google Chrome Canary.app/Contents/MacOS/Google Chrome Canary".into(),
            ),
            ProcessScope::Tree,
            OnRootExit::Stop,
        )
        .unwrap()
        .with_active_audio(Some(false));

        app.modal.candidates = vec![
            ModalCandidate {
                selected: false,
                target: chrome.clone(),
            },
            ModalCandidate {
                selected: false,
                target: canary.clone(),
            },
        ];
        app.modal.cursor_idx = 0;
        app.modal.hovered_candidate = Some((1, 20, 8));

        // State changes must not reorder the cursor or hovered row.
        let chrome_idle = chrome.with_active_audio(Some(false));
        app.apply_discovery_event(&DiscoveryEvent {
            revision: 10,
            kind: DiscoveryEventKind::StateChanged(chrome_idle),
        });

        let canary_playing = canary.with_active_audio(Some(true));
        app.apply_discovery_event(&DiscoveryEvent {
            revision: 11,
            kind: DiscoveryEventKind::StateChanged(canary_playing),
        });

        assert_eq!(app.modal.candidates[0].target.pid(), Some(30208));
        assert_eq!(app.modal.candidates[0].target.active_audio(), Some(false));
        assert_eq!(app.modal.candidates[1].target.pid(), Some(73325));
        assert_eq!(app.modal.candidates[1].target.active_audio(), Some(true));
        assert_eq!(app.modal.cursor_idx, 0);
        assert_eq!(app.modal.hovered_candidate, Some((1, 20, 8)));
    }

    #[test]
    fn microphones_discovery_query_planning_and_playback_monitor_mute() {
        use crate::hit_test::ModalTab;
        use capture_system_or_process_audio::{
            CombineMode, CombineResolution, DefaultDeviceMode, DiscoveryDedupe, DiscoveryEvent,
            DiscoveryEventKind, DiscoveryOrder, InputConversionPolicy, TargetKind,
        };

        let cargo_toml = include_str!("../Cargo.toml");
        for forbidden in [
            "capture-system-or-process-audio-windows",
            "capture-system-or-process-audio-linux",
            "capture-system-or-process-audio-macos",
        ] {
            assert!(
                !cargo_toml.contains(forbidden),
                "tools/cspa-tui-demo/Cargo.toml must not import {forbidden}"
            );
        }
        let backend_rs = include_str!("backend.rs");
        assert!(
            !backend_rs.contains("#[cfg"),
            "tools/cspa-tui-demo/src/backend.rs must have 0 #[cfg] attributes"
        );

        let backend = TestBackend::new(120, 36);
        let mut terminal = Terminal::new(backend).expect("test terminal");
        let mut app = App::new();
        app.synthetic_mode = true;
        let mut hit_map = HitMap::default();

        app.open_add_source_modal();
        app.handle_click_action(ClickAction::SwitchModalTab(ModalTab::Microphones));
        assert_eq!(app.modal.tab, ModalTab::Microphones);
        assert!(
            app.modal
                .candidates
                .iter()
                .any(|c| c.target.kind() == TargetKind::DefaultInput),
            "Microphones tab must surface Default Microphone"
        );

        app.handle_click_action(ClickAction::ToggleModalDedupe);
        assert_eq!(app.modal.dedupe, DiscoveryDedupe::PerBackend);
        app.handle_click_action(ClickAction::ToggleModalOrder);
        assert_eq!(app.modal.order, DiscoveryOrder::BackendThenKind);
        app.handle_click_action(ClickAction::CycleModalBackendFilter);
        assert!(app.modal.backend_filter.is_some());
        // Stage the logical microphone rather than a backend-specific target.
        while app.modal.backend_filter.is_some() {
            app.handle_click_action(ClickAction::CycleModalBackendFilter);
        }
        app.handle_click_action(ClickAction::ToggleModalDedupe);
        assert_eq!(app.modal.dedupe, DiscoveryDedupe::ByLogicalTarget);

        app.apply_discovery_event(&DiscoveryEvent {
            revision: 77,
            kind: DiscoveryEventKind::DefaultInputChanged {
                device_id: "mic-usb-01".into(),
            },
        });
        assert!(
            app.modal
                .last_event_note
                .contains("Default mic -> mic-usb-01")
        );

        terminal
            .draw(|f| ui::render(f, &app, &mut hit_map))
            .expect("draw microphones modal");

        let mic_idx = app
            .modal
            .candidates
            .iter()
            .position(|c| c.target.kind() == TargetKind::DefaultInput)
            .expect("DefaultInput candidate");
        app.handle_click_action(ClickAction::ToggleModalCandidate(mic_idx));
        app.handle_click_action(ClickAction::ConfirmAddModalCandidates);
        assert_eq!(app.staged_sources.len(), 2);
        assert_eq!(app.staged_sources[1].kind_badge(), "MIC");

        assert_eq!(
            app.staged_sources[1].target.default_device_mode(),
            DefaultDeviceMode::Follow
        );
        app.handle_click_action(ClickAction::CycleStagedDeviceMode(1));
        assert_eq!(
            app.staged_sources[1].target.default_device_mode(),
            DefaultDeviceMode::Pin
        );
        app.handle_click_action(ClickAction::CycleStagedDeviceMode(1));
        assert_eq!(
            app.staged_sources[1].target.default_device_mode(),
            DefaultDeviceMode::Follow
        );

        app.handle_click_action(ClickAction::CycleStagedBackend(1));
        assert!(app.staged_sources[1].target.backend().is_some());
        while app.staged_sources[1].target.backend().is_some() {
            app.handle_click_action(ClickAction::CycleStagedBackend(1));
        }

        app.handle_click_action(ClickAction::CycleStagedGain(1));
        assert!((app.staged_sources[1].target.gain() - 0.5).abs() < 1e-4);
        for _ in 0..3 {
            app.handle_click_action(ClickAction::CycleStagedGain(1));
        }
        assert!((app.staged_sources[1].target.gain() - 1.0).abs() < 1e-4);

        assert_eq!(
            app.current_plan.as_ref().unwrap().combine_resolution,
            CombineResolution::CspaMixer
        );
        app.handle_click_action(ClickAction::CycleCombineMode);
        assert_eq!(app.combine_mode, CombineMode::NativeOnly);
        assert!(
            app.current_plan.is_err(),
            "CombineMode::NativeOnly must reject [DefaultOutput + Microphone]"
        );
        app.handle_click_action(ClickAction::CycleCombineMode);
        assert_eq!(app.combine_mode, CombineMode::CspaMixer);
        assert!(app.current_plan.is_ok());
        app.handle_click_action(ClickAction::CycleCombineMode);
        assert_eq!(app.combine_mode, CombineMode::Auto);

        app.handle_click_action(ClickAction::ToggleInputConversion);
        assert_eq!(
            app.input_conversion,
            InputConversionPolicy::StrictNoResample
        );
        app.handle_click_action(ClickAction::ToggleInputConversion);
        assert_eq!(app.input_conversion, InputConversionPolicy::AutoWhenMixing);

        app.start_recording();
        for _ in 0..6 {
            std::thread::sleep(Duration::from_millis(12));
            app.on_tick();
        }
        terminal
            .draw(|f| ui::render(f, &app, &mut hit_map))
            .expect("draw active heterogeneous recording");

        let buf = terminal.backend().buffer();
        let mut screen = String::new();
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                screen.push_str(buf[(x, y)].symbol());
            }
            screen.push('\n');
        }
        assert!(
            screen.contains("48k->48k BYPASS") && screen.contains("16k 1ch->48k 2ch SYNC"),
            "Telemetry panel must render both BYPASS and SYNC normalizer badges:\n{screen}"
        );
        assert!(
            screen.contains("Skew cross-clock"),
            "Cross-clock [DefaultOutput + Microphone] must report Skew cross-clock:\n{screen}"
        );

        app.stop_recording();
        for _ in 0..10 {
            std::thread::sleep(Duration::from_millis(12));
            app.on_tick();
            if !app.takes.is_empty() {
                break;
            }
        }
        assert_eq!(app.takes.len(), 1);

        // Playback mutes the live monitor to prevent acoustic feedback.
        app.playback_status.active = true;
        app.playback_status.paused = false;
        app.sync_worker_monitor();
        std::thread::sleep(Duration::from_millis(30));
        while app.worker.try_recv().is_some() {}
        let frames_before = app.live_frames;
        std::thread::sleep(Duration::from_millis(35));
        app.on_tick();
        assert_eq!(
            app.live_frames, frames_before,
            "live capture monitor must remain parked while playback is active"
        );
    }

    fn collect_screen_text(terminal: &Terminal<TestBackend>) -> String {
        let buf = terminal.backend().buffer();
        let mut screen = String::new();
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                screen.push_str(buf[(x, y)].symbol());
            }
            screen.push('\n');
        }
        screen
    }

    #[test]
    fn permissions_surfaces_and_modal_render_across_os_profiles_and_recover_on_grant() {
        use crate::app::StagedSource;
        use crate::backend::PermissionJobOutcome;
        use capture_system_or_process_audio::{
            CaptureTarget, DefaultDeviceMode, OnRootExit, PermissionEntry, PermissionKind,
            PermissionPolicy, PermissionReport, PermissionState, PlatformOs, ProcessIdentity,
            ProcessScope,
        };
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

        for width in [100u16, 120u16] {
            let mut terminal = Terminal::new(TestBackend::new(width, 36)).expect("test terminal");
            let mut app = App::new();
            app.synthetic_mode = true;
            let mut hit_map = HitMap::default();

            app.staged_sources.push(StagedSource {
                enabled: true,
                target: CaptureTarget::microphone(DefaultDeviceMode::Follow),
            });
            app.staged_sources.push(StagedSource {
                enabled: true,
                target: CaptureTarget::application(
                    ProcessIdentity {
                        pid: 9001,
                        generation: 12345,
                    },
                    Some("com.example.SynthApp".into()),
                    Some("SynthApp".into()),
                    Some("/Applications/SynthApp.app/Contents/MacOS/SynthApp".into()),
                    ProcessScope::Exact,
                    OnRootExit::Stop,
                )
                .expect("valid exact app"),
            });

            let mac_initial = PermissionReport::new(
                PlatformOs::MacOs,
                PermissionEntry {
                    kind: PermissionKind::Microphone,
                    state: PermissionState::NotDetermined,
                    required: true,
                    can_prompt: true,
                    requires_relaunch: false,
                },
                PermissionEntry {
                    kind: PermissionKind::SystemAudio,
                    state: PermissionState::Unknown,
                    required: true,
                    can_prompt: true,
                    requires_relaunch: false,
                },
                PermissionEntry {
                    kind: PermissionKind::ScreenRecording,
                    state: PermissionState::Denied,
                    required: true,
                    can_prompt: false,
                    requires_relaunch: true,
                },
            );
            app.set_permission_report_for_test(mac_initial);

            terminal
                .draw(|f| ui::render(f, &app, &mut hit_map))
                .expect("draw macos main view");
            let screen = collect_screen_text(&terminal);

            assert!(
                screen.contains("[Perms:DENY!(K)]"),
                "width {width} header must show DENY! pill:\n{screen}"
            );
            assert!(
                screen.contains("Mic:ASK*")
                    && screen.contains("Sys:UNK*")
                    && screen.contains("Scr:DNY*")
                    && screen.contains("[Pol:AutoRq]"),
                "width {width} panel [2] must show Mic/Sys/Scr chips and Policy toggle:\n{screen}"
            );
            assert!(
                screen.contains("[APP!]"),
                "width {width} staged sources must mark denied exact app with [APP!]:\n{screen}"
            );

            for action in [
                ClickAction::TogglePermissionPolicy,
                ClickAction::RefreshPermissions,
                ClickAction::RequestRequiredPermissions,
                ClickAction::OpenPermissionsModal,
            ] {
                let (rect, _) = hit_map
                    .regions()
                    .iter()
                    .find(|(_, act)| *act == action)
                    .unwrap_or_else(|| panic!("width {width}: missing hitbox for {action:?}"));
                assert!(rect.width > 0 && rect.height > 0);
            }

            app.handle_click_action(ClickAction::TogglePermissionPolicy);
            assert_eq!(app.permission_policy, PermissionPolicy::CheckOnly);
            assert_eq!(
                app.build_capture_request().unwrap().permission_policy(),
                PermissionPolicy::CheckOnly
            );

            app.open_add_source_modal();
            app.set_permission_report_for_test(mac_initial);
            terminal
                .draw(|f| ui::render(f, &app, &mut hit_map))
                .expect("draw add source modal with permissions");
            let modal_screen = collect_screen_text(&terminal);
            assert!(
                modal_screen.contains("[ScreenRec:Settings (G)]"),
                "Applications tab on macOS with denied SCK must offer Settings pill:\n{modal_screen}"
            );

            app.handle_key_event(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE));
            assert!(!app.modal.open);
            assert!(app.permissions_modal.open);

            terminal
                .draw(|f| ui::render(f, &app, &mut hit_map))
                .expect("draw permissions modal");
            let perm_modal_screen = collect_screen_text(&terminal);
            assert!(
                perm_modal_screen.contains("OS Recording Permissions & Consent (macOS)")
                    && perm_modal_screen.contains("[Policy: CheckOnly   (P)]")
                    && perm_modal_screen.contains("[Ask OS (1)]")
                    && perm_modal_screen.contains("[Settgs (3)]")
                    && perm_modal_screen.contains("CoreAudio CATap"),
                "macOS Permissions modal must render per-domain actions and diagnostics:\n{perm_modal_screen}"
            );

            // Modal hitboxes mask background controls.
            assert!(
                !hit_map
                    .regions()
                    .iter()
                    .any(|(_, act)| *act == ClickAction::CycleCombineMode),
                "background hitboxes must be masked while PermissionsModal is open"
            );
            assert!(hit_map.regions().iter().any(|(_, act)| {
                *act == ClickAction::RequestDomainPermission(PermissionKind::Microphone)
            }));
            assert!(hit_map.regions().iter().any(|(_, act)| {
                *act == ClickAction::OpenDomainSettings(PermissionKind::ScreenRecording)
            }));

            app.handle_key_event(KeyEvent::new(KeyCode::Char('o'), KeyModifiers::NONE));
            assert!(!app.open_settings_if_denied);
            app.handle_key_event(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::NONE));
            assert_eq!(app.permission_policy, PermissionPolicy::AutoRequest);
            app.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
            assert!(!app.permissions_modal.open);

            let mac_relaunch = PermissionReport::new(
                PlatformOs::MacOs,
                PermissionEntry {
                    kind: PermissionKind::Microphone,
                    state: PermissionState::Granted,
                    required: true,
                    can_prompt: false,
                    requires_relaunch: false,
                },
                PermissionEntry {
                    kind: PermissionKind::SystemAudio,
                    state: PermissionState::Granted,
                    required: true,
                    can_prompt: false,
                    requires_relaunch: false,
                },
                PermissionEntry {
                    kind: PermissionKind::ScreenRecording,
                    state: PermissionState::Granted,
                    required: true,
                    can_prompt: false,
                    requires_relaunch: true,
                },
            );
            app.permission_runner.inject_outcome(PermissionJobOutcome {
                label: "Request screen_recording".to_owned(),
                result: Ok(mac_relaunch),
            });
            app.on_tick();
            terminal
                .draw(|f| ui::render(f, &app, &mut hit_map))
                .expect("draw relaunch state");
            let rel_screen = collect_screen_text(&terminal);
            assert!(rel_screen.contains("[Perms:RLNCH(K)]"));
            assert!(rel_screen.contains("[APP~]"));

            app.error_banner = Some("Live monitor open failed: permission denied".to_owned());
            let mac_all_ok = PermissionReport::new(
                PlatformOs::MacOs,
                PermissionEntry {
                    kind: PermissionKind::Microphone,
                    state: PermissionState::Granted,
                    required: true,
                    can_prompt: false,
                    requires_relaunch: false,
                },
                PermissionEntry {
                    kind: PermissionKind::SystemAudio,
                    state: PermissionState::Granted,
                    required: true,
                    can_prompt: false,
                    requires_relaunch: false,
                },
                PermissionEntry {
                    kind: PermissionKind::ScreenRecording,
                    state: PermissionState::Granted,
                    required: true,
                    can_prompt: false,
                    requires_relaunch: false,
                },
            );
            app.permission_runner.inject_outcome(PermissionJobOutcome {
                label: "Re-verify permissions".to_owned(),
                result: Ok(mac_all_ok),
            });
            app.on_tick();
            assert!(
                app.error_banner.is_none(),
                "permission grant must clear stale permission error banner"
            );

            let win_denied = PermissionReport::new(
                PlatformOs::Windows,
                PermissionEntry {
                    kind: PermissionKind::Microphone,
                    state: PermissionState::Denied,
                    required: true,
                    can_prompt: false,
                    requires_relaunch: false,
                },
                PermissionEntry {
                    kind: PermissionKind::SystemAudio,
                    state: PermissionState::Granted,
                    required: true,
                    can_prompt: false,
                    requires_relaunch: false,
                },
                PermissionEntry {
                    kind: PermissionKind::ScreenRecording,
                    state: PermissionState::Granted,
                    required: false,
                    can_prompt: false,
                    requires_relaunch: false,
                },
            );
            app.set_permission_report_for_test(win_denied);
            app.permissions_modal.open = true;
            terminal
                .draw(|f| ui::render(f, &app, &mut hit_map))
                .expect("draw windows modal");
            let win_screen = collect_screen_text(&terminal);
            assert!(
                win_screen.contains("OS Recording Permissions & Consent (Windows)")
                    && win_screen.contains("[Settgs (1)]")
                    && win_screen.contains("ms-settings:privacy-microphone"),
                "Windows denied mic must offer [Settgs (1)] and ms-settings URI:\n{win_screen}"
            );

            let linux_restricted = PermissionReport::new(
                PlatformOs::Linux,
                PermissionEntry {
                    kind: PermissionKind::Microphone,
                    state: PermissionState::Restricted,
                    required: true,
                    can_prompt: false,
                    requires_relaunch: false,
                },
                PermissionEntry {
                    kind: PermissionKind::SystemAudio,
                    state: PermissionState::Restricted,
                    required: true,
                    can_prompt: false,
                    requires_relaunch: false,
                },
                PermissionEntry {
                    kind: PermissionKind::ScreenRecording,
                    state: PermissionState::Restricted,
                    required: false,
                    can_prompt: false,
                    requires_relaunch: false,
                },
            );
            app.set_permission_report_for_test(linux_restricted);
            terminal
                .draw(|f| ui::render(f, &app, &mut hit_map))
                .expect("draw linux modal");
            let linux_screen = collect_screen_text(&terminal);
            assert!(
                linux_screen.contains("OS Recording Permissions & Consent (Linux)")
                    && linux_screen.contains("Flatpak")
                    && !linux_screen.contains("[Settgs"),
                "Linux restricted state must explain Flatpak/socket remediation without fake Settings buttons:\n{linux_screen}"
            );
        }
    }
}
