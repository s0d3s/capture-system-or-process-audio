#ifndef CSPA_HPP
#define CSPA_HPP

#include "cspa_experimental.h"
#include <cstddef>
#include <cstdint>
#include <cstring>
#include <string>
#include <utility>
#include <vector>

#if defined(__cpp_exceptions) || (defined(_HAS_EXCEPTIONS) && _HAS_EXCEPTIONS != 0)
#define CSPA_HAS_EXCEPTIONS 1
#include <stdexcept>
#else
#define CSPA_HAS_EXCEPTIONS 0
#endif

namespace cspa {

enum class Status : uint32_t {
    Ok = CSPA_STATUS_OK,
    InvalidArgument = CSPA_STATUS_INVALID_ARGUMENT,
    AbiMismatch = CSPA_STATUS_ABI_MISMATCH,
    UnsupportedScope = CSPA_STATUS_UNSUPPORTED_SCOPE,
    PreciseTimingUnqualified = CSPA_STATUS_PRECISE_TIMING_UNQUALIFIED,
    AlreadyPending = CSPA_STATUS_ALREADY_PENDING,
    NoMemory = CSPA_STATUS_NO_MEMORY,
    CallbackFailure = CSPA_STATUS_CALLBACK_FAILURE,
    FormatChanged = CSPA_STATUS_FORMAT_CHANGED,
    InternalError = CSPA_STATUS_INTERNAL_ERROR,
    PermissionDenied = CSPA_STATUS_PERMISSION_DENIED,
    DeviceUnavailable = CSPA_STATUS_DEVICE_UNAVAILABLE,
    BackendUnavailable = CSPA_STATUS_BACKEND_UNAVAILABLE,
    NoSuchProcess = CSPA_STATUS_NO_SUCH_PROCESS,
    PidReused = CSPA_STATUS_PID_REUSED,
    RerouteNotPermitted = CSPA_STATUS_REROUTE_NOT_PERMITTED,
    UnsupportedFormat = CSPA_STATUS_UNSUPPORTED_FORMAT,
    CleanupTimeout = CSPA_STATUS_CLEANUP_TIMEOUT,
    Stopped = CSPA_STATUS_STOPPED,
};

enum class PlatformOs : uint32_t {
    Windows = CSPA_PLATFORM_OS_WINDOWS,
    Linux = CSPA_PLATFORM_OS_LINUX,
    MacOs = CSPA_PLATFORM_OS_MACOS,
};

enum class TargetKind : uint32_t {
    Process = CSPA_TARGET_KIND_PROCESS,
    Application = CSPA_TARGET_KIND_APPLICATION,
    Device = CSPA_TARGET_KIND_DEVICE,
    DefaultOutput = CSPA_TARGET_KIND_DEFAULT_OUTPUT,
    InputDevice = CSPA_TARGET_KIND_INPUT_DEVICE,
    DefaultInput = CSPA_TARGET_KIND_DEFAULT_INPUT,
};

enum class ProcessScope : uint32_t {
    Tree = CSPA_SCOPE_TREE,
    Exact = CSPA_SCOPE_EXACT,
};

enum class OnRootExitMode : uint32_t {
    Stop = CSPA_ON_ROOT_EXIT_STOP,
    Wait = CSPA_ON_ROOT_EXIT_WAIT,
};

enum class DefaultDeviceMode : uint32_t {
    Follow = CSPA_DEFAULT_DEVICE_FOLLOW,
    Pin = CSPA_DEFAULT_DEVICE_PIN,
};

enum class BackendId : uint32_t {
    None = CSPA_BACKEND_NONE,
    Wasapi = CSPA_BACKEND_WASAPI,
    PipeWire = CSPA_BACKEND_PIPEWIRE,
    PulseAudio = CSPA_BACKEND_PULSEAUDIO,
    CoreAudio = CSPA_BACKEND_COREAUDIO,
    ScreenCaptureKit = CSPA_BACKEND_SCREENCAPTUREKIT,
};

enum class ProcessTreeStrategy : uint32_t {
    NativeKernelCapture = CSPA_TREE_STRATEGY_NATIVE_KERNEL_CAPTURE,
    NativeGraphMix = CSPA_TREE_STRATEGY_NATIVE_GRAPH_MIX,
    SoftwareStreamMix = CSPA_TREE_STRATEGY_SOFTWARE_STREAM_MIX,
};

enum class CombineMode : uint32_t {
    Auto = CSPA_COMBINE_MODE_AUTO,
    NativeOnly = CSPA_COMBINE_MODE_NATIVE_ONLY,
    CspaMixer = CSPA_COMBINE_MODE_CSPA_MIXER,
};

enum class CombineResolution : uint32_t {
    PassThrough = CSPA_COMBINE_RESOLUTION_PASS_THROUGH,
    SharedNativeGraphMix = CSPA_COMBINE_RESOLUTION_SHARED_NATIVE_GRAPH_MIX,
    CspaMixer = CSPA_COMBINE_RESOLUTION_CSPA_MIXER,
};

enum class InputConversionPolicy : uint32_t {
    AutoWhenMixing = CSPA_INPUT_CONVERSION_AUTO_WHEN_MIXING,
    StrictNoResample = CSPA_INPUT_CONVERSION_STRICT_NO_RESAMPLE,
};

enum class OnFormatChange : uint32_t {
    Adapt = CSPA_ON_FORMAT_CHANGE_ADAPT,
    Fail = CSPA_ON_FORMAT_CHANGE_FAIL,
};

enum class OnInputFailure : uint32_t {
    Degrade = CSPA_ON_INPUT_FAILURE_DEGRADE,
    Fail = CSPA_ON_INPUT_FAILURE_FAIL,
};

enum class SampleFormat : uint32_t {
    I16 = CSPA_SAMPLE_FORMAT_I16,
    I24 = CSPA_SAMPLE_FORMAT_I24,
    F32 = CSPA_SAMPLE_FORMAT_F32,
    I32 = CSPA_SAMPLE_FORMAT_I32,
};

enum class ByteOrder : uint32_t {
    Little = CSPA_BYTE_ORDER_LITTLE,
    Big = CSPA_BYTE_ORDER_BIG,
};

enum class DitherMode : uint32_t {
    Tpdf = CSPA_DITHER_TPDF,
    None = CSPA_DITHER_NONE,
};

enum class Overflow : uint32_t {
    DropOldest = CSPA_OVERFLOW_DROP_OLDEST,
    DropNewest = CSPA_OVERFLOW_DROP_NEWEST,
    Fail = CSPA_OVERFLOW_FAIL,
};

enum class DiscoveryDedupe : uint32_t {
    ByLogicalTarget = CSPA_DISCOVERY_DEDUPE_BY_LOGICAL_TARGET,
    PerBackend = CSPA_DISCOVERY_DEDUPE_PER_BACKEND,
};

enum class DiscoveryOrder : uint32_t {
    KindThenBackend = CSPA_DISCOVERY_ORDER_KIND_THEN_BACKEND,
    BackendThenKind = CSPA_DISCOVERY_ORDER_BACKEND_THEN_KIND,
};

enum class DiscoveryEventKind : uint32_t {
    Added = CSPA_DISCOVERY_EVENT_ADDED,
    Removed = CSPA_DISCOVERY_EVENT_REMOVED,
    StateChanged = CSPA_DISCOVERY_EVENT_STATE_CHANGED,
    DefaultDeviceChanged = CSPA_DISCOVERY_EVENT_DEFAULT_DEVICE_CHANGED,
    DefaultInputChanged = CSPA_DISCOVERY_EVENT_DEFAULT_INPUT_CHANGED,
    WatcherFailed = CSPA_DISCOVERY_EVENT_WATCHER_FAILED,
    VisibilityChanged = CSPA_DISCOVERY_EVENT_VISIBILITY_CHANGED,
    ResnapshotRequired = CSPA_DISCOVERY_EVENT_RESNAPSHOT_REQUIRED,
};

enum class OverlapSemantics : uint32_t {
    SingleTarget = CSPA_OVERLAP_SINGLE_TARGET,
    SoftwareSummation = CSPA_OVERLAP_SOFTWARE_SUMMATION,
    NativeSetUnion = CSPA_OVERLAP_NATIVE_SET_UNION,
};

enum class PartialReason : uint32_t {
    None = 0,
    Complete = CSPA_PARTIAL_REASON_COMPLETE,
    WouldBlock = CSPA_PARTIAL_REASON_WOULD_BLOCK,
    Boundary = CSPA_PARTIAL_REASON_BOUNDARY,
    Cancelled = CSPA_PARTIAL_REASON_CANCELLED,
    Timeout = CSPA_PARTIAL_REASON_TIMEOUT,
    Eof = CSPA_PARTIAL_REASON_EOF,
    Failure = CSPA_PARTIAL_REASON_FAILURE,
    Ready = CSPA_PARTIAL_REASON_READY,
};

enum class PermissionKind : uint32_t {
    Microphone = CSPA_PERMISSION_KIND_MICROPHONE,
    SystemAudio = CSPA_PERMISSION_KIND_SYSTEM_AUDIO,
    ScreenRecording = CSPA_PERMISSION_KIND_SCREEN_RECORDING,
};

enum class PermissionState : uint32_t {
    Granted = CSPA_PERMISSION_STATE_GRANTED,
    NotDetermined = CSPA_PERMISSION_STATE_NOT_DETERMINED,
    Denied = CSPA_PERMISSION_STATE_DENIED,
    Restricted = CSPA_PERMISSION_STATE_RESTRICTED,
    Unknown = CSPA_PERMISSION_STATE_UNKNOWN,
};

enum class PermissionPolicy : uint32_t {
    AutoRequest = CSPA_PERMISSION_POLICY_AUTO_REQUEST,
    CheckOnly = CSPA_PERMISSION_POLICY_CHECK_ONLY,
};

struct PermissionEntry {
    PermissionKind kind = PermissionKind::Microphone;
    PermissionState state = PermissionState::Unknown;
    bool required = false;
    bool can_prompt = false;
    bool requires_relaunch = false;

