use capture_system_or_process_audio::schema::{ByteOrder, Format, SampleType};
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::Path;

const WAVE_FORMAT_PCM: u16 = 1;
const WAVE_FORMAT_IEEE_FLOAT: u16 = 3;

pub fn encode_wav_bytes(format: &Format, pcm_bytes: &[u8]) -> io::Result<Vec<u8>> {
    let channels = format.channels();
    let sample_rate = format.rate();
    let (format_tag, bits_per_sample): (u16, u16) = match format.sample() {
        SampleType::I16 => (WAVE_FORMAT_PCM, 16),
        SampleType::I24 => (WAVE_FORMAT_PCM, 24),
        SampleType::I32 => (WAVE_FORMAT_PCM, 32),
        SampleType::F32 => (WAVE_FORMAT_IEEE_FLOAT, 32),
    };
    let bytes_per_sample = (bits_per_sample / 8) as usize;
    let block_align = channels.saturating_mul(bits_per_sample / 8);
    let byte_rate = sample_rate.saturating_mul(u32::from(block_align));

    let aligned_len = if block_align > 0 {
        (pcm_bytes.len() / usize::from(block_align)) * usize::from(block_align)
    } else {
        pcm_bytes.len()
    };
    let payload = &pcm_bytes[..aligned_len];
    let data_len = u32::try_from(payload.len())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "WAV payload exceeds 4 GiB"))?;
    let riff_size = 36u32.saturating_add(data_len);

    let mut out = Vec::with_capacity(44 + payload.len());
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&riff_size.to_le_bytes());
    out.extend_from_slice(b"WAVE");

    out.extend_from_slice(b"fmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&format_tag.to_le_bytes());
    out.extend_from_slice(&channels.to_le_bytes());
    out.extend_from_slice(&sample_rate.to_le_bytes());
    out.extend_from_slice(&byte_rate.to_le_bytes());
    out.extend_from_slice(&block_align.to_le_bytes());
    out.extend_from_slice(&bits_per_sample.to_le_bytes());

    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());

    if format.order() == ByteOrder::Little || bytes_per_sample <= 1 {
        out.extend_from_slice(payload);
    } else {
        for chunk in payload.chunks_exact(bytes_per_sample) {
            for &b in chunk.iter().rev() {
                out.push(b);
            }
        }
    }

    Ok(out)
}

pub fn save_wav_file(path: &Path, format: &Format, pcm_bytes: &[u8]) -> io::Result<()> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent)?;
    }
    let wav_bytes = encode_wav_bytes(format, pcm_bytes)?;
    let mut file = File::create(path)?;
    file.write_all(&wav_bytes)?;
    file.flush()
}

pub fn decode_pcm_to_stereo_f32(pcm_bytes: &[u8], format: &Format, out_stereo: &mut Vec<f32>) {
    let channels = usize::from(format.channels().max(1));
    let bps = match format.sample() {
        SampleType::I16 => 2usize,
        SampleType::I24 => 3usize,
        SampleType::I32 | SampleType::F32 => 4usize,
    };
    let frame_stride = channels * bps;
    if frame_stride == 0 {
        return;
    }
    let frames = pcm_bytes.len() / frame_stride;
    out_stereo.reserve(frames * 2);

    for frame in pcm_bytes.chunks_exact(frame_stride) {
        let left = decode_one_sample(&frame[0..bps], format.sample(), format.order());
        let right = if channels >= 2 {
            decode_one_sample(&frame[bps..2 * bps], format.sample(), format.order())
        } else {
            left
        };
        out_stereo.push(left);
        out_stereo.push(right);
    }
}

