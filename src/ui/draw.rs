use std::{collections::HashMap, sync::Arc, time::Duration, usize};

use anyhow::{Context, Result, anyhow};
use parley::FontContext;
use skia_safe::{Canvas, Color, HSV, Paint, Path, RGB, Rect};
use strum::{EnumCount, EnumString};
use taffy::{NodeId, PrintTree, TaffyTree, TraversePartialTree};
use winit::window::{CursorIcon, Window};

use super::{
    InputState,
    text::{draw_text, layout_text},
};
/* :---- Book keeping and utils ---- */
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

#[derive(Debug, Clone, Copy)]
struct StaticConfig {
    file_start: *const u8,
    base_font_size: f32,
    display_scale: f32,
}
impl StaticConfig {
    fn new(file_start: *const u8, base_font_size: f32, display_scale: f32) -> Self {
        Self {
            file_start,
            base_font_size,
            display_scale,
        }
    }
}

trait HasStaticConfig {
    fn file_start(&self) -> *const u8;
    fn base_font_size(&self) -> f32;
    fn display_scale(&self) -> f32;
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
}

pub fn read_str_from_array_tagged_word(ptr: usize, file_start: *const u8) -> Result<String> {
    let mut str_cursor = unsafe { file_start.add(ptr) };
    let size = unsafe { TaggedWord::read_in(&mut str_cursor) }
        .read_as_array()
        .with_context(|| format!("Reading string at loc {:x} failed.", ptr))?;

    let str = std::str::from_utf8(unsafe { std::slice::from_raw_parts(str_cursor, size) })?;
    Ok(str.to_owned())
}

