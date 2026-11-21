//! S3 (F8): the Audio output screen without a window. A song plays on the
//! app's `FakeOutput`; the screen lists the devices with their outputs,
//! shows the rate, the buffer size and its latency, split mode for
//! 2-output devices, and the routing matrix. Every routing change applies
//! live and is saved for its device; every folder is temporary.

use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use app::testing::FakeOutput;
use app::{
    AudioOutput, AudioRenderer, DeviceInfo, OutputCheck, OutputError, OutputRequest, OutputStream,
    RuntimeError, SessionOptions, StreamHealth, StreamInfo,
};
use domain::{
    Arrangement, Bus, Destination, Routing, SectionMarker, SectionType, SongPackage, Stem,
    StemRole, TempoPoint, TimeSignaturePoint,
};
use hound::{SampleFormat, WavSpec, WavWriter};
use library::ConfigStore;
use openvamp_desktop::audio_settings::AudioSettings;
use openvamp_desktop::hold::{PANIC_HOLD, PanicHold};
use openvamp_desktop::mixer::{
    BUS_SAVE_DELAY, MixerAction, MixerPage, bus_icon, restore_mix, unsaved_mix,
};
use openvamp_desktop::output_page::{
    APPLY_PATIENCE, DeviceState, OutputAction, OutputPage, OutputScreen, click_cell,
    problem_notice, rate_note, refusal,
};
use openvamp_desktop::show::{Show, Source, replace};
use openvamp_desktop::tick::{Drawn, Timed};

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

/// [`FakeOutput`] as a device that opens buffers of another size than
/// asked, as one that rounds or clamps the size does: every stream opened
/// while `frames` is not 0 plays buffers of that many frames.
struct Rounding {
    fake: FakeOutput,
    frames: Arc<AtomicU32>,
}

/// A stream of [`Rounding`]: the fake's, with the size it plays.
struct Rounded {
    stream: Box<dyn OutputStream>,
    info: StreamInfo,
}

impl OutputStream for Rounded {
    fn info(&self) -> &StreamInfo {
        &self.info
    }

    fn health(&self) -> StreamHealth {
        self.stream.health()
    }
}

impl AudioOutput for Rounding {
    fn devices(&self) -> Result<Vec<DeviceInfo>, OutputError> {
        self.fake.devices()
    }

    fn check(&self, request: &OutputRequest) -> OutputCheck {
        self.fake.check(request)
    }

    fn open(
        &mut self,
        request: &OutputRequest,
        renderer: AudioRenderer,
    ) -> Result<Box<dyn OutputStream>, OutputError> {
        let stream = self.fake.open(request, renderer)?;
        let mut info = stream.info().clone();
        match self.frames.load(Ordering::SeqCst) {
            0 => {}
            frames => info.buffer_frames = frames,
        }
        Ok(Box::new(Rounded { stream, info }))
    }
}

