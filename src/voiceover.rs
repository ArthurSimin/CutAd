//! Record local microphone PCM without blocking the editor or logging its contents.

use anyhow::{Context, Result, bail, ensure};
use eframe::egui;
use rodio::cpal::{
    self, FromSample, SampleFormat, SizedSample,
    traits::{DeviceTrait, HostTrait, StreamTrait},
};
use std::fs::File;
use std::io::{BufWriter, Seek, Write};
use std::path::PathBuf;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicU8, AtomicU32, Ordering},
    mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TryRecvError},
};
use std::time::Duration;

const ARMED: u8 = 0;
const CAPTURING: u8 = 1;
const STOP: u8 = 2;
const CANCEL: u8 = 3;
const MAX_PCM_BYTES: u32 = u32::MAX - 36;

#[derive(Default)]
struct State {
    signal: AtomicU8,
    ready: AtomicBool,
    millis: AtomicU32,
    peak: AtomicU32,
    overflow: AtomicBool,
    failure: Mutex<Option<String>>,
}

pub struct Recording {
    state: Arc<State>,
    completed: Receiver<Result<Option<PathBuf>>>,
}

impl Recording {
    pub fn start(device: Option<String>, destination: PathBuf, ctx: egui::Context) -> Self {
        let (sender, completed) = mpsc::channel();
        let state = Arc::new(State::default());
        let worker_state = Arc::clone(&state);
        std::thread::spawn(move || {
            let result = record(device.as_deref(), destination, &worker_state, &ctx);
            let _ = sender.send(result);
            ctx.request_repaint();
        });
        Self { state, completed }
    }

    pub fn ready(&self) -> bool {
        self.state.ready.load(Ordering::Acquire)
    }

    pub fn begin(&self) {
        let _ = self.state.signal.compare_exchange(
            ARMED,
            CAPTURING,
            Ordering::AcqRel,
            Ordering::Relaxed,
        );
    }

    pub fn capturing(&self) -> bool {
        self.state.signal.load(Ordering::Acquire) == CAPTURING
    }

    pub fn finishing(&self) -> bool {
        self.state.signal.load(Ordering::Acquire) >= STOP
    }

    pub fn stop(&self, discard: bool) {
        self.state
            .signal
            .store(if discard { CANCEL } else { STOP }, Ordering::Release);
    }

    pub fn elapsed(&self) -> Duration {
        Duration::from_millis(u64::from(self.state.millis.load(Ordering::Relaxed)))
    }

    pub fn peak(&self) -> f32 {
        self.state.peak.load(Ordering::Relaxed) as f32 / 32_768.0
    }

    pub fn result(&self) -> Option<Result<Option<PathBuf>>> {
        match self.completed.try_recv() {
            Ok(result) => Some(result),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => {
                Some(Err(anyhow::anyhow!("The recording worker stopped.")))
            }
        }
    }
}

impl Drop for Recording {
    fn drop(&mut self) {
        self.stop(true);
    }
}

pub fn input_names() -> Result<Vec<String>> {
    let mut names = cpal::default_host()
        .input_devices()
        .context("Could not list recording devices.")?
        .filter_map(|device| device.name().ok())
        .collect::<Vec<_>>();
    names.sort_unstable();
    names.dedup();
    Ok(names)
}

