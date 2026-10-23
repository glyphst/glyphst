//! The table's columns (frame 04d): each as wide as its widest text in the
//! rows shown, venue and date added when the window has room for them, and
//! the title taking the rest. Width beyond what every column's text needs is
//! shared out in proportion to it, so no one column holds all the space.

use serde::{Deserialize, Serialize};

/// A column of the table, left to right.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Column {
    Title,
    Authors,
    Venue,
    Year,
    Added,
    Tags,
    Read,
}

impl Column {
    /// Every column, in the table's order.
    pub const ALL: [Column; 7] = [
        Column::Title,
        Column::Authors,
        Column::Venue,
        Column::Year,
        Column::Added,
        Column::Tags,
        Column::Read,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Column::Title => "title",
            Column::Authors => "authors",
            Column::Venue => "venue",
            Column::Year => "year",
            Column::Added => "added",
            Column::Tags => "tags",
            Column::Read => "read",
        }
    }

    /// Shown only when the window has room for it.
    fn optional(self) -> bool {
        matches!(self, Column::Venue | Column::Added)
    }
}

pub const GAP: f32 = 12.0;
const YEAR: f32 = 44.0;
const ADDED: f32 = 72.0;
const READ: f32 = 84.0;
const TITLE_MIN: f32 = 200.0;
/// An optional column comes in while the title keeps at least this much
/// of its text (or all of it, when shorter).
const TITLE_ROOM: f32 = 520.0;

/// The widest text of each column over the rows shown, in points.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Natural {
    pub title: f32,
    pub authors: f32,
    pub venue: f32,
    pub tags: f32,
}

impl Natural {
    /// The larger of each width.
    pub fn max(self, other: Natural) -> Natural {
        Natural {
            title: self.title.max(other.title),
            authors: self.authors.max(other.authors),
            venue: self.venue.max(other.venue),
            tags: self.tags.max(other.tags),
        }
    }
}

/// Where each shown column starts and how wide its cell is.
#[derive(Clone, Debug, PartialEq)]
pub struct Layout {
    pub columns: Vec<(Column, f32, f32)>,
}

impl Layout {
    /// The column's left edge and width, when it is shown.
    pub fn get(&self, column: Column) -> Option<(f32, f32)> {
        self.columns
            .iter()
            .find(|(shown, _, _)| *shown == column)
            .map(|&(_, x, width)| (x, width))
    }
}

/// The columns of a table `width` wide whose rows need `natural`, without
/// the `hidden` ones.
pub fn layout(width: f32, natural: Natural, hidden: &[Column]) -> Layout {
    let flexible = |column: Column| match column {
        Column::Title => natural.title.max(TITLE_MIN),
        Column::Authors => natural.authors.clamp(90.0, 300.0),
        Column::Venue => natural.venue.clamp(80.0, 260.0),
        Column::Tags => natural.tags.clamp(120.0, 520.0),
        Column::Year => YEAR,
        Column::Added => ADDED,
        Column::Read => READ,
    };
    let mut shown = Column::ALL
        .into_iter()
        .filter(|column| !column.optional() && !hidden.contains(column))
        .collect::<Vec<_>>();
    // The width the title has once the others have theirs.
    let title_room = |shown: &[Column]| {
        let others = shown
            .iter()
            .filter(|column| **column != Column::Title)
            .map(|column| flexible(*column) + GAP)
            .sum::<f32>();
        width - others
    };
    for optional in [Column::Venue, Column::Added] {
        if hidden.contains(&optional) || (optional == Column::Venue && natural.venue <= 0.0) {
            continue;
        }
        let mut with = shown.clone();
        with.push(optional);
        if title_room(&with) >= natural.title.clamp(TITLE_MIN, TITLE_ROOM) {
            shown = with;
        }
    }
    shown.sort_by_key(|column| Column::ALL.iter().position(|known| known == column));

    let mut widths = shown
        .iter()
        .map(|column| (*column, flexible(*column)))
        .collect::<Vec<_>>();
    let room = title_room(&shown);
    let title_need = natural.title.max(TITLE_MIN);
    if room > title_need {
        // Room to spare: the title gets its text, and what is left is
        // shared out by how much text each flexible column holds.
        let spare = room - title_need;
        let weight = |column: Column, width: f32| match column {
            Column::Year | Column::Added | Column::Read => 0.0,
            _ => width,
        };
        if let Some((_, width)) = widths.iter_mut().find(|(shown, _)| *shown == Column::Title) {
            *width = title_need;
        }
        let total = widths
            .iter()
            .map(|(column, width)| weight(*column, *width))
            .sum::<f32>()
            .max(1.0);
        for (column, width) in &mut widths {
            *width += spare * weight(*column, *width) / total;
        }
    } else {
        // Too narrow: the title, authors and tags each give way in
        // proportion to what they hold above their floors.
        if let Some((_, width)) = widths.iter_mut().find(|(shown, _)| *shown == Column::Title) {
            *width = title_need;
        }
        let floor = |column: Column| match column {
            Column::Title => TITLE_MIN,
            Column::Authors => 90.0,
            Column::Venue => 80.0,
            Column::Tags => 120.0,
            _ => f32::INFINITY,
        };
        let above = |column: Column, width: f32| (width - floor(column)).max(0.0);
        let short = title_need - room;
        let total = widths
            .iter()
            .map(|(column, width)| above(*column, *width))
            .sum::<f32>();
        if total > 0.0 {
            let share = (short / total).min(1.0);
            for (column, width) in &mut widths {
                *width -= above(*column, *width) * share;
            }
        }
    }
    let mut x = 0.0;
    let columns = widths
        .into_iter()
        .map(|(column, width)| {
            let at = x;
            x += width + GAP;
            (column, at, width)
        })
        .collect();
    Layout { columns }
}

