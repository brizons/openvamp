//! S1: the six buses and the part-to-bus rules table (F1.2, F7.2).

use domain::{Bus, DEFAULT_BUS, Stem, StemRole, bus_for_name, bus_for_stem};

fn stem(part: &str, file: &str, role: StemRole) -> Stem {
    Stem {
        id: "s".to_owned(),
        file: file.to_owned(),
        part: part.to_owned(),
        role,
    }
}

fn track(part: &str, file: &str) -> Stem {
    stem(part, file, StemRole::Track)
}

#[track_caller]
fn assert_names(cases: &[(&str, Bus)]) {
    for (name, bus) in cases {
        assert_eq!(bus_for_name(name), Some(*bus), "{name:?} goes to {bus}");
    }
}

#[test]
fn the_six_buses_have_stable_ids_and_display_names() {
    let expected = [
        (Bus::ClickGuide, "clickGuide", "Click & Guide"),
        (Bus::Drums, "drums", "Drums"),
        (Bus::Bass, "bass", "Bass"),
        (Bus::KeysPads, "keysPads", "Keys & Pads"),
        (Bus::Guitars, "guitars", "Guitars"),
        (Bus::Vocals, "vocals", "Vocals"),
    ];
    assert_eq!(Bus::ALL, expected.map(|(bus, _, _)| bus), "mixer order");
    for (bus, id, name) in expected {
        assert_eq!(bus.id(), id);
        assert_eq!(bus.name(), name);
        assert_eq!(bus.to_string(), name);
        assert_eq!(Bus::from_id(id), Some(bus));
        assert_eq!(serde_json::to_string(&bus).unwrap(), format!("\"{id}\""));
        assert_eq!(
            serde_json::from_str::<Bus>(&format!("\"{id}\"")).unwrap(),
            bus
        );
    }
    assert_eq!(Bus::from_id("Drums"), None, "ids are case-sensitive");
    assert_eq!(Bus::from_id("brass"), None);
}

#[test]
fn click_and_guide_stems_always_go_to_click_and_guide() {
    for role in [StemRole::Click, StemRole::Guide] {
        let stem = stem("drums", "stems/Bass.wav", role);
        assert_eq!(bus_for_stem(&stem), Bus::ClickGuide, "{role}");
    }
}

#[test]
fn parts_go_to_the_bus_of_their_instrument() {
    assert_names(&[
        ("Click", Bus::ClickGuide),
        ("Guide", Bus::ClickGuide),
        ("Click Track", Bus::ClickGuide),
        ("Count In", Bus::ClickGuide),
        ("Drums", Bus::Drums),
        ("Kick In", Bus::Drums),
        ("Snare Top", Bus::Drums),
        ("Hi-Hat", Bus::Drums),
        ("Toms", Bus::Drums),
        ("Overheads", Bus::Drums),
        ("Perc", Bus::Drums),
        ("Shaker", Bus::Drums),
        ("Loop", Bus::Drums),
        ("Bass", Bus::Bass),
        ("Bass DI", Bus::Bass),
        ("Sub", Bus::Bass),
        ("Synth Bass", Bus::Bass),
        ("Keys", Bus::KeysPads),
        ("Piano", Bus::KeysPads),
        ("Rhodes", Bus::KeysPads),
        ("Organ", Bus::KeysPads),
        ("Pad", Bus::KeysPads),
        ("Synth", Bus::KeysPads),
        ("Strings", Bus::KeysPads),
        ("Brass", Bus::KeysPads),
        ("FX", Bus::KeysPads),
        ("Gtr", Bus::Guitars),
        ("Guitar", Bus::Guitars),
        ("EG", Bus::Guitars),
        ("AG", Bus::Guitars),
        ("Acoustic", Bus::Guitars),
        ("Vox", Bus::Vocals),
        ("Lead Vocal", Bus::Vocals),
        ("BGV", Bus::Vocals),
        ("Choir", Bus::Vocals),
        ("Spoken Word", Bus::Vocals),
    ]);
}

