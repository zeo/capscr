// the ratios a shift-drag snaps to; frontend/src/views/selection.ts keeps the
// same list and both read fixtures/aspect.json in their tests
const ASPECT_RATIOS: [f64; 5] = [1.0, 16.0 / 9.0, 16.0 / 10.0, 4.0 / 3.0, 21.0 / 9.0];

/// the width:height ratio nearest to the drag, or None when either side is 0
pub fn aspect_target(width: f64, height: f64) -> Option<f64> {
    if width <= 0.0 || height <= 0.0 {
        return None;
    }
    let ratio = width / height;
    ASPECT_RATIOS
        .into_iter()
        .min_by(|a, b| (ratio - a).abs().total_cmp(&(ratio - b).abs()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(serde::Deserialize)]
    struct Case {
        width: f64,
        height: f64,
        snap: Option<(f64, f64)>,
    }

    #[test]
    fn aspect_target_matches_the_shared_fixture() {
        let cases: Vec<Case> = serde_json::from_str(include_str!("fixtures/aspect.json")).unwrap();
        for case in cases {
            assert_eq!(
                aspect_target(case.width, case.height),
                case.snap.map(|(w, h)| w / h),
                "{}x{}",
                case.width,
                case.height
            );
        }
    }
}
