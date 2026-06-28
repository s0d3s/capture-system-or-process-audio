#![deny(unsafe_code)]

pub(crate) mod capabilities;
pub(crate) mod coreaudio_stream;
pub(crate) mod discovery;
pub(crate) mod manager;
#[cfg(target_os = "macos")]
pub(crate) mod native_bridge;
pub(crate) mod permissions;
pub(crate) mod proc_info;
pub(crate) mod sck_stream;

pub use capture_system_or_process_audio_core as core;
pub use core::stream_runtime::{EndReason, StartupCleanup};
pub use core::traits::{
    AudioStreamBackend, CapabilityError, CaptureManagerBackend, DefaultEndpointObserver,
    DiscoveryBackend, OpenableStreamBackend, PermissionsBackend, TimingRequirement,
    TopologyControllerBackend,
};
pub use coreaudio_stream::{CoreAudioInputStream, CoreAudioTapStream, PlatformTopologyController};
pub use discovery::{
    PlatformDefaultInputObserver, PlatformDefaultOutputObserver, PlatformDiscovery,
};
pub use manager::PlatformManager;
pub use permissions::PlatformPermissions;
pub use sck_stream::{PlatformStream, ScreenCaptureKitStream};

pub type AudioStream = core::traits::AudioStream<PlatformStream>;

pub type Discovery = core::traits::Discovery<PlatformDiscovery>;

pub type CaptureManager = core::traits::CaptureManager<PlatformManager>;

pub type Permissions = core::traits::Permissions<PlatformPermissions>;

#[cfg(any(test, feature = "native-tests"))]
pub use capabilities::resolve_macos_plan_with_readiness;
#[cfg(any(test, feature = "native-tests"))]
pub use coreaudio_stream::{open_coreaudio_input_point, pack_planar_or_interleaved_f32_into_bytes};
#[cfg(any(test, feature = "native-tests"))]
pub use discovery::collect_live_coreaudio_process_objects;
#[cfg(all(target_os = "macos", feature = "native-tests"))]
pub use native_bridge::NativeTestVirtualMicHandle;
#[cfg(all(target_os = "macos", any(test, feature = "native-tests")))]
pub use native_bridge::{
    list_coreaudio_input_devices, list_coreaudio_output_devices, microphone_auth_status,
    os_version, sck_preflight_granted,
};
#[cfg(any(test, feature = "native-tests"))]
pub use proc_info::{current_mach_usage_counts, inspect_process, inspect_process_executable};
