//! 相机控制接口
//!
//! 提供曝光、白平衡、亮度等相机参数的控制功能。
//! 使用 IAMVideoProcAmp 和 IAMCameraControl 接口。

use crate::error::CameraError;
use crate::types::{CameraControlRange, CameraControlType, CameraControlValue, CameraResult};
use std::ffi::c_void;
use std::mem::MaybeUninit;
use windows::core::Interface;
use windows::Win32::Media::DirectShow::{
    CameraControl_Exposure, CameraControl_Flags_Auto, CameraControl_Flags_Manual,
    CameraControl_Focus, CameraControl_Iris, CameraControl_Pan, CameraControl_Tilt,
    CameraControl_Zoom, IAMCameraControl, IAMVideoProcAmp, VideoProcAmp_BacklightCompensation,
    VideoProcAmp_Brightness, VideoProcAmp_Contrast, VideoProcAmp_Gain, VideoProcAmp_Gamma,
    VideoProcAmp_Hue, VideoProcAmp_Saturation, VideoProcAmp_Sharpness, VideoProcAmp_WhiteBalance,
};
use windows::Win32::Media::KernelStreaming::GUID_NULL;
use windows::Win32::Media::MediaFoundation::{IMFSourceReader, MF_SOURCE_READER_MEDIASOURCE};

/// 控制 ID 类型
#[derive(Debug, Clone, Copy)]
pub(crate) enum MFControlId {
    /// IAMVideoProcAmp 布尔类型控制
    ProcAmpBoolean(i32),
    /// IAMVideoProcAmp 范围类型控制
    ProcAmpRange(i32),
    /// IAMCameraControl 值类型控制
    CameraControlValue(i32),
    /// IAMCameraControl 范围类型控制
    CameraControlRange(i32),
}

/// 将 CameraControlType 转换为 Windows 控制 ID
pub(crate) fn control_type_to_mf_id(control: CameraControlType) -> Option<MFControlId> {
    match control {
        CameraControlType::Brightness => Some(MFControlId::ProcAmpRange(VideoProcAmp_Brightness.0)),
        CameraControlType::Contrast => Some(MFControlId::ProcAmpRange(VideoProcAmp_Contrast.0)),
        CameraControlType::Hue => Some(MFControlId::ProcAmpRange(VideoProcAmp_Hue.0)),
        CameraControlType::Saturation => Some(MFControlId::ProcAmpRange(VideoProcAmp_Saturation.0)),
        CameraControlType::Sharpness => Some(MFControlId::ProcAmpRange(VideoProcAmp_Sharpness.0)),
        CameraControlType::Gamma => Some(MFControlId::ProcAmpRange(VideoProcAmp_Gamma.0)),
        CameraControlType::WhiteBalance => {
            Some(MFControlId::ProcAmpRange(VideoProcAmp_WhiteBalance.0))
        }
        CameraControlType::BacklightCompensation => Some(MFControlId::ProcAmpBoolean(
            VideoProcAmp_BacklightCompensation.0,
        )),
        CameraControlType::Gain => Some(MFControlId::ProcAmpRange(VideoProcAmp_Gain.0)),
        CameraControlType::Pan => Some(MFControlId::CameraControlRange(CameraControl_Pan.0)),
        CameraControlType::Tilt => Some(MFControlId::CameraControlRange(CameraControl_Tilt.0)),
        CameraControlType::Zoom => Some(MFControlId::CameraControlRange(CameraControl_Zoom.0)),
        CameraControlType::Exposure => {
            Some(MFControlId::CameraControlValue(CameraControl_Exposure.0))
        }
        CameraControlType::Iris => Some(MFControlId::CameraControlValue(CameraControl_Iris.0)),
        CameraControlType::Focus => Some(MFControlId::CameraControlValue(CameraControl_Focus.0)),
        CameraControlType::AutoExposure => None, // 通过 flag 控制
        CameraControlType::AutoFocus => None,    // 通过 flag 控制
        CameraControlType::AutoWhiteBalance => None, // 通过 flag 控制
    }
}

