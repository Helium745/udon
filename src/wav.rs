mod adpcm;

use crate::source::{ChannelCount, Sample, SampleRate, Source};
use std::sync::Arc;

/// A Source object for decoding and playing samples from a .wav file.
///
/// This type is constructed by passing the entire .wav file contents in as bytes. That is to say, the entire file
/// must be provided at once, and the WavPlayer will take ownership of it.
///
/// Supported encodings are integer PCM (8, 16, 24 or 32 bits), 32-bit float, and 4-bit IMA or Microsoft ADPCM.
/// ADPCM is decoded one block at a time during playback, so it's never fully expanded into memory.
///
/// Once created, the file contents will be atomically reference-counted, so making multiple copies of this type using
/// .clone() is relatively costless. Cloning the player is useful if you intend to play it more than once.
#[derive(Clone, Debug)]
#[cfg_attr(feature = "serde-derives", derive(serde::Serialize, serde::Deserialize))]
pub struct WavPlayer {
    file: Arc<[u8]>,
    channels: ChannelCount,
    sample_rate: SampleRate,
    sample_bytes: usize,
    data_start: usize,
    next_sample_offset: usize,
    format: Format,
    length: usize,
    adpcm: Option<adpcm::Decoder>,
}

#[derive(Clone, Copy, Debug)]
#[cfg_attr(feature = "serde-derives", derive(serde::Serialize, serde::Deserialize))]
pub enum Error {
    /// This does not appear to be a .wav file
    InvalidFile,

    /// The audio data in this file is malformed
    MalformedData,

    /// The audio data in this file is encoded in a way we don't support
    UnknownFormat,
}

#[derive(Clone, Copy, Debug)]
#[cfg_attr(feature = "serde-derives", derive(serde::Serialize, serde::Deserialize))]
pub enum Format {
    U8,
    I16,
    I24,
    I32,
    F32,
    ImaAdpcm,
    MsAdpcm,
}

impl WavPlayer {
    pub fn new(file: impl Into<Vec<u8>>) -> Result<Self, Error> {
        fn find_section(data: &[u8], section_name: &[u8; 4]) -> Option<(usize, usize)> {
            let mut data_start = 12;
            let data_len = loop {
                if data.len() < data_start + 8 {
                    return None;
                }
                let is_data_chunk = data[data_start..(data_start + section_name.len())] == section_name[..];
                let data_len = u32::from_le_bytes([
                    data[data_start + 4],
                    data[data_start + 5],
                    data[data_start + 6],
                    data[data_start + 7],
                ]) as usize;
                data_start += 8;
                if is_data_chunk {
                    break data_len
                } else {
                    data_start += data_len;
                }
            };
            Some((data_start, data_len))
        }

        let mut file = file.into();
        if file.get(0..4) != Some(&[b'R', b'I', b'F', b'F']) || file.get(8..12) != Some(&[b'W', b'A', b'V', b'E']) {
            return Err(Error::InvalidFile)
        }

        let (fmt_start, fmt_len) = find_section(&file, b"fmt ").ok_or(Error::InvalidFile)?;
        if fmt_len < 16 {
            return Err(Error::InvalidFile)
        }
        let fmt = file.get(fmt_start..(fmt_start + fmt_len)).ok_or(Error::InvalidFile)?;

        let audio_format = i16::from_le_bytes([fmt[0], fmt[1]]);
        let channels = u16::from_le_bytes([fmt[2], fmt[3]]);
        let sample_rate = u32::from_le_bytes([fmt[4], fmt[5], fmt[6], fmt[7]]);
        let sample_bits = u16::from_le_bytes([fmt[14], fmt[15]]);
        let ext_guid = fmt.get(24..40);

        let data_format_pcm = &[1, 0, 0, 0, 0, 0, 16, 0, 128, 0, 0, 170, 0, 56, 155, 113];
        let data_format_float = &[3, 0, 0, 0, 0, 0, 16, 0, 128, 0, 0, 170, 0, 56, 155, 113];
        let format = match (audio_format, sample_bits) {
            (1, 8) => Format::U8,
            (1, 16) => Format::I16,
            (1, 24) => Format::I24,
            (1, 32) => Format::I32,
            (3, 32) => Format::F32,
            (0x02, 4) => Format::MsAdpcm,
            (0x11, 4) => Format::ImaAdpcm,
            (-2, 8) if ext_guid == Some(data_format_pcm) => Format::U8,
            (-2, 16) if ext_guid == Some(data_format_pcm) => Format::I16,
            (-2, 24) if ext_guid == Some(data_format_pcm) => Format::I24,
            (-2, 32) if ext_guid == Some(data_format_pcm) => Format::I32,
            (-2, 32) if ext_guid == Some(data_format_float) => Format::F32,
            _ => return Err(Error::UnknownFormat),
        };

        let channels = ChannelCount::new(channels).ok_or(Error::UnknownFormat)?;
        let sample_rate = SampleRate::new(sample_rate).ok_or(Error::UnknownFormat)?;

        let (data_start, data_len) = find_section(&file, b"data").ok_or(Error::InvalidFile)?;

        let expected_file_length = data_len + data_start;
        if expected_file_length > file.len() {
            return Err(Error::MalformedData)
        }

        let adpcm = match format {
            Format::ImaAdpcm | Format::MsAdpcm => {
                let fact_frames = find_section(&file, b"fact")
                    .and_then(|(start, len)| if len >= 4 { file.get(start..(start + 4)) } else { None })
                    .map(|x| u32::from_le_bytes([x[0], x[1], x[2], x[3]]) as usize);
                Some(adpcm::Decoder::new(format, fmt, channels.get(), data_start, data_len, fact_frames)?)
            },
            _ => None,
        };

        file.truncate(expected_file_length);

        let sample_bytes = usize::from(sample_bits / 8);
        let length = match &adpcm {
            Some(decoder) => decoder.length(),
            None => data_len / sample_bytes,
        };

        Ok(Self {
            file: file.into(),
            channels,
            sample_rate,
            sample_bytes,
            data_start,
            next_sample_offset: data_start,
            format,
            length,
            adpcm,
        })
    }

