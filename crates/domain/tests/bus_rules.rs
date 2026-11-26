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

/// Every word of the rules table and every two-word phrase, with the bus
/// `bus_for_name` gave it at bc999ea, before the table mapped names to
/// parts: the parts kept every bus. Taken from the base commit's code, not
/// from the current table.
#[test]
fn every_rules_word_and_phrase_keeps_its_bus_from_before_the_parts() {
    let cases: &[(&str, Bus)] = &[
        ("click", Bus::ClickGuide),
        ("clk", Bus::ClickGuide),
        ("metronome", Bus::ClickGuide),
        ("guide", Bus::ClickGuide),
        ("cue", Bus::ClickGuide),
        ("count", Bus::ClickGuide),
        ("clic", Bus::ClickGuide),
        ("metronomo", Bus::ClickGuide),
        ("guia", Bus::ClickGuide),
        ("cuenta", Bus::ClickGuide),
        ("drum", Bus::Drums),
        ("kit", Bus::Drums),
        ("kick", Bus::Drums),
        ("kik", Bus::Drums),
        ("snare", Bus::Drums),
        ("snr", Bus::Drums),
        ("hat", Bus::Drums),
        ("hihat", Bus::Drums),
        ("hh", Bus::Drums),
        ("tom", Bus::Drums),
        ("overhead", Bus::Drums),
        ("cymbal", Bus::Drums),
        ("clap", Bus::Drums),
        ("snap", Bus::Drums),
        ("shaker", Bus::Drums),
        ("tamb", Bus::Drums),
        ("tambourine", Bus::Drums),
        ("perc", Bus::Drums),
        ("percussion", Bus::Drums),
        ("conga", Bus::Drums),
        ("bongo", Bus::Drums),
        ("cajon", Bus::Drums),
        ("timbale", Bus::Drums),
        ("cowbell", Bus::Drums),
        ("timpani", Bus::Drums),
        ("room", Bus::Drums),
        ("loop", Bus::Drums),
        ("seq", Bus::Drums),
        ("sequence", Bus::Drums),
        ("beat", Bus::Drums),
        ("bateria", Bus::Drums),
        ("bombo", Bus::Drums),
        ("caja", Bus::Drums),
        ("redoblante", Bus::Drums),
        ("platillo", Bus::Drums),
        ("percusion", Bus::Drums),
        ("secuencia", Bus::Drums),
        ("bass", Bus::Bass),
        ("sub", Bus::Bass),
        ("synthbass", Bus::Bass),
        ("subbass", Bus::Bass),
        ("bassline", Bus::Bass),
        ("bajo", Bus::Bass),
        ("keys", Bus::KeysPads),
        ("keyboard", Bus::KeysPads),
        ("kbd", Bus::KeysPads),
        ("piano", Bus::KeysPads),
        ("pno", Bus::KeysPads),
        ("rhodes", Bus::KeysPads),
        ("wurli", Bus::KeysPads),
        ("wurlitzer", Bus::KeysPads),
        ("organ", Bus::KeysPads),
        ("hammond", Bus::KeysPads),
        ("ep", Bus::KeysPads),
        ("clav", Bus::KeysPads),
        ("synth", Bus::KeysPads),
        ("pad", Bus::KeysPads),
        ("string", Bus::KeysPads),
        ("orchestra", Bus::KeysPads),
        ("orch", Bus::KeysPads),
        ("violin", Bus::KeysPads),
        ("viola", Bus::KeysPads),
        ("cello", Bus::KeysPads),
        ("brass", Bus::KeysPads),
        ("horn", Bus::KeysPads),
        ("trumpet", Bus::KeysPads),
        ("trombone", Bus::KeysPads),
        ("sax", Bus::KeysPads),
        ("saxophone", Bus::KeysPads),
        ("flute", Bus::KeysPads),
        ("woodwind", Bus::KeysPads),
        ("harp", Bus::KeysPads),
        ("bell", Bus::KeysPads),
        ("glock", Bus::KeysPads),
        ("glockenspiel", Bus::KeysPads),
        ("celesta", Bus::KeysPads),
        ("mellotron", Bus::KeysPads),
        ("accordion", Bus::KeysPads),
        ("fx", Bus::KeysPads),
        ("sfx", Bus::KeysPads),
        ("riser", Bus::KeysPads),
        ("sweep", Bus::KeysPads),
        ("swell", Bus::KeysPads),
        ("ambient", Bus::KeysPads),
        ("ambience", Bus::KeysPads),
        ("atmos", Bus::KeysPads),
        ("atmosphere", Bus::KeysPads),
        ("texture", Bus::KeysPads),
        ("drone", Bus::KeysPads),
        ("arp", Bus::KeysPads),
        ("noise", Bus::KeysPads),
        ("impact", Bus::KeysPads),
        ("teclado", Bus::KeysPads),
        ("organo", Bus::KeysPads),
        ("sintetizador", Bus::KeysPads),
        ("sintetizadores", Bus::KeysPads),
        ("sinte", Bus::KeysPads),
        ("colchon", Bus::KeysPads),
        ("colchones", Bus::KeysPads),
        ("cuerda", Bus::KeysPads),
        ("violines", Bus::KeysPads),
        ("chelo", Bus::KeysPads),
        ("viento", Bus::KeysPads),
        ("metales", Bus::KeysPads),
        ("trompeta", Bus::KeysPads),
        ("saxo", Bus::KeysPads),
        ("flauta", Bus::KeysPads),
        ("campana", Bus::KeysPads),
        ("guitar", Bus::Guitars),
        ("gtr", Bus::Guitars),
        ("eg", Bus::Guitars),
        ("ag", Bus::Guitars),
        ("banjo", Bus::Guitars),
        ("mandolin", Bus::Guitars),
        ("mando", Bus::Guitars),
        ("ukulele", Bus::Guitars),
        ("uke", Bus::Guitars),
        ("dobro", Bus::Guitars),
        ("acoustic", Bus::Guitars),
        ("electric", Bus::Guitars),
        ("steel", Bus::Guitars),
        ("nylon", Bus::Guitars),
        ("guitarra", Bus::Guitars),
        ("requinto", Bus::Guitars),
        ("charango", Bus::Guitars),
        ("electrica", Bus::Guitars),
        ("electrico", Bus::Guitars),
        ("acustica", Bus::Guitars),
        ("acustico", Bus::Guitars),
        ("vocal", Bus::Vocals),
        ("vox", Bus::Vocals),
        ("voc", Bus::Vocals),
        ("bgv", Bus::Vocals),
        ("bv", Bus::Vocals),
        ("choir", Bus::Vocals),
        ("voice", Bus::Vocals),
        ("singer", Bus::Vocals),
        ("harmony", Bus::Vocals),
        ("harmonies", Bus::Vocals),
        ("spoken", Bus::Vocals),
        ("speech", Bus::Vocals),
        ("voz", Bus::Vocals),
        ("voces", Bus::Vocals),
        ("coro", Bus::Vocals),
        ("armonia", Bus::Vocals),
        ("bass guitar", Bus::Bass),
        ("bass synth", Bus::Bass),
        ("synth bass", Bus::Bass),
        ("sub bass", Bus::Bass),
        ("bass drum", Bus::Drums),
        ("bajo sinte", Bus::Bass),
        ("bajo sintetizador", Bus::Bass),
        ("voz guia", Bus::Vocals),
        ("backing vocal", Bus::Vocals),
        ("electric guitar", Bus::Guitars),
        ("electric gtr", Bus::Guitars),
        ("guitarra electrica", Bus::Guitars),
        ("acoustic guitar", Bus::Guitars),
        ("acoustic gtr", Bus::Guitars),
        ("ac gtr", Bus::Guitars),
        ("guitarra acustica", Bus::Guitars),
    ];
    assert_eq!(cases.len(), 171, "155 words and 16 phrases");
    assert_names(cases);
}

#[test]
fn a_stored_part_id_decides_the_bus_before_the_file_name() {
    // A stem the user set to Other stores "other", which names no
    // instrument; the file name must not move it back to Guitars.
    assert_eq!(bus_for_stem(&track("other", "EG 1.wav")), DEFAULT_BUS);
    assert_eq!(bus_for_stem(&track("synth-bass", "Drums.wav")), Bus::Bass);
    assert_eq!(
        bus_for_stem(&stem("other", "EG 1.wav", StemRole::Click)),
        Bus::ClickGuide,
        "the role still comes first"
    );
}

#[test]
fn a_free_string_part_still_goes_where_its_words_or_the_file_name_say() {
    // Packages written before the parts store any text.
    assert_eq!(bus_for_stem(&track("Electric Gtr", "x.wav")), Bus::Guitars);
    assert_eq!(bus_for_stem(&track("Other", "EG 1.wav")), Bus::Guitars);
    assert_eq!(bus_for_stem(&track("misc", "Track 1.wav")), DEFAULT_BUS);
}