struct Bench {
    dir: tempfile::TempDir,
    fake: FakeOutput,
    /// The size the device plays whatever it is asked ([`Rounding`]); 0
    /// for the size asked.
    rounds_to: Arc<AtomicU32>,
    show: Option<Show>,
    settings: AudioSettings,
    page: OutputPage,
    /// The Mixer screen, for the bus changes a turn of the timer saves.
    mixer: MixerPage,
    /// The Panic control's hold, which a turn of the timer moves on.
    hold: PanicHold,
    /// The screen's clock: it stands still unless a test moves it on, so a
    /// change waits for the device however slow the machine is.
    clock: Instant,
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
            rounds_to: Arc::new(AtomicU32::new(0)),
            show: None,
            settings,
            page: OutputPage::new(),
            mixer: MixerPage::new(),
            hold: PanicHold::new(),
            clock: Instant::now(),
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
        let output = || {
            Box::new(Rounding {
                fake: self.fake.clone(),
                frames: Arc::clone(&self.rounds_to),
            }) as Box<dyn AudioOutput>
        };
        replace(&mut self.show, &source, &options, output, |_| {})
            .unwrap_or_else(|_| panic!("the song opens"));
        // As the screen does once what played has closed.
        self.page.song_closed();
    }

    /// Does `action`; reopens the song when the screen asks for it.
    /// Does `action` as the screen does: a song opened again on another
    /// device keeps the mix it had not saved.
    fn act(&mut self, action: OutputAction) {
        let unsaved = self.show.as_ref().and_then(unsaved_mix);
        let reopen = self
            .page
            .act(action, self.show.as_mut(), &mut self.settings, self.clock);
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
        self.run_until("a change never settled", |bench| !bench.view().applying);
    }

    /// [`Self::settle`] until `done`, else fails saying `never`.
    fn run_until(&mut self, never: &str, done: impl Fn(&Self) -> bool) {
        let deadline = Instant::now() + WAIT;
        while !done(self) {
            assert!(Instant::now() < deadline, "{never}");
            if let Some(show) = &self.show {
                wait_disks(show);
            }
            self.fake.render(256);
            if let Some(show) = &mut self.show {
                show.poll();
            }
            self.page
                .follow(self.show.as_mut(), &mut self.settings, self.clock);
        }
    }

    /// The screen's clock moved on by `by`, and the screen polled once.
    fn later(&mut self, by: Duration) {
        self.clock += by;
        if let Some(show) = &mut self.show {
            show.poll();
        }
        self.page
            .follow(self.show.as_mut(), &mut self.settings, self.clock);
    }

    /// A turn of the screen's timer, its clock moved on by `by`
    /// ([`Timed::turn`]): whether the screen draws again.
    fn turn(&mut self, by: Duration) -> bool {
        self.clock += by;
        Timed {
            show: self.show.as_mut(),
            hold: &mut self.hold,
            mixer: &mut self.mixer,
            output: &mut self.page,
            settings: &mut self.settings,
        }
        .turn(self.clock)
    }

    /// Turns of the timer, the clock standing still, until one draws
    /// nothing; bounded by time, since the disk thread may be slow under
    /// load.
    fn until_still(&mut self) {
        let deadline = Instant::now() + WAIT;
        while self.turn(Duration::ZERO) {
            assert!(Instant::now() < deadline, "the screen never stood still");
            if let Some(show) = &self.show {
                wait_disks(show);
            }
        }
    }

    /// What the screens draw now that a turn can change ([`Drawn`]).
    fn drawn(&self) -> Drawn {
        Drawn::of(
            self.show.as_ref().expect("a song is open"),
            &self.mixer,
            &self.page,
        )
    }

    /// The session's last snapshot.
    fn snapshot(&self) -> app::Snapshot {
        self.show
            .as_ref()
            .expect("a song is open")
            .snapshot()
            .clone()
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

const BUFFER_NOT_KEPT: &str = "The buffer size could not be changed: ";
const ROUTING_NOT_KEPT: &str = "The routing could not be applied: ";

/// The notices that start with `start`.
fn notices_of<'a>(view: &'a OutputScreen, start: &str) -> Vec<&'a str> {
    view.notices
        .iter()
        .map(String::as_str)
        .filter(|notice| notice.starts_with(start))
        .collect()
}

#[test]
fn a_buffer_size_the_device_rounds_is_not_kept_and_says_what_it_plays() {
    let mut bench = Bench::on("fake:2");
    bench.play();
    // From now on the interface plays buffers of 200 frames, whatever it
    // is asked for.
    bench.rounds_to.store(200, Ordering::SeqCst);

    bench.act(OutputAction::Buffer(128));
    bench.settle();

    let view = bench.view();
    assert_eq!(view.buffer, 200, "what the device plays");
    assert_eq!(
        notices_of(&view, BUFFER_NOT_KEPT),
        [
            "The buffer size could not be changed: audio device \"Interface\" plays buffers of 200 frames, not 128"
        ]
    );
    assert_eq!(bench.settings.buffer_frames(), 256, "not kept");
}

#[test]
fn a_buffer_size_the_device_clamps_to_the_size_it_played_is_not_kept_after_the_patience() {
    let mut bench = Bench::on("fake:2");
    bench.play();
    // The interface plays 256 frames whatever it is asked for: the stream
    // it opens again looks the same as the one before.
    bench.rounds_to.store(256, Ordering::SeqCst);

    bench.act(OutputAction::Buffer(128));
    bench.run_until("the stream never opened again", |bench| {
        bench.fake.opened().len() == 2
    });
    bench.later(APPLY_PATIENCE - Duration::from_millis(1));
    assert!(bench.view().applying, "it may still be ramping out");
    bench.later(Duration::from_millis(1));

    let view = bench.view();
    assert!(!view.applying);
    assert_eq!(
        notices_of(&view, BUFFER_NOT_KEPT),
        [
            "The buffer size could not be changed: audio device \"Interface\" plays buffers of 256 frames, not 128"
        ]
    );
    assert_eq!(bench.settings.buffer_frames(), 256);
}

