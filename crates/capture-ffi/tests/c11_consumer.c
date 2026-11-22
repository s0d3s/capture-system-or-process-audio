#include "../include/cspa_experimental.h"
#include <stdio.h>
#include <string.h>
#include <time.h>

static uint32_t failing_callback(
    const cspa_managed_read_outcome_record *outcome,
    void *user_data
) {
    (void)outcome;
    int *invocations = (int *)user_data;
    (*invocations) += 1;
    return CSPA_STATUS_CALLBACK_FAILURE;
}

static int g_context_released = 0;

static void on_context_released(void *user_data) {
    (void)user_data;
    g_context_released += 1;
}

static void init_target(cspa_capture_target_record *t) {
    memset(t, 0, sizeof(*t));
    t->struct_size = (uint32_t)sizeof(*t);
    t->schema_revision = CSPA_EXPERIMENTAL_SCHEMA_REVISION;
}

#define REQUIRE(cond, msg) \
    do { \
        if (!(cond)) { \
            fprintf(stderr, "C11_CONSUMER_FAIL: %s (line %d)\n", msg, __LINE__); \
            return 1; \
        } \
    } while (0)

int main(int argc, char **argv) {
    REQUIRE(argc == 1 || (argc == 2 && strcmp(argv[1], "--headless") == 0), "unknown argument");
    REQUIRE(
        cspa_abi_check(CSPA_EXPERIMENTAL_ABI_MAJOR, CSPA_EXPERIMENTAL_SCHEMA_REVISION) == CSPA_STATUS_OK,
        "valid ABI must pass"
    );
    REQUIRE(cspa_abi_check(0u, 0u) == CSPA_STATUS_ABI_MISMATCH, "legacy schema_revision 0 must reject");
    REQUIRE(cspa_abi_check(1u, 1u) == CSPA_STATUS_ABI_MISMATCH, "bad ABI major must reject");

    cspa_capture_target_record bad_target;
    memset(&bad_target, 0, sizeof(bad_target));
    bad_target.struct_size = 48u;
    bad_target.schema_revision = CSPA_EXPERIMENTAL_SCHEMA_REVISION;
    REQUIRE(
        cspa_target_process(1234u, 999ull, CSPA_SCOPE_TREE, CSPA_ON_ROOT_EXIT_STOP, 0ull, 1.0f, &bad_target)
            == CSPA_STATUS_ABI_MISMATCH,
        "bad struct_size must reject"
    );

    init_target(&bad_target);
    bad_target.schema_revision = 0u;
    REQUIRE(
        cspa_target_microphone(CSPA_DEFAULT_DEVICE_FOLLOW, 1.0f, &bad_target) == CSPA_STATUS_ABI_MISMATCH,
        "stale schema_revision 0 on target must reject"
    );

    REQUIRE(cspa_test_simulate_panic_containment(0u) == CSPA_STATUS_OK, "normal call must pass");
    REQUIRE(
        cspa_test_simulate_panic_containment(1u) == CSPA_STATUS_INTERNAL_ERROR,
        "simulated panic must be contained at C boundary"
    );

    cspa_capture_target_record t_proc, t_app, t_lowered, t_dev, t_in_dev, t_def_out, t_mic;
    init_target(&t_proc);
    init_target(&t_app);
    init_target(&t_lowered);
    init_target(&t_dev);
    init_target(&t_in_dev);
    init_target(&t_def_out);
    init_target(&t_mic);

    REQUIRE(
        cspa_target_process(
            1234u, 999ull, CSPA_SCOPE_TREE, CSPA_ON_ROOT_EXIT_WAIT, 2500000000ull, 1.0f, &t_proc
        ) == CSPA_STATUS_OK,
        "cspa_target_process failed"
    );
    REQUIRE(t_proc.kind == CSPA_TARGET_KIND_PROCESS && t_proc.pid == 1234u, "process fields mismatch");

    REQUIRE(
        cspa_target_application(
            1234u,
            999ull,
            "com.example.studio",
            "StudioApp",
            "/Applications/StudioApp.app/Contents/MacOS/StudioApp",
            CSPA_SCOPE_TREE,
            CSPA_ON_ROOT_EXIT_STOP,
            0ull,
            1.0f,
            &t_app
        ) == CSPA_STATUS_OK,
        "cspa_target_application failed"
    );
    REQUIRE(t_app.kind == CSPA_TARGET_KIND_APPLICATION, "application kind mismatch");
    REQUIRE(strcmp(t_app.app_id, "com.example.studio") == 0, "application app_id mismatch");

    REQUIRE(
        cspa_target_lower_to_process(&t_app, &t_lowered) == CSPA_STATUS_OK
            && t_lowered.kind == CSPA_TARGET_KIND_PROCESS && t_lowered.pid == 1234u,
        "cspa_target_lower_to_process failed"
    );

    REQUIRE(
        cspa_target_device("speaker-endpoint-0", 1.0f, &t_dev) == CSPA_STATUS_OK
            && t_dev.kind == CSPA_TARGET_KIND_DEVICE
            && strcmp(t_dev.device_id, "speaker-endpoint-0") == 0,
        "cspa_target_device failed"
    );
    REQUIRE(
        cspa_target_input_device("usb-mic-0", 0.8f, &t_in_dev) == CSPA_STATUS_OK
            && t_in_dev.kind == CSPA_TARGET_KIND_INPUT_DEVICE
            && strcmp(t_in_dev.device_id, "usb-mic-0") == 0,
        "cspa_target_input_device failed"
    );
    REQUIRE(
        cspa_target_default_output(CSPA_DEFAULT_DEVICE_PIN, 1.0f, &t_def_out) == CSPA_STATUS_OK
            && t_def_out.kind == CSPA_TARGET_KIND_DEFAULT_OUTPUT,
        "cspa_target_default_output failed"
    );
    REQUIRE(
        cspa_target_microphone(CSPA_DEFAULT_DEVICE_FOLLOW, 1.0f, &t_mic) == CSPA_STATUS_OK
            && t_mic.kind == CSPA_TARGET_KIND_DEFAULT_INPUT,
        "cspa_target_microphone failed"
    );

    cspa_permission_report_record perm_rep;
    REQUIRE(cspa_permission_report_init(&perm_rep) == CSPA_STATUS_OK, "permission_report_init failed");
    REQUIRE(
        cspa_permissions_check(NULL, 0u, NULL, &perm_rep) == CSPA_STATUS_OK
            && perm_rep.entry_count == CSPA_PERMISSION_KIND_COUNT
            && perm_rep.entries[0].kind == CSPA_PERMISSION_KIND_MICROPHONE
            && perm_rep.entries[1].kind == CSPA_PERMISSION_KIND_SYSTEM_AUDIO
            && perm_rep.entries[2].kind == CSPA_PERMISSION_KIND_SCREEN_RECORDING,
        "cspa_permissions_check all domains failed"
    );
    REQUIRE(
        cspa_permissions_check(&t_def_out, 1u, NULL, &perm_rep) == CSPA_STATUS_OK
            && perm_rep.entries[1].required == 1u
            && perm_rep.entries[0].required == 0u,
        "cspa_permissions_check target-aware required flags failed"
    );

    cspa_capture_target_record dup_targets[3] = { t_proc, t_proc, t_app };
    cspa_capture_request_record req;
    REQUIRE(cspa_capture_request_init(&req, dup_targets, 3ull) == CSPA_STATUS_OK, "request_init failed");
    REQUIRE(req.permission_policy == CSPA_PERMISSION_POLICY_AUTO_REQUEST, "default permission_policy must be AutoRequest");

    cspa_target_plan_record tplans[4];
    memset(tplans, 0, sizeof(tplans));
    cspa_plan_report_record report;
    memset(&report, 0, sizeof(report));
    report.struct_size = (uint32_t)sizeof(report);
    report.schema_revision = CSPA_EXPERIMENTAL_SCHEMA_REVISION;
    report.target_plans = tplans;
    report.target_plans_capacity = 4ull;

    REQUIRE(
        cspa_request_inspect_for_os(CSPA_PLATFORM_OS_WINDOWS, &req, &report) == CSPA_STATUS_OK,
        "inspect_for_os Windows failed"
    );
    REQUIRE(
        report.combine_resolution == CSPA_COMBINE_RESOLUTION_CSPA_MIXER
            && report.uses_software_mixer == 1u
            && report.overlap_semantics == CSPA_OVERLAP_SOFTWARE_SUMMATION
            && report.duplicate_target_count == 1ull,
        "Windows duplicate-target software-mix plan mismatch"
    );

    cspa_capture_target_record t_app2;
    init_target(&t_app2);
    REQUIRE(
        cspa_target_application(
            5678u, 999ull, "com.example.player", "PlayerApp", "/usr/bin/player",
            CSPA_SCOPE_TREE, CSPA_ON_ROOT_EXIT_STOP, 0ull, 1.0f, &t_app2
        ) == CSPA_STATUS_OK,
        "cspa_target_application 2 failed"
    );
    cspa_capture_target_record distinct_linux[2] = { t_proc, t_app2 };
    REQUIRE(cspa_capture_request_init(&req, distinct_linux, 2ull) == CSPA_STATUS_OK, "init distinct_linux");
    REQUIRE(
        cspa_request_inspect_for_os(CSPA_PLATFORM_OS_LINUX, &req, &report) == CSPA_STATUS_OK,
        "inspect_for_os Linux Auto failed"
    );
    REQUIRE(
        report.selected_backend == CSPA_BACKEND_PIPEWIRE
            && report.combine_resolution == CSPA_COMBINE_RESOLUTION_SHARED_NATIVE_GRAPH_MIX
            && report.overlap_semantics == CSPA_OVERLAP_NATIVE_SET_UNION
            && report.duplicate_target_count == 0ull,
        "Linux SharedNativeGraphMix plan mismatch"
    );

    req.combine_mode = CSPA_COMBINE_MODE_CSPA_MIXER;
    REQUIRE(
        cspa_request_inspect_for_os(CSPA_PLATFORM_OS_LINUX, &req, &report) == CSPA_STATUS_OK
            && report.combine_resolution == CSPA_COMBINE_RESOLUTION_CSPA_MIXER
            && report.overlap_semantics == CSPA_OVERLAP_SOFTWARE_SUMMATION,
        "Linux forced CspaMixer plan mismatch"
    );

    cspa_capture_target_record t_pw_out = t_def_out;
    cspa_capture_target_record t_pa_mic = t_in_dev;
    REQUIRE(cspa_target_with_backend(&t_pw_out, CSPA_BACKEND_PIPEWIRE, &t_pw_out) == CSPA_STATUS_OK, "pin pw");
    REQUIRE(cspa_target_with_backend(&t_pa_mic, CSPA_BACKEND_PULSEAUDIO, &t_pa_mic) == CSPA_STATUS_OK, "pin pa");
    cspa_capture_target_record linux_xb[2] = { t_pw_out, t_pa_mic };
    REQUIRE(cspa_capture_request_init(&req, linux_xb, 2ull) == CSPA_STATUS_OK, "init linux_xb");
    REQUIRE(
        cspa_request_inspect_for_os(CSPA_PLATFORM_OS_LINUX, &req, &report) == CSPA_STATUS_OK
            && report.combine_resolution == CSPA_COMBINE_RESOLUTION_CSPA_MIXER
            && tplans[0].resolved_backend == CSPA_BACKEND_PIPEWIRE
            && tplans[1].resolved_backend == CSPA_BACKEND_PULSEAUDIO,
        "Linux cross-backend PipeWire+PulseAudio plan mismatch"
    );
    req.combine_mode = CSPA_COMBINE_MODE_NATIVE_ONLY;
    REQUIRE(
        cspa_request_inspect_for_os(CSPA_PLATFORM_OS_LINUX, &req, &report) == CSPA_STATUS_UNSUPPORTED_SCOPE,
        "CombineMode::NativeOnly must reject cross-backend request"
    );

    cspa_capture_target_record t_sck_app;
    init_target(&t_sck_app);
    REQUIRE(
        cspa_target_application(
            4242u, 999ull, "com.example.sck", "SckApp", "/Applications/SckApp.app",
            CSPA_SCOPE_EXACT, CSPA_ON_ROOT_EXIT_STOP, 0ull, 1.0f, &t_sck_app
        ) == CSPA_STATUS_OK,
        "create sck exact app failed"
    );
    REQUIRE(
        cspa_target_with_backend(&t_sck_app, CSPA_BACKEND_SCREENCAPTUREKIT, &t_sck_app) == CSPA_STATUS_OK,
        "pin sck failed"
    );
    cspa_capture_target_record t_ca_mic = t_mic;
    REQUIRE(cspa_target_with_backend(&t_ca_mic, CSPA_BACKEND_COREAUDIO, &t_ca_mic) == CSPA_STATUS_OK, "pin ca");
    cspa_capture_target_record mac_xb[2] = { t_sck_app, t_ca_mic };
    REQUIRE(cspa_capture_request_init(&req, mac_xb, 2ull) == CSPA_STATUS_OK, "init mac_xb");
    REQUIRE(
        cspa_request_inspect_for_os(CSPA_PLATFORM_OS_MACOS, &req, &report) == CSPA_STATUS_OK
            && report.combine_resolution == CSPA_COMBINE_RESOLUTION_CSPA_MIXER
            && tplans[0].resolved_backend == CSPA_BACKEND_SCREENCAPTUREKIT
            && tplans[1].resolved_backend == CSPA_BACKEND_COREAUDIO,
        "macOS cross-backend SCK+CoreAudio plan mismatch"
    );

    cspa_capture_target_record t_exact;
    init_target(&t_exact);
    REQUIRE(
        cspa_target_process(1234u, 999ull, CSPA_SCOPE_EXACT, CSPA_ON_ROOT_EXIT_STOP, 0ull, 1.0f, &t_exact)
            == CSPA_STATUS_OK,
        "create exact process failed"
    );
    REQUIRE(cspa_capture_request_init(&req, &t_exact, 1ull) == CSPA_STATUS_OK, "init exact req");
    REQUIRE(
        cspa_request_inspect_for_os(CSPA_PLATFORM_OS_WINDOWS, &req, &report) == CSPA_STATUS_UNSUPPORTED_SCOPE,
        "WASAPI Process scope=exact must reject"
    );

    REQUIRE(cspa_capture_request_init(&req, &t_proc, 1ull) == CSPA_STATUS_OK, "init linux kernel-only req");
    req.tree_strategy.start_level = CSPA_TREE_STRATEGY_NATIVE_KERNEL_CAPTURE;
    req.tree_strategy.fallback_limit = CSPA_TREE_STRATEGY_NATIVE_KERNEL_CAPTURE;
    REQUIRE(
        cspa_request_inspect_for_os(CSPA_PLATFORM_OS_LINUX, &req, &report) == CSPA_STATUS_UNSUPPORTED_SCOPE,
        "Linux NativeKernelCapture-only Process must reject"
    );

    cspa_capture_target_record t_sck_proc = t_proc;
    REQUIRE(
        cspa_target_with_backend(&t_sck_proc, CSPA_BACKEND_SCREENCAPTUREKIT, &t_sck_proc) == CSPA_STATUS_OK,
        "pin sck on proc failed"
    );
    REQUIRE(cspa_capture_request_init(&req, &t_sck_proc, 1ull) == CSPA_STATUS_OK, "init mac sck proc req");
    REQUIRE(
        cspa_request_inspect_for_os(CSPA_PLATFORM_OS_MACOS, &req, &report) == CSPA_STATUS_UNSUPPORTED_SCOPE,
        "macOS ScreenCaptureKit Process target must reject"
    );

    REQUIRE(cspa_capture_request_init(&req, &t_def_out, 1ull) == CSPA_STATUS_OK, "init precise req");
    req.require_precise_timing = 1u;
    REQUIRE(
        cspa_request_inspect(&req, &report) == CSPA_STATUS_PRECISE_TIMING_UNQUALIFIED,
        "require_precise_timing must reject"
    );

    if (argc == 2) {
        printf("C11_CONSUMER_HEADLESS_OK\n");
        return 0;
    }

    cspa_discovery *disc = NULL;
    REQUIRE(cspa_discovery_create(&disc) == CSPA_STATUS_OK && disc != NULL, "discovery_create failed");

    cspa_capture_target_record out_devs[32];
    memset(out_devs, 0, sizeof(out_devs));
    size_t out_dev_count = 0u;
    REQUIRE(
        cspa_discovery_output_devices(disc, out_devs, 32u, &out_dev_count) == CSPA_STATUS_OK
            && out_dev_count >= 1u,
        "expected >= 1 output device"
    );

    cspa_capture_target_record in_devs[32];
    memset(in_devs, 0, sizeof(in_devs));
    size_t in_dev_count = 0u;
    REQUIRE(
        cspa_discovery_input_devices(disc, in_devs, 32u, &in_dev_count) == CSPA_STATUS_OK,
        "cspa_discovery_input_devices failed"
    );
    size_t checked_in = in_dev_count < 32u ? in_dev_count : 32u;
    for (size_t i = 0u; i < checked_in; ++i) {
        REQUIRE(in_devs[i].kind == CSPA_TARGET_KIND_INPUT_DEVICE, "input_devices must return INPUT_DEVICE");
        REQUIRE(strstr(in_devs[i].device_id, ".monitor") == NULL, "monitor sink leaked into input_devices");
    }

    cspa_discovery_query_record q_logical;
    REQUIRE(cspa_discovery_query_init(&q_logical) == CSPA_STATUS_OK, "query_init failed");
    cspa_capture_target_record logical_targets[64];
    memset(logical_targets, 0, sizeof(logical_targets));
    size_t logical_count = 0u;
    REQUIRE(
        cspa_discover_query(&q_logical, logical_targets, 64u, &logical_count) == CSPA_STATUS_OK
            && logical_count >= 1u,
        "cspa_discover_query ByLogicalTarget failed"
    );
    size_t n_log = logical_count < 64u ? logical_count : 64u;
    for (size_t i = 0u; i < n_log; ++i) {
        REQUIRE(logical_targets[i].backend == CSPA_BACKEND_NONE, "ByLogicalTarget must leave backend unpinned");
    }

    cspa_discovery_query_record q_per_backend = q_logical;
    q_per_backend.dedupe = CSPA_DISCOVERY_DEDUPE_PER_BACKEND;
    q_per_backend.order = CSPA_DISCOVERY_ORDER_BACKEND_THEN_KIND;
    cspa_capture_target_record pb_targets[64];
    memset(pb_targets, 0, sizeof(pb_targets));
    size_t pb_count = 0u;
    REQUIRE(
        cspa_discovery_query(disc, &q_per_backend, pb_targets, 64u, &pb_count) == CSPA_STATUS_OK
            && pb_count >= logical_count,
        "cspa_discovery_query PerBackend failed"
    );
    size_t n_pb = pb_count < 64u ? pb_count : 64u;
    for (size_t i = 0u; i < n_pb; ++i) {
        REQUIRE(pb_targets[i].backend != CSPA_BACKEND_NONE, "PerBackend must pin target.backend");
    }

    cspa_discovery_subscription *sub = NULL;
    REQUIRE(
        cspa_discovery_subscribe_query(disc, &q_logical, 256u, &sub) == CSPA_STATUS_OK && sub != NULL,
        "subscribe_query failed"
    );
    uint64_t rev = 0ull;
    REQUIRE(cspa_test_discovery_publish_synthetic(disc, 257u, &rev) == CSPA_STATUS_OK, "publish_synthetic failed");
    uint32_t overflowed = 0u;
    REQUIRE(
        cspa_discovery_subscription_is_overflowed(sub, &overflowed) == CSPA_STATUS_OK && overflowed == 1u,
        "expected 256-event subscription overflow"
    );
    size_t snap_count = 0u;
    REQUIRE(
        cspa_discovery_subscription_resnapshot(sub, NULL, 0u, &rev, &snap_count) == CSPA_STATUS_OK,
        "resnapshot failed"
    );
    REQUIRE(
        cspa_discovery_subscription_is_overflowed(sub, &overflowed) == CSPA_STATUS_OK && overflowed == 0u,
        "overflow flag must clear after resnapshot"
    );
    cspa_discovery_subscription_release(sub);
    cspa_discovery_release(disc);

    cspa_stream *stream = NULL;
    REQUIRE(
        cspa_stream_open(&t_def_out, CSPA_OVERFLOW_DROP_OLDEST, &stream) == CSPA_STATUS_OK && stream != NULL,
        "cspa_stream_open failed"
    );
    cspa_pcm_format_record st_fmt;
    memset(&st_fmt, 0, sizeof(st_fmt));
    st_fmt.struct_size = (uint32_t)sizeof(st_fmt);
    st_fmt.schema_revision = CSPA_EXPERIMENTAL_SCHEMA_REVISION;
    REQUIRE(cspa_stream_format(stream, &st_fmt) == CSPA_STATUS_OK && st_fmt.rate_hz > 0u, "stream_format failed");

    uint8_t st_buf[512 * 16];
    memset(st_buf, 0, sizeof(st_buf));
    cspa_managed_read_outcome_record st_out;
    memset(&st_out, 0, sizeof(st_out));
    st_out.struct_size = (uint32_t)sizeof(st_out);
    st_out.schema_revision = CSPA_EXPERIMENTAL_SCHEMA_REVISION;
    REQUIRE(
        cspa_stream_read_into(stream, st_buf, sizeof(st_buf), 256ull, 60ull, NULL, &st_out) == CSPA_STATUS_OK,
        "cspa_stream_read_into failed"
    );
    REQUIRE(st_out.block.retained_owner == NULL, "stream read_into must not allocate retained owner");
    REQUIRE(cspa_stream_stop(stream, 1u) == CSPA_STATUS_OK, "stream_stop failed");
    uint32_t st_cleaned = 0u;
    REQUIRE(
        cspa_stream_wait_cleanup(stream, 2000ull, &st_cleaned) == CSPA_STATUS_OK && st_cleaned == 1u,
        "stream_wait_cleanup failed"
    );
    cspa_stream_release(stream);

    cspa_capture_target_record mix_targets[2];
    mix_targets[0] = t_def_out;
    mix_targets[1] = (in_dev_count > 0u) ? t_mic : out_devs[0];

    cspa_mixer_config_record mix_cfg;
    REQUIRE(cspa_mixer_config_init(&mix_cfg) == CSPA_STATUS_OK, "mixer_config_init failed");
    mix_cfg.silence_on_idle = 1u;
    mix_cfg.clamp_f32 = 1u;

    cspa_output_converter_record conv_cfg;
    memset(&conv_cfg, 0, sizeof(conv_cfg));
    conv_cfg.struct_size = (uint32_t)sizeof(conv_cfg);
    conv_cfg.schema_revision = CSPA_EXPERIMENTAL_SCHEMA_REVISION;
    conv_cfg.rate_hz = 16000u;
    conv_cfg.channels = 2u;
    conv_cfg.sample_format = CSPA_SAMPLE_FORMAT_I16;
    conv_cfg.byte_order = CSPA_BYTE_ORDER_LITTLE;
    conv_cfg.dither = CSPA_DITHER_TPDF;

    cspa_capture_request_record mix_req;
    REQUIRE(cspa_capture_request_init(&mix_req, mix_targets, 2ull) == CSPA_STATUS_OK, "mix_req init failed");
    mix_req.combine_mode = CSPA_COMBINE_MODE_CSPA_MIXER;
    mix_req.mixer_config = &mix_cfg;
    mix_req.converter = &conv_cfg;

    cspa_manager *mgr = NULL;
    uint32_t open_st = cspa_manager_open(&mix_req, &mgr);
    if (open_st == CSPA_STATUS_PERMISSION_DENIED && in_dev_count > 0u) {
        /* A headless session may lack microphone consent. */
        mix_targets[1] = out_devs[0];
        open_st = cspa_manager_open(&mix_req, &mgr);
    }
    REQUIRE(open_st == CSPA_STATUS_OK && mgr != NULL, "cspa_manager_open failed");

    size_t input_count = 0u;
    REQUIRE(cspa_manager_input_count(mgr, &input_count) == CSPA_STATUS_OK && input_count == 2u, "input_count != 2");

    cspa_input_status_record in_status;
    memset(&in_status, 0, sizeof(in_status));
    in_status.struct_size = (uint32_t)sizeof(in_status);
    in_status.schema_revision = CSPA_EXPERIMENTAL_SCHEMA_REVISION;
    REQUIRE(cspa_manager_input_status(mgr, 0u, &in_status) == CSPA_STATUS_OK, "input_status(0) failed");
    REQUIRE(
        in_status.backend != CSPA_BACKEND_NONE
            && in_status.native_rate_hz > 0u
            && in_status.mixer_rate_hz >= in_status.native_rate_hz,
        "normalizer input_status fields invalid"
    );

    cspa_pcm_format_record mgr_fmt;
    memset(&mgr_fmt, 0, sizeof(mgr_fmt));
    mgr_fmt.struct_size = (uint32_t)sizeof(mgr_fmt);
    mgr_fmt.schema_revision = CSPA_EXPERIMENTAL_SCHEMA_REVISION;
    REQUIRE(
        cspa_manager_output_format(mgr, &mgr_fmt) == CSPA_STATUS_OK
            && mgr_fmt.rate_hz == 16000u
            && mgr_fmt.channels == 2u
            && mgr_fmt.sample_format == CSPA_SAMPLE_FORMAT_I16,
        "OutputConverter output_format mismatch"
    );

    cspa_managed_read_outcome_record outcome;
    memset(&outcome, 0, sizeof(outcome));
    outcome.struct_size = (uint32_t)sizeof(outcome);
    outcome.schema_revision = CSPA_EXPERIMENTAL_SCHEMA_REVISION;
    REQUIRE(cspa_manager_read(mgr, 160ull, 80ull, NULL, &outcome) == CSPA_STATUS_OK, "cspa_manager_read failed");
    REQUIRE(
        outcome.actual_frames > 0ull
            && outcome.has_mixed_metadata == 1u
            && outcome.block.bytes != NULL
            && outcome.block.retained_owner != NULL,
        "owned read outcome invalid"
    );
    uint8_t first_two_bytes[2];
    memcpy(first_two_bytes, outcome.block.bytes, 2u);

    uint64_t mem_v1 = 0ull, mem_v2 = 0ull;
    REQUIRE(
        cspa_manager_update_targets(mgr, &mix_targets[0], 1u, &mem_v1) == CSPA_STATUS_OK && mem_v1 >= 2ull,
        "update_targets 2->1 failed"
    );
    REQUIRE(
        cspa_manager_update_targets(mgr, mix_targets, 2u, &mem_v2) == CSPA_STATUS_OK && mem_v2 > mem_v1,
        "update_targets 1->2 failed"
    );

    uint8_t into_dst[512 * 4];
    memset(into_dst, 0, sizeof(into_dst));
    cspa_managed_read_outcome_record into_out;
    memset(&into_out, 0, sizeof(into_out));
    into_out.struct_size = (uint32_t)sizeof(into_out);
    into_out.schema_revision = CSPA_EXPERIMENTAL_SCHEMA_REVISION;
    REQUIRE(
        cspa_manager_read_into(mgr, into_dst, sizeof(into_dst), 160ull, 80ull, NULL, &into_out) == CSPA_STATUS_OK
            && into_out.actual_frames > 0ull
            && into_out.block.retained_owner == NULL,
        "post-hot-swap cspa_manager_read_into failed"
    );

    REQUIRE(cspa_manager_stop(mgr, 1u) == CSPA_STATUS_OK, "manager_stop failed");
    uint32_t cleaned = 0u;
    REQUIRE(
        cspa_manager_wait_cleanup(mgr, 2000ull, &cleaned) == CSPA_STATUS_OK && cleaned == 1u,
        "manager_wait_cleanup failed"
    );
    cspa_manager_release(mgr);

    /* A retained block outlives the manager. */
    REQUIRE(memcmp(outcome.block.bytes, first_two_bytes, 2u) == 0, "retained block corrupted after release");
    cspa_block_release(&outcome.block);

    /* Callback context remains valid until the worker joins. */
    cspa_capture_request_record cb_req;
    REQUIRE(cspa_capture_request_init(&cb_req, &t_def_out, 1ull) == CSPA_STATUS_OK, "cb_req init failed");
    cb_req.combine_mode = CSPA_COMBINE_MODE_CSPA_MIXER;
    cb_req.mixer_config = &mix_cfg;

    cspa_manager *cb_mgr = NULL;
    REQUIRE(cspa_manager_open(&cb_req, &cb_mgr) == CSPA_STATUS_OK && cb_mgr != NULL, "cb_mgr open failed");
    int cb_invocations_local = 0;
    g_context_released = 0;
    REQUIRE(
        cspa_manager_start_callback(cb_mgr, 128ull, 40ull, failing_callback, &cb_invocations_local, on_context_released)
            == CSPA_STATUS_OK,
        "start_callback failed"
    );
    size_t recorded_invocations = 0u;
    struct timespec ts_start, ts_now;
    (void)timespec_get(&ts_start, TIME_UTC);
    for (;;) {
        if (cspa_manager_callback_invocations(cb_mgr, &recorded_invocations) == CSPA_STATUS_OK
            && recorded_invocations >= 1u) {
            break;
        }
        (void)timespec_get(&ts_now, TIME_UTC);
        int64_t elapsed_ms = ((int64_t)(ts_now.tv_sec - ts_start.tv_sec) * 1000ll)
            + ((int64_t)(ts_now.tv_nsec - ts_start.tv_nsec) / 1000000ll);
        if (elapsed_ms >= 1500ll) {
            break;
        }
    }
    REQUIRE(recorded_invocations >= 1u, "callback worker did not fire within 1500 ms");
    REQUIRE(g_context_released == 0, "callback context must not be released before stop_callback");
    REQUIRE(cspa_manager_stop_callback(cb_mgr) == CSPA_STATUS_OK, "stop_callback failed");
    REQUIRE(g_context_released == 1, "callback context was not released after worker join");
    REQUIRE(
        cspa_manager_callback_invocations(cb_mgr, &recorded_invocations) == CSPA_STATUS_OK
            && recorded_invocations == 1u && cb_invocations_local == 1,
        "failing callback must be invoked exactly once"
    );
    uint32_t cb_cleaned = 0u;
    REQUIRE(
        cspa_manager_wait_cleanup(cb_mgr, 2000ull, &cb_cleaned) == CSPA_STATUS_OK && cb_cleaned == 1u,
        "cb_mgr wait_cleanup failed"
    );
    cspa_manager_release(cb_mgr);

    printf("C11_CONSUMER_OK\n");
    return 0;
}
