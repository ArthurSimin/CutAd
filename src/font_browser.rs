use crate::theme::{ACCENT, BACK, MUTED};
use anyhow::Result;
use cutad::dafont;
use cutad::fonts::{self, CATALOG, FontFace, FontRef};
use eframe::egui::{self, FontData, FontDefinitions, FontFamily, FontId, RichText};
use rfd::FileDialog;
use std::{
    path::Path,
    sync::{
        Arc,
        mpsc::{self, Receiver, TryRecvError},
    },
};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Page {
    Local,
    Online,
    DaFont,
}

struct Refreshed {
    fonts: Arc<Vec<FontFace>>,
    selected: Option<FontRef>,
}

pub enum Choice {
    Cancel,
    Choose(Option<FontRef>),
}

pub struct FontBrowser {
    page: Page,
    query: String,
    fonts: Arc<Vec<FontFace>>,
    candidate: Option<FontRef>,
    sample: Option<u64>,
    error: Option<String>,
    running: Option<Receiver<Result<Refreshed>>>,
    can_apply: bool,
    dafont: DaFontBrowser,
}

impl FontBrowser {
    pub fn new(ctx: &egui::Context, current: Option<FontRef>, can_apply: bool) -> Self {
        let mut browser = Self {
            page: Page::Local,
            query: String::new(),
            fonts: Arc::new(Vec::new()),
            candidate: current,
            sample: None,
            error: None,
            running: None,
            can_apply,
            dafont: DaFontBrowser::default(),
        };
        browser.request(ctx, || {
            Ok(Refreshed {
                fonts: fonts::available(),
                selected: None,
            })
        });
        browser
    }

