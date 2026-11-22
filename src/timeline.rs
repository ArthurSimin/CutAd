use crate::theme::{ACCENT, LINE, MUTED, TEXT};
use crate::thumbnails::Thumbnails;
use crate::waveforms::{BLOCK_FRAMES, Waveforms};
use cutad::project::{Asset, AssetKind, Clip, FPS, Project, TrackKind, timestamp};
use eframe::egui::{
    self, Align2, Color32, FontId, Id, Pos2, Rect, Sense, Stroke, StrokeKind, pos2, vec2,
};

const HEADER: f32 = 126.0;
const RULER: f32 = 32.0;
const ROW: f32 = 52.0;

#[derive(Clone)]
pub struct Selection {
    pub row: usize,
    pub clip: Option<String>,
}

#[derive(Clone)]
pub struct Placement {
    pub id: String,
    pub row: usize,
    pub start: u32,
    pub source: u32,
    pub length: u32,
}

#[derive(Clone, Copy)]
enum Move {
    Slide,
    TrimStart,
    TrimEnd,
}

#[derive(Clone)]
struct Drag {
    original: Placement,
    proposed: Placement,
    anchor: u32,
    mode: Move,
}

#[derive(Default)]
pub struct Timeline {
    drag: Option<Drag>,
    menu: Option<String>,
}

#[derive(Clone, Copy)]
pub enum ClipAction {
    Split,
    Duplicate,
    TrimLeft,
    TrimRight,
    DetachAudio,
    Replace,
    Effects,
    Remove,
}

pub struct MediaDrag(pub String);

pub struct Interaction {
    pub select: Option<Selection>,
    pub seek: Option<u32>,
    pub edit: Option<Placement>,
    pub action: Option<(String, ClipAction)>,
    pub add: Option<(String, usize, u32)>,
}

pub struct View<'a> {
    pub project: &'a Project,
    pub selection: &'a Selection,
    pub frame: u32,
    pub zoom: f32,
    pub snap: bool,
}

