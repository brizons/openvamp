//! S3 (F8): the Audio output screen without a window. A song plays on the
//! app's `FakeOutput`; the screen lists the devices with their outputs,
//! shows the rate, the buffer size and its latency, split mode for
//! 2-output devices, and the routing matrix. Every routing change applies
//! live and is saved for its device; every folder is temporary.

use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};

use app::testing::FakeOutput;
use app::{AudioOutput, OutputError, RuntimeError, SessionOptions};
use domain::{
    Arrangement, Bus, Destination, Routing, SectionMarker, SectionType, SongPackage, Stem,
    StemRole, TempoPoint, TimeSignaturePoint,
};
use hound::{SampleFormat, WavSpec, WavWriter};
use library::ConfigStore;
use openvamp_desktop::audio_settings::AudioSettings;
use openvamp_desktop::mixer::{MixerPage, bus_icon, restore_mix, unsaved_mix};
use openvamp_desktop::output_page::{
    DeviceState, OutputAction, OutputPage, OutputScreen, click_cell, problem_notice, rate_note,
};
use openvamp_desktop::show::{Show, Source, replace};

const RATE: u32 = 8_000;

fn write_song(dir: &Path) {
    let bar = 2 * u64::from(RATE);
    let package = SongPackage {
        format_version: 1,
        title: "Rivers Rise".to_owned(),
        artist: None,
        sample_rate: RATE,
        key: None,
        tempo_map: vec![TempoPoint { at: 0, bpm: 120.0 }],
        time_signature_map: vec![TimeSignaturePoint {
            at: 0,
            numerator: 4,
            denominator: 4,
        }],
        stems: vec![Stem {
            id: "keys".to_owned(),
            file: "keys.wav".to_owned(),
            part: "keys".to_owned(),
            role: StemRole::Track,
        }],
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
    };
    fs::create_dir_all(dir).unwrap();
    fs::write(dir.join("song.json"), package.to_json().unwrap()).unwrap();
    let spec = WavSpec {
        channels: 1,
        sample_rate: RATE,
        bits_per_sample: 16,
        sample_format: SampleFormat::Int,
    };
    let mut writer = WavWriter::create(dir.join("keys.wav"), spec).unwrap();
    for frame in 0..bar {
        writer.write_sample((frame % 50) as i16 * 20).unwrap();
    }
    writer.finalize().unwrap();
}

/// How long the helpers wait for the disk thread or a change, at most.
const WAIT: Duration = Duration::from_secs(30);

/// Waits until the disk threads of the song `show` plays have buffered
/// what they can, as the setlist tests do before each callback.
fn wait_disks(show: &Show) {
    let monitors = match show {
        Show::Song(song) => vec![song.session.disk_monitor()],
        Show::Setlist(setlist) => setlist.session.disk_monitors(),
    };
    for monitor in monitors {
        assert!(monitor.wait_idle(WAIT), "the disk thread keeps up");
    }
}

struct Bench {
    dir: tempfile::TempDir,
    fake: FakeOutput,
    show: Option<Show>,
    settings: AudioSettings,
    page: OutputPage,
}

impl Bench {
    /// A laptop (2 outputs, `fake:1`) and an interface (8 outputs,
    /// `fake:2`), with the song open on `device`.
    fn on(device: &str) -> Self {
        Self::with_saved(device, None)
    }

    /// [`Self::on`], with `saved` saved for `device` before the song opens.
    fn with_saved(device: &str, saved: Option<Routing>) -> Self {
        let dir = tempfile::tempdir().unwrap();
        write_song(&dir.path().join("Rivers Rise.song"));
        let fake = FakeOutput::without_devices()
            .with_device("Laptop", 2, None)
            .with_device("Interface", 8, None);
        let mut settings = AudioSettings::load(Some(Self::store(dir.path())));
        settings.choose_device(Some(device));
        if let Some(saved) = saved {
            settings.save_routing(device, &saved).unwrap();
        }
        let mut bench = Self {
            dir,
            fake,
            show: None,
            settings,
            page: OutputPage::new(),
        };
        bench.open();
        bench
    }