    pub fn show(&mut self, ctx: &egui::Context) -> Option<Choice> {
        self.collect(ctx);
        let mut choice = None;
        let modal = egui::Modal::new(egui::Id::new("cutad_font_library")).show(ctx, |ui| {
            ui.set_min_width(560.0);
            ui.set_max_width(560.0);
            ui.heading("Font library");
            ui.horizontal(|ui| {
                ui.selectable_value(&mut self.page, Page::Local, "Installed");
                ui.selectable_value(&mut self.page, Page::DaFont, "DaFont");
                ui.selectable_value(&mut self.page, Page::Online, "Open fonts");
                if ui
                    .add_enabled(self.running.is_none(), egui::Button::new("Import font…"))
                    .clicked()
                    && let Some(path) = FileDialog::new()
                        .set_title("Import a font into CutAd")
                        .add_filter("Fonts and font ZIPs", &["ttf", "otf", "zip"])
                        .pick_file()
                {
                    self.request(ctx, move || {
                        let fonts = fonts::import(&path)?;
                        Ok(Refreshed {
                            fonts: fonts::available(),
                            selected: fonts.first().cloned(),
                        })
                    });
                }
            });
            ui.add_sized(
                [ui.available_width(), 29.0],
                egui::TextEdit::singleline(&mut self.query).hint_text(
                    if self.page == Page::DaFont {
                        "Filter this page"
                    } else {
                        "Search installed and open fonts"
                    },
                ),
            );
            let term = self.query.trim().to_lowercase();
            if let Some(error) = &self.error {
                ui.colored_label(egui::Color32::LIGHT_RED, error);
            }
            if self.running.is_some() {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("Updating fonts…");
                });
            }
            if self.page == Page::Local {
                if self.running.is_none() && ui.small_button("Rescan system fonts").clicked() {
                    self.request(ctx, || {
                        Ok(Refreshed {
                            fonts: fonts::refresh(),
                            selected: None,
                        })
                    });
                }
                let mut candidate = None;
                egui::ScrollArea::vertical()
                    .id_salt("installed_fonts")
                    .max_height(270.0)
                    .min_scrolled_height(270.0)
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        let mut matches = self
                            .fonts
                            .iter()
                            .filter(|face| {
                                format!("{} {}", face.reference.family, face.style)
                                    .to_lowercase()
                                    .contains(&term)
                            })
                            .peekable();
                        if matches.peek().is_none() && self.running.is_none() {
                            ui.label(RichText::new("No matching fonts").color(MUTED));
                        }
                        for face in matches {
                            let selected = self.candidate.as_ref() == Some(&face.reference);
                            let tag = if face.imported {
                                "   ·   CutAd library"
                            } else {
                                ""
                            };
                            let label =
                                format!("{}   ·   {}{tag}", face.reference.family, face.style);
                            if ui.selectable_label(selected, label).clicked() {
                                candidate = Some(face.reference.clone());
                            }
                        }
                    });
                if let Some(candidate) = candidate {
                    self.sample = self.preview(ctx, &candidate);
                    self.candidate = Some(candidate);
                }
                if let Some(selected) = &self.candidate {
                    ui.separator();
                    ui.label(RichText::new(&selected.family).color(MUTED).small());
                    let face = if self
                        .sample
                        .is_some_and(|ready| ctx.cumulative_frame_nr() >= ready)
                    {
                        FontFamily::Name("cutad-font-sample".into())
                    } else {
                        FontFamily::Proportional
                    };
                    ui.add(
                        egui::Label::new(
                            RichText::new("Aa Bb Cc  0123").font(FontId::new(30.0, face)),
                        )
                        .truncate(),
                    );
                }
            } else if self.page == Page::DaFont {
                if let Some(entry) = self.dafont.show(ui, self.running.is_none(), &term) {
                    self.request(ctx, move || {
                        let installed = dafont::install(&entry)?;
                        Ok(Refreshed {
                            fonts: fonts::available(),
                            selected: installed.first().cloned(),
                        })
                    });
                }
            } else {
                ui.label(
                    RichText::new("Open-licensed display fonts from Google Fonts.").color(MUTED),
                );
                let mut download = None;
                let mut candidate = None;
                egui::ScrollArea::vertical()
                    .id_salt("downloadable_fonts")
                    .max_height(270.0)
                    .min_scrolled_height(270.0)
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        for font in CATALOG.iter().filter(|font| {
                            format!("{} {}", font.name, font.category)
                                .to_lowercase()
                                .contains(&term)
                        }) {
                            ui.horizontal(|ui| {
                                let installed = self.fonts.iter().find(|face| {
                                    face.reference.family.eq_ignore_ascii_case(font.name)
                                });
                                if let Some(face) = installed {
                                    if ui.button("Preview").clicked() {
                                        candidate = Some(face.reference.clone());
                                    }
                                } else if ui
                                    .add_enabled(
                                        self.running.is_none(),
                                        egui::Button::new("Install"),
                                    )
                                    .clicked()
                                {
                                    download = Some(font);
                                }
                                ui.label(RichText::new(font.name).strong());
                                ui.label(RichText::new(font.category).color(MUTED));
                            });
                        }
                    });
                if let Some(candidate) = candidate {
                    self.sample = self.preview(ctx, &candidate);
                    self.candidate = Some(candidate);
                    self.page = Page::Local;
                    self.query.clear();
                }
                if let Some(font) = download {
                    self.request(ctx, move || {
                        let selected = fonts::get(font)?;
                        Ok(Refreshed {
                            fonts: fonts::available(),
                            selected: Some(selected),
                        })
                    });
                }
            }
            ui.separator();
            ui.horizontal(|ui| {
                if self.can_apply {
                    if ui.button("Preset font").clicked() {
                        choice = Some(Choice::Choose(None));
                    }
                    let button = egui::Button::new(RichText::new("Use font").strong().color(BACK))
                        .fill(ACCENT);
                    if ui.add_enabled(self.candidate.is_some(), button).clicked() {
                        choice = Some(Choice::Choose(self.candidate.clone()));
                    }
                }
                if ui.button("Close").clicked() {
                    choice = Some(Choice::Cancel);
                }
            });
        });
        if modal.should_close() {
            choice = Some(Choice::Cancel);
        }
        if choice.is_some() && self.sample.is_some() {
            ctx.set_fonts(FontDefinitions::default());
        }
        choice
    }

    fn request(
        &mut self,
        ctx: &egui::Context,
        task: impl FnOnce() -> Result<Refreshed> + Send + 'static,
    ) {
        let (sender, receiver) = mpsc::channel();
        let ctx = ctx.clone();
        self.error = None;
        self.running = Some(receiver);
        std::thread::spawn(move || {
            let _ = sender.send(task());
            ctx.request_repaint();
        });
    }

    fn collect(&mut self, ctx: &egui::Context) {
        let Some(receiver) = &self.running else {
            return;
        };
        match receiver.try_recv() {
            Ok(result) => {
                self.running = None;
                match result {
                    Ok(refreshed) => {
                        self.fonts = refreshed.fonts;
                        if let Some(selected) = refreshed.selected {
                            self.candidate = Some(selected);
                            self.page = Page::Local;
                            self.query.clear();
                        }
                        if let Some(candidate) = &self.candidate {
                            self.sample = self.preview(ctx, candidate);
                        }
                    }
                    Err(error) => self.error = Some(format!("{error:#}")),
                }
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => {
                self.running = None;
                self.error = Some("The font operation stopped.".into());
            }
        }
    }

    fn preview(&self, ctx: &egui::Context, font: &FontRef) -> Option<u64> {
        let path = self
            .fonts
            .iter()
            .find(|face| &face.reference == font)
            .map(|face| &face.path)?;
        let bytes = sample_bytes(path)?;
        let mut definition = FontDefinitions::default();
        let key = "cutad-font-sample".to_owned();
        definition
            .font_data
            .insert(key.clone(), Arc::new(FontData::from_owned(bytes)));
        let mut stack = vec![key];
        stack.extend(
            definition
                .families
                .get(&FontFamily::Proportional)
                .cloned()
                .unwrap_or_default(),
        );
        definition
            .families
            .insert(FontFamily::Name("cutad-font-sample".into()), stack);
        ctx.set_fonts(definition);
        Some(ctx.cumulative_frame_nr() + 1)
    }
}

