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

