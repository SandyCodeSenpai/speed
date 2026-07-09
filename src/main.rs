#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use eframe::egui::{self, Align2, Color32, FontId, Key};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([900.0, 500.0]),
        ..Default::default()
    };
    eframe::run_native(
        "RSVP Reader",
        options,
        Box::new(|_cc| Ok(Box::new(App::load()))),
    )
}

struct Word {
    text: String,
    para_end: bool,
}

#[derive(Serialize, Deserialize, Clone, Copy)]
struct Saved {
    index: usize,
    wpm: f32,
}

struct App {
    words: Vec<Word>,
    idx: usize,
    playing: bool,
    wpm: f32,
    last_advance: Instant,
    pdf_path: Option<PathBuf>,
    error: Option<String>,
    progress: HashMap<String, Saved>,
}

fn store_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("rsvp-reader/progress.json")
}

fn split_words(text: &str) -> Vec<Word> {
    let mut words = Vec::new();
    for para in text.split("\n\n") {
        let toks: Vec<&str> = para.split_whitespace().collect();
        let last = toks.len().saturating_sub(1);
        for (i, t) in toks.iter().enumerate() {
            words.push(Word {
                text: (*t).to_string(),
                para_end: i == last,
            });
        }
    }
    words
}

fn extract_text(path: &Path) -> Result<String, String> {
    // ponytail: prefer poppler's pdftotext when installed — it decodes
    // CID/Identity-encoded fonts (common in ebooks) that pdf-extract can't.
    if let Ok(out) = std::process::Command::new("pdftotext")
        .arg(path)
        .arg("-")
        .output()
    {
        if out.status.success() {
            let text = String::from_utf8_lossy(&out.stdout).into_owned();
            if !text.trim().is_empty() {
                return Ok(text);
            }
        }
    }
    // ponytail: pdf-extract can panic on malformed PDFs, so catch_unwind
    let path = path.to_path_buf();
    std::panic::catch_unwind(move || pdf_extract::extract_text(&path))
        .map_err(|_| "PDF parser crashed on this file".to_string())?
        .map_err(|e| e.to_string())
}

fn extract_pdf(path: &Path) -> Result<Vec<Word>, String> {
    let text = extract_text(path)?;
    let words = split_words(&text);
    if words.is_empty() {
        return Err("No extractable text — is this a scanned/image-only PDF?".into());
    }
    Ok(words)
}

/// Optimal recognition point: the letter your eye should land on, ~35% in.
fn orp_index(len: usize) -> usize {
    ((len as f32 - 1.0) * 0.35).round() as usize
}

fn delay_for(word: &Word, wpm: f32) -> Duration {
    let base = 60_000.0 / wpm;
    let mut m: f32 = 1.0;
    if word.text.chars().count() > 8 {
        m = 1.4;
    }
    match word.text.chars().last() {
        Some('.' | '!' | '?') => m = m.max(2.0),
        Some(',' | ';' | ':') => m = m.max(1.5),
        _ => {}
    }
    if word.para_end {
        m = m.max(2.5);
    }
    Duration::from_millis((base * m) as u64)
}

impl App {
    fn load() -> Self {
        let progress: HashMap<String, Saved> = std::fs::read_to_string(store_path())
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        App {
            words: Vec::new(),
            idx: 0,
            playing: false,
            wpm: 300.0,
            last_advance: Instant::now(),
            pdf_path: None,
            error: None,
            progress,
        }
    }

    fn save_progress(&mut self) {
        if let Some(path) = &self.pdf_path {
            self.progress.insert(
                path.to_string_lossy().into_owned(),
                Saved { index: self.idx, wpm: self.wpm },
            );
        }
        let p = store_path();
        let _ = std::fs::create_dir_all(p.parent().unwrap());
        if let Ok(json) = serde_json::to_string(&self.progress) {
            let _ = std::fs::write(p, json);
        }
    }

    fn open_pdf(&mut self, path: PathBuf) {
        self.playing = false;
        match extract_pdf(&path) {
            Ok(words) => {
                self.error = None;
                if let Some(saved) = self.progress.get(&path.to_string_lossy().into_owned()) {
                    self.idx = saved.index.min(words.len() - 1);
                    self.wpm = saved.wpm;
                } else {
                    self.idx = 0;
                }
                self.words = words;
                self.pdf_path = Some(path);
            }
            Err(e) => {
                self.error = Some(e);
                self.words.clear();
                self.pdf_path = None;
                self.idx = 0;
            }
        }
    }

    fn toggle_play(&mut self) {
        self.playing = !self.playing && !self.words.is_empty();
        if self.playing {
            self.last_advance = Instant::now();
        } else {
            self.save_progress();
        }
    }
}

impl eframe::App for App {
    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.save_progress();
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Advance the word on schedule.
        if self.playing {
            let delay = delay_for(&self.words[self.idx], self.wpm);
            if self.last_advance.elapsed() >= delay {
                if self.idx + 1 < self.words.len() {
                    self.idx += 1;
                    self.last_advance = Instant::now();
                } else {
                    self.playing = false;
                    self.save_progress();
                }
            }
            let next = delay_for(&self.words[self.idx], self.wpm)
                .saturating_sub(self.last_advance.elapsed());
            ctx.request_repaint_after(next);
        }

