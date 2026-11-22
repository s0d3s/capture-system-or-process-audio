#include "../include/cspa.hpp"
#include <cstdio>
#include <cstring>
#include <vector>

#define REQUIRE(cond, msg) \
    do { \
        if (!(cond)) { \
            std::fprintf(stderr, "CPP17_CONSUMER_FAIL: %s (line %d)\n", msg, __LINE__); \
            return 1; \
        } \
    } while (0)

int main(int argc, char **argv) {
    REQUIRE(argc == 1 || (argc == 2 && std::strcmp(argv[1], "--headless") == 0), "unknown argument");
    auto mic_res = cspa::Target::microphone(cspa::DefaultDeviceMode::Follow, 1.0f);
    REQUIRE(mic_res.is_ok(), "Target::microphone failed");
    cspa::Target mic = mic_res.take_value();
    REQUIRE(mic.kind() == cspa::TargetKind::DefaultInput, "microphone kind mismatch");

    auto in_dev_res = cspa::Target::input_device("studio-mic-1", 0.9f);
    REQUIRE(in_dev_res.is_ok(), "Target::input_device failed");
    cspa::Target in_dev = in_dev_res.take_value();
    REQUIRE(
        in_dev.kind() == cspa::TargetKind::InputDevice
            && std::strcmp(in_dev.device_id(), "studio-mic-1") == 0,
        "input_device fields mismatch"
    );

    auto app_res = cspa::Target::application(
        4242,
        1000,
        "com.example.daw",
        "ExampleDAW",
        "/Applications/ExampleDAW.app",
        cspa::ProcessScope::Exact
    );
    REQUIRE(app_res.is_ok(), "Target::application failed");
    cspa::Target app = app_res.take_value();
    auto lowered_res = app.lower_to_process();
    REQUIRE(
        lowered_res.is_ok() && lowered_res.value().kind() == cspa::TargetKind::Process,
        "lower_to_process failed"
    );

    auto sck_app_res = app.with_backend(cspa::BackendId::ScreenCaptureKit);
    auto ca_mic_res = mic.with_backend(cspa::BackendId::CoreAudio);
    REQUIRE(sck_app_res.is_ok() && ca_mic_res.is_ok(), "with_backend failed");

    cspa::CaptureRequest mac_xb_req({sck_app_res.take_value(), ca_mic_res.take_value()});
    mac_xb_req
        .set_tree_strategy(cspa::ProcessTreeRange{
            cspa::ProcessTreeStrategy::NativeKernelCapture,
            true,
            cspa::ProcessTreeStrategy::SoftwareStreamMix
        })
        .set_combine_mode(cspa::CombineMode::Auto)
        .set_input_conversion(cspa::InputConversionPolicy::AutoWhenMixing);

    auto mac_plan = mac_xb_req.inspect_for_os(cspa::PlatformOs::MacOs);
    REQUIRE(
        mac_plan.is_ok()
            && mac_plan.value().combine_resolution == cspa::CombineResolution::CspaMixer
            && mac_plan.value().uses_software_mixer
            && mac_plan.value().target_plans.size() == 2
            && mac_plan.value().target_plans[0].resolved_backend == CSPA_BACKEND_SCREENCAPTUREKIT
            && mac_plan.value().target_plans[1].resolved_backend == CSPA_BACKEND_COREAUDIO,
        "macOS SCK+CoreAudio software-mix plan mismatch"
    );

    mac_xb_req.set_combine_mode(cspa::CombineMode::NativeOnly);
    auto mac_native_only = mac_xb_req.inspect_for_os(cspa::PlatformOs::MacOs);
    REQUIRE(
        !mac_native_only.is_ok() && mac_native_only.status() == cspa::Status::UnsupportedScope,
        "CombineMode::NativeOnly must reject cross-backend session"
    );

#if CSPA_HAS_EXCEPTIONS
    bool caught_expected = false;
    try {
        (void)cspa::Target::device_or_throw("");
    } catch (const cspa::Error &err) {
        caught_expected = (err.status() == cspa::Status::InvalidArgument);
    }
    REQUIRE(caught_expected, "device_or_throw did not throw cspa::Error on empty device_id");
    std::printf("CPP17_CONSUMER_EXCEPTIONS_OK\n");
#else
    std::printf("CPP17_CONSUMER_NOEXCEPT_OK\n");
#endif

    if (argc == 2) {
        std::printf("CPP17_CONSUMER_HEADLESS_OK\n");
        return 0;
    }

    auto disc_res = cspa::Discovery::create();
    REQUIRE(disc_res.is_ok(), "Discovery::create failed");
    cspa::Discovery disc = disc_res.take_value();

    auto out_devs_res = disc.output_devices();
    REQUIRE(out_devs_res.is_ok() && !out_devs_res.value().empty(), "output_devices empty");
    auto in_devs_res = disc.input_devices();
    REQUIRE(in_devs_res.is_ok(), "input_devices failed");
    for (const auto &d : in_devs_res.value()) {
        REQUIRE(d.kind() == cspa::TargetKind::InputDevice, "input_devices kind mismatch");
        REQUIRE(!d.available_backends().empty(), "available_backends empty on input_device");
    }

    cspa::DiscoveryQuery q_logical;
    q_logical.set_dedupe(cspa::DiscoveryDedupe::ByLogicalTarget)
        .set_order(cspa::DiscoveryOrder::KindThenBackend);
    auto log_res = disc.query(q_logical);
    REQUIRE(log_res.is_ok() && !log_res.value().empty(), "ByLogicalTarget query failed");

    cspa::DiscoveryQuery q_pb;
    q_pb.set_dedupe(cspa::DiscoveryDedupe::PerBackend)
        .set_order(cspa::DiscoveryOrder::BackendThenKind);
    auto pb_res = disc.query(q_pb);
    REQUIRE(
        pb_res.is_ok() && pb_res.value().size() >= log_res.value().size(),
        "PerBackend query failed"
    );
    for (const auto &t : pb_res.value()) {
        REQUIRE(t.backend() != cspa::BackendId::None, "PerBackend must pin backend");
    }

    auto sub_res = disc.subscribe_query(q_logical, 256);
    REQUIRE(sub_res.is_ok(), "subscribe_query failed");
    cspa::DiscoverySubscription sub = sub_res.take_value();
    uint64_t rev = 0;
    REQUIRE(
        cspa_test_discovery_publish_synthetic(disc.raw(), 257, &rev) == CSPA_STATUS_OK,
        "publish_synthetic failed"
    );
    auto ov_res = sub.is_overflowed();
    REQUIRE(ov_res.is_ok() && ov_res.value(), "expected subscription overflow");
    auto snap_res = sub.resnapshot(&rev);
    REQUIRE(snap_res.is_ok() && !sub.is_overflowed().value(), "resnapshot failed to clear overflow");

    cspa::Target pin_out = cspa::Target::default_output(cspa::DefaultDeviceMode::Pin).take_value();
    auto perm_all = cspa::check_permissions();
    REQUIRE(perm_all.is_ok(), "cspa::check_permissions() failed");
    auto perm_out = cspa::check_permissions({pin_out});
    REQUIRE(
        perm_out.is_ok()
            && perm_out.value().system_audio().required
            && !perm_out.value().microphone().required,
        "cspa::check_permissions({pin_out}) required flags mismatch"
    );
    {
        auto st_res = cspa::AudioStream::open(pin_out, cspa::Overflow::DropOldest, cspa::PermissionPolicy::CheckOnly);
        REQUIRE(st_res.is_ok(), "AudioStream::open failed");
        cspa::AudioStream stream = st_res.take_value();
        auto fmt_res = stream.format();
        REQUIRE(fmt_res.is_ok() && fmt_res.value().rate_hz > 0, "AudioStream::format failed");
        std::vector<uint8_t> buf(256 * 16, 0);
        auto rd = stream.read_into(buf.data(), buf.size(), 128, 60);
        REQUIRE(rd.is_ok(), "AudioStream::read_into failed");
        REQUIRE(stream.stop(true) == cspa::Status::Ok && stream.wait_cleanup(), "AudioStream cleanup failed");
    }

    cspa::Target second_target = !in_devs_res.value().empty()
        ? mic
        : out_devs_res.value()[0];

    cspa::MixerConfig mix_cfg;
    mix_cfg.set_silence_on_idle(true).set_clamp_f32(true);
    cspa::OutputConverter conv_cfg(48000, 2, cspa::SampleFormat::F32);

    cspa::CaptureRequest req({pin_out, second_target});
    req.set_combine_mode(cspa::CombineMode::CspaMixer)
        .set_mixer_config(mix_cfg)
        .set_output_converter(conv_cfg);

    auto mgr_res = cspa::ManagedStream::open(req);
    if (!mgr_res.is_ok()
        && mgr_res.status() == cspa::Status::PermissionDenied
        && !in_devs_res.value().empty()) {
        second_target = out_devs_res.value()[0];
        cspa::CaptureRequest fallback_req({pin_out, second_target});
        fallback_req.set_combine_mode(cspa::CombineMode::CspaMixer)
            .set_mixer_config(mix_cfg)
            .set_output_converter(conv_cfg);
        mgr_res = cspa::ManagedStream::open(fallback_req);
    }
    REQUIRE(mgr_res.is_ok(), "ManagedStream::open failed");
    cspa::ManagedStream mgr = mgr_res.take_value();

    auto cnt_res = mgr.input_count();
    REQUIRE(cnt_res.is_ok() && cnt_res.value() == 2, "input_count mismatch");
    auto st0 = mgr.input_status(0);
    REQUIRE(
        st0.is_ok() && st0.value().native_rate_hz > 0 && st0.value().mixer_rate_hz == 48000,
        "input_status(0) normalizer metadata mismatch"
    );

    auto r_owned = mgr.read(240, 80);
    REQUIRE(r_owned.is_ok() && r_owned.value().frames > 0 && r_owned.value().has_mixed_metadata, "owned read failed");
    cspa::PcmBlock retained = std::move(r_owned.value().block);
    const uint8_t *ptr_before = retained.data();
    REQUIRE(ptr_before != nullptr && retained.byte_len() > 0, "retained block invalid");
    float s0_before = 0.0f;
    std::memcpy(&s0_before, ptr_before, sizeof(float));

    auto v1 = mgr.update_targets({pin_out});
    REQUIRE(v1.is_ok() && v1.value() >= 2, "update_targets 2->1 failed");
    auto v2 = mgr.update_targets({pin_out, second_target});
    REQUIRE(v2.is_ok() && v2.value() > v1.value(), "update_targets 1->2 failed");

    std::vector<uint8_t> into_buf(512 * 8, 0);
    auto r_into = mgr.read_into(into_buf.data(), into_buf.size(), 240, 80);
    REQUIRE(r_into.is_ok() && r_into.value().frames > 0, "read_into after hot-swap failed");

    REQUIRE(mgr.stop(true) == cspa::Status::Ok && mgr.wait_cleanup(), "ManagedStream cleanup failed");
    mgr.reset();

    float s0_after = 0.0f;
    std::memcpy(&s0_after, retained.data(), sizeof(float));
    REQUIRE(retained.data() == ptr_before && s0_after == s0_before, "retained block invalid after stream reset");
    retained.release();

    return 0;
}