fn decode_one_sample(bytes: &[u8], sample_type: SampleType, order: ByteOrder) -> f32 {
    let v = match (sample_type, order) {
        (SampleType::F32, ByteOrder::Little) => {
            f32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
        }
        (SampleType::F32, ByteOrder::Big) => {
            f32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
        }
        (SampleType::I16, ByteOrder::Little) => {
            f32::from(i16::from_le_bytes([bytes[0], bytes[1]])) / 32768.0
        }
        (SampleType::I16, ByteOrder::Big) => {
            f32::from(i16::from_be_bytes([bytes[0], bytes[1]])) / 32768.0
        }
        (SampleType::I24, ByteOrder::Little) => {
            let sign = if bytes[2] & 0x80 != 0 { 0xFF } else { 0x00 };
            let raw = i32::from_le_bytes([bytes[0], bytes[1], bytes[2], sign]);
            (raw as f32) / 8_388_608.0
        }
        (SampleType::I24, ByteOrder::Big) => {
            let sign = if bytes[0] & 0x80 != 0 { 0xFF } else { 0x00 };
            let raw = i32::from_be_bytes([sign, bytes[0], bytes[1], bytes[2]]);
            (raw as f32) / 8_388_608.0
        }
        (SampleType::I32, ByteOrder::Little) => {
            (i32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as f64 / 2_147_483_648.0)
                as f32
        }
        (SampleType::I32, ByteOrder::Big) => {
            (i32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as f64 / 2_147_483_648.0)
                as f32
        }
    };
    if v.is_finite() { v } else { 0.0 }
}

#[cfg(test)]
mod tests {
    use super::*;
    use capture_system_or_process_audio::schema::Layout;

    #[test]
    fn encodes_valid_f32_and_i16_riff_wav_headers() {
        let fmt_f32 = Format::new(
            SampleType::F32,
            ByteOrder::Little,
            48_000,
            2,
            Layout::Unknown,
        )
        .expect("valid f32 format");
        let mut raw = Vec::new();
        for i in 0..100 {
            let s = (i as f32) * 0.01;
            raw.extend_from_slice(&s.to_le_bytes());
            raw.extend_from_slice(&(-s).to_le_bytes());
        }
        let wav = encode_wav_bytes(&fmt_f32, &raw).expect("encode wav");
        assert_eq!(&wav[0..4], b"RIFF");
        assert_eq!(&wav[8..12], b"WAVE");
        assert_eq!(&wav[12..16], b"fmt ");
        assert_eq!(
            u16::from_le_bytes([wav[20], wav[21]]),
            WAVE_FORMAT_IEEE_FLOAT
        );
        assert_eq!(u16::from_le_bytes([wav[22], wav[23]]), 2);
        assert_eq!(
            u32::from_le_bytes([wav[24], wav[25], wav[26], wav[27]]),
            48_000
        );
        assert_eq!(&wav[36..40], b"data");
        assert_eq!(
            u32::from_le_bytes([wav[40], wav[41], wav[42], wav[43]]) as usize,
            raw.len()
        );
        assert_eq!(&wav[44..], raw.as_slice());

        let fmt_i16 = Format::new(
            SampleType::I16,
            ByteOrder::Little,
            16_000,
            1,
            Layout::Unknown,
        )
        .expect("valid i16 format");
        let i16_bytes: Vec<u8> = [16384i16, -16384i16]
            .iter()
            .flat_map(|s| s.to_le_bytes())
            .collect();
        let wav_i16 = encode_wav_bytes(&fmt_i16, &i16_bytes).expect("encode i16 wav");
        assert_eq!(
            u16::from_le_bytes([wav_i16[20], wav_i16[21]]),
            WAVE_FORMAT_PCM
        );
        assert_eq!(u16::from_le_bytes([wav_i16[34], wav_i16[35]]), 16);

        let mut stereo = Vec::new();
        decode_pcm_to_stereo_f32(&i16_bytes, &fmt_i16, &mut stereo);
        assert_eq!(stereo.len(), 4);
        assert!((stereo[0] - 0.5).abs() < 1e-4);
        assert!((stereo[1] - 0.5).abs() < 1e-4);
        assert!((stereo[2] + 0.5).abs() < 1e-4);
    }
}
