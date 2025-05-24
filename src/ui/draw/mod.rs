mod cursors;
mod draw_pass;
mod layout_pass;
mod text;
mod text_pass;
mod traits;
mod utils;
mod vm_state;

use std::{collections::HashMap, sync::Arc, time::Duration, usize};

use anyhow::{Result, anyhow};
use parley::FontContext;
use skia_safe::{Canvas, Color, HSV, RGB};
use strum::{EnumCount, EnumString};
use utils::StaticConfig;
use vm_state::VMState;
use winit::window::{CursorIcon, Window};

use draw_pass::draw_pass;
use layout_pass::layout_pass;
use text_pass::text_pass;

use super::InputState;

#[derive(Debug, Clone, Copy)]
pub struct CarriedState {
    pub is_jmp: bool,
    #[allow(dead_code)]
    pub scroll_y: f32,
}
impl CarriedState {
    pub fn new() -> Self {
        CarriedState {
            is_jmp: false,
            scroll_y: 0.0,
        }
    }
}

/* :----- Defines the representation of data in memory -----: */
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, EnumString, EnumCount, strum::Display, PartialEq, Eq)]
#[repr(usize)]
pub enum Tag {
    // Fundamental
    Array = 0,
    Pxs,  /*1*/
    Rems, /* 2 */
    Frac, /* 3 */
    Auto, /* 4 */

    // Color
    Rgb,  /* 5 */
    Hsv,  /* 6 */
    Rgba, /* 7 */
    Hsva, /* 8 */

    // Element Boundaries
    Enter, /*9 any */
    Leave, /*10 any */

    // Shape
    Rect,      /* 11 x, y, width, height */
    BeginPath, /* 12 */
    EndPath,   /* 13 */
    MoveTo,    /* 14 x, y */
    LineTo,    /* 15 x, y */
    QuadTo,    /* 16 cx, cy, x, y */
    CubicTo,   /* 17 cx1, cy1, cx2, cy2, x, y */
    ArcTo,     /* 18 */
    ClosePath, /* 19  */

    // Pencil
    Color, /* 20 _, TaggedWord{Rgb, param}  */

    // Layout
    Width,   /* 21 _, Pxs, param */
    Height,  // 22
    Padding, // 23 _, left, top, right, bottom
    Margin,  // 24
    Display, /* 25 display option */
    Gap,     /* 26 */

    // States
    Hover,        /* 27 rel_pointer, [... no jmp], [jmp ...] */
    MousePressed, /* 28 rel_pointer, [... no jmp], [jmp ...] */
    Clicked,      /* 29 rel_pointer, [... no jmp], [jmp ...] */
    OpenLatch,    /* 30 rel_pointer, [... no jmp], [jmp ...] */
    ClosedLatch,  /* 31 rel_pointer, [... no jmp], [jmp ...] */
    PushArg,      /* 34, any */
    PullArg,      /* 35 */
    PullArgOr,    /* 36 [default] */
    LoadReg,      /* 37 word */
    FromReg,      /* 38 word */
    FromRegOr,    /* 39 word */

    // Event
    Event, /* 40 word(id) */

    // Text
    Text,          /* 41 x, y, ptr */
    TextPtr,       /* 42 ptr  */
    FontSize,      /* 43 real */
    FontAlignment, /* 44 alignment */
    FontFamily,    /* 45 _, TextPtr */

    // Cursors
    CursorDefault, /* 46 */
    CursorPointer, /* 47 */
}

#[derive(Clone, Copy)]
#[repr(C)] /* should align to machine word */
pub struct TaggedWord {
    pub tag: Tag,         /* size: usize */
    pub word: ParamUnion, /*size: usize */
} // no padding since 2*usize are always aligned
impl std::fmt::Debug for TaggedWord {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self.tag)
    }
}

#[derive(Clone, Copy)]
#[repr(C)]
pub union ParamUnion {
    pub word: usize, /* this makes the size of the entire thing usize */
    pub real: f32,
    pub short_color: (u8, u8, u8),
    pub long_color: (u8, u8, u8, u8),
    pub display_option: DisplayOption,
    pub font_alignment: StoredAlignment,
    pub _debug_bytes: [u8; size_of::<usize>()],
}

