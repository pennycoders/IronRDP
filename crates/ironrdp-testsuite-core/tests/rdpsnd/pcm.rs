use ironrdp_rdpsnd::pcm::PcmDecimator;
use ironrdp_rdpsnd::pdu::{AudioFormat, WaveFormat};

const RATE: u32 = 44100;

fn pcm(n_channels: u16, n_samples_per_sec: u32) -> AudioFormat {
    AudioFormat {
        format: WaveFormat::PCM,
        n_channels,
        n_samples_per_sec,
        n_avg_bytes_per_sec: n_samples_per_sec * u32::from(n_channels) * 2,
        n_block_align: n_channels * 2,
        bits_per_sample: 16,
        data: None,
    }
}

fn decimator(factor: u32) -> PcmDecimator {
    PcmDecimator::new(&pcm(2, RATE), &pcm(2, RATE / factor)).expect("supported conversion")
}

fn stereo(frames: impl IntoIterator<Item = (i16, i16)>) -> Vec<u8> {
    frames
        .into_iter()
        .flat_map(|(left, right)| [left.to_le_bytes(), right.to_le_bytes()])
        .flatten()
        .collect()
}

fn left_channel(pcm: &[u8]) -> Vec<i16> {
    pcm.chunks_exact(4)
        .map(|frame| i16::from_le_bytes([frame[0], frame[1]]))
        .collect()
}

fn sine(frequency: f64, amplitude: f64, frames: u32) -> Vec<u8> {
    stereo((0..frames).map(|n| {
        let phase = 2.0 * core::f64::consts::PI * frequency * f64::from(n) / f64::from(RATE);
        // The amplitude stays within i16 range.
        #[expect(
            clippy::cast_possible_truncation,
            clippy::as_conversions,
            reason = "amplitude <= i16::MAX"
        )]
        let sample = (amplitude * phase.sin()) as i16;
        (sample, sample)
    }))
}

fn convert(decimator: &mut PcmDecimator, input: &[u8]) -> Vec<u8> {
    let mut output = Vec::new();
    decimator.process(input, &mut output);
    output
}

/// Peak level once the filters have settled.
fn settled_peak(pcm: &[u8]) -> f64 {
    left_channel(pcm)
        .into_iter()
        .skip(32)
        .map(|sample| f64::from(sample).abs())
        .fold(0.0, f64::max)
}

#[test]
fn output_format_divides_the_rate() {
    assert_eq!(PcmDecimator::output_format(&pcm(2, 44100), 2), Some(pcm(2, 22050)));
    assert_eq!(PcmDecimator::output_format(&pcm(1, 48000), 4), Some(pcm(1, 12000)));
}

#[test]
fn output_format_rejects_unsupported_input() {
    let mut adpcm = pcm(2, 44100);
    adpcm.format = WaveFormat::ADPCM;
    let mut eight_bit = pcm(2, 44100);
    eight_bit.bits_per_sample = 8;

    assert_eq!(PcmDecimator::output_format(&adpcm, 2), None);
    assert_eq!(PcmDecimator::output_format(&eight_bit, 2), None);
    assert_eq!(PcmDecimator::output_format(&pcm(2, 44100), 3), None);
    assert_eq!(PcmDecimator::output_format(&pcm(2, 22050), 4), None);
}

#[test]
fn new_accepts_only_supported_fractions() {
    assert!(PcmDecimator::new(&pcm(2, 44100), &pcm(2, 22050)).is_some());
    assert!(PcmDecimator::new(&pcm(2, 44100), &pcm(2, 11025)).is_some());
    assert!(PcmDecimator::new(&pcm(2, 44100), &pcm(2, 44100)).is_none());
    assert!(PcmDecimator::new(&pcm(2, 44100), &pcm(1, 22050)).is_none());
    assert!(PcmDecimator::new(&pcm(2, 48000), &pcm(2, 22050)).is_none());
    assert!(PcmDecimator::new(&pcm(0, 44100), &pcm(0, 22050)).is_none());
}