    bool is_granted() const noexcept { return state == PermissionState::Granted; }
};

struct PermissionReport {
    PlatformOs os = PlatformOs::Windows;
    bool is_granted = false;
    bool has_denial = false;
    bool requires_relaunch = false;
    PermissionEntry entries[CSPA_PERMISSION_KIND_COUNT]{};

    const PermissionEntry &microphone() const noexcept { return entries[0]; }
    const PermissionEntry &system_audio() const noexcept { return entries[1]; }
    const PermissionEntry &screen_recording() const noexcept { return entries[2]; }

    static PermissionReport from_raw(const cspa_permission_report_record &raw) noexcept {
        PermissionReport out{};
        out.os = static_cast<PlatformOs>(raw.platform_os);
        out.is_granted = raw.is_granted != 0;
        out.has_denial = raw.has_denial != 0;
        out.requires_relaunch = raw.requires_relaunch != 0;
        for (size_t i = 0; i < CSPA_PERMISSION_KIND_COUNT; ++i) {
            out.entries[i].kind = static_cast<PermissionKind>(raw.entries[i].kind);
            out.entries[i].state = static_cast<PermissionState>(raw.entries[i].state);
            out.entries[i].required = raw.entries[i].required != 0;
            out.entries[i].can_prompt = raw.entries[i].can_prompt != 0;
            out.entries[i].requires_relaunch = raw.entries[i].requires_relaunch != 0;
        }
        return out;
    }
};

template <typename T>
class Result {
public:
    static Result ok(T value) noexcept {
        return Result(Status::Ok, std::move(value));
    }

    static Result err(Status status) noexcept {
        return Result(status, T{});
    }

    bool is_ok() const noexcept { return status_ == Status::Ok; }
    Status status() const noexcept { return status_; }
    const T &value() const noexcept { return value_; }
    T &value() noexcept { return value_; }
    T take_value() noexcept { return std::move(value_); }

private:
    Result(Status status, T value) noexcept
        : status_(status), value_(std::move(value)) {}

    Status status_;
    T value_;
};

#if CSPA_HAS_EXCEPTIONS
class Error : public std::runtime_error {
public:
    explicit Error(Status status)
        : std::runtime_error("cspa error"), status_(status) {}

    Status status() const noexcept { return status_; }

private:
    Status status_;
};

template <typename T>
inline T unwrap_or_throw(Result<T> res) {
    if (!res.is_ok()) {
        throw Error(res.status());
    }
    return res.take_value();
}
#endif

struct ProcessTreeRange {
    ProcessTreeStrategy start_level = ProcessTreeStrategy::NativeKernelCapture;
    bool has_fallback_limit = true;
    ProcessTreeStrategy fallback_limit = ProcessTreeStrategy::SoftwareStreamMix;

    cspa_process_tree_range_record to_raw() const noexcept {
        cspa_process_tree_range_record raw{};
        raw.start_level = static_cast<uint32_t>(start_level);
        raw.fallback_limit = has_fallback_limit ? static_cast<uint32_t>(fallback_limit) : 0u;
        return raw;
    }
};

class Backends {
public:
    Backends() noexcept {
        (void)cspa_backends_init(&record_);
    }

    static uint32_t pack_list(const std::vector<BackendId> &list) noexcept {
        uint32_t packed = 0u;
        for (size_t i = 0; i < list.size() && i < 4; ++i) {
            packed |= (static_cast<uint32_t>(list[i]) & 0xffu) << (i * 8);
        }
        return packed;
    }

    static std::vector<BackendId> unpack_list(uint32_t packed) {
        std::vector<BackendId> out;
        while (packed != 0u) {
            out.push_back(static_cast<BackendId>(packed & 0xffu));
            packed >>= 8;
        }
        return out;
    }

    Backends &set_windows(const std::vector<BackendId> &list) noexcept {
        record_.windows_mask = pack_list(list);
        return *this;
    }

    Backends &set_linux(const std::vector<BackendId> &list) noexcept {
        record_.linux_mask = pack_list(list);
        return *this;
    }

    Backends &set_macos(const std::vector<BackendId> &list) noexcept {
        record_.macos_mask = pack_list(list);
        return *this;
    }

    const cspa_backends_record &raw() const noexcept { return record_; }

private:
    cspa_backends_record record_{};
};

class Target {
public:
    Target() noexcept {
        std::memset(&record_, 0, sizeof(record_));
        record_.struct_size = static_cast<uint32_t>(sizeof(record_));
        record_.schema_revision = CSPA_EXPERIMENTAL_SCHEMA_REVISION;
    }

    explicit Target(cspa_capture_target_record record) noexcept : record_(record) {}

    static Result<Target> process(
        uint32_t pid,
        uint64_t start_time = 0,
        ProcessScope scope = ProcessScope::Tree,
        OnRootExitMode on_root_exit = OnRootExitMode::Stop,
        uint64_t wait_timeout_ns = 0,
        float gain = 1.0f
    ) noexcept {
        Target t;
        uint32_t st = cspa_target_process(
            pid,
            start_time,
            static_cast<uint32_t>(scope),
            static_cast<uint32_t>(on_root_exit),
            wait_timeout_ns,
            gain,
            &t.record_
        );
        if (st != CSPA_STATUS_OK) {
            return Result<Target>::err(static_cast<Status>(st));
        }
        return Result<Target>::ok(t);
    }