trait IntoCompactLength {
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

fn resolve_taffy_length<T>(length: T, extend: f32) -> f32
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
    LibraryCall,  /* 32 word */
    Return,       /* 33 */
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
    FlexRow,
    FlexColumn,
    Grid,
    None, /* hidden */
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
    define_reader!(read_as_library_call, Tag::LibraryCall, usize);
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

/* :::::---- Defines the structure of multi tagged word sequences ie how an instruction demands parameters ----::::: */

pub trait ReadIn: Sized + Copy {
    unsafe fn read_in(cursor: &mut *const u8) -> Self {
        let n = std::mem::size_of::<Self>();
        let ptr = (*cursor) as *const Self;
        *cursor = unsafe { cursor.add(n) };
        unsafe { *ptr }
    }
}
impl ReadIn for TaggedWord {}

trait HasStack {
    fn stack_pop(&mut self) -> Option<TaggedWord>;
    fn stack_push(&mut self, v: TaggedWord) -> ();
}
trait HasRegister {
    fn regs_get(&mut self, k: usize) -> Option<TaggedWord>;
    fn regs_set(&mut self, k: usize, v: TaggedWord) -> ();
}
trait HasCursor {
    unsafe fn read_from_cursor(&mut self) -> Option<TaggedWord>;
    unsafe fn peak_cursor(&self) -> Option<TaggedWord>;
}

trait Executor<S, C, G>
where
    Self: Intepreter,
    S: HasRegister + HasStack,
    C: HasCursor,
    G: HasStaticConfig,
{
    fn get_config(&self) -> G;
    fn get_cursor(&mut self) -> &mut C;
    fn get_vm_state(&mut self) -> &mut S;

    fn maybe_dereference_from_vm_state(&mut self, tagged_word: TaggedWord) -> Result<TaggedWord> {
        let (tag, word) = match &tagged_word.tag {
            Tag::PullArg => {
                let pulled = &self
                    .get_vm_state()
                    .stack_pop()
                    .ok_or(anyhow!("PullArg called with empty stack."))?;
                (pulled.tag, pulled.word)
            }
            Tag::PullArgOr => {
                if let Some(pulled) = &self.get_vm_state().stack_pop() {
                    (pulled.tag, pulled.word)
                } else {
                    /* read the next word, and provide it as the default */
                    let default = unsafe { self.get_cursor().read_from_cursor() }
                        .ok_or(anyhow!("Unexpected EoF"))?;
                    // maybe_cursor = Some(cursor);
                    (default.tag, default.word)
                }
            }
            Tag::FromReg => {
                let pulled = self
                    .get_vm_state()
                    .regs_get(unsafe { tagged_word.word.word })
                    .ok_or(anyhow!(
                        "FromReg called for register id {}, but it is empty",
                        &unsafe { tagged_word.word.word }
                    ))?;
                (pulled.tag, pulled.word)
            }
            Tag::FromRegOr => {
                if let Some(pulled) = self
                    .get_vm_state()
                    .regs_get(unsafe { tagged_word.word.word })
                {
                    (pulled.tag, pulled.word)
                } else {
                    /* read the next word, and provide it as the default */
                    let default = unsafe { self.get_cursor().read_from_cursor() }
                        .ok_or(anyhow!("Unexpected EoF"))?;
                    // maybe_cursor = Some(cursor);
                    (default.tag, default.word)
                }
            }
            _ => (tagged_word.tag, tagged_word.word),
        };
        Ok(TaggedWord { tag, word })
    }

    unsafe fn read_from_cursor_with_arg(&mut self) -> Result<Option<TaggedWord>> {
        if let Some(tagged_word) = unsafe { self.get_cursor().read_from_cursor() } {
            return Ok(Some(self.maybe_dereference_from_vm_state(tagged_word)?));
        }
        Ok(None)
    }

    fn advance(&mut self) -> Result<Option<()>> {
        let maybe_tagged_word = unsafe { self.get_cursor().read_from_cursor() };
        if let Some(tagged_word) = maybe_tagged_word {
            match tagged_word.tag {
                Tag::Enter => self.handle_enter()?,
                Tag::Leave => self.handle_leave()?,
                Tag::Rect => self.read_as_rect()?,
                Tag::BeginPath => self.read_as_begin_path()?,
                Tag::Color => self.read_as_pencil_color()?,
                Tag::Width => self.read_as_width()?,
                Tag::Height => self.read_as_height()?,
                Tag::Padding => self.read_as_padding()?,
                Tag::Margin => self.read_as_margin()?,
                Tag::Display => self.handle_display(tagged_word.read_as_display()?)?,
                Tag::Gap => self.read_as_gap()?,
                Tag::Hover => self.handle_hover(tagged_word.read_as_hover()?)?,
                Tag::MousePressed => {
                    self.handle_mouse_pressed(tagged_word.read_as_mouse_pressed()?)?
                }
                Tag::Clicked => self.handle_clicked(tagged_word.read_as_clicked()?)?,
                Tag::OpenLatch => self.handle_open_latch(tagged_word.read_as_open_latch()?)?,
                Tag::ClosedLatch => {
                    self.handle_closed_latch(tagged_word.read_as_closed_latch()?)?
                }
                Tag::LibraryCall => {
                    self.handle_library_call(tagged_word.read_as_library_call()?)?
                }
                Tag::Return => self.handle_return()?,
                Tag::PushArg => self.blanket_handle_push_arg()?,
                Tag::LoadReg => {
                    self.blanket_handle_set_reg(tagged_word.read_as_load_register()?)?
                }
                Tag::Event => self.handle_event(tagged_word.read_as_event()?)?,
                Tag::Text => self.read_as_text()?,
                Tag::TextPtr => todo!(),
                Tag::FontSize => self.handle_font_size(tagged_word.read_as_font_size()?)?,
                Tag::FontAlignment => {
                    self.handle_font_alignment(tagged_word.read_as_font_alignment()?)?
                }
                Tag::FontFamily => self.read_as_font_family()?,
                Tag::CursorDefault => self.handle_cursor(tagged_word.read_as_any_cursor()?)?,
                Tag::CursorPointer => self.handle_cursor(tagged_word.read_as_any_cursor()?)?,
                _ => {
                    return Err(anyhow!(
                        "Found Tag `{:?}` in illegal position",
                        tagged_word.tag
                    ));
                }
            }
        } else {
            return Ok(None);
        };
        Ok(Some(()))
    }

    fn read_as_width(&mut self) -> Result<()> {
        let width = unsafe { self.read_from_cursor_with_arg() }?
            .ok_or(anyhow!("Early EOF"))?
            .read_as_taffy_length_pctauto(self.get_config().base_font_size())?;
        self.handle_width(width)?;
        Ok(())
    }

    fn read_as_height(&mut self) -> Result<()> {
        let height = unsafe { self.read_from_cursor_with_arg() }?
            .ok_or(anyhow!("Early EOF"))?
            .read_as_taffy_length_pctauto(self.get_config().base_font_size())?;
        self.handle_height(height)?;
        Ok(())
    }

    fn read_as_margin(&mut self) -> Result<()> {
        let left = unsafe { self.read_from_cursor_with_arg() }?
            .ok_or(anyhow!("Early EOF"))?
            .read_as_taffy_length_pctauto(self.get_config().base_font_size())?;
        let top = unsafe { self.read_from_cursor_with_arg() }?
            .ok_or(anyhow!("Early EOF"))?
            .read_as_taffy_length_pctauto(self.get_config().base_font_size())?;
        let right = unsafe { self.read_from_cursor_with_arg() }?
            .ok_or(anyhow!("Early EOF"))?
            .read_as_taffy_length_pctauto(self.get_config().base_font_size())?;
        let bottom = unsafe { self.read_from_cursor_with_arg() }?
            .ok_or(anyhow!("Early EOF"))?
            .read_as_taffy_length_pctauto(self.get_config().base_font_size())?;
        self.handle_margin(left, top, right, bottom)?;
        Ok(())
    }

    fn read_as_padding(&mut self) -> Result<()> {
        let left = unsafe { self.read_from_cursor_with_arg() }?
            .ok_or(anyhow!("Early EOF"))?
            .read_as_taffy_length_pct(self.get_config().base_font_size())?;
        let top = unsafe { self.read_from_cursor_with_arg() }?
            .ok_or(anyhow!("Early EOF"))?
            .read_as_taffy_length_pct(self.get_config().base_font_size())?;
        let right = unsafe { self.read_from_cursor_with_arg() }?
            .ok_or(anyhow!("Early EOF"))?
            .read_as_taffy_length_pct(self.get_config().base_font_size())?;
        let bottom = unsafe { self.read_from_cursor_with_arg() }?
            .ok_or(anyhow!("Early EOF"))?
            .read_as_taffy_length_pct(self.get_config().base_font_size())?;
        self.handle_padding(left, top, right, bottom)?;
        Ok(())
    }

    fn read_as_gap(&mut self) -> Result<()> {
        let width = unsafe { self.read_from_cursor_with_arg() }?
            .ok_or(anyhow!("Early EOF"))?
            .read_as_taffy_length_pct(self.get_config().base_font_size())?;
        let height = unsafe { self.read_from_cursor_with_arg() }?
            .ok_or(anyhow!("Early EOF"))?
            .read_as_taffy_length_pct(self.get_config().base_font_size())?;
        self.handle_gap(width, height)?;
        Ok(())
    }

    fn read_as_text(&mut self) -> Result<()> {
        let x = unsafe { self.read_from_cursor_with_arg() }?
            .ok_or(anyhow!("Early EOF"))?
            .read_as_taffy_length_pct(self.get_config().base_font_size())?;
        let y = unsafe { self.read_from_cursor_with_arg() }?
            .ok_or(anyhow!("Early EOF"))?
            .read_as_taffy_length_pct(self.get_config().base_font_size())?;

        let ptr = unsafe { self.read_from_cursor_with_arg() }?
            .ok_or(anyhow!("Early EOF"))?
            .read_as_text_ptr()?;
        let txt = read_str_from_array_tagged_word(ptr, self.get_config().file_start())?;
        self.handle_text(x, y, &txt)?;
        Ok(())
    }

    fn read_as_font_family(&mut self) -> Result<()> {
        let ptr = unsafe { self.read_from_cursor_with_arg() }?
            .ok_or(anyhow!("Early EOF"))?
            .read_as_text_ptr()?;
        let txt = read_str_from_array_tagged_word(ptr, self.get_config().file_start())?;
        self.handle_font_family(&txt)?;
        Ok(())
    }

    fn read_as_rect(&mut self) -> Result<()> {
        let x = unsafe { self.read_from_cursor_with_arg() }?
            .ok_or(anyhow!("Early EOF"))?
            .read_as_taffy_length_pct(self.get_config().base_font_size())?;
        let y = unsafe { self.read_from_cursor_with_arg() }?
            .ok_or(anyhow!("Early EOF"))?
            .read_as_taffy_length_pct(self.get_config().base_font_size())?;

        let w = unsafe { self.read_from_cursor_with_arg() }?
            .ok_or(anyhow!("Early EOF"))?
            .read_as_taffy_length_pctauto(self.get_config().base_font_size())?;
        let h = unsafe { self.read_from_cursor_with_arg() }?
            .ok_or(anyhow!("Early EOF"))?
            .read_as_taffy_length_pctauto(self.get_config().base_font_size())?;
        self.handle_rect(x, y, w, h)?;
        Ok(())
    }

    fn read_as_pencil_color(&mut self) -> Result<()> {
        let color = unsafe { self.read_from_cursor_with_arg() }?
            .ok_or(anyhow!("Early EOF"))?
            .read_as_any_color()?;
        self.handle_pencil_color(color)?;
        Ok(())
    }

    fn read_as_begin_path(&mut self) -> Result<()> {
        self.handle_begin_path()?;
        while let Some(tagged_word) = unsafe { self.get_cursor().read_from_cursor() } {
            match tagged_word.tag {
                Tag::BeginPath => return Err(anyhow!("Nested paths are forbidden.")),
                Tag::EndPath => break,
                Tag::MoveTo => {
                    let x = unsafe { self.read_from_cursor_with_arg() }?
                        .ok_or(anyhow!("Early EOF"))?
                        .read_as_taffy_length_pct(self.get_config().base_font_size())?;
                    let y = unsafe { self.read_from_cursor_with_arg() }?
                        .ok_or(anyhow!("Early EOF"))?
                        .read_as_taffy_length_pct(self.get_config().base_font_size())?;
                    self.handle_move_to(x, y)?;
                }
                Tag::LineTo => {
                    let x = unsafe { self.read_from_cursor_with_arg() }?
                        .ok_or(anyhow!("Early EOF"))?
                        .read_as_taffy_length_pct(self.get_config().base_font_size())?;
                    let y = unsafe { self.read_from_cursor_with_arg() }?
                        .ok_or(anyhow!("Early EOF"))?
                        .read_as_taffy_length_pct(self.get_config().base_font_size())?;
                    self.handle_line_to(x, y)?;
                }
                Tag::QuadTo => {
                    let cx = unsafe { self.read_from_cursor_with_arg() }?
                        .ok_or(anyhow!("Early EOF"))?
                        .read_as_taffy_length_pct(self.get_config().base_font_size())?;
                    let cy = unsafe { self.read_from_cursor_with_arg() }?
                        .ok_or(anyhow!("Early EOF"))?
                        .read_as_taffy_length_pct(self.get_config().base_font_size())?;
                    let x = unsafe { self.read_from_cursor_with_arg() }?
                        .ok_or(anyhow!("Early EOF"))?
                        .read_as_taffy_length_pct(self.get_config().base_font_size())?;
                    let y = unsafe { self.read_from_cursor_with_arg() }?
                        .ok_or(anyhow!("Early EOF"))?
                        .read_as_taffy_length_pct(self.get_config().base_font_size())?;
                    self.handle_quad_to(cx, cy, x, y)?;
                }
                Tag::CubicTo => {
                    let cx1 = unsafe { self.read_from_cursor_with_arg() }?
                        .ok_or(anyhow!("Early EOF"))?
                        .read_as_taffy_length_pct(self.get_config().base_font_size())?;
                    let cy1 = unsafe { self.read_from_cursor_with_arg() }?
                        .ok_or(anyhow!("Early EOF"))?
                        .read_as_taffy_length_pct(self.get_config().base_font_size())?;
                    let cx2 = unsafe { self.read_from_cursor_with_arg() }?
                        .ok_or(anyhow!("Early EOF"))?
                        .read_as_taffy_length_pct(self.get_config().base_font_size())?;
                    let cy2 = unsafe { self.read_from_cursor_with_arg() }?
                        .ok_or(anyhow!("Early EOF"))?
                        .read_as_taffy_length_pct(self.get_config().base_font_size())?;
                    let x = unsafe { self.read_from_cursor_with_arg() }?
                        .ok_or(anyhow!("Early EOF"))?
                        .read_as_taffy_length_pct(self.get_config().base_font_size())?;
                    let y = unsafe { self.read_from_cursor_with_arg() }?
                        .ok_or(anyhow!("Early EOF"))?
                        .read_as_taffy_length_pct(self.get_config().base_font_size())?;
                    self.handle_cubic_to(cx1, cy1, cx2, cy2, x, y)?;
                }
                Tag::ArcTo => {
                    let tx = unsafe { self.read_from_cursor_with_arg() }?
                        .ok_or(anyhow!("Early EOF"))?
                        .read_as_taffy_length_pct(self.get_config().base_font_size())?;
                    let ty = unsafe { self.read_from_cursor_with_arg() }?
                        .ok_or(anyhow!("Early EOF"))?
                        .read_as_taffy_length_pct(self.get_config().base_font_size())?;
                    let x = unsafe { self.read_from_cursor_with_arg() }?
                        .ok_or(anyhow!("Early EOF"))?
                        .read_as_taffy_length_pct(self.get_config().base_font_size())?;
                    let y = unsafe { self.read_from_cursor_with_arg() }?
                        .ok_or(anyhow!("Early EOF"))?
                        .read_as_taffy_length_pct(self.get_config().base_font_size())?;
                    let r = unsafe { self.read_from_cursor_with_arg() }?
                        .ok_or(anyhow!("Early EOF"))?
                        .read_as_taffy_length_pct(self.get_config().base_font_size())?;
                    self.handle_arc_to(tx, ty, x, y, r)?;
                }
                Tag::ClosePath => self.handle_close_path()?,
                _ => {
                    return Err(anyhow!(
                        "Expected only tags of Path family after `BeginPath`"
                    ));
                }
            }
        }

        if unsafe { self.get_cursor().peak_cursor().map(|x| x.tag) } != Some(Tag::EndPath) {
            return Err(anyhow!(
                "A path was opened with `BeginPath` but was never closed with `EndPath`"
            ));
        }
        self.handle_end_path()?;
        Ok(())
    }

    fn blanket_handle_push_arg(&mut self) -> Result<()> {
        let tagged_word =
            unsafe { self.get_cursor().read_from_cursor() }.ok_or(anyhow!("Unexpected EOF"))?;
        let tagged_word = self.maybe_dereference_from_vm_state(tagged_word)?;
        self.get_vm_state().stack_push(tagged_word);
        Ok(())
    }

    fn blanket_handle_set_reg(&mut self, id: usize) -> Result<()> {
        let tagged_word =
            unsafe { self.get_cursor().read_from_cursor() }.ok_or(anyhow!("Unexpected EOF"))?;
        let tagged_word = self.maybe_dereference_from_vm_state(tagged_word)?;
        self.get_vm_state().regs_set(id, tagged_word);
        Ok(())
    }
}

trait Intepreter {
    fn handle_enter(&mut self) -> Result<()> {
        Ok(())
    }
    fn handle_leave(&mut self) -> Result<()> {
        Ok(())
    }
    fn handle_library_call(&mut self, _id: usize) -> Result<()> {
        Ok(())
    }
    fn handle_return(&mut self) -> Result<()> {
        Ok(())
    }
    fn handle_width(&mut self, _x: taffy::LengthPercentageAuto) -> Result<()> {
        Ok(())
    }
    fn handle_height(&mut self, _y: taffy::LengthPercentageAuto) -> Result<()> {
        Ok(())
    }
    fn handle_margin(
        &mut self,
        _left: taffy::LengthPercentageAuto,
        _top: taffy::LengthPercentageAuto,
        _right: taffy::LengthPercentageAuto,
        _bottom: taffy::LengthPercentageAuto,
    ) -> Result<()> {
        Ok(())
    }
    fn handle_padding(
        &mut self,
        _left: taffy::LengthPercentage,
        _top: taffy::LengthPercentage,
        _right: taffy::LengthPercentage,
        _bottom: taffy::LengthPercentage,
    ) -> Result<()> {
        Ok(())
    }
    fn handle_display(&mut self, _display: DisplayOption) -> Result<()> {
        Ok(())
    }
    fn handle_gap(
        &mut self,
        _width: taffy::LengthPercentage,
        _height: taffy::LengthPercentage,
    ) -> Result<()> {
        Ok(())
    }
    fn handle_hover(&mut self, _rel_ptr: usize) -> Result<()> {
        Ok(())
    }
    fn handle_mouse_pressed(&mut self, _rel_ptr: usize) -> Result<()> {
        Ok(())
    }
    fn handle_clicked(&mut self, _rel_ptr: usize) -> Result<()> {
        Ok(())
    }
    fn handle_open_latch(&mut self, _rel_ptr: usize) -> Result<()> {
        Ok(())
    }
    fn handle_closed_latch(&mut self, _rel_ptr: usize) -> Result<()> {
        Ok(())
    }
    fn handle_text(
        &mut self,
        _x: taffy::LengthPercentage,
        _y: taffy::LengthPercentage,
        _txt: &str,
    ) -> Result<()> {
        Ok(())
    }
    fn handle_font_alignment(&mut self, _alignment: StoredAlignment) -> Result<()> {
        Ok(())
    }
    fn handle_font_family(&mut self, _font_desc: &str) -> Result<()> {
        Ok(())
    }
    fn handle_font_size(&mut self, _size: f32) -> Result<()> {
        Ok(())
    }

