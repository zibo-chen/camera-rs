//! MF has a process lifetime; COM has a thread lifetime. Guards are !Send.
use crate::{CameraError, CameraResult};
use std::{cell::RefCell, marker::PhantomData, rc::Rc, sync::Mutex};
use windows::Win32::{
    Foundation::RPC_E_CHANGED_MODE,
    Media::MediaFoundation::{MFShutdown, MFStartup, MFSTARTUP_NOSOCKET, MF_VERSION},
    System::Com::{CoInitializeEx, CoUninitialize, COINIT_MULTITHREADED},
};
static MF_USERS: Mutex<usize> = Mutex::new(0);
thread_local! { static LEGACY_GUARD: RefCell<Option<MFGuard>> = const { RefCell::new(None) }; }

/// Compatibility API; initialization belongs to the calling thread.
pub fn initialize_mf() -> CameraResult<()> {
    LEGACY_GUARD.with(|cell| {
        let mut slot = cell.borrow_mut();
        if slot.is_none() {
            *slot = Some(MFGuard::new()?);
        }
        Ok(())
    })
}
pub fn uninitialize_mf() -> CameraResult<()> {
    LEGACY_GUARD.with(|cell| {
        cell.borrow_mut().take();
    });
    Ok(())
}

pub struct MFGuard {
    owns_com: bool,
    _thread: PhantomData<Rc<()>>,
}
impl MFGuard {
    pub fn new() -> CameraResult<Self> {
        let hr = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        // Existing STA callers own their apartment. Undo only initialization
        // performed here, including a successful S_FALSE result.
        let owns_com = hr.is_ok();
        if hr.is_err() && hr != RPC_E_CHANGED_MODE {
            return Err(CameraError::native(
                crate::CameraErrorKind::BackendFailure,
                crate::BackendId::MEDIA_FOUNDATION,
                crate::OperationStage::Startup,
                "CoInitializeEx".into(),
                i64::from(hr.0),
                format!("{hr:?}"),
            ));
        }
        let mut users = MF_USERS.lock().unwrap();
        if *users == 0 {
            if let Err(error) = unsafe { MFStartup(MF_VERSION, MFSTARTUP_NOSOCKET) } {
                if owns_com {
                    unsafe {
                        CoUninitialize();
                    }
                }
                return Err(super::native_error(
                    crate::CameraErrorKind::BackendFailure,
                    crate::OperationStage::Startup,
                    "MFStartup",
                    error,
                ));
            }
        }
        *users += 1;
        Ok(Self {
            owns_com,
            _thread: PhantomData,
        })
    }
}
impl Drop for MFGuard {
    fn drop(&mut self) {
        let mut users = MF_USERS.lock().unwrap();
        *users -= 1;
        if *users == 0 {
            unsafe {
                let _ = MFShutdown();
            }
        }
        if self.owns_com {
            unsafe {
                CoUninitialize();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parallel_thread_guards_and_legacy_pairing() {
        let workers: Vec<_> = (0..8)
            .map(|_| {
                std::thread::spawn(|| {
                    let _first = MFGuard::new().unwrap();
                    let _second = MFGuard::new().unwrap();
                    initialize_mf().unwrap();
                    uninitialize_mf().unwrap();
                })
            })
            .collect();
        for worker in workers {
            worker.join().unwrap();
        }
    }
}