#[test]
fn names_are_normalized_for_case_separators_digits_and_plurals() {
    assert_names(&[
        ("VOX", Bus::Vocals),
        ("vox", Bus::Vocals),
        ("bgv-alto", Bus::Vocals),
        ("BGVs", Bus::Vocals),
        ("Kick_In", Bus::Drums),
        ("kick.in", Bus::Drums),
        ("Gtr2", Bus::Guitars),
        ("EG 1", Bus::Guitars),
        ("Pad1", Bus::KeysPads),
        ("Pads", Bus::KeysPads),
        ("AcGtr", Bus::Guitars),
        ("SynthBass", Bus::Bass),
        ("Room Mics L/R (Drums Live)", Bus::Drums),
    ]);
}

#[test]
fn the_last_instrument_named_wins_and_generic_words_only_break_ties() {
    assert_names(&[
        // A file name often starts with the song title: the instrument comes last.
        ("Guide Me O Thou - Piano", Bus::KeysPads),
        ("Count On Me - Bass", Bus::Bass),
        ("Guide Vox", Bus::Vocals),
        // Generic words (loop, fx, ambient, acoustic...) never beat an instrument.
        ("Gtr Loop", Bus::Guitars),
        ("Piano Loop", Bus::KeysPads),
        ("Vocal FX", Bus::Vocals),
        ("EG 2 Lead (Swells & Ambient)", Bus::Guitars),
        ("Acoustic Piano", Bus::KeysPads),
        // Two-word instruments.
        ("Bass Guitar", Bus::Bass),
        ("Bass Drum", Bus::Drums),
        ("Bass Synth", Bus::Bass),
    ]);
}

#[test]
fn the_file_name_is_used_when_the_part_names_no_instrument() {
    // F1 acceptance: "A stem named `EG 1.wav` is assigned to the
    // electric-guitar part and bus without user action."
    assert_eq!(bus_for_stem(&track("", "EG 1.wav")), Bus::Guitars);
    assert_eq!(
        bus_for_stem(&track("stem 3", "stems/EG 1.wav")),
        Bus::Guitars
    );
    assert_eq!(
        bus_for_stem(&track("", r"Audio\BGV_Alto.flac")),
        Bus::Vocals
    );
    // The part wins over the file name.
    assert_eq!(bus_for_stem(&track("Bass", "stems/drums.wav")), Bus::Bass);
    // Only the file's own name counts, not the folders above it.
    assert_eq!(bus_for_stem(&track("", "Keys/Track 7.wav")), DEFAULT_BUS);
}

#[test]
fn unknown_parts_fall_to_the_default_bus() {
    assert_eq!(DEFAULT_BUS, Bus::KeysPads);
    for name in ["", "Other", "Track 12", "Stereo Mix", "1 2 3", "---"] {
        assert_eq!(bus_for_name(name), None, "{name:?} names no instrument");
    }
    assert_eq!(
        bus_for_stem(&track("Other", "stems/track 12.wav")),
        DEFAULT_BUS
    );
}

#[test]
fn spanish_parts_go_to_the_bus_of_their_instrument() {
    assert_names(&[
        ("Clic", Bus::ClickGuide),
        ("Metrónomo", Bus::ClickGuide),
        ("Guía", Bus::ClickGuide),
        ("Cuenta", Bus::ClickGuide),
        ("Batería", Bus::Drums),
        ("Bombo", Bus::Drums),
        ("Caja", Bus::Drums),
        ("Redoblante", Bus::Drums),
        ("Platillos", Bus::Drums),
        ("Percusión", Bus::Drums),
        ("Loop", Bus::Drums),
        ("Secuencia", Bus::Drums),
        ("Secuencias", Bus::Drums),
        ("Bajo", Bus::Bass),
        ("Bajo Eléctrico", Bus::Bass),
        ("Sub", Bus::Bass),
        ("Teclado", Bus::KeysPads),
        ("Teclados", Bus::KeysPads),
        ("Piano", Bus::KeysPads),
        ("Órgano", Bus::KeysPads),
        ("Sintetizador", Bus::KeysPads),
        ("Sintetizadores", Bus::KeysPads),
        ("Sinte", Bus::KeysPads),
        ("Pad", Bus::KeysPads),
        ("Colchón", Bus::KeysPads),
        ("Colchones", Bus::KeysPads),
        ("Cuerdas", Bus::KeysPads),
        ("Violín", Bus::KeysPads),
        ("Violines", Bus::KeysPads),
        ("Chelo", Bus::KeysPads),
        ("Vientos", Bus::KeysPads),
        ("Metales", Bus::KeysPads),
        ("Trompeta", Bus::KeysPads),
        ("Saxo", Bus::KeysPads),
        ("Flauta", Bus::KeysPads),
        ("Campanas", Bus::KeysPads),
        ("Guitarra", Bus::Guitars),
        ("Guitarras", Bus::Guitars),
        ("Guitarra Eléctrica", Bus::Guitars),
        ("Guitarra Acústica", Bus::Guitars),
        ("Eléctrica", Bus::Guitars),
        ("Acústica", Bus::Guitars),
        ("Requinto", Bus::Guitars),
        ("Charango", Bus::Guitars),
        ("Voz", Bus::Vocals),
        ("Voces", Bus::Vocals),
        ("Coro", Bus::Vocals),
        ("Coros", Bus::Vocals),
        ("Segunda Voz", Bus::Vocals),
        ("Armonía", Bus::Vocals),
        ("Armonías", Bus::Vocals),
    ]);
}