/// An item's authors in at most `width`: as many surnames as fit, up to
/// four, then `+n` for the rest. `measure` gives a text's width.
pub fn authors_fit(surnames: &[String], width: f32, measure: impl Fn(&str) -> f32) -> String {
    let line = |shown: usize| {
        let mut text = surnames[..shown].join(", ");
        if surnames.len() > shown {
            text.push_str(&format!(" +{}", surnames.len() - shown));
        }
        text
    };
    if surnames.is_empty() {
        return String::new();
    }
    for shown in (1..=surnames.len().min(4)).rev() {
        let text = line(shown);
        if measure(&text) <= width {
            return text;
        }
    }
    line(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn natural() -> Natural {
        Natural {
            title: 420.0,
            authors: 110.0,
            venue: 70.0,
            tags: 260.0,
        }
    }

    fn total(layout: &Layout) -> f32 {
        let (_, x, width) = layout.columns.last().copied().unwrap();
        x + width
    }

    #[test]
    fn narrow_authors_take_only_their_text() {
        let layout = layout(1100.0, natural(), &[]);
        let (_, authors) = layout.get(Column::Authors).unwrap();
        assert!(authors < 140.0, "{authors}");
        assert!(total(&layout) <= 1100.0 + 0.5);
    }

    #[test]
    fn a_wide_window_adds_the_venue_and_date_added() {
        let narrow = layout(900.0, natural(), &[]);
        assert!(narrow.get(Column::Venue).is_none());
        assert!(narrow.get(Column::Added).is_none());
        let wide = layout(2400.0, natural(), &[]);
        assert!(wide.get(Column::Venue).is_some());
        assert!(wide.get(Column::Added).is_some());
        assert!((total(&wide) - 2400.0).abs() < 1.0);
        // The spare width is shared: the title is not most of the table.
        let (_, title) = wide.get(Column::Title).unwrap();
        assert!(title / 2400.0 < 0.5, "{title}");
        let (_, authors) = wide.get(Column::Authors).unwrap();
        assert!(authors > 110.0);
    }

    #[test]
    fn long_titles_still_leave_room_for_the_date_added() {
        let natural = Natural {
            title: 720.0,
            authors: 360.0,
            venue: 0.0,
            tags: 280.0,
        };
        let layout = layout(1590.0, natural, &[]);
        assert!(layout.get(Column::Added).is_some(), "{layout:?}");
        assert!(layout.get(Column::Venue).is_none());
    }

    #[test]
    fn a_narrow_table_shares_the_shortfall() {
        let natural = Natural {
            title: 522.0,
            authors: 263.0,
            venue: 137.0,
            tags: 307.0,
        };
        let layout = layout(1144.0, natural, &[]);
        assert!((total(&layout) - 1144.0).abs() < 1.0, "{layout:?}");
        let (_, title) = layout.get(Column::Title).unwrap();
        let (_, tags) = layout.get(Column::Tags).unwrap();
        assert!(title < 522.0 && tags > 200.0, "{layout:?}");
    }

    #[test]
    fn columns_the_user_turned_off_stay_off() {
        let hidden = [Column::Venue, Column::Year];
        let layout = layout(2400.0, natural(), &hidden);
        assert!(layout.get(Column::Venue).is_none());
        assert!(layout.get(Column::Year).is_none());
        assert!(layout.get(Column::Added).is_some());
    }

    #[test]
    fn the_title_keeps_its_floor() {
        for width in [760.0, 900.0, 1200.0, 1600.0] {
            let layout = layout(width, natural(), &[]);
            let (_, title) = layout.get(Column::Title).unwrap();
            assert!(title >= TITLE_MIN, "{width}: {title}");
            assert!(total(&layout) <= width + 0.5, "{width}");
            let starts = layout
                .columns
                .iter()
                .map(|(_, x, _)| *x)
                .collect::<Vec<_>>();
            assert!(starts.windows(2).all(|pair| pair[0] < pair[1]));
        }
    }

    #[test]
    fn authors_show_as_many_names_as_fit() {
        let names = ["Vaswani", "Shazeer", "Parmar", "Uszkoreit", "Jones"]
            .map(String::from)
            .to_vec();
        let measure = |text: &str| text.chars().count() as f32 * 7.0;
        assert_eq!(
            authors_fit(&names, 400.0, measure),
            "Vaswani, Shazeer, Parmar, Uszkoreit +1"
        );
        assert_eq!(authors_fit(&names, 150.0, measure), "Vaswani, Shazeer +3");
        assert_eq!(authors_fit(&names, 20.0, measure), "Vaswani +4");
        assert_eq!(authors_fit(&names[..1], 20.0, measure), "Vaswani");
    }
}
