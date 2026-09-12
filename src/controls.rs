//! Controls carry units and report whether readback is actual or only requested.
use crate::api::CaptureSession;
use crate::backends::BackendCamera;
use crate::{
    BackendType, CameraControlType as Raw, CameraControlValue as RawValue, CameraError,
    CameraResult,
};
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ControlId {
    ExposureMode,
    ExposureTime,
    ExposureCompensation,
    FocusMode,
    FocusPosition,
    WhiteBalanceMode,
    WhiteBalanceTemperature,
    Zoom,
    Brightness,
    Contrast,
    Hue,
    Saturation,
    Sharpness,
    Gamma,
    BacklightCompensation,
    Gain,
    Pan,
    Tilt,
    Iris,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ControlUnit {
    Boolean,
    Mode,
    Native,
    HundredMicroseconds,
    Log2Seconds,
    MilliEv,
    CompensationIndex,
    Kelvin,
    Percent,
    Degrees,
    ArcSeconds,
    Iso,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ControlMode {
    Manual,
    Automatic,
    Locked,
    Single,
    Continuous,
}
#[derive(Clone, Debug, PartialEq)]
pub enum ControlValue {
    Number(f64),
    Mode(ControlMode),
}
#[derive(Clone, Debug, PartialEq)]
pub enum ControlReadback {
    Actual(ControlValue),
    Requested(ControlValue),
    Unknown,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Kelvin(u32);
impl Kelvin {
    pub fn new(value: u32) -> CameraResult<Self> {
        if !(1_000..=40_000).contains(&value) {
            return Err(CameraError::InvalidConfig(
                "White-balance temperature must be 1000..=40000 K".into(),
            ));
        }
        Ok(Self(value))
    }
    pub const fn get(self) -> u32 {
        self.0
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Exposure {
    Auto,
    Manual(std::time::Duration),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WhiteBalance {
    Auto,
    Temperature(Kelvin),
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Focus {
    Auto,
    Position(f64),
}
#[derive(Clone, Debug)]
pub struct ControlRange {
    pub min: f64,
    pub max: f64,
    pub step: Option<f64>,
    pub default: Option<ControlValue>,
}
#[derive(Clone, Debug)]
pub struct ControlDescriptor {
    pub id: ControlId,
    pub unit: ControlUnit,
    pub readable: bool,
    pub writable: Option<bool>,
    pub range: Option<ControlRange>,
    pub modes: Vec<ControlMode>,
    /// Some controls only apply while the associated automatic mode is disabled.
    pub requires_manual: Option<ControlId>,
}
#[derive(Debug)]
pub struct ControlOutcome {
    pub id: ControlId,
    pub result: CameraResult<()>,
}
fn mapping(backend: BackendType, id: ControlId) -> CameraResult<(Raw, ControlUnit)> {
    use ControlId::*;
    use ControlUnit as U;
    let av = backend == BackendType::AVFoundation;
    let c2 = backend == BackendType::Camera2;
    let unsupported = || CameraError::ControlNotSupported(format!("{id:?} on {backend}"));
    Ok(match id {
        ExposureMode => (if av { Raw::Exposure } else { Raw::AutoExposure }, U::Mode),
        ExposureTime if av || c2 => return Err(unsupported()),
        ExposureTime => (
            Raw::Exposure,
            if backend == BackendType::MediaFoundation {
                U::Log2Seconds
            } else {
                U::HundredMicroseconds
            },
        ),
        ExposureCompensation if av => (Raw::Brightness, U::MilliEv),
        ExposureCompensation if c2 => (Raw::Exposure, U::CompensationIndex),
        ExposureCompensation => return Err(unsupported()),
        FocusMode => (if av { Raw::Focus } else { Raw::AutoFocus }, U::Mode),
        WhiteBalanceMode => (
            if av {
                Raw::WhiteBalance
            } else {
                Raw::AutoWhiteBalance
            },
            U::Mode,
        ),
        FocusPosition if av || c2 => return Err(unsupported()),
        FocusPosition => (Raw::Focus, U::Native),
        WhiteBalanceTemperature if av || c2 => return Err(unsupported()),
        WhiteBalanceTemperature => (Raw::WhiteBalance, U::Kelvin),
        Brightness if av || c2 => return Err(unsupported()),
        Brightness => (Raw::Brightness, U::Native),
        Contrast => (Raw::Contrast, U::Native),
        Hue => (Raw::Hue, U::Native),
        Saturation => (Raw::Saturation, U::Native),
        Sharpness => (Raw::Sharpness, U::Native),
        Gamma => (Raw::Gamma, U::Native),
        BacklightCompensation => (Raw::BacklightCompensation, U::Native),
        Gain => (Raw::Gain, if c2 { U::Iso } else { U::Native }),
        Zoom => (Raw::Zoom, if av || c2 { U::Percent } else { U::Native }),
        Pan => (Raw::Pan, U::Native),
        Tilt => (Raw::Tilt, U::Native),
        Iris => (Raw::Iris, U::Native),
    })
}
const IDS: &[ControlId] = &[
    ControlId::ExposureMode,
    ControlId::ExposureTime,
    ControlId::ExposureCompensation,
    ControlId::FocusMode,
    ControlId::FocusPosition,
    ControlId::WhiteBalanceMode,
    ControlId::WhiteBalanceTemperature,
    ControlId::Zoom,
    ControlId::Brightness,
    ControlId::Contrast,
    ControlId::Hue,
    ControlId::Saturation,
    ControlId::Sharpness,
    ControlId::Gamma,
    ControlId::BacklightCompensation,
    ControlId::Gain,
    ControlId::Pan,
    ControlId::Tilt,
    ControlId::Iris,
];
fn decode(backend: BackendType, id: ControlId, value: RawValue) -> CameraResult<ControlValue> {
    let (_, unit) = mapping(backend, id)?;
    if unit == ControlUnit::Mode {
        let mode = if backend == BackendType::AVFoundation {
            match value.as_i32() {
                Some(0) => ControlMode::Locked,
                Some(1) => ControlMode::Single,
                Some(2) => ControlMode::Continuous,
                _ => return Err(CameraError::NotReadable("Unknown native mode".into())),
            }
        } else if value.as_bool().unwrap_or(false) {
            ControlMode::Automatic
        } else {
            ControlMode::Manual
        };
        Ok(ControlValue::Mode(mode))
    } else {
        Ok(ControlValue::Number(match value {
            RawValue::Float(v) => v as f64,
            _ => value
                .as_i32()
                .ok_or_else(|| CameraError::NotReadable("Native numeric value".into()))?
                as f64,
        }))
    }
}
fn describe_range(
    backend: BackendType,
    id: ControlId,
    raw: Raw,
    range: crate::CameraControlRange,
) -> ControlRange {
    ControlRange {
        min: range.min as f64,
        max: range.max as f64,
        step: (range.step > 0).then_some(range.step as f64),
        default: if backend == BackendType::Camera2 {
            None
        } else {
            decode(
                backend,
                id,
                if raw.is_auto_control() {
                    RawValue::Boolean(range.default != 0)
                } else {
                    RawValue::manual(range.default)
                },
            )
            .ok()
        },
    }
}
fn descriptor(camera: &BackendCamera, id: ControlId) -> CameraResult<ControlDescriptor> {
    let backend = camera.backend_type();
    let (raw, unit) = mapping(backend, id)?;
    if !camera.supports_control(raw) {
        return Err(CameraError::ControlNotSupported(format!("{id:?}")));
    }
    let readable = backend != BackendType::Camera2 && camera.get_control(raw).is_ok();
    let range = camera
        .get_control_range(raw)
        .ok()
        .map(|r| describe_range(backend, id, raw, r));
    #[allow(unused_mut)]
    let mut modes = if unit == ControlUnit::Mode {
        if backend == BackendType::AVFoundation {
            vec![
                ControlMode::Locked,
                ControlMode::Single,
                ControlMode::Continuous,
            ]
        } else {
            vec![ControlMode::Manual, ControlMode::Automatic]
        }
    } else {
        vec![]
    };
    #[allow(unused_mut)]
    let mut writable = None;
    #[allow(unreachable_patterns)]
    match camera {
        #[cfg(all(
            any(feature = "native", feature = "backend-camera2"),
            target_os = "android"
        ))]
        BackendCamera::Camera2(c) => {
            if unit == ControlUnit::Mode {
                modes = c
                    .supported_modes(raw)?
                    .into_iter()
                    .filter_map(|value| camera2_mode(id, value))
                    .collect();
                modes.dedup();
                writable = Some(!modes.is_empty());
            } else {
                writable = range.as_ref().map(|r| r.max > r.min);
            }
        }
        #[cfg(all(
            feature = "backend-uvc",
            any(target_os = "linux", target_os = "macos", target_os = "android")
        ))]
        BackendCamera::Uvc(c) if id == ControlId::ExposureMode => {
            modes = c.exposure_modes()?;
        }
        #[cfg(all(
            any(feature = "native", feature = "backend-avfoundation"),
            any(target_os = "macos", target_os = "ios")
        ))]
        BackendCamera::AVFoundation(c) => {
            writable = Some(true);
            if unit == ControlUnit::Mode {
                modes = c
                    .supported_modes(raw)?
                    .into_iter()
                    .filter_map(|v| match decode(backend, id, RawValue::manual(v)).ok()? {
                        ControlValue::Mode(m) => Some(m),
                        _ => None,
                    })
                    .collect();
                writable = Some(!modes.is_empty());
            }
        }
        #[cfg(camera_v4l2)]
        BackendCamera::V4l2(c) => {
            writable = Some(c.control_writable(raw)?);
        }
        _ => {}
    }
    let requires_manual = match id {
        ControlId::ExposureTime => Some(ControlId::ExposureMode),
        ControlId::FocusPosition => Some(ControlId::FocusMode),
        ControlId::WhiteBalanceTemperature => Some(ControlId::WhiteBalanceMode),
        _ => None,
    };
    Ok(ControlDescriptor {
        id,
        unit,
        readable,
        writable,
        range,
        modes,
        requires_manual,
    })
}
impl CaptureSession {
    pub async fn controls(&self) -> CameraResult<Vec<ControlDescriptor>> {
        if self.device().id.backend() == &crate::BackendId::SYNTHETIC {
            return Ok(Vec::new());
        }
        self.with_native(|c| {
            let mut controls = Vec::new();
            for &id in IDS {
                match descriptor(&c, id) {
                    Ok(descriptor) => controls.push(descriptor),
                    Err(CameraError::ControlNotSupported(_)) => {}
                    Err(error) => return Err(error),
                }
            }
            Ok(controls)
        })
        .await
    }
    pub async fn control(&self, id: ControlId) -> CameraResult<ControlReadback> {
        let actual = self
            .with_native(move |c| {
                let (raw, _) = mapping(c.backend_type(), id)?;
                decode(c.backend_type(), id, c.get_control(raw)?)
            })
            .await;
        match actual {
            Ok(v) => Ok(ControlReadback::Actual(v)),
            Err(CameraError::NotReadable(_)) => Ok(self
                .requested_control(id)
                .map_or(ControlReadback::Unknown, ControlReadback::Requested)),
            Err(e) => Err(e),
        }
    }
    pub async fn set_control(&self, id: ControlId, value: ControlValue) -> CameraResult<()> {
        let requested = value.clone();
        self.with_native(move |c| {
            let backend = c.backend_type();
            let (raw, unit) = mapping(backend, id)?;
            let desc = descriptor(&c, id)?;
            if desc.writable == Some(false) {
                return Err(CameraError::ControlNotSupported("Read-only control".into()));
            }
            let raw_value = match value {
                ControlValue::Mode(mode) if unit == ControlUnit::Mode => {
                    if !desc.modes.contains(&mode) {
                        return Err(CameraError::InvalidConfig("Invalid control mode".into()));
                    }
                    #[cfg(all(
                        any(feature = "native", feature = "backend-camera2"),
                        target_os = "android"
                    ))]
                    #[allow(irrefutable_let_patterns)]
                    if let BackendCamera::Camera2(native) = &c {
                        let value = native
                            .supported_modes(raw)?
                            .into_iter()
                            .find(|&value| camera2_mode(id, value) == Some(mode))
                            .ok_or_else(|| {
                                CameraError::ControlNotSupported("Camera2 mode".into())
                            })?;
                        return c.set_control(raw, RawValue::manual(value));
                    }
                    if backend == BackendType::AVFoundation {
                        RawValue::manual(match mode {
                            ControlMode::Locked => 0,
                            ControlMode::Single => 1,
                            ControlMode::Continuous => 2,
                            _ => unreachable!(),
                        })
                    } else {
                        RawValue::Boolean(mode == ControlMode::Automatic)
                    }
                }
                ControlValue::Number(v) if unit != ControlUnit::Mode => {
                    if !v.is_finite()
                        || v.fract() != 0.0
                        || v < i32::MIN as f64
                        || v > i32::MAX as f64
                    {
                        return Err(CameraError::InvalidConfig(
                            "Native control expects a finite representable integer".into(),
                        ));
                    }
                    if let Some(range) = desc.range {
                        if v < range.min
                            || v > range.max
                            || range
                                .step
                                .is_some_and(|step| step > 0.0 && (v - range.min) % step != 0.0)
                        {
                            return Err(CameraError::InvalidConfig(
                                "Control value outside native range/step".into(),
                            ));
                        }
                    }
                    RawValue::manual(v as i32)
                }
                _ => {
                    return Err(CameraError::InvalidConfig(
                        "Control value kind differs from descriptor".into(),
                    ))
                }
            };
            c.set_control(raw, raw_value)
        })
        .await?;
        self.record_control(id, requested);
        Ok(())
    }
    /// Apply every item and retain every failure. This operation is not atomic.
    pub async fn set_controls(
        &self,
        values: impl IntoIterator<Item = (ControlId, ControlValue)>,
    ) -> Vec<ControlOutcome> {
        let mut outcomes = Vec::new();
        for (id, value) in values {
            outcomes.push(ControlOutcome {
                id,
                result: self.set_control(id, value).await,
            });
        }
        outcomes
    }
    pub async fn reset_controls(&self) -> CameraResult<Vec<ControlOutcome>> {
        let controls = self.controls().await?;
        let mut outcomes = Vec::new();
        for desc in controls {
            let result = if let Some(value) = desc.range.and_then(|r| r.default) {
                self.set_control(desc.id, value).await
            } else {
                Err(CameraError::ControlNotSupported(
                    "Native default is unavailable".into(),
                ))
            };
            outcomes.push(ControlOutcome {
                id: desc.id,
                result,
            });
        }
        Ok(outcomes)
    }
    /// Set a representable exposure duration. Automatic exposure must be disabled separately.
    pub async fn set_exposure_time(&self, duration: std::time::Duration) -> CameraResult<()> {
        let unit = self
            .controls()
            .await?
            .into_iter()
            .find(|c| c.id == ControlId::ExposureTime)
            .ok_or_else(|| CameraError::ControlNotSupported("Exposure duration".into()))?
            .unit;
        let value = match unit {
            ControlUnit::HundredMicroseconds => duration.as_secs_f64() * 10_000.0,
            ControlUnit::Log2Seconds => duration.as_secs_f64().log2(),
            _ => {
                return Err(CameraError::ControlNotSupported(
                    "Exposure duration unit".into(),
                ))
            }
        };
        if !value.is_finite() || (value - value.round()).abs() > 1e-6 {
            return Err(CameraError::InvalidConfig(
                "Duration is not representable in the native control unit".into(),
            ));
        }
        self.set_control(ControlId::ExposureTime, ControlValue::Number(value.round()))
            .await
    }

    pub async fn set_exposure(&self, exposure: Exposure) -> CameraResult<()> {
        match exposure {
            Exposure::Auto => {
                self.set_control(
                    ControlId::ExposureMode,
                    ControlValue::Mode(ControlMode::Automatic),
                )
                .await
            }
            Exposure::Manual(duration) => {
                self.set_control(
                    ControlId::ExposureMode,
                    ControlValue::Mode(ControlMode::Manual),
                )
                .await?;
                self.set_exposure_time(duration).await
            }
        }
    }

    pub async fn set_white_balance(&self, value: WhiteBalance) -> CameraResult<()> {
        match value {
            WhiteBalance::Auto => {
                self.set_control(
                    ControlId::WhiteBalanceMode,
                    ControlValue::Mode(ControlMode::Automatic),
                )
                .await
            }
            WhiteBalance::Temperature(kelvin) => {
                self.set_control(
                    ControlId::WhiteBalanceMode,
                    ControlValue::Mode(ControlMode::Manual),
                )
                .await?;
                self.set_control(
                    ControlId::WhiteBalanceTemperature,
                    ControlValue::Number(f64::from(kelvin.get())),
                )
                .await
            }
        }
    }

    pub async fn set_focus(&self, value: Focus) -> CameraResult<()> {
        match value {
            Focus::Auto => {
                self.set_control(
                    ControlId::FocusMode,
                    ControlValue::Mode(ControlMode::Automatic),
                )
                .await
            }
            Focus::Position(position) => {
                self.set_control(
                    ControlId::FocusMode,
                    ControlValue::Mode(ControlMode::Manual),
                )
                .await?;
                self.set_control(ControlId::FocusPosition, ControlValue::Number(position))
                    .await
            }
        }
    }
}