#[derive(Debug, Clone, Copy)]
#[repr(usize)]
#[allow(dead_code)]
pub enum DisplayOption {
    Block = 0,
    FlexRow,    /* 1 */
    FlexColumn, /* 2 */
    Grid,       /* 3 */
    None,       /* 4 hidden */
}

#[derive(Debug, Clone, Copy)]
#[repr(usize)]
#[allow(dead_code)]
pub enum StoredAlignment {
    Start = 0,
    End,
    Left,
    Middle,
    Right,
    Justified,
}

/* :----- Defines the structure within a tagged word. ie how to inteprete the `word` bytes given a tag -----: */
trait ExtractFromWord {
    fn extract(param: &ParamUnion) -> Self;
}
impl ExtractFromWord for usize {
    fn extract(param: &ParamUnion) -> Self {
        unsafe { param.word }
    }
}
impl ExtractFromWord for f32 {
    fn extract(param: &ParamUnion) -> Self {
        unsafe { param.real }
    }
}
impl ExtractFromWord for () {
    fn extract(_param: &ParamUnion) -> Self {
        ()
    }
}
impl ExtractFromWord for DisplayOption {
    fn extract(param: &ParamUnion) -> Self {
        unsafe { param.display_option }
    }
}
impl ExtractFromWord for StoredAlignment {
    fn extract(param: &ParamUnion) -> Self {
        unsafe { param.font_alignment }
    }
}

impl ExtractFromWord for ParamUnion {
    fn extract(param: &ParamUnion) -> Self {
        param.clone()
    }
}
macro_rules! define_reader {
    ($name:ident, $tag:path, $return_type:ty) => {
        pub fn $name(&self) -> Result<$return_type> {
            match &self.tag {
                $tag => Ok(<$return_type as ExtractFromWord>::extract(&self.word)),
                _ => Err(anyhow!(
                    concat!(
                        "Expected `",
                        stringify!($tag),
                        "` tagged word, got `{}` instead"
                    ),
                    if self.tag as usize <= Tag::COUNT {
                        format!("{}", self.tag)
                    } else {
                        format!("corupted tag ({})", self.tag as usize)
                    },
                )),
            }
        }
    };
}

impl TaggedWord {
    define_reader!(read_as_array, Tag::Array, usize);
    define_reader!(read_as_event, Tag::Event, usize);
    define_reader!(read_as_hover, Tag::Hover, usize);
    define_reader!(read_as_mouse_pressed, Tag::MousePressed, usize);
    define_reader!(read_as_clicked, Tag::Clicked, usize);
    define_reader!(read_as_open_latch, Tag::OpenLatch, usize);
    define_reader!(read_as_closed_latch, Tag::ClosedLatch, usize);
    define_reader!(read_as_text_ptr, Tag::TextPtr, usize);
    define_reader!(read_as_display, Tag::Display, DisplayOption);
    define_reader!(read_as_font_size, Tag::FontSize, f32);
    define_reader!(read_as_font_alignment, Tag::FontAlignment, StoredAlignment);
    define_reader!(read_as_load_register, Tag::LoadReg, usize);

    pub fn read_as_any_color(&self) -> Result<Color> {
        match &self.tag {
            Tag::Rgb => {
                let (r, g, b) = unsafe { self.word.short_color };
                Ok(RGB { r, g, b }.to_hsv().to_color(255))
            }
            Tag::Hsv => {
                let (h, s, v) = unsafe { self.word.short_color };
                Ok(HSV {
                    h: h as f32 / 255.0,
                    s: s as f32 / 255.0,
                    v: v as f32 / 255.0,
                }
                .to_color(255))
            }
            Tag::Rgba => {
                let (r, g, b, a) = unsafe { self.word.long_color };
                Ok(RGB { r, g, b }.to_hsv().to_color(a))
            }
            Tag::Hsva => {
                let (h, s, v, a) = unsafe { self.word.long_color };
                Ok(HSV {
                    h: h as f32 / 255.0,
                    s: s as f32 / 255.0,
                    v: v as f32 / 255.0,
                }
                .to_color(a))
            }

            _ => Err(anyhow!(
                "Expected `Rgb`, `Hsv`, `Rgba`, or `Hsva` tagged word, got `{}` instead",
                if self.tag as usize <= Tag::COUNT {
                    format!("{}", self.tag)
                } else {
                    format!("corupted tag ({})", self.tag as usize)
                },
            )),
        }
    }

