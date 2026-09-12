//! Snapshot-based device monitoring. Slow observers always see the newest inventory.
use crate::{CameraError, CameraResult, CameraSystem, DeviceInfo};
use std::{sync::Arc, time::Duration};
use tokio::sync::watch;
#[derive(Clone, Debug)]
/// Values for `DeviceSnapshot`.
pub struct DeviceSnapshot {
    /// The generation value.
    pub generation: u64,
    /// The devices value.
    pub devices: Vec<DeviceInfo>,
    /// The error value.
    pub error: Option<Arc<CameraError>>,
}
/// Values for `DeviceWatcher`.
pub struct DeviceWatcher {
    state: watch::Receiver<DeviceSnapshot>,
    cancel: watch::Sender<bool>,
    task: Option<tokio::task::JoinHandle<()>>,
}
impl CameraSystem {
    /// Performs the `watch_devices` operation.
    pub async fn watch_devices(&self, interval: Duration) -> CameraResult<DeviceWatcher> {
        if interval.is_zero() {
            return Err(CameraError::invalid_config(
                "Device scan interval must be positive".into(),
            ));
        }
        let devices = self.devices().await?;
        let system = self.clone();
        let (tx, rx) = watch::channel(DeviceSnapshot {
            generation: 0,
            devices,
            error: None,
        });
        let (cancel, mut cancelled) = watch::channel(false);
        let task = tokio::spawn(async move {
            loop {
                tokio::select! {biased;_ = cancelled.changed()=>break,_=tokio::time::sleep(interval)=>{}}
                let result = tokio::select! {biased;_ = cancelled.changed()=>break,result=system.devices()=>result};
                if *cancelled.borrow() {
                    break;
                }
                let previous = tx.borrow().clone();
                let next = match result {
                    Ok(devices) => DeviceSnapshot {
                        generation: previous.generation + 1,
                        devices,
                        error: None,
                    },
                    Err(e) => DeviceSnapshot {
                        generation: previous.generation + 1,
                        devices: previous.devices.clone(),
                        error: Some(Arc::new(e)),
                    },
                };
                let same = next.error == previous.error
                    && next.devices.len() == previous.devices.len()
                    && next.devices.iter().all(|d| {
                        previous
                            .devices
                            .iter()
                            .any(|p| p.id == d.id && p.name == d.name && p.index == d.index)
                    });
                if !same {
                    tx.send_replace(next);
                }
            }
        });
        Ok(DeviceWatcher {
            state: rx,
            cancel,
            task: Some(task),
        })
    }
}
impl DeviceWatcher {
    /// Performs the `snapshot` operation.
    pub fn snapshot(&self) -> DeviceSnapshot {
        self.state.borrow().clone()
    }
    /// Wait for a changed inventory/error. Generation gaps indicate coalesced updates.
    pub async fn changed(&mut self) -> CameraResult<DeviceSnapshot> {
        self.state
            .changed()
            .await
            .map_err(|_| CameraError::stream_stopped())?;
        Ok(self.state.borrow_and_update().clone())
    }
    /// Performs the `stop` operation.
    pub async fn stop(&mut self) -> CameraResult<()> {
        self.cancel.send_replace(true);
        if let Some(task) = self.task.take() {
            task.await
                .map_err(|e| CameraError::worker_failure(e.to_string()).with_source(e))?;
        }
        Ok(())
    }
}
impl Drop for DeviceWatcher {
    fn drop(&mut self) {
        self.cancel.send_replace(true);
    }
}
