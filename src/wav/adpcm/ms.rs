//! Microsoft ADPCM, format tag 0x0002.

use super::{clamp_i16, to_sample};
use crate::source::Sample;

const ADAPTATION_TABLE: [i32; 16] = [230, 230, 230, 230, 307, 409, 512, 614, 768, 614, 512, 409, 307, 230, 230, 230];

/// Size of each channel's header: u8 predictor index, i16 delta, i16 sample 1, i16 sample 2
const HEADER_SIZE: usize = 7;

/// Reads the predictor coefficient table which follows the samples-per-block field in the "fmt " chunk.
pub(super) fn read_coefs(fmt: &[u8]) -> Option<Box<[[i32; 2]]>> {
    let count = usize::from(u16::from_le_bytes([*fmt.get(20)?, *fmt.get(21)?]));
    let coefs = fmt.get(22..(22 + count * 4))?;
    if count == 0 {
        return None
    }
    Some(
        coefs
            .chunks_exact(4)
            .map(|x| [i32::from(i16::from_le_bytes([x[0], x[1]])), i32::from(i16::from_le_bytes([x[2], x[3]]))])
            .collect(),
    )
}

pub(super) fn frames_in_block(len: usize, channels: usize) -> usize {
    match len.checked_sub(HEADER_SIZE * channels) {
        Some(data_len) => 2 + data_len * 2 / channels,
        None => 0,
    }
}

/// Decodes one block into `out` as interleaved samples, returning the number of frames written.
pub(super) fn decode_block(block: &[u8], channels: usize, coefs: &[[i32; 2]], out: &mut [Sample]) -> usize {
    let frames = frames_in_block(block.len(), channels).min(out.len() / channels);
    if frames < 2 {
        return 0
    }

    // The header fields are each stored for all channels before moving on to the next field
    let (header, data) = block.split_at(HEADER_SIZE * channels);
    let read_i16 = |field_offset: usize, channel: usize| {
        let i = field_offset * channels + channel * 2;
        i32::from(i16::from_le_bytes([header[i], header[i + 1]]))
    };

    for channel in 0..channels {
        let [coef1, coef2] = match coefs.get(usize::from(header[channel])) {
            Some(&coef) => coef,
            None => return 0,
        };
        let mut delta = read_i16(1, channel);
        let mut sample1 = read_i16(3, channel);
        let mut sample2 = read_i16(5, channel);

        // The header samples are output oldest first
        out[channel] = to_sample(sample2);
        out[channels + channel] = to_sample(sample1);

        // Nibbles are high first, and alternate between channels
        for frame in 2..frames {
            let i = (frame - 2) * channels + channel;
            let byte = data[i / 2];
            let nibble = if i % 2 == 0 { byte >> 4 } else { byte & 0x0F };

            let predictor = (sample1 * coef1 + sample2 * coef2) >> 8;
            let signed_nibble = i32::from(nibble) - if nibble & 8 != 0 { 16 } else { 0 };
            let sample = clamp_i16(predictor + signed_nibble * delta);

            sample2 = sample1;
            sample1 = sample;
            delta = ((ADAPTATION_TABLE[usize::from(nibble)] * delta) >> 8).max(16);

            out[frame * channels + channel] = to_sample(sample);
        }
    }
    frames
}
