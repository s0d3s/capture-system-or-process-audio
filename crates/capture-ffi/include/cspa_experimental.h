#ifndef CSPA_EXPERIMENTAL_H
#define CSPA_EXPERIMENTAL_H

#include <stddef.h>
#include <stdint.h>

#define CSPA_EXPERIMENTAL_ABI_MAJOR 0u
#define CSPA_EXPERIMENTAL_SCHEMA_REVISION 1u

#define CSPA_DEVICE_ID_MAX_BYTES 256u
#define CSPA_APP_ID_MAX_BYTES 128u
#define CSPA_NAME_MAX_BYTES 128u
#define CSPA_EXECUTABLE_PATH_MAX_BYTES 256u
#define CSPA_EVENT_SUBJECT_MAX_BYTES 256u

/* Status codes */
#define CSPA_STATUS_OK 0u
#define CSPA_STATUS_INVALID_ARGUMENT 1u
#define CSPA_STATUS_ABI_MISMATCH 2u
#define CSPA_STATUS_UNSUPPORTED_SCOPE 3u
#define CSPA_STATUS_PRECISE_TIMING_UNQUALIFIED 4u
#define CSPA_STATUS_ALREADY_PENDING 5u
#define CSPA_STATUS_NO_MEMORY 6u
#define CSPA_STATUS_CALLBACK_FAILURE 7u
#define CSPA_STATUS_FORMAT_CHANGED 8u
#define CSPA_STATUS_INTERNAL_ERROR 9u
#define CSPA_STATUS_PERMISSION_DENIED 10u
#define CSPA_STATUS_DEVICE_UNAVAILABLE 11u
#define CSPA_STATUS_BACKEND_UNAVAILABLE 12u
#define CSPA_STATUS_NO_SUCH_PROCESS 13u
#define CSPA_STATUS_PID_REUSED 14u
#define CSPA_STATUS_REROUTE_NOT_PERMITTED 15u
#define CSPA_STATUS_UNSUPPORTED_FORMAT 16u
#define CSPA_STATUS_CLEANUP_TIMEOUT 17u
#define CSPA_STATUS_STOPPED 18u

/* Platform OS */
#define CSPA_PLATFORM_OS_WINDOWS 1u
#define CSPA_PLATFORM_OS_LINUX 2u
#define CSPA_PLATFORM_OS_MACOS 3u

/* CaptureTarget kind */
#define CSPA_TARGET_KIND_PROCESS 1u
#define CSPA_TARGET_KIND_APPLICATION 2u
#define CSPA_TARGET_KIND_DEVICE 3u
#define CSPA_TARGET_KIND_DEFAULT_OUTPUT 4u
#define CSPA_TARGET_KIND_INPUT_DEVICE 5u
#define CSPA_TARGET_KIND_DEFAULT_INPUT 6u

/* Process scope */
#define CSPA_SCOPE_TREE 1u
#define CSPA_SCOPE_EXACT 2u

/* OnRootExit */
#define CSPA_ON_ROOT_EXIT_STOP 1u
#define CSPA_ON_ROOT_EXIT_WAIT 2u

/* DefaultDeviceMode */
#define CSPA_DEFAULT_DEVICE_FOLLOW 1u
#define CSPA_DEFAULT_DEVICE_PIN 2u

/* BackendId */
#define CSPA_BACKEND_NONE 0u
#define CSPA_BACKEND_WASAPI 1u
#define CSPA_BACKEND_PIPEWIRE 2u
#define CSPA_BACKEND_PULSEAUDIO 3u
#define CSPA_BACKEND_COREAUDIO 4u
#define CSPA_BACKEND_SCREENCAPTUREKIT 5u

/* ProcessTreeStrategy (Process & Application targets only) */
#define CSPA_TREE_STRATEGY_NATIVE_KERNEL_CAPTURE 1u
#define CSPA_TREE_STRATEGY_NATIVE_GRAPH_MIX 2u
#define CSPA_TREE_STRATEGY_SOFTWARE_STREAM_MIX 3u