    /// Returns the total number of samples in this wav file
    pub fn length(&self) -> usize {
        self.length
    }
}

impl Source for WavPlayer {
    #[inline]
    fn channel_count(&self) -> ChannelCount {
        self.channels
    }

    #[inline]
    fn sample_rate(&self) -> SampleRate {
        self.sample_rate
    }

    fn write_samples(&mut self, buffer: &mut [Sample]) -> usize {
        use std::convert::TryInto;

        if let Some(decoder) = &mut self.adpcm {
            return decoder.write_samples(&self.file, buffer)
        }

        if let Some(i) = self.file.get(self.next_sample_offset..) {
            let output_iter = buffer.iter_mut();

            let samples_written;
            match self.format {
                Format::U8 => {
                    let iter = output_iter.zip(i.iter().copied());
                    samples_written = iter.len();
                    iter.for_each(|(out, b)| *out = get_sample_u8(b));
                },
                Format::I16 => {
                    let iter =
                        output_iter.zip(i.chunks_exact(2).map(|x| <&[u8] as TryInto<&[u8; 2]>>::try_into(x).unwrap()));
                    samples_written = iter.len();
                    iter.for_each(|(out, b)| *out = get_sample_i16(b));
                },
                Format::I24 => {
                    let iter =
                        output_iter.zip(i.chunks_exact(3).map(|x| <&[u8] as TryInto<&[u8; 3]>>::try_into(x).unwrap()));
                    samples_written = iter.len();
                    iter.for_each(|(out, b)| *out = get_sample_i24(b));
                },
                Format::I32 => {
                    let iter =
                        output_iter.zip(i.chunks_exact(4).map(|x| <&[u8] as TryInto<&[u8; 4]>>::try_into(x).unwrap()));
                    samples_written = iter.len();
                    iter.for_each(|(out, b)| *out = get_sample_i32(b));
                },
                Format::F32 => {
                    let iter =
                        output_iter.zip(i.chunks_exact(4).map(|x| <&[u8] as TryInto<&[u8; 4]>>::try_into(x).unwrap()));
                    samples_written = iter.len();
                    iter.for_each(|(out, b)| *out = get_sample_f32(b));
                },
                Format::ImaAdpcm | Format::MsAdpcm => unreachable!("ADPCM is handled by adpcm::Decoder"),
            }

            self.next_sample_offset += samples_written * self.sample_bytes;
            samples_written
        } else {
            0
        }
    }

    fn reset(&mut self) {
        self.next_sample_offset = self.data_start;
        if let Some(decoder) = &mut self.adpcm {
            decoder.reset();
        }
    }
}

#[inline(always)]
fn get_sample_u8(data: u8) -> f32 {
    let sample = i16::from(data) - 0x80;
    f32::from(sample) / f32::from(i8::MAX)
}

#[inline(always)]
fn get_sample_i16(data: &[u8; 2]) -> f32 {
    let sample = i16::from_le_bytes(*data);
    f32::from(sample) / f32::from(i16::MAX)
}

#[inline(always)]
fn get_sample_i24(data: &[u8; 3]) -> f32 {
    let sample = i32::from_le_bytes([data[0], data[1], data[2], 0]);
    let sign_mask = 1 << 23;
    let sample = (sample ^ sign_mask).wrapping_sub(sign_mask) as i32;
    (f64::from(sample) / 8388608.0) as f32 // 2^23, or the imaginary i24::MAX
}

#[inline(always)]
fn get_sample_i32(data: &[u8; 4]) -> f32 {
    let sample = i32::from_le_bytes(*data);
    (f64::from(sample) / f64::from(i32::MAX)) as f32
}

#[inline(always)]
fn get_sample_f32(data: &[u8; 4]) -> f32 {
    f32::from_le_bytes(*data)
}
