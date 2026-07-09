#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use eframe::egui::{self, Align2, Color32, FontId, Key, Rect};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::time::{Duration, Instant};

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([900.0, 500.0]),
        ..Default::default()
    };
    eframe::run_native(
        "RSVP Reader",
        options,
        Box::new(|cc| Ok(Box::new(App::load(cc.egui_ctx.clone())))),
    )
}

struct Word {
    text: String,
    para_end: bool,
    page: usize, // 0-based
    rect: Rect,  // position on page in PDF points
}

struct PageInfo {
    width: f32,
    height: f32,
}

#[derive(Serialize, Deserialize)]
struct Saved {
    index: usize,
    wpm: f32,
}

#[derive(PartialEq, Clone, Copy)]
enum Mode {
    Pdf,
    Reader,
    Focus,
}

struct App {
    words: Vec<Word>,
    page_starts: Vec<usize>,
    pages: Vec<PageInfo>,
    toc: Vec<Chapter>,
    idx: usize,
    mode: Mode,
    last_scrolled: usize,
    playing: bool,
    wpm: f32,
    last_advance: Instant,
    pdf_path: Option<PathBuf>,
    error: Option<String>,
    progress: HashMap<String, Saved>,
    textures: HashMap<usize, egui::TextureHandle>,
    pending: HashSet<usize>,
    render_tx: Sender<(PathBuf, usize)>,
    render_rx: Receiver<(usize, egui::ColorImage)>,
}

fn store_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("rsvp-reader/progress.json")
}

struct Chapter {
    level: usize,
    title: String,
    page: usize, // 1-based
}

fn load_toc(path: &Path) -> Vec<Chapter> {
    let Ok(doc) = lopdf::Document::load(path) else {
        return Vec::new();
    };
    doc.get_toc()
        .map(|t| {
            t.toc
                .into_iter()
                .map(|e| Chapter { level: e.level, title: e.title, page: e.page })
                .collect()
        })
        .unwrap_or_default()
}

fn xml_attr(s: &str, key: &str) -> Option<f32> {
    let i = s.find(key)? + key.len();
    s[i..].split('"').next()?.parse().ok()
}

