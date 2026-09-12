//! Device enumeration and management.
//!
//! Provides Windows camera discovery and device information.

use crate::error::CameraError;
use crate::types::{CameraDeviceInfo, CameraResult};
use std::mem::MaybeUninit;
use windows::core::PWSTR;
use windows::Win32::Media::MediaFoundation::{
    IMFActivate, IMFAttributes, IMFMediaSource, IMFSourceReader, MFCreateAttributes,
    MFCreateSourceReaderFromMediaSource, MFEnumDeviceSources, MF_DEVSOURCE_ATTRIBUTE_FRIENDLY_NAME,
    MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE, MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_GUID,
    MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_SYMBOLIC_LINK, MF_READWRITE_DISABLE_CONVERTERS,
};

use super::com::MFGuard;

/// Queries IMFActivate pointers for all video capture devices.
pub(crate) fn query_activate_pointers() -> CameraResult<Vec<IMFActivate>> {
    let attributes = unsafe {
        let mut attr: Option<IMFAttributes> = None;
        MFCreateAttributes(&mut attr, 1).map_err(|e| {
            super::native_error(
                crate::CameraErrorKind::BackendFailure,
                crate::OperationStage::Enumeration,
                "create enumeration attributes",
                e,
            )
        })?;

        let attr = attr.ok_or_else(|| {
            CameraError::backend_failure(
                crate::BackendId::MEDIA_FOUNDATION,
                crate::OperationStage::Enumeration,
                "MFCreateAttributes returned no enumeration attributes".into(),
            )
        })?;

        attr.SetGUID(
            &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE,
            &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_GUID,
        )
        .map_err(|e| {
            super::native_error(
                crate::CameraErrorKind::BackendFailure,
                crate::OperationStage::Enumeration,
                "set device source type",
                e,
            )
        })?;

        attr
    };

    let mut count: u32 = 0;
    let mut devices_ptr: MaybeUninit<*mut Option<IMFActivate>> = MaybeUninit::uninit();

    unsafe {
        MFEnumDeviceSources(&attributes, devices_ptr.as_mut_ptr(), &mut count).map_err(|e| {
            super::native_error(
                crate::CameraErrorKind::BackendFailure,
                crate::OperationStage::Enumeration,
                "enumerate devices",
                e,
            )
        })?;
    }

    if count == 0 {
        return Ok(Vec::new());
    }

    let devices_ptr = unsafe { devices_ptr.assume_init() };
    let mut device_list = Vec::with_capacity(count as usize);

    unsafe {
        let slice = std::slice::from_raw_parts(devices_ptr, count as usize);
        device_list.extend(slice.iter().flatten().cloned());
        // Release the device list allocation.
        windows::Win32::System::Com::CoTaskMemFree(Some(devices_ptr as *const _));
    }

    Ok(device_list)
}

/// Reads device information from IMFActivate.
pub(crate) fn activate_to_device_info(
    index: u32,
    activate: &IMFActivate,
) -> CameraResult<CameraDeviceInfo> {
    let name = get_device_string(activate, &MF_DEVSOURCE_ATTRIBUTE_FRIENDLY_NAME)?;
    let symlink = get_device_string(
        activate,
        &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_SYMBOLIC_LINK,
    )?;

    Ok(CameraDeviceInfo {
        index,
        name,
        description: format!("Media Foundation Camera: {}", symlink),
        vendor_id: None,
        product_id: None,
        serial_number: None,
        device_path: Some(symlink),
    })
}

