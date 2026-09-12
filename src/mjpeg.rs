use crate::{CameraError, CameraResult};
use turbojpeg::Decompressor;

fn complete_frame(data: &[u8]) -> CameraResult<&[u8]> {
    if data.len() < 4 || !data.starts_with(&[0xff, 0xd8]) {
        return Err(CameraError::invalid_frame(
            "MJPEG frame is missing its JPEG start marker".into(),
        ));
    }
    if data.ends_with(&[0xff, 0xd9]) {
        return Ok(data);
    }
    let end = data
        .windows(2)
        .rposition(|marker| marker == [0xff, 0xd9])
        .map(|position| position + 2)
        .ok_or_else(|| {
            CameraError::invalid_frame("MJPEG frame is missing its JPEG end marker".into())
        })?;
    Ok(&data[..end])
}

#[allow(dead_code)]
pub(crate) fn validate(data: &[u8]) -> CameraResult<()> {
    complete_frame(data).map(|_| ())
}

/// Reuse a decoder on the healthy path, but discard it after libjpeg rejects a
/// frame. Recreating only after an error avoids carrying decoder state across a
/// broken USB transfer without adding allocations to normal capture.
pub(crate) fn decode_with<T>(
    decoder: &mut Option<Decompressor>,
    data: &[u8],
    decode: impl FnOnce(&mut Decompressor, &[u8]) -> CameraResult<T>,
) -> CameraResult<T> {
    let complete = complete_frame(data)?;
    if decoder.is_none() {
        *decoder = Some(
            Decompressor::new().map_err(|error| CameraError::invalid_frame(error.to_string()))?,
        );
    }
    let result = decode(decoder.as_mut().unwrap(), complete);
    if result.is_err() {
        *decoder = None;
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_non_jpeg_payload_without_allocating_a_decoder() {
        let mut decoder = None;
        assert!(decode_with(&mut decoder, &[0; 32], |_, _| Ok(())).is_err());
        assert!(decoder.is_none());
    }

    #[test]
    fn rejects_a_truncated_jpeg_before_allocating_a_decoder() {
        let mut decoder = None;
        assert!(decode_with(&mut decoder, &[0xff, 0xd8, 1, 2, 3], |_, _| Ok(())).is_err());
        assert!(decoder.is_none());
    }

    #[test]
    fn trims_transport_padding_after_the_last_end_marker() {
        let mut decoder = None;
        let frame = [0xff, 0xd8, 1, 2, 0xff, 0xd9, 0, 0, 0];
        let decoded_length = decode_with(&mut decoder, &frame, |_, complete| Ok(complete.len()))
            .expect("padding after EOI is valid transport data");
        assert_eq!(decoded_length, 6);
    }
}
