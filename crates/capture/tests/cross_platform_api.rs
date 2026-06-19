#![forbid(unsafe_code)]

use capture_system_or_process_audio::{
    AudioStream, CapabilityError, CaptureManager, CaptureTarget, CombineMode, CombineResolution,
    DefaultDeviceMode, DefaultEndpointObserver, DefaultOutputObserver, Discovery, DiscoveryDedupe,
    DiscoveryFilter, DiscoveryOrder, DiscoveryQuery, MixerConfig, Overflow, PartialReason,
    PermissionKind, PermissionPolicy, PlatformOs, Stop, capture, capture_single,
    check_permission_kinds, check_permissions, check_request_permissions, discover, discover_query,
    open_stream, request, request_permissions, subscribe_discovery,
};
use std::thread;
use std::time::{Duration, Instant};

#[test]
fn cross_platform_api_test_has_zero_cfg_attributes_and_forbids_unsafe() {
    let lib_src = include_str!("../src/lib.rs");
    assert!(
        lib_src.starts_with("#![forbid(unsafe_code)]"),
        "crates/capture/src/lib.rs must start with #![forbid(unsafe_code)]"
    );

    let self_src = include_str!("cross_platform_api.rs");
    assert!(
        self_src.starts_with("#![forbid(unsafe_code)]"),
        "cross_platform_api.rs must forbid unsafe code"
    );
    let outer_cfg = concat!("#[", "cfg");
    let inner_cfg = concat!("#![", "cfg");
    assert!(
        !self_src.contains(outer_cfg) && !self_src.contains(inner_cfg),
        "cross_platform_api.rs must contain 0 conditional compilation attributes"
    );
    for os_crate in [
        concat!("capture_system_or_process_audio_", "windows"),
        concat!("capture_system_or_process_audio_", "linux"),
        concat!("capture_system_or_process_audio_", "macos"),
    ] {
        assert!(
            !self_src.contains(os_crate),
            "cross_platform_api.rs must not import OS-specific crate {os_crate}"
        );
    }
}

