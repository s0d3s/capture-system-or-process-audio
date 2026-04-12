use crate::{
    capabilities::{HRESULT_E_ACCESSDENIED, HRESULT_ERROR_INVALID_PARAMETER, map_windows_error},
    core::{
        schema::{BackendId, ProcessIdentity},
        traits::CapabilityError,
    },
};
use std::sync::{Arc, Condvar, Mutex};
use windows::{
    Win32::{
        Foundation::{CloseHandle, FILETIME, HANDLE, WAIT_OBJECT_0, WAIT_TIMEOUT},
        Media::Audio::*,
        System::{
            Com::{BLOB, StructuredStorage::PROPVARIANT},
            Threading::*,
            Variant::VT_BLOB,
        },
    },
    core::{Interface, Ref, implement},
};

pub(crate) struct ProcessLease(HANDLE);

impl ProcessLease {
    pub(crate) fn handle(&self) -> HANDLE {
        self.0
    }

    pub(crate) fn exited(&self) -> Result<bool, CapabilityError> {
        // SAFETY: this lease owns a live SYNCHRONIZE-capable handle until drop.
        match unsafe { WaitForSingleObject(self.0, 0) } {
            WAIT_OBJECT_0 => Ok(true),
            WAIT_TIMEOUT => Ok(false),
            other => Err(CapabilityError::BackendFailure {
                backend: BackendId::Wasapi,
                code: other.0 as i32,
            }),
        }
    }

    fn open(pid: u32) -> Result<(Self, ProcessIdentity), CapabilityError> {
        if pid == 0 {
            return Err(CapabilityError::InvalidTarget);
        }
        // SAFETY: returned process handle is owned, queried without mutation and closed exactly once.
        unsafe {
            let raw_handle = OpenProcess(
                PROCESS_ACCESS_RIGHTS(PROCESS_QUERY_LIMITED_INFORMATION.0 | 0x00100000),
                false,
                pid,
            )
            .map_err(|err| match err.code().0 {
                HRESULT_E_ACCESSDENIED => CapabilityError::PermissionDenied,
                HRESULT_ERROR_INVALID_PARAMETER => CapabilityError::NoSuchProcess,
                _ => CapabilityError::NoSuchProcess,
            })?;
            let handle = Self(raw_handle);
            if handle.exited()? {
                return Err(CapabilityError::NoSuchProcess);
            }
            let mut creation = FILETIME::default();
            let mut exit = FILETIME::default();
            let mut kernel = FILETIME::default();
            let mut user = FILETIME::default();
            GetProcessTimes(handle.0, &mut creation, &mut exit, &mut kernel, &mut user)
                .map_err(map_windows_error)?;
            let generation =
                u64::from(creation.dwHighDateTime) << 32 | u64::from(creation.dwLowDateTime);
            Ok((handle, ProcessIdentity { pid, generation }))
        }
    }

    fn executable_path(&self) -> Result<String, CapabilityError> {
        let mut buffer = [0u16; 1024];
        let mut length = buffer.len() as u32;
        // SAFETY: handle has PROCESS_QUERY_LIMITED_INFORMATION; buffer and length are valid.
        unsafe {
            QueryFullProcessImageNameW(
                self.0,
                PROCESS_NAME_FORMAT(0),
                windows::core::PWSTR(buffer.as_mut_ptr()),
                &mut length,
            )
            .map_err(map_windows_error)?;
        }
        Ok(String::from_utf16_lossy(&buffer[..length as usize]))
    }
}