fn sample_bytes(path: &Path) -> Option<Vec<u8>> {
    let size = std::fs::metadata(path).ok()?.len();
    if size > 24 * 1024 * 1024 {
        return None;
    }
    std::fs::read(path).ok()
}

#[derive(Default)]
struct DaFontBrowser {
    catalog: Vec<dafont::Entry>,
    category: dafont::Category,
    page: u32,
    address: String,
    confirmation: Option<dafont::Entry>,
    exact: bool,
    loading: Option<Receiver<Result<Vec<dafont::Entry>>>>,
    error: Option<String>,
}

impl DaFontBrowser {
    fn show(
        &mut self,
        ui: &mut egui::Ui,
        can_install: bool,
        filter: &str,
    ) -> Option<dafont::Entry> {
        let mut install = None;
        if self.page == 0 {
            self.page = 1;
            self.load(ui.ctx(), false);
        }
        if let Some(loading) = &self.loading {
            match loading.try_recv() {
                Ok(Ok(entries)) => {
                    self.catalog = entries;
                    self.loading = None;
                }
                Ok(Err(error)) => {
                    self.error = Some(format!("{error:#}"));
                    self.loading = None;
                }
                Err(TryRecvError::Disconnected) => {
                    self.error = Some("DaFont browsing stopped.".into());
                    self.loading = None;
                }
                Err(TryRecvError::Empty) => {}
            }
        }
        ui.horizontal(|ui| {
            let category = self.category;
            egui::ComboBox::from_id_salt("dafont_category")
                .selected_text(category.label())
                .show_ui(ui, |ui| {
                    for value in dafont::Category::ALL {
                        ui.selectable_value(&mut self.category, value, value.label());
                    }
                });
            if self.category != category {
                self.page = 1;
                self.load(ui.ctx(), false);
            }
            ui.add_enabled_ui(self.loading.is_none(), |ui| {
                if ui
                    .add_enabled(self.page > 1, egui::Button::new("Previous"))
                    .clicked()
                {
                    self.page -= 1;
                    self.load(ui.ctx(), false);
                }
                if ui.button("Next page").clicked() {
                    self.page += 1;
                    self.load(ui.ctx(), false);
                }
            });
            ui.hyperlink_to("Search all of DaFont", "https://www.dafont.com/");
        });
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.address)
                    .desired_width((ui.available_width() - 92.0).max(120.0))
                    .hint_text("Paste a DaFont font page URL"),
            );
            if ui.button("Find font").clicked() {
                self.load(ui.ctx(), true);
            }
        });
        if let Some(error) = &self.error {
            ui.colored_label(egui::Color32::LIGHT_RED, error);
        }
        if self.loading.is_some() {
            ui.spinner();
        }
        egui::ScrollArea::vertical()
            .id_salt("dafont_results")
            .max_height(232.0)
            .min_scrolled_height(232.0)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                let mut entries = self
                    .catalog
                    .iter()
                    .filter(|entry| self.exact || entry.name.to_lowercase().contains(filter))
                    .peekable();
                if self.loading.is_none() && self.error.is_none() && entries.peek().is_none() {
                    ui.label(RichText::new("No fonts match this page filter").color(MUTED));
                }
                for entry in entries {
                    ui.horizontal(|ui| {
                        if ui
                            .add_enabled(can_install, egui::Button::new("Install…"))
                            .clicked()
                        {
                            self.confirmation = Some(entry.clone());
                        }
                        ui.hyperlink_to(&entry.name, entry.listing());
                        ui.label(RichText::new(&entry.license).small().color(MUTED));
                    });
                }
            });
        if let Some(entry) = self.confirmation.clone() {
            ui.separator();
            ui.label(RichText::new(format!("Install {} · {}", entry.name, entry.license)).strong());
            ui.horizontal(|ui| {
                ui.hyperlink_to("View author and license", entry.listing());
                if ui
                    .add_enabled(can_install, egui::Button::new("Install font"))
                    .clicked()
                {
                    install = self.confirmation.take();
                }
                if ui.small_button("Cancel").clicked() {
                    self.confirmation = None;
                }
            });
        }
        install
    }

    fn load(&mut self, ctx: &egui::Context, lookup: bool) {
        let (sender, receiver) = mpsc::channel();
        let ctx = ctx.clone();
        let category = self.category;
        let page = self.page;
        let url = self.address.clone();
        self.exact = lookup;
        self.confirmation = None;
        self.error = None;
        self.loading = Some(receiver);
        std::thread::spawn(move || {
            let result = if lookup {
                dafont::lookup(&url)
            } else {
                dafont::browse(category, page)
            };
            let _ = sender.send(result);
            ctx.request_repaint();
        });
    }
}