    fn store(dir: &Path) -> ConfigStore {
        ConfigStore::new(dir.join("config"))
    }

    fn open(&mut self) {
        let devices = self.fake.devices().unwrap();
        self.page.set_devices(devices.clone());
        let options: SessionOptions = self.settings.options(&devices);
        let source = Source::Song(self.dir.path().join("Rivers Rise.song"));
        let fake = self.fake.clone();
        replace(
            &mut self.show,
            &source,
            &options,
            || Box::new(fake.clone()),
            |_| {},
        )
        .unwrap_or_else(|_| panic!("the song opens"));
    }

    /// Does `action`; reopens the song when the screen asks for it.
    /// Does `action` as the screen does: a song opened again on another
    /// device keeps the mix it had not saved.
    fn act(&mut self, action: OutputAction) {
        let unsaved = self.show.as_ref().and_then(unsaved_mix);
        let reopen = self
            .page
            .act(action, self.show.as_mut(), &mut self.settings);
        if reopen {
            self.open();
            if let (Some(show), Some(unsaved)) = (&mut self.show, &unsaved) {
                restore_mix(show, unsaved);
            }
        }
        if let Some(show) = &mut self.show {
            show.poll();
        }
    }

    /// Plays the song until its transport runs.
    fn play(&mut self) {
        let show = self.show.as_mut().expect("a song is open");
        show.send(openvamp_desktop::action::UiAction::TogglePlay);
        let deadline = Instant::now() + WAIT;
        while Instant::now() < deadline {
            wait_disks(show);
            self.fake.render(256);
            show.poll();
            if show.snapshot().output_frame > 0 {
                return;
            }
        }
        panic!("the transport never started");
    }

    /// Audio callbacks, the song and the screen polled after each as the
    /// screen's timer does, until no change waits for the device; bounded
    /// by time, not by a count of callbacks, since the disk thread may be
    /// slow under load.
    fn settle(&mut self) {
        let deadline = Instant::now() + WAIT;
        while self.view().applying {
            assert!(Instant::now() < deadline, "a change never settled");
            if let Some(show) = &self.show {
                wait_disks(show);
            }
            self.fake.render(256);
            if let Some(show) = &mut self.show {
                show.poll();
            }
            self.page.follow(self.show.as_mut(), &mut self.settings);
        }
    }

    /// The routing saved for `device`, as the settings file holds it.
    fn stored(&self, device: &str) -> Option<Routing> {
        Self::store(self.dir.path())
            .load_routing()
            .routing
            .devices
            .get(device)
            .cloned()
    }

    fn view(&self) -> OutputScreen {
        self.page.view(self.show.as_ref(), &self.settings)
    }

    fn routing(&self) -> Routing {
        self.show
            .as_ref()
            .expect("a song plays")
            .snapshot()
            .mixer
            .routing
            .clone()
    }

    /// The routing the next session opens with on `device`.
    fn saved(&self, device: &str) -> Option<Routing> {
        let mut settings = AudioSettings::load(Some(Self::store(self.dir.path())));
        settings.choose_device(Some(device));
        settings.options(&self.fake.devices().unwrap()).routing
    }
}

fn cells(view: &OutputScreen, bus: Bus) -> Vec<u16> {
    let row = &view.rows[bus.index()];
    assert_eq!(row.bus, bus);
    (1..)
        .zip(&row.cells)
        .filter_map(|(output, &on)| on.then_some(output))
        .collect()
}

