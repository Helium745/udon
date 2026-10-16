//! ADPCM decoding support for `WavPlayer`.
//!
//! Supports IMA ADPCM (format tag 0x0011) and Microsoft ADPCM (format tag 0x0002), both with 4 bits per sample.
//!
//! Decoding is done on the fly, one block at a time, into a buffer which is allocated once on construction.
//! This means that no allocations happen during playback, and the compressed file contents are never expanded
//! into memory all at once.

mod ima;
mod ms;

use super::{Error, Format};
use crate::source::Sample;

/// Stateful decoder for the ADPCM-encoded data section of a .wav file.
#[derive(Clone, Debug)]
#[cfg_attr(feature = "serde-derives", derive(serde::Serialize, serde::Deserialize))]
pub(super) struct Decoder {
    kind: Kind,
    channels: usize,
    block_align: usize,
    data_start: usize,
    total_frames: usize,

    next_block: usize,
    frames_remaining: usize,

    /// Interleaved samples of the most recently decoded block
    block: Box<[Sample]>,
    block_len: usize,
    block_pos: usize,
}

#[derive(Clone, Debug)]
#[cfg_attr(feature = "serde-derives", derive(serde::Serialize, serde::Deserialize))]
enum Kind {
    Ima,
    Ms { coefs: Box<[[i32; 2]]> },
}

impl Decoder {
    /// Creates a decoder from the contents of the "fmt " chunk.
    ///
    /// `data_start` and `data_len` describe the location of the "data" chunk within the file,
    /// and `fact_frames` is the frame count from the "fact" chunk, if there is one.
    pub fn new(
        format: Format,
        fmt: &[u8],
        channels: u16,
        data_start: usize,
        data_len: usize,
        fact_frames: Option<usize>,
    ) -> Result<Self, Error> {
        let channels = usize::from(channels);
        let block_align = usize::from(u16::from_le_bytes([fmt[12], fmt[13]]));
        let header_samples_per_block = fmt.get(18..20).map(|x| usize::from(u16::from_le_bytes([x[0], x[1]])));

        let kind = match format {
            Format::ImaAdpcm => Kind::Ima,
            Format::MsAdpcm => Kind::Ms { coefs: ms::read_coefs(fmt).ok_or(Error::MalformedData)? },
            _ => return Err(Error::UnknownFormat),
        };

        let samples_per_block = kind.frames_in_block(block_align, channels);
        if samples_per_block < kind.min_frames() {
            return Err(Error::MalformedData)
        }
        // Some encoders pad blocks, so a smaller value in the header is allowed, but a bigger one can't be right.
        let samples_per_block = match header_samples_per_block {
            Some(n) if n > samples_per_block => return Err(Error::MalformedData),
            Some(n) if n >= kind.min_frames() => n,
            _ => samples_per_block,
        };

        let full_blocks = data_len / block_align;
        let last_block_frames = match data_len % block_align {
            0 => 0,
            n => kind.frames_in_block(n, channels).min(samples_per_block),
        };
        let data_frames = full_blocks * samples_per_block + last_block_frames;
        let total_frames = fact_frames.map_or(data_frames, |n| n.min(data_frames));

        Ok(Self {
            kind,
            channels,
            block_align,
            data_start,
            total_frames,
            next_block: data_start,
            frames_remaining: total_frames,
            block: vec![0.0; samples_per_block * channels].into_boxed_slice(),
            block_len: 0,
            block_pos: 0,
        })
    }

    /// Returns the total number of samples (not frames) which this decoder will output
    pub fn length(&self) -> usize {
        self.total_frames * self.channels
    }

    /// Writes decoded samples to `buffer`, given the full (truncated) file contents.
    pub fn write_samples(&mut self, file: &[u8], buffer: &mut [Sample]) -> usize {
        let mut written = 0;
        while written < buffer.len() {
            if self.block_pos == self.block_len && !self.decode_next_block(file) {
                break
            }
            let count = (buffer.len() - written).min(self.block_len - self.block_pos);
            buffer[written..(written + count)].copy_from_slice(&self.block[self.block_pos..(self.block_pos + count)]);
            written += count;
            self.block_pos += count;
        }
        written
    }

    pub fn reset(&mut self) {
        self.next_block = self.data_start;
        self.frames_remaining = self.total_frames;
        self.block_len = 0;
        self.block_pos = 0;
    }

    fn decode_next_block(&mut self, file: &[u8]) -> bool {
        let start = self.next_block;
        let end = file.len().min(start + self.block_align);
        let data = match file.get(start..end) {
            Some(data) if self.frames_remaining > 0 => data,
            _ => return false,
        };

        let frames = match &self.kind {
            Kind::Ima => ima::decode_block(data, self.channels, &mut self.block),
            Kind::Ms { coefs } => ms::decode_block(data, self.channels, coefs, &mut self.block),
        };
        let frames = frames.min(self.frames_remaining);
        if frames == 0 {
            // Malformed block: stop here so that subsequent calls consistently return 0
            self.frames_remaining = 0;
            return false
        }

        self.next_block = end;
        self.frames_remaining -= frames;
        self.block_len = frames * self.channels;
        self.block_pos = 0;
        true
    }
}

impl Kind {
    /// The number of complete frames which can be decoded from a block of `len` bytes
    fn frames_in_block(&self, len: usize, channels: usize) -> usize {
        match self {
            Kind::Ima => ima::frames_in_block(len, channels),
            Kind::Ms { .. } => ms::frames_in_block(len, channels),
        }
    }

    /// The number of frames stored in a block's header, which every block must contain
    fn min_frames(&self) -> usize {
        match self {
            Kind::Ima => 1,
            Kind::Ms { .. } => 2,
        }
    }
}

#[inline(always)]
fn to_sample(sample: i32) -> Sample {
    f32::from(sample as i16) / f32::from(i16::MAX)
}

#[inline(always)]
fn clamp_i16(sample: i32) -> i32 {
    sample.max(i32::from(i16::MIN)).min(i32::from(i16::MAX))
}