    static Result<Target> application(
        uint32_t pid,
        uint64_t start_time = 0,
        const char *app_id = nullptr,
        const char *name = nullptr,
        const char *executable_path = nullptr,
        ProcessScope scope = ProcessScope::Tree,
        OnRootExitMode on_root_exit = OnRootExitMode::Stop,
        uint64_t wait_timeout_ns = 0,
        float gain = 1.0f
    ) noexcept {
        Target t;
        uint32_t st = cspa_target_application(
            pid,
            start_time,
            app_id,
            name,
            executable_path,
            static_cast<uint32_t>(scope),
            static_cast<uint32_t>(on_root_exit),
            wait_timeout_ns,
            gain,
            &t.record_
        );
        if (st != CSPA_STATUS_OK) {
            return Result<Target>::err(static_cast<Status>(st));
        }
        return Result<Target>::ok(t);
    }

    static Result<Target> device(const char *device_id, float gain = 1.0f) noexcept {
        Target t;
        uint32_t st = cspa_target_device(device_id, gain, &t.record_);
        if (st != CSPA_STATUS_OK) {
            return Result<Target>::err(static_cast<Status>(st));
        }
        return Result<Target>::ok(t);
    }

    static Result<Target> input_device(const char *device_id, float gain = 1.0f) noexcept {
        Target t;
        uint32_t st = cspa_target_input_device(device_id, gain, &t.record_);
        if (st != CSPA_STATUS_OK) {
            return Result<Target>::err(static_cast<Status>(st));
        }
        return Result<Target>::ok(t);
    }

    static Result<Target> default_output(
        DefaultDeviceMode mode = DefaultDeviceMode::Follow,
        float gain = 1.0f
    ) noexcept {
        Target t;
        uint32_t st = cspa_target_default_output(static_cast<uint32_t>(mode), gain, &t.record_);
        if (st != CSPA_STATUS_OK) {
            return Result<Target>::err(static_cast<Status>(st));
        }
        return Result<Target>::ok(t);
    }

    static Result<Target> microphone(
        DefaultDeviceMode mode = DefaultDeviceMode::Follow,
        float gain = 1.0f
    ) noexcept {
        Target t;
        uint32_t st = cspa_target_microphone(static_cast<uint32_t>(mode), gain, &t.record_);
        if (st != CSPA_STATUS_OK) {
            return Result<Target>::err(static_cast<Status>(st));
        }
        return Result<Target>::ok(t);
    }

    Result<Target> with_backend(BackendId backend) const noexcept {
        Target out;
        uint32_t st = cspa_target_with_backend(
            &record_,
            static_cast<uint32_t>(backend),
            &out.record_
        );
        if (st != CSPA_STATUS_OK) {
            return Result<Target>::err(static_cast<Status>(st));
        }
        return Result<Target>::ok(out);
    }

    Result<Target> with_gain(float gain) const noexcept {
        Target out;
        uint32_t st = cspa_target_with_gain(&record_, gain, &out.record_);
        if (st != CSPA_STATUS_OK) {
            return Result<Target>::err(static_cast<Status>(st));
        }
        return Result<Target>::ok(out);
    }

    Result<Target> lower_to_process() const noexcept {
        Target out;
        uint32_t st = cspa_target_lower_to_process(&record_, &out.record_);
        if (st != CSPA_STATUS_OK) {
            return Result<Target>::err(static_cast<Status>(st));
        }
        return Result<Target>::ok(out);
    }

#if CSPA_HAS_EXCEPTIONS
    static Target process_or_throw(
        uint32_t pid,
        uint64_t start_time = 0,
        ProcessScope scope = ProcessScope::Tree,
        OnRootExitMode on_root_exit = OnRootExitMode::Stop,
        uint64_t wait_timeout_ns = 0,
        float gain = 1.0f
    ) {
        return unwrap_or_throw(process(pid, start_time, scope, on_root_exit, wait_timeout_ns, gain));
    }

    static Target device_or_throw(const char *device_id, float gain = 1.0f) {
        return unwrap_or_throw(device(device_id, gain));
    }

    static Target input_device_or_throw(const char *device_id, float gain = 1.0f) {
        return unwrap_or_throw(input_device(device_id, gain));
    }

    static Target default_output_or_throw(
        DefaultDeviceMode mode = DefaultDeviceMode::Follow,
        float gain = 1.0f
    ) {
        return unwrap_or_throw(default_output(mode, gain));
    }

    static Target microphone_or_throw(
        DefaultDeviceMode mode = DefaultDeviceMode::Follow,
        float gain = 1.0f
    ) {
        return unwrap_or_throw(microphone(mode, gain));
    }

    Target with_backend_or_throw(BackendId backend) const {
        return unwrap_or_throw(with_backend(backend));
    }

    Target with_gain_or_throw(float gain) const {
        return unwrap_or_throw(with_gain(gain));
    }

    Target lower_to_process_or_throw() const {
        return unwrap_or_throw(lower_to_process());
    }
#endif

    TargetKind kind() const noexcept { return static_cast<TargetKind>(record_.kind); }
    BackendId backend() const noexcept { return static_cast<BackendId>(record_.backend); }
    std::vector<BackendId> available_backends() const {
        return Backends::unpack_list(record_.available_backends_mask);
    }
    uint32_t pid() const noexcept { return record_.pid; }
    const char *device_id() const noexcept { return record_.device_id; }
    const char *app_id() const noexcept { return record_.app_id; }
    const char *name() const noexcept { return record_.name; }
    const cspa_capture_target_record &raw() const noexcept { return record_; }

private:
    cspa_capture_target_record record_{};
};

class MixerConfig {
public:
    MixerConfig() noexcept {
        (void)cspa_mixer_config_init(&record_);
    }

    MixerConfig &set_clamp_f32(bool v) noexcept {
        record_.clamp_f32 = v ? 1u : 0u;
        return *this;
    }

    MixerConfig &set_silence_on_idle(bool v) noexcept {
        record_.silence_on_idle = v ? 1u : 0u;
        return *this;
    }

    MixerConfig &set_on_input_failure(OnInputFailure mode) noexcept {
        record_.on_input_failure = static_cast<uint32_t>(mode);
        return *this;
    }

    const cspa_mixer_config_record &raw() const noexcept { return record_; }

private:
    cspa_mixer_config_record record_{};
};

class OutputConverter {
public:
    OutputConverter(
        uint32_t rate_hz,
        uint32_t channels,
        SampleFormat sample_format = SampleFormat::F32,
        ByteOrder byte_order = ByteOrder::Little,
        DitherMode dither = DitherMode::Tpdf,
        uint64_t seed = 0
    ) noexcept {
        std::memset(&record_, 0, sizeof(record_));
        record_.struct_size = static_cast<uint32_t>(sizeof(record_));
        record_.schema_revision = CSPA_EXPERIMENTAL_SCHEMA_REVISION;
        record_.rate_hz = rate_hz;
        record_.channels = channels;
        record_.sample_format = static_cast<uint32_t>(sample_format);
        record_.byte_order = static_cast<uint32_t>(byte_order);
        record_.dither = static_cast<uint32_t>(dither);
        record_.seed = seed;
    }

    const cspa_output_converter_record &raw() const noexcept { return record_; }

private:
    cspa_output_converter_record record_{};
};

class DiscoveryQuery {
public:
    DiscoveryQuery() noexcept {
        (void)cspa_discovery_query_init(&record_);
    }