#[test]
fn divides_the_number_of_frames() {
    let input = sine(440.0, 1000.0, 4400);
    assert_eq!(convert(&mut decimator(2), &input).len(), 2200 * 4);
    assert_eq!(convert(&mut decimator(4), &input).len(), 1100 * 4);
}

#[test]
fn block_sizes_do_not_change_the_output() {
    let input = sine(1000.0, 8000.0, 1003);
    for factor in PcmDecimator::FACTORS {
        let whole = convert(&mut decimator(factor), &input);

        let mut blocks = decimator(factor);
        let mut pieces = Vec::new();
        for block in input.chunks(7 * 4) {
            blocks.process(block, &mut pieces);
        }

        assert_eq!(whole, pieces, "factor {factor}");
    }
}

#[test]
fn a_constant_level_passes_unchanged() {
    let input = stereo(core::iter::repeat_n((1234, -4321), 400));
    for factor in PcmDecimator::FACTORS {
        let output = convert(&mut decimator(factor), &input);
        let settled: Vec<_> = output.chunks_exact(4).skip(32).collect();
        assert!(!settled.is_empty());
        for frame in settled {
            assert_eq!(frame, stereo([(1234, -4321)]), "factor {factor}");
        }
    }
}

#[test]
fn speech_frequencies_keep_their_level() {
    for frequency in [200.0, 1000.0, 4000.0] {
        let level = settled_peak(&convert(&mut decimator(2), &sine(frequency, 10_000.0, 8820)));
        assert!((level - 10_000.0).abs() < 200.0, "{frequency} Hz came out at {level}");
    }
}

#[test]
fn frequencies_above_the_new_limit_are_removed() {
    // At 22.05 kHz, 18 kHz would fold back to 4.05 kHz.
    let level = settled_peak(&convert(&mut decimator(2), &sine(18_000.0, 10_000.0, 8820)));
    assert!(level < 100.0, "18 kHz leaked through at {level}");

    // At 11.025 kHz, 9 kHz would fold back to 2.025 kHz.
    let level = settled_peak(&convert(&mut decimator(4), &sine(9_000.0, 10_000.0, 8820)));
    assert!(level < 100.0, "9 kHz leaked through at {level}");
}

#[test]
fn full_scale_input_saturates() {
    let square: Vec<i16> = (0..2000)
        .map(|n| if (n / 20) % 2 == 0 { i16::MAX } else { i16::MIN })
        .collect();
    let output = left_channel(&convert(&mut decimator(2), &stereo(square.iter().map(|&s| (s, s)))));

    let taps: [f64; 15] = [
        -120.0, 0.0, 530.0, 0.0, -2242.0, 0.0, 9993.0, 16446.0, 9993.0, 0.0, -2242.0, 0.0, 530.0, 0.0, -120.0,
    ];
    let mut history = [0.0; 15];
    let mut expected = Vec::new();
    for (n, &sample) in square.iter().enumerate() {
        history.copy_within(1.., 0);
        history[14] = f64::from(sample);
        if n % 2 == 0 {
            let acc: f64 = taps.iter().zip(&history).map(|(tap, h)| tap * h).sum();
            expected.push((acc / 32768.0).round().clamp(-32768.0, 32767.0));
        }
    }

    assert_eq!(output.len(), expected.len());
    for (got, want) in output.iter().zip(&expected) {
        assert!((f64::from(*got) - want).abs() <= 1.0, "got {got}, want {want}");
    }
}

#[test]
fn mono_is_supported() {
    let mut decimator = PcmDecimator::new(&pcm(1, RATE), &pcm(1, RATE / 2)).expect("supported conversion");
    let input: Vec<u8> = (0..100i16).flat_map(i16::to_le_bytes).collect();
    assert_eq!(convert(&mut decimator, &input).len(), 50 * 2);
}

#[test]
fn a_trailing_partial_frame_is_ignored() {
    let mut input = sine(440.0, 1000.0, 10);
    input.extend_from_slice(&[1, 2, 3]);
    assert_eq!(convert(&mut decimator(2), &input).len(), 5 * 4);
}