/// 获取 IAMVideoProcAmp 接口
pub(crate) fn get_video_proc_amp(source_reader: &IMFSourceReader) -> CameraResult<IAMVideoProcAmp> {
    unsafe {
        let mut receiver: MaybeUninit<IAMVideoProcAmp> = MaybeUninit::uninit();
        let ptr = receiver.as_mut_ptr() as *mut *mut c_void;

        source_reader
            .GetServiceForStream(
                MF_SOURCE_READER_MEDIASOURCE.0 as u32,
                &GUID_NULL,
                &IAMVideoProcAmp::IID,
                ptr,
            )
            .map_err(|e| CameraError::Other(format!("Failed to get IAMVideoProcAmp: {}", e)))?;

        Ok(receiver.assume_init())
    }
}

/// 获取 IAMCameraControl 接口
pub(crate) fn get_camera_control(
    source_reader: &IMFSourceReader,
) -> CameraResult<IAMCameraControl> {
    unsafe {
        let mut receiver: MaybeUninit<IAMCameraControl> = MaybeUninit::uninit();
        let ptr = receiver.as_mut_ptr() as *mut *mut c_void;

        source_reader
            .GetServiceForStream(
                MF_SOURCE_READER_MEDIASOURCE.0 as u32,
                &GUID_NULL,
                &IAMCameraControl::IID,
                ptr,
            )
            .map_err(|e| CameraError::Other(format!("Failed to get IAMCameraControl: {}", e)))?;

        Ok(receiver.assume_init())
    }
}

fn automatic_base(control: CameraControlType) -> Option<CameraControlType> {
    match control {
        CameraControlType::AutoExposure => Some(CameraControlType::Exposure),
        CameraControlType::AutoFocus => Some(CameraControlType::Focus),
        CameraControlType::AutoWhiteBalance => Some(CameraControlType::WhiteBalance),
        _ => None,
    }
}
/// 获取控制值
pub(crate) fn get_control(
    source_reader: &IMFSourceReader,
    control: CameraControlType,
) -> CameraResult<CameraControlValue> {
    if let Some(base) = automatic_base(control) {
        let value = get_control(source_reader, base)?;
        return Ok(CameraControlValue::Boolean(value.is_auto()));
    }
    let mf_id = control_type_to_mf_id(control).ok_or_else(|| {
        CameraError::UnsupportedFormat(format!("Unsupported control: {:?}", control))
    })?;

    match mf_id {
        MFControlId::ProcAmpBoolean(id) | MFControlId::ProcAmpRange(id) => {
            let proc_amp = get_video_proc_amp(source_reader)?;
            let mut value = 0i32;
            let mut flags = 0i32;

            unsafe {
                proc_amp.Get(id, &mut value, &mut flags).map_err(|e| {
                    CameraError::Other(format!("Failed to get VideoProcAmp value: {}", e))
                })?;
            }

            let is_auto = flags & CameraControl_Flags_Auto.0 != 0;
            Ok(CameraControlValue::Integer { value, is_auto })
        }
        MFControlId::CameraControlValue(id) | MFControlId::CameraControlRange(id) => {
            let cam_ctrl = get_camera_control(source_reader)?;
            let mut value = 0i32;
            let mut flags = 0i32;

            unsafe {
                cam_ctrl.Get(id, &mut value, &mut flags).map_err(|e| {
                    CameraError::Other(format!("Failed to get CameraControl value: {}", e))
                })?;
            }

            let is_auto = flags & CameraControl_Flags_Auto.0 != 0;
            Ok(CameraControlValue::Integer { value, is_auto })
        }
    }
}

/// 设置控制值
pub(crate) fn set_control(
    source_reader: &IMFSourceReader,
    control: CameraControlType,
    value: CameraControlValue,
) -> CameraResult<()> {
    if let Some(base) = automatic_base(control) {
        let automatic = value.as_bool().ok_or_else(|| {
            CameraError::InvalidConfig("Automatic control expects boolean".into())
        })?;
        let range = get_control_range(source_reader, base)?;
        if automatic && !range.supports_auto {
            return Err(CameraError::ControlNotSupported(
                "Automatic mode unavailable".into(),
            ));
        }
        let current = get_control(source_reader, base)?
            .as_i32()
            .ok_or_else(|| CameraError::NotReadable("Native control value".into()))?;
        return set_control(
            source_reader,
            base,
            CameraControlValue::Integer {
                value: current,
                is_auto: automatic,
            },
        );
    }
    let mf_id = control_type_to_mf_id(control).ok_or_else(|| {
        CameraError::UnsupportedFormat(format!("Unsupported control: {:?}", control))
    })?;

    let (int_value, is_auto) = match value {
        CameraControlValue::Integer { value, is_auto } => (value, is_auto),
        CameraControlValue::Boolean(b) => (if b { 1 } else { 0 }, false),
        CameraControlValue::Float(f) => (f as i32, false),
    };

    let flags = if is_auto {
        CameraControl_Flags_Auto.0
    } else {
        CameraControl_Flags_Manual.0
    };

    match mf_id {
        MFControlId::ProcAmpBoolean(id) | MFControlId::ProcAmpRange(id) => {
            let proc_amp = get_video_proc_amp(source_reader)?;
            unsafe {
                proc_amp.Set(id, int_value, flags).map_err(|e| {
                    CameraError::Other(format!("Failed to set VideoProcAmp value: {}", e))
                })?;
            }
        }
        MFControlId::CameraControlValue(id) | MFControlId::CameraControlRange(id) => {
            let cam_ctrl = get_camera_control(source_reader)?;
            unsafe {
                cam_ctrl.Set(id, int_value, flags).map_err(|e| {
                    CameraError::Other(format!("Failed to set CameraControl value: {}", e))
                })?;
            }
        }
    }

    Ok(())
}