fn record(
    device: Option<&str>,
    destination: PathBuf,
    state: &Arc<State>,
    ctx: &egui::Context,
) -> Result<Option<PathBuf>> {
    let destination = std::path::absolute(destination)?;
    ensure!(
        !destination.exists(),
        "Choose a new file for the voiceover."
    );
    let host = cpal::default_host();
    let device = match device {
        Some(name) => host
            .input_devices()?
            .find(|device| device.name().ok().as_deref() == Some(name)),
        None => host.default_input_device(),
    }
    .context("The selected microphone is unavailable.")?;
    let config = device
        .default_input_config()
        .context("Could not configure the microphone.")?;
    let format = config.sample_format();
    let config = config.config();
    let channels = config.channels.min(2);
    let rate = config.sample_rate.0;
    ensure!(
        channels > 0 && (8_000..=384_000).contains(&rate),
        "Unsupported microphone format."
    );
    let scratch = tempfile::NamedTempFile::new_in(
        destination
            .parent()
            .context("Choose a local output folder.")?,
    )?;
    let mut output = BufWriter::new(scratch.reopen()?);
    output.write_all(&wav_header(rate, channels, 0))?;
    let (sender, input) = mpsc::sync_channel(32);
    macro_rules! input_stream {
        ($sample:ty) => {
            open_stream::<$sample>(&device, &config, channels, sender, state.clone())
        };
    }
    let stream = match format {
        SampleFormat::I8 => input_stream!(i8),
        SampleFormat::I16 => input_stream!(i16),
        SampleFormat::I24 => input_stream!(cpal::I24),
        SampleFormat::I32 => input_stream!(i32),
        SampleFormat::I64 => input_stream!(i64),
        SampleFormat::U8 => input_stream!(u8),
        SampleFormat::U16 => input_stream!(u16),
        SampleFormat::U32 => input_stream!(u32),
        SampleFormat::U64 => input_stream!(u64),
        SampleFormat::F32 => input_stream!(f32),
        SampleFormat::F64 => input_stream!(f64),
        _ => bail!("Unsupported microphone sample format {format}."),
    }?;
    stream.play().context("Could not start the microphone.")?;
    state.ready.store(true, Ordering::Release);
    ctx.request_repaint();

    let mut pcm_bytes = 0_u32;
    let mut append = |samples: Vec<u8>| -> Result<()> {
        let bytes = u32::try_from(samples.len())?;
        ensure!(
            bytes <= MAX_PCM_BYTES - pcm_bytes,
            "Voiceover reached the WAV file size limit."
        );
        output.write_all(&samples)?;
        pcm_bytes += bytes;
        let sample_frames = u64::from(pcm_bytes) / (u64::from(channels) * 2);
        let millis = sample_frames * 1000 / u64::from(rate);
        state.millis.store(millis as u32, Ordering::Relaxed);
        Ok(())
    };
    while state.signal.load(Ordering::Acquire) < STOP {
        ensure!(
            !state.overflow.load(Ordering::Relaxed),
            "The input buffer filled before audio could be saved."
        );
        if let Some(failure) = state.failure.lock().unwrap().take() {
            bail!("{failure}");
        }
        match input.recv_timeout(Duration::from_millis(30)) {
            Ok(samples) => append(samples)?,
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => bail!("The microphone stream disconnected."),
        }
    }
    drop(stream);
    if state.signal.load(Ordering::Acquire) == CANCEL {
        return Ok(None);
    }
    ensure!(
        !state.overflow.load(Ordering::Relaxed),
        "The input buffer filled before audio could be saved."
    );
    if let Some(failure) = state.failure.lock().unwrap().take() {
        bail!("{failure}");
    }
    for samples in input.try_iter() {
        append(samples)?;
    }
    ensure!(
        u64::from(pcm_bytes) * 30 >= u64::from(rate) * u64::from(channels) * 2,
        "Record for at least one timeline frame."
    );
    finish_wave(output, rate, channels, pcm_bytes)?;
    scratch
        .persist_noclobber(&destination)
        .context("Could not save the voiceover.")?;
    Ok(Some(destination))
}

fn open_stream<T: SizedSample>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    channels: u16,
    sender: SyncSender<Vec<u8>>,
    state: Arc<State>,
) -> Result<cpal::Stream>
where
    i16: FromSample<T>,
{
    let input_channels = usize::from(config.channels);
    let channels = usize::from(channels);
    let errors = Arc::clone(&state);
    device
        .build_input_stream(
            config,
            move |samples: &[T], _| {
                if state.signal.load(Ordering::Acquire) != CAPTURING {
                    return;
                }
                let mut data = Vec::with_capacity(samples.len() / input_channels * channels * 2);
                let mut peak = 0;
                for frame in samples.chunks_exact(input_channels) {
                    for sample in &frame[..channels] {
                        let sample = i16::from_sample_(*sample);
                        peak = peak.max(u32::from(sample.unsigned_abs()));
                        data.extend_from_slice(&sample.to_le_bytes());
                    }
                }
                state.peak.store(peak, Ordering::Relaxed);
                if !data.is_empty() && sender.try_send(data).is_err() {
                    state.overflow.store(true, Ordering::Relaxed);
                }
            },
            move |error| {
                if let Ok(mut failure) = errors.failure.lock() {
                    *failure = Some(format!("Microphone error. {error}"));
                }
            },
            None,
        )
        .context("Could not open the microphone stream.")
}