#[test]
fn a_click_makes_a_mono_output_a_pair_and_a_third_output_starts_over_on_its_own() {
    let mut routing = Routing::preset(8);
    let at = |routing: &Routing| routing.destination(Bus::Drums);
    assert_eq!(at(&routing), Destination::Mono(2));

    routing = click_cell(&routing, Bus::Drums, 3);
    assert_eq!(at(&routing), Destination::Stereo { left: 2, right: 3 });
    // A third output: the bus plays on it alone.
    routing = click_cell(&routing, Bus::Drums, 6);
    assert_eq!(at(&routing), Destination::Mono(6));
    routing = click_cell(&routing, Bus::Drums, 1);
    assert_eq!(at(&routing), Destination::Stereo { left: 1, right: 6 });
    // One side of a pair off: the other plays alone.
    routing = click_cell(&routing, Bus::Drums, 1);
    assert_eq!(at(&routing), Destination::Mono(6));
    // A bus always plays somewhere: its only output stays.
    routing = click_cell(&routing, Bus::Drums, 6);
    assert_eq!(at(&routing), Destination::Mono(6));
    assert_eq!(
        routing.destination(Bus::Bass),
        Routing::preset(8).destination(Bus::Bass),
        "other buses stay"
    );
}

#[test]
fn a_click_in_split_mode_turns_it_off_and_edits_the_buses_as_stored() {
    let split = Routing {
        split_mode: true,
        ..Routing::preset(2)
    };

    let routing = click_cell(&split, Bus::Vocals, 1);

    assert!(!routing.split_mode);
    assert_eq!(routing.destination(Bus::Vocals), Destination::Mono(2));
    assert_eq!(routing.destination(Bus::Drums), Destination::MAIN);
}

#[test]
fn the_screen_shows_the_device_rate_buffer_latency_and_matrix_of_the_interface() {
    let bench = Bench::on("fake:2");

    let view = bench.view();

    let devices: Vec<(&str, bool)> = view
        .devices
        .iter()
        .map(|device| (device.label.as_str(), device.chosen))
        .collect();
    assert_eq!(
        devices,
        [
            ("Laptop · 2 outputs", false),
            ("Interface · 8 outputs", true)
        ]
    );
    assert_eq!(view.device, "Interface · 8 outputs");
    assert_eq!(view.sample_rate, "8 kHz");
    assert_eq!(view.buffer_sizes, [64, 128, 256, 512, 1024]);
    assert_eq!(view.buffer, 256);
    assert_eq!(view.latency, "32.0 ms", "256 frames at 8 kHz");
    assert_eq!(view.split_mode, None, "only for 2-output devices");
    assert_eq!(view.hint, "Tap a cell to send a bus to an output");
    assert_eq!(view.outputs, 8);
    assert_eq!(view.preset, "Preset: 8 outputs");
    let routed: Vec<Vec<u16>> = Bus::ALL.iter().map(|&bus| cells(&view, bus)).collect();
    assert_eq!(
        routed,
        [vec![1], vec![2], vec![3], vec![5, 6], vec![4], vec![7, 8]]
    );
    assert_eq!(view.notices, Vec::<String>::new());
}

#[test]
fn a_cell_click_applies_live_and_is_saved_for_the_device() {
    let mut bench = Bench::on("fake:2");

    bench.act(OutputAction::Cell {
        bus: Bus::Vocals,
        output: 3,
    });

    assert_eq!(
        bench.routing().destination(Bus::Vocals),
        Destination::Mono(3)
    );
    assert_eq!(cells(&bench.view(), Bus::Vocals), [3]);
    let saved = bench.saved("fake:2").expect("a device is listed");
    assert_eq!(saved.destination(Bus::Vocals), Destination::Mono(3));
    assert_eq!(
        bench.saved("fake:1"),
        Some(Routing::preset(2)),
        "per device"
    );
}