impl Timeline {
    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        view: View<'_>,
        thumbnails: &mut Thumbnails,
    ) -> Interaction {
        let mut events = Interaction {
            select: None,
            seek: None,
            edit: None,
            action: None,
            add: None,
        };
        let scale = view.zoom / FPS as f32;
        let width = (HEADER + (view.project.duration() + 3 * FPS).max(12 * FPS) as f32 * scale)
            .max(ui.available_width() - 1.0);
        let height =
            (RULER + view.project.tracks.len() as f32 * ROW + 10.0).max(ui.available_height());
        egui::ScrollArea::both()
            .id_salt("timeline_scroll")
            .auto_shrink([false; 2])
            .show(ui, |ui| {
                let (rect, _) = ui.allocate_exact_size(vec2(width, height), Sense::hover());
                let response = ui.interact(
                    rect,
                    Id::new("timeline_interaction"),
                    Sense::click_and_drag(),
                );
                let cursor = ui.input(|input| input.pointer.interact_pos());
                if let Some(payload) = response.dnd_release_payload::<MediaDrag>()
                    && let Some(cursor) = cursor
                    && cursor.y >= rect.top() + RULER
                {
                    events.add = Some((
                        payload.0.clone(),
                        row_at(view.project, rect, cursor),
                        frame_at(rect, scale, cursor),
                    ));
                }
                let origin = ui.input(|input| input.pointer.press_origin()).or(cursor);
                if response.drag_started_by(egui::PointerButton::Primary)
                    && let Some(origin) = origin
                    && let Some((row, clip, bounds)) = hit(view.project, rect, scale, origin)
                {
                    let placed = Placement {
                        id: clip.id.clone(),
                        row,
                        start: clip.start,
                        source: clip.source,
                        length: clip.length,
                    };
                    let edge = (bounds.width() / 4.0).min(8.0);
                    let mode = if origin.x < bounds.left() + edge {
                        Move::TrimStart
                    } else if origin.x > bounds.right() - edge {
                        Move::TrimEnd
                    } else {
                        Move::Slide
                    };
                    self.drag = Some(Drag {
                        original: placed.clone(),
                        proposed: placed,
                        anchor: frame_at(rect, scale, origin),
                        mode,
                    });
                    events.select = Some(Selection {
                        row,
                        clip: Some(clip.id.clone()),
                    });
                }
                if (response.dragged_by(egui::PointerButton::Primary)
                    || response.drag_stopped_by(egui::PointerButton::Primary))
                    && let Some(cursor) = cursor
                {
                    let cursor_frame = frame_at(rect, scale, cursor);
                    if let Some(drag) = &mut self.drag {
                        update_drag(drag, &view, rect, scale, cursor, cursor_frame);
                    } else if cursor.x >= rect.left() + HEADER {
                        events.seek = Some(cursor_frame);
                    }
                }
                if response.drag_stopped()
                    && let Some(drag) = self.drag.take()
                {
                    let (a, b) = (drag.original, drag.proposed);
                    if (a.row, a.start, a.source, a.length) != (b.row, b.start, b.source, b.length)
                    {
                        events.edit = Some(b);
                    }
                }
                if response.clicked()
                    && let Some(cursor) = cursor
                {
                    if let Some((row, clip, _)) = hit(view.project, rect, scale, cursor) {
                        events.select = Some(Selection {
                            row,
                            clip: Some(clip.id.clone()),
                        });
                    } else {
                        if cursor.y >= rect.top() + RULER {
                            events.select = Some(Selection {
                                row: row_at(view.project, rect, cursor),
                                clip: None,
                            });
                        }
                        if cursor.x >= rect.left() + HEADER {
                            events.seek = Some(frame_at(rect, scale, cursor));
                        }
                    }
                }
                if response.secondary_clicked() {
                    self.menu = cursor
                        .and_then(|cursor| hit(view.project, rect, scale, cursor))
                        .map(|(row, clip, _)| {
                            events.select = Some(Selection {
                                row,
                                clip: Some(clip.id.clone()),
                            });
                            clip.id.clone()
                        });
                }
                if let Some(id) = &self.menu
                    && let Some((row, index)) = view.project.location(id)
                {
                    let track = &view.project.tracks[row];
                    let clip = &track.clips[index];
                    let asset = view.project.asset(&clip.asset_id);
                    let has_audio = track.kind == TrackKind::Video
                        && !clip.freeze
                        && asset.is_some_and(|asset| asset.audio);
                    let inside = clip.start < view.frame && view.frame < clip.end();
                    response.context_menu(|ui| {
                        ui.set_min_width(174.0);
                        for (label, action, enabled) in [
                            ("Split at playhead", ClipAction::Split, inside),
                            ("Trim start to playhead", ClipAction::TrimLeft, inside),
                            ("Trim end to playhead", ClipAction::TrimRight, inside),
                            ("Duplicate clip", ClipAction::Duplicate, true),
                            ("Detach audio", ClipAction::DetachAudio, has_audio),
                            ("Replace media…", ClipAction::Replace, true),
                            ("Clip effects", ClipAction::Effects, true),
                            ("Remove clip", ClipAction::Remove, true),
                        ] {
                            if ui.add_enabled(enabled, egui::Button::new(label)).clicked() {
                                events.action = Some((id.clone(), action));
                                ui.close();
                            }
                        }
                    });
                }
                self.paint(ui, rect, &view, scale, thumbnails);
            });
        events
    }

    fn paint(
        &self,
        ui: &egui::Ui,
        rect: Rect,
        view: &View<'_>,
        scale: f32,
        thumbnails: &mut Thumbnails,
    ) {
        let painter = ui.painter();
        let visible = painter.clip_rect().intersect(rect);
        let project = view.project;
        let header = rect.left() + HEADER;
        painter.rect_filled(rect, 6, Color32::from_rgb(19, 18, 29));
        painter.rect_filled(
            Rect::from_min_size(rect.min, vec2(rect.width(), RULER)),
            5,
            LINE,
        );
        painter.text(
            rect.min + vec2(12.0, RULER / 2.0),
            Align2::LEFT_CENTER,
            "Tracks",
            FontId::proportional(13.0),
            TEXT,
        );
        for (row, track) in project.tracks.iter().enumerate() {
            let y = rect.top() + RULER + row as f32 * ROW;
            if y > visible.bottom() || y + ROW < visible.top() {
                continue;
            }
            painter.rect_filled(
                Rect::from_min_size(pos2(rect.left(), y), vec2(rect.width(), ROW - 1.0)),
                0,
                Color32::from_rgb(28, 26, 42),
            );
            let active = if row == view.selection.row {
                Color32::from_rgb(75, 59, 103)
            } else {
                LINE
            };
            let lane = Rect::from_min_size(pos2(rect.left(), y), vec2(HEADER - 1.0, ROW - 1.0));
            painter.rect_filled(lane, 0, active);
            let label = painter.with_clip_rect(lane.shrink(5.0).intersect(visible));
            label.text(
                pos2(rect.left() + 12.0, y + 15.0),
                Align2::LEFT_CENTER,
                &track.name,
                FontId::proportional(13.0),
                TEXT,
            );
            let track_kind = if track.muted {
                format!("{} · off", track.kind.label())
            } else {
                track.kind.label().to_owned()
            };
            label.text(
                pos2(rect.left() + 12.0, y + 33.0),
                Align2::LEFT_CENTER,
                track_kind,
                FontId::proportional(11.0),
                MUTED,
            );
        }
        let step = [1, 2, 5, 10, 30, 60, 300]
            .into_iter()
            .find(|n| *n as f32 * view.zoom >= 66.0)
            .unwrap_or(300);
        let from = (((visible.left() - header) / view.zoom).max(0.0) as u32 / step) * step;
        let to = ((visible.right() - header) / view.zoom).max(0.0) as u32 + step;
        for sec in (from..=to).step_by(step as usize) {
            let x = header + sec as f32 * view.zoom;
            painter.line_segment(
                [pos2(x, rect.top() + RULER - 6.0), pos2(x, visible.bottom())],
                Stroke::new(1.0, LINE),
            );
            let time = timestamp(sec * FPS);
            let title = if sec >= 3600 { &time[..8] } else { &time[3..8] };
            painter.text(
                pos2(x + 5.0, rect.top() + 13.0),
                Align2::LEFT_CENTER,
                title,
                FontId::monospace(11.0),
                MUTED,
            );
        }
        for (row, track) in project.tracks.iter().enumerate() {
            for clip in &track.clips {
                if self
                    .drag
                    .as_ref()
                    .is_some_and(|drag| drag.original.id == clip.id)
                {
                    continue;
                }
                let bounds = clip_rect(rect, scale, row, clip.start, clip.length);
                paint_clip(
                    painter,
                    bounds,
                    visible,
                    project,
                    clip,
                    track.kind,
                    view.selection.clip.as_deref() == Some(&clip.id),
                    thumbnails,
                );
            }
        }
        if let Some(drag) = &self.drag
            && let Some((row, column)) = project.location(&drag.original.id)
        {
            let p = &drag.proposed;
            let bounds = clip_rect(rect, scale, p.row, p.start, p.length);
            paint_clip(
                painter,
                bounds,
                visible,
                project,
                &project.tracks[row].clips[column],
                project.tracks[p.row].kind,
                true,
                thumbnails,
            );
        }
        let x = header + view.frame as f32 * scale;
        painter.line_segment(
            [pos2(x, rect.top() + RULER - 4.0), pos2(x, visible.bottom())],
            Stroke::new(1.3, ACCENT),
        );
        painter.rect_filled(
            Rect::from_center_size(pos2(x, rect.top() + RULER - 7.0), vec2(9.0, 8.0)),
            2,
            ACCENT,
        );
    }
}

