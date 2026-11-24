use crate::project::FPS;
use anyhow::{Result, bail, ensure};
use std::sync::atomic::{AtomicBool, Ordering};

pub const MAX_ANALYSIS: u32 = FPS * 45;
pub const MIN_OVERLAP: usize = (FPS * 3) as usize;

/// Match relative changes in loudness while tolerating different recorder gain.
pub fn offset(reference: &[f64], other: &[f64], cancel: &AtomicBool) -> Result<i32> {
    let maximum = (FPS * 30) as i32;
    let mut scores = Vec::new();
    for lag in -maximum..=maximum {
        if cancel.load(Ordering::Relaxed) {
            bail!("Canceled.");
        }
        let a = &reference[(lag.max(0) as usize).min(reference.len())..];
        let b = &other[((-lag).max(0) as usize).min(other.len())..];
        let overlap = a.len().min(b.len());
        if overlap < MIN_OVERLAP {
            continue;
        }
        let (mut sum_a, mut sum_b, mut square_a, mut square_b, mut product) =
            (0.0, 0.0, 0.0, 0.0, 0.0);
        for (&a, &b) in a.iter().zip(b) {
            sum_a += a;
            sum_b += b;
            square_a += a * a;
            square_b += b * b;
            product += a * b;
        }
        let count = overlap as f64;
        let variance_a = square_a - sum_a * sum_a / count;
        let variance_b = square_b - sum_b * sum_b / count;
        if variance_a <= count * 1e-6 || variance_b <= count * 1e-6 {
            continue;
        }
        let correlation = (product - sum_a * sum_b / count) / (variance_a * variance_b).sqrt();
        scores.push((lag, correlation));
    }
    let (lag, best) = scores
        .iter()
        .copied()
        .max_by(|a, b| a.1.total_cmp(&b.1))
        .ok_or_else(|| {
            anyhow::anyhow!("Choose clips with at least three seconds of audible overlap.")
        })?;
    ensure!(
        best >= 0.60
            && !scores
                .iter()
                .any(|(alternate, score)| (alternate - lag).abs() > 3 && *score >= best - 0.035),
        "Could not find a unique audio match. Choose clips with a common, distinctive sound."
    );
    Ok(lag)
}