    fn handle_rect(
        &mut self,
        _x: taffy::LengthPercentage,
        _y: taffy::LengthPercentage,
        _w: taffy::LengthPercentageAuto,
        _h: taffy::LengthPercentageAuto,
    ) -> Result<()> {
        Ok(())
    }

    fn handle_pencil_color(&mut self, _color: Color) -> Result<()> {
        Ok(())
    }

    fn handle_cursor(&mut self, _cursor: CursorIcon) -> Result<()> {
        Ok(())
    }

    fn handle_event(&mut self, _id: usize) -> Result<()> {
        Ok(())
    }

    fn handle_begin_path(&mut self) -> Result<()> {
        Ok(())
    }

    fn handle_move_to(
        &mut self,
        _x: taffy::LengthPercentage,
        _y: taffy::LengthPercentage,
    ) -> Result<()> {
        Ok(())
    }

    fn handle_line_to(
        &mut self,
        _x: taffy::LengthPercentage,
        _y: taffy::LengthPercentage,
    ) -> Result<()> {
        Ok(())
    }

    fn handle_quad_to(
        &mut self,
        _cx: taffy::LengthPercentage,
        _cy: taffy::LengthPercentage,
        _x: taffy::LengthPercentage,
        _y: taffy::LengthPercentage,
    ) -> Result<()> {
        Ok(())
    }