#[test]
fn changes_waiting_when_the_device_is_unplugged_are_not_kept_and_say_why() {
    // Saved on outputs 1/2 only, so the stream opens with two channels.
    let mut bench = Bench::with_saved("fake:2", Some(Routing::stereo(8)));
    bench.play();
    bench.act(OutputAction::Reset);
    bench.act(OutputAction::Buffer(512));
    assert!(bench.view().applying);

    bench.fake.unplug("fake:2");
    bench.settle();

    let view = bench.view();
    let unplugged =
        "audio device \"Interface\" was unplugged; playback is paused until it is plugged back in";
    assert_eq!(
        notices_of(&view, BUFFER_NOT_KEPT),
        [format!("{BUFFER_NOT_KEPT}{unplugged}")]
    );
    assert_eq!(
        notices_of(&view, ROUTING_NOT_KEPT),
        [format!("{ROUTING_NOT_KEPT}{unplugged}")]
    );
    assert_eq!(bench.settings.buffer_frames(), 256);
    assert_eq!(bench.stored("fake:2"), Some(Routing::stereo(8)));
}

#[test]
fn changes_waiting_when_the_song_closes_are_not_kept_and_say_why() {
    let mut bench = Bench::with_saved("fake:2", Some(Routing::stereo(8)));
    bench.play();
    bench.act(OutputAction::Reset);
    bench.act(OutputAction::Buffer(512));
    assert!(bench.view().applying);

    bench.show = None;
    bench.page.follow(None, &mut bench.settings, bench.clock);

    let view = bench.view();
    assert!(!view.applying);
    let closed = "the song closed before audio device \"Interface\" opened with it";
    assert_eq!(
        notices_of(&view, BUFFER_NOT_KEPT),
        [format!("{BUFFER_NOT_KEPT}{closed}")]
    );
    assert_eq!(
        notices_of(&view, ROUTING_NOT_KEPT),
        [format!("{ROUTING_NOT_KEPT}{closed}")]
    );
    assert_eq!(bench.settings.buffer_frames(), 256);
    assert_eq!(bench.stored("fake:2"), Some(Routing::stereo(8)));
}

#[test]
fn a_buffer_size_set_while_an_earlier_refusal_holds_the_device_is_kept_once_it_opens_with_it() {
    let mut bench = Bench::on("fake:2");
    bench.fake.set_buffer_range("fake:2", Some((256, 256)));
    bench.act(OutputAction::Buffer(128));
    assert_eq!(notices_of(&bench.view(), BUFFER_NOT_KEPT).len(), 1);
    bench.fake.set_buffer_range("fake:2", None);

    // The session opens the held device with it on its next try.
    bench.act(OutputAction::Buffer(512));
    assert!(bench.view().applying, "that hold is not its refusal");
    bench.settle();

    let view = bench.view();
    assert_eq!((view.buffer, bench.settings.buffer_frames()), (512, 512));
    assert_eq!(view.notices, Vec::<String>::new());
}

#[test]
fn a_buffer_size_set_while_an_earlier_refusal_holds_the_device_is_not_kept_when_it_keeps_refusing()
{
    let mut bench = Bench::on("fake:2");
    bench.fake.set_buffer_range("fake:2", Some((256, 256)));
    bench.act(OutputAction::Buffer(128));
    let attempts = bench.fake.open_attempts();

    bench.act(OutputAction::Buffer(64));
    assert!(bench.view().applying, "that hold is not its refusal");
    bench.run_until("the session never tried the device again", |bench| {
        bench.fake.open_attempts() > attempts
    });
    bench.later(APPLY_PATIENCE - Duration::from_millis(1));
    assert!(bench.view().applying);
    bench.later(Duration::from_millis(1));

    let view = bench.view();
    assert!(!view.applying);
    assert_eq!(
        notices_of(&view, BUFFER_NOT_KEPT),
        [
            "The buffer size could not be changed: audio device \"Interface\" cannot use a buffer of 64 frames; it supports 256 to 256"
        ]
    );
    assert_eq!(bench.settings.buffer_frames(), 256);
}

