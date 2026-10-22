# Development

Commands for working on the reconstruction: research inputs, scripted diagnostic runs and
traces. Playing and building are in the [README](../README.md).

## Local inputs

No proprietary executable, symbols or game assets belong in this repository. Point the tools
at a locally obtained reference build:

```powershell
$env:SKATE2_GAME_ROOT = '<the folder of the game''s .xex, which holds its data folder>'
$env:SKATE2_MAP_FILE = '<path to sk82_na_m.map>'
```

Without `SKATE2_GAME_ROOT` the game asks for the `.xex` once and keeps its folder in
`config.json`.

```powershell
cargo test --workspace
cargo run -p skate2-pc -- --check-data
cargo run --release -p skate2-pc -- --inventory-bam-pres
cargo run --release -p skate2-pc -- --inventory-bam-sim
cargo run --release -p skate2-pc -- --inventory-bam-sim-resident
cargo run --release -p skate2-pc -- --inventory-render-assets
cargo run -p skate2-symbols -- $env:SKATE2_MAP_FILE
```

## Building on macOS

The PC app builds on Apple Silicon with Homebrew's SDL3 linked dynamically
(the Windows and Linux builds keep building SDL3 statically from source; see
the platform sections of `apps/skate2-pc/Cargo.toml`):

```sh
brew install sdl3
cargo build --release -p skate2-pc
SKATE2_GAME_ROOT='<folder of default.xex>' target/release/skate2-pc
```

Without `SKATE2_GAME_ROOT` the game shows the same `.xex` file dialog as on
Windows and remembers the folder in `config.json`. Pads go through the SDL3
platform; audio through cpal (CoreAudio).

## Running the game

`skate2-pc.exe` with no arguments is a player's launch: the quiet game, asking for the game's
folder when it is not known and showing a fatal error in a message box.
`skate2-pc --live-skating --quiet` suppresses routine console traces and profiling output and
writes no log files; fatal errors, including unimplemented state or intent ids, still print.
Omit `--quiet` to see diagnostic traces.

## Scripted runs

`--live-completions <n>` counts completed simulation frames that reached the window;
`--live-capture <file.png>` saves the last one, and the run then exits:

```powershell
cargo run --release -p skate2-pc -- --live-skating --live-completions 600 --live-capture artifacts/controlled-live-600.png
```

- `--live-ollie-demo --live-completions 26 --live-hold` replays the source data's Ollie
  input through the selected input path and holds the airborne frame until Escape.
- `--live-y-demo --live-completions 360` presses raw FaceY once at frame 33, then releases
  it: a bounded off-board, bail and recovery window.
- `--live-push-demo --live-completions 121` holds the original data's raw A sample for corpus
  steps 32 to 43 and checks the completed Action and physical state at the press and the
  board's travel and deck speed at step 119.

Each demo overrides controller 0 only while its flag is present.

## Traces

```powershell
$env:SKATE2_TRACE_XINPUT = '1'
$env:SKATE2_TRACE_SELECTED_STATES = '1'
.\target\release\skate2-pc.exe --live-skating *> artifacts\live-hands-on.log
```

`SKATE2_TRACE_XINPUT` logs every raw slot-0 pad packet and sample;
`SKATE2_TRACE_SELECTED_STATES` adds each completed frame's Action, Motion and physical state
ids, so a failed frame can be matched to the controller input that led to it. Traces leave
the simulation unchanged.

Area-specific diagnostics are documented with their area: online play in
[online/implementation.md](online/implementation.md#running-it), Party Play in
[partyplay/README.md](partyplay/README.md#7-port-status), replay and photo export in
[MEDIA_EXPORT.md](MEDIA_EXPORT.md). Graph state and condition names for reading traces are in
[NAMED_IDS.md](NAMED_IDS.md).
