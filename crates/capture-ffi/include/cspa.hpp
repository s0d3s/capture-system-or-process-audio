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