    fn handle_cubic_to(
        &mut self,
        _cx1: taffy::LengthPercentage,
        _cy1: taffy::LengthPercentage,
        _cx2: taffy::LengthPercentage,
        _cy2: taffy::LengthPercentage,
        _x: taffy::LengthPercentage,
        _y: taffy::LengthPercentage,
    ) -> Result<()> {
        Ok(())
    }

    fn handle_arc_to(
        &mut self,
        _tx: taffy::LengthPercentage,
        _ty: taffy::LengthPercentage,
        _x: taffy::LengthPercentage,
        _y: taffy::LengthPercentage,
        _r: taffy::LengthPercentage,
    ) -> Result<()> {
        Ok(())
    }

    fn handle_close_path(&mut self) -> Result<()> {
        Ok(())
    }

    fn handle_end_path(&mut self) -> Result<()> {
        Ok(())
    }
}
// Now anything that implements HasStack + HasRegister + HasCursor + HasStaticConfig + Intepreter can implement Executor
// and have the method in intepreter correctly called with the inputs read according to the vm definition, with cursor allowing
// flexibility on how the memory is laid out (since we have to handle our ragged members).

// :::::::------ The Cursors that we need (defining how the memory is laid out) ----:::::

struct LinearCursor {
    region_start: *const u8,
    region_end: *const u8,

    cursor: *const u8,
    last_read: Option<TaggedWord>,
    lib_jmp_depth: i32,
    element_depth: i32,
}
impl LinearCursor {
    fn new(region_start: *const u8, region_end: *const u8) -> Self {
        Self {
            region_start,
            region_end,
            cursor: region_start,
            last_read: None,
            lib_jmp_depth: 0,
            element_depth: 0,
        }
    }
}
impl LinearCursor {
    fn jmp_lib(&mut self, dest: *const u8) {
        self.cursor = dest;
        self.lib_jmp_depth += 1;
    }
    fn ret_lib(&mut self, ret: *const u8) {
        self.cursor = ret;
        self.lib_jmp_depth -= 1;
    }
    fn add_depth(&mut self) {
        self.element_depth += 1;
    }
    fn sub_depth(&mut self) {
        self.element_depth -= 1;
    }
}
impl HasCursor for LinearCursor {
    unsafe fn read_from_cursor(&mut self) -> Option<TaggedWord> {
        if self.lib_jmp_depth > 0
            || (self.cursor >= self.region_start && self.cursor < self.region_end)
        {
            self.last_read = Some(unsafe { TaggedWord::read_in(&mut self.cursor) });
            if self.last_read.unwrap().tag != Tag::Enter {
                if self.element_depth <= 0 {
                    return None;
                }
            }

            self.last_read
        } else {
            None
        }
    }

    unsafe fn peak_cursor(&self) -> Option<TaggedWord> {
        self.last_read
    }
}

struct RaggedCursor {
    regions: Vec<(*const u8, *const u8)>,

    cursor: *const u8,
    region_i: usize,
    last_read: Option<TaggedWord>,
}
impl RaggedCursor {
    fn new(regions: Vec<(*const u8, *const u8)>) -> Result<Self> {
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

// ::: ---- Basic VM State Implementation --- ::
struct VMState {
    regs: HashMap<usize, TaggedWord>,
    stack: Vec<TaggedWord>,
}
impl VMState {
    fn new() -> Self {
        VMState {
            regs: HashMap::new(),
            stack: Vec::new(),
        }
    }
}
impl HasRegister for VMState {
    fn regs_get(&mut self, k: usize) -> Option<TaggedWord> {
        self.regs.get(&k).cloned()
    }

    fn regs_set(&mut self, k: usize, v: TaggedWord) -> () {
        self.regs.insert(k, v);
    }
}
impl HasStack for VMState {
    fn stack_pop(&mut self) -> Option<TaggedWord> {
        self.stack.pop()
    }

    fn stack_push(&mut self, v: TaggedWord) -> () {
        self.stack.push(v);
    }
}

// ::: ---- Rendering Code --- :::
// Rendering is done in three passes
// 1) Construct the layout tree
// 2) Layout text now that bounds are known
// 3) Draw everything

// ::: ---- First Pass, Construct Layout Tree ----:::
#[derive(Clone, Default)]
struct LayoutContext {
    ragged_members: Vec<(*const u8, *const u8)>,
    maybe_font_layout: Option<parley::Layout<()>>,
}

struct LayoutIntepreter<'a> {
    config: StaticConfig,
    state: VMState,
    cursor: LinearCursor,

    library: &'a HashMap<usize, Vec<u8>>,
    last_frame_state: &'a HashMap<*const u8, CarriedState>,

    tree: TaffyTree<LayoutContext>,
    node_stack: Vec<NodeId>,
    cur_start_ptr: *const u8,
    call_stack: Vec<*const u8>,
    root: Option<NodeId>,
}
impl<'a> LayoutIntepreter<'a> {
    fn new(
        region_start: *const u8,
        region_end: *const u8,
        config: StaticConfig,
        library: &'a HashMap<usize, Vec<u8>>,
        last_frame_state: &'a HashMap<*const u8, CarriedState>,
    ) -> Self {
        Self {
            config,
            state: VMState::new(),
            cursor: LinearCursor::new(region_start, region_end),
            tree: TaffyTree::new(),
            node_stack: Vec::new(),
            cur_start_ptr: region_start,
            call_stack: Vec::new(),
            library,
            last_frame_state,
            root: None,
        }
    }
}

impl<'a> Executor<VMState, LinearCursor, StaticConfig> for LayoutIntepreter<'a> {
    fn get_config(&self) -> StaticConfig {
        self.config
    }