#[cfg(any(
    test,
    all(
        any(feature = "native", feature = "backend-camera2"),
        target_os = "android"
    )
))]
fn camera2_mode(id: ControlId, value: i32) -> Option<ControlMode> {
    match (id, value) {
        (ControlId::FocusMode, 0) => Some(ControlMode::Manual),
        (ControlId::FocusMode, 1) => Some(ControlMode::Single),
        (ControlId::FocusMode, 3 | 4) => Some(ControlMode::Continuous),
        (ControlId::ExposureMode | ControlId::WhiteBalanceMode, 0) => Some(ControlMode::Manual),
        (ControlId::ExposureMode | ControlId::WhiteBalanceMode, 1) => Some(ControlMode::Automatic),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn camera2_focus_modes_preserve_single_and_continuous_semantics() {
        assert_eq!(
            camera2_mode(ControlId::FocusMode, 1),
            Some(ControlMode::Single)
        );
        assert_eq!(
            camera2_mode(ControlId::FocusMode, 3),
            Some(ControlMode::Continuous)
        );
        assert_eq!(
            camera2_mode(ControlId::ExposureMode, 1),
            Some(ControlMode::Automatic)
        );
        assert_eq!(camera2_mode(ControlId::WhiteBalanceMode, 8), None);
    }

    #[test]
    fn camera2_limits_do_not_claim_a_device_default() {
        let range = describe_range(
            BackendType::Camera2,
            ControlId::Gain,
            Raw::Gain,
            crate::CameraControlRange {
                min: 100,
                max: i32::MAX,
                step: 0,
                default: 0,
                supports_auto: false,
            },
        );
        assert_eq!(range.min, 100.0);
        assert_eq!(range.max, i32::MAX as f64);
        assert!(range.default.is_none());
        assert!(range.step.is_none());
    }
}