#[test]
fn split_mode_is_offered_for_2_output_devices_and_switches_live() {
    let mut bench = Bench::on("fake:1");
    let view = bench.view();
    assert_eq!(view.split_mode, Some(false), "the 2-output preset: stereo");
    assert_eq!(view.hint, "Tap a cell to send a bus to an output");
    assert_eq!(cells(&view, Bus::Drums), [1, 2]);

    bench.act(OutputAction::ToggleSplit);

    let view = bench.view();
    assert_eq!(view.split_mode, Some(true));
    assert!(bench.routing().split_mode);
    assert_eq!(view.hint, "Split mode is on: only outputs 1 and 2 are used");
    assert_eq!(cells(&view, Bus::ClickGuide), [1]);
    assert_eq!(cells(&view, Bus::Drums), [2]);
    let split = Routing {
        split_mode: true,
        ..Routing::stereo(2)
    };
    assert_eq!(bench.saved("fake:1"), Some(split));

    bench.act(OutputAction::ToggleSplit);

    assert_eq!(bench.view().split_mode, Some(false));
    assert_eq!(cells(&bench.view(), Bus::Drums), [1, 2]);
    assert_eq!(bench.saved("fake:1"), Some(Routing::stereo(2)));
}

#[test]
fn a_preset_for_fewer_outputs_and_reset_back_to_the_devices_own() {
    let mut bench = Bench::on("fake:2");
    bench.act(OutputAction::OpenPresetMenu);
    let view = bench.view();
    assert!(view.preset_menu);
    let presets: Vec<(u16, &str)> = view
        .presets
        .iter()
        .map(|(outputs, label)| (*outputs, label.as_str()))
        .collect();
    assert_eq!(
        presets,
        [
            (1, "1 output"),
            (2, "2 outputs"),
            (3, "3 outputs"),
            (4, "4 outputs"),
            (6, "6 outputs"),
            (8, "8 outputs")
        ]
    );

    bench.act(OutputAction::Preset(4));

    assert!(!bench.view().preset_menu);
    assert_eq!(bench.view().preset, "Preset: 4 outputs");
    assert_eq!(bench.routing(), Routing::preset(4).fit(8));
    assert_eq!(bench.saved("fake:2"), Some(Routing::preset(4).fit(8)));
    bench.act(OutputAction::Reset);
    assert_eq!(bench.routing(), Routing::preset(8));
    assert_eq!(bench.view().preset, "Preset: 8 outputs");
    assert_eq!(bench.saved("fake:2"), Some(Routing::preset(8)));
}

#[test]
fn a_buffer_size_applies_live_and_its_latency_shows() {
    let mut bench = Bench::on("fake:2");

    bench.act(OutputAction::Buffer(128));

    let view = bench.view();
    assert_eq!(view.buffer, 128);
    assert_eq!(view.latency, "16.0 ms");
    assert_eq!(bench.settings.buffer_frames(), 128, "the next session too");
}

#[test]
fn choosing_another_device_reopens_the_song_on_it_with_its_routing() {
    let mut bench = Bench::on("fake:2");
    bench.act(OutputAction::OpenDeviceMenu);
    assert!(bench.view().device_menu);

    bench.act(OutputAction::ChooseDevice(0));

    let view = bench.view();
    assert!(!view.device_menu);
    assert_eq!(view.device, "Laptop · 2 outputs");
    assert_eq!(bench.routing(), Routing::preset(2));
    assert_eq!(view.split_mode, Some(false));
}

#[test]
fn with_nothing_playing_the_routing_of_the_chosen_device_is_edited_and_saved() {
    let mut bench = Bench::on("fake:2");
    bench.show = None;

    let view = bench.view();
    assert_eq!(view.sample_rate, "Set by the song that plays");
    assert_eq!(view.latency, "");
    bench.act(OutputAction::Cell {
        bus: Bus::Bass,
        output: 1,
    });

    assert_eq!(cells(&bench.view(), Bus::Bass), [1, 3]);
    assert_eq!(
        bench.saved("fake:2").unwrap().destination(Bus::Bass),
        Destination::Stereo { left: 1, right: 3 }
    );
}