    fn get_cursor(&mut self) -> &mut LinearCursor {
        &mut self.cursor
    }

    fn get_vm_state(&mut self) -> &mut VMState {
        &mut self.state
    }
}

impl<'a> Intepreter for LayoutIntepreter<'a> {
    fn handle_enter(&mut self) -> Result<()> {
        let cur_node = if let Some(n) = self.node_stack.last() {
            *n
        } else {
            let n = self
                .tree
                .new_leaf_with_context(taffy::Style::default(), LayoutContext::default())?;
            self.node_stack.push(n);
            n
        };

        self.cursor.add_depth();

        if self.root.is_some() {
            // otherwise this is the root
            let mut ctx: LayoutContext = self
                .tree
                .get_node_context(cur_node)
                .cloned()
                .unwrap_or_default(); /* TODO: eliminate copy here */
            ctx.ragged_members
                .push((self.cur_start_ptr, self.cursor.cursor));
            self.tree.set_node_context(cur_node, Some(ctx))?;
        } else {
            self.root = Some(cur_node);
        }

        Ok(())
    }

    fn handle_leave(&mut self) -> Result<()> {
        let cur_node = self
            .node_stack
            .pop()
            .ok_or(anyhow!("At-least one `Leave` too many"))?;
        let parent = self.node_stack.last();

        // Push a new node range
        let mut ctx: LayoutContext = self
            .tree
            .get_node_context(cur_node)
            .cloned()
            .unwrap_or_default(); /* TODO: eliminate copy here */
        ctx.ragged_members
            .push((self.cur_start_ptr, self.cursor.cursor));
        self.tree.set_node_context(cur_node, Some(ctx))?;

        self.cursor.sub_depth();

        // Update connectivness
        if let Some(parent) = parent {
            /* root node doesn't have a parent. */
            self.tree.add_child(*parent, cur_node)?;
        }
        Ok(())
    }

    fn handle_library_call(&mut self, id: usize) -> Result<()> {
        let code_ptr = self
            .library
            .get(&id)
            .ok_or(anyhow!("Requested library element {} not found.", id))?
            .as_ptr();
        self.call_stack.push(self.cursor.cursor);
        self.handle_enter()?; /* lib_call is implicit node enter */
        self.cursor.jmp_lib(code_ptr);
        self.cur_start_ptr = self.cursor.cursor;
        Ok(())
    }

    fn handle_return(&mut self) -> Result<()> {
        self.handle_leave()?;
        let ret_ptr = self.call_stack.pop().ok_or(anyhow!(
            "`InlineReturn` tag called without being in library code."
        ))?;
        self.cursor.ret_lib(ret_ptr);
        self.cur_start_ptr = self.cursor.cursor;
        Ok(())
    }

    fn handle_width(&mut self, x: taffy::LengthPercentageAuto) -> Result<()> {
        let cur_node = self.node_stack.last().unwrap();
        let mut cur_style = self.tree.style(*cur_node)?.clone();
        cur_style.size.width = taffy::Dimension::from(x);
        self.tree.set_style(*cur_node, cur_style)?;
        Ok(())
    }

    fn handle_height(&mut self, y: taffy::LengthPercentageAuto) -> Result<()> {
        let cur_node = self.node_stack.last().unwrap();
        let mut cur_style = self.tree.style(*cur_node)?.clone();
        cur_style.size.height = taffy::Dimension::from(y);
        self.tree.set_style(*cur_node, cur_style)?;
        Ok(())
    }

    fn handle_margin(
        &mut self,
        left: taffy::LengthPercentageAuto,
        top: taffy::LengthPercentageAuto,
        right: taffy::LengthPercentageAuto,
        bottom: taffy::LengthPercentageAuto,
    ) -> Result<()> {
        let cur_node = self.node_stack.last().unwrap();
        let mut cur_style = self.tree.style(*cur_node)?.clone();
        cur_style.margin = taffy::Rect {
            left,
            right,
            top,
            bottom,
        };
        self.tree.set_style(*cur_node, cur_style)?;
        Ok(())
    }

    fn handle_padding(
        &mut self,
        left: taffy::LengthPercentage,
        top: taffy::LengthPercentage,
        right: taffy::LengthPercentage,
        bottom: taffy::LengthPercentage,
    ) -> Result<()> {
        let cur_node = self.node_stack.last().unwrap();
        let mut cur_style = self.tree.style(*cur_node)?.clone();
        cur_style.padding = taffy::Rect {
            left,
            right,
            top,
            bottom,
        };
        self.tree.set_style(*cur_node, cur_style)?;
        Ok(())
    }

    fn handle_display(&mut self, display: DisplayOption) -> Result<()> {
        let cur_node = self.node_stack.last().unwrap();
        let mut cur_style = self.tree.style(*cur_node)?.clone();
        match display {
            DisplayOption::Block => cur_style.display = taffy::Display::Block,
            DisplayOption::FlexRow => cur_style.display = taffy::Display::Flex,
            DisplayOption::FlexColumn => cur_style.display = taffy::Display::Flex,
            DisplayOption::Grid => cur_style.display = taffy::Display::Grid,
            DisplayOption::None => cur_style.display = taffy::Display::None,
        }
        match display {
            DisplayOption::FlexRow => cur_style.flex_direction = taffy::FlexDirection::Row,
            DisplayOption::FlexColumn => cur_style.flex_direction = taffy::FlexDirection::Column,
            _ => (),
        }
        self.tree.set_style(*cur_node, cur_style)?;
        Ok(())
    }

    fn handle_gap(
        &mut self,
        width: taffy::LengthPercentage,
        height: taffy::LengthPercentage,
    ) -> Result<()> {
        let cur_node = self.node_stack.last().unwrap();
        let mut cur_style = self.tree.style(*cur_node)?.clone();
        cur_style.gap = taffy::Size { width, height };
        self.tree.set_style(*cur_node, cur_style)?;
        Ok(())
    }

    fn handle_hover(&mut self, rel_ptr: usize) -> Result<()> {
        if !self
            .last_frame_state
            .get(&self.cursor.cursor)
            .map(|x| &x.is_jmp)
            .unwrap_or(&false)
        {
            self.cursor.cursor = unsafe { self.cursor.cursor.add(rel_ptr) };
        }
        Ok(())
    }

    fn handle_mouse_pressed(&mut self, rel_ptr: usize) -> Result<()> {
        if !self
            .last_frame_state
            .get(&self.cursor.cursor)
            .map(|x| &x.is_jmp)
            .unwrap_or(&false)
        {
            self.cursor.cursor = unsafe { self.cursor.cursor.add(rel_ptr) };
        }
        Ok(())
    }

