//! # Calendar
//!
//! A simple calendar storing events by date in the filesystem under
//! `/calendar/<date>`, persisting with the volume. Dates are `YYYY-MM-DD`
//! strings; each date holds a newline-separated list of event entries. Supports
//! adding events, listing a day's events, listing all dates with events, and
//! removing a day.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use cibos_sdk::Filesystem;

/// The calendar over a filesystem.
pub struct Calendar {
    fs: Filesystem,
}

fn key(date: &str) -> String {
    format!("/calendar/{date}")
}

fn valid_date(date: &str) -> bool {
    // YYYY-MM-DD shape check (not a full validity check).
    let parts: Vec<&str> = date.split('-').collect();
    parts.len() == 3
        && parts[0].len() == 4
        && parts[1].len() == 2
        && parts[2].len() == 2
        && parts.iter().all(|p| p.chars().all(|c| c.is_ascii_digit()))
}

impl Calendar {
    /// Open the calendar on `fs`.
    #[must_use]
    pub fn new(fs: Filesystem) -> Self {
        Calendar { fs }
    }

    /// Add an event to a date. Returns false if the date is malformed.
    pub fn add_event(&self, date: &str, event: &str) -> bool {
        if !valid_date(date) {
            return false;
        }
        let mut events = self.events(date);
        events.push(event.to_string());
        self.fs.write(&key(date), events.join("\n").as_bytes())
    }

    /// Events on a date, in insertion order.
    #[must_use]
    pub fn events(&self, date: &str) -> Vec<String> {
        match self.fs.read(&key(date)) {
            Some(bytes) => {
                let text = String::from_utf8_lossy(&bytes);
                if text.is_empty() {
                    Vec::new()
                } else {
                    text.split('\n').map(str::to_string).collect()
                }
            }
            None => Vec::new(),
        }
    }

    /// All dates that have events, sorted (lexicographic == chronological for
    /// `YYYY-MM-DD`).
    #[must_use]
    pub fn dates(&self) -> Vec<String> {
        // `list` returns immediate child names (the contract), so each entry is
        // already a bare date key.
        let mut dates = self.fs.list("/calendar/");
        dates.sort();
        dates
    }

    /// Remove all events on a date; returns whether the date existed.
    pub fn clear_date(&self, date: &str) -> bool {
        self.fs.delete(&key(date))
    }

    /// Total number of events across all dates.
    #[must_use]
    pub fn total_events(&self) -> usize {
        self.dates().iter().map(|d| self.events(d).len()).sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_and_list_events() {
        let cal = Calendar::new(Filesystem::new());
        assert!(cal.add_event("2026-06-01", "ship CIBOS"));
        assert!(cal.add_event("2026-06-01", "celebrate"));
        assert!(cal.add_event("2026-06-15", "review"));
        assert_eq!(
            cal.events("2026-06-01"),
            vec!["ship CIBOS".to_string(), "celebrate".to_string()]
        );
        assert_eq!(cal.dates(), vec!["2026-06-01".to_string(), "2026-06-15".to_string()]);
        assert_eq!(cal.total_events(), 3);
    }

    #[test]
    fn rejects_bad_dates_and_clears() {
        let cal = Calendar::new(Filesystem::new());
        assert!(!cal.add_event("June 1", "x"));
        assert!(!cal.add_event("2026-6-1", "x"));
        cal.add_event("2026-12-25", "holiday");
        assert!(cal.clear_date("2026-12-25"));
        assert!(cal.events("2026-12-25").is_empty());
        assert!(!cal.clear_date("2026-12-25"));
    }
}