fn frame_at(rect: Rect, scale: f32, cursor: Pos2) -> u32 {
    ((cursor.x - rect.left() - HEADER) / scale).round().max(0.0) as u32
}

fn row_at(project: &Project, rect: Rect, cursor: Pos2) -> usize {
    (((cursor.y - rect.top() - RULER) / ROW).floor().max(0.0) as usize)
        .min(project.tracks.len() - 1)
}

fn clip_rect(rect: Rect, scale: f32, row: usize, start: u32, length: u32) -> Rect {
    Rect::from_min_size(
        pos2(
            rect.left() + HEADER + start as f32 * scale,
            rect.top() + RULER + row as f32 * ROW + 6.0,
        ),
        vec2((length as f32 * scale - 1.0).max(2.0), ROW - 12.0),
    )
}

fn hit(project: &Project, rect: Rect, scale: f32, pos: Pos2) -> Option<(usize, &Clip, Rect)> {
    let row = row_at(project, rect, pos);
    project.tracks[row].clips.iter().find_map(|clip| {
        let bounds = clip_rect(rect, scale, row, clip.start, clip.length);
        bounds.contains(pos).then_some((row, clip, bounds))
    })
}

fn snapped(view: &View<'_>, scale: f32, current: &str, position: i64) -> u32 {
    let pos = position.max(0) as u32;
    if !view.snap {
        return pos;
    }
    let anchors = view
        .project
        .tracks
        .iter()
        .flat_map(|track| &track.clips)
        .filter(|clip| clip.id != current)
        .flat_map(|clip| [clip.start, clip.end()]);
    anchors
        .chain([0, view.frame])
        .min_by_key(|frame| frame.abs_diff(pos))
        .filter(|frame| frame.abs_diff(pos) as f32 * scale <= 8.0)
        .unwrap_or(pos)
}

