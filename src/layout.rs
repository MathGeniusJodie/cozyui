//! Bottom-anchored widget layout math: where each widget sits for a given
//! window height, and how tall the window needs to be to show everything.
//! Pure functions of primitive/`Rect` data — no framebuffer or widget access;
//! `main.rs` applies the results.

use crate::{Rect, WIDGET_COUNT, WidgetId};

/// Where every widget sits. Tweak positions here.
///
/// `(x, y)`: x is pixels from the window's left edge to the widget's left
/// edge; y is pixels from the BOTTOM of the window up to the widget's bottom
/// edge (y = 0 puts the widget flush with the bottom; negative y hangs that
/// many pixels of it below the bottom edge). Widgets keep their bottom-left
/// corner pinned, so ones with dynamic sizes grow up/right from here.
///
/// Fwends is the one exception: it is pinned to the window's TOP edge and
/// only its x is used.
const fn widget_xy(widget: WidgetId) -> (isize, isize) {
    match widget {
        WidgetId::Puter => (463, 39),
        WidgetId::Toodle => (322, 301),
        WidgetId::Fwends => (713, -10),
        WidgetId::Twirl => (587, 321),
        WidgetId::Wavey => (113, 31),
        WidgetId::Fizzle => (393, 31),
        // 330 was Day's plain-view x before it grew a week-number column;
        // shifting left by that column's width keeps the plain view's card
        // pinned to the same on-screen position (see `day::PLAIN_CARD_X`).
        WidgetId::Day => (330 - crate::day::WEEK_COL_W as isize, 197),
        WidgetId::Budgit => (322, 752),
        WidgetId::Stats => (322, 604),
        WidgetId::Hunger => (463, 3),
        WidgetId::Gauges => (335, 915),
    }
}

/// The desk background's bottom edge, in the same bottom-up coordinate as
/// `widget_xy` (negative = hangs below the window's bottom edge).
pub(super) const DESK_Y: isize = -39;

/// Empty space kept above the tallest widget when the window height is
/// content-driven rather than set by the window manager.
const TOP_PADDING: usize = 25;

/// How far below the window's top edge Fwends' top is pinned.
pub(super) const FWENDS_TOP: usize = 10;

/// Widget (x, y) top-left positions, indexed by `WidgetId::index()`.
/// Converts the bottom-anchored `widget_xy` table for a `screen_h`-tall
/// window: top = screen_h - y - height.
pub(super) fn widget_positions(
    heights: &[usize; WIDGET_COUNT],
    screen_h: usize,
) -> [(isize, isize); WIDGET_COUNT] {
    let mut positions = [(0, 0); WIDGET_COUNT];
    for widget in WidgetId::ALL {
        let (x, y) = widget_xy(widget);
        let h = heights[widget.index()] as isize;
        let top = (screen_h as isize - y - h).max(0);
        positions[widget.index()] = (x, top);
    }
    // Fwends stays pinned just below the window's top edge.
    positions[WidgetId::Fwends.index()].1 = FWENDS_TOP as isize;
    positions
}

/// Window height needed to show every bottom-anchored widget and the desk,
/// plus TOP_PADDING; the actual window height (`min_h`) wins if larger.
/// Fwends contributes its `widget_heights` entry (its minimum height) at
/// y = 0 like everything else, even though it ends up pinned to the top.
pub(super) fn required_screen_height(
    heights: &[usize; WIDGET_COUNT],
    desk_h: usize,
    min_h: usize,
) -> usize {
    let mut needed = (desk_h as isize + DESK_Y).max(0) as usize;
    for widget in WidgetId::ALL {
        let (_, y) = widget_xy(widget);
        let h = heights[widget.index()] as isize;
        needed = needed.max((h + y).max(0) as usize);
    }
    (needed + TOP_PADDING).max(min_h)
}

pub(super) const fn move_rect(rect: &mut Rect, x: isize, y: isize) -> bool {
    if rect.x == x && rect.y == y {
        return false;
    }

    rect.x = x;
    rect.y = y;
    true
}

pub(super) const fn rects_intersect(a: Rect, b: Rect) -> bool {
    a.x < b.x + b.w as isize
        && b.x < a.x + a.w as isize
        && a.y < b.y + b.h as isize
        && b.y < a.y + a.h as isize
}

pub(super) fn stretched_desk_source_x(x: usize, target_w: usize, source_w: usize) -> usize {
    if source_w <= 1 || target_w <= source_w {
        return x.min(source_w.saturating_sub(1));
    }

    let middle = source_w / 2;
    let left_w = middle;
    let right_w = source_w - middle - 1;
    let middle_w = target_w.saturating_sub(left_w + right_w).max(1);

    if x < left_w {
        x
    } else if x < left_w + middle_w {
        middle
    } else {
        middle + 1 + (x - left_w - middle_w).min(right_w.saturating_sub(1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn move_rect_reports_whether_it_actually_moved() {
        let mut rect = Rect::new(1, 2, 10, 10);

        assert!(!move_rect(&mut rect, 1, 2));
        assert!(move_rect(&mut rect, 3, 4));
        assert_eq!((rect.x, rect.y), (3, 4));
    }

    #[test]
    fn rects_intersect_excludes_merely_touching_edges() {
        let a = Rect::new(0, 0, 10, 10);

        assert!(rects_intersect(a, Rect::new(5, 5, 10, 10)));
        // Shares only the edge at x=10; `<` (not `<=`) means this must not count.
        assert!(!rects_intersect(a, Rect::new(10, 0, 10, 10)));
        assert!(!rects_intersect(a, Rect::new(20, 20, 10, 10)));
    }

    #[test]
    fn stretched_desk_source_x_is_identity_when_target_no_wider_than_source() {
        assert_eq!(stretched_desk_source_x(0, 5, 10), 0);
        assert_eq!(stretched_desk_source_x(9, 5, 10), 9);
        // target_w clamps to source_w - 1, never past the last source column.
        assert_eq!(stretched_desk_source_x(9, 10, 10), 9);
    }

    #[test]
    fn stretched_desk_source_x_degenerates_to_zero_for_a_hairline_source() {
        assert_eq!(stretched_desk_source_x(0, 100, 1), 0);
        assert_eq!(stretched_desk_source_x(50, 100, 0), 0);
    }

    #[test]
    fn stretched_desk_source_x_preserves_edges_and_repeats_the_middle_column() {
        // source_w = 5 -> left edge is column 0, middle is column 2, right
        // edge is column 4; stretching to target_w = 11 repeats the middle.
        assert_eq!(stretched_desk_source_x(0, 11, 5), 0);
        assert_eq!(stretched_desk_source_x(1, 11, 5), 1);
        assert_eq!(stretched_desk_source_x(5, 11, 5), 2);
        assert_eq!(stretched_desk_source_x(9, 11, 5), 3);
        assert_eq!(stretched_desk_source_x(10, 11, 5), 4);
    }

    #[test]
    fn widget_positions_always_pins_fwends_below_the_top_edge() {
        let heights = [0; WIDGET_COUNT];

        for screen_h in [0, 100, 2000] {
            let positions = widget_positions(&heights, screen_h);
            assert_eq!(positions[WidgetId::Fwends.index()].1, FWENDS_TOP as isize);
        }
    }

    #[test]
    fn required_screen_height_floors_to_the_real_window_size() {
        let heights = [0; WIDGET_COUNT];

        // No widget/desk content comes anywhere close to this, so the
        // ConfigureNotify-reported `min_h` floor must win outright.
        assert_eq!(required_screen_height(&heights, 0, 100_000), 100_000);
    }
}
