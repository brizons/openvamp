//! F2 acceptance, pinned in its own words: "An arrangement `Intro, Verse,
//! Chorus, Chorus, Outro` renders the chorus twice with no gap or click at
//! the seam."
//!
//! `render.rs` (`jump_seams_crossfade_with_equal_power_and_contiguous_seams_are_copies`)
//! already checks every frame of this arrangement against the crossfade
//! formula. This test adds a tone whose phase differs at the chorus end and
//! start, so a hard cut there would be heard as a click.

mod common;

use std::f64::consts::FRAC_PI_2;

use common::{bits, four_sections, mono, plan, sine};
use domain::StemRole;
use engine::{ClickSettings, MixSettings, render_offline};

/// How many frames each side of a seam are compared.
const AROUND: u64 = 2_000;

#[test]
fn an_arrangement_intro_verse_chorus_chorus_outro_renders_the_chorus_twice_with_no_gap_or_click_at_the_seam()
 {
    let song = four_sections(vec![("tone", StemRole::Track)]).build();
    let plan = plan(&song, "f23");
    // 441.0625 Hz at 48 kHz: a quarter cycle at the chorus end (20 s) and
    // three quarters at its start (12 s), so a hard cut would jump from +0.5
    // to -0.5.
    let source = sine(441.0625, 48_000, 1_152_000);
    let settings = MixSettings {
        click: ClickSettings {
            enabled: false,
            ..ClickSettings::default()
        },
        ..MixSettings::default()
    };
    let output = render_offline(
        &plan,
        &[mono("tone", StemRole::Track, source.clone())],
        &settings,
    )
    .expect("the plan renders");
    let left: Vec<f32> = output.iter().step_by(2).copied().collect();
    let right: Vec<f32> = output.iter().skip(1).step_by(2).copied().collect();
    assert_eq!(bits(&left), bits(&right));

    // The chorus plays twice, back to back, from the same source range.
    let order: Vec<&str> = plan
        .segments
        .iter()
        .map(|s| s.section_id.as_str())
        .collect();
    assert_eq!(order, ["intro", "verse", "chorus", "chorus", "outro"]);
    let (first, second) = (&plan.segments[2], &plan.segments[3]);
    assert_eq!((first.src_start, first.len), (576_000, 384_000));
    assert_eq!((second.src_start, second.len), (576_000, 384_000));

    // No gap: the sections play end to end, and the render is that long.
    for pair in plan.segments.windows(2) {
        assert_eq!(pair[1].out_start, pair[0].out_start + pair[0].len);
    }
    assert_eq!(plan.length, 192_000 + 384_000 + 2 * 384_000 + 192_000);
    assert_eq!(left.len() as u64, plan.length);

    // Around every seam the output is the source at the section boundaries,
    // sample for sample: the outgoing section's last frames, then the
    // incoming section's first frames. Only the repeat crossfades.
    for pair in plan.segments.windows(2) {
        let (outgoing, incoming) = (&pair[0], &pair[1]);
        let seam = incoming.out_start;
        for i in 1..=AROUND {
            let expected = source[(outgoing.src_start + outgoing.len - i) as usize];
            assert_eq!(left[(seam - i) as usize].to_bits(), expected.to_bits());
        }
        for i in incoming.fade_in..AROUND {
            let expected = source[(incoming.src_start + i) as usize];
            assert_eq!(left[(seam + i) as usize].to_bits(), expected.to_bits());
        }
        for i in 0..incoming.fade_in {
            let tail = f64::from(source[(outgoing.src_start + outgoing.len + i) as usize]);
            let head = f64::from(source[(incoming.src_start + i) as usize]);
            let t = (i as f64 + 0.5) / incoming.fade_in as f64;
            let expected = tail * (FRAC_PI_2 * t).cos() + head * (FRAC_PI_2 * t).sin();
            assert!((f64::from(left[(seam + i) as usize]) - expected).abs() < 1e-6);
        }
    }
    let fades: Vec<u64> = plan.segments.iter().map(|s| s.fade_in).collect();
    assert_eq!(
        fades,
        [0, 0, 0, 256, 0],
        "only the repeat jumps in the source"
    );

    // No click: across the repeat no frame steps further than the tone
    // itself does (with room for the crossfade), where a cut would step by
    // about 1.0.
    let step = |samples: &[f32], n: usize| (samples[n + 1] - samples[n]).abs();
    let tone_step = (0..source.len() - 1)
        .map(|n| step(&source, n))
        .fold(0.0f32, f32::max);
    let cut = (source[576_000] - source[959_999]).abs();
    assert!(cut > 30.0 * tone_step, "a hard cut would click: {cut}");
    let seam = second.out_start as usize;
    for n in seam - AROUND as usize..seam + AROUND as usize {
        assert!(
            step(&left, n) <= 2.0 * tone_step,
            "frame {n} steps by {}, the tone by at most {tone_step}",
            step(&left, n)
        );
    }
}