impl Drop for ProcessLease {
    fn drop(&mut self) {
        // SAFETY: this is the sole owner of the process handle.
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

pub fn inspect_process(pid: u32) -> Result<ProcessIdentity, CapabilityError> {
    Ok(ProcessLease::open(pid)?.1)
}

pub fn inspect_process_executable(pid: u32) -> Result<(ProcessIdentity, String), CapabilityError> {
    let (lease, identity) = ProcessLease::open(pid)?;
    let path = lease.executable_path()?;
    Ok((identity, path))
}

pub fn windows_build_number() -> Option<u32> {
    #[repr(C)]
    struct RtlOsVersionInfoExW {
        os_version_info_size: u32,
        major_version: u32,
        minor_version: u32,
        build_number: u32,
        platform_id: u32,
        csd_version: [u16; 128],
        service_pack_major: u16,
        service_pack_minor: u16,
        suite_mask: u16,
        product_type: u8,
        reserved: u8,
    }
    #[link(name = "ntdll")]
    unsafe extern "system" {
        fn RtlGetVersion(version_information: *mut RtlOsVersionInfoExW) -> i32;
    }
    let mut info = RtlOsVersionInfoExW {
        os_version_info_size: std::mem::size_of::<RtlOsVersionInfoExW>() as u32,
        major_version: 0,
        minor_version: 0,
        build_number: 0,
        platform_id: 0,
        csd_version: [0; 128],
        service_pack_major: 0,
        service_pack_minor: 0,
        suite_mask: 0,
        product_type: 0,
        reserved: 0,
    };
    // SAFETY: `info` is properly initialized with its struct size and owned on the stack.
    let status = unsafe { RtlGetVersion(&mut info) };
    (status == 0 && info.build_number > 0).then_some(info.build_number)
}

#[implement(IActivateAudioInterfaceCompletionHandler)]
struct Completion {
    state: Arc<(Mutex<bool>, Condvar)>,
}

impl IActivateAudioInterfaceCompletionHandler_Impl for Completion_Impl {
    fn ActivateCompleted(
        &self,
        _: Ref<'_, IActivateAudioInterfaceAsyncOperation>,
    ) -> windows::core::Result<()> {
        let (ready, wake) = &*self.state;
        *ready.lock().unwrap_or_else(|error| error.into_inner()) = true;
        wake.notify_one();
        Ok(())
    }
}

pub(crate) fn activate(
    root: ProcessIdentity,
) -> Result<(IAudioClient, ProcessLease), CapabilityError> {
    let (lease, current) = ProcessLease::open(root.pid)?;
    if current != root {
        return Err(CapabilityError::PidReused);
    }
    // SAFETY: worker apartment is initialized; parameters/variant/handler remain alive until native completion.
    // ManuallyDrop prevents PROPVARIANT from freeing borrowed stack storage; operation never outlives that storage.
    unsafe {
        let mut parameters = AUDIOCLIENT_ACTIVATION_PARAMS {
            ActivationType: AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK,
            Anonymous: AUDIOCLIENT_ACTIVATION_PARAMS_0 {
                ProcessLoopbackParams: AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS {
                    TargetProcessId: root.pid,
                    ProcessLoopbackMode: PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE,
                },
            },
        };
        let mut variant = std::mem::ManuallyDrop::new(PROPVARIANT::default());
        let fields = &mut *variant.Anonymous.Anonymous;
        fields.vt = VT_BLOB;
        fields.Anonymous.blob = BLOB {
            cbSize: std::mem::size_of_val(&parameters) as u32,
            pBlobData: (&mut parameters as *mut AUDIOCLIENT_ACTIVATION_PARAMS).cast(),
        };
        let state = Arc::new((Mutex::new(false), Condvar::new()));
        let handler: IActivateAudioInterfaceCompletionHandler = Completion {
            state: state.clone(),
        }
        .into();
        let operation = ActivateAudioInterfaceAsync(
            VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK,
            &IAudioClient::IID,
            Some(&*variant),
            &handler,
        )
        .map_err(map_windows_error)?;
        let (ready, wake) = &*state;
        let completed = wake
            .wait_while(
                ready.lock().unwrap_or_else(|error| error.into_inner()),
                |ready| !*ready,
            )
            .unwrap_or_else(|error| error.into_inner());
        drop(completed);
        let mut status = windows::core::HRESULT(0);
        let mut interface = None;
        operation
            .GetActivateResult(&mut status, &mut interface)
            .map_err(map_windows_error)?;
        status.ok().map_err(map_windows_error)?;
        if lease.exited()? {
            return Err(CapabilityError::NoSuchProcess);
        }
        let raw_if = interface.ok_or(CapabilityError::DeviceUnavailable)?;
        Ok((raw_if.cast().map_err(map_windows_error)?, lease))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::{Command, Stdio};

    type TestResult<Value> = std::result::Result<Value, Box<dyn std::error::Error + Send + Sync>>;

    #[test]
    #[ignore = "child protocol helper; run only under the identity test"]
    fn identity_child() {
        let mut line = String::new();
        std::io::stdin().read_line(&mut line).unwrap();
    }

    #[test]
    fn retained_identity_observes_actual_child_exit() -> TestResult<()> {
        use crate::core::{
            StorageLimits,
            ordering::{self, Item, Offer, Overflow},
            schema::{ByteOrder, Format, Header, Layout, SampleType},
            storage::Pool,
        };
        use std::sync::atomic::{AtomicU32, Ordering::SeqCst};
        let mut child = Command::new(std::env::current_exe()?)
            .args([
                "--exact",
                "process::tests::identity_child",
                "--ignored",
                "--nocapture",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;
        let (lease, identity) = ProcessLease::open(child.id())?;
        assert_eq!(identity, inspect_process(child.id())?);
        assert!(!lease.exited()?);
        let format = Arc::new(
            Format::new(
                SampleType::I16,
                ByteOrder::Little,
                48000,
                1,
                Layout::Unknown,
            )
            .unwrap(),
        );
        let pool = Pool::new(StorageLimits::new(2, 2, 2, 8).unwrap(), 2, format).unwrap();
        let (mut producer, mut reader) = ordering::channel(pool, 2).unwrap();
        let ended = AtomicU32::new(0);
        let header = Header {
            sequence: 1,
            segment: 1,
            generation: 1,
            frames: 2,
            sample_position: None,
            timestamp: None,
        };
        assert!(!crate::endpoint::root_ended(
            Some(&lease),
            &producer,
            &ended
        )?);
        assert_eq!(
            producer.offer(&[1, 0, 2, 0], header, Overflow::Fail),
            Offer::Queued
        );
        drop(child.stdin.take());
        // SAFETY: lease retains the process handle throughout this finite wait.
        assert_eq!(
            unsafe { WaitForSingleObject(lease.handle(), 2000) },
            WAIT_OBJECT_0
        );
        assert!(child.wait()?.success());
        assert!(lease.exited()?);
        assert!(inspect_process(identity.pid).is_err());
        assert!(crate::endpoint::root_ended(
            Some(&lease),
            &producer,
            &ended
        )?);
        assert_eq!(ended.load(SeqCst), 2);
        assert_eq!(
            producer.offer(
                &[3, 0, 4, 0],
                Header {
                    sequence: 2,
                    ..header
                },
                Overflow::Fail
            ),
            Offer::Stopped
        );
        producer.close();
        drop(producer);
        let Some(Item::Audio(tail)) = reader.next_item() else {
            panic!("accepted tail lost");
        };
        assert_eq!(tail.bytes(), &[1, 0, 2, 0]);
        assert!(reader.next_item().is_none());
        assert!(reader.ended());
        Ok(())
    }
}