/* CombineMode */
#define CSPA_COMBINE_MODE_AUTO 1u
#define CSPA_COMBINE_MODE_NATIVE_ONLY 2u
#define CSPA_COMBINE_MODE_CSPA_MIXER 3u

/* CombineResolution */
#define CSPA_COMBINE_RESOLUTION_PASS_THROUGH 1u
#define CSPA_COMBINE_RESOLUTION_SHARED_NATIVE_GRAPH_MIX 2u
#define CSPA_COMBINE_RESOLUTION_CSPA_MIXER 3u

/* InputConversionPolicy */
#define CSPA_INPUT_CONVERSION_AUTO_WHEN_MIXING 1u
#define CSPA_INPUT_CONVERSION_STRICT_NO_RESAMPLE 2u

/* OnFormatChange */
#define CSPA_ON_FORMAT_CHANGE_ADAPT 1u
#define CSPA_ON_FORMAT_CHANGE_FAIL 2u

/* OnInputFailure */
#define CSPA_ON_INPUT_FAILURE_DEGRADE 1u
#define CSPA_ON_INPUT_FAILURE_FAIL 2u

/* SampleType, ByteOrder, DitherMode, Overflow */
#define CSPA_SAMPLE_FORMAT_I16 1u
#define CSPA_SAMPLE_FORMAT_I24 2u
#define CSPA_SAMPLE_FORMAT_F32 3u
#define CSPA_SAMPLE_FORMAT_I32 4u

#define CSPA_BYTE_ORDER_LITTLE 1u
#define CSPA_BYTE_ORDER_BIG 2u

#define CSPA_DITHER_TPDF 1u
#define CSPA_DITHER_NONE 2u

#define CSPA_OVERFLOW_DROP_OLDEST 1u
#define CSPA_OVERFLOW_DROP_NEWEST 2u
#define CSPA_OVERFLOW_FAIL 3u

/* DiscoveryQuery dedupe & order */
#define CSPA_DISCOVERY_DEDUPE_BY_LOGICAL_TARGET 1u
#define CSPA_DISCOVERY_DEDUPE_PER_BACKEND 2u

#define CSPA_DISCOVERY_ORDER_KIND_THEN_BACKEND 1u
#define CSPA_DISCOVERY_ORDER_BACKEND_THEN_KIND 2u

/* DiscoveryEventKind */
#define CSPA_DISCOVERY_EVENT_ADDED 1u
#define CSPA_DISCOVERY_EVENT_REMOVED 2u
#define CSPA_DISCOVERY_EVENT_STATE_CHANGED 3u
#define CSPA_DISCOVERY_EVENT_DEFAULT_DEVICE_CHANGED 4u
#define CSPA_DISCOVERY_EVENT_DEFAULT_INPUT_CHANGED 5u
#define CSPA_DISCOVERY_EVENT_WATCHER_FAILED 6u
#define CSPA_DISCOVERY_EVENT_VISIBILITY_CHANGED 7u
#define CSPA_DISCOVERY_EVENT_RESNAPSHOT_REQUIRED 8u

/* OverlapCombinationSemantics & TargetAcquisition */
#define CSPA_OVERLAP_SINGLE_TARGET 1u
#define CSPA_OVERLAP_SOFTWARE_SUMMATION 2u
#define CSPA_OVERLAP_NATIVE_SET_UNION 3u

#define CSPA_ACQUISITION_DIRECT_ENDPOINT 1u
#define CSPA_ACQUISITION_PROCESS_TREE 2u

/* ParticipationState */
#define CSPA_PARTICIPATION_STARTING 1u
#define CSPA_PARTICIPATION_ACTIVE 2u
#define CSPA_PARTICIPATION_IDLE 3u
#define CSPA_PARTICIPATION_WAITING_RELAUNCH 4u
#define CSPA_PARTICIPATION_LATE 5u
#define CSPA_PARTICIPATION_EXITED 6u
#define CSPA_PARTICIPATION_FAILED 7u

