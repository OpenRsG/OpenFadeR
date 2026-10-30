//! User-facing explanations keyed by the existing saved setting identifiers.

pub(super) fn text(key: &str) -> &'static str {
    match key {
        "language" => {
            tr!("Choose the interface language. System follows your operating system's language.")
        }
        "tooltips" => tr!(
            "Show explanations when you hold the pointer over a control. Clickable settings help remains available when tooltips are off."
        ),
        "tempo_decimals" => tr!(
            "Choose how precisely tempo is displayed. This changes the readout, not the audio or beat grid."
        ),
        "tempo_reverse" => tr!(
            "Reverse the on-screen vertical tempo fader. Use Invert hardware tempo fader separately for a controller that moves the wrong way."
        ),
        "browse_font" | "browse_bold" | "line_space" => tr!(
            "Adjust the track list's text size, weight or row spacing for comfortable reading. Track data is unchanged."
        ),
        "center_selected" => tr!(
            "Keep the selected track near the center of the visible browser list while navigating."
        ),
        "played_color" | "played_reset_on_quit" | "played_reset" => tr!(
            "Mark tracks played in this session. Reset clears the played color, not the track or its library history."
        ),
        "key" => tr!(
            "Choose musical-key notation: note names, Camelot-style codes, Open Key, or the library's stored text."
        ),
        "waveform_renderer" => tr!(
            "Auto chooses a supported waveform renderer. CPU is useful when a GPU driver causes display problems; this does not change the audio engine."
        ),
        "draw_rate" => tr!(
            "Higher drawing rates make waveforms smoother but require more rendering work. Lower rates can save power."
        ),
        "overview" => {
            tr!("Choose how much waveform detail is drawn in track overviews and browser previews.")
        }
        "beat_count" => tr!(
            "Choose whether the waveform readout describes the current bar position or counts toward upcoming beats and bars."
        ),
        "beat_grid" => {
            tr!("Change the appearance of beat-grid lines. This does not move the actual beats.")
        }
        "cue_markers" => tr!("Choose the size of hot-cue flags and letters on waveforms."),
        "deck_number" => tr!(
            "Place the deck number beside or within the waveform to make decks easier to identify."
        ),
        "click_wave" => tr!(
            "Allow waveform pointer gestures to control playback and cueing. Turn it off if you only want to inspect the waveform."
        ),
        "jog_display" => {
            tr!("Choose which position or cue countdown the center of the jog display shows.")
        }
        "traffic_light" => {
            tr!("Choose how harmonic-key compatibility is indicated when comparing tracks.")
        }
        "media_related" => {
            tr!("Show related tracks in the media browser to help choose another track.")
        }
        "media_hot_cue_bank" => {
            tr!("Show cue banks in the media browser for collecting and reusing hot cues.")
        }
        "media_track_suggestion" => {
            tr!("Show the track-suggestion browser alongside your other music sources.")
        }
        "media_explorer" => {
            tr!("Show the folder explorer in the media browser. Hiding it does not remove music.")
        }
        "panel_cue_analysis" => {
            tr!("Show the playlist used to review tracks prepared by CUE Analysis.")
        }
        "panel_palette" => {
            tr!("Show the playlist palette for quick access to playlists while browsing.")
        }
        "panel_preview_cues" => tr!("Draw cue positions on browser waveform previews."),
        "panel_all_tracks" => tr!("Include the All Tracks entry in the playlist browser."),
        "deck_sync_rate" => tr!("Show the tempo relationship used by synchronized decks."),
        "phrase_enlarged" | "phrase_full" | "phrase_names_always" => tr!(
            "Show analyzed musical phrases and their labels on detailed or overview waveforms. Phrase analysis must be available for the track."
        ),
        "vocal_enlarged" | "vocal_full" => tr!(
            "Show estimated vocal regions on detailed or overview waveforms. This display depends on the track's vocal analysis."
        ),
        "effect_panel" => {
            tr!("Choose the arrangement of effect controls in the performance workspace.")
        }
        "wave_order" => {
            tr!("Choose which deck appears in each enlarged waveform lane in four-deck layouts.")
        }
        "pad_mode" => tr!(
            "Choose whether pad-mode display follows the active controls automatically or is selected manually."
        ),
        "skin" => tr!(
            "Choose the overall theme. Open Themes to preview, customize or save a personal copy."
        ),
        "wave" => tr!("Choose the waveform color scheme used to distinguish frequency bands."),
        "hot_cue_colors" | "pad_fx_color" | "sampler_color" => tr!(
            "Choose how cue, effect-pad or sampler colors are displayed. This does not change their sound or assignments."
        ),
        "deck_layout" | "export_layout" | "show_mixer" | "decks" => tr!(
            "Arrange the visible players, waveform lanes and mixer. Switching layouts keeps loaded tracks and playback state."
        ),
        "sample_rate" => tr!(
            "Set the output sample rate. Auto uses the driver's supported default; choosing a rate may reopen the audio stream."
        ),
        "buffer_size" => tr!(
            "Smaller audio buffers reduce control-to-sound latency but need more reliable real-time processing. Increase the buffer if you hear dropouts; Auto uses the driver default."
        ),
        "mixer_mode" => tr!(
            "Internal mixes decks in OpenFadeR. External sends decks to separate hardware output pairs and requires an audio device with enough channels."
        ),
        "out_master" => tr!("Choose the hardware channel pair that carries the master mix."),
        "out_phones" => tr!(
            "Choose a headphone cue output pair. A separate cue mix needs independent output channels or a configured cue device."
        ),
        "out_booth" | "booth_level" => {
            tr!("Route and adjust a separate booth-monitor output. Off leaves that output unused.")
        }
        "metronome_tone" | "metronome_volume" => tr!(
            "Choose the sound and level of the beat-grid metronome. Use it while checking beat alignment."
        ),
        "preview_volume" => {
            tr!("Set the level of browser preview playback without changing deck channel faders.")
        }
        "input_device" | "input_pair" => tr!(
            "Choose the audio input device and channel pair for input recording. Microphone and AUX controls can also use this input device."
        ),
        "rec_source" => tr!(
            "Record the master mix or a selected external audio input. Choosing Input does not record the deck mix."
        ),
        "mic_on" | "mic_pair" | "mic_level" => tr!(
            "Enable a microphone, choose its input channels and set its level. Check the input device before raising the microphone level."
        ),
        "talkover_mode" | "talkover_level" => tr!(
            "Lower the music while microphone talkover is active. Advanced mode reduces the mid range instead of the entire signal."
        ),
        "aux_on" | "aux_device" | "aux_pair" => tr!(
            "Bring an external line source into the mixer through the AUX strip. Choose its device and input channel pair."
        ),
        "aux_duck" | "aux_release" => tr!(
            "Choose how far the music is lowered by AUX ducking and how quickly its level returns afterward."
        ),
        "eject_lock" => tr!(
            "Prevent loading another track into a deck while it is playing. Pause the deck before replacing its track."
        ),
        "needle_lock" => tr!(
            "Prevent overview-waveform needle searches while a deck is playing, to avoid an accidental jump."
        ),
        "memory_cue_lock" => tr!("Prevent recalling a memory cue while the deck is playing."),
        "tempo_invert" => tr!(
            "Reverse incoming hardware tempo-fader values when moving your controller's fader changes tempo in the wrong direction."
        ),
        "load_nearest_cue" => tr!(
            "When a track loads, use the memory or hot cue closest to its beginning as the starting position."
        ),
        "auto_cue" | "auto_cue_level" => tr!(
            "Place the transport cue at the first sound above the chosen threshold. A lower threshold includes quieter introductions."
        ),
        "vinyl_brake" | "vinyl_start" => tr!(
            "Set how long a deck takes to stop or reach full speed when vinyl-style braking or starting is used."
        ),
        "jog_speed" => {
            tr!("Adjust how far the track moves for a turn of the controller's jog wheel.")
        }
        "jog_cue_return" => tr!(
            "After a jog audition from CUE, return to the cue point and pause when the gesture ends."
        ),
        "continuous_play" => {
            tr!("Automatically load the next track in the list when the current track ends.")
        }
        "sync_type" => {
            tr!("BPM sync matches tempo. Beat sync also aligns the decks' beat positions.")
        }
        "sync_half_double" => tr!(
            "Allow synchronization between related half-time or double-time tempo values, such as 70 and 140 BPM."
        ),
        "hot_cue_gate" => tr!(
            "While paused, play a hot cue only while its button is held, then return to the cue on release."
        ),
        "auto_loop_beats" => {
            tr!("Choose the initial beat length used when starting an automatic beat loop.")
        }
        "range" => tr!(
            "Choose the tempo-fader range applied to decks at startup. Wider ranges allow larger tempo changes."
        ),
        "quantize" => tr!(
            "Choose whether decks start with Quantize enabled. Quantize aligns enabled actions to the beat grid."
        ),
        "keylock" => tr!(
            "Choose whether decks start with key lock enabled. Key lock preserves musical pitch when changing tempo."
        ),
        "auto_gain" => tr!(
            "Use analyzed track loudness to reduce level differences between loaded tracks. Channel faders and trim still work."
        ),
        "eq_type" => tr!(
            "Choose ordinary EQ or isolator-style frequency controls, including how fully a band is removed at its minimum."
        ),
        "fader_curve" => tr!(
            "Choose how channel-fader position changes loudness. This is separate from the crossfader's curve."
        ),
        "cfx_knob" => tr!("Choose the mode available to the channel Sound Color FX knobs."),
        "crossfader_enabled" => tr!(
            "Turn off manual crossfader control to avoid accidental bumps. Both sides stay open and channel faders still work; Automix keeps its automatic transitions."
        ),
        "curve" => tr!(
            "Choose a smooth fade, sharp cut or constant-power curve for the manual crossfader."
        ),
        "output_level" => tr!(
            "Adjust the overall deck output level before the master output. Lower it if your mix is too hot."
        ),
        "controller_audio" => tr!(
            "Allow a recognized controller to select its matching audio interface when connected. If no matching interface is found, the current output stays selected."
        ),
        "release_units" | "release_cfx_off" => tr!(
            "Choose whether Release FX affect the master or individual effect units, and whether holding them temporarily disables Color FX."
        ),
        "merge_enabled" | "merge_stop_deck" => tr!(
            "Enable MERGE FX transitions and choose whether the operating deck stops when the transition finishes."
        ),
        "sampler_slots" => tr!(
            "Choose how many sampler pads are available. Low-power mode can limit sample duration to reduce memory use."
        ),
        "sampler_keep_loaded" => {
            tr!("Keep a loaded sample when importing or loading another sample into its slot.")
        }
        "capture_dir" => tr!(
            "Choose where captured sampler audio is saved. Empty uses the default OpenFadeR Sampler folder."
        ),
        "capture_loop_empty" => tr!(
            "During loop playback, press an empty sampler pad to capture the loop into that slot."
        ),
        "sampler_auto_panel" => {
            tr!("Open the sampler panel when a controller's SAMPLER button is used.")
        }
        "sequence_load" => {
            tr!("When loading a sequence into a slot, loop its full length and start playback.")
        }
        "slicer_length" | "slicer_roll" => tr!(
            "Choose the musical range divided into eight Slicer pads and the repeated-note length used by Slicer Roll."
        ),
        "quantize_mode" => tr!(
            "Choose whether a quantized hot cue or loop jumps immediately to an aligned position or waits for the next beat."
        ),
        "snap_length" => {
            tr!("Choose the beat spacing used when placing beat-grid-aligned positions.")
        }
        "quantize_length" => tr!(
            "Choose the beat subdivision used to align quantized actions. The deck's Q control must be on."
        ),
        "quantize_hot_cue" | "quantize_loop" | "quantize_reverse" | "quantize_sequencer" => {
            tr!("Apply beat-grid timing to this action when the deck's Q control is enabled.")
        }
        "quantize_all_decks" => tr!(
            "Make one Q button change Quantize on every deck together instead of only its own deck."
        ),
        "midi_clock_port" => tr!(
            "Send MIDI clock at the playing master deck's tempo. Enter part of the output port's name; leave empty to turn clock output off."
        ),
        "soft_takeover" => tr!(
            "Prevent hardware knobs and faders from jumping values. A control takes over only after it reaches or crosses the value shown on screen."
        ),
        "high_precision" => tr!(
            "Use finer timing when analyzing beat grids. This increases analysis work but does not change the audio buffer."
        ),
        "detect_tempo_changes" => tr!(
            "Look for tempo changes within a track and create grid sections when needed. Steady-tempo tracks keep one section."
        ),
        "bpm_range" => tr!(
            "Limit the tempo search range used during analysis. Choose a range appropriate for your music if half-time or double-time results are common."
        ),
        "part_grid" => tr!("Analyze tempo and beat-grid positions for tracks."),
        "part_key" => tr!("Analyze musical key so tracks can be compared for harmonic mixing."),
        "part_phrase" => tr!(
            "Estimate musical phrases from the analyzed grid. Phrase analysis needs BPM/Grid analysis."
        ),
        "part_vocal" => tr!(
            "Estimate vocal regions for the waveform display. This heuristic is separate from full STEMS separation."
        ),
        "grid_source" => tr!(
            "Choose whether connected-library grids and cues or OpenFadeR's analysis are used for preparation. Analysis locks and cue protection still apply."
        ),
        "auto_analysis" => {
            tr!("Analyze tracks that are missing tempo or key information automatically.")
        }
        "analyze_now" => tr!(
            "Analyze the library using the selected analysis options. This can take CPU time; locked preparation data is protected."
        ),
        "analysis_update" => tr!(
            "Choose what happens when an application update changes analysis results. Ask lets you decide before reanalysis starts."
        ),
        "key_detector" => tr!(
            "Classic uses the standard key detector. Neural uses an additional model and needs the supported analysis feature; see its status beside this setting."
        ),
        "key_write_tag" => tr!(
            "Write the detected musical key into the audio file's tag. This changes file metadata, so keep backups of files you edit."
        ),
        "process" => tr!(
            "Choose how many resources track analysis may use. More workers finish a batch faster but compete with playback and other applications."
        ),
        "cue_mode" => tr!(
            "Auto places preparation cues from the analysis. Manual uses the cue-count and bar-position options below."
        ),
        "cue_hot" | "cue_memory" => {
            tr!("Choose how many hot cues or memory cues manual CUE Analysis creates.")
        }
        "cue_first_bar" | "cue_repeat" | "cue_repeat_bars" => tr!(
            "Place manual analysis cues from the first bar and optionally repeat them at the chosen bar interval. These controls are unavailable in automatic cue mode."
        ),
        "cue_keep" => {
            tr!("Preserve existing cue points instead of replacing them during CUE Analysis.")
        }
        "cue_comment" => tr!("Label generated cues with their phrase or bar information."),
        "stems" => tr!(
            "Separate tracks into parts that can be muted or mixed independently. First use downloads a 316 MB model and separation needs extra memory and processing time."
        ),
        "stems_mode" => tr!(
            "Choose whether the STEMS controls expose three grouped parts or four separate parts."
        ),
        "stems_analysis" => tr!(
            "Choose separation quality or faster processing for the next track. This does not change a track that has already been separated."
        ),
        "stems_layout" | "layout_active" | "layout_iso" | "layout_fx" => tr!(
            "Auto shows the standard STEMS controls. Custom lets you choose Active Stem buttons, Stem Isolator knobs and Stem FX; Hidden removes those controls."
        ),
        "stem_files" => tr!(
            "Use the separate parts already stored in a .stem.mp4 file instead of running separation again."
        ),
        "stem_waveform" => tr!("Display the original mix waveform or the separated stem waveform."),
        "active_part" => tr!(
            "Choose whether an Active Stem pad mutes its part or solos it while the other parts are muted."
        ),
        "stems_reset" => tr!(
            "Choose whether loading another track resets the STEMS mute/solo choices or keeps them."
        ),
        "doubles_from" => tr!(
            "Choose the source deck used by STEM Instant Doubles. Neighbour uses the adjacent mixer deck."
        ),
        "stems_multithread" => tr!(
            "Allow multiple CPU threads during separation. Turning this off reduces CPU concurrency, but separation may take longer."
        ),
        "stems_processor" => tr!(
            "AUTO separates on the graphics card where that is safe, and on the CPU where the only graphics card also draws the desktop (it would freeze while a segment runs). CPU never uses the graphics card. Takes effect from the next track."
        ),
        "groove" => tr!(
            "Enable Beat Lab's drum tools. STEMS must also be enabled for its separated-drum workflow."
        ),
        "drum_pack" => {
            tr!("Choose the downloaded factory sample pack used by Beat Lab's factory presets.")
        }
        "drum_lock" => tr!("Prevent replacing a Drum Swap slot while that slot is playing."),
        "drum_titles" => tr!("Show the source track title on Drum Swap slots."),
        "drum_source" => tr!("Capture either the separated drums or the original track audio."),
        "drum_dir" => tr!(
            "Choose where drum captures are saved. Empty uses the default OpenFadeR Drums folder."
        ),
        "video" => tr!(
            "Show the VIDEO workspace for video-track playback and mixing. It requires a working ffmpeg installation."
        ),
        "lighting" => tr!(
            "Show the LIGHTING workspace for fixtures and DMX scenes. Lighting is beta; check your fixture and network configuration before use."
        ),
        "rec_location" => tr!(
            "Choose the folder for recorded mixes. Empty uses the default OpenFadeR music folder."
        ),
        "rec_threshold" => tr!(
            "Set the level treated as silence for recording start, stop and splitting. Very quiet music may fall below this threshold."
        ),
        "rec_start_on_audio" => tr!(
            "Start recording when REC is pressed, or wait for incoming audio above the silence threshold."
        ),
        "rec_end_on_silence" => {
            tr!("Stop recording manually or after twenty seconds below the silence threshold.")
        }
        "rec_split" | "rec_split_secs" => tr!(
            "Split a recording into separate files when silence lasts long enough. Set the silence threshold and duration carefully for quiet tracks."
        ),
        "rec_optimise" => tr!(
            "Adjust the recorded file's audio level after recording finishes. This is file processing, not a live mixer gain control."
        ),
        "rec_import" | "rec_tag_window" => tr!(
            "Import a finished recording into the Collection and optionally open its tag information."
        ),
        "rec_cue" | "rec_tracklist" | "rec_controls" => tr!(
            "Save a cue sheet, track list or control log beside the recorded audio for later review."
        ),
        "rec_min_track_secs" => {
            tr!("Ignore very short deck appearances when building a recording's track list.")
        }
        "export_loop_hz" => tr!("Choose the sample rate when exporting a loop as a WAV file."),
        "check_updates" | "update_channel" => tr!(
            "Check for updates automatically and choose stable releases or beta pre-releases. Beta versions may change more often."
        ),
        "backup_before_update" => tr!(
            "Back up the Collection before an application update. Library backups do not include your music files."
        ),
        "low_power" => tr!(
            "Reduce analysis concurrency and sampler memory on smaller machines. Auto follows the device profile; On applies the limits explicitly."
        ),
        "add" => tr!(
            "Connect a library file or music folder to the Collection. Library formats controls which external formats are available."
        ),
        "djs.reset" => tr!(
            "Restore the selected player-settings scope to its defaults. Review the scope before resetting."
        ),
        "djs.beh.delete_tracks" => tr!(
            "Also remove a playlist's tracks from the device when deleting that playlist. Leave off if other playlists still need the files."
        ),
        "djs.beh.import_history" | "djs.beh.delete_history_after_import" => tr!(
            "Import player history into the Collection and optionally remove the device's history after a successful import."
        ),
        "djs.beh.sync_cues" => tr!(
            "Keep preparation data synchronized between a Collection track and its linked device copy. Resolve conflicts before overwriting either copy."
        ),
        "djs.beh.preview" | "djs.beh.sync_ask" | "djs.beh.review_imports" => tr!(
            "Review planned device writes, playlist synchronization or Collection changes before they are applied."
        ),
        "djs.beh.backup_before_write" | "djs.beh.auto_backup" => tr!(
            "Keep a backup of a device library before rewriting or removing it. This is separate from backing up the audio files."
        ),
        "djs.beh.check_on_eject" => {
            tr!("Check the device library before ejecting and warn if it needs attention.")
        }
        "djs.beh.computer_name" => {
            tr!("Set the computer name stored with player-related library information.")
        }
        "lib_search_user" | "lib_search" => tr!(
            "Choose folders searched when relocating missing music. Separate multiple paths with semicolons; this does not move the files."
        ),
        "lib_db" => tr!(
            "Choose the library path opened at startup when no path is supplied. Changing this path does not move existing music."
        ),
        "lib_mytag_comment" => tr!(
            "Include your custom Tags in track comments. This changes comment metadata, not the audio."
        ),
        "lib_dropdown" => tr!("Open the track's context menu by holding it for one second."),
        "lib_taglist_load" | "lib_taglist_quit" => tr!(
            "Remove entries from the temporary Tag List after loading or quitting. The tracks remain in the Collection and on disk."
        ),
        "lib_artwork_embed" => tr!(
            "Write downloaded artwork into supported audio-file tags. Keep a backup of files whose metadata you edit."
        ),
        "lib_protect" => tr!(
            "Block accidental library edits and deletions. Turn protection off deliberately when you need to change protected data."
        ),
        "lib_edit" => tr!("Allow library names to be edited by double-clicking them."),
        "lib_health_long" | "lib_health_kbps" => tr!(
            "Set the library-health thresholds for very long tracks or low bitrate. Zero disables that check."
        ),
        "inbox_folders" => tr!(
            "Watch these folders for new music and ZIP files to add to INBOX. Separate multiple folders with semicolons."
        ),
        "inbox_move_files" => tr!(
            "Physically move audio files as tracks change workflow stages. Leave off to keep original paths; review the root folder and path pattern before enabling."
        ),
        "inbox_root" | "inbox_pattern" => tr!(
            "Choose where moved INBOX audio is stored and how its path is named. These values are used only when Move the file with the track is enabled."
        ),
        "djs.apply" => tr!(
            "Write the selected player-setting defaults using this format's supported operation."
        ),
        "djs.read" => tr!("Read the supported player settings from the selected device."),
        _ if key.starts_with("ext_") => {
            tr!("Choose the hardware output pair for this deck when using an external mixer.")
        }
        _ if key.starts_with("ctl_driver:") => tr!(
            "Auto lets a recognizing controller driver claim the MIDI port, otherwise it uses a mapping. Choose an explicit driver only when automatic selection is unsuitable."
        ),
        _ if key.starts_with("conn.") && key.ends_with(".mode") => tr!(
            "Import reads the source into the Collection. Two-way also writes supported edits back to that source; keep a backup before enabling writeback."
        ),
        _ if key.starts_with("conn.") && key.ends_with(".wins") => tr!(
            "Choose which copy wins when source and Collection metadata differ. Newest uses the recorded modification times."
        ),
        _ if key.starts_with("conn.") && key.ends_with(".act") => tr!(
            "Sync now reads the connected source again. Disconnect stops synchronization but keeps imported tracks in the Collection."
        ),
        _ if key.starts_with("det.") => tr!(
            "Connect this detected library so its supported tracks and playlists can be synchronized into the Collection."
        ),
        _ => "",
    }
}