    DiscoveryQuery &set_backends(const Backends &backends) noexcept {
        record_.has_backends = 1u;
        record_.backends = backends.raw();
        return *this;
    }

    DiscoveryQuery &set_dedupe(DiscoveryDedupe dedupe) noexcept {
        record_.dedupe = static_cast<uint32_t>(dedupe);
        return *this;
    }

    DiscoveryQuery &set_order(DiscoveryOrder order) noexcept {
        record_.order = static_cast<uint32_t>(order);
        return *this;
    }

    DiscoveryQuery &set_active_only(bool active_only) noexcept {
        record_.active_only = active_only ? 1u : 0u;
        return *this;
    }

    DiscoveryQuery &set_include_non_audio(bool include_non_audio) noexcept {
        record_.include_non_audio = include_non_audio ? 1u : 0u;
        return *this;
    }

    DiscoveryQuery &set_kinds(
        bool apps,
        bool procs,
        bool output_devs,
        bool input_devs
    ) noexcept {
        record_.include_applications = apps ? 1u : 0u;
        record_.include_processes = procs ? 1u : 0u;
        record_.include_output_devices = output_devs ? 1u : 0u;
        record_.include_input_devices = input_devs ? 1u : 0u;
        return *this;
    }

    const cspa_discovery_query_record &raw() const noexcept { return record_; }

private:
    cspa_discovery_query_record record_{};
};

struct DiscoveryEvent {
    DiscoveryEventKind kind = DiscoveryEventKind::Added;
    uint32_t visibility = 0;
    uint64_t revision = 0;
    uint64_t minimum_revision = 0;
    std::string subject;
    Target target;
};

class DiscoverySubscription {
public:
    DiscoverySubscription() noexcept : handle_(nullptr) {}
    explicit DiscoverySubscription(cspa_discovery_subscription *handle) noexcept
        : handle_(handle) {}

    DiscoverySubscription(const DiscoverySubscription &) = delete;
    DiscoverySubscription &operator=(const DiscoverySubscription &) = delete;

    DiscoverySubscription(DiscoverySubscription &&other) noexcept : handle_(other.handle_) {
        other.handle_ = nullptr;
    }

    DiscoverySubscription &operator=(DiscoverySubscription &&other) noexcept {
        if (this != &other) {
            reset();
            handle_ = other.handle_;
            other.handle_ = nullptr;
        }
        return *this;
    }

    ~DiscoverySubscription() noexcept { reset(); }

    void reset() noexcept {
        if (handle_ != nullptr) {
            cspa_discovery_subscription_release(handle_);
            handle_ = nullptr;
        }
    }

    Result<bool> is_overflowed() const noexcept {
        if (handle_ == nullptr) {
            return Result<bool>::err(Status::InvalidArgument);
        }
        uint32_t ov = 0;
        uint32_t st = cspa_discovery_subscription_is_overflowed(handle_, &ov);
        if (st != CSPA_STATUS_OK) {
            return Result<bool>::err(static_cast<Status>(st));
        }
        return Result<bool>::ok(ov != 0);
    }

    Result<bool> poll(DiscoveryEvent &out_event) const {
        if (handle_ == nullptr) {
            return Result<bool>::err(Status::InvalidArgument);
        }
        cspa_discovery_event_record raw{};
        raw.struct_size = static_cast<uint32_t>(sizeof(raw));
        raw.schema_revision = CSPA_EXPERIMENTAL_SCHEMA_REVISION;
        uint32_t has_ev = 0;
        uint32_t st = cspa_discovery_subscription_poll(handle_, &raw, &has_ev);
        if (st != CSPA_STATUS_OK) {
            return Result<bool>::err(static_cast<Status>(st));
        }
        if (has_ev != 0) {
            out_event.kind = static_cast<DiscoveryEventKind>(raw.kind);
            out_event.visibility = raw.visibility;
            out_event.revision = raw.revision;
            out_event.minimum_revision = raw.minimum_revision;
            out_event.subject = raw.subject;
            out_event.target = Target(raw.target);
            return Result<bool>::ok(true);
        }
        return Result<bool>::ok(false);
    }

    Result<std::vector<Target>> resnapshot(uint64_t *out_revision = nullptr) const {
        if (handle_ == nullptr) {
            return Result<std::vector<Target>>::err(Status::InvalidArgument);
        }
        size_t count = 0;
        uint64_t rev = 0;
        uint32_t st = cspa_discovery_subscription_resnapshot(handle_, nullptr, 0, &rev, &count);
        if (st != CSPA_STATUS_OK) {
            return Result<std::vector<Target>>::err(static_cast<Status>(st));
        }
        std::vector<cspa_capture_target_record> buf(count + 8);
        size_t filled = 0;
        st = cspa_discovery_subscription_resnapshot(handle_, buf.data(), buf.size(), &rev, &filled);
        if (st != CSPA_STATUS_OK) {
            return Result<std::vector<Target>>::err(static_cast<Status>(st));
        }
        if (out_revision != nullptr) {
            *out_revision = rev;
        }
        size_t n = filled < buf.size() ? filled : buf.size();
        std::vector<Target> out;
        out.reserve(n);
        for (size_t i = 0; i < n; ++i) {
            out.emplace_back(buf[i]);
        }
        return Result<std::vector<Target>>::ok(std::move(out));
    }

    cspa_discovery_subscription *raw() const noexcept { return handle_; }

private:
    cspa_discovery_subscription *handle_;
};

class Discovery {
public:
    Discovery() noexcept : handle_(nullptr) {}
    explicit Discovery(cspa_discovery *handle) noexcept : handle_(handle) {}

    static Result<Discovery> create() noexcept {
        cspa_discovery *raw = nullptr;
        uint32_t st = cspa_discovery_create(&raw);
        if (st != CSPA_STATUS_OK) {
            return Result<Discovery>::err(static_cast<Status>(st));
        }
        return Result<Discovery>::ok(Discovery(raw));
    }

    static Result<Discovery> with_backends(const Backends &backends) noexcept {
        cspa_discovery *raw = nullptr;
        uint32_t st = cspa_discovery_create_with_backends(&backends.raw(), &raw);
        if (st != CSPA_STATUS_OK) {
            return Result<Discovery>::err(static_cast<Status>(st));
        }
        return Result<Discovery>::ok(Discovery(raw));
    }

#if CSPA_HAS_EXCEPTIONS
    static Discovery create_or_throw() { return unwrap_or_throw(create()); }
#endif

    Discovery(const Discovery &) = delete;
    Discovery &operator=(const Discovery &) = delete;

    Discovery(Discovery &&other) noexcept : handle_(other.handle_) {
        other.handle_ = nullptr;
    }

    Discovery &operator=(Discovery &&other) noexcept {
        if (this != &other) {
            reset();
            handle_ = other.handle_;
            other.handle_ = nullptr;
        }
        return *this;
    }

    ~Discovery() noexcept { reset(); }

    void reset() noexcept {
        if (handle_ != nullptr) {
            cspa_discovery_release(handle_);
            handle_ = nullptr;
        }
    }

    Result<std::vector<Target>> query(const DiscoveryQuery &q) const {
        if (handle_ == nullptr) {
            return Result<std::vector<Target>>::err(Status::InvalidArgument);
        }
        size_t count = 0;
        uint32_t st = cspa_discovery_query(handle_, &q.raw(), nullptr, 0, &count);
        if (st != CSPA_STATUS_OK) {
            return Result<std::vector<Target>>::err(static_cast<Status>(st));
        }
        std::vector<cspa_capture_target_record> buf(count + 8);
        size_t total = 0;
        st = cspa_discovery_query(handle_, &q.raw(), buf.data(), buf.size(), &total);
        if (st != CSPA_STATUS_OK) {
            return Result<std::vector<Target>>::err(static_cast<Status>(st));
        }
        size_t n = total < buf.size() ? total : buf.size();
        std::vector<Target> out;
        out.reserve(n);
        for (size_t i = 0; i < n; ++i) {
            out.emplace_back(buf[i]);
        }
        return Result<std::vector<Target>>::ok(std::move(out));
    }

