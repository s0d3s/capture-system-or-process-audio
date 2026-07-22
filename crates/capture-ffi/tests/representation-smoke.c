#include "../include/cspa_experimental.h"

#ifdef __cplusplus
#define CHECK static_assert
#else
#define CHECK _Static_assert
#endif

CHECK(CSPA_EXPERIMENTAL_ABI_MAJOR == 0u, "ABI major must be 0");
CHECK(CSPA_EXPERIMENTAL_SCHEMA_REVISION == 1u, "schema revision must be 1");
CHECK(sizeof(void *) == 8u, "64-bit ABI layout invariant");

CHECK(sizeof(cspa_process_identity_record) == 16u, "process identity layout");
CHECK(offsetof(cspa_process_identity_record, generation) == 8u, "process identity generation offset");

CHECK(sizeof(cspa_process_tree_range_record) == 8u, "process tree range layout");
CHECK(sizeof(cspa_backends_record) == 24u, "backends layout");
CHECK(sizeof(cspa_pcm_format_record) == 24u, "pcm format layout");

CHECK(sizeof(cspa_capture_target_record) == 832u, "capture target layout");
CHECK(offsetof(cspa_capture_target_record, struct_size) == 0u, "capture target prefix");
CHECK(offsetof(cspa_capture_target_record, backend) == 24u, "capture target backend offset");
CHECK(offsetof(cspa_capture_target_record, available_backends_mask) == 28u, "capture target available_backends offset");
CHECK(offsetof(cspa_capture_target_record, process_start_time) == 48u, "capture target start_time offset");
CHECK(offsetof(cspa_capture_target_record, device_id) == 64u, "capture target device_id offset");
CHECK(offsetof(cspa_capture_target_record, app_id) == 320u, "capture target app_id offset");
CHECK(offsetof(cspa_capture_target_record, name) == 448u, "capture target name offset");
CHECK(offsetof(cspa_capture_target_record, executable_path) == 576u, "capture target executable_path offset");

CHECK(sizeof(cspa_discovery_query_record) == 72u, "discovery query layout");
CHECK(offsetof(cspa_discovery_query_record, backends) == 48u, "discovery query backends offset");

CHECK(sizeof(cspa_mixer_config_record) == 72u, "mixer config layout");
CHECK(offsetof(cspa_mixer_config_record, alignment_window_ns) == 32u, "mixer config alignment_window offset");

CHECK(sizeof(cspa_output_converter_record) == 40u, "output converter layout");
CHECK(offsetof(cspa_output_converter_record, seed) == 32u, "output converter seed offset");

CHECK(sizeof(cspa_aggregate_budgets_record) == 48u, "aggregate budgets layout");
CHECK(offsetof(cspa_aggregate_budgets_record, max_total_pcm_memory) == 32u, "aggregate budgets memory offset");

CHECK(sizeof(cspa_capture_request_record) == 96u, "capture request layout");
CHECK(offsetof(cspa_capture_request_record, struct_size) == 0u, "capture request prefix");
CHECK(offsetof(cspa_capture_request_record, tree_strategy) == 8u, "capture request tree_strategy offset");
CHECK(offsetof(cspa_capture_request_record, permission_policy) == 40u, "capture request permission_policy offset");
CHECK(offsetof(cspa_capture_request_record, targets) == 48u, "capture request targets offset");
CHECK(offsetof(cspa_capture_request_record, budgets) == 88u, "capture request budgets offset");

CHECK(sizeof(cspa_permission_entry_record) == 24u, "permission entry layout");
CHECK(sizeof(cspa_permission_report_record) == 104u, "permission report layout");
CHECK(offsetof(cspa_permission_report_record, entries) == 32u, "permission report entries offset");

CHECK(sizeof(cspa_target_plan_record) == 24u, "target plan layout");
CHECK(sizeof(cspa_plan_report_record) == 112u, "plan report layout");
CHECK(offsetof(cspa_plan_report_record, permission_policy) == 48u, "plan report permission_policy offset");
CHECK(offsetof(cspa_plan_report_record, tree_strategy) == 56u, "plan report tree_strategy offset");
CHECK(offsetof(cspa_plan_report_record, target_plans) == 96u, "plan report target_plans offset");

CHECK(sizeof(cspa_discovery_event_record) == 1120u, "discovery event layout");
CHECK(offsetof(cspa_discovery_event_record, subject) == 32u, "discovery event subject offset");
CHECK(offsetof(cspa_discovery_event_record, target) == 288u, "discovery event target offset");

CHECK(sizeof(cspa_input_status_record) == 96u, "input status layout");
CHECK(offsetof(cspa_input_status_record, native_rate_hz) == 24u, "input status native_rate offset");
CHECK(offsetof(cspa_input_status_record, queued_packets) == 56u, "input status queued_packets offset");

CHECK(sizeof(cspa_header_record) == 80u, "header layout");
CHECK(offsetof(cspa_header_record, valid) == 72u, "header valid offset");

CHECK(sizeof(cspa_block_record) == 104u, "block layout");
CHECK(sizeof(((cspa_block_record *)0)->retained_owner) == sizeof(void *), "owner handle width");

CHECK(sizeof(cspa_mixed_block_metadata_record) == 96u, "mixed block metadata layout");
CHECK(sizeof(cspa_managed_read_outcome_record) == 264u, "managed read outcome layout");
CHECK(offsetof(cspa_managed_read_outcome_record, block) == 64u, "managed read outcome block offset");
CHECK(offsetof(cspa_managed_read_outcome_record, mixed_metadata) == 168u, "managed read outcome metadata offset");

int main(void) {
    cspa_capture_target_record target;
    cspa_capture_request_record request;
    target.struct_size = (uint32_t)sizeof(target);
    target.schema_revision = CSPA_EXPERIMENTAL_SCHEMA_REVISION;
    request.struct_size = (uint32_t)sizeof(request);
    request.schema_revision = CSPA_EXPERIMENTAL_SCHEMA_REVISION;
    return target.struct_size == 0u || request.struct_size == 0u;
}
