//! Song folder fixtures, generated inside temporary directories.
#![allow(dead_code)]

use std::fs;
use std::path::Path;

use domain::{
    Arrangement, SectionMarker, SectionType, SongPackage, Stem, StemRole, TempoPoint,
    TimeSignaturePoint,
};
use hound::{SampleFormat, WavSpec, WavWriter};

/// A package with one section at 0, the Default arrangement, and `stems`
/// (`id`, `file`, `role`), at 120 bpm in 4/4.
pub fn manifest(sample_rate: u32, stems: &[(&str, &str, StemRole)]) -> SongPackage {
    SongPackage {
        format_version: 1,
        title: "Library Test".to_owned(),
        artist: None,
        sample_rate,
        key: None,
        tempo_map: vec![TempoPoint { at: 0, bpm: 120.0 }],
        time_signature_map: vec![TimeSignaturePoint {
            at: 0,
            numerator: 4,
            denominator: 4,
        }],
        stems: stems
            .iter()
            .map(|&(id, file, role)| Stem {
                id: id.to_owned(),
                file: file.to_owned(),
                part: id.to_owned(),
                role,
            })
            .collect(),
        sections: vec![SectionMarker {
            id: "intro".to_owned(),
            section_type: SectionType::Intro,
            number: None,
            start: 0,
        }],
        arrangements: vec![Arrangement {
            id: "default".to_owned(),
            name: "Default".to_owned(),
            sections: vec!["intro".to_owned()],
            infinite_loop: Vec::new(),
        }],
    }
}

pub fn write_manifest(dir: &Path, package: &SongPackage) {
    fs::write(dir.join("song.json"), package.to_json().unwrap()).unwrap();
}

fn create(path: &Path, spec: WavSpec) -> WavWriter<std::io::BufWriter<fs::File>> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    WavWriter::create(path, spec).unwrap()
}

/// Writes interleaved integer samples with `bits` bits per sample.
pub fn write_int(path: &Path, bits: u16, channels: u16, sample_rate: u32, samples: &[i32]) {
    let spec = WavSpec {
        channels,
        sample_rate,
        bits_per_sample: bits,
        sample_format: SampleFormat::Int,
    };
    let mut writer = create(path, spec);
    for &sample in samples {
        writer.write_sample(sample).unwrap();
    }
    writer.finalize().unwrap();
}

/// Writes interleaved 32-bit float samples.
pub fn write_float(path: &Path, channels: u16, sample_rate: u32, samples: &[f32]) {
    let spec = WavSpec {
        channels,
        sample_rate,
        bits_per_sample: 32,
        sample_format: SampleFormat::Float,
    };
    let mut writer = create(path, spec);
    for &sample in samples {
        writer.write_sample(sample).unwrap();
    }
    writer.finalize().unwrap();
}

/// Interleaved integer samples with `bits` bits per sample as the bytes of a
/// FLAC file, in blocks of `block_frames` frames. The encoder writes a
/// STREAMINFO with the length and no SEEKTABLE.
///
/// flacenc 0.5.1 records the short last block as the minimum block size, so
/// the stream reads as variable-size blocks numbered as fixed-size ones,
/// which decoders refuse (`flac -t` warns, symphonia finds no frame). As the
/// FLAC format requires, the minimum here leaves the last block out.
pub fn flac_bytes(
    bits: usize,
    channels: usize,
    sample_rate: u32,
    block_frames: usize,
    samples: &[i32],
) -> Vec<u8> {
    use flacenc::component::BitRepr;
    use flacenc::error::Verify;

    let config = flacenc::config::Encoder::default()
        .into_verified()
        .expect("the default encoder configuration is valid");
    let source =
        flacenc::source::MemSource::from_samples(samples, channels, bits, sample_rate as usize);
    let mut stream = flacenc::encode_with_fixed_block_size(&config, source, block_frames)
        .expect("the samples encode");
    stream
        .stream_info_mut()
        .set_block_sizes(block_frames, block_frames)
        .expect("a valid block size");
    let mut sink = flacenc::bitsink::ByteSink::new();
    stream.write(&mut sink).expect("the stream writes");
    sink.as_slice().to_vec()
}

/// Writes interleaved integer samples as a FLAC file in blocks of 4 096
/// frames, the encoder's default.
pub fn write_flac(path: &Path, bits: usize, channels: usize, sample_rate: u32, samples: &[i32]) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(
        path,
        flac_bytes(bits, channels, sample_rate, 4_096, samples),
    )
    .unwrap();
}