#[test]
fn a_routing_waiting_when_another_device_is_chosen_is_not_saved_for_the_device_it_left() {
    let mut bench = Bench::with_saved("fake:2", Some(Routing::stereo(8)));
    bench.play();
    bench.act(OutputAction::Reset);
    assert!(bench.view().applying);

    bench.act(OutputAction::ChooseDevice(0));
    bench.settle();

    assert_eq!(bench.view().device, "Laptop · 2 outputs");
    assert_eq!(bench.stored("fake:2"), Some(Routing::stereo(8)));
    assert_eq!(bench.stored("fake:1"), None);
    assert_eq!(
        notices_of(&bench.view(), ROUTING_NOT_KEPT),
        [
            "The routing could not be applied: the song closed before audio device \"Interface\" opened with it"
        ]
    );
}

#[test]
fn a_routing_waiting_when_another_device_is_chosen_is_not_blamed_on_the_new_device() {
    let mut bench = Bench::with_saved("fake:2", Some(Routing::stereo(8)));
    bench.play();
    bench.act(OutputAction::Reset);
    bench.act(OutputAction::ChooseDevice(0));
    // The laptop then refuses a buffer size: its hold is not the routing's.
    bench.fake.set_buffer_range("fake:1", Some((256, 256)));

    bench.act(OutputAction::Buffer(128));

    let view = bench.view();
    assert_eq!(
        notices_of(&view, BUFFER_NOT_KEPT).len(),
        1,
        "{:?}",
        view.notices
    );
    let routing = notices_of(&view, ROUTING_NOT_KEPT);
    assert!(
        routing.iter().all(|notice| !notice.contains("Laptop")),
        "{routing:?}"
    );
    assert_eq!(bench.stored("fake:2"), Some(Routing::stereo(8)));
}

#[test]
fn a_full_queue_is_no_refusal_of_a_buffer_size_or_a_routing() {
    // The session still takes the change from its next polls; both
    // setters' answers read the same way.
    let full = RuntimeError::QueueFull {
        command: "SetRouting".to_owned(),
    };
    assert_eq!(refusal(Err(full)), None);
    assert_eq!(refusal(Ok(())), None);
    let size = RuntimeError::BufferSize { frames: 32 };
    assert_eq!(refusal(Err(size.clone())), Some(size));
}

#[test]
fn a_routing_given_while_the_device_is_unplugged_is_saved_and_plays_once_it_is_back() {
    let mut bench = Bench::with_saved("fake:2", Some(Routing::stereo(8)));
    bench.fake.unplug("fake:2");
    bench.show.as_mut().unwrap().poll();

    // The session opens the device with it, fitted to its outputs, when it
    // is back: nothing to wait for.
    bench.act(OutputAction::Reset);

    let view = bench.view();
    assert!(!view.applying);
    assert_eq!(view.notices, Vec::<String>::new());
    assert_eq!(bench.stored("fake:2"), Some(Routing::preset(8)));
    bench.fake.plug("fake:2");
    bench.run_until("the device never played again", |bench| {
        bench.view().device_state == DeviceState::Open
    });
    assert_eq!(bench.routing(), Routing::preset(8));
    assert_eq!(
        bench.fake.opened().last().map(|request| request.channels),
        Some(8)
    );
}

/// Asks the interface, which plays 256 frames whatever it is asked for,
/// for 128 with the song stopped: the stream opens again at once and looks
/// the same as the one before, so only [`APPLY_PATIENCE`] settles the
/// change, and the session shows nothing new when it does.
fn clamped_buffer_waiting() -> Bench {
    let mut bench = Bench::on("fake:2");
    bench.rounds_to.store(256, Ordering::SeqCst);
    bench.act(OutputAction::Buffer(128));
    bench.until_still();
    assert!(bench.view().applying, "the change waits for its patience");
    bench
}

#[test]
fn what_the_screens_draw_changes_when_a_waiting_change_settles_though_the_session_shows_the_same() {
    let mut bench = clamped_buffer_waiting();
    let (snapshot, before) = (bench.snapshot(), bench.drawn());
    assert_eq!(bench.drawn(), before, "nothing changed, nothing to draw");

    bench.later(APPLY_PATIENCE);

    assert_eq!(bench.snapshot(), snapshot);
    assert!(!bench.view().applying);
    assert_ne!(bench.drawn(), before);
}

