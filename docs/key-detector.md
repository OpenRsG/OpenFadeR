# Key detector

PREFERENCES > ANALYSIS > KEY DETECTOR chooses what the key part of an analysis runs. The setting only matters while KEY is on.

| Detector | What it is |
|---|---|
| CLASSIC | Chroma of the track matched against key profiles (the default). |
| NEURAL | A small convolutional network over a constant-Q spectrogram, run by ONNX Runtime on the CPU (one thread per analysis, no GPU). |

NEURAL is available in builds with the `stems` feature (ONNX Runtime). Without it the row is disabled and says so.

## The model

- Network: the key CNN of Korzeniowski & Widmer, "Genre-Agnostic Key Classification With Convolutional Neural Networks" (ISMIR 2018): 24 classes, 0.5 M parameters.
- Weights: the published checkpoint of `a1ex90/MusicalKeyCNN` (commit `61d8fc1`), **MIT licence**, trained on the GiantSteps-MTG key set (electronic dance music from Beatport previews, community-corrected keys). Expect lower accuracy on other genres.
- File: `keynet.onnx`, 1,852,740 bytes, sha256 `da9b9e947c9434069494302886f2e88e6b5cdd5e8c6acbe5eb97f693bf21fe17`. `examples/keynet_export.py` converts the checkpoint to it.
- Front end: our own Rust code (`src/analysis/key/cqt.rs`): stereo to mono, integer decimation to about 11 kHz, constant-Q spectrogram of 105 bins, as the model documents.

The model downloads once into the models cache the first time NEURAL is chosen (or is already chosen at start-up), with its progress on the track info status line. The size and sha256 are checked; a wrong or cut-off file is deleted. Offline or on a wrong file, a toast says so and the analysis stays CLASSIC (the preference is not changed); a run started meanwhile says "CLASSIC" in the status line. To install by hand, put the file at the path the message names, or set `OPENFADER_KEY_MODEL` to a model file and `OPENFADER_KEY_MODEL_URL` to a mirror (size and sha256 still checked).

The analysis cache records which detector produced a key: a CLASSIC key never answers a NEURAL request and the other way round, so switching the setting and re-analysing (ANALYZE, or bulk analysis) recomputes the key only.

## Accuracy

Measured with `examples/key_eval.rs` on the public GiantSteps key set (604 tracks, `.LOFI.mp3` previews, the dataset's key annotations):

| Detector | Correct | Fifth | Relative | Parallel | Other | MIREX weighted |
|---|---|---|---|---|---|---|
| CLASSIC | 55.1 % | 11.9 % | 8.6 % | 6.1 % | 18.2 % | 64.9 |
| NEURAL | 66.2 % | 8.9 % | 6.5 % | 4.6 % | 13.7 % | 73.6 |

NEURAL's model was trained on a dataset that overlaps this genre (whether the GiantSteps test tracks were excluded from its training split is not verified); on other genres the gap to CLASSIC may be smaller.

Run it yourself: `cargo run --release --example key_eval -- AUDIO_DIR LABEL_DIR [THREADS]`.
