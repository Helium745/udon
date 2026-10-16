//! IMA ADPCM (also known as DVI ADPCM), format tag 0x0011.

use super::{clamp_i16, to_sample};
use crate::source::Sample;

const INDEX_TABLE: [i32; 16] = [-1, -1, -1, -1, 2, 4, 6, 8, -1, -1, -1, -1, 2, 4, 6, 8];

const STEP_TABLE: [i32; 89] = [
    7, 8, 9, 10, 11, 12, 13, 14, 16, 17, 19, 21, 23, 25, 28, 31, 34, 37, 41, 45, 50, 55, 60, 66, 73, 80, 88, 97, 107,
    118, 130, 143, 157, 173, 190, 209, 230, 253, 279, 307, 337, 371, 408, 449, 494, 544, 598, 658, 724, 796, 876, 963,
    1060, 1166, 1282, 1411, 1552, 1707, 1878, 2066, 2272, 2499, 2749, 3024, 3327, 3660, 4026, 4428, 4871, 5358, 5894,
    6484, 7132, 7845, 8630, 9493, 10442, 11487, 12635, 13899, 15289, 16818, 18500, 20350, 22385, 24623, 27086, 29794,
    32767,
];

/// Size of each channel's header: i16 initial sample, u8 step index, u8 reserved
const HEADER_SIZE: usize = 4;

/// After the header, each channel's data is interleaved in groups of 4 bytes (8 samples)
const GROUP_SIZE: usize = 4;

pub(super) fn frames_in_block(len: usize, channels: usize) -> usize {
    match len.checked_sub(HEADER_SIZE * channels) {
        Some(data_len) => 1 + (data_len / (GROUP_SIZE * channels)) * GROUP_SIZE * 2,
        None => 0,
    }
}

/// Decodes one block into `out` as interleaved samples, returning the number of frames written.
pub(super) fn decode_block(block: &[u8], channels: usize, out: &mut [Sample]) -> usize {
    let frames = frames_in_block(block.len(), channels).min(out.len() / channels);
    if frames == 0 {
        return 0
    }

    let (header, data) = block.split_at(HEADER_SIZE * channels);
    for (channel, header) in header.chunks_exact(HEADER_SIZE).enumerate() {
        let mut predictor = i32::from(i16::from_le_bytes([header[0], header[1]]));
        let mut step_index = i32::from(header[2]).min(88);
        out[channel] = to_sample(predictor);

        // Each group of 4 bytes per channel holds 8 samples, low nibble first
        let groups = data.chunks_exact(GROUP_SIZE).skip(channel).step_by(channels);
        let nibbles = groups.flat_map(|group| group.iter().flat_map(|&b| [b & 0x0F, b >> 4]));
        for (frame, nibble) in (1..frames).zip(nibbles) {
            let sample = expand_nibble(nibble, &mut predictor, &mut step_index);
            out[frame * channels + channel] = to_sample(sample);
        }
    }
    frames
}

#[inline(always)]
fn expand_nibble(nibble: u8, predictor: &mut i32, step_index: &mut i32) -> i32 {
    let step = STEP_TABLE[*step_index as usize];
    let mut diff = step >> 3;
    if nibble & 1 != 0 {
        diff += step >> 2;
    }
    if nibble & 2 != 0 {
        diff += step >> 1;
    }
    if nibble & 4 != 0 {
        diff += step;
    }
    if nibble & 8 != 0 {
        diff = -diff;
    }
    *predictor = clamp_i16(*predictor + diff);
    *step_index = (*step_index + INDEX_TABLE[usize::from(nibble)]).max(0).min(88);
    *predictor
}
