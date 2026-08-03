import ast
import asyncio
import enum
import inspect
import os
import pathlib
import struct
import sys
import sysconfig
import threading
import time
import types
import unittest
import warnings


EXPECTED_ENUM_SPECS = {
    "TargetKind": ["PROCESS", "APPLICATION", "DEVICE", "DEFAULT_OUTPUT", "INPUT_DEVICE", "DEFAULT_INPUT"],
    "BackendId": ["WASAPI", "PIPEWIRE", "PULSEAUDIO", "COREAUDIO", "SCREENCAPTUREKIT"],
    "PlatformOs": ["WINDOWS", "LINUX", "MACOS"],
    "ProcessScope": ["TREE", "EXACT"],
    "RootExitPolicy": ["STOP", "WAIT"],
    "DefaultDeviceMode": ["FOLLOW", "PIN"],
    "ProcessTreeStrategy": ["NATIVE_KERNEL_CAPTURE", "NATIVE_GRAPH_MIX", "SOFTWARE_STREAM_MIX"],
    "CombineMode": ["AUTO", "NATIVE_ONLY", "CSPA_MIXER"],
    "CombineResolutionKind": ["PASS_THROUGH", "SHARED_NATIVE_GRAPH_MIX", "CSPA_MIXER"],
    "TargetAcquisitionKind": ["DIRECT_ENDPOINT", "PROCESS_TREE"],
    "InputConversionPolicy": ["AUTO_WHEN_MIXING", "STRICT_NO_RESAMPLE"],
    "OverlapSemantics": ["SINGLE_TARGET", "SOFTWARE_SUMMATION", "NATIVE_SET_UNION"],
    "PlanErrorCode": [
        "INVALID_TARGET",
        "UNSUPPORTED_SCOPE",
        "REVERSED_STRATEGY_RANGE",
        "INVALID_BACKEND",
        "BACKEND_UNAVAILABLE",
        "PERMISSION_DENIED",
        "NO_SUCH_PROCESS",
        "PID_REUSED",
        "PRECISE_TIMING_UNQUALIFIED",
        "UNSUPPORTED_CAPTURE_PLAN",
        "CAPACITY",
    ],
    "DiscoveryDedupe": ["BY_LOGICAL_TARGET", "PER_BACKEND"],
    "DiscoveryOrder": ["KIND_THEN_BACKEND", "BACKEND_THEN_KIND"],
    "DiscoveryEventKind": [
        "ADDED",
        "REMOVED",
        "STATE_CHANGED",
        "DEFAULT_DEVICE_CHANGED",
        "DEFAULT_INPUT_CHANGED",
        "WATCHER_FAILED",
        "VISIBILITY_CHANGED",
        "RESNAPSHOT_REQUIRED",
    ],
    "Visibility": ["COMPLETE", "LIMITED"],
    "SampleType": ["I16", "I24", "I32", "F32"],
    "ByteOrder": ["LITTLE", "BIG"],
    "Channel": ["MONO", "FRONT_LEFT", "FRONT_RIGHT", "CENTER", "LOW_FREQUENCY", "REAR_LEFT", "REAR_RIGHT"],
    "DitherMode": ["TPDF", "NONE"],
    "OnFormatChange": ["ADAPT", "FAIL"],
    "OnInputFailure": ["DEGRADE", "FAIL"],
    "Overflow": ["DROP_OLDEST", "DROP_NEWEST", "FAIL"],
    "BoundaryKind": ["SESSION_CHANGED", "CONFIGURATION_CHANGED", "DEVICE_CHANGED", "FORMAT_CHANGED", "GAP"],
    "PartialReason": ["COMPLETE", "WOULD_BLOCK", "BOUNDARY", "CANCELLED", "TIMEOUT", "EOF", "FAILURE", "READY"],
    "FailureKind": [
        "QUEUE_OVERFLOW",
        "POOL_EXHAUSTION",
        "PACKET_TOO_LARGE",
        "INVALID_PACKET",
        "TICKET_EXHAUSTED",
        "CONTROL_EXHAUSTED",
        "BACKEND",
        "CALLBACK",
        "CALLBACK_PANIC",
    ],
    "ParticipationState": ["STARTING", "ACTIVE", "IDLE", "WAITING_RELAUNCH", "LATE", "EXITED", "FAILED"],
    "StopMode": ["DRAIN", "ABORT"],
    "TimingRequirement": ["RAW", "PRECISE"],
    "PermissionKind": ["MICROPHONE", "SYSTEM_AUDIO", "SCREEN_RECORDING"],
    "PermissionState": ["GRANTED", "NOT_DETERMINED", "DENIED", "RESTRICTED", "UNKNOWN"],
    "PermissionPolicy": ["AUTO_REQUEST", "CHECK_ONLY"],
}