/* PartialReason & BoundaryKind */
#define CSPA_PARTIAL_REASON_COMPLETE 1u
#define CSPA_PARTIAL_REASON_WOULD_BLOCK 2u
#define CSPA_PARTIAL_REASON_BOUNDARY 3u
#define CSPA_PARTIAL_REASON_CANCELLED 4u
#define CSPA_PARTIAL_REASON_TIMEOUT 5u
#define CSPA_PARTIAL_REASON_EOF 6u
#define CSPA_PARTIAL_REASON_FAILURE 7u
#define CSPA_PARTIAL_REASON_READY 8u

#define CSPA_BOUNDARY_NONE 0u
#define CSPA_BOUNDARY_SESSION_CHANGED 1u
#define CSPA_BOUNDARY_CONFIGURATION_CHANGED 2u
#define CSPA_BOUNDARY_DEVICE_CHANGED 3u
#define CSPA_BOUNDARY_FORMAT_CHANGED 4u
#define CSPA_BOUNDARY_GAP 5u

/* ActiveAudio tri-state */
#define CSPA_ACTIVE_AUDIO_UNKNOWN 0u
#define CSPA_ACTIVE_AUDIO_INACTIVE 1u
#define CSPA_ACTIVE_AUDIO_ACTIVE 2u

/* Recording Permissions */
#define CSPA_PERMISSION_KIND_COUNT 3u
#define CSPA_PERMISSION_KIND_MICROPHONE 1u
#define CSPA_PERMISSION_KIND_SYSTEM_AUDIO 2u
#define CSPA_PERMISSION_KIND_SCREEN_RECORDING 3u

#define CSPA_PERMISSION_STATE_GRANTED 1u
#define CSPA_PERMISSION_STATE_NOT_DETERMINED 2u
#define CSPA_PERMISSION_STATE_DENIED 3u
#define CSPA_PERMISSION_STATE_RESTRICTED 4u
#define CSPA_PERMISSION_STATE_UNKNOWN 5u

#define CSPA_PERMISSION_POLICY_AUTO_REQUEST 1u
#define CSPA_PERMISSION_POLICY_CHECK_ONLY 2u

/* Opaque handles */
typedef struct cspa_owner cspa_owner;
typedef struct cspa_cancel cspa_cancel;
typedef struct cspa_discovery cspa_discovery;
typedef struct cspa_discovery_subscription cspa_discovery_subscription;
typedef struct cspa_stream cspa_stream;
typedef struct cspa_manager cspa_manager;

typedef struct cspa_process_identity_record {
    uint32_t pid;
    uint32_t reserved;
    uint64_t generation;
} cspa_process_identity_record;

typedef struct cspa_process_tree_range_record {
    uint32_t start_level;
    uint32_t fallback_limit;
} cspa_process_tree_range_record;

typedef struct cspa_backends_record {
    uint32_t struct_size;
    uint32_t schema_revision;
    uint32_t windows_mask;
    uint32_t linux_mask;
    uint32_t macos_mask;
    uint32_t reserved;
} cspa_backends_record;

typedef struct cspa_pcm_format_record {
    uint32_t struct_size;
    uint32_t schema_revision;
    uint32_t sample_format;
    uint32_t byte_order;
    uint32_t rate_hz;
    uint32_t channels;
} cspa_pcm_format_record;

typedef struct cspa_capture_target_record {
    uint32_t struct_size;
    uint32_t schema_revision;
    uint32_t kind;
    uint32_t scope;
    uint32_t on_root_exit;
    uint32_t default_device_mode;
    uint32_t backend;
    uint32_t available_backends_mask;
    uint32_t pid;
    uint32_t gain_bits;
    uint32_t active_audio;
    uint32_t reserved;
    uint64_t process_start_time;
    uint64_t wait_timeout_ns;
    char device_id[CSPA_DEVICE_ID_MAX_BYTES];
    char app_id[CSPA_APP_ID_MAX_BYTES];
    char name[CSPA_NAME_MAX_BYTES];
    char executable_path[CSPA_EXECUTABLE_PATH_MAX_BYTES];
} cspa_capture_target_record;

