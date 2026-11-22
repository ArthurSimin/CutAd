use crate::waveforms::Waveforms;
use cutad::{
    media,
    project::{Asset, AssetKind},
};
use eframe::egui::{ColorImage, Context, TextureHandle, TextureOptions};
use std::{
    collections::{HashMap, VecDeque},
    path::PathBuf,
    sync::mpsc::{self, Receiver},
};

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
struct Key {
    path: PathBuf,
    frame: u32,
}

#[derive(Default)]
pub struct Thumbnails {
    pub waveforms: Waveforms,
    cache: HashMap<Key, Option<TextureHandle>>,
    order: VecDeque<Key>,
    workers: Vec<(Key, Receiver<Option<Vec<u8>>>)>,
}

impl Thumbnails {
    pub fn collect(&mut self, ctx: &Context) {
        self.waveforms.collect();
        let mut completed = Vec::new();
        self.workers.retain(|(key, response)| {
            match response.try_recv() {
                Ok(data) => completed.push((key.clone(), data)),
                Err(mpsc::TryRecvError::Disconnected) => completed.push((key.clone(), None)),
                Err(mpsc::TryRecvError::Empty) => return true,
            }
            false
        });
        for (key, pixels) in completed {
            let handle = pixels.map(|rgb| {
                ctx.load_texture(
                    "source_media",
                    ColorImage::from_rgb(media::THUMBNAIL_SIZE, &rgb),
                    TextureOptions::LINEAR,
                )
            });
            if self.order.len() >= 240
                && let Some(old) = self.order.pop_front()
            {
                self.cache.remove(&old);
            }
            self.order.push_back(key.clone());
            self.cache.insert(key, handle);
        }
    }

    pub fn image(&mut self, ctx: &Context, asset: &Asset, frame: u32) -> Option<&TextureHandle> {
        if !matches!(asset.kind, AssetKind::Video | AssetKind::Image) {
            return None;
        }
        let path = asset.path.clone()?;
        let frame = if asset.kind == AssetKind::Image {
            0
        } else {
            frame.min(asset.frames.saturating_sub(1))
        };
        let key = Key { path, frame };
        if !self.cache.contains_key(&key)
            && self.workers.len() < 2
            && !self.workers.iter().any(|(active, _)| active == &key)
        {
            let (sender, receiver) = mpsc::channel();
            self.workers.push((key.clone(), receiver));
            let pending = key.clone();
            let ctx = ctx.clone();
            std::thread::spawn(move || {
                let result = media::source_thumbnail(&pending.path, pending.frame).ok();
                let _ = sender.send(result);
                ctx.request_repaint();
            });
        }
        self.cache.get(&key).and_then(Option::as_ref)
    }
}
