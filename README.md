<div align="center">

  <img src="parrot.webp" alt="Parrot" width="120" />

# Parrot: Russian & OpenAI Text-to-Speech

**Highlight text in any app, press a shortcut, hear it read aloud**

A fork of [rishiskhare/parrot](https://github.com/rishiskhare/parrot) that replaces
Kokoro with two engines: **Silero** for Russian, running entirely on your machine,
and **OpenAI** for the highest quality, over the network.

![Platform](https://img.shields.io/badge/platform-macOS%20%7C%20Windows%20%7C%20Linux-lightgrey)
![License](https://img.shields.io/badge/license-MIT-red)
![Version](https://img.shields.io/badge/version-26.2.5-blue)

</div>

---

## What is Parrot? 🦜

Parrot reads your selected text aloud. Select text anywhere, press a shortcut, and it speaks.

The backend is written in Rust and the engine is chosen at runtime, so the two
engines are interchangeable:

|  | **Silero** | **OpenAI** |
| --- | --- | --- |
| Runs | on your machine | on OpenAI's servers |
| Languages | Russian and other CIS languages | most major languages |
| Voices | 29 Russian, 60 in total | 11 |
| Cost | none | billed per use |
| Needs | a Python environment (see below) | an API key |
| Your text | never leaves the machine | is sent to OpenAI |

### Why this fork exists

Upstream Parrot uses Kokoro, which has no Russian at all. Silero does, and is the
best free Russian TTS available — but it is published only as a PyTorch package,
with no ONNX export, so it cannot run in the ONNX pipeline upstream is built on.
This fork therefore runs Silero in a Python sidecar process and drops the ONNX
dependency entirely.

### How It Works

1. **Select text** in any application
2. **Press the shortcut** (default: `Option+Space` on macOS, `Ctrl+Space` on Windows/Linux)
3. A small overlay appears while Parrot synthesizes and plays the audio
4. Press `Option+P` to pause/resume (all shortcuts are customizable)

## Installation

Download the installer from [Releases](../../releases), then set up whichever engine you want.

### Silero (local, Russian)

Silero needs PyTorch, which is too large to ship inside the installer, so it is
installed separately:

```sh
scripts/setup-silero.sh
```

This creates a Python environment and downloads the model (~90 MB) into the app
data directory, then verifies that synthesis works. It takes a few minutes and
about 1 GB of disk, mostly PyTorch. Re-running it is harmless.

Then pick **Silero** in **Settings → Models**.

### OpenAI (network, all languages)

Paste an API key into **Settings → Models → OpenAI TTS**. Nothing else to install.
The key is kept in the app's settings file; if you would rather it not sit on disk
in clear text, set `OPENAI_API_KEY` in the environment instead — that takes
precedence over the stored value.

## Features

- **Two engines, one shortcut:** local Silero or OpenAI, switched in settings
- **Works in any app:** reads selected text from browsers, editors, PDFs, terminals, anywhere
- **Streaming playback:** audio starts playing before the full text has been synthesized
- **Spend tracking:** running estimate of OpenAI cost, with a monthly cap you set
- **Pause & resume:** pause and resume playback mid-sentence with a keyboard shortcut
- **Floating overlay:** a lightweight indicator shows speaking status with pause controls

## Engines

### Silero

Ships as `v5_cis_base`: 60 voices, 29 of them Russian, plus Bashkir, Tatar,
Kazakh, Kyrgyz, Uzbek, Tajik, Azerbaijani, Armenian, Georgian, Belarusian,
Ukrainian, Chuvash, Udmurt, Erzya, Moksha, Yakut, Kalmyk, Khakas and Kabardian.
Stress and `ё` placement come from Silero's own neural accentuation, which is
what makes Russian sound right rather than merely intelligible.

Synthesis runs roughly 20× faster than real time on a modern CPU, so a sentence
is ready in about a tenth of a second. Set `SILERO_MODEL_URL` before running the
setup script to use a different Silero package — `v4_ru` is a fifth of the size
and several times faster, with fewer voices.

The model runs in a Python sidecar that stays warm between requests. It starts
with the app and exits with it.

### OpenAI

Eleven voices — Alloy, Ash, Ballad, Coral, Echo, Fable, Nova, Onyx, Sage,
Shimmer, Verse — across three models:

| Model | Notes |
| --- | --- |
| `gpt-4o-mini-tts` | Best quality. Takes free-form delivery instructions ("speak calmly"). |
| `tts-1` | Lowest latency. Honours the playback speed slider directly. |
| `tts-1-hd` | Higher fidelity than `tts-1`, twice the price. |

Audio is requested as raw 24 kHz PCM, matching Silero's output rate, so playback
behaves identically whichever engine is active.

#### Spending

OpenAI does not expose a remaining balance to a project API key, so the app
cannot show one. Instead it prices every request locally from OpenAI's published
rates and tracks the total for the calendar month. Set a monthly cap and the app
reports what is left against it and stops synthesizing once it is reached — the
check happens before the request, so an exhausted budget costs nothing.

Treat the figure as an estimate, not a bill. For a hard limit, set one in the
OpenAI dashboard as well.

## Keyboard Shortcuts

All shortcuts are fully customizable in **Settings → General**.

| Action                  | macOS          | Windows / Linux |
| ----------------------- | -------------- | --------------- |
| Speak selected text     | `Option+Space` | `Ctrl+Space`    |
| Pause / resume playback | `Option+P`     | `Alt+P`         |
| Open settings           | `Cmd+,`        | `Ctrl+,`        |
| Open debug panel        | `Cmd+Shift+D`  | `Ctrl+Shift+D`  |

The pause/resume shortcut is only active while Parrot is playing. It can be customized or disabled in **Settings → General**.

## Settings Overview

| Category               | Options                                                                    |
| ---------------------- | -------------------------------------------------------------------------- |
| **General**            | Shortcuts, voice, output device, audio feedback                            |
| **Models**             | Switch engines; OpenAI key, model, instructions, proxy, monthly budget     |
| **Advanced → App**     | Start hidden, autostart, tray icon, overlay position, model unload timeout |
| **Advanced → Speech**  | Worker threads, playback speed, fast first response                        |
| **Advanced → History** | Entry limit, auto-delete period                                            |
| **History**            | Browse, replay, copy, and delete past utterances                           |
| **Debug**              | Log level, keyboard implementation, diagnostics                            |

## Command-Line Interface

Parrot supports CLI flags for scripting and window manager integration. Remote control flags are delivered to the already-running instance; you do not need to keep a second instance running.

```
parrot [FLAGS]
```

| Flag                     | Description                                           |
| ------------------------ | ----------------------------------------------------- |
| `--toggle-transcription` | Toggle TTS on/off in the running instance             |
| `--start-hidden`         | Launch without showing the main window                |
| `--no-tray`              | Launch without a tray icon (closing the window quits) |
| `--debug`                | Enable verbose trace logging                          |

**Example: bind to a window manager shortcut:**

```sh
parrot --toggle-transcription
```

> **macOS:** When using the app bundle, invoke the binary directly:
>
> ```sh
> /Applications/Parrot.app/Contents/MacOS/Parrot --toggle-transcription
> ```

## Linux Notes

### Text Input Tools

For reliable text pasting on Linux, install the appropriate tool for your display server:

| Display Server | Recommended | Install                    |
| -------------- | ----------- | -------------------------- |
| X11            | `xdotool`   | `sudo apt install xdotool` |
| Wayland        | `wtype`     | `sudo apt install wtype`   |
| Both           | `dotool`    | `sudo apt install dotool`  |

`dotool` requires adding your user to the `input` group: `sudo usermod -aG input $USER` (log out and back in after).

### Global Shortcuts on Wayland

Parrot's built-in global shortcut capture has limited support on Wayland. The recommended approach is to configure your desktop environment or window manager to invoke the CLI flag instead.

**GNOME:**

1. Open **Settings > Keyboard > Keyboard Shortcuts > Custom Shortcuts**
2. Add a new shortcut with the command `parrot --toggle-transcription`

**KDE Plasma:**

1. Open **System Settings > Shortcuts > Custom Shortcuts**
2. Create a new **Command/URL** shortcut with `parrot --toggle-transcription`

**Sway / i3:**

```ini
bindsym $mod+o exec parrot --toggle-transcription
```

**Hyprland:**

```ini
bind = $mainMod, O, exec, parrot --toggle-transcription
```

### Unix Signal Control

You can also send signals directly to the Parrot process, useful for hotkey daemons that manage their own keybindings:

| Signal    | Action     |
| --------- | ---------- |
| `SIGUSR1` | Toggle TTS |
| `SIGUSR2` | Toggle TTS |

```sh
pkill -USR2 -n parrot   # toggle TTS
```

### Other Linux Notes

- The speaking overlay is disabled by default on Linux (`Overlay Position: None`) because some compositors treat it as the active window and steal focus.
- If the app fails to start, try setting `WEBKIT_DISABLE_DMABUF_RENDERER=1`.
- If you see `error while loading shared libraries: libgtk-layer-shell.so.0`, install the runtime package:

  | Distro        | Package               | Command                                |
  | ------------- | --------------------- | -------------------------------------- |
  | Ubuntu/Debian | `libgtk-layer-shell0` | `sudo apt install libgtk-layer-shell0` |
  | Fedora/RHEL   | `gtk-layer-shell`     | `sudo dnf install gtk-layer-shell`     |
  | Arch          | `gtk-layer-shell`     | `sudo pacman -S gtk-layer-shell`       |

## Building from Source

**Prerequisites:** [Rust](https://rustup.rs/) (latest stable), [Bun](https://bun.sh/)

```sh
# Clone the repository
git clone https://github.com/rishiskhare/parrot
cd parrot

# Install frontend dependencies
bun install

# Run in development mode
bun run tauri dev

# Build a release binary
bun run tauri build
```

> On macOS, if you hit a CMake error:
>
> ```sh
> CMAKE_POLICY_VERSION_MINIMUM=3.5 bun run tauri dev
> ```

## Architecture

Parrot is built with [Tauri 2](https://tauri.app/), a Rust backend with a React/TypeScript frontend. The entire synthesis and audio pipeline runs in Rust, which keeps CPU and memory usage low even during continuous playback.

```
src-tauri/src/
├── managers/
│   ├── tts.rs          # Streaming TTS synthesis and audio playback
│   ├── model.rs        # Model download, extraction, and lifecycle
│   └── history.rs      # Utterance storage and retention
├── audio_toolkit/      # Audio device enumeration and resampling
├── commands/           # Tauri IPC handlers (frontend ↔ backend)
├── settings.rs         # Persistent settings with serde
├── shortcut/           # Global hotkey capture (Tauri + HandyKeys backends)
└── overlay.rs          # Floating speaking indicator window

src/
├── components/settings/   # Settings UI (35+ components)
├── overlay/               # Speaking overlay window
└── stores/settingsStore.ts  # Zustand state management
```

**Key dependencies:** `rodio` (audio playback), `cpal` (audio devices), `reqwest` (OpenAI calls), `tauri-specta` (type-safe IPC)

## Acknowledgments

This is a fork of [Parrot](https://github.com/rishiskhare/parrot) by
[Rishi Khare](https://github.com/rishiskhare), which is itself a fork of
[Handy](https://github.com/cjpais/Handy) by [CJ Pais](https://github.com/cjpais).
Both are MIT licensed and between them provided the Tauri architecture, the audio
pipeline, and the UI this builds on.

Russian speech comes from [Silero](https://github.com/snakers4/silero-models).

## License

MIT. See [LICENSE](LICENSE) for full text.

Parrot is a derivative work of [Handy](https://github.com/cjpais/Handy) (© 2025 CJ Pais). Both are distributed under the MIT License.