    fn handle_clicked(&mut self, rel_ptr: usize) -> Result<()> {
        if !self
            .last_frame_state
            .get(&self.cursor.cursor)
            .map(|x| &x.is_jmp)
            .unwrap_or(&false)
        {
            self.cursor.cursor = unsafe { self.cursor.cursor.add(rel_ptr) };
        }
        Ok(())
    }

    fn handle_open_latch(&mut self, _rel_ptr: usize) -> Result<()> {
        Ok(())
    }

    fn handle_closed_latch(&mut self, rel_ptr: usize) -> Result<()> {
        self.cursor.cursor = unsafe { self.cursor.cursor.add(rel_ptr) }; /* the closed latch always jumps */
        Ok(())
    }
}

fn layout_pass(
    region_start: *const u8,
    region_end: *const u8,
    config: StaticConfig,
    library: &HashMap<usize, Vec<u8>>,
    last_frame_state: &HashMap<*const u8, CarriedState>,
) -> Result<(NodeId, TaffyTree<LayoutContext>)> {
    let mut intepreter =
        LayoutIntepreter::new(region_start, region_end, config, library, last_frame_state);
    while let Some(_) = intepreter.advance()? {}
    let root = intepreter.root.ok_or(anyhow!("No root in layout."))?;
    Ok((root, intepreter.tree))
}

// ::: ---- Second Pass, Layout Text ----:::

struct TextLayoutIntepreter<'a> {
    config: StaticConfig,
    state: VMState,
    cursor: RaggedCursor,

    font_context: &'a mut FontContext,
    layout_context: &'a mut parley::LayoutContext<()>,

    font_alignment: parley::Alignment,
    font_family: String,
    font_size: f32,

    tree: &'a mut TaffyTree<LayoutContext>,
    node: NodeId,
}

impl<'a> TextLayoutIntepreter<'a> {
    fn new(
        tree: &'a mut TaffyTree<LayoutContext>,
        node: NodeId,
        regions: Vec<(*const u8, *const u8)>,
        font_context: &'a mut FontContext,
        layout_context: &'a mut parley::LayoutContext<()>,
        config: StaticConfig,
    ) -> Result<Self> {
        Ok(Self {
            config,
            state: VMState::new(),
            cursor: RaggedCursor::new(regions)?,

            font_context,
            layout_context,

            font_alignment: parley::Alignment::Start,
            font_family: String::from("Arial"),
            font_size: config.base_font_size(),

            tree,
            node,
        })
    }
}

impl<'a> Executor<VMState, RaggedCursor, StaticConfig> for TextLayoutIntepreter<'a> {
    fn get_config(&self) -> StaticConfig {
        self.config
    }

    fn get_cursor(&mut self) -> &mut RaggedCursor {
        &mut self.cursor
    }

    fn get_vm_state(&mut self) -> &mut VMState {
        &mut self.state
    }
}

impl<'a> Intepreter for TextLayoutIntepreter<'a> {
    fn handle_text(
        &mut self,
        _x: taffy::LengthPercentage,
        _y: taffy::LengthPercentage,
        txt: &str,
    ) -> Result<()> {
        let layout = layout_text(
            &txt,
            self.tree.get_final_layout(self.node).size.width, /* TODO: why is this print tree */
            self.font_alignment,
            self.font_context,
            self.layout_context,
            &self.font_family,
            self.font_size,
            self.config.display_scale(),
        );

        self.tree
            .get_node_context_mut(self.node)
            .ok_or(anyhow!("All nodes must have context"))?
            .maybe_font_layout = Some(layout.clone());
        let mut style = self.tree.style(self.node)?.clone();
        style.size = taffy::Size {
            width: taffy::prelude::length(layout.width()),
            height: taffy::prelude::length(layout.height()),
        };
        self.tree.set_style(self.node, style)?;
        Ok(())
    }

    fn handle_font_alignment(&mut self, alignment: StoredAlignment) -> Result<()> {
        self.font_alignment = match alignment {
            /* we need the separate stored alignment to make sure it is usize */
            StoredAlignment::Start => parley::Alignment::Start,
            StoredAlignment::End => parley::Alignment::End,
            StoredAlignment::Left => parley::Alignment::Left,
            StoredAlignment::Middle => parley::Alignment::Middle,
            StoredAlignment::Right => parley::Alignment::Right,
            StoredAlignment::Justified => parley::Alignment::Justified,
        };
        Ok(())
    }

    fn handle_font_family(&mut self, font_desc: &str) -> Result<()> {
        self.font_family = String::from(font_desc);
        Ok(())
    }