typedef struct cspa_discovery_query_record {
    uint32_t struct_size;
    uint32_t schema_revision;
    uint32_t has_backends;
    uint32_t dedupe;
    uint32_t order;
    uint32_t active_only;
    uint32_t include_non_audio;
    uint32_t include_applications;
    uint32_t include_processes;
    uint32_t include_output_devices;
    uint32_t include_input_devices;
    uint32_t reserved;
    cspa_backends_record backends;
} cspa_discovery_query_record;

typedef struct cspa_mixer_config_record {
    uint32_t struct_size;
    uint32_t schema_revision;
    uint32_t max_input_slots;
    uint32_t output_queue_slots;
    uint32_t clamp_f32;
    uint32_t silence_on_idle;
    uint32_t on_input_failure;
    uint32_t drift_crossfade_frames;
    uint64_t alignment_window_ns;
    uint64_t max_input_buffer_ns;
    uint64_t drift_threshold_ns;
    uint64_t drift_hysteresis_ns;
    uint64_t drift_hold_ns;
} cspa_mixer_config_record;

typedef struct cspa_output_converter_record {
    uint32_t struct_size;
    uint32_t schema_revision;
    uint32_t rate_hz;
    uint32_t channels;
    uint32_t sample_format;
    uint32_t byte_order;
    uint32_t dither;
    uint32_t reserved;
    uint64_t seed;
} cspa_output_converter_record;

typedef struct cspa_aggregate_budgets_record {
    uint32_t struct_size;
    uint32_t schema_revision;
    uint32_t max_targets;
    uint32_t max_active_streams;
    uint32_t max_pending_opens;
    uint32_t max_native_graph_links;
    uint32_t max_discovery_queue;
    uint32_t reserved;
    uint64_t max_total_pcm_memory;
    uint64_t cleanup_timeout_ns;
} cspa_aggregate_budgets_record;

typedef struct cspa_capture_request_record {
    uint32_t struct_size;
    uint32_t schema_revision;
    cspa_process_tree_range_record tree_strategy;
    uint32_t combine_mode;
    uint32_t input_conversion;
    uint32_t on_format_change;
    uint32_t allow_reroute;
    uint32_t require_precise_timing;
    uint32_t overflow;
    uint32_t permission_policy;
    uint32_t reserved;
    const cspa_capture_target_record *targets;
    uint64_t target_count;
    const cspa_backends_record *backends;
    const cspa_output_converter_record *converter;
    const cspa_mixer_config_record *mixer_config;
    const cspa_aggregate_budgets_record *budgets;
} cspa_capture_request_record;

typedef struct cspa_permission_entry_record {
    uint32_t kind;
    uint32_t state;
    uint32_t required;
    uint32_t can_prompt;
    uint32_t requires_relaunch;
    uint32_t reserved;
} cspa_permission_entry_record;

typedef struct cspa_permission_report_record {
    uint32_t struct_size;
    uint32_t schema_revision;
    uint32_t platform_os;
    uint32_t is_granted;
    uint32_t has_denial;
    uint32_t requires_relaunch;
    uint32_t entry_count;
    uint32_t reserved;
    cspa_permission_entry_record entries[CSPA_PERMISSION_KIND_COUNT];
} cspa_permission_report_record;

typedef struct cspa_target_plan_record {
    uint32_t target_index;
    uint32_t resolved_backend;
    uint32_t acquisition_kind;
    uint32_t tree_strategy;
    uint32_t attempt_count;
    uint32_t reserved;
} cspa_target_plan_record;