/// Reads a string attribute from the device.
fn get_device_string(activate: &IMFActivate, key: &windows::core::GUID) -> CameraResult<String> {
    let mut pwstr = PWSTR::null();
    let mut len = 0u32;

    unsafe {
        activate
            .GetAllocatedString(key, &mut pwstr, &mut len)
            .map_err(|e| {
                super::native_error(
                    crate::CameraErrorKind::BackendFailure,
                    crate::OperationStage::Enumeration,
                    "get device string",
                    e,
                )
            })?;

        if pwstr.is_null() {
            return Err(CameraError::backend_failure(
                crate::BackendId::MEDIA_FOUNDATION,
                crate::OperationStage::Enumeration,
                "Device string is null".into(),
            ));
        }

        let result = pwstr.to_string().map_err(|e| {
            CameraError::backend_failure(
                crate::BackendId::MEDIA_FOUNDATION,
                crate::OperationStage::Enumeration,
                "Failed to convert device string".into(),
            )
            .with_operation("convert device string")
            .with_source(e)
        })?;

        // Release the string allocation.
        windows::Win32::System::Com::CoTaskMemFree(Some(pwstr.0 as *const _));

        Ok(result)
    }
}

/// Creates an IMFSourceReader from IMFActivate.
pub(crate) fn create_source_reader(
    activate: &IMFActivate,
    callback: &windows::Win32::Media::MediaFoundation::IMFSourceReaderCallback,
) -> CameraResult<(IMFSourceReader, IMFMediaSource)> {
    unsafe {
        // Activate the device to obtain IMFMediaSource.
        let media_source: IMFMediaSource = activate.ActivateObject().map_err(|e| {
            super::native_error(
                crate::CameraErrorKind::DeviceOpenFailed,
                crate::OperationStage::Open,
                "activate device",
                e,
            )
        })?;

        let result = (|| -> CameraResult<_> {
            // Create SourceReader attributes.
            let mut attr: Option<IMFAttributes> = None;
            MFCreateAttributes(&mut attr, 1).map_err(|e| {
                super::native_error(
                    crate::CameraErrorKind::DeviceOpenFailed,
                    crate::OperationStage::Open,
                    "create source reader attributes",
                    e,
                )
            })?;

            let attr = attr.ok_or_else(|| {
                CameraError::backend_failure(
                    crate::BackendId::MEDIA_FOUNDATION,
                    crate::OperationStage::Open,
                    "MFCreateAttributes returned no source reader attributes".into(),
                )
            })?;

            // Disable automatic conversion because the backend handles formats itself.
            attr.SetUINT32(&MF_READWRITE_DISABLE_CONVERTERS, 1)
                .map_err(|e| {
                    super::native_error(
                        crate::CameraErrorKind::DeviceOpenFailed,
                        crate::OperationStage::Open,
                        "disable source reader converters",
                        e,
                    )
                })?;

            attr.SetUnknown(
                &windows::Win32::Media::MediaFoundation::MF_SOURCE_READER_ASYNC_CALLBACK,
                callback,
            )
            .map_err(|e| {
                super::native_error(
                    crate::CameraErrorKind::DeviceOpenFailed,
                    crate::OperationStage::Open,
                    "set source reader callback",
                    e,
                )
            })?;

            // Create the SourceReader.
            let source_reader =
                MFCreateSourceReaderFromMediaSource(&media_source, &attr).map_err(|e| {
                    super::native_error(
                        crate::CameraErrorKind::DeviceOpenFailed,
                        crate::OperationStage::Open,
                        "create source reader",
                        e,
                    )
                })?;

            Ok((source_reader, media_source.clone()))
        })();
        if result.is_err() {
            let _ = media_source.Shutdown();
        }
        result
    }
}

/// Lists all available camera devices.
pub fn list_devices() -> CameraResult<Vec<CameraDeviceInfo>> {
    let _guard = MFGuard::new()?;
    let activates = query_activate_pointers()?;
    let mut devices = Vec::with_capacity(activates.len());

    for (index, activate) in activates.iter().enumerate() {
        match activate_to_device_info(index as u32, activate) {
            Ok(info) => devices.push(info),
            Err(e) => {
                log::warn!("Failed to get device info for index {}: {}", index, e);
            }
        }
    }

    Ok(devices)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_list_devices() {
        if cfg!(target_os = "windows") {
            // This test may fail when the host has no camera.
            let result = list_devices();
            assert!(result.is_ok());
            println!("Found {} devices", result.unwrap().len());
        }
    }
}
