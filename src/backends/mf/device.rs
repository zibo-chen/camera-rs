//! 设备枚举和管理
//!
//! 提供 Windows 摄像头设备的发现和信息获取功能。

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

/// 查询所有视频捕获设备的 IMFActivate 指针
pub(crate) fn query_activate_pointers() -> CameraResult<Vec<IMFActivate>> {
    let attributes = unsafe {
        let mut attr: Option<IMFAttributes> = None;
        MFCreateAttributes(&mut attr, 1)
            .map_err(|e| CameraError::Other(format!("Failed to create attributes: {}", e)))?;

        let attr =
            attr.ok_or_else(|| CameraError::Other("MFCreateAttributes returned None".to_string()))?;

        attr.SetGUID(
            &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE,
            &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_GUID,
        )
        .map_err(|e| CameraError::Other(format!("Failed to set device source type: {}", e)))?;

        attr
    };

    let mut count: u32 = 0;
    let mut devices_ptr: MaybeUninit<*mut Option<IMFActivate>> = MaybeUninit::uninit();

    unsafe {
        MFEnumDeviceSources(&attributes, devices_ptr.as_mut_ptr(), &mut count)
            .map_err(|e| CameraError::Other(format!("Failed to enumerate devices: {}", e)))?;
    }

    if count == 0 {
        return Ok(Vec::new());
    }

    let devices_ptr = unsafe { devices_ptr.assume_init() };
    let mut device_list = Vec::with_capacity(count as usize);

    unsafe {
        let slice = std::slice::from_raw_parts(devices_ptr, count as usize);
        device_list.extend(slice.iter().flatten().cloned());
        // 释放设备列表内存
        windows::Win32::System::Com::CoTaskMemFree(Some(devices_ptr as *const _));
    }

    Ok(device_list)
}

/// 从 IMFActivate 获取设备信息
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

/// 获取设备字符串属性
fn get_device_string(activate: &IMFActivate, key: &windows::core::GUID) -> CameraResult<String> {
    let mut pwstr = PWSTR::null();
    let mut len = 0u32;

    unsafe {
        activate
            .GetAllocatedString(key, &mut pwstr, &mut len)
            .map_err(|e| CameraError::Other(format!("Failed to get device string: {}", e)))?;

        if pwstr.is_null() {
            return Err(CameraError::Other("Device string is null".to_string()));
        }

        let result = pwstr
            .to_string()
            .map_err(|e| CameraError::Other(format!("Failed to convert PWSTR: {}", e)))?;

        // 释放字符串内存
        windows::Win32::System::Com::CoTaskMemFree(Some(pwstr.0 as *const _));

        Ok(result)
    }
}

/// 从 IMFActivate 创建 IMFSourceReader
pub(crate) fn create_source_reader(
    activate: &IMFActivate,
    callback: &windows::Win32::Media::MediaFoundation::IMFSourceReaderCallback,
) -> CameraResult<(IMFSourceReader, IMFMediaSource)> {
    unsafe {
        // 激活设备获取 IMFMediaSource
        let media_source: IMFMediaSource = activate.ActivateObject().map_err(|e| {
            CameraError::DeviceOpenFailed(format!("Failed to activate device: {}", e))
        })?;

        let result = (|| -> CameraResult<_> {
            // 创建 SourceReader 属性
            let mut attr: Option<IMFAttributes> = None;
            MFCreateAttributes(&mut attr, 1).map_err(|e| {
                CameraError::Other(format!("Failed to create source reader attributes: {}", e))
            })?;

            let attr = attr.ok_or_else(|| {
                CameraError::Other("MFCreateAttributes returned None".to_string())
            })?;

            // 禁用自动格式转换，我们自己处理
            attr.SetUINT32(&MF_READWRITE_DISABLE_CONVERTERS, 1)
                .map_err(|e| {
                    CameraError::Other(format!("Failed to set disable converters: {}", e))
                })?;

            attr.SetUnknown(
                &windows::Win32::Media::MediaFoundation::MF_SOURCE_READER_ASYNC_CALLBACK,
                callback,
            )
            .map_err(|e| CameraError::Other(e.to_string()))?;

            // 创建 SourceReader
            let source_reader =
                MFCreateSourceReaderFromMediaSource(&media_source, &attr).map_err(|e| {
                    CameraError::DeviceOpenFailed(format!("Failed to create source reader: {}", e))
                })?;

            Ok((source_reader, media_source.clone()))
        })();
        if result.is_err() {
            let _ = media_source.Shutdown();
        }
        result
    }
}

/// 列出所有可用的摄像头设备
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
            // 这个测试可能会失败如果没有摄像头
            let result = list_devices();
            assert!(result.is_ok());
            println!("Found {} devices", result.unwrap().len());
        }
    }
}