#[test]
fn a_routing_refitted_and_a_device_with_too_few_outputs_read_as_notices() {
    let refitted = RuntimeError::RoutingRefitted {
        device: "Interface".to_owned(),
        outputs: 4,
    };
    assert_eq!(problem_notice(&refitted), Some(refitted.to_string()));
    let too_few = OutputError::TooFewChannels {
        device: "Interface".to_owned(),
        channels: 2,
        needed: 8,
    };
    assert_eq!(
        problem_notice(&RuntimeError::QueueFull {
            command: "Play".to_owned(),
        }),
        None,
        "only routing problems"
    );
    assert_eq!(
        openvamp_desktop::output_page::output_notice(&too_few),
        Some(too_few.to_string())
    );
}

fn notices_starting(view: &OutputScreen, start: &str) -> usize {
    view.notices
        .iter()
        .filter(|notice| notice.starts_with(start))
        .count()
}

#[test]
fn a_routing_the_device_refuses_is_not_saved_and_says_why() {
    // Saved on outputs 1/2 only, so the stream opens with two channels.
    let mut bench = Bench::with_saved("fake:2", Some(Routing::stereo(8)));
    bench.fake.set_rates("fake:2", Some(&[]));

    // The preset needs all eight: the stream must open again, and cannot.
    bench.act(OutputAction::Reset);

    assert_eq!(
        notices_starting(&bench.view(), "The routing could not be applied: "),
        1,
        "{:?}",
        bench.view().notices
    );
    assert_eq!(bench.saved("fake:2"), Some(Routing::stereo(8)));
}

#[test]
fn a_routing_a_full_queue_holds_back_still_arrives_and_is_saved() {
    let mut bench = Bench::on("fake:2");
    let show = bench.show.as_mut().unwrap();
    let timing = app::Command::SetJumpTiming(app::JumpTiming::SectionEnd);
    while show.send_mixer(timing).is_ok() {}

    bench.act(OutputAction::Cell {
        bus: Bus::Vocals,
        output: 3,
    });

    assert_eq!(bench.view().notices, Vec::<String>::new());
    let saved = bench.saved("fake:2").unwrap();
    assert_eq!(saved.destination(Bus::Vocals), Destination::Mono(3));
}

#[test]
fn a_buffer_size_refused_is_not_kept_and_says_why() {
    let mut bench = Bench::on("fake:2");
    bench.fake.set_buffer_range("fake:2", Some((256, 256)));

    bench.act(OutputAction::Buffer(128));

    assert_eq!(
        bench.settings.buffer_frames(),
        256,
        "the size the device takes"
    );
    let view = bench.view();
    assert_eq!(
        notices_starting(&view, "The buffer size could not be changed: "),
        1,
        "{:?}",
        view.notices
    );

    // A size out of range is refused before the device is asked.
    let mut bench = Bench::on("fake:2");
    bench.act(OutputAction::Buffer(32));
    assert_eq!(bench.settings.buffer_frames(), 256);
    assert_eq!(
        notices_starting(&bench.view(), "The buffer size could not be changed: "),
        1
    );
}

#[test]
fn the_preset_label_says_custom_once_the_routing_is_no_preset() {
    let mut bench = Bench::on("fake:2");
    assert_eq!(bench.view().preset, "Preset: 8 outputs");

    bench.act(OutputAction::Cell {
        bus: Bus::Vocals,
        output: 3,
    });
    assert_eq!(bench.view().preset, "Preset: Custom");
    // Back to the preset's own outputs, step by step.
    for output in [7, 3, 8] {
        bench.act(OutputAction::Cell {
            bus: Bus::Vocals,
            output,
        });
    }
    assert_eq!(bench.view().preset, "Preset: 8 outputs");

    bench.act(OutputAction::Preset(4));
    assert_eq!(bench.view().preset, "Preset: 4 outputs");
    bench.act(OutputAction::Cell {
        bus: Bus::Drums,
        output: 1,
    });
    assert_eq!(bench.view().preset, "Preset: Custom");

    let mut laptop = Bench::on("fake:1");
    assert_eq!(laptop.view().preset, "Preset: 2 outputs");
    laptop.act(OutputAction::ToggleSplit);
    assert_eq!(laptop.view().preset, "Preset: Custom");
    laptop.act(OutputAction::ToggleSplit);
    assert_eq!(laptop.view().preset, "Preset: 2 outputs");
}

