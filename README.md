# Clipperino

A small, local video editor for Linux. Rust owns the project format, edits, transcription workflow, and rendering. GPUI draws the interface. The CLI and app read the same JSON project, so an edit made in a terminal appears in an open window.

The first version edits **multiple source videos** on one linked video/audio timeline. It cuts and removes ranges; it does not yet add overlays, transitions, or independent audio clips. It assumes English speech from one speaker. The default theme is light, with a dark mode and custom JSON themes. Word timestamps are enabled by default and can be turned off with `/transcript_word_timestamps` in the user config; Whisper's word timing is approximate.

## Requirements

- Rust stable and Cargo
- `ffmpeg` and `ffprobe` for media inspection, frames, silence detection, and export
- `mpv` for preview audio
- `whisper-cli` from [whisper.cpp](https://github.com/ggml-org/whisper.cpp) for local transcription
- `curl` if you use the built-in model download command

On Arch Linux, the runtime tools are available as `ffmpeg`, `mpv`, and `whisper-cpp` packages. The model downloads once; transcription and all editing then run locally. The model path and executable names are configurable.

## Quick start

```sh
cargo run -p clipperino -- new
cargo run -p clipperino -- model-download
cargo run -p clipperino -- import /path/to/take.mp4
cargo run -p clipperino -- transcribe asset-1
cargo run -p clipperino -- auto-cut asset-1 --dry-run
cargo run -p clipperino -- auto-cut asset-1
cargo run -p clipperino-app -- project.json
```

The app can also create `project.json`, import videos with its file picker, download the model, transcribe the selected asset, and apply auto-cut. Drag panel headers onto other panels to swap them. Drag the bars between panels to resize them. The layout and theme choice save to the user config. Click or drag across the timeline ruler or tracks to scrub with the playhead. The timeline has linked video and audio lanes, In/Out marks, range removal, and Undo.

Export with:

```sh
cargo run -p clipperino -- render output.mp4
```

For another project, add `--project /path/to/project.json` before the command. The CLI writes JSON to stdout and errors to stderr. Useful agent commands include:

```sh
clipperino inspect
clipperino timeline
clipperino frame --at-ms 1200 --output /tmp/frame.png
clipperino frame --asset asset-1 --at-ms 1200 --output /tmp/source-frame.png
clipperino contact-sheet asset-1 --every-ms 5000 --output /tmp/contact.png
clipperino cut 2500 3200
clipperino undo
clipperino project get /settings
clipperino project set /settings/silence_min_ms 900
clipperino config show
clipperino config set /theme '"dark"'
clipperino layout-swap assets transcript
clipperino theme-export light /tmp/my-theme.json
clipperino schema
```

Set `/custom_theme` in the user config to the theme file path. Relative theme paths resolve from the config directory. Editing the theme or project JSON while the app is open reloads it. CLI project edits validate the entire project and save atomically; `undo` restores the previous edit. The JSON project stores source timestamps in integer milliseconds. Timeline positions are derived by adding ordered segment durations, so cuts never rewrite transcript source times.

User config lives at `$XDG_CONFIG_HOME/clipperino/config.json`, or `~/.config/clipperino/config.json`. Local models live at `$XDG_DATA_HOME/clipperino/models/`, or `~/.local/share/clipperino/models/`. The project cache and undo history live beside `project.json` in `.clipperino-cache/`.

Preview video uses a 24 fps proxy stream from FFmpeg, with mpv playing the source audio. It is intended for selecting cuts; final export re-encodes from the original media. Playback can briefly pause at a cut between source segments. For precise review, render the project and check the output.