    fn handle_font_size(&mut self, size: f32) -> Result<()> {
        self.font_size = size;
        Ok(())
    }
}
fn text_pass(
    tree: &mut TaffyTree<LayoutContext>,
    node: NodeId,
    font_context: &mut FontContext,
    layout_context: &mut parley::LayoutContext<()>,
    config: StaticConfig,
) -> Result<()> {
    let ctx = tree
        .get_node_context(node)
        .ok_or(anyhow!("Each node in the taffy tree must have a context"))?;
    let regions = ctx.ragged_members.clone();
    let mut intepreter =
        TextLayoutIntepreter::new(tree, node, regions, font_context, layout_context, config)?;
    while let Some(_) = intepreter.advance()? {}
    let children: Vec<_> = tree.child_ids(node).collect();
    for child in children {
        text_pass(tree, child, font_context, layout_context, config)?;
    }
    Ok(())
}

// :::::::-------- Third Pass, Draw ------ :::::
struct DrawIntepreter<'a, F>
where
    F: FnMut(usize) -> () + Clone,
{
    config: StaticConfig,
    state: VMState,
    cursor: RaggedCursor,

    font_family: String,
    font_size: f32,

    paint: Paint,
    canvas: &'a Canvas,
    window: Arc<Window>,
    is_hovered: bool,

    x: f32,
    y: f32,
    width: f32,
    #[allow(dead_code)]
    height: f32,

    cb_push_evt: F,

    input_state: InputState,
    frame_state: &'a HashMap<*const u8, CarriedState>,
    next_frame_state: &'a mut HashMap<*const u8, CarriedState>,

    tree: &'a TaffyTree<LayoutContext>,
    node: NodeId,

    maybe_active_path: Option<Path>,
}

impl<'a, F> DrawIntepreter<'a, F>
where
    F: FnMut(usize) -> () + Clone,
{
    fn new(
        window: Arc<Window>,
        canvas: &'a Canvas,
        x: f32,
        y: f32,
        tree: &'a TaffyTree<LayoutContext>,
        node: NodeId,
        cb_push_evt: F,
        regions: Vec<(*const u8, *const u8)>,
        frame_state: &'a HashMap<*const u8, CarriedState>,
        next_frame_state: &'a mut HashMap<*const u8, CarriedState>,
        input_state: &InputState,
        config: StaticConfig,
    ) -> Result<Self> {
        let mut paint = Paint::default();
        paint.set_anti_alias(true);

        let layout = tree.get_final_layout(node);

        let is_hovered = input_state.cursor_pos.x < (x + layout.size.width) as f64
            && input_state.cursor_pos.x > x as f64
            && input_state.cursor_pos.y < (y + layout.size.height) as f64
            && input_state.cursor_pos.y > y as f64;

        Ok(Self {
            window,
            paint,
            x,
            y,
            cb_push_evt,
            width: layout.size.width,
            height: layout.size.height,
            config,
            is_hovered,
            state: VMState::new(),
            cursor: RaggedCursor::new(regions)?,
            canvas,
            frame_state,
            next_frame_state,
            input_state: input_state.clone(),

            font_family: String::from("Arial"),
            font_size: config.base_font_size(),

            tree,
            node,
            maybe_active_path: None,
        })
    }
}

impl<'a, F> Executor<VMState, RaggedCursor, StaticConfig> for DrawIntepreter<'a, F>
where
    F: FnMut(usize) -> () + Clone,
{
    fn get_config(&self) -> StaticConfig {
        self.config
    }

    fn get_cursor(&mut self) -> &mut RaggedCursor {
        &mut self.cursor
    }

    fn get_vm_state(&mut self) -> &mut VMState {
        &mut self.state
    }
}

impl<'a, F> Intepreter for DrawIntepreter<'a, F>
where
    F: FnMut(usize) -> () + Clone,
{
    fn handle_enter(&mut self) -> Result<()> {
        // if tag.read_as_enter(None).is_ok() {
        //             /* in enter compute scroll state. */
        //             let desired_height = layout.size.height.max(
        //                 ctx.maybe_font_layout
        //                     .as_ref()
        //                     .map(|x| x.height())
        //                     .unwrap_or(0.0),
        //             );

        //             if desired_height > window_height {
        //                 let scroll_y = last_frame_jmps
        //                     .get(&cursor)
        //                     .map(|x| x.scroll_amount)
        //                     .unwrap_or(0.0);

        //                 // let scroll_y = scroll_y.clamp(-(desired_height - window_height), 0.0);
        //                 // let scroll_y =
        //                 //     soft_clamp(scroll_y, -(desired_height - window_height), 0.0, 0.99);

        //                 let scroll_y = scroll_decay(
        //                     scroll_y,
        //                     dt,
        //                     -(desired_height - window_height),
        //                     0.0,
        //                     5000.78,
        //                 );

        //                 /* looks like we're scolling waaa */
        //                 canvas.translate((0.0, scroll_y));

        //                 if is_hovered {
        //                     let mut state = last_frame_jmps
        //                         .get(&cursor)
        //                         .cloned()
        //                         .unwrap_or(CarriedState::new());
        //                     // let next_scroll_y = state.scroll_amount + input_state.scroll_action.1;
        //                     // let next_scroll_y =
        //                     //     next_scroll_y.clamp(-(desired_height - window_height), 0.0);

        //                     let next_scroll_y = if input_state.scroll_action.1.abs() < 0.5 {
        //                         scroll_decay(
        //                             state.scroll_amount,
        //                             dt,
        //                             -(desired_height - window_height),
        //                             0.0,
        //                             50.78,
        //                         )
        //                     } else {
        //                         state.scroll_amount + input_state.scroll_action.1
        //                     };

        //                     state.scroll_amount = next_scroll_y;

        //                     next_last_frame_jmps.insert(cursor, state);
        //                 }
        //             }
        Ok(()) /* scoll */
    }

    fn handle_rect(
        &mut self,
        x: taffy::LengthPercentage,
        y: taffy::LengthPercentage,
        w: taffy::LengthPercentageAuto,
        h: taffy::LengthPercentageAuto,
    ) -> Result<()> {
        let x = resolve_taffy_length(x, self.width);
        let y = resolve_taffy_length(y, self.width);
        let w = resolve_taffy_length(w, self.width);
        let h = resolve_taffy_length(h, self.width);

        let rect = Rect::from_xywh(x + self.x, y + self.y, w, h);
        self.canvas.draw_rect(rect, &self.paint);
        Ok(())
    }

    fn handle_pencil_color(&mut self, color: Color) -> Result<()> {
        self.paint.set_color(color);
        Ok(())
    }

    fn handle_hover(&mut self, rel_ptr: usize) -> Result<()> {
        // if we are NOT hovered we want to execute the jump to ptr, otherwise continue (do nothing)
        // this way the hover state is the one right after the tag
        if self.is_hovered {
            self.next_frame_state
                .entry(self.cursor.cursor)
                .or_insert(CarriedState::new())
                .is_jmp = true;
        }

        if !self
            .frame_state
            .get(&self.cursor.cursor)
            .map(|x| &x.is_jmp)
            .unwrap_or(&false)
        {
            self.cursor.cursor = unsafe { self.cursor.cursor.add(rel_ptr) };
        }
        Ok(())
    }

    fn handle_cursor(&mut self, cursor: CursorIcon) -> Result<()> {
        self.window.set_cursor(cursor);
        Ok(())
    }

    fn handle_event(&mut self, id: usize) -> Result<()> {
        self.cb_push_evt.clone()(id);
        Ok(())
    }

    fn handle_mouse_pressed(&mut self, rel_ptr: usize) -> Result<()> {
        if self.is_hovered && self.input_state.mouse_down {
            self.next_frame_state
                .entry(self.cursor.cursor)
                .or_insert(CarriedState::new())
                .is_jmp = true;
        }

        if !self
            .frame_state
            .get(&self.cursor.cursor)
            .map(|x| &x.is_jmp)
            .unwrap_or(&false)
        {
            self.cursor.cursor = unsafe { self.cursor.cursor.add(rel_ptr) };
        }
        Ok(())
    }

    fn handle_clicked(&mut self, rel_ptr: usize) -> Result<()> {
        if self.is_hovered && self.input_state.mouse_just_released {
            self.next_frame_state
                .entry(self.cursor.cursor)
                .or_insert(CarriedState::new())
                .is_jmp = true;
        }

        if !self
            .frame_state
            .get(&self.cursor.cursor)
            .map(|x| &x.is_jmp)
            .unwrap_or(&false)
        {
            self.cursor.cursor = unsafe { self.cursor.cursor.add(rel_ptr) };
        }

        Ok(())
    }

    fn handle_open_latch(&mut self, _rel_ptr: usize) -> Result<()> {
        /* always falls through */
        Ok(())
    }

    fn handle_closed_latch(&mut self, rel_ptr: usize) -> Result<()> {
        self.cursor.cursor = unsafe { self.cursor.cursor.add(rel_ptr) };
        Ok(())
    }

    fn handle_text(
        &mut self,
        x: taffy::LengthPercentage,
        y: taffy::LengthPercentage,
        _txt: &str,
    ) -> Result<()> {
        let ctx = self
            .tree
            .get_node_context(self.node)
            .ok_or(anyhow!("all nodes need to have context"))?;
        let layout = self.tree.get_final_layout(self.node);

        draw_text(
            ctx.maybe_font_layout.as_ref().ok_or(anyhow!(
                "Somehow trying to draw font node without corresponding layout"
            ))?,
            resolve_taffy_length(x, layout.size.width) + self.x,
            resolve_taffy_length(y, layout.size.height) + self.y,
            &self.canvas,
            &self.paint,
            &self.font_family,
            self.font_size,
            self.config.display_scale,
        )?;
        Ok(())
    }

    fn handle_begin_path(&mut self) -> Result<()> {
        self.maybe_active_path = Some(Path::new());
        Ok(())
    }

    fn handle_move_to(
        &mut self,
        x: taffy::LengthPercentage,
        y: taffy::LengthPercentage,
    ) -> Result<()> {
        let layout = self.tree.get_final_layout(self.node);
        let path = self
            .maybe_active_path
            .as_mut()
            .ok_or(anyhow!("No active path"))?;
        let x = self.x + resolve_taffy_length(x, layout.size.width);
        let y = self.y + resolve_taffy_length(y, layout.size.height);
        path.move_to((x, y));
        Ok(())
    }

    fn handle_line_to(
        &mut self,
        x: taffy::LengthPercentage,
        y: taffy::LengthPercentage,
    ) -> Result<()> {
        let layout = self.tree.get_final_layout(self.node);
        let path = self
            .maybe_active_path
            .as_mut()
            .ok_or(anyhow!("No active path"))?;
        let x = self.x + resolve_taffy_length(x, layout.size.width);
        let y = self.y + resolve_taffy_length(y, layout.size.height);
        path.line_to((x, y));
        Ok(())
    }

    fn handle_quad_to(
        &mut self,
        cx: taffy::LengthPercentage,
        cy: taffy::LengthPercentage,
        x: taffy::LengthPercentage,
        y: taffy::LengthPercentage,
    ) -> Result<()> {
        let layout = self.tree.get_final_layout(self.node);
        let path = self
            .maybe_active_path
            .as_mut()
            .ok_or(anyhow!("No active path"))?;
        let cx = self.x + resolve_taffy_length(cx, layout.size.width);
        let cy = self.y + resolve_taffy_length(cy, layout.size.height);
        let x = self.x + resolve_taffy_length(x, layout.size.width);
        let y = self.y + resolve_taffy_length(y, layout.size.height);
        path.quad_to((cx, cy), (x, y));
        Ok(())
    }

    fn handle_cubic_to(
        &mut self,
        cx1: taffy::LengthPercentage,
        cy1: taffy::LengthPercentage,
        cx2: taffy::LengthPercentage,
        cy2: taffy::LengthPercentage,
        x: taffy::LengthPercentage,
        y: taffy::LengthPercentage,
    ) -> Result<()> {
        let layout = self.tree.get_final_layout(self.node);
        let path = self
            .maybe_active_path
            .as_mut()
            .ok_or(anyhow!("No active path"))?;
        let cx1 = self.x + resolve_taffy_length(cx1, layout.size.width);
        let cy1 = self.y + resolve_taffy_length(cy1, layout.size.height);
        let cx2 = self.x + resolve_taffy_length(cx2, layout.size.width);
        let cy2 = self.y + resolve_taffy_length(cy2, layout.size.height);
        let x = self.x + resolve_taffy_length(x, layout.size.width);
        let y = self.y + resolve_taffy_length(y, layout.size.height);
        path.cubic_to((cx1, cy1), (cx2, cy2), (x, y));
        Ok(())
    }

    fn handle_arc_to(
        &mut self,
        tx: taffy::LengthPercentage,
        ty: taffy::LengthPercentage,
        x: taffy::LengthPercentage,
        y: taffy::LengthPercentage,
        r: taffy::LengthPercentage,
    ) -> Result<()> {
        let layout = self.tree.get_final_layout(self.node);
        let path = self
            .maybe_active_path
            .as_mut()
            .ok_or(anyhow!("No active path"))?;
        let tx = self.x + resolve_taffy_length(tx, layout.size.width);
        let ty = self.y + resolve_taffy_length(ty, layout.size.height);
        let x = self.x + resolve_taffy_length(x, layout.size.width);
        let y = self.y + resolve_taffy_length(y, layout.size.height);
        let r = self.y
            + resolve_taffy_length(
                r,
                if tx > ty {
                    layout.size.width
                } else {
                    layout.size.height
                },
            );
        path.arc_to_tangent((tx, ty), (x, y), r);
        Ok(())
    }

    fn handle_close_path(&mut self) -> Result<()> {
        let path = self
            .maybe_active_path
            .as_mut()
            .ok_or(anyhow!("No active path"))?;
        path.close();
        Ok(())
    }

    fn handle_end_path(&mut self) -> Result<()> {
        let path = self
            .maybe_active_path
            .take()
            .ok_or(anyhow!("No active path"))?;
        self.canvas.draw_path(&path, &self.paint);
        Ok(())
    }

    fn handle_font_size(&mut self, size: f32) -> Result<()> {
        self.font_size = size;
        Ok(())
    }

    fn handle_font_family(&mut self, font_desc: &str) -> Result<()> {
        self.font_family = String::from(font_desc);
        Ok(())
    }
}

fn draw_pass<F>(
    window: Arc<Window>,
    canvas: &Canvas,
    px: f32,
    py: f32,
    tree: &TaffyTree<LayoutContext>,
    node: NodeId,
    cb_push_evt: F,
    frame_state: &HashMap<*const u8, CarriedState>,
    next_frame_state: &mut HashMap<*const u8, CarriedState>,
    input_state: &InputState,
    config: StaticConfig,
) -> Result<()>
where
    F: FnMut(usize) -> () + Clone,
{
    let layout = tree.get_final_layout(node);
    let x = px + layout.location.x;
    let y = py + layout.location.y;

    let ctx = tree
        .get_node_context(node)
        .ok_or(anyhow!("Each node in the taffy tree must have a context"))?;
    let regions = ctx.ragged_members.clone();
    let mut intepreter = DrawIntepreter::new(
        window.clone(),
        canvas,
        x,
        y,
        tree,
        node,
        cb_push_evt.clone(),
        regions,
        frame_state,
        next_frame_state,
        input_state,
        config,
    )?;
    while let Some(_) = intepreter.advance()? {}

    for child in tree.child_ids(node) {
        draw_pass(
            window.clone(),
            canvas,
            x,
            y,
            tree,
            child,
            cb_push_evt.clone(),
            frame_state,
            next_frame_state,
            input_state,
            config,
        )?;
    }
    Ok(())
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
    library: &HashMap<usize, Vec<u8>>,
    frame_state: &HashMap<*const u8, CarriedState>,
    _dt: Duration,
) -> Result<HashMap<*const u8, CarriedState>>
where
    F: FnMut(usize) -> () + Clone,
{
    let config = StaticConfig::new(file_start, base_font_size, display_scale);

    let region_start = unsafe { file_start.add(loc) };
    let (root, mut tree) = layout_pass(region_start, file_end, config, library, frame_state)?;
    tree.compute_layout(
        root,
        taffy::Size {
            width: taffy::prelude::length(width),
            height: taffy::prelude::length(height),
        },
    )?;

    text_pass(&mut tree, root, font_ctx, layout_ctx, config)?;
    let mut next_frame_state: HashMap<*const u8, CarriedState> = HashMap::new();
    draw_pass(
        window,
        canvas,
        0.0,
        0.0,
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