    Result<std::vector<Target>> output_devices() const {
        if (handle_ == nullptr) {
            return Result<std::vector<Target>>::err(Status::InvalidArgument);
        }
        size_t count = 0;
        uint32_t st = cspa_discovery_output_devices(handle_, nullptr, 0, &count);
        if (st != CSPA_STATUS_OK) {
            return Result<std::vector<Target>>::err(static_cast<Status>(st));
        }
        std::vector<cspa_capture_target_record> buf(count + 8);
        size_t total = 0;
        st = cspa_discovery_output_devices(handle_, buf.data(), buf.size(), &total);
        if (st != CSPA_STATUS_OK) {
            return Result<std::vector<Target>>::err(static_cast<Status>(st));
        }
        size_t n = total < buf.size() ? total : buf.size();
        std::vector<Target> out;
        out.reserve(n);
        for (size_t i = 0; i < n; ++i) {
            out.emplace_back(buf[i]);
        }
        return Result<std::vector<Target>>::ok(std::move(out));
    }

    Result<std::vector<Target>> input_devices() const {
        if (handle_ == nullptr) {
            return Result<std::vector<Target>>::err(Status::InvalidArgument);
        }
        size_t count = 0;
        uint32_t st = cspa_discovery_input_devices(handle_, nullptr, 0, &count);
        if (st != CSPA_STATUS_OK) {
            return Result<std::vector<Target>>::err(static_cast<Status>(st));
        }
        std::vector<cspa_capture_target_record> buf(count + 8);
        size_t total = 0;
        st = cspa_discovery_input_devices(handle_, buf.data(), buf.size(), &total);
        if (st != CSPA_STATUS_OK) {
            return Result<std::vector<Target>>::err(static_cast<Status>(st));
        }
        size_t n = total < buf.size() ? total : buf.size();
        std::vector<Target> out;
        out.reserve(n);
        for (size_t i = 0; i < n; ++i) {
            out.emplace_back(buf[i]);
        }
        return Result<std::vector<Target>>::ok(std::move(out));
    }

    Result<DiscoverySubscription> subscribe_query(
        const DiscoveryQuery &q,
        size_t max_queue = 256
    ) const noexcept {
        if (handle_ == nullptr) {
            return Result<DiscoverySubscription>::err(Status::InvalidArgument);
        }
        cspa_discovery_subscription *sub = nullptr;
        uint32_t st = cspa_discovery_subscribe_query(handle_, &q.raw(), max_queue, &sub);
        if (st != CSPA_STATUS_OK) {
            return Result<DiscoverySubscription>::err(static_cast<Status>(st));
        }
        return Result<DiscoverySubscription>::ok(DiscoverySubscription(sub));
    }

    cspa_discovery *raw() const noexcept { return handle_; }

private:
    cspa_discovery *handle_;
};

struct PlanReport {
    PlatformOs os = PlatformOs::Windows;
    BackendId selected_backend = BackendId::None;
    CombineMode combine_mode = CombineMode::Auto;
    CombineResolution combine_resolution = CombineResolution::PassThrough;
    InputConversionPolicy input_conversion = InputConversionPolicy::AutoWhenMixing;
    bool uses_software_mixer = false;
    bool uses_output_converter = false;
    bool requires_discovery_subscription = false;
    OverlapSemantics overlap_semantics = OverlapSemantics::SingleTarget;
    bool allow_reroute = false;
    PermissionPolicy permission_policy = PermissionPolicy::AutoRequest;
    uint64_t duplicate_target_count = 0;
    uint64_t estimated_active_streams = 0;
    uint64_t estimated_pcm_bytes = 0;
    std::vector<cspa_target_plan_record> target_plans;
};

class CaptureRequest {
public:
    explicit CaptureRequest(std::vector<Target> targets)
        : targets_(std::move(targets)) {}

    CaptureRequest &set_tree_strategy(ProcessTreeRange range) noexcept {
        tree_strategy_ = range;
        return *this;
    }

    CaptureRequest &set_combine_mode(CombineMode mode) noexcept {
        combine_mode_ = mode;
        return *this;
    }

    CaptureRequest &set_input_conversion(InputConversionPolicy policy) noexcept {
        input_conversion_ = policy;
        return *this;
    }

    CaptureRequest &set_on_format_change(OnFormatChange policy) noexcept {
        on_format_change_ = policy;
        return *this;
    }

    CaptureRequest &set_backends(Backends backends) noexcept {
        has_backends_ = true;
        backends_ = backends;
        return *this;
    }

    CaptureRequest &set_output_converter(OutputConverter converter) noexcept {
        has_converter_ = true;
        converter_ = converter;
        return *this;
    }

    CaptureRequest &set_mixer_config(MixerConfig config) noexcept {
        has_mixer_ = true;
        mixer_config_ = config;
        return *this;
    }

    CaptureRequest &set_allow_reroute(bool allow) noexcept {
        allow_reroute_ = allow;
        return *this;
    }

    CaptureRequest &set_require_precise_timing(bool require) noexcept {
        require_precise_timing_ = require;
        return *this;
    }

    CaptureRequest &set_overflow(Overflow overflow) noexcept {
        overflow_ = overflow;
        return *this;
    }

    CaptureRequest &set_permission_policy(PermissionPolicy policy) noexcept {
        permission_policy_ = policy;
        return *this;
    }

    void build_raw(
        std::vector<cspa_capture_target_record> &raw_targets,
        cspa_capture_request_record &out_req
    ) const {
        raw_targets.clear();
        raw_targets.reserve(targets_.size());
        for (const auto &t : targets_) {
            raw_targets.push_back(t.raw());
        }
        (void)cspa_capture_request_init(&out_req, raw_targets.data(), raw_targets.size());
        out_req.tree_strategy = tree_strategy_.to_raw();
        out_req.combine_mode = static_cast<uint32_t>(combine_mode_);
        out_req.input_conversion = static_cast<uint32_t>(input_conversion_);
        out_req.on_format_change = static_cast<uint32_t>(on_format_change_);
        out_req.allow_reroute = allow_reroute_ ? 1u : 0u;
        out_req.require_precise_timing = require_precise_timing_ ? 1u : 0u;
        out_req.overflow = static_cast<uint32_t>(overflow_);
        out_req.permission_policy = static_cast<uint32_t>(permission_policy_);
        out_req.backends = has_backends_ ? &backends_.raw() : nullptr;
        out_req.converter = has_converter_ ? &converter_.raw() : nullptr;
        out_req.mixer_config = has_mixer_ ? &mixer_config_.raw() : nullptr;
        out_req.budgets = nullptr;
    }

    Result<PermissionReport> check_permissions() const {
        std::vector<cspa_capture_target_record> raw_targets;
        cspa_capture_request_record raw_req{};
        build_raw(raw_targets, raw_req);
        cspa_permission_report_record rep{};
        (void)cspa_permission_report_init(&rep);
        uint32_t st = cspa_request_check_permissions(&raw_req, &rep);
        if (st != CSPA_STATUS_OK) {
            return Result<PermissionReport>::err(static_cast<Status>(st));
        }
        return Result<PermissionReport>::ok(PermissionReport::from_raw(rep));
    }