#[test]
fn a_change_its_patience_alone_settles_draws_the_screen_again() {
    let mut bench = clamped_buffer_waiting();
    let snapshot = bench.snapshot();
    assert!(
        !bench.turn(APPLY_PATIENCE - Duration::from_millis(1)),
        "it may still be ramping out: nothing new to draw"
    );

    assert!(
        bench.turn(Duration::from_millis(1)),
        "the screen draws the verdict"
    );
    assert_eq!(bench.snapshot(), snapshot, "the session shows the same");
    let view = bench.view();
    assert!(!view.applying);
    assert_eq!(
        notices_of(&view, BUFFER_NOT_KEPT),
        [
            "The buffer size could not be changed: audio device \"Interface\" plays buffers of 256 frames, not 128"
        ]
    );
    assert!(!bench.turn(Duration::ZERO), "and nothing after it");
}

/// The song open with settings that cannot be written, after a bus change
/// waiting to be saved.
fn bus_change_waiting_unsavable() -> Bench {
    let mut bench = Bench::on("fake:2");
    let blocked = bench.dir.path().join("blocked");
    fs::write(&blocked, "a file, not a folder").unwrap();
    bench.settings = AudioSettings::load(Some(ConfigStore::new(blocked.join("config"))));
    let show = bench.show.as_mut().expect("a song is open");
    bench.mixer.act(
        MixerAction::BusFader {
            bus: Bus::Drums,
            position: 0.3,
        },
        show,
        bench.clock,
    );
    bench.until_still();
    assert!(bench.mixer.buses_waiting());
    bench
}

#[test]
fn what_the_screens_draw_changes_when_bus_changes_stop_waiting_though_the_session_shows_the_same() {
    let mut bench = bus_change_waiting_unsavable();
    let (snapshot, before) = (bench.snapshot(), bench.drawn());

    let show = bench.show.as_mut().expect("a song is open");
    bench
        .mixer
        .tick(show, &mut bench.settings, bench.clock + BUS_SAVE_DELAY);

    assert_eq!(bench.snapshot(), snapshot, "the session still has them");
    assert!(!bench.mixer.buses_waiting());
    assert_ne!(bench.drawn(), before);
}

#[test]
fn bus_changes_that_could_not_be_saved_once_they_settled_draw_the_screen_again() {
    let mut bench = bus_change_waiting_unsavable();
    let snapshot = bench.snapshot();

    assert!(bench.turn(BUS_SAVE_DELAY), "the screen says why");
    assert_eq!(bench.snapshot(), snapshot);
    let show = bench.show.as_ref().expect("a song is open");
    let notices = bench
        .mixer
        .view(Some(show))
        .expect("a song is open")
        .notices;
    assert_eq!(
        notices
            .iter()
            .filter(|notice| notice.starts_with("The bus levels could not be saved: "))
            .count(),
        1,
        "{notices:?}"
    );
    assert!(!bench.turn(Duration::ZERO));
}

#[test]
fn a_still_screen_draws_nothing_however_long_it_waits() {
    let mut bench = Bench::on("fake:2");
    bench.until_still();

    assert!(!bench.turn(Duration::from_secs(60)));
}

#[test]
fn a_playing_song_draws_on_every_turn() {
    let mut bench = Bench::on("fake:2");
    bench.play();

    assert!(bench.turn(Duration::ZERO));
    assert!(bench.turn(Duration::ZERO));
}

#[test]
fn a_hold_of_panic_draws_on_every_turn_while_it_counts_with_or_without_a_song() {
    let mut bench = Bench::on("fake:2");
    bench.until_still();
    bench.hold.press(bench.clock);
    assert!(bench.turn(PANIC_HOLD / 4), "the fill grows");
    assert!(bench.turn(PANIC_HOLD / 4), "the fill grows");
    bench.hold.cancel();
    assert!(!bench.turn(PANIC_HOLD / 4), "no hold, nothing to draw");

    // With no song open, as the screen starts.
    let mut mixer = MixerPage::new();
    let mut turn = |hold: &mut PanicHold, now| {
        Timed {
            show: None,
            hold,
            mixer: &mut mixer,
            output: &mut bench.page,
            settings: &mut bench.settings,
        }
        .turn(now)
    };
    let now = Instant::now();
    let mut hold = PanicHold::new();
    hold.press(now);
    assert!(turn(&mut hold, now + PANIC_HOLD / 4), "the fill grows");
    hold.cancel();
    assert!(!turn(&mut hold, now + PANIC_HOLD / 2));
}