/// 获取控制范围
pub(crate) fn get_control_range(
    source_reader: &IMFSourceReader,
    control: CameraControlType,
) -> CameraResult<CameraControlRange> {
    if let Some(base) = automatic_base(control) {
        let range = get_control_range(source_reader, base)?;
        if !range.supports_auto {
            return Err(CameraError::ControlNotSupported(
                "Automatic mode unavailable".into(),
            ));
        }
        // GetRange exposes capability flags, not an automatic-mode default.
        return Err(CameraError::NotReadable(
            "Automatic mode default/range is not reported by Media Foundation".into(),
        ));
    }
    let mf_id = control_type_to_mf_id(control).ok_or_else(|| {
        CameraError::UnsupportedFormat(format!("Unsupported control: {:?}", control))
    })?;

    let mut min = 0i32;
    let mut max = 0i32;
    let mut step = 0i32;
    let mut default = 0i32;
    let mut flags = 0i32;

    match mf_id {
        MFControlId::ProcAmpBoolean(id) | MFControlId::ProcAmpRange(id) => {
            let proc_amp = get_video_proc_amp(source_reader)?;
            unsafe {
                proc_amp
                    .GetRange(id, &mut min, &mut max, &mut step, &mut default, &mut flags)
                    .map_err(|e| {
                        CameraError::Other(format!("Failed to get VideoProcAmp range: {}", e))
                    })?;
            }
        }
        MFControlId::CameraControlValue(id) | MFControlId::CameraControlRange(id) => {
            let cam_ctrl = get_camera_control(source_reader)?;
            unsafe {
                cam_ctrl
                    .GetRange(id, &mut min, &mut max, &mut step, &mut default, &mut flags)
                    .map_err(|e| {
                        CameraError::Other(format!("Failed to get CameraControl range: {}", e))
                    })?;
            }
        }
    }

    Ok(CameraControlRange {
        min,
        max,
        step,
        default,
        supports_auto: flags & CameraControl_Flags_Auto.0 != 0,
    })
}

/// 检查是否支持指定控制
pub(crate) fn supports_control(
    source_reader: &IMFSourceReader,
    control: CameraControlType,
) -> bool {
    if let Some(base) = automatic_base(control) {
        return get_control_range(source_reader, base).is_ok_and(|r| r.supports_auto);
    }
    get_control_range(source_reader, control).is_ok()
}

/// 获取所有支持的控制类型
pub(crate) fn get_supported_controls(source_reader: &IMFSourceReader) -> Vec<CameraControlType> {
    let all_controls = [
        CameraControlType::Brightness,
        CameraControlType::Contrast,
        CameraControlType::Hue,
        CameraControlType::Saturation,
        CameraControlType::Sharpness,
        CameraControlType::Gamma,
        CameraControlType::WhiteBalance,
        CameraControlType::BacklightCompensation,
        CameraControlType::Gain,
        CameraControlType::Pan,
        CameraControlType::Tilt,
        CameraControlType::Zoom,
        CameraControlType::Exposure,
        CameraControlType::Iris,
        CameraControlType::Focus,
        CameraControlType::AutoExposure,
        CameraControlType::AutoFocus,
        CameraControlType::AutoWhiteBalance,
    ];

    all_controls
        .into_iter()
        .filter(|&ctrl| supports_control(source_reader, ctrl))
        .collect()
}