    Result<PermissionReport> request_permissions(
        uint64_t timeout_ms = 30000,
        bool open_settings_if_denied = false
    ) const {
        std::vector<cspa_capture_target_record> raw_targets;
        cspa_capture_request_record raw_req{};
        build_raw(raw_targets, raw_req);
        cspa_permission_report_record rep{};
        (void)cspa_permission_report_init(&rep);
        uint32_t st = cspa_request_request_permissions(
            &raw_req,
            timeout_ms,
            open_settings_if_denied ? 1u : 0u,
            &rep
        );
        if (st != CSPA_STATUS_OK) {
            return Result<PermissionReport>::err(static_cast<Status>(st));
        }
        return Result<PermissionReport>::ok(PermissionReport::from_raw(rep));
    }

    Result<PlanReport> inspect() const {
        std::vector<cspa_capture_target_record> raw_targets;
        cspa_capture_request_record raw_req{};
        build_raw(raw_targets, raw_req);
        std::vector<cspa_target_plan_record> tplans(targets_.size());
        cspa_plan_report_record rep{};
        rep.struct_size = static_cast<uint32_t>(sizeof(rep));
        rep.schema_revision = CSPA_EXPERIMENTAL_SCHEMA_REVISION;
        rep.target_plans = tplans.empty() ? nullptr : tplans.data();
        rep.target_plans_capacity = tplans.size();
        uint32_t st = cspa_request_inspect(&raw_req, &rep);
        if (st != CSPA_STATUS_OK) {
            return Result<PlanReport>::err(static_cast<Status>(st));
        }
        return Result<PlanReport>::ok(decode_plan_report(rep, tplans));
    }

    Result<PlanReport> inspect_for_os(PlatformOs os) const {
        std::vector<cspa_capture_target_record> raw_targets;
        cspa_capture_request_record raw_req{};
        build_raw(raw_targets, raw_req);
        std::vector<cspa_target_plan_record> tplans(targets_.size());
        cspa_plan_report_record rep{};
        rep.struct_size = static_cast<uint32_t>(sizeof(rep));
        rep.schema_revision = CSPA_EXPERIMENTAL_SCHEMA_REVISION;
        rep.target_plans = tplans.empty() ? nullptr : tplans.data();
        rep.target_plans_capacity = tplans.size();
        uint32_t st = cspa_request_inspect_for_os(static_cast<uint32_t>(os), &raw_req, &rep);
        if (st != CSPA_STATUS_OK) {
            return Result<PlanReport>::err(static_cast<Status>(st));
        }
        return Result<PlanReport>::ok(decode_plan_report(rep, tplans));
    }

#if CSPA_HAS_EXCEPTIONS
    PlanReport inspect_or_throw() const { return unwrap_or_throw(inspect()); }
    PlanReport inspect_for_os_or_throw(PlatformOs os) const {
        return unwrap_or_throw(inspect_for_os(os));
    }
    PermissionReport check_permissions_or_throw() const {
        return unwrap_or_throw(check_permissions());
    }
    PermissionReport request_permissions_or_throw(
        uint64_t timeout_ms = 30000,
        bool open_settings_if_denied = false
    ) const {
        return unwrap_or_throw(request_permissions(timeout_ms, open_settings_if_denied));
    }
#endif

    static PlanReport decode_plan_report(
        const cspa_plan_report_record &rep,
        std::vector<cspa_target_plan_record> &tplans
    ) {
        PlanReport out{};
        out.os = static_cast<PlatformOs>(rep.platform_os);
        out.selected_backend = static_cast<BackendId>(rep.selected_backend);
        out.combine_mode = static_cast<CombineMode>(rep.combine_mode);
        out.combine_resolution = static_cast<CombineResolution>(rep.combine_resolution);
        out.input_conversion = static_cast<InputConversionPolicy>(rep.input_conversion);
        out.uses_software_mixer = rep.uses_software_mixer != 0;
        out.uses_output_converter = rep.uses_output_converter != 0;
        out.requires_discovery_subscription = rep.requires_discovery_subscription != 0;
        out.overlap_semantics = static_cast<OverlapSemantics>(rep.overlap_semantics);
        out.allow_reroute = rep.allow_reroute != 0;
        out.permission_policy = static_cast<PermissionPolicy>(rep.permission_policy);
        out.duplicate_target_count = rep.duplicate_target_count;
        out.estimated_active_streams = rep.estimated_active_streams;
        out.estimated_pcm_bytes = rep.estimated_pcm_bytes;
        if (rep.target_plan_count < tplans.size()) {
            tplans.resize(static_cast<size_t>(rep.target_plan_count));
        }
        out.target_plans = std::move(tplans);
        return out;
    }

private:
    std::vector<Target> targets_;
    ProcessTreeRange tree_strategy_{};
    CombineMode combine_mode_ = CombineMode::Auto;
    InputConversionPolicy input_conversion_ = InputConversionPolicy::AutoWhenMixing;
    OnFormatChange on_format_change_ = OnFormatChange::Adapt;
    bool has_backends_ = false;
    Backends backends_{};
    bool has_converter_ = false;
    OutputConverter converter_{48000, 2};
    bool has_mixer_ = false;
    MixerConfig mixer_config_{};
    bool allow_reroute_ = false;
    bool require_precise_timing_ = false;
    Overflow overflow_ = Overflow::DropOldest;
    PermissionPolicy permission_policy_ = PermissionPolicy::AutoRequest;
};

inline Result<PermissionReport> check_permissions(
    const std::vector<Target> &targets = {}
) {
    std::vector<cspa_capture_target_record> raw_targets;
    raw_targets.reserve(targets.size());
    for (const auto &t : targets) {
        raw_targets.push_back(t.raw());
    }
    cspa_permission_report_record rep{};
    (void)cspa_permission_report_init(&rep);
    uint32_t st = cspa_permissions_check(
        raw_targets.empty() ? nullptr : raw_targets.data(),
        raw_targets.size(),
        nullptr,
        &rep
    );
    if (st != CSPA_STATUS_OK) {
        return Result<PermissionReport>::err(static_cast<Status>(st));
    }
    return Result<PermissionReport>::ok(PermissionReport::from_raw(rep));
}

inline Result<PermissionReport> request_permissions(
    const std::vector<Target> &targets = {},
    uint64_t timeout_ms = 30000,
    bool open_settings_if_denied = false
) {
    std::vector<cspa_capture_target_record> raw_targets;
    raw_targets.reserve(targets.size());
    for (const auto &t : targets) {
        raw_targets.push_back(t.raw());
    }
    cspa_permission_report_record rep{};
    (void)cspa_permission_report_init(&rep);
    uint32_t st = cspa_permissions_request(
        raw_targets.empty() ? nullptr : raw_targets.data(),
        raw_targets.size(),
        nullptr,
        timeout_ms,
        open_settings_if_denied ? 1u : 0u,
        &rep
    );
    if (st != CSPA_STATUS_OK) {
        return Result<PermissionReport>::err(static_cast<Status>(st));
    }
    return Result<PermissionReport>::ok(PermissionReport::from_raw(rep));
}

class PcmBlock {
public:
    PcmBlock() noexcept { std::memset(&record_, 0, sizeof(record_)); }

    explicit PcmBlock(cspa_block_record record) noexcept : record_(record) {}

    PcmBlock(const PcmBlock &) = delete;
    PcmBlock &operator=(const PcmBlock &) = delete;

    PcmBlock(PcmBlock &&other) noexcept : record_(other.record_) {
        std::memset(&other.record_, 0, sizeof(other.record_));
    }

    PcmBlock &operator=(PcmBlock &&other) noexcept {
        if (this != &other) {
            release();
            record_ = other.record_;
            std::memset(&other.record_, 0, sizeof(other.record_));
        }
        return *this;
    }