typedef struct cspa_plan_report_record {
    uint32_t struct_size;
    uint32_t schema_revision;
    uint32_t platform_os;
    uint32_t selected_backend;
    uint32_t combine_mode;
    uint32_t combine_resolution;
    uint32_t input_conversion;
    uint32_t uses_software_mixer;
    uint32_t uses_output_converter;
    uint32_t requires_discovery_subscription;
    uint32_t overlap_semantics;
    uint32_t allow_reroute;
    uint32_t permission_policy;
    uint32_t reserved;
    cspa_process_tree_range_record tree_strategy;
    uint64_t target_plan_count;
    uint64_t duplicate_target_count;
    uint64_t estimated_active_streams;
    uint64_t estimated_pcm_bytes;
    cspa_target_plan_record *target_plans;
    uint64_t target_plans_capacity;
} cspa_plan_report_record;

typedef struct cspa_discovery_event_record {
    uint32_t struct_size;
    uint32_t schema_revision;
    uint32_t kind;
    uint32_t visibility;
    uint64_t revision;
    uint64_t minimum_revision;
    char subject[CSPA_EVENT_SUBJECT_MAX_BYTES];
    cspa_capture_target_record target;
} cspa_discovery_event_record;

typedef struct cspa_input_status_record {
    uint32_t struct_size;
    uint32_t schema_revision;
    uint32_t target_index;
    uint32_t backend;
    uint32_t participation_state;
    uint32_t failure_code;
    uint32_t native_rate_hz;
    uint32_t native_channels;
    uint32_t native_sample_format;
    uint32_t mixer_rate_hz;
    uint32_t mixer_channels;
    uint32_t is_resampled;
    uint32_t is_identity_bypass;
    uint32_t reserved;
    uint64_t queued_packets;
    uint64_t dropped_frames;
    uint64_t clipped_samples;
    uint64_t nonfinite_samples;
    uint64_t drift_slips;
} cspa_input_status_record;

typedef struct cspa_header_record {
    uint64_t sequence;
    uint64_t segment;
    uint64_t generation;
    uint64_t frames;
    uint64_t sample_position;
    uint64_t ticks;
    uint64_t ticks_per_second;
    uint64_t clock_domain;
    uint64_t uncertainty_ns;
    uint32_t valid;
    uint32_t provenance;
} cspa_header_record;

typedef struct cspa_block_record {
    cspa_header_record header;
    const uint8_t *bytes;
    uint64_t byte_length;
    cspa_owner *retained_owner;
} cspa_block_record;

typedef struct cspa_mixed_block_metadata_record {
    uint32_t struct_size;
    uint32_t schema_revision;
    uint32_t synthesized_silence;
    uint32_t skew_valid;
    uint64_t membership_version;
    uint64_t contributing_mask;
    uint64_t late_inputs;
    uint64_t failed_inputs;
    uint64_t nonfinite_inputs;
    uint64_t clipped_samples;
    uint64_t nonfinite_samples;
    uint64_t drift_slips;
    uint64_t skew_estimate_ns;
    uint64_t skew_uncertainty_ns;
} cspa_mixed_block_metadata_record;

typedef struct cspa_managed_read_outcome_record {
    uint32_t struct_size;
    uint32_t schema_revision;
    uint32_t partial_reason;
    uint32_t boundary_kind;
    uint32_t has_mixed_metadata;
    uint32_t reserved;
    uint64_t session;
    uint64_t segment;
    uint64_t actual_frames;
    uint64_t copy_bytes;
    uint64_t copy_destinations;
    cspa_block_record block;
    cspa_mixed_block_metadata_record mixed_metadata;
} cspa_managed_read_outcome_record;

typedef uint32_t (*cspa_managed_callback_fn)(
    const cspa_managed_read_outcome_record *outcome,
    void *user_data
);
typedef void (*cspa_context_release_fn)(void *user_data);

#ifdef __cplusplus
extern "C" {
#endif

uint32_t cspa_abi_check(uint32_t abi_major, uint32_t schema_revision);

