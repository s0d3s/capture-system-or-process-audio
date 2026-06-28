#![deny(unsafe_code)]

pub(crate) mod capabilities;
pub(crate) mod discovery;
pub(crate) mod loader;
pub(crate) mod manager;
pub(crate) mod permissions;
pub(crate) mod pipewire_stream;
pub(crate) mod procfs;
pub(crate) mod pulse_stream;

pub use capture_system_or_process_audio_core as core;
pub use core::stream_runtime::{EndReason, StartupCleanup};
pub use core::traits::{
    AudioStreamBackend, CapabilityError, CaptureManagerBackend, DefaultEndpointObserver,
    DiscoveryBackend, OpenableStreamBackend, PermissionsBackend, TimingRequirement,
    TopologyControllerBackend,
};
pub use discovery::{
    PlatformDefaultInputObserver, PlatformDefaultOutputObserver, PlatformDiscovery,
};
pub use manager::PlatformManager;
pub use permissions::PlatformPermissions;
pub use pipewire_stream::{PipeWireStream, PlatformTopologyController};
pub use pulse_stream::{PlatformStream, PulseStream};

pub type AudioStream = core::traits::AudioStream<PlatformStream>;

pub type Discovery = core::traits::Discovery<PlatformDiscovery>;

pub type CaptureManager = core::traits::CaptureManager<PlatformManager>;

pub type Permissions = core::traits::Permissions<PlatformPermissions>;

#[cfg(any(test, feature = "native-tests"))]
pub use capabilities::PulseServerKind;
#[cfg(any(test, feature = "native-tests"))]
pub use discovery::{collect_pipewire_graph, collect_pulse_state};
#[cfg(any(test, feature = "native-tests"))]
pub use loader::{LinuxClientPaths, probe_pipewire_server, probe_pulse_server};
#[cfg(any(test, feature = "native-tests"))]
pub use procfs::{current_process_switch_counts, inspect_process, inspect_process_executable};