    ~PcmBlock() noexcept { release(); }

    void release() noexcept {
        if (record_.retained_owner != nullptr) {
            cspa_block_release(&record_);
        }
    }

    const uint8_t *data() const noexcept { return record_.bytes; }
    uint64_t byte_len() const noexcept { return record_.byte_length; }
    uint64_t frames() const noexcept { return record_.header.frames; }
    uint64_t sequence() const noexcept { return record_.header.sequence; }
    uint64_t segment() const noexcept { return record_.header.segment; }
    bool valid() const noexcept { return record_.header.valid != 0 && record_.bytes != nullptr; }

private:
    cspa_block_record record_{};
};

struct ReadOutcome {
    PartialReason reason = PartialReason::None;
    uint32_t boundary_kind = 0;
    bool has_mixed_metadata = false;
    uint64_t session = 0;
    uint64_t segment = 0;
    uint64_t frames = 0;
    uint64_t copy_bytes = 0;
    uint64_t copy_destinations = 0;
    PcmBlock block;
    cspa_mixed_block_metadata_record mixed_metadata{};
};

class CancelToken {
public:
    CancelToken() noexcept : handle_(nullptr) {}
    explicit CancelToken(cspa_cancel *handle) noexcept : handle_(handle) {}

    static Result<CancelToken> create() noexcept {
        cspa_cancel *raw = nullptr;
        uint32_t st = cspa_cancel_create(&raw);
        if (st != CSPA_STATUS_OK) {
            return Result<CancelToken>::err(static_cast<Status>(st));
        }
        return Result<CancelToken>::ok(CancelToken(raw));
    }

    CancelToken(const CancelToken &) = delete;
    CancelToken &operator=(const CancelToken &) = delete;

    CancelToken(CancelToken &&other) noexcept : handle_(other.handle_) {
        other.handle_ = nullptr;
    }

    CancelToken &operator=(CancelToken &&other) noexcept {
        if (this != &other) {
            if (handle_ != nullptr) {
                cspa_cancel_release(handle_);
            }
            handle_ = other.handle_;
            other.handle_ = nullptr;
        }
        return *this;
    }

    ~CancelToken() noexcept {
        if (handle_ != nullptr) {
            cspa_cancel_release(handle_);
            handle_ = nullptr;
        }
    }

    void cancel() noexcept {
        if (handle_ != nullptr) {
            cspa_cancel_trigger(handle_);
        }
    }

    bool is_cancelled() const noexcept {
        return handle_ != nullptr && cspa_cancel_is_cancelled(handle_) != 0;
    }

    cspa_cancel *raw() const noexcept { return handle_; }

private:
    cspa_cancel *handle_;
};

class AudioStream {
public:
    AudioStream() noexcept : handle_(nullptr) {}
    explicit AudioStream(cspa_stream *handle) noexcept : handle_(handle) {}

    static Result<AudioStream> open(
        const Target &target,
        Overflow overflow = Overflow::DropOldest,
        PermissionPolicy permission_policy = PermissionPolicy::AutoRequest
    ) noexcept {
        cspa_stream *raw = nullptr;
        uint32_t st = cspa_stream_open_with_policy(
            &target.raw(),
            static_cast<uint32_t>(overflow),
            static_cast<uint32_t>(permission_policy),
            &raw
        );
        if (st != CSPA_STATUS_OK) {
            return Result<AudioStream>::err(static_cast<Status>(st));
        }
        return Result<AudioStream>::ok(AudioStream(raw));
    }

#if CSPA_HAS_EXCEPTIONS
    static AudioStream open_or_throw(
        const Target &target,
        Overflow overflow = Overflow::DropOldest,
        PermissionPolicy permission_policy = PermissionPolicy::AutoRequest
    ) {
        return unwrap_or_throw(open(target, overflow, permission_policy));
    }
#endif

    AudioStream(const AudioStream &) = delete;
    AudioStream &operator=(const AudioStream &) = delete;

    AudioStream(AudioStream &&other) noexcept : handle_(other.handle_) {
        other.handle_ = nullptr;
    }

    AudioStream &operator=(AudioStream &&other) noexcept {
        if (this != &other) {
            reset();
            handle_ = other.handle_;
            other.handle_ = nullptr;
        }
        return *this;
    }

    ~AudioStream() noexcept { reset(); }

    void reset() noexcept {
        if (handle_ != nullptr) {
            cspa_stream_release(handle_);
            handle_ = nullptr;
        }
    }

    Result<cspa_pcm_format_record> format() const noexcept {
        if (handle_ == nullptr) {
            return Result<cspa_pcm_format_record>::err(Status::InvalidArgument);
        }
        cspa_pcm_format_record fmt{};
        fmt.struct_size = static_cast<uint32_t>(sizeof(fmt));
        fmt.schema_revision = CSPA_EXPERIMENTAL_SCHEMA_REVISION;
        uint32_t st = cspa_stream_format(handle_, &fmt);
        if (st != CSPA_STATUS_OK) {
            return Result<cspa_pcm_format_record>::err(static_cast<Status>(st));
        }
        return Result<cspa_pcm_format_record>::ok(fmt);
    }

    Result<ReadOutcome> read_into(
        uint8_t *destination,
        size_t destination_bytes,
        uint64_t requested_frames,
        uint64_t timeout_ms = 100,
        CancelToken *cancel = nullptr
    ) noexcept {
        if (handle_ == nullptr) {
            return Result<ReadOutcome>::err(Status::InvalidArgument);
        }
        cspa_managed_read_outcome_record raw{};
        raw.struct_size = static_cast<uint32_t>(sizeof(raw));
        raw.schema_revision = CSPA_EXPERIMENTAL_SCHEMA_REVISION;
        uint32_t st = cspa_stream_read_into(
            handle_,
            destination,
            destination_bytes,
            requested_frames,
            timeout_ms,
            cancel != nullptr ? cancel->raw() : nullptr,
            &raw
        );
        if (st != CSPA_STATUS_OK) {
            return Result<ReadOutcome>::err(static_cast<Status>(st));
        }
        ReadOutcome out{};
        out.reason = static_cast<PartialReason>(raw.partial_reason);
        out.boundary_kind = raw.boundary_kind;
        out.has_mixed_metadata = raw.has_mixed_metadata != 0;
        out.session = raw.session;
        out.segment = raw.segment;
        out.frames = raw.actual_frames;
        out.copy_bytes = raw.copy_bytes;
        out.copy_destinations = raw.copy_destinations;
        out.block = PcmBlock(raw.block);
        out.mixed_metadata = raw.mixed_metadata;
        return Result<ReadOutcome>::ok(std::move(out));
    }

    Status stop(bool abort_mode = false) noexcept {
        if (handle_ == nullptr) {
            return Status::InvalidArgument;
        }
        return static_cast<Status>(cspa_stream_stop(handle_, abort_mode ? 1u : 0u));
    }

    bool wait_cleanup(uint64_t timeout_ms = 2000) noexcept {
        if (handle_ == nullptr) {
            return true;
        }
        uint32_t cleaned = 0;
        if (cspa_stream_wait_cleanup(handle_, timeout_ms, &cleaned) != CSPA_STATUS_OK) {
            return false;
        }
        return cleaned != 0;
    }

private:
    cspa_stream *handle_;
};

class ManagedStream {
public:
    ManagedStream() noexcept : handle_(nullptr) {}
    explicit ManagedStream(cspa_manager *handle) noexcept : handle_(handle) {}

