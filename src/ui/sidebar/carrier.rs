//! A marker that travels the tree's own lines, and where it is at a given
//! progress.
//!
//! The failure spider was the first thing that needed a *spatial* position on
//! the tree rather than a temporal one. [`crate::anim::ElementId::TrunkSegment`]
//! deliberately cannot give it: a segment is read at a fixed `1×1` extent so it
//! paints as one object rather than a per-terminal-row gradient, which makes it
//! a single point in its run and never a position along its own length. So a
//! travelling marker reads its own element's bounded `progress` against
//! geometry computed from the tree's layout, and this is that geometry.
//!
//! Split out of the spider rather than left inside it because it is the shape
//! the *next* carrier wants too — `data/herdr-spider-glyph-build/README.md`
//! names "a pane carries a signal along a connector" as the item this should
//! serve, and says plainly that it should reuse the waypoint-and-lerp approach
//! "generalised past four fixed legs" instead of growing a second one. That is
//! the only reason this is a path of arbitrary length rather than the spider's
//! own fixed five points: a connector carrier walks a different, shorter route
//! over the same cell grid, and it must not have to reimplement arc-length
//! parameterisation to do it.

/// A path over the cell grid, walked at a bounded progress.
///
/// Every leg is a single cell-grid axis move in the paths built for it, never a
/// diagonal, because that is how the tree's own lines are drawn — see the
/// `AGENTS.md` entry on the character tree being the layout authority. Nothing
/// here *enforces* that, because arc-length parameterisation over a diagonal is
/// still well defined; it is a property of the waypoints a caller supplies.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct CarrierPath {
    waypoints: Vec<(u16, u16)>,
    /// Each leg's length in cells, indexed like `waypoints.windows(2)`.
    legs: Vec<f32>,
    total: f32,
}

impl CarrierPath {
    /// `None` for a path with nothing to walk — fewer than two points, which is
    /// a position rather than a route.
    ///
    /// A path whose points are all the *same* point is not `None`: it has a
    /// well-defined position (that point) at every `t`, and a caller that
    /// computed a degenerate route from a zero-height card should get the
    /// answer rather than have to special-case it.
    pub(crate) fn new(waypoints: impl IntoIterator<Item = (u16, u16)>) -> Option<Self> {
        let waypoints: Vec<(u16, u16)> = waypoints.into_iter().collect();
        if waypoints.len() < 2 {
            return None;
        }
        // Manhattan length, not Euclidean: the legs are axis moves over a cell
        // grid, so a leg's length is the number of cells the marker steps
        // through. Using a Euclidean norm here would be the same number for
        // every axis-aligned leg anyway, and wrong the moment a caller supplies
        // a diagonal one — it would under-count the cells actually crossed.
        let legs: Vec<f32> = waypoints
            .windows(2)
            .map(|pair| {
                let (x0, y0) = pair[0];
                let (x1, y1) = pair[1];
                f32::from(x0.abs_diff(x1)) + f32::from(y0.abs_diff(y1))
            })
            .collect();
        let total = legs.iter().sum();
        Some(Self {
            waypoints,
            legs,
            total,
        })
    }

    /// Where the marker sits at `t` in `0.0..=1.0`: `0.0` is the first
    /// waypoint, `1.0` the last.
    ///
    /// Each leg gets a share of `t` proportional to its own length in cells, so
    /// a long leg is not rushed relative to a short one and the marker moves at
    /// one speed for the whole route rather than one speed per leg.
    pub(crate) fn position(&self, t: f32) -> (u16, u16) {
        let last = self.waypoints[self.waypoints.len() - 1];
        let t = t.clamp(0.0, 1.0);
        if self.total <= 0.0 {
            return last;
        }
        let mut travelled = t * self.total;
        for (i, &len) in self.legs.iter().enumerate() {
            if travelled <= len || i == self.legs.len() - 1 {
                let leg_t = if len > 0.0 {
                    (travelled / len).clamp(0.0, 1.0)
                } else {
                    1.0
                };
                let (x0, y0) = self.waypoints[i];
                let (x1, y1) = self.waypoints[i + 1];
                return (lerp_u16(x0, x1, leg_t), lerp_u16(y0, y1, leg_t));
            }
            travelled -= len;
        }
        last
    }
}

fn lerp_u16(a: u16, b: u16, t: f32) -> u16 {
    let a = f32::from(a);
    let b = f32::from(b);
    (a + (b - a) * t).round().max(0.0) as u16
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_path_needs_two_points_to_be_a_route() {
        assert!(CarrierPath::new([]).is_none());
        assert!(
            CarrierPath::new([(3, 4)]).is_none(),
            "one point is a position"
        );
        assert!(CarrierPath::new([(3, 4), (3, 9)]).is_some());
    }

    #[test]
    fn a_walk_starts_at_the_first_point_and_ends_at_the_last() {
        let path = CarrierPath::new([(2, 10), (2, 4), (9, 4)]).expect("two or more points");
        assert_eq!(path.position(0.0), (2, 10));
        assert_eq!(path.position(1.0), (9, 4));
    }

    #[test]
    fn progress_outside_the_unit_range_is_clamped_rather_than_extrapolated() {
        let path = CarrierPath::new([(2, 10), (2, 4)]).expect("two or more points");
        assert_eq!(path.position(-5.0), (2, 10));
        assert_eq!(path.position(17.0), (2, 4));
    }

    /// The property the spider's own climb was asserted on before this was
    /// extracted, and the reason legs are weighted by length: with each leg
    /// given an equal share of `t` instead, a marker crossing a 1-cell leg and
    /// a 20-cell leg would spend the same time on each and visibly stall.
    #[test]
    fn a_long_leg_takes_proportionally_longer_than_a_short_one() {
        // Ten cells up, then one cell across: eleven cells total, so the corner
        // is reached at 10/11 of the way through.
        let path = CarrierPath::new([(0, 10), (0, 0), (1, 0)]).expect("two or more points");
        assert_eq!(path.position(10.0 / 11.0), (0, 0), "at the corner");
        assert_eq!(
            path.position(0.5),
            (0, 5),
            "half way is half way up the long leg, not at the corner"
        );
    }

    #[test]
    fn a_walk_never_moves_backwards() {
        let path = CarrierPath::new([(0, 12), (0, 3), (7, 3), (7, 0)]).expect("two or more points");
        let mut previous = path.position(0.0);
        let mut travelled = 0u32;
        for step in 1..=200 {
            let current = path.position(step as f32 / 200.0);
            travelled += previous.0.abs_diff(current.0) as u32;
            travelled += previous.1.abs_diff(current.1) as u32;
            previous = current;
        }
        // Monotone along the route: the total distance stepped equals the
        // route's own length, which it cannot if the walk ever doubled back.
        assert_eq!(travelled, 9 + 7 + 3);
    }

    /// A degenerate route still answers. A card measured at zero height can
    /// produce one, and the caller should get its single point rather than a
    /// `None` it has to translate into the same point itself.
    #[test]
    fn a_path_that_goes_nowhere_answers_with_its_own_point() {
        let path = CarrierPath::new([(6, 2), (6, 2), (6, 2)]).expect("two or more points");
        assert_eq!(path.position(0.0), (6, 2));
        assert_eq!(path.position(0.5), (6, 2));
        assert_eq!(path.position(1.0), (6, 2));
    }
}
