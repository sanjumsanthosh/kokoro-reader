<div align="center">

# 🎧 Kokoro Reader

**Your documents, read aloud — on your machine, with no cloud in the loop.**

A macOS desktop reader for long Markdown: it renders the text, narrates it with the
Kokoro‑82M speech model running locally, and keeps every word of audio on disk so
re-reading costs nothing.

<br>

[![Tauri](https://img.shields.io/badge/Tauri-2-24C8DB?logo=tauri&logoColor=white)](https://tauri.app/)
[![Rust](https://img.shields.io/badge/Rust-core-000000?logo=rust&logoColor=white)](./src-tauri)
[![TypeScript](https://img.shields.io/badge/TypeScript-webview-3178C6?logo=typescript&logoColor=white)](./src)
[![Offline](https://img.shields.io/badge/network-after%20setup%2C%20none-brightgreen)](#privacy)
[![Platform](https://img.shields.io/badge/platform-macOS-lightgrey)](#requirements)

</div>

---

## Why

Agent output is long. Review docs are long. Half of what you "read" is skimmed and
forgotten. Kokoro Reader turns a Markdown file into something you can *listen* to while
walking, cooking, or staring at a different screen — and because the model and the audio
cache are local, nothing you read leaves the machine.

It started as a way to hear what Codex wrote without switching windows.

---

## What it does

| | |
|---|---|
| **Offline narration** | Kokoro‑82M (fp32, 24 kHz) runs in Rust through `kokoro-en`. One download, then zero network. |
| **Section‑aligned speech** | A document is split into sections, each with its own narration text. Click a paragraph, hear that paragraph. |
| **Audio cache queue** | Speech is synthesized once per section and cached as WAV. The queue shows what is cached, queued, caching, or failed — grouped by article. |
| **Real player** | Play/pause, stop, three voices, 0.5–2.0× speed, per‑section restart, jump‑to‑marker, 5‑second seek. |
| **Two‑pane editor** | Markdown on the left, the speech layer on the right, with alignment diagnostics when they drift apart. |
| **Library + read state** | Articles table with date added, date marked read, cached audio size, and batch deletion. |
| **Honest storage** | Physical disk accounting for models, audio cache, project files, and app data — shared WAV files counted once, not per article. |
| **Round‑trip to Codex** | A "Back to Codex" action returns you to the agent that produced the document. |

---

## How it works

```
Markdown  ──►  sections  ──►  narration text  ──►  Kokoro (Rust)  ──►  WAV cache
   │                │                                     │                │
   └── rendered ────┴── highlighted while playing ◄───────┴── playback ◄───┘
```

- **The webview owns interaction** — rendering, playback position, cache panel, editor.
- **Rust owns the facts** — project metadata, file deletion, actual disk accounting, inference.
- Sections are separated by `<!-- kokoro-reader-section -->`; each section's speech text is
  stored next to its source, so a re-read never has to re‑synthesize.

### On disk

```
~/Library/Application Support/com.local.kokororeader/
├── projects/<id>/source.md        # the Markdown you read
├── projects/<id>/narration.txt    # the speech layer, per section
├── cache/…                        # synthesized WAV clips
└── models/kokoro-82m-v1.0-fp32    # one-time download
```

**Guard rails:** 2 MB max document, 510 tokens per sentence, 2 GB audio cache ceiling.

---

## Keyboard

| Key | Action |
|---|---|
| `Space` | Pause / resume |
| `S` | Restart section |
| `J` / `K` | Next / previous section |
| `0` | Restart document |
| `T` | Jump to marker |
| `←` / `→` | Back / forward 5 seconds |
| `Ctrl/Cmd + F` | Find in document |

---

## Requirements

- **macOS** — built and tested on Apple Silicon; the storage and reveal actions target Finder
- **Rust** toolchain and **Bun**
- **Tauri 2** prerequisites (`xcode-select --install`)

## Run it

```bash
git clone https://github.com/sanjumsanthosh/kokoro-reader
cd kokoro-reader
bun install
bun run tauri dev
```

First launch shows **Set up offline speech** — download the model once, then everything runs locally.

## Test and build

```bash
bun test                                        # webview logic (cache queue, text, playback)
cargo test --manifest-path src-tauri/Cargo.toml  # Rust core
bun run tauri build                             # packaged app
```

---

## Privacy

- Network is used **once**, to download the speech model.
- Documents, narration, and audio stay in the app support directory.
- No telemetry, no accounts, no sync.

---

## Limitations

- macOS first: the Windows/Linux path is untested.
- Kokoro is English-only in this build.
- Very long documents are chunked at section boundaries; the 2 MB guard rejects anything
  wilder than a book chapter.

---

## Roadmap

- [ ] Windows and Linux builds
- [ ] Import EPUB / HTML
- [ ] Per-voice pronunciation overrides for code and identifiers
- [ ] Export an article to a single MP3 with chapter markers

---

## License

No license file yet — add one before distributing builds.

## Author

Built by [@sanjumsanthosh](https://github.com/sanjumsanthosh).
