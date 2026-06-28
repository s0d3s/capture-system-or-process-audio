#![deny(unsafe_code)]

pub(crate) mod capabilities;
mod packets;
pub(crate) mod permissions;

#[cfg(windows)]
#[allow(unsafe_code)]
pub(crate) mod discovery;

#[cfg(windows)]
#[allow(unsafe_code)]
pub(crate) mod endpoint;

#[cfg(windows)]
pub(crate) mod manager;

#[cfg(windows)]
#[allow(unsafe_code)]
pub(crate) mod process;

pub use capture_system_or_process_audio_core as core;
pub use core::{
    routing::RouteRollbackGuard,
    stream_runtime::{EndReason, StartupCleanup},
    traits::{
        AudioStreamBackend, CapabilityError, CaptureManagerBackend, DefaultEndpointObserver,
        DiscoveryBackend, OpenableStreamBackend, PermissionsBackend, TimingRequirement,
    },
};
pub use permissions::PlatformPermissions;

pub type Permissions = core::traits::Permissions<PlatformPermissions>;

#[cfg(windows)]
pub use discovery::{
    PlatformDefaultInputObserver, PlatformDefaultOutputObserver, PlatformDiscovery,
};

#[cfg(windows)]
pub use endpoint::{NativeFormat, PlatformStream};

#[cfg(windows)]
pub use manager::PlatformManager;

#[cfg(windows)]
pub type AudioStream = core::traits::AudioStream<PlatformStream>;

#[cfg(windows)]
pub type Discovery = core::traits::Discovery<PlatformDiscovery>;

#[cfg(windows)]
pub type CaptureManager = core::traits::CaptureManager<PlatformManager>;

#[cfg(all(windows, any(test, feature = "native-tests")))]
pub use endpoint::RawProcessCapture;

#[cfg(all(windows, any(test, feature = "native-tests")))]
pub use process::{inspect_process, inspect_process_executable};