fn update_drag(drag: &mut Drag, view: &View<'_>, rect: Rect, scale: f32, cursor: Pos2, frame: u32) {
    let original = &drag.original;
    let delta = i64::from(frame) - i64::from(drag.anchor);
    let position = |value| snapped(view, scale, &original.id, i64::from(value) + delta);
    let proposed = &mut drag.proposed;
    let end = original.start + original.length;
    let frozen = view
        .project
        .location(&original.id)
        .is_some_and(|(row, col)| view.project.tracks[row].clips[col].freeze);
    match drag.mode {
        Move::Slide => {
            proposed.start = position(original.start);
            proposed.row = row_at(view.project, rect, cursor);
        }
        Move::TrimStart => {
            let min = if frozen {
                0
            } else {
                original.start.saturating_sub(original.source)
            };
            let start = position(original.start).clamp(min, end - 1);
            proposed.start = start;
            if !frozen {
                proposed.source = original.source + start - original.start;
            }
            proposed.length = end - start;
        }
        Move::TrimEnd => {
            let mut end = position(end);
            if !frozen
                && let Some((row, column)) = view.project.location(&original.id)
                && let Some(asset) = view
                    .project
                    .asset(&view.project.tracks[row].clips[column].asset_id)
                && !matches!(asset.kind, AssetKind::Image | AssetKind::Text)
            {
                end = end.min(original.start + asset.frames - original.source);
            }
            proposed.length = end.saturating_sub(original.start).max(1);
        }
    }
}

