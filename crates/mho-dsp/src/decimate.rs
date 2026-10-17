//! Min/max decimation: reduces long records for plotting without hiding glitches.

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MinMax {
    /// Index of the first sample in the bucket.
    pub start: usize,
    pub min: f64,
    pub max: f64,
}

/// Splits `v` into `buckets` equally sized ranges and returns min/max of each.
/// Returns one bucket per sample if `v` is already short enough.
pub fn minmax_decimate(v: &[f64], buckets: usize) -> Vec<MinMax> {
    if v.is_empty() || buckets == 0 {
        return Vec::new();
    }
    if v.len() <= buckets {
        return v.iter().enumerate().map(|(i, &x)| MinMax { start: i, min: x, max: x }).collect();
    }
    let size = v.len() as f64 / buckets as f64;
    (0..buckets)
        .map(|b| {
            let start = (b as f64 * size) as usize;
            let end = (((b + 1) as f64 * size) as usize).min(v.len()).max(start + 1);
            let (min, max) = v[start..end]
                .iter()
                .fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), &x| (lo.min(x), hi.max(x)));
            MinMax { start, min, max }
        })
        .collect()
}

/// Flattens min/max buckets into an alternating polyline `(index, value)` suitable for
/// line plots: each bucket contributes its min and max point.
pub fn minmax_polyline(buckets: &[MinMax]) -> Vec<(usize, f64)> {
    let mut out = Vec::with_capacity(buckets.len() * 2);
    for b in buckets {
        if b.min == b.max {
            out.push((b.start, b.min));
        } else {
            out.push((b.start, b.min));
            out.push((b.start, b.max));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_spike() {
        let mut v = vec![0.0; 10_000];
        v[4321] = 5.0;
        let d = minmax_decimate(&v, 100);
        assert_eq!(d.len(), 100);
        assert!(d.iter().any(|b| b.max == 5.0));
    }

    #[test]
    fn short_input_passthrough() {
        let d = minmax_decimate(&[1.0, 2.0], 10);
        assert_eq!(d.len(), 2);
        assert_eq!(minmax_polyline(&d), vec![(0, 1.0), (1, 2.0)]);
    }
}