        // Keyboard shortcuts.
        if !ctx.wants_keyboard_input() {
            ctx.input(|i| {
                if i.key_pressed(Key::Space) {
                    self.toggle_play();
                }
                if i.key_pressed(Key::ArrowLeft) {
                    self.idx = self.idx.saturating_sub(10);
                }
                if i.key_pressed(Key::ArrowRight) && !self.words.is_empty() {
                    self.idx = (self.idx + 10).min(self.words.len() - 1);
                }
            });
        }

        egui::TopBottomPanel::top("top").show(ctx, |ui| {
            ui.horizontal(|ui| {
                if ui.button("Open PDF…").clicked() {
                    if let Some(path) = rfd::FileDialog::new()
                        .add_filter("PDF", &["pdf"])
                        .pick_file()
                    {
                        self.open_pdf(path);
                    }
                }
                if ui
                    .button(if self.playing { "Pause" } else { "Play" })
                    .clicked()
                {
                    self.toggle_play();
                }
                ui.add(
                    egui::Slider::new(&mut self.wpm, 100.0..=1000.0)
                        .step_by(25.0)
                        .text("WPM"),
                );
                if !self.words.is_empty() {
                    ui.label(format!("{} / {}", self.idx + 1, self.words.len()));
                }
            });
        });

        egui::TopBottomPanel::bottom("bottom").show(ctx, |ui| {
            if !self.words.is_empty() {
                let max = self.words.len() - 1;
                ui.spacing_mut().slider_width = ui.available_width() - 16.0;
                ui.add(
                    egui::Slider::new(&mut self.idx, 0..=max)
                        .show_value(false)
                        .trailing_fill(true),
                );
            }
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            let rect = ui.available_rect_before_wrap();
            let painter = ui.painter();
            let font = FontId::proportional(56.0);

            if let Some(err) = &self.error {
                painter.text(
                    rect.center(),
                    Align2::CENTER_CENTER,
                    err,
                    FontId::proportional(20.0),
                    Color32::LIGHT_RED,
                );
                return;
            }
            if self.words.is_empty() {
                painter.text(
                    rect.center(),
                    Align2::CENTER_CENTER,
                    "Open a PDF to start reading.\nSpace = play/pause   Left/Right arrows = jump 10 words",
                    FontId::proportional(20.0),
                    Color32::GRAY,
                );
                return;
            }

            let chars: Vec<char> = self.words[self.idx].text.chars().collect();
            let orp = orp_index(chars.len());
            let prefix: String = chars[..orp].iter().collect();
            let orp_ch: String = chars[orp].to_string();
            let suffix: String = chars[orp + 1..].iter().collect();

            // Pin the ORP letter at a fixed point so the eye never moves.
            let anchor = egui::pos2(rect.left() + rect.width() * 0.45, rect.center().y);
            let text_color = ui.visuals().strong_text_color();
            let orp_rect = painter.text(
                anchor,
                Align2::CENTER_CENTER,
                &orp_ch,
                font.clone(),
                Color32::from_rgb(220, 50, 50),
            );
            painter.text(
                orp_rect.left_center(),
                Align2::RIGHT_CENTER,
                &prefix,
                font.clone(),
                text_color,
            );
            painter.text(
                orp_rect.right_center(),
                Align2::LEFT_CENTER,
                &suffix,
                font.clone(),
                text_color,
            );

            // Guide ticks above and below the ORP.
            let tick = Color32::from_gray(100);
            painter.line_segment(
                [anchor - egui::vec2(0.0, 60.0), anchor - egui::vec2(0.0, 44.0)],
                (2.0, tick),
            );
            painter.line_segment(
                [anchor + egui::vec2(0.0, 44.0), anchor + egui::vec2(0.0, 60.0)],
                (2.0, tick),
            );
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn orp_lands_about_a_third_in() {
        assert_eq!(orp_index(1), 0);
        assert_eq!(orp_index(4), 1);
        assert_eq!(orp_index(9), 3);
    }

    #[test]
    fn delays_scale_with_punctuation_and_paragraphs() {
        let w = |t: &str, p: bool| Word { text: t.into(), para_end: p };
        let base = delay_for(&w("word", false), 300.0);
        assert_eq!(base, Duration::from_millis(200));
        assert!(delay_for(&w("word,", false), 300.0) > base);
        assert!(delay_for(&w("word.", false), 300.0) > delay_for(&w("word,", false), 300.0));
        assert!(delay_for(&w("word", true), 300.0) > delay_for(&w("word.", false), 300.0));
        assert!(delay_for(&w("supercalifragilistic", false), 300.0) > base);
    }

    #[test]
    fn split_flags_paragraph_ends() {
        let words = split_words("One two.\n\nThree four.");
        let texts: Vec<&str> = words.iter().map(|w| w.text.as_str()).collect();
        assert_eq!(texts, ["One", "two.", "Three", "four."]);
        assert_eq!(
            words.iter().map(|w| w.para_end).collect::<Vec<_>>(),
            [false, true, false, true]
        );
    }
}