fn finish_wave(mut output: BufWriter<File>, rate: u32, channels: u16, bytes: u32) -> Result<()> {
    output.rewind()?;
    output.write_all(&wav_header(rate, channels, bytes))?;
    output.flush()?;
    output.get_ref().sync_all()?;
    Ok(())
}

fn wav_header(rate: u32, channels: u16, bytes: u32) -> [u8; 44] {
    let align = channels * 2;
    let mut header = [0; 44];
    header[..4].copy_from_slice(b"RIFF");
    header[4..8].copy_from_slice(&(bytes + 36).to_le_bytes());
    header[8..16].copy_from_slice(b"WAVEfmt ");
    header[16..20].copy_from_slice(&16_u32.to_le_bytes());
    header[20..22].copy_from_slice(&1_u16.to_le_bytes());
    header[22..24].copy_from_slice(&channels.to_le_bytes());
    header[24..28].copy_from_slice(&rate.to_le_bytes());
    header[28..32].copy_from_slice(&(rate * u32::from(align)).to_le_bytes());
    header[32..34].copy_from_slice(&align.to_le_bytes());
    header[34..36].copy_from_slice(&16_u16.to_le_bytes());
    header[36..40].copy_from_slice(b"data");
    header[40..].copy_from_slice(&bytes.to_le_bytes());
    header
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    #[test]
    #[ignore = "requires an explicitly configured virtual input playing a test signal"]
    fn capture_device_saves_audible_wav_and_discards_without_saving() -> Result<()> {
        let device = std::env::var("CUTAD_TEST_VOICEOVER_DEVICE").context(
            "Set CUTAD_TEST_VOICEOVER_DEVICE to a virtual input playing generated audio.",
        )?;
        let root = tempfile::tempdir()?;
        let wait = |condition: &mut dyn FnMut() -> bool| -> Result<()> {
            let deadline = Instant::now() + Duration::from_secs(6);
            while !condition() {
                ensure!(
                    Instant::now() < deadline,
                    "Timed out waiting for the virtual audio input."
                );
                std::thread::sleep(Duration::from_millis(10));
            }
            Ok(())
        };
        let record = |name| {
            Recording::start(
                Some(device.clone()),
                root.path().join(name),
                egui::Context::default(),
            )
        };
        let take = record("take.wav");
        let mut outcome = None;
        wait(&mut || {
            outcome = take.result();
            take.ready() || outcome.is_some()
        })?;
        if let Some(outcome) = outcome {
            outcome?;
            bail!("The audio input closed before capture started.");
        }
        take.begin();
        wait(&mut || {
            outcome = take.result();
            take.elapsed() >= Duration::from_millis(160) || outcome.is_some()
        })?;
        if let Some(outcome) = outcome {
            outcome?;
            bail!("The audio input closed while recording.");
        }
        take.stop(false);
        assert!(
            take.peak() > 0.015,
            "The recorded test signal must be audible."
        );
        wait(&mut || {
            outcome = take.result();
            outcome.is_some()
        })?;
        let path = outcome
            .take()
            .context("Missing recording result.")??
            .context("File was discarded.")?;
        let asset = cutad::media::probe(&path)?;
        assert!(asset.audio && asset.frames >= 4);
        let bytes = std::fs::read(path)?;
        assert_eq!(&bytes[..4], b"RIFF");
        assert_eq!(&bytes[8..12], b"WAVE");
        assert_eq!(
            u32::from_le_bytes(bytes[40..44].try_into()?) as usize,
            bytes.len() - 44
        );

        let discarded = record("discarded.wav");
        wait(&mut || discarded.ready())?;
        discarded.begin();
        discarded.stop(true);
        wait(&mut || {
            outcome = discarded.result();
            outcome.is_some()
        })?;
        assert!(outcome.context("Missing discard result.")??.is_none());
        assert!(!root.path().join("discarded.wav").exists());
        Ok(())
    }
}
