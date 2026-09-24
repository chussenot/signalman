//! Scoring rules for calibrated judgments. Pure functions over probabilities
//! and outcomes, so they are easy to check by hand and to reuse.

// Counts and bin indices are small; the casts to and from f64 are exact in
// practice and truncation is the intended bucketing.
#![allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)]

/// Multi-class Brier score for one question: the squared distance between
/// the reported distribution and the one-hot truth, summed over options.
/// `0.0` is perfect; `2.0` is full confidence in the wrong option.
pub fn brier(probabilities: &[(bool, f64)]) -> f64 {
    probabilities
        .iter()
        .map(|(truth, p)| {
            let y = if *truth { 1.0 } else { 0.0 };
            (p - y).powi(2)
        })
        .sum()
}

/// Expected calibration error over `(confidence, correct)` pairs: confidence
/// is bucketed into `bins` equal-width bins and the gap between mean
/// confidence and accuracy is averaged, weighted by bin size. `0.0` means
/// a 0.8 confidence was right 80% of the time.
pub fn expected_calibration_error(pairs: &[(f64, bool)], bins: usize) -> f64 {
    if pairs.is_empty() || bins == 0 {
        return 0.0;
    }
    let mut sum_conf = vec![0.0; bins];
    let mut sum_correct = vec![0.0; bins];
    let mut count = vec![0usize; bins];
    for (conf, correct) in pairs {
        let c = conf.clamp(0.0, 1.0);
        // 1.0 falls in the last bin, not one past it.
        let mut b = (c * bins as f64).floor() as usize;
        if b >= bins {
            b = bins - 1;
        }
        sum_conf[b] += c;
        sum_correct[b] += if *correct { 1.0 } else { 0.0 };
        count[b] += 1;
    }
    let n = pairs.len() as f64;
    (0..bins)
        .filter(|b| count[*b] > 0)
        .map(|b| {
            let k = count[b] as f64;
            (k / n) * ((sum_conf[b] / k) - (sum_correct[b] / k)).abs()
        })
        .sum()
}

/// Arithmetic mean; `None` when empty.
pub fn mean(values: &[f64]) -> Option<f64> {
    if values.is_empty() {
        None
    } else {
        Some(values.iter().sum::<f64>() / values.len() as f64)
    }
}

/// Nearest-rank percentile (`0.0..=1.0`) of unsorted values; `None` when empty.
pub fn percentile(values: &[f64], q: f64) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    let mut v = values.to_vec();
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let rank = ((q.clamp(0.0, 1.0) * v.len() as f64).ceil() as usize).clamp(1, v.len());
    Some(v[rank - 1])
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn brier_is_zero_when_certain_and_right_and_two_when_certain_and_wrong() {
        assert!((brier(&[(true, 1.0), (false, 0.0)]) - 0.0).abs() < 1e-12);
        assert!((brier(&[(true, 0.0), (false, 1.0)]) - 2.0).abs() < 1e-12);
        // Uniform over two options: 0.5 + 0.25 ... (0.5-1)^2 + (0.5-0)^2 = 0.5
        assert!((brier(&[(true, 0.5), (false, 0.5)]) - 0.5).abs() < 1e-12);
    }

    #[test]
    fn ece_is_zero_for_a_calibrated_model_and_large_for_an_overconfident_one() {
        // 80% confidence, right 4 times out of 5: perfectly calibrated bin.
        let calibrated = [
            (0.8, true),
            (0.8, true),
            (0.8, true),
            (0.8, true),
            (0.8, false),
        ];
        assert!(expected_calibration_error(&calibrated, 10).abs() < 1e-12);
        // 95% confidence, right half the time.
        let over = [(0.95, true), (0.95, false)];
        assert!((expected_calibration_error(&over, 10) - 0.45).abs() < 1e-12);
        assert!(expected_calibration_error(&[], 10).abs() < 1e-12);
    }

    #[test]
    fn percentiles_use_nearest_rank() {
        let v = [5.0, 1.0, 3.0, 2.0, 4.0];
        assert_eq!(percentile(&v, 0.5), Some(3.0));
        assert_eq!(percentile(&v, 0.95), Some(5.0));
        assert_eq!(percentile(&v, 0.0), Some(1.0));
        assert_eq!(percentile(&[], 0.5), None);
        assert_eq!(mean(&v), Some(3.0));
    }
}