def ensure_numpy_or_pep3118_shim():
    try:
        import numpy  # noqa: F401
        return
    except ImportError:
        pass

    class _Flags:
        def __init__(self, writeable):
            self.writeable = writeable

    class _Pep3118Array:
        def __init__(self, mv, shape, dtype_str, writeable, data_ptr):
            self._mv = mv
            self.shape = tuple(shape)
            self.dtype = dtype_str
            self.flags = _Flags(writeable)
            self.__array_interface__ = {"data": (data_ptr, not writeable)}

        def reshape(self, shape):
            return _Pep3118Array(self._mv, shape, self.dtype, self.flags.writeable, self.__array_interface__["data"][0])

        def copy(self):
            raw = bytearray(self._mv.tobytes())
            return _Pep3118Array(memoryview(raw), self.shape, self.dtype, True, 0)

        def __getitem__(self, idx):
            r, c = idx
            cols = self.shape[1]
            flat_idx = r * cols + c
            raw = self._mv.tobytes()
            if self.dtype == "float32":
                return struct.unpack_from("<f", raw, flat_idx * 4)[0]
            return struct.unpack_from("<i", raw, flat_idx * 4)[0]

    shim = types.ModuleType("numpy")

    def asarray(obj):
        mv = memoryview(obj)
        fmt_map = {"f": "float32", "<f": "float32", "i": "int32", "<i": "int32", "h": "int16", "<h": "int16"}
        dtype = fmt_map.get(mv.format, "uint8")
        ptr = getattr(obj, "data_ptr", 0)
        return _Pep3118Array(mv, mv.shape, dtype, not mv.readonly, ptr)

    def frombuffer(buf, dtype="<i4"):
        mv = memoryview(buf)
        dtype_name = "int32" if "i4" in dtype else "float32"
        return _Pep3118Array(mv, (len(mv) // 4,), dtype_name, False, 0)

    shim.asarray = asarray
    shim.frombuffer = frombuffer
    sys.modules["numpy"] = shim


_IMPORT_WARNINGS = []
_GIL_ENABLED_BEFORE_IMPORT = getattr(sys, "_is_gil_enabled", lambda: True)()


def load_extension():
    ensure_numpy_or_pep3118_shim()
    ext_dir = os.environ.get("CSPA_PYTHON_EXTENSION_DIR")
    if ext_dir and ext_dir not in sys.path:
        sys.path.insert(0, ext_dir)
    with warnings.catch_warnings(record=True) as caught:
        warnings.simplefilter("always")
        import capture_system_or_process_audio as cspa
        _IMPORT_WARNINGS.extend(caught)

    return cspa


class TestMinimalPythonSlice(unittest.TestCase):
    def setUp(self):
        self.cspa = load_extension()

    def test_enums_frozen_classes_and_adt_payloads(self):
        cspa = self.cspa

        for enum_name, expected_members in EXPECTED_ENUM_SPECS.items():
            self.assertTrue(hasattr(cspa, enum_name), f"missing enum {enum_name}")
            cls = getattr(cspa, enum_name)
            self.assertTrue(issubclass(cls, enum.Enum), f"{enum_name} must subclass enum.Enum")
            self.assertFalse(issubclass(cls, str), f"{enum_name} must be plain enum.Enum, not StrEnum")
            self.assertEqual([m.name for m in cls], expected_members)
            for member in cls:
                self.assertEqual(member.value, member.name.lower())

        class ForeignStrEnum(enum.StrEnum):
            TREE = "tree"

        pid = os.getpid()
        t_enum = cspa.process(
            pid,
            scope=cspa.ProcessScope.TREE,
            on_root_exit=cspa.RootExitPolicy.WAIT,
            wait_timeout_ms=2500,
            backend=cspa.BackendId.WASAPI,
            gain=0.9,
            start_time=999,
        )
        t_str = cspa.process(
            pid,
            scope="tree",
            on_root_exit="wait",
            wait_timeout_ms=2500,
            backend="wasapi",
            gain=0.9,
            start_time=999,
        )
        self.assertEqual(t_enum, t_str)
        self.assertIs(t_enum.kind, cspa.TargetKind.PROCESS)
        self.assertIs(t_enum.scope, cspa.ProcessScope.TREE)
        self.assertIs(t_enum.on_root_exit, cspa.RootExitPolicy.WAIT)
        self.assertIs(t_enum.backend, cspa.BackendId.WASAPI)
        self.assertEqual(t_enum.wait_timeout_ms, 2500)

        mixer_request = cspa.request(t_enum, combine_mode="cspa_mixer")
        self.assertIs(mixer_request.combine_mode, cspa.CombineMode.CSPA_MIXER)

        for bad_type in (cspa.CombineMode.AUTO, ForeignStrEnum.TREE, 123, ["tree"]):
            with self.assertRaises(TypeError):
                cspa.process(pid, scope=bad_type, start_time=999)
        with self.assertRaises(ValueError):
            cspa.process(pid, scope="invalid_scope", start_time=999)

        frozen_instances = [
            t_enum,
            cspa.Backends(),
            cspa.Format(),
            cspa.OutputConverter(),
            cspa.ProcessTreeRange(),
            cspa.MixerConfig(),
            cspa.AggregateManagerBudgets(),
            cspa.DiscoveryQuery(),
            cspa.Cancellation(),
            cspa.DiscoveryService(),
            cspa.request(t_enum),
            cspa.request(t_enum.with_backend(None)).inspect(),
        ]
        for inst in frozen_instances:
            with self.assertRaises(AttributeError):
                inst.mutated_field = 1

        with self.assertRaises(PermissionError) as perm_ctx:
            cspa._synthetic_permission_denied_demo()
        self.assertIsInstance(perm_ctx.exception, OSError)

        rep_all = cspa.check_permissions()
        self.assertEqual(len(rep_all.entries), 3)
        self.assertIs(rep_all.microphone.kind, cspa.PermissionKind.MICROPHONE)
        self.assertIs(rep_all.system_audio.kind, cspa.PermissionKind.SYSTEM_AUDIO)
        self.assertIs(rep_all.screen_recording.kind, cspa.PermissionKind.SCREEN_RECORDING)
        with self.assertRaises(AttributeError):
            rep_all.mutated_field = 1
        with self.assertRaises(AttributeError):
            rep_all.microphone.mutated_field = 1

        rep_kinds = cspa.check_permissions(kinds=[cspa.PermissionKind.MICROPHONE, "system_audio"])
        self.assertTrue(rep_kinds.microphone.required)
        self.assertTrue(rep_kinds.system_audio.required)
        self.assertFalse(rep_kinds.screen_recording.required)
        self.assertIs(rep_kinds.state_for("microphone"), rep_kinds.microphone.state)

        with self.assertRaises(ValueError):
            cspa.check_permissions(cspa.default_output(), kinds=["microphone"])

        req_default_perm = cspa.request(cspa.default_output())
        self.assertIs(req_default_perm.permission_policy, cspa.PermissionPolicy.AUTO_REQUEST)
        req_opt_out = cspa.request(cspa.default_output(), request_permissions=False)
        self.assertIs(req_opt_out.permission_policy, cspa.PermissionPolicy.CHECK_ONLY)
        self.assertIs(req_opt_out.inspect().permission_policy, cspa.PermissionPolicy.CHECK_ONLY)
        self.assertIs(
            req_default_perm.with_permission_policy("check_only").permission_policy,
            cspa.PermissionPolicy.CHECK_ONLY,
        )
        with self.assertRaises(ValueError):
            cspa.request(cspa.default_output(), permission_policy="auto_request", request_permissions=False)

        rep_sync = cspa.request_permissions(cspa.default_output(), timeout_ms=500)
        self.assertTrue(rep_sync.system_audio.required)
        self.assertFalse(rep_sync.microphone.required)

        async def _check_async_perm():
            r1 = await cspa.request_permissions_async(cspa.default_output(), timeout_ms=500)
            r2 = await req_opt_out.request_permissions_async(timeout_ms=500)
            self.assertEqual(r1.system_audio.state, r2.system_audio.state)

        asyncio.run(_check_async_perm())

        mgr_fail_in = cspa._synthetic_mixed_demo(inject_failed_code=503)
        handles_fail = mgr_fail_in.input_handles()
        self.assertIs(handles_fail[1].participation_state, cspa.ParticipationState.FAILED)
        self.assertEqual(handles_fail[1].failure_code, 503)
        self.assertIs(handles_fail[0].failure_code, None)
        self.assertIsInstance(handles_fail[0].native_format, cspa.Format)
        self.assertIsInstance(handles_fail[0].mixer_format, cspa.Format)
        self.assertTrue(handles_fail[0].is_identity_bypass)
        self.assertFalse(handles_fail[0].is_resampled)
        mgr_fail_in.stop(cspa.StopMode.ABORT)

        mgr_fail_out = cspa._synthetic_failure_demo(cspa.FailureKind.POOL_EXHAUSTION)
        out_fail = mgr_fail_out.read(64, 0, None)
        self.assertIs(out_fail.reason, cspa.PartialReason.FAILURE)
        self.assertIs(out_fail.failure, cspa.FailureKind.POOL_EXHAUSTION)

    def test_capture_targets_planning_and_backends_validation(self):
        cspa = self.cspa
        pid = os.getpid()
        t_proc = cspa.process(
            pid,
            scope="tree",
            on_root_exit="wait",
            wait_timeout_ms=2500,
            gain=1.0,
            start_time=999,
        )
        t_app = cspa.find_app("PythonTestApp", pid=pid, start_time=999, scope="tree")
        self.assertIs(t_app.kind, cspa.TargetKind.APPLICATION)
        lowered = t_app.lower_to_process()
        self.assertIs(lowered.kind, cspa.TargetKind.PROCESS)
        self.assertEqual(lowered.pid, pid)

        t_dev = cspa.device("endpoint-xyz")
        self.assertIs(t_dev.kind, cspa.TargetKind.DEVICE)
        self.assertEqual(t_dev.device_id, "endpoint-xyz")

        t_in_dev = cspa.input_device("usb-mic-0", gain=0.8)
        self.assertIs(t_in_dev.kind, cspa.TargetKind.INPUT_DEVICE)
        self.assertEqual(t_in_dev.device_id, "usb-mic-0")

        t_def_follow = cspa.default_output("follow")
        self.assertIs(t_def_follow.kind, cspa.TargetKind.DEFAULT_OUTPUT)
        self.assertIs(t_def_follow.default_device_mode, cspa.DefaultDeviceMode.FOLLOW)

        t_mic = cspa.microphone(cspa.DefaultDeviceMode.FOLLOW)
        self.assertIs(t_mic.kind, cspa.TargetKind.DEFAULT_INPUT)
        self.assertIs(t_mic.default_device_mode, cspa.DefaultDeviceMode.FOLLOW)

        backends = cspa.Backends(
            windows=[cspa.BackendId.WASAPI],
            linux=["pipewire", cspa.BackendId.PULSEAUDIO],
            macos=["coreaudio", "screencapturekit"],
        )
        self.assertEqual(backends.windows, [cspa.BackendId.WASAPI])
        self.assertEqual(backends.linux, [cspa.BackendId.PIPEWIRE, cspa.BackendId.PULSEAUDIO])
        self.assertEqual(backends.macos, [cspa.BackendId.COREAUDIO, cspa.BackendId.SCREENCAPTUREKIT])

        budgets = cspa.AggregateManagerBudgets(cleanup_timeout_ms=2000)
        conv = cspa.OutputConverter(rate=16000, channels=2, sample_format="i16", dither="tpdf")
        self.assertEqual(conv.rate, 16000)
        self.assertIs(conv.sample_format, cspa.SampleType.I16)
        self.assertIs(conv.dither, cspa.DitherMode.TPDF)

        req_single = cspa.request(t_proc, output_converter=conv, budgets=budgets)
        plan_single = req_single.inspect_for_os(cspa.PlatformOs.WINDOWS)
        self.assertTrue(plan_single.uses_output_converter)
        self.assertIs(plan_single.combine_resolution, cspa.CombineResolutionKind.CSPA_MIXER)
        self.assertIs(plan_single.overlap_semantics, cspa.OverlapSemantics.SINGLE_TARGET)

        req_dup = cspa.request([t_proc, t_proc, t_app], on_input_failure="degrade")
        plan_win = req_dup.inspect_for_os("windows")
        self.assertIs(plan_win.combine_resolution, cspa.CombineResolutionKind.CSPA_MIXER)
        self.assertTrue(plan_win.uses_software_mixer)
        self.assertIs(plan_win.overlap_semantics, cspa.OverlapSemantics.SOFTWARE_SUMMATION)
        self.assertEqual(len(plan_win.duplicate_target_ids), 1)

        t_app2 = cspa.find_app("PlayerApp", pid=pid + 1, start_time=999, scope="tree")
        plan_linux_auto = cspa.request([t_proc, t_app2]).inspect_for_os("linux")
        self.assertIs(plan_linux_auto.selected_backend, cspa.BackendId.PIPEWIRE)
        self.assertIs(plan_linux_auto.combine_resolution, cspa.CombineResolutionKind.SHARED_NATIVE_GRAPH_MIX)
        self.assertIs(plan_linux_auto.combine_backend, cspa.BackendId.PIPEWIRE)
        self.assertIs(plan_linux_auto.overlap_semantics, cspa.OverlapSemantics.NATIVE_SET_UNION)
        self.assertIs(plan_linux_auto.target_plans[0].acquisition, cspa.TargetAcquisitionKind.PROCESS_TREE)
        self.assertIs(plan_linux_auto.target_plans[0].process_tree_strategy, cspa.ProcessTreeStrategy.NATIVE_GRAPH_MIX)

        plan_linux_forced = cspa.request([t_proc, t_app2], combine_mode=cspa.CombineMode.CSPA_MIXER).inspect_for_os("linux")
        self.assertIs(plan_linux_forced.combine_resolution, cspa.CombineResolutionKind.CSPA_MIXER)
        self.assertIs(plan_linux_forced.overlap_semantics, cspa.OverlapSemantics.SOFTWARE_SUMMATION)

        t_pw_out = t_def_follow.with_backend("pipewire")
        t_pa_mic = t_in_dev.with_backend(cspa.BackendId.PULSEAUDIO)
        plan_linux_xb = cspa.request([t_pw_out, t_pa_mic]).inspect_for_os("linux")
        self.assertIs(plan_linux_xb.combine_resolution, cspa.CombineResolutionKind.CSPA_MIXER)
        self.assertIsNone(plan_linux_xb.selected_backend)
        self.assertIs(plan_linux_xb.target_plans[0].resolved_backend, cspa.BackendId.PIPEWIRE)
        self.assertIs(plan_linux_xb.target_plans[1].resolved_backend, cspa.BackendId.PULSEAUDIO)

        with self.assertRaises(RuntimeError):
            cspa.request([t_pw_out, t_pa_mic], combine_mode="native_only").inspect_for_os("linux")

        t_sck_app = cspa.find_app("SckApp", pid=4242, start_time=999, scope="exact", backend="screencapturekit")
        t_ca_mic = t_mic.with_backend("coreaudio")
        plan_mac_xb = cspa.request([t_sck_app, t_ca_mic]).inspect_for_os("macos")
        self.assertIs(plan_mac_xb.combine_resolution, cspa.CombineResolutionKind.CSPA_MIXER)
        self.assertIs(plan_mac_xb.target_plans[0].resolved_backend, cspa.BackendId.SCREENCAPTUREKIT)
        self.assertIs(plan_mac_xb.target_plans[1].resolved_backend, cspa.BackendId.COREAUDIO)

        with self.assertRaises(TypeError):
            cspa.capture(pid)
        with self.assertRaises(TypeError):
            cspa.capture("default_output")
        with self.assertRaises(ValueError):
            cspa.capture([])

    def test_discovery_snapshots_rollup_and_256_cursor_overflow(self):
        cspa = self.cspa
        svc = cspa.DiscoveryService()
        svc.seed_demo_inventory()

        self.assertEqual(svc.default_output_device_id, "endpoint-default")
        self.assertEqual(svc.default_input_device_id, "mic-default")
        in_devs = svc.input_devices()
        self.assertEqual(len(in_devs), 1)
        self.assertEqual(in_devs[0].device_id, "mic-default")
        self.assertFalse(any(d.device_id.endswith(".monitor") for d in in_devs))

        catalog_per_bk = svc.discover(dedupe="per_backend", order="backend_then_kind", include_non_audio=True)
        self.assertTrue(all(t.backend is not None for t in catalog_per_bk.targets))

        apps_default = svc.applications()
        self.assertEqual(len(apps_default), 2)
        app_pids = [a.pid for a in apps_default]
        self.assertIn(1000, app_pids)
        self.assertIn(2000, app_pids)

        apps_active = svc.applications(active_only=True)
        self.assertEqual(len(apps_active), 1)
        self.assertEqual(apps_active[0].pid, 1000)
        self.assertIs(apps_active[0].active_audio, True)

        apps_all = svc.applications(include_non_audio=True)
        self.assertEqual(len(apps_all), 3)

        found_pre_audio = svc.find_app("SilentDaemon")
        self.assertEqual(found_pre_audio.pid, 3000)

        with svc.subscribe(max_discovery_queue=256) as sub:
            self.assertEqual(svc.active_subscriber_count, 1)
            svc.publish_synthetic_events(256)
            self.assertFalse(sub.is_overflowed)
            self.assertEqual(sub.pending_len, 256)

            svc.publish_synthetic_events(1)
            self.assertTrue(sub.is_overflowed)
            events = sub.poll_events()
            self.assertIs(events[-1].kind, cspa.DiscoveryEventKind.RESNAPSHOT_REQUIRED)
            self.assertIsNotNone(events[-1].minimum_revision)

            snap = sub.resnapshot()
            self.assertFalse(sub.is_overflowed)
            self.assertGreaterEqual(snap.revision, 258)
        self.assertEqual(svc.active_subscriber_count, 0)

    def test_mixed_metadata_converter_and_status_only_input_handles(self):
        cspa = self.cspa
        mgr = cspa._synthetic_mixed_demo(sample_format=cspa.SampleType.F32, clamp_f32=False)
        handles = mgr.input_handles()
        self.assertEqual(len(handles), 2)
        for h in handles:
            self.assertFalse(hasattr(h, "read"))
            self.assertFalse(hasattr(h, "read_into"))
            self.assertIs(h.participation_state, cspa.ParticipationState.STARTING)

        out = mgr.read(480, 50, None)
        self.assertEqual(out.frames, 480)
        self.assertIs(out.reason, cspa.PartialReason.COMPLETE)
        meta = out.mixed_metadata
        self.assertIsNotNone(meta)
        self.assertEqual(meta.contributing_mask, 0b11)
        self.assertFalse(meta.synthesized_silence)
        self.assertEqual(meta.clipped_samples, 2)
        self.assertEqual(meta.nonfinite_samples, 2)
        self.assertEqual(meta.nonfinite_inputs, 0b10)
        self.assertEqual(meta.skew_estimate_ns, 2_000_000)
        self.assertEqual(meta.skew_uncertainty_ns, 250_000)

        handles_after = mgr.input_handles()
        self.assertEqual(handles_after[1].nonfinite_samples, 2)
        mgr.stop("abort")
        self.assertTrue(mgr.wait_cleanup())

    def test_segmented_delivery_buffer_lifetime_and_read_into(self):
        cspa = self.cspa
        mgr = cspa._synthetic_segmented_demo()

        out1 = mgr.read(4, 0, None)
        self.assertEqual(out1.session, 1)
        self.assertEqual(out1.frames, 2)
        self.assertIs(out1.reason, cspa.PartialReason.COMPLETE)
        blk1 = out1.blocks[0]
        ptr_before = blk1.data_ptr
        mv1 = memoryview(blk1)
        self.assertTrue(mv1.readonly)
        self.assertEqual(mv1.ndim, 2)
        self.assertEqual(mv1.shape, (2, 2))
        self.assertEqual(blk1.materialized_copies, 0)
        saved_bytes = bytes(mv1)

        out_bound = mgr.read(4, 0, None)
        self.assertIs(out_bound.reason, cspa.PartialReason.BOUNDARY)
        self.assertEqual(out_bound.frames, 0)
        self.assertIs(out_bound.boundary_kind, cspa.BoundaryKind.CONFIGURATION_CHANGED)

        with self.assertRaises(BufferError):
            mgr.read_into(b"0123456789abcdef", 2, 0, None)

        dst = bytearray(16)
        out2 = mgr.read_into(dst, 2, 0, None)
        self.assertEqual(out2.session, 2)
        self.assertEqual(out2.frames, 2)
        self.assertEqual(out2.copy_bytes, 16)

        mgr.stop(cspa.StopMode.ABORT)
        self.assertTrue(mgr.wait_cleanup())
        self.assertEqual(blk1.data_ptr, ptr_before)
        self.assertEqual(bytes(mv1), saved_bytes)
        mv1.release()

        self.assertEqual(cspa._measure_steady_state_read_into_allocations(1000), 0)

    def test_free_threaded_8_thread_barrier_concurrency_and_soundness(self):
        cspa = self.cspa
        gil_disabled_cfg = bool(sysconfig.get_config_var("Py_GIL_DISABLED"))
        is_gil_enabled_fn = getattr(sys, "_is_gil_enabled", None)
        if gil_disabled_cfg and is_gil_enabled_fn is not None and not _GIL_ENABLED_BEFORE_IMPORT:
            self.assertFalse(
                is_gil_enabled_fn(),
                "importing capture_system_or_process_audio re-enabled the GIL on a free-threaded runtime",
            )
            gil_warnings = [w for w in _IMPORT_WARNINGS if "GIL" in str(w.message).upper()]
            self.assertEqual(gil_warnings, [], f"unexpected GIL warnings on import: {gil_warnings}")

        shared_target = cspa.microphone(cspa.DefaultDeviceMode.FOLLOW)
        shared_svc = cspa.DiscoveryService()
        shared_svc.seed_demo_inventory()

        seed_mgr = cspa._synthetic_mixed_demo(sample_format="f32", clamp_f32=False)
        seed_out = seed_mgr.read(480, 50, None)
        shared_blk = seed_out.blocks[0]
        shared_ptr = shared_blk.data_ptr
        shared_bytes = bytes(memoryview(shared_blk))
        seed_mgr.stop(cspa.StopMode.ABORT)

        num_threads = 8
        barrier = threading.Barrier(num_threads)
        errors = []
        errors_lock = threading.Lock()

        def stress_worker(tid: int):
            try:
                barrier.wait(timeout=10.0)
                for it in range(24):
                    t_p = cspa.process(
                        1000 + tid,
                        scope=cspa.ProcessScope.TREE if (it % 2 == 0) else "tree",
                        on_root_exit=cspa.RootExitPolicy.WAIT if (it % 3 == 0) else "stop",
                        wait_timeout_ms=1500,
                        start_time=5000 + it,
                    )
                    plan = cspa.request([shared_target, t_p]).inspect_for_os("windows")
                    if plan.combine_resolution is not cspa.CombineResolutionKind.CSPA_MIXER:
                        raise AssertionError("unexpected combine_resolution")

                    cat = shared_svc.discover(include_non_audio=True)
                    if len(cat.input_devices) != 1:
                        raise AssertionError("unexpected input_devices count")

                    mv = memoryview(shared_blk)
                    if not mv.readonly or bytes(mv) != shared_bytes:
                        mv.release()
                        raise AssertionError("shared PcmBlock memoryview corrupted")
                    mv.release()

                    arr = shared_blk.to_numpy(copy=False)
                    if arr.shape != (480, 2) or arr.__array_interface__["data"][0] != shared_ptr:
                        raise AssertionError("shared PcmBlock to_numpy pointer mismatch")

                    local_mgr = cspa._synthetic_segmented_demo()
                    r1 = local_mgr.read(4, 0, None)
                    rb = local_mgr.read(4, 0, None)
                    dst = bytearray(16)
                    r2 = local_mgr.read_into(dst, 2, 0, None)
                    local_mgr.stop(cspa.StopMode.ABORT)
                    if r1.frames != 2 or rb.reason is not cspa.PartialReason.BOUNDARY or r2.copy_bytes != 16:
                        raise AssertionError("independent parallel ManagedStream read/read_into failed")
            except BaseException as exc:
                with errors_lock:
                    errors.append((tid, repr(exc)))

        threads = [threading.Thread(target=stress_worker, args=(i,)) for i in range(num_threads)]
        for t in threads:
            t.start()
        for t in threads:
            t.join(timeout=30.0)
            self.assertFalse(t.is_alive(), "8-thread stress worker deadlocked")
        self.assertEqual(errors, [], f"free-threaded 8-thread stress errors: {errors}")
        self.assertEqual(shared_blk.materialized_copies, 0)

        # Concurrent reads contend for a single reader claim.
        contended_mgr = cspa._synthetic_waiting_demo()
        claim_barrier = threading.Barrier(num_threads)
        outcomes = []
        outcomes_lock = threading.Lock()

        def claim_worker():
            claim_barrier.wait(timeout=10.0)
            try:
                res = contended_mgr.read(64, 40, None)
                with outcomes_lock:
                    outcomes.append(("ok", res.reason))
            except RuntimeError as exc:
                with outcomes_lock:
                    outcomes.append(("rejected", str(exc)))

        c_threads = [threading.Thread(target=claim_worker) for _ in range(num_threads)]
        for t in c_threads:
            t.start()
        for t in c_threads:
            t.join(timeout=10.0)
        contended_mgr.stop(cspa.StopMode.ABORT)

        oks = [o for o in outcomes if o[0] == "ok"]
        rejs = [o for o in outcomes if o[0] == "rejected"]
        self.assertGreaterEqual(len(oks), 1)
        self.assertGreaterEqual(len(rejs), 1)
        self.assertEqual(len(oks) + len(rejs), num_threads)

    def test_gil_release_and_per_read_cancellation_during_waiting(self):
        cspa = self.cspa
        mgr = cspa._synthetic_waiting_demo()
        cancel = cspa.Cancellation()
        progress = []

        def ticker():
            for i in range(5):
                time.sleep(0.01)
                progress.append(i)
            cancel.cancel()

        worker = threading.Thread(target=ticker)
        worker.start()
        out = mgr.read(64, 1000, cancel)
        worker.join()
        self.assertIs(out.reason, cspa.PartialReason.CANCELLED)
        self.assertEqual(out.frames, 0)
        self.assertEqual(len(progress), 5)

        with mgr:
            pass

    def test_asyncio_nonblocking_cancel_and_one_reader_arbitration(self):
        cspa = self.cspa

        async def run_async_checks():
            mgr = cspa._synthetic_waiting_demo()
            ticks = []

            async def loop_ticker():
                for _ in range(5):
                    await asyncio.sleep(0.005)
                    ticks.append(1)

            ticker_task = asyncio.create_task(loop_ticker())
            read_task = asyncio.create_task(mgr.read_async(64, 500, None))
            await asyncio.sleep(0.015)

            self.assertEqual(mgr.active_reader_count, 1)
            with self.assertRaises(RuntimeError):
                mgr.read(64, 0, None)
            with self.assertRaises(RuntimeError):
                mgr.read_into(bytearray(512), 64, 0, None)
            with self.assertRaises(RuntimeError):
                await mgr.read_async(64, 0, None)

            read_task.cancel()
            with self.assertRaises(asyncio.CancelledError):
                await read_task
            self.assertEqual(mgr.active_reader_count, 0)

            await ticker_task
            self.assertGreaterEqual(len(ticks), 3)

            async with mgr:
                pass

            mgr_iter = cspa._synthetic_retention_demo()
            total_frames = 0
            async for outcome in mgr_iter:
                total_frames += outcome.frames
            self.assertEqual(total_frames, 64)
            mgr_iter.stop(cspa.StopMode.ABORT)

        asyncio.run(run_async_checks())

    def test_context_exit_abort_vs_stop_drain_and_error_preservation(self):
        cspa = self.cspa

        mgr_drain = cspa._synthetic_retention_demo()
        mgr_drain.stop(cspa.StopMode.DRAIN)
        drained = 0
        for outcome in mgr_drain:
            drained += outcome.frames
        self.assertEqual(drained, 64)

        mgr_timeout_normal = cspa._synthetic_cleanup_timeout_demo()
        with self.assertRaises(RuntimeError):
            with mgr_timeout_normal:
                pass
        self.assertIsNotNone(mgr_timeout_normal.last_cleanup_error)

        mgr_timeout_exc = cspa._synthetic_cleanup_timeout_demo()
        with self.assertRaises(KeyError):
            with mgr_timeout_exc:
                raise KeyError("user_body_failure")
        self.assertIsNotNone(mgr_timeout_exc.last_cleanup_error)

        async def run_async_exit_check():
            mgr_async_exc = cspa._synthetic_cleanup_timeout_demo()
            with self.assertRaises(ValueError):
                async with mgr_async_exc:
                    raise ValueError("async_body_failure")
            self.assertIsNotNone(mgr_async_exc.last_cleanup_error)

        asyncio.run(run_async_exit_check())

    def test_callback_worker_and_single_shot_exception_containment(self):
        cspa = self.cspa
        mgr = cspa._synthetic_retention_demo()
        retained_blocks = []

        def failing_cb(outcome):
            if outcome.blocks:
                retained_blocks.append(outcome.blocks[0])
            raise RuntimeError("boom_in_callback")

        mgr.start_callback(failing_cb, frames=8, timeout_ms=20)
        deadline = time.time() + 2.0
        while mgr.last_callback_error is None and time.time() < deadline:
            time.sleep(0.005)
        mgr.stop_callback()

        self.assertEqual(mgr.callback_invocations, 1)
        self.assertIsNotNone(mgr.last_callback_error)
        self.assertIn("boom_in_callback", mgr.last_callback_error)
        self.assertEqual(len(retained_blocks), 1)
        mv = memoryview(retained_blocks[0])
        self.assertEqual(mv.shape, (8, 2))
        mv.release()
        mgr.stop(cspa.StopMode.ABORT)
        self.assertTrue(mgr.wait_cleanup())

    def test_optional_numpy_views_packed_i24_and_no_numpy_fallback(self):
        cspa = self.cspa