    pub fn read_as_taffy_length_pct(&self, base_font_size: f32) -> Result<taffy::LengthPercentage> {
        match &self.tag {
            Tag::Pxs => Ok(taffy::LengthPercentage::length(unsafe { self.word.real })),
            Tag::Rems => Ok(taffy::LengthPercentage::length(
                base_font_size * unsafe { self.word.real },
            )),
            Tag::Frac => Ok(taffy::LengthPercentage::percent(unsafe { self.word.real })),
            _ => Err(anyhow!(
                "Expected `Pxs`, `Rems`,  or `Frac` tagged word, got `{}` instead",
                if self.tag as usize <= Tag::COUNT {
                    format!("{}", self.tag)
                } else {
                    format!("corupted tag ({})", self.tag as usize)
                },
            )),
        }
    }

    pub fn read_as_taffy_length_pctauto(
        &self,
        base_font_size: f32,
    ) -> Result<taffy::LengthPercentageAuto> {
        match &self.tag {
            Tag::Auto => Ok(taffy::LengthPercentageAuto::auto()),
            Tag::Pxs => Ok(taffy::LengthPercentageAuto::length(unsafe {
                self.word.real
            })),
            Tag::Rems => Ok(taffy::LengthPercentageAuto::length(
                base_font_size * unsafe { self.word.real },
            )),
            Tag::Frac => Ok(taffy::LengthPercentageAuto::percent(unsafe {
                self.word.real
            })),
            _ => Err(anyhow!(
                "Expected `Pxs`, `Rems`, `Auto`, or `Frac` tagged word, got `{}` instead",
                if self.tag as usize <= Tag::COUNT {
                    format!("{}", self.tag)
                } else {
                    format!("corupted tag ({})", self.tag as usize)
                },
            )),
        }
    }

    pub fn read_as_any_cursor(&self) -> Result<CursorIcon> {
        match &self.tag {
            Tag::CursorDefault => Ok(CursorIcon::Default),
            Tag::CursorPointer => Ok(CursorIcon::Pointer),
            _ => Err(anyhow!(
                "Expected a tagged word of the `Cursor` family, got `{}` instead",
                if self.tag as usize <= Tag::COUNT {
                    format!("{}", self.tag)
                } else {
                    format!("corupted tag ({})", self.tag as usize)
                },
            )),
        }
    }
}

//::::: ----- Finally the main draw call ------
pub fn draw<F>(
    loc: usize,
    file_start: *const u8,
    file_end: *const u8,
    width: f32,
    height: f32,
    canvas: &Canvas,
    window: Arc<Window>,
    cb_push_evt: F,
    input_state: &InputState,
    font_ctx: &mut FontContext,
    layout_ctx: &mut parley::LayoutContext<()>,
    display_scale: f32,
    base_font_size: f32,
    frame_state: &HashMap<*const u8, CarriedState>,
    dt: Duration,
) -> Result<HashMap<*const u8, CarriedState>>
where
    F: FnMut(usize) -> () + Clone,
{
    let config = StaticConfig::new(file_start, base_font_size, display_scale, dt);

    assert!(file_start as usize % size_of::<usize>() == 0);
    assert!(unsafe { file_start.add(loc) } as usize % size_of::<usize>() == 0);

    let region_start = unsafe { file_start.add(loc) };
    let (root, mut tree) = layout_pass(region_start, file_end, config, frame_state)?;
    tree.compute_layout(
        root,
        taffy::Size {
            width: taffy::prelude::length(width),
            height: taffy::prelude::length(height),
        },
    )?;

    // tree.print_tree(root);

    text_pass(&mut tree, root, font_ctx, layout_ctx, config)?;
    let mut next_frame_state: HashMap<*const u8, CarriedState> = HashMap::new();
    let mut vm_state = VMState::new();
    draw_pass(
        window,
        canvas,
        0.0,
        0.0,
        &mut vm_state,
        &mut tree,
        root,
        cb_push_evt,
        frame_state,
        &mut next_frame_state,
        input_state,
        config,
    )?;

    Ok(next_frame_state)
}