#[test]
fn a_mix_not_saved_yet_carries_onto_the_device_the_song_opens_again_on() {
    let mut bench = Bench::on("fake:2");
    let show = bench.show.as_mut().unwrap();
    for command in [
        app::Command::SetStemVolume {
            stem: 0,
            volume: domain::Volume::from_db(-6.0),
        },
        app::Command::SetStemMute {
            stem: 0,
            mute: true,
        },
        app::Command::SetStemBus {
            stem: 0,
            bus: Some(Bus::Vocals),
        },
    ] {
        show.send_mixer(command).unwrap();
    }
    show.poll();
    let mix = show.snapshot().mixer.mix.clone();

    bench.act(OutputAction::ChooseDevice(0));

    let show = bench.show.as_ref().unwrap();
    assert_eq!(bench.view().device, "Laptop · 2 outputs");
    assert_eq!(show.snapshot().mixer.mix, mix);
    assert!(show.snapshot().mixer.mix_changed);
    let mixer = MixerPage::new().view(Some(show)).unwrap();
    assert!(mixer.can_save, "Save mix to arrangement stays offered");
}

#[test]
fn a_buffer_size_refused_once_the_song_has_ramped_out_is_not_kept_and_says_why() {
    let mut bench = Bench::on("fake:2");
    bench.play();
    bench.fake.set_buffer_range("fake:2", Some((256, 256)));

    bench.act(OutputAction::Buffer(128));

    let view = bench.view();
    assert!(
        view.applying,
        "the song ramps out before the device opens again"
    );
    assert_eq!(view.notices, Vec::<String>::new());
    assert_eq!(bench.settings.buffer_frames(), 256, "not kept yet");
    bench.settle();
    let view = bench.view();
    assert!(!view.applying);
    assert_eq!(
        notices_starting(&view, "The buffer size could not be changed: "),
        1,
        "{:?}",
        view.notices
    );
    assert_eq!(bench.settings.buffer_frames(), 256);
}

#[test]
fn a_buffer_size_taken_once_the_song_has_ramped_out_is_kept() {
    let mut bench = Bench::on("fake:2");
    bench.play();

    bench.act(OutputAction::Buffer(512));

    assert!(bench.view().applying);
    assert_eq!(bench.settings.buffer_frames(), 256, "not kept yet");
    bench.settle();
    let view = bench.view();
    assert!(!view.applying);
    assert_eq!((view.buffer, bench.settings.buffer_frames()), (512, 512));
    assert_eq!(view.notices, Vec::<String>::new());
}

#[test]
fn a_routing_needing_more_outputs_the_device_refuses_while_playing_is_not_saved() {
    // Saved on outputs 1/2 only, so the stream opens with two channels.
    let mut bench = Bench::with_saved("fake:2", Some(Routing::stereo(8)));
    bench.play();
    // The device refuses to open again, whatever it is asked.
    bench.fake.set_rates("fake:2", Some(&[]));

    // The preset needs all eight outputs: the stream opens again once the
    // song has ramped out, and the device refuses it.
    bench.act(OutputAction::Reset);

    assert!(bench.view().applying);
    assert_eq!(bench.stored("fake:2"), Some(Routing::stereo(8)));
    bench.settle();
    let view = bench.view();
    assert!(!view.applying);
    assert_eq!(
        notices_starting(&view, "The routing could not be applied: "),
        1,
        "{:?}",
        view.notices
    );
    assert_eq!(bench.stored("fake:2"), Some(Routing::stereo(8)));
}

