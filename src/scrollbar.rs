//! A vertical scrollbar: where its thumb is, and what a click or a drag on
//! its track means. Pure geometry, so the panel's chat can draw it and the
//! mouse can use it with the same numbers.

use ratatui::layout::Rect;

/// The thumb inside a track `track` rows tall: (first row, rows). None when
/// everything fits (no bar needed). `top` is the first line shown.
pub fn thumb(track: u16, total: usize, view: usize, top: usize) -> Option<(u16, u16)> {
    if track == 0 || view == 0 || total <= view {
        return None;
    }
    let t = track as usize;
    let len = (t * view / total).clamp(1, t);
    let room = t - len;
    let max_top = total - view;
    let top = top.min(max_top);
    let start = (top * room + max_top / 2) / max_top;
    Some((start as u16, len as u16))
}

/// The first line shown when the thumb's first row is `start`.
pub fn top_for(track: u16, total: usize, view: usize, start: u16) -> usize {
    let Some((_, len)) = thumb(track, total, view, 0) else {
        return 0;
    };
    let room = (track - len) as usize;
    let max_top = total - view;
    if room == 0 {
        return 0;
    }
    ((start as usize).min(room) * max_top + room / 2) / room
}

/// A press at row `y` of the track (0 = its first row). On the thumb: the
/// grab offset inside it (a drag keeps it). Off it: None (the caller jumps
/// there with `top_at`).
pub fn grab(track: u16, total: usize, view: usize, top: usize, y: u16) -> Option<u16> {
    let (s, len) = thumb(track, total, view, top)?;
    (y >= s && y < s + len).then(|| y - s)
}

/// The first line shown with the pointer at row `y` of the track and the
/// thumb held at `grab` rows from its start (a click off the thumb centres
/// it there).
pub fn top_at(track: u16, total: usize, view: usize, y: u16, grab: u16) -> usize {
    top_for(track, total, view, y.saturating_sub(grab))
}

/// Where a bar is: the column at the right edge of `body`, and its rows.
pub fn bar_rect(body: Rect) -> Rect {
    Rect::new(
        body.x + body.width.saturating_sub(1),
        body.y,
        u16::from(body.width > 0),
        body.height,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_thumb_when_everything_fits() {
        assert_eq!(thumb(10, 10, 10, 0), None);
        assert_eq!(thumb(10, 3, 10, 0), None);
        assert_eq!(thumb(0, 100, 10, 0), None);
    }

    #[test]
    fn the_thumb_is_proportional_and_reaches_both_ends() {
        // 100 lines, 10 in view, a 20 row track: a 2 row thumb.
        assert_eq!(thumb(20, 100, 10, 0), Some((0, 2)));
        assert_eq!(thumb(20, 100, 10, 90), Some((18, 2)));
        // Never past the end, however far `top` is.
        assert_eq!(thumb(20, 100, 10, 5000), Some((18, 2)));
        // Never smaller than one row.
        assert_eq!(thumb(5, 10_000, 5, 0), Some((0, 1)));
        // The middle.
        let (s, _) = thumb(20, 100, 10, 45).unwrap();
        assert!((8..=10).contains(&s), "{s}");
    }

    #[test]
    fn dragging_maps_back_to_lines() {
        for top in [0usize, 1, 17, 45, 89, 90] {
            let (s, _) = thumb(20, 100, 10, top).unwrap();
            let back = top_for(20, 100, 10, s);
            // Rounded to a row of the track: within one thumb step.
            assert!(back.abs_diff(top) <= 100 / 20, "{top} -> {s} -> {back}");
        }
        assert_eq!(top_for(20, 100, 10, 0), 0);
        assert_eq!(top_for(20, 100, 10, 18), 90);
        assert_eq!(top_for(20, 100, 10, 200), 90);
    }

    #[test]
    fn a_press_on_the_thumb_grabs_it_off_it_does_not() {
        // Thumb rows 0..2 at the top.
        assert_eq!(grab(20, 100, 10, 0, 0), Some(0));
        assert_eq!(grab(20, 100, 10, 0, 1), Some(1));
        assert_eq!(grab(20, 100, 10, 0, 2), None);
        assert_eq!(grab(20, 100, 10, 0, 19), None);
        // A click off the thumb centres it: row 10 with a 2 row thumb.
        let top = top_at(20, 100, 10, 10, 1);
        let (s, _) = thumb(20, 100, 10, top).unwrap();
        assert_eq!(s, 9);
    }

    #[test]
    fn the_bar_is_the_last_column_of_the_body() {
        let r = bar_rect(Rect::new(4, 7, 30, 12));
        assert_eq!((r.x, r.y, r.width, r.height), (33, 7, 1, 12));
        assert_eq!(bar_rect(Rect::new(4, 7, 0, 12)).width, 0);
    }
}
