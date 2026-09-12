//! Camera control interface.
//!
//! Controls exposure, white balance, brightness, and related parameters through
//! IAMVideoProcAmp and IAMCameraControl.

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

/// Control ID category.
#[derive(Debug, Clone, Copy)]
pub(crate) enum MFControlId {
    /// Boolean IAMVideoProcAmp control.
    ProcAmpBoolean(i32),
    /// Ranged IAMVideoProcAmp control.
    ProcAmpRange(i32),
    /// Value IAMCameraControl control.
    CameraControlValue(i32),
    /// Ranged IAMCameraControl control.
    CameraControlRange(i32),
}

/// Maps CameraControlType to a Windows control ID.
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
        CameraControlType::AutoExposure => None, // Controlled through flags.
        CameraControlType::AutoFocus => None,    // Controlled through flags.
        CameraControlType::AutoWhiteBalance => None, // Controlled through flags.
    }
}

/// Returns the IAMVideoProcAmp interface.
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
            .map_err(|e| {
                super::native_error(
                    crate::CameraErrorKind::ControlFailure,
                    crate::OperationStage::BackendCommand,
                    "get IAMVideoProcAmp",
                    e,
                )
            })?;

        Ok(receiver.assume_init())
    }
}

/// Returns the IAMCameraControl interface.
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
            .map_err(|e| {
                super::native_error(
                    crate::CameraErrorKind::ControlFailure,
                    crate::OperationStage::BackendCommand,
                    "get IAMCameraControl",
                    e,
                )
            })?;

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
/// Reads a control value.
pub(crate) fn get_control(
    source_reader: &IMFSourceReader,
    control: CameraControlType,
) -> CameraResult<CameraControlValue> {
    if let Some(base) = automatic_base(control) {
        let value = get_control(source_reader, base)?;
        return Ok(CameraControlValue::Boolean(value.is_auto()));
    }
    let mf_id = control_type_to_mf_id(control)
        .ok_or_else(|| CameraError::control_not_supported(format!("{control:?}")))?;

    match mf_id {
        MFControlId::ProcAmpBoolean(id) | MFControlId::ProcAmpRange(id) => {
            let proc_amp = get_video_proc_amp(source_reader)?;
            let mut value = 0i32;
            let mut flags = 0i32;

            unsafe {
                proc_amp.Get(id, &mut value, &mut flags).map_err(|e| {
                    super::native_error(
                        crate::CameraErrorKind::ControlFailure,
                        crate::OperationStage::BackendCommand,
                        "get VideoProcAmp value",
                        e,
                    )
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
                    super::native_error(
                        crate::CameraErrorKind::ControlFailure,
                        crate::OperationStage::BackendCommand,
                        "get CameraControl value",
                        e,
                    )
                })?;
            }

            let is_auto = flags & CameraControl_Flags_Auto.0 != 0;
            Ok(CameraControlValue::Integer { value, is_auto })
        }
    }
}

/// Sets a control value.
pub(crate) fn set_control(
    source_reader: &IMFSourceReader,
    control: CameraControlType,
    value: CameraControlValue,
) -> CameraResult<()> {
    if let Some(base) = automatic_base(control) {
        let automatic = value.as_bool().ok_or_else(|| {
            CameraError::invalid_config("Automatic control expects boolean".into())
        })?;
        let range = get_control_range(source_reader, base)?;
        if automatic && !range.supports_auto {
            return Err(CameraError::control_not_supported(
                "Automatic mode unavailable".into(),
            ));
        }
        let current = get_control(source_reader, base)?
            .as_i32()
            .ok_or_else(|| CameraError::control_not_readable("Native control value".into()))?;
        return set_control(
            source_reader,
            base,
            CameraControlValue::Integer {
                value: current,
                is_auto: automatic,
            },
        );
    }
    let mf_id = control_type_to_mf_id(control)
        .ok_or_else(|| CameraError::control_not_supported(format!("{control:?}")))?;

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
                    super::native_error(
                        crate::CameraErrorKind::ControlFailure,
                        crate::OperationStage::BackendCommand,
                        "set VideoProcAmp value",
                        e,
                    )
                })?;
            }
        }
        MFControlId::CameraControlValue(id) | MFControlId::CameraControlRange(id) => {
            let cam_ctrl = get_camera_control(source_reader)?;
            unsafe {
                cam_ctrl.Set(id, int_value, flags).map_err(|e| {
                    super::native_error(
                        crate::CameraErrorKind::ControlFailure,
                        crate::OperationStage::BackendCommand,
                        "set CameraControl value",
                        e,
                    )
                })?;
            }
        }
    }

    Ok(())
}

/// Returns a control range.
pub(crate) fn get_control_range(
    source_reader: &IMFSourceReader,
    control: CameraControlType,
) -> CameraResult<CameraControlRange> {
    if let Some(base) = automatic_base(control) {
        let range = get_control_range(source_reader, base)?;
        if !range.supports_auto {
            return Err(CameraError::control_not_supported(
                "Automatic mode unavailable".into(),
            ));
        }
        // GetRange exposes capability flags, not an automatic-mode default.
        return Err(CameraError::control_not_readable(
            "Automatic mode default/range is not reported by Media Foundation".into(),
        ));
    }
    let mf_id = control_type_to_mf_id(control)
        .ok_or_else(|| CameraError::control_not_supported(format!("{control:?}")))?;

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
                        super::native_error(
                            crate::CameraErrorKind::ControlFailure,
                            crate::OperationStage::BackendCommand,
                            "get VideoProcAmp range",
                            e,
                        )
                    })?;
            }
        }
        MFControlId::CameraControlValue(id) | MFControlId::CameraControlRange(id) => {
            let cam_ctrl = get_camera_control(source_reader)?;
            unsafe {
                cam_ctrl
                    .GetRange(id, &mut min, &mut max, &mut step, &mut default, &mut flags)
                    .map_err(|e| {
                        super::native_error(
                            crate::CameraErrorKind::ControlFailure,
                            crate::OperationStage::BackendCommand,
                            "get CameraControl range",
                            e,
                        )
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

/// Checks whether a control is supported.
pub(crate) fn supports_control(
    source_reader: &IMFSourceReader,
    control: CameraControlType,
) -> bool {
    if let Some(base) = automatic_base(control) {
        return get_control_range(source_reader, base).is_ok_and(|r| r.supports_auto);
    }
    get_control_range(source_reader, control).is_ok()
}

/// Returns all supported control types.
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
