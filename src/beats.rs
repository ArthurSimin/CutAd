//! Estimate beat positions from changes in frequency-band energy.

use crate::project::FPS;
use anyhow::{Result, ensure};
use std::io::Read;
use std::sync::atomic::{AtomicBool, Ordering};

pub(crate) const SAMPLE_RATE: u32 = 16_000;
const HOPS_PER_SECOND: u32 = 50;
const HOP: usize = (SAMPLE_RATE / HOPS_PER_SECOND) as usize;

pub(crate) fn detect(mut pcm: impl Read, frames: u32, cancel: &AtomicBool) -> Result<Vec<u32>> {
    let mut input = [0_u8; HOP * 2];
    let mut rise = Vec::new();
    let mut previous = [0.0_f32; 3];
    let (mut bass, mut mid) = (0.0_f32, 0.0_f32);
    let hop_count = u64::from(frames) * u64::from(HOPS_PER_SECOND) / u64::from(FPS);
    for _ in 0..hop_count {
        ensure!(!cancel.load(Ordering::Relaxed), "Beat analysis canceled.");
        pcm.read_exact(&mut input)?;
        let mut energy = [0.0_f32; 3];
        for sample in input.chunks_exact(2) {
            let point = f32::from(i16::from_le_bytes([sample[0], sample[1]])) / 32_768.0;
            bass += (point - bass) * 0.045;
            mid += (point - mid) * 0.24;
            for (sum, band) in energy.iter_mut().zip([bass, mid - bass, point - mid]) {
                *sum += band * band;
            }
        }
        let bands = energy.map(|sum| (1.0 + sum * 480.0 / HOP as f32).ln());
        let difference: f32 = bands
            .iter()
            .zip(previous)
            .map(|(now, before)| (now - before).max(0.0))
            .sum();
        rise.push(difference);
        previous = bands;
    }

    let mut peaks = Vec::new();
    let mut strengths = vec![0.0_f32; rise.len()];
    for (index, &value) in rise.iter().enumerate() {
        let neighborhood = index.saturating_sub(2)..(index + 3).min(rise.len());
        let preceding = &rise[index.saturating_sub(20)..index];
        let baseline = preceding.iter().sum::<f32>() / preceding.len().max(1) as f32;
        if value < (1.9 * baseline).max(0.20)
            || rise[neighborhood].iter().any(|&nearby| nearby > value)
        {
            continue;
        }
        peaks.push(index);
        strengths[index] = (value - baseline).min(3.0);
    }
    if peaks.len() < 3 {
        return Ok(Vec::new());
    }

    let score = |period: usize| {
        peaks
            .iter()
            .map(|&index| {
                let mut score = strengths.get(index + period).copied().unwrap_or(0.0);
                for nearby in [index + period - 1, index + period + 1] {
                    score = score.max(strengths.get(nearby).copied().unwrap_or(0.0) * 0.75);
                }
                strengths[index].min(score)
            })
            .sum::<f32>()
    };
    // The broad 56–230 BPM search favors a directly observed beat over a multiple.
    let (period, correlation) = (13..=54)
        .map(|period| (period, score(period)))
        .max_by(|(left_period, left_score), (right_period, right_score)| {
            left_score
                .total_cmp(right_score)
                .then_with(|| right_period.cmp(left_period))
        })
        .expect("non-empty beat-period range");
    let observed = peaks.iter().map(|&peak| strengths[peak]).sum::<f32>();
    if correlation < observed * 0.22 {
        return Ok(Vec::new());
    }
    let mut linked = Vec::new();
    for peak in peaks {
        let tolerance = 2;
        let forward = peak + period - tolerance..peak + period + tolerance + 1;
        let paired = forward
            .into_iter()
            .any(|i| strengths.get(i).is_some_and(|value| *value > 0.0))
            || (period.saturating_sub(tolerance)..=period + tolerance)
                .filter_map(|step| peak.checked_sub(step))
                .any(|i| strengths[i] > 0.0);
        if paired && linked.last().is_none_or(|last| peak - last >= period / 2) {
            linked.push(peak);
        }
    }
    Ok(linked
        .into_iter()
        .map(|index| {
            ((index as u64 * u64::from(FPS) + u64::from(HOPS_PER_SECOND) / 2)
                / u64::from(HOPS_PER_SECOND)) as u32
        })
        .filter(|&tick| tick < frames)
        .collect())
}