#[test]
fn unified_facade_discovery_inspect_stream_manager_zero_alloc_hot_swap_and_drop()
-> Result<(), CapabilityError> {
    let disc = Discovery::new();
    assert!(!disc.is_watcher_running());

    let out_devs = disc.output_devices()?;
    assert!(
        !out_devs.is_empty(),
        "expected at least one output device on {:?}",
        PlatformOs::current()
    );
    let _in_devs = disc.input_devices()?;
    let default_out_id = disc.default_output_device_id()?;
    assert!(default_out_id.is_some());
    assert!(!disc.is_watcher_running());

    let snap = discover(DiscoveryFilter::all(false, false))?;
    assert!(!snap.targets.is_empty());

    let per_backend_q = DiscoveryQuery::default()
        .with_dedupe(DiscoveryDedupe::PerBackend)
        .with_order(DiscoveryOrder::BackendThenKind);
    let cat = discover_query(&per_backend_q)?;
    assert!(!cat.output_devices.is_empty());

    let sub = disc.subscribe_query(DiscoveryQuery::default(), 256)?;
    assert!(disc.is_watcher_running());
    let resnap = sub.resnapshot_catalog()?;
    assert!(!resnap.output_devices.is_empty());
    drop(sub);
    let wait_t0 = Instant::now();
    while disc.is_watcher_running() && wait_t0.elapsed() < Duration::from_millis(600) {
        thread::sleep(Duration::from_millis(10));
    }
    assert!(!disc.is_watcher_running());

    let sub2 = subscribe_discovery(DiscoveryQuery::default(), 128)?;
    assert!(!sub2.resnapshot_catalog()?.output_devices.is_empty());
    drop(sub2);

    let out_obs = DefaultOutputObserver::acquire();
    let cur_ep = out_obs.latest_endpoint_id();
    assert!(cur_ep.is_some());
    let _ = out_obs.changed_from(cur_ep.as_deref());
    let obs_allocs = allocation_counter::measure(|| {
        for _ in 0..200 {
            let _ = out_obs.changed_from(cur_ep.as_deref());
        }
    });
    assert_eq!(obs_allocs.count_total, 0);
    drop(out_obs);

    let t_follow = CaptureTarget::default_output(DefaultDeviceMode::Follow);
    let t_pin = CaptureTarget::default_output(DefaultDeviceMode::Pin);
    let first_dev_id = out_devs[0]
        .clean_device_id()
        .ok_or(CapabilityError::InvalidTarget)?
        .to_owned();
    let t_dev = CaptureTarget::device(first_dev_id.as_str())?;

    let pt_plan = request(vec![t_follow.clone()]).inspect()?;
    assert!(!pt_plan.uses_software_mixer);
    let mix_plan = request(vec![t_follow.clone(), t_dev.clone()]).inspect()?;
    assert!(mix_plan.uses_software_mixer);
    assert_eq!(mix_plan.combine_resolution, CombineResolution::CspaMixer);

    let all_perms = check_permissions(&[])?;
    assert_eq!(all_perms.os, PlatformOs::current());
    assert_eq!(all_perms.entries().len(), 3);
    assert_eq!(all_perms.microphone().kind, PermissionKind::Microphone);
    assert_eq!(all_perms.system_audio().kind, PermissionKind::SystemAudio);
    assert_eq!(
        all_perms.screen_recording().kind,
        PermissionKind::ScreenRecording
    );

    let out_req_check_only =
        request(vec![t_follow.clone()]).with_permission_policy(PermissionPolicy::CheckOnly);
    let out_perms = check_request_permissions(&out_req_check_only)?;
    assert!(out_perms.system_audio().required);
    assert!(!out_perms.microphone().required);
    assert!(!out_perms.has_denial());

    let mic_only_perms =
        check_permission_kinds(&[PermissionKind::Microphone], &Default::default())?;
    assert!(mic_only_perms.microphone().required);
    assert!(!mic_only_perms.system_audio().required);

    let req_out_perms =
        request_permissions(std::slice::from_ref(&t_follow), Duration::from_millis(500))?;
    assert!(!req_out_perms.has_denial());

    let mut stream = AudioStream::open(&t_pin, Overflow::DropOldest)?;
    assert!(stream.format().rate() >= 8_000);
    let s_bpf = stream.format().bytes_per_frame();
    let mut s_dst = vec![0u8; 128 * s_bpf];
    for _ in 0..2 {
        let _ = stream
            .read_into(&mut s_dst, 128, Duration::ZERO)
            .expect("warm AudioStream::read_into");
    }
    let stream_allocs = allocation_counter::measure(|| {
        for _ in 0..64 {
            let _ = stream
                .read_into(&mut s_dst, 128, Duration::ZERO)
                .expect("steady AudioStream::read_into");
        }
    });
    assert_eq!(
        stream_allocs.count_total, 0,
        "AudioStream::read_into allocated {} times in steady state",
        stream_allocs.count_total
    );
    stream.stop(Stop::Abort);
    assert!(stream.wait_cleanup(Duration::from_secs(2)));

    let stream2 = open_stream(&t_pin, Overflow::DropOldest)?;
    stream2.stop(Stop::Abort);
    assert!(stream2.wait_cleanup(Duration::from_secs(2)));

    let mixer_cfg = MixerConfig {
        silence_on_idle: true,
        ..MixerConfig::default()
    };
    let mix_req = request(vec![t_follow.clone()])
        .with_combine_mode(CombineMode::CspaMixer)
        .with_mixer_config(mixer_cfg);
    let mut mgr = CaptureManager::open(mix_req)?;
    assert!(mgr.is_software_mix());

    let m_bpf = mgr
        .output_format()
        .ok_or(CapabilityError::UnsupportedFormat)?
        .bytes_per_frame();
    let mut m_dst = vec![0u8; 240 * m_bpf];
    for _ in 0..4 {
        let r = mgr
            .read_into(&mut m_dst, 240, Duration::from_millis(25))
            .expect("warm CaptureManager::read_into");
        assert_eq!(r.frames, 240);
    }

    let mgr_allocs = allocation_counter::measure(|| {
        for _ in 0..64 {
            let r = mgr
                .read_into(&mut m_dst, 240, Duration::ZERO)
                .expect("steady CaptureManager::read_into");
            assert_eq!(r.frames, 240);
        }
    });
    assert_eq!(
        mgr_allocs.count_total, 0,
        "CaptureManager::read_into allocated {} times in steady state",
        mgr_allocs.count_total
    );

    let seg2 = mgr.update_targets(vec![t_follow.clone(), t_dev])?;
    assert!(seg2 >= 2);
    assert_eq!(mgr.targets().len(), 2);
    assert_eq!(mgr.input_handles().len(), 2);
    let m_bpf2 = mgr
        .output_format()
        .ok_or(CapabilityError::UnsupportedFormat)?
        .bytes_per_frame();
    if m_dst.len() < 240 * m_bpf2 {
        m_dst.resize(240 * m_bpf2, 0);
    }
    for _ in 0..6 {
        let r = mgr
            .read_into(&mut m_dst, 240, Duration::from_millis(25))
            .expect("post-swap warm CaptureManager::read_into");
        if r.reason != PartialReason::Boundary {
            assert_eq!(r.frames, 240);
        }
    }
    let swap_allocs = allocation_counter::measure(|| {
        for _ in 0..32 {
            let r = mgr
                .read_into(&mut m_dst, 240, Duration::ZERO)
                .expect("post-swap steady CaptureManager::read_into");
            assert_eq!(r.frames, 240);
        }
    });
    assert_eq!(
        swap_allocs.count_total, 0,
        "post-swap CaptureManager::read_into allocated {} times in steady state",
        swap_allocs.count_total
    );

    let seg3 = mgr.update_targets(vec![t_pin.clone()])?;
    assert!(seg3 > seg2);
    assert_eq!(mgr.targets().len(), 1);

    let drop_t0 = Instant::now();
    drop(mgr);
    let drop_ms = drop_t0.elapsed().as_millis() as u64;
    assert!(
        drop_ms <= 2_000,
        "CaptureManager Drop exceeded 2000ms budget ({drop_ms}ms)"
    );

    let pt1 = capture_single(t_pin)?;
    assert!(!pt1.is_software_mix());
    drop(pt1);

    let pt2 = capture(vec![t_follow])?;
    assert!(!pt2.is_software_mix());
    drop(pt2);

    println!(
        "{{\"event\":\"cross-platform-api\",\"os\":\"{:?}\",\"output_devices\":{},\"steady_state_obs_allocs\":{},\"steady_state_stream_read_into_allocs\":{},\"steady_state_mgr_read_into_allocs\":{},\"post_swap_mgr_read_into_allocs\":{},\"drop_cleanup_ms\":{},\"zero_cfg_verified\":true,\"pass\":true}}",
        PlatformOs::current(),
        out_devs.len(),
        obs_allocs.count_total,
        stream_allocs.count_total,
        mgr_allocs.count_total,
        swap_allocs.count_total,
        drop_ms
    );
    Ok(())
}