#[test]
fn a_routing_needing_more_outputs_taken_while_playing_is_saved_once_it_plays() {
    let mut bench = Bench::with_saved("fake:2", Some(Routing::stereo(8)));
    bench.play();

    bench.act(OutputAction::Reset);

    assert!(bench.view().applying);
    assert_eq!(bench.stored("fake:2"), Some(Routing::stereo(8)), "not yet");
    bench.settle();
    assert!(!bench.view().applying);
    assert_eq!(bench.view().notices, Vec::<String>::new());
    assert_eq!(bench.stored("fake:2"), Some(Routing::preset(8)));
}

#[test]
fn a_change_needing_no_reopen_is_kept_while_an_earlier_refusal_holds_the_device() {
    let mut bench = Bench::on("fake:2");
    bench.fake.set_buffer_range("fake:2", Some((256, 256)));
    // Silent: the device is asked at once, refuses, and is held.
    bench.act(OutputAction::Buffer(128));
    assert_eq!(
        notices_starting(&bench.view(), "The buffer size could not be changed: "),
        1
    );

    bench.act(OutputAction::Cell {
        bus: Bus::Vocals,
        output: 3,
    });

    let view = bench.view();
    assert_eq!(
        notices_starting(&view, "The routing could not be applied: "),
        0,
        "{:?}",
        view.notices
    );
    assert_eq!(
        bench
            .stored("fake:2")
            .map(|routing| routing.destination(Bus::Vocals)),
        Some(Destination::Mono(3))
    );
}

#[test]
fn a_routing_for_more_outputs_than_the_device_now_has_plays_refitted_and_is_saved() {
    let mut bench = Bench::with_saved("fake:2", Some(Routing::stereo(8)));
    bench.play();
    // The interface now has two outputs, though the list still says eight.
    bench.fake.set_channels("fake:2", 2);

    bench.act(OutputAction::Reset);
    bench.settle();

    let view = bench.view();
    assert!(!view.applying, "the session took it, fitted to two outputs");
    assert_eq!(
        notices_starting(&view, "The routing could not be applied: "),
        0,
        "{:?}",
        view.notices
    );
    assert_eq!(bench.routing().outputs, 2);
    assert_eq!(
        bench.stored("fake:2"),
        Some(Routing::preset(8)),
        "saved as asked, for when the device has its eight outputs again"
    );
}

#[test]
fn the_device_button_dot_says_whether_the_device_plays_the_song() {
    let mut bench = Bench::on("fake:2");
    assert_eq!(bench.view().device_state, DeviceState::Open);

    bench.fake.unplug("fake:2");
    bench.show.as_mut().unwrap().poll();

    let view = bench.view();
    assert_eq!(view.device_state, DeviceState::Lost);
    assert_eq!(
        view.device, "Interface · 8 outputs",
        "the device it waits for"
    );

    bench.show = None;
    assert_eq!(
        bench.view().device_state,
        DeviceState::Closed,
        "nothing plays: no claim"
    );
}

#[test]
fn the_sample_rate_says_it_matches_the_song_only_while_one_plays() {
    let mut bench = Bench::on("fake:2");

    let view = bench.view();
    assert_eq!(view.sample_rate, "8 kHz");
    assert_eq!(view.sample_rate_note.as_deref(), Some("matches the song"));

    bench.show = None;
    let view = bench.view();
    assert_eq!(view.sample_rate, "Set by the song that plays");
    assert_eq!(view.sample_rate_note, None);
}

#[test]
fn the_rate_note_compares_the_device_with_the_song() {
    assert_eq!(rate_note(48_000, 48_000), "matches the song");
    assert_eq!(rate_note(48_000, 44_100), "the song is 44.1 kHz");
}

#[test]
fn each_matrix_row_carries_its_bus_icon_as_the_mixer_draws_it() {
    let view = Bench::on("fake:2").view();

    let icons: Vec<&str> = view.rows.iter().map(|row| row.icon).collect();
    assert_eq!(
        icons,
        [
            "headphones",
            "drum",
            "bass",
            "piano",
            "electric-guitar",
            "mic"
        ]
    );
    assert!(view.rows.iter().all(|row| row.icon == bus_icon(row.bus)));
}