fn xml_unescape(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

/// Words with on-page bounding boxes via `pdftotext -bbox`.
fn extract_bbox(path: &Path) -> Result<(Vec<Word>, Vec<PageInfo>), String> {
    let out = std::process::Command::new("pdftotext")
        .arg("-bbox")
        .arg(path)
        .arg("-")
        .output()
        .map_err(|_| "pdftotext not found — install poppler: brew install poppler".to_string())?;
    if !out.status.success() {
        return Err(format!(
            "pdftotext failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    let xml = String::from_utf8_lossy(&out.stdout);
    let mut words: Vec<Word> = Vec::new();
    let mut pages: Vec<PageInfo> = Vec::new();
    let attrs = |rest: &str| -> Option<(f32, f32, f32, f32)> {
        Some((
            xml_attr(rest, "xMin=\"")?,
            xml_attr(rest, "yMin=\"")?,
            xml_attr(rest, "xMax=\"")?,
            xml_attr(rest, "yMax=\"")?,
        ))
    };
    for line in xml.lines() {
        let t = line.trim();
        if let Some(rest) = t.strip_prefix("<page ") {
            if let (Some(w), Some(h)) = (xml_attr(rest, "width=\""), xml_attr(rest, "height=\"")) {
                pages.push(PageInfo { width: w, height: h });
            }
        } else if let Some(rest) = t.strip_prefix("<word ") {
            let text = rest
                .split('>')
                .nth(1)
                .and_then(|s| s.split('<').next())
                .map(xml_unescape)
                .unwrap_or_default();
            if text.trim().is_empty() || pages.is_empty() {
                continue;
            }
            if let Some((x0, y0, x1, y1)) = attrs(rest) {
                words.push(Word {
                    text,
                    para_end: false,
                    page: pages.len() - 1,
                    rect: Rect::from_min_max(egui::pos2(x0, y0), egui::pos2(x1, y1)),
                });
            }
        }
    }
    if words.is_empty() {
        return Err("No extractable text — is this a scanned/image-only PDF?".into());
    }
    // Paragraph ends: page break or a vertical gap larger than ~1.8 lines.
    for i in 0..words.len() {
        let end = i + 1 == words.len() || {
            let (a, b) = (&words[i], &words[i + 1]);
            a.page != b.page || b.rect.min.y - a.rect.min.y > 1.8 * a.rect.height()
        };
        words[i].para_end = end;
    }
    Ok((words, pages))
}

fn extract_pdf(path: &Path) -> Result<(Vec<Word>, Vec<usize>, Vec<PageInfo>), String> {
    let (words, pages) = extract_bbox(path)?;
    let mut page_starts = Vec::new();
    for (i, w) in words.iter().enumerate() {
        while page_starts.len() <= w.page {
            page_starts.push(i);
        }
    }
    Ok((words, page_starts, pages))
}

/// Renders one page to an image with pdftoppm (runs on a background thread).
fn render_page(path: &Path, page: usize) -> Option<egui::ColorImage> {
    let n = (page + 1).to_string();
    let root = std::env::temp_dir().join(format!("rsvp-{}-{}", std::process::id(), page));
    // ponytail: fixed 144 dpi — re-render per zoom level if crispness matters
    let status = std::process::Command::new("pdftoppm")
        .args(["-png", "-r", "144", "-f", &n, "-l", &n, "-singlefile"])
        .arg(path)
        .arg(&root)
        .status()
        .ok()?;
    if !status.success() {
        return None;
    }
    let png = root.with_extension("png");
    let bytes = std::fs::read(&png).ok()?;
    let _ = std::fs::remove_file(&png);
    let img = image::load_from_memory(&bytes).ok()?.to_rgba8();
    let size = [img.width() as usize, img.height() as usize];
    Some(egui::ColorImage::from_rgba_unmultiplied(size, &img))
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
    fn load(ctx: egui::Context) -> Self {
        let progress: HashMap<String, Saved> = std::fs::read_to_string(store_path())
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        let (render_tx, req_rx) = channel::<(PathBuf, usize)>();
        let (res_tx, render_rx) = channel();
        std::thread::spawn(move || {
            for (path, page) in req_rx {
                if let Some(img) = render_page(&path, page) {
                    let _ = res_tx.send((page, img));
                    ctx.request_repaint();
                }
            }
        });
        App {
            words: Vec::new(),
            page_starts: Vec::new(),
            pages: Vec::new(),
            toc: Vec::new(),
            idx: 0,
            mode: Mode::Pdf,
            last_scrolled: usize::MAX,
            playing: false,
            wpm: 300.0,
            last_advance: Instant::now(),
            pdf_path: None,
            error: None,
            progress,
            textures: HashMap::new(),
            pending: HashSet::new(),
            render_tx,
            render_rx,
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
        self.textures.clear();
        self.pending.clear();
        match extract_pdf(&path) {
            Ok((words, page_starts, pages)) => {
                self.error = None;
                if let Some(saved) = self.progress.get(&path.to_string_lossy().into_owned()) {
                    self.idx = saved.index.min(words.len() - 1);
                    self.wpm = saved.wpm;
                } else {
                    self.idx = 0;
                }
                self.words = words;
                self.page_starts = page_starts;
                self.toc = load_toc(&path);
                self.pages = pages;
                self.pdf_path = Some(path);
            }
            Err(e) => {
                self.error = Some(e);
                self.words.clear();
                self.page_starts.clear();
                self.pages.clear();
                self.toc.clear();
                self.pdf_path = None;
                self.idx = 0;
            }
        }
    }

    /// 1-based page the current word is on.
    fn current_page(&self) -> usize {
        self.page_starts.partition_point(|&s| s <= self.idx).max(1)
    }

    fn goto_page(&mut self, page: usize) {
        if self.words.is_empty() {
            return;
        }
        let start = self
            .page_starts
            .get(page.saturating_sub(1))
            .copied()
            .unwrap_or(0);
        self.idx = start.min(self.words.len() - 1);
        self.last_advance = Instant::now();
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
                ui.selectable_value(&mut self.mode, Mode::Pdf, "PDF");
                ui.selectable_value(&mut self.mode, Mode::Reader, "Reader");
                ui.selectable_value(&mut self.mode, Mode::Focus, "Focus");
                if !self.toc.is_empty() {
                    let mut jump: Option<usize> = None;
                    ui.menu_button("Chapters", |ui| {
                        ui.set_min_width(300.0);
                        egui::ScrollArea::vertical().max_height(400.0).show(ui, |ui| {
                            for ch in &self.toc {
                                let label = format!(
                                    "{}{}",
                                    "    ".repeat(ch.level.saturating_sub(1)),
                                    ch.title
                                );
                                if ui.button(label).clicked() {
                                    jump = Some(ch.page);
                                    ui.close();
                                }
                            }
                        });
                    });
                    if let Some(page) = jump {
                        self.goto_page(page);
                    }
                }
                if !self.words.is_empty() {
                    let mut page = self.current_page();
                    let resp = ui.add(
                        egui::DragValue::new(&mut page)
                            .range(1..=self.page_starts.len())
                            .prefix("page "),
                    );
                    if resp.changed() {
                        self.goto_page(page);
                    }
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
            if let Some(err) = &self.error {
                ui.painter().text(
                    ui.available_rect_before_wrap().center(),
                    Align2::CENTER_CENTER,
                    err,
                    FontId::proportional(20.0),
                    Color32::LIGHT_RED,
                );
                return;
            }
            if self.words.is_empty() {
                ui.painter().text(
                    ui.available_rect_before_wrap().center(),
                    Align2::CENTER_CENTER,
                    "Open a PDF to start reading.\nSpace = play/pause   Left/Right arrows = jump 10 words\nClick any word to jump there",
                    FontId::proportional(20.0),
                    Color32::GRAY,
                );
                return;
            }
            match self.mode {
                Mode::Pdf => self.draw_pdf(ui),
                Mode::Reader => self.draw_reader(ui),
                Mode::Focus => self.draw_focus(ui),
            }
        });
    }
}

impl App {
    fn request_page(&mut self, page: usize) {
        if page >= self.pages.len()
            || self.textures.contains_key(&page)
            || self.pending.contains(&page)
        {
            return;
        }
        if let Some(path) = &self.pdf_path {
            self.pending.insert(page);
            let _ = self.render_tx.send((path.clone(), page));
        }
    }

    /// The real PDF page with the current word highlighted on it.
    fn draw_pdf(&mut self, ui: &mut egui::Ui) {
        // Collect finished renders into textures.
        while let Ok((page, img)) = self.render_rx.try_recv() {
            self.pending.remove(&page);
            let tex = ui.ctx().load_texture(
                format!("page-{page}"),
                img,
                egui::TextureOptions::LINEAR,
            );
            self.textures.insert(page, tex);
        }

        let page = self.words[self.idx].page;
        self.request_page(page);
        self.request_page(page + 1); // prefetch
        if self.textures.len() > 8 {
            let keep = page.saturating_sub(2)..=page + 2;
            self.textures.retain(|p, _| keep.contains(p));
        }

        let avail = ui.available_rect_before_wrap();
        let info = &self.pages[page];
        let scale = (avail.width() / info.width).min(avail.height() / info.height);
        let size = egui::vec2(info.width * scale, info.height * scale);
        let img_rect = Rect::from_center_size(avail.center(), size);

        let resp = ui.allocate_rect(img_rect, egui::Sense::click());
        let painter = ui.painter();
        if let Some(tex) = self.textures.get(&page) {
            painter.image(
                tex.id(),
                img_rect,
                Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                Color32::WHITE,
            );
        } else {
            painter.rect_filled(img_rect, 4.0, Color32::from_gray(245));
            painter.text(
                img_rect.center(),
                Align2::CENTER_CENTER,
                "Rendering page…",
                FontId::proportional(16.0),
                Color32::GRAY,
            );
        }

        // Highlight the current word at its real position.
        let r = self.words[self.idx].rect;
        let hl = Rect::from_min_max(
            img_rect.min + r.min.to_vec2() * scale,
            img_rect.min + r.max.to_vec2() * scale,
        )
        .expand(2.0 * scale);
        painter.rect_filled(hl, 3.0, Color32::from_rgba_unmultiplied(255, 200, 0, 110));

        // Click a word on the page to jump there.
        if resp.clicked() {
            if let Some(pos) = resp.interact_pointer_pos() {
                let pdf_pos = egui::pos2(
                    (pos.x - img_rect.min.x) / scale,
                    (pos.y - img_rect.min.y) / scale,
                );
                let (lo, hi) = (
                    self.page_starts[page],
                    self.page_starts
                        .get(page + 1)
                        .copied()
                        .unwrap_or(self.words.len()),
                );
                if let Some(i) =
                    (lo..hi).find(|&i| self.words[i].rect.expand(3.0).contains(pdf_pos))
                {
                    self.idx = i;
                    self.last_advance = Instant::now();
                }
            }
        }
    }

    fn draw_focus(&mut self, ui: &mut egui::Ui) {
        let rect = ui.available_rect_before_wrap();
        let painter = ui.painter();
        let font = FontId::proportional(56.0);

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
            font,
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
    }

    /// E-reader style page view with the current word highlighted.
    fn draw_reader(&mut self, ui: &mut egui::Ui) {
        let page = self.current_page();
        let start = self.page_starts[page - 1];
        let end = self
            .page_starts
            .get(page)
            .copied()
            .unwrap_or(self.words.len());

        let mut clicked: Option<usize> = None;
        let scroll_to = if self.idx != self.last_scrolled {
            self.last_scrolled = self.idx;
            true
        } else {
            false
        };
        let sel = ui.visuals().selection;
        let read_color = ui.visuals().weak_text_color();
        let unread_color = ui.visuals().strong_text_color();

        egui::ScrollArea::vertical()
            .auto_shrink(false)
            .show(ui, |ui| {
                ui.vertical_centered(|ui| {
                    ui.set_max_width(640.0);
                    ui.add_space(24.0);
                    let mut i = start;
                    while i < end {
                        ui.horizontal_wrapped(|ui| {
                            ui.spacing_mut().item_spacing = egui::vec2(7.0, 8.0);
                            while i < end {
                                let w = &self.words[i];
                                let current = i == self.idx;
                                let mut text =
                                    egui::RichText::new(&w.text).size(19.0).color(
                                        if i < self.idx { read_color } else { unread_color },
                                    );
                                if current {
                                    text = text.color(sel.stroke.color).background_color(sel.bg_fill);
                                }
                                let resp = ui
                                    .add(egui::Label::new(text).sense(egui::Sense::click()))
                                    .on_hover_cursor(egui::CursorIcon::PointingHand);
                                if resp.clicked() {
                                    clicked = Some(i);
                                }
                                if current && scroll_to {
                                    resp.scroll_to_me(Some(egui::Align::Center));
                                }
                                let para_end = w.para_end;
                                i += 1;
                                if para_end {
                                    break;
                                }
                            }
                        });
                        ui.add_space(14.0);
                    }
                    ui.add_space(24.0);
                });
            });

        if let Some(i) = clicked {
            self.idx = i;
            self.last_advance = Instant::now();
        }
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
        let w = |t: &str, p: bool| Word { text: t.into(), para_end: p, page: 0, rect: Rect::ZERO };
        let base = delay_for(&w("word", false), 300.0);
        assert_eq!(base, Duration::from_millis(200));
        assert!(delay_for(&w("word,", false), 300.0) > base);
        assert!(delay_for(&w("word.", false), 300.0) > delay_for(&w("word,", false), 300.0));
        assert!(delay_for(&w("word", true), 300.0) > delay_for(&w("word.", false), 300.0));
        assert!(delay_for(&w("supercalifragilistic", false), 300.0) > base);
    }

    #[test]
    fn xml_word_line_parses() {
        assert_eq!(xml_attr(r#"xMin="226.04" yMin="103.8""#, "yMin=\""), Some(103.8));
        assert_eq!(xml_unescape("a&amp;b&#39;s"), "a&b's");
    }
}
