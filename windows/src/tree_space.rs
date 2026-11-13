//! Find a clear part of a ledge for the full-grown tree, even while it is small.
#[derive(Clone, Copy)]
pub struct Occupant {
    pub x: f32,
    pub bottom: f32,
    pub half_width: f32,
    pub height: f32,
}

pub fn nearest(desired: f32, left: f32, right: f32, bottom: f32, height: f32, half_width: f32, occupants: &[Occupant]) -> Option<f32> {
    let min = left + half_width;
    let max = right - half_width;
    if min > max {
        return None;
    }
    let mut blocked: Vec<(f32, f32)> = occupants
        .iter()
        .filter(|item| bottom < item.bottom + item.height && bottom + height > item.bottom)
        .map(|item| {
            let radius = item.half_width + half_width + 3.0;
            (item.x - radius, item.x + radius)
        })
        .collect();
    blocked.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut choices = vec![desired.clamp(min, max), min, max];
    for &(start, end) in &blocked {
        if start >= min && start <= max {
            choices.push(start);
        }
        if end >= min && end <= max {
            choices.push(end);
        }
    }
    choices
        .into_iter()
        .filter(|x| blocked.iter().all(|&(start, end)| *x <= start || *x >= end))
        .min_by(|a, b| (a - desired).abs().total_cmp(&(b - desired).abs()).then_with(|| a.total_cmp(b)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chooses_the_nearest_clear_space_and_reserves_the_grown_width() {
        let snowman = Occupant { x: 100.0, bottom: 0.0, half_width: 20.0, height: 100.0 };
        assert_eq!(nearest(100.0, 0.0, 300.0, 0.0, 140.0, 30.0, &[snowman]), Some(47.0));
        assert_eq!(nearest(180.0, 0.0, 300.0, 0.0, 140.0, 30.0, &[snowman]), Some(180.0));
        assert_eq!(nearest(100.0, 0.0, 300.0, 200.0, 50.0, 30.0, &[snowman]), Some(100.0));
        assert_eq!(nearest(100.0, 80.0, 120.0, 0.0, 100.0, 30.0, &[snowman]), None);
    }

    #[test]
    fn reports_when_the_whole_ledge_is_occupied() {
        let snowman = Occupant { x: 50.0, bottom: 0.0, half_width: 35.0, height: 100.0 };
        assert_eq!(nearest(50.0, 0.0, 100.0, 0.0, 100.0, 20.0, &[snowman]), None);
    }
}
