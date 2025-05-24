use super::TaggedWord;
use super::traits::{HasCursor, ReadIn};
use anyhow::{Result, anyhow};

// Now anything that implements HasStack + HasRegister + HasCursor + HasStaticConfig + Intepreter can implement Executor
// and have the method in intepreter correctly called with the inputs read according to the vm definition, with cursor allowing
// flexibility on how the memory is laid out (since we have to handle our ragged members).

// :::::::------ The Cursors that we need (defining how the memory is laid out) ----:::::

pub(super) struct LinearCursor {
    region_start: *const u8,
    region_end: *const u8,

    pub cursor: *const u8,
    last_read: Option<TaggedWord>,
    element_depth: i32,
}
impl LinearCursor {
    pub fn new(region_start: *const u8, region_end: *const u8) -> Self {
        Self {
            region_start,
            region_end,
            cursor: region_start,
            last_read: None,
            element_depth: 0,
        }
    }
}
impl LinearCursor {
    pub fn add_depth(&mut self) {
        self.element_depth += 1;
    }
    pub fn sub_depth(&mut self) {
        self.element_depth -= 1;
    }
}
impl HasCursor for LinearCursor {
    unsafe fn read_from_cursor(&mut self) -> Option<TaggedWord> {
        if self.element_depth > 0
            && (self.cursor >= self.region_start && self.cursor < self.region_end)
        {
            self.last_read = Some(unsafe { TaggedWord::read_in(&mut self.cursor) });
            self.last_read
        } else {
            None
        }
    }

    unsafe fn peak_cursor(&self) -> Option<TaggedWord> {
        self.last_read
    }
}

pub(super) struct RaggedCursor {
    regions: Vec<(*const u8, *const u8)>,

    pub cursor: *const u8,
    region_i: usize,
    last_read: Option<TaggedWord>,
}
impl RaggedCursor {
    pub fn new(regions: Vec<(*const u8, *const u8)>) -> Result<Self> {
        let cursor = regions.get(0).ok_or(anyhow!("Regions can't be empty"))?.0;

        Ok(RaggedCursor {
            regions,
            cursor,
            region_i: 0,
            last_read: None,
        })
    }
}
impl HasCursor for RaggedCursor {
    unsafe fn read_from_cursor(&mut self) -> Option<TaggedWord> {
        // Get the info for the current region
        if self.region_i >= self.regions.len() {
            return None;
        }
        let &(start, end) = self.regions.get(self.region_i).unwrap();
        // Check if we are at the end of the current region and skip ahead if we are
        let (start, end) = if self.cursor >= end {
            // skip to the enxt region
            self.region_i += 1;
            if self.region_i >= self.regions.len() {
                return None;
            }
            let &(start, end) = self.regions.get(self.region_i).unwrap();
            self.cursor = start;
            (start, end)
        } else {
            (start, end)
        };
        // Read normally
        if self.cursor >= start && self.cursor < end {
            self.last_read = Some(unsafe { TaggedWord::read_in(&mut self.cursor) });
            self.last_read
        } else {
            None
        }
    }

    unsafe fn peak_cursor(&self) -> Option<TaggedWord> {
        self.last_read
    }
}