    static Result<ManagedStream> open(const CaptureRequest &request) {
        std::vector<cspa_capture_target_record> raw_targets;
        cspa_capture_request_record raw_req{};
        request.build_raw(raw_targets, raw_req);
        cspa_manager *raw = nullptr;
        uint32_t st = cspa_manager_open(&raw_req, &raw);
        if (st != CSPA_STATUS_OK) {
            return Result<ManagedStream>::err(static_cast<Status>(st));
        }
        return Result<ManagedStream>::ok(ManagedStream(raw));
    }

#if CSPA_HAS_EXCEPTIONS
    static ManagedStream open_or_throw(const CaptureRequest &request) {
        return unwrap_or_throw(open(request));
    }
#endif

    ManagedStream(const ManagedStream &) = delete;
    ManagedStream &operator=(const ManagedStream &) = delete;

    ManagedStream(ManagedStream &&other) noexcept : handle_(other.handle_) {
        other.handle_ = nullptr;
    }

    ManagedStream &operator=(ManagedStream &&other) noexcept {
        if (this != &other) {
            reset();
            handle_ = other.handle_;
            other.handle_ = nullptr;
        }
        return *this;
    }

    ~ManagedStream() noexcept { reset(); }

    void reset() noexcept {
        if (handle_ != nullptr) {
            cspa_manager_release(handle_);
            handle_ = nullptr;
        }
    }

    Result<cspa_pcm_format_record> output_format() const noexcept {
        if (handle_ == nullptr) {
            return Result<cspa_pcm_format_record>::err(Status::InvalidArgument);
        }
        cspa_pcm_format_record fmt{};
        fmt.struct_size = static_cast<uint32_t>(sizeof(fmt));
        fmt.schema_revision = CSPA_EXPERIMENTAL_SCHEMA_REVISION;
        uint32_t st = cspa_manager_output_format(handle_, &fmt);
        if (st != CSPA_STATUS_OK) {
            return Result<cspa_pcm_format_record>::err(static_cast<Status>(st));
        }
        return Result<cspa_pcm_format_record>::ok(fmt);
    }

    Result<uint64_t> update_targets(const std::vector<Target> &new_targets) {
        if (handle_ == nullptr || new_targets.empty()) {
            return Result<uint64_t>::err(Status::InvalidArgument);
        }
        std::vector<cspa_capture_target_record> raw_targets;
        raw_targets.reserve(new_targets.size());
        for (const auto &t : new_targets) {
            raw_targets.push_back(t.raw());
        }
        uint64_t version = 0;
        uint32_t st = cspa_manager_update_targets(
            handle_,
            raw_targets.data(),
            raw_targets.size(),
            &version
        );
        if (st != CSPA_STATUS_OK) {
            return Result<uint64_t>::err(static_cast<Status>(st));
        }
        return Result<uint64_t>::ok(version);
    }

#if CSPA_HAS_EXCEPTIONS
    uint64_t update_targets_or_throw(const std::vector<Target> &new_targets) {
        return unwrap_or_throw(update_targets(new_targets));
    }
#endif

    Result<size_t> input_count() const noexcept {
        if (handle_ == nullptr) {
            return Result<size_t>::err(Status::InvalidArgument);
        }
        size_t n = 0;
        uint32_t st = cspa_manager_input_count(handle_, &n);
        if (st != CSPA_STATUS_OK) {
            return Result<size_t>::err(static_cast<Status>(st));
        }
        return Result<size_t>::ok(n);
    }

    Result<cspa_input_status_record> input_status(size_t index) const noexcept {
        if (handle_ == nullptr) {
            return Result<cspa_input_status_record>::err(Status::InvalidArgument);
        }
        cspa_input_status_record rec{};
        rec.struct_size = static_cast<uint32_t>(sizeof(rec));
        rec.schema_revision = CSPA_EXPERIMENTAL_SCHEMA_REVISION;
        uint32_t st = cspa_manager_input_status(handle_, index, &rec);
        if (st != CSPA_STATUS_OK) {
            return Result<cspa_input_status_record>::err(static_cast<Status>(st));
        }
        return Result<cspa_input_status_record>::ok(rec);
    }

    Result<ReadOutcome> read(
        uint64_t requested_frames,
        uint64_t timeout_ms = 200,
        CancelToken *cancel = nullptr
    ) noexcept {
        if (handle_ == nullptr) {
            return Result<ReadOutcome>::err(Status::InvalidArgument);
        }
        cspa_managed_read_outcome_record raw{};
        raw.struct_size = static_cast<uint32_t>(sizeof(raw));
        raw.schema_revision = CSPA_EXPERIMENTAL_SCHEMA_REVISION;
        uint32_t st = cspa_manager_read(
            handle_,
            requested_frames,
            timeout_ms,
            cancel != nullptr ? cancel->raw() : nullptr,
            &raw
        );
        if (st != CSPA_STATUS_OK) {
            return Result<ReadOutcome>::err(static_cast<Status>(st));
        }
        ReadOutcome out{};
        out.reason = static_cast<PartialReason>(raw.partial_reason);
        out.boundary_kind = raw.boundary_kind;
        out.has_mixed_metadata = raw.has_mixed_metadata != 0;
        out.session = raw.session;
        out.segment = raw.segment;
        out.frames = raw.actual_frames;
        out.copy_bytes = raw.copy_bytes;
        out.copy_destinations = raw.copy_destinations;
        out.block = PcmBlock(raw.block);
        out.mixed_metadata = raw.mixed_metadata;
        return Result<ReadOutcome>::ok(std::move(out));
    }

    Result<ReadOutcome> read_into(
        uint8_t *destination,
        size_t destination_bytes,
        uint64_t requested_frames,
        uint64_t timeout_ms = 200,
        CancelToken *cancel = nullptr
    ) noexcept {
        if (handle_ == nullptr) {
            return Result<ReadOutcome>::err(Status::InvalidArgument);
        }
        cspa_managed_read_outcome_record raw{};
        raw.struct_size = static_cast<uint32_t>(sizeof(raw));
        raw.schema_revision = CSPA_EXPERIMENTAL_SCHEMA_REVISION;
        uint32_t st = cspa_manager_read_into(
            handle_,
            destination,
            destination_bytes,
            requested_frames,
            timeout_ms,
            cancel != nullptr ? cancel->raw() : nullptr,
            &raw
        );
        if (st != CSPA_STATUS_OK) {
            return Result<ReadOutcome>::err(static_cast<Status>(st));
        }
        ReadOutcome out{};
        out.reason = static_cast<PartialReason>(raw.partial_reason);
        out.boundary_kind = raw.boundary_kind;
        out.has_mixed_metadata = raw.has_mixed_metadata != 0;
        out.session = raw.session;
        out.segment = raw.segment;
        out.frames = raw.actual_frames;
        out.copy_bytes = raw.copy_bytes;
        out.copy_destinations = raw.copy_destinations;
        out.block = PcmBlock(raw.block);
        out.mixed_metadata = raw.mixed_metadata;
        return Result<ReadOutcome>::ok(std::move(out));
    }

    Status stop(bool abort_mode = false) noexcept {
        if (handle_ == nullptr) {
            return Status::InvalidArgument;
        }
        return static_cast<Status>(cspa_manager_stop(handle_, abort_mode ? 1u : 0u));
    }

    bool wait_cleanup(uint64_t timeout_ms = 2000) noexcept {
        if (handle_ == nullptr) {
            return true;
        }
        uint32_t cleaned = 0;
        if (cspa_manager_wait_cleanup(handle_, timeout_ms, &cleaned) != CSPA_STATUS_OK) {
            return false;
        }
        return cleaned != 0;
    }

    cspa_manager *raw() const noexcept { return handle_; }

private:
    cspa_manager *handle_;
};

} // namespace cspa

#endif
