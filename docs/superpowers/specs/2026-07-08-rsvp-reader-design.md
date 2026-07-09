# RSVP Speed Reader — Design

Date: 2026-07-08. Approved by user in conversation.

## Goal
Desktop app to improve reading speed on non-fiction PDFs using RSVP
(one word flashed at a fixed point).

## Stack
Pure Rust: `eframe` (egui) GUI, `pdf-extract` for text, `rfd` file dialog,
`serde`/`serde_json` + `dirs` for progress persistence. Single `main.rs`.

## Behavior
- Open PDF → extract all text → split into words (punctuation attached),
  paragraph boundaries flagged.
- Display one word, large font, ORP letter (~35% in) drawn red and pinned to a
  fixed horizontal position.
- Timing: base = 60s/WPM. Multipliers: >8 chars ×1.4; `,;:` ×1.5; `.!?` ×2;
  paragraph end ×2.5 (max wins).
- Controls: Space play/pause, ←/→ jump ±10 words, WPM slider 100–1000,
  scrubbable position slider, word counter.
- Persistence: `{path: {index, wpm}}` JSON in OS config dir, saved on pause
  and exit; reopening a PDF resumes.
- Error: no extractable text → message ("scanned PDF?") instead of blank.

## Non-goals (v1)
Chapter nav, stats, OCR, EPUB, theming.
