# Speed — RSVP Speed Reader for PDFs

A fast, native desktop speed-reading app written in Rust. Open any PDF book and read it with **RSVP (Rapid Serial Visual Presentation)** — either as a moving highlight over the real PDF pages, or one word at a time at a fixed focus point. Built for getting through non-fiction books faster without losing comprehension.

## Features

- **PDF mode** — the actual rendered PDF page with a highlight sweeping word-by-word across it at your chosen pace, like a guided finger. Click any word on the page to jump the cursor there.
- **Reader mode** — clean, e-reader-style reflowed text with a karaoke highlight; read words are dimmed so you never lose your place.
- **Focus mode** — classic Spritz-style RSVP: one word flashed at a fixed point, with the optimal-recognition-point letter pinned and colored so your eyes never move.
- **Smart pacing** — automatic slow-downs on long words, commas, sentence ends, and paragraph breaks.
- **Chapter navigation** — the book's real table of contents, pulled from the PDF outline.
- **Jump to any page** and scrub anywhere with the progress bar.
- **Resumes where you left off** — position and WPM are saved per book.
- **100–1000 WPM**, adjustable live while reading.

## Requirements

- [Rust](https://rustup.rs) (to build)
- [Poppler](https://poppler.freedesktop.org) — used for text extraction and page rendering

```sh
# macOS
brew install poppler

# Debian/Ubuntu
sudo apt install poppler-utils
```

## Build & Run

```sh
git clone https://github.com/SandyCodeSenpai/speed.git
cd speed
cargo run --release
```

The release binary lands at `target/release/rsvp-reader`.

## Usage

1. Click **Open PDF…** and pick a book.
2. Press **Space** to start reading.
3. Adjust the WPM slider until it feels slightly too fast — that's the training zone.

### Keyboard shortcuts

| Key | Action |
|-----|--------|
| `Space` | Play / pause |
| `←` | Jump back 10 words |
| `→` | Jump forward 10 words |

### Modes

| Mode | What you see | Best for |
|------|--------------|----------|
| **PDF** | Real page, moving highlight | Books with layout, figures, context |
| **Reader** | Reflowed clean text, karaoke highlight | Distraction-free long sessions |
| **Focus** | One word at a fixed point | Maximum speed training |

## How it works

- `pdftotext -bbox` (Poppler) extracts every word with its exact bounding box on the page.
- `pdftoppm` renders pages to images on a background thread — the current and next page are pre-rendered so playback never stutters.
- The PDF outline (table of contents) is read with [`lopdf`](https://crates.io/crates/lopdf).
- The UI is [`egui`](https://github.com/emilk/egui)/`eframe` — pure Rust, single binary, no web stack.
- Reading position per book is stored in your OS config directory (`rsvp-reader/progress.json`).

## Limitations

- Scanned/image-only PDFs are not supported (no OCR).
- The highlight also visits headers and page numbers, since they are words on the page.
- Pages render at a fixed 144 DPI.

## License

[MIT](LICENSE)
