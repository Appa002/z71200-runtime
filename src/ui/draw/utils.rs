use std::time::Duration;

use super::TaggedWord;
use super::traits::{HasStaticConfig, ReadIn};
use anyhow::{Context, Result};

/* :---- Book keeping and utils ---- */

#[derive(Debug, Clone, Copy)]
pub(super) struct StaticConfig {
    file_start: *const u8,
    base_font_size: f32,
    display_scale: f32,
    #[allow(dead_code)]
    dt: Duration,
}
impl StaticConfig {
    pub fn new(
        file_start: *const u8,
        base_font_size: f32,
        display_scale: f32,
        dt: Duration,
    ) -> Self {
        Self {
            file_start,
            base_font_size,
            display_scale,
            dt,
        }
    }
}

impl HasStaticConfig for StaticConfig {
    fn file_start(&self) -> *const u8 {
        self.file_start
    }

    fn base_font_size(&self) -> f32 {
        self.base_font_size
    }

    fn display_scale(&self) -> f32 {
        self.display_scale
    }

    fn get_dt(&self) -> Duration {
        self.dt
    }
}

pub(super) trait IntoCompactLength {
    fn into_compact(&self) -> taffy::CompactLength;
}
impl IntoCompactLength for taffy::LengthPercentage {
    fn into_compact(&self) -> taffy::CompactLength {
        self.into_raw()
    }
}
impl IntoCompactLength for taffy::LengthPercentageAuto {
    fn into_compact(&self) -> taffy::CompactLength {
        self.into_raw()
    }
}

pub(super) fn resolve_taffy_length<T>(length: T, extend: f32) -> f32
where
    T: IntoCompactLength,
{
    let compact: taffy::CompactLength = length.into_compact();
    if compact.tag() == taffy::CompactLength::AUTO_TAG {
        extend
    } else if compact.tag() == taffy::CompactLength::LENGTH_TAG {
        compact.value()
    } else if compact.tag() == taffy::CompactLength::PERCENT_TAG {
        compact.value() * extend
    } else {
        0.0
    }
}

pub fn read_str_from_array_tagged_word(ptr: usize, file_start: *const u8) -> Result<String> {
    let mut str_cursor = unsafe { file_start.add(ptr) };
    let size = unsafe { TaggedWord::read_in(&mut str_cursor) }
        .read_as_array()
        .with_context(|| format!("Reading string at loc {:x} failed.", ptr))?;

    let str = std::str::from_utf8(unsafe { std::slice::from_raw_parts(str_cursor, size) })?;
    Ok(str.to_owned())
}