#[test]
fn accents_are_ignored() {
    assert_names(&[
        ("Bateria", Bus::Drums),
        ("BATERÍA", Bus::Drums),
        ("percusion", Bus::Drums),
        ("Organo", Bus::KeysPads),
        ("Colchon", Bus::KeysPads),
        ("Violin", Bus::KeysPads),
        ("Guia", Bus::ClickGuide),
        ("Metronomo", Bus::ClickGuide),
        ("Armonias", Bus::Vocals),
        ("GUITARRA ELÉCTRICA", Bus::Guitars),
    ]);
}

#[test]
fn spanish_names_follow_the_same_precedence() {
    assert_names(&[
        // The last instrument word wins over a song title.
        ("Bajo Tu Gracia - Piano", Bus::KeysPads),
        ("Cuenta Conmigo - Bajo", Bus::Bass),
        ("Guía Mis Pasos - Guitarra", Bus::Guitars),
        ("Tu Voz - Teclado", Bus::KeysPads),
        // Generic words never beat an instrument.
        ("Guitarra Loop", Bus::Guitars),
        ("Bajo Acústico", Bus::Bass),
        ("Piano Eléctrico", Bus::KeysPads),
        ("Voz Armonía", Bus::Vocals),
        // Two-word instruments, with the noun first.
        ("Voz Guía", Bus::Vocals),
        ("Bajo Sinte", Bus::Bass),
        ("Bajo Sintetizador", Bus::Bass),
        // Spanish and English words mix.
        ("Sinte Bass", Bus::Bass),
        ("EG Requinto", Bus::Guitars),
    ]);
    assert_eq!(bus_for_stem(&track("", "stems/Batería 2.wav")), Bus::Drums);
}

#[test]
fn every_bus_sits_in_bus_all_at_its_index() {
    for (place, bus) in Bus::ALL.into_iter().enumerate() {
        assert_eq!(bus.index(), place, "{bus}");
        assert_eq!(Bus::ALL[bus.index()], bus);
    }
}

#[test]
fn names_that_now_pick_a_precise_part_keep_their_bus() {
    // S2: these two-word names pick a more precise part (electric or
    // acoustic guitar, synth bass, backing vocals) without changing the bus
    // the words gave them before.
    assert_names(&[
        ("Electric Guitar", Bus::Guitars),
        ("Song - Electric Gtr 2", Bus::Guitars),
        ("Guitarra Eléctrica FX", Bus::Guitars),
        ("Acoustic Guitars", Bus::Guitars),
        ("Acoustic Gtr Loop", Bus::Guitars),
        ("Ac Gtr", Bus::Guitars),
        ("Piano Guitarra Acústica", Bus::Guitars),
        ("Electric Guitar Bass", Bus::Bass),
        ("Synth Bass Pad", Bus::KeysPads),
        ("Sub Bass", Bus::Bass),
        ("SubBass", Bus::Bass),
        ("Backing Vocals", Bus::Vocals),
        ("Backing Vox Choir", Bus::Vocals),
    ]);
    for name in ["Backing", "Ac", "Backing Track"] {
        assert_eq!(bus_for_name(name), None, "{name:?} names no instrument");
    }
}