fn paint_clip(
    painter: &egui::Painter,
    bounds: Rect,
    visible: Rect,
    project: &Project,
    clip: &Clip,
    kind: TrackKind,
    selected: bool,
    thumbnails: &mut Thumbnails,
) {
    if !visible.intersects(bounds) {
        return;
    }
    let Some(asset) = project.asset(&clip.asset_id) else {
        return;
    };
    let fill = match (clip.slot, kind, asset.kind) {
        (true, _, _) => Color32::from_rgb(49, 55, 64),
        (_, TrackKind::Audio, _) => Color32::from_rgb(38, 94, 74),
        (_, _, AssetKind::Text) => Color32::from_rgb(105, 68, 41),
        (_, _, AssetKind::Image) => Color32::from_rgb(75, 65, 111),
        _ => Color32::from_rgb(37, 69, 96),
    };
    let highlight = painter.ctx().animate_bool_with_time(
        Id::new(("timeline_selection", &clip.id)),
        selected,
        painter.ctx().global_style().animation_time,
    );
    let idle = if clip.slot { MUTED } else { fill };
    let border = idle.lerp_to_gamma(ACCENT, highlight);
    painter.rect(
        bounds,
        5,
        fill,
        Stroke::new(1.0 + highlight * 0.35, border),
        StrokeKind::Inside,
    );
    if bounds.width() > 27.0 {
        let mut label = if clip.slot {
            format!("Slot · {}", asset.label)
        } else {
            asset.label.clone()
        };
        if clip.transition.is_some() {
            label.push_str("  ·  Cut transition");
        }
        if clip.freeze {
            label.push_str("  ·  Freeze");
        }
        if !clip.effects.is_empty() {
            label.push_str(&format!("  ·  {} FX", clip.effects.len()));
        }
        let clip_painter = painter.with_clip_rect(bounds.shrink(2.0).intersect(visible));
        let thumbnails_top = bounds.top() + 19.0;
        let wave = asset.audio && !clip.slot && !clip.freeze && bounds.width() > 39.0;
        let wave_height = if kind == TrackKind::Audio { 20.0 } else { 9.0 };
        let height =
            (bounds.bottom() - thumbnails_top - 3.0 - if wave { wave_height } else { 0.0 })
                .max(5.0);
        if kind == TrackKind::Video && !clip.slot && bounds.width() > 49.0 {
            let tile_width = (height * 144.0 / 81.0).max(42.0);
            let count = (bounds.width() / tile_width).ceil().max(1.0) as u32;
            let first = ((visible.left() - bounds.left()).max(0.0) / tile_width) as u32;
            let last = (((visible.right() - bounds.left()).max(0.0) / tile_width).ceil() as u32)
                .min(count);
            for index in first..last {
                let frame = if clip.freeze {
                    clip.source
                } else {
                    clip.source
                        + (u64::from(index) * u64::from(clip.length) / u64::from(count)) as u32
                };
                if let Some(texture) = thumbnails.image(painter.ctx(), asset, frame) {
                    let tile = Rect::from_min_size(
                        pos2(bounds.left() + index as f32 * tile_width, thumbnails_top),
                        vec2(tile_width, height),
                    );
                    clip_painter.image(
                        texture.id(),
                        tile,
                        Rect::from_min_max(Pos2::ZERO, pos2(1.0, 1.0)),
                        Color32::WHITE.gamma_multiply(0.82),
                    );
                }
            }
        }
        if wave {
            let strip = Rect::from_min_max(
                pos2(bounds.left(), bounds.bottom() - wave_height - 2.0),
                pos2(bounds.right(), bounds.bottom() - 2.0),
            );
            paint_waveform(
                &clip_painter,
                strip,
                visible,
                clip,
                asset,
                &mut thumbnails.waveforms,
            );
        }
        for frame in clip.position_animation.times(clip.length) {
            let x = bounds.left() + bounds.width() * frame as f32 / clip.length as f32;
            let center = pos2(
                x.clamp(bounds.left() + 5.0, bounds.right() - 5.0),
                bounds.bottom() - 7.0,
            );
            let points = [
                vec2(0.0, -4.0),
                vec2(4.0, 0.0),
                vec2(0.0, 4.0),
                vec2(-4.0, 0.0),
            ];
            clip_painter.add(egui::Shape::convex_polygon(
                points.into_iter().map(|offset| center + offset).collect(),
                ACCENT,
                Stroke::new(0.7, Color32::BLACK),
            ));
        }
        clip_painter.text(
            pos2(bounds.left() + 7.0, bounds.top() + 10.0),
            Align2::LEFT_CENTER,
            label,
            FontId::proportional(11.5),
            TEXT,
        );
    }
}

fn paint_waveform(
    painter: &egui::Painter,
    strip: Rect,
    visible: Rect,
    clip: &Clip,
    asset: &Asset,
    waveforms: &mut Waveforms,
) {
    let view = strip.intersect(visible);
    if !view.is_positive() {
        return;
    }
    let scale = strip.width() / clip.length as f32;
    let first = ((view.left() - strip.left()) / scale).floor().max(0.0) as u32;
    let end = (((view.right() - strip.left()) / scale).ceil() as u32).min(clip.length);
    let step = ((view.width() / 520.0).max(1.0) / scale).ceil().max(1.0) as u32;
    let source_begin = clip.source + first;
    let source_end = clip.source + end;
    let mut shapes = Vec::new();
    let mut start = (source_begin / BLOCK_FRAMES) * BLOCK_FRAMES;
    while start < source_end {
        let min = source_begin.max(start);
        let max = source_end.min(start + BLOCK_FRAMES);
        if let Some(tile) = waveforms.tile(painter.ctx(), asset, start) {
            let slice = &tile[(min - start) as usize..(max - start) as usize];
            for (i, span) in slice.chunks(step as usize).enumerate() {
                let peak = *span.iter().max().unwrap_or(&0);
                if peak == 0 {
                    continue;
                }
                let amplitude = (f32::from(peak) / 255.0 * clip.gain).min(1.0);
                let x = strip.left() + (min - clip.source + i as u32 * step) as f32 * scale;
                let height = (strip.height() * 0.5 * amplitude).max(0.65);
                let y = strip.center().y;
                shapes.push(egui::Shape::line_segment(
                    [pos2(x, y - height), pos2(x, y + height)],
                    Stroke::new(1.0, ACCENT.gamma_multiply(0.74)),
                ));
            }
        }
        start += BLOCK_FRAMES;
    }
    painter.extend(shapes);
}
