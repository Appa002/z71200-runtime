use anyhow::{Context, Result, anyhow};
use parley::{Alignment, FontContext, LayoutContext};
use skia_safe::{Canvas, Color, Color4f, HSV, Paint, Path, RGB, Rect};
use std::{collections::HashMap, fmt::Debug, str, sync::Arc};
use strum::{EnumCount, EnumString};
use taffy::{NodeId, PrintTree, TaffyTree, TraversePartialTree};
use winit::window::{CursorIcon, Window};

use super::{InputState, text::draw_text};

// tags that can be used in tagged machine words, comment shows what is expected right after.
// the size after is maximally 32 bit or machine word, and there is implicit padding expected to machine word (as captured in the tagged word struct)
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
    ClosePath, /*  */

    // Pencil
    Color, /* 19 _, TaggedWord{Rgb, param}  */

    // Layout
    Width,   /* 20 _, Pxs, param */
    Height,  // 21
    Padding, // 22 _, left, top, right, bottom
    Margin,  // 23
    Display, /* 24 display option */
    Gap,     /* 25 */

    // States
    Hover,        /* 26 rel_pointer, [... no jmp], [jmp ...] */
    MousePressed, /* 27 rel_pointer, [... no jmp], [jmp ...] */
    Clicked,      /* 28 rel_pointer, [... no jmp], [jmp ...] */
    OpenLatch,    /* 29 rel_pointer, [... no jmp], [jmp ...] */
    CloseLatch,   /* 30 rel_pointer, [... no jmp], [jmp ...] */
    LibraryCall,  /* 31 word */
    Return,       /* 32 */
    PushArg,      /* _, any */
    PullArg,      /* _ */
    PullArgOr,    /* _, [default] */
    LoadReg,      /* word */
    FromReg,      /* word */
    FromRegOr,    /* word */

    // Event
    Event, /* 33 word(id) */

    // Text
    Text,          /* 34 x, y, ptr */
    TextPtr,       /* 35 ptr  */
    FontSize,      /* 36 real */
    FontAlignment, /* 37 alignment */
    FontFamily,    /* 38 _, TextPtr */

    // Cursors
    CursorDefault, /* 39 */
    CursorPointer, /* 40 */
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

pub trait ReadFromMemory: Sized + Copy {
    unsafe fn read_in(cursor: &mut *const u8, trace: &mut Vec<Self>) -> Self {
        let n = std::mem::size_of::<Self>();
        let ptr = (*cursor) as *const Self;
        *cursor = unsafe { cursor.add(n) };
        let it = unsafe { *ptr };
        trace.push(it);
        it
    }
}
impl ReadFromMemory for TaggedWord {}

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

fn maybe_resolve_from_vm_state<'a>(
    it: &TaggedWord,
    maybe_vm_state: Option<(
        &mut Vec<TaggedWord>,
        &'a mut *const u8,
        &mut Vec<TaggedWord>,
        &HashMap<usize, TaggedWord>,
    )>,
) -> Result<((Tag, ParamUnion), Option<&'a mut *const u8>)> {
    let mut maybe_cursor: Option<&'a mut *const u8> = None;

    let (tag, word) = if let Some((stack, cursor, trace, regs)) = maybe_vm_state {
        match &it.tag {
            Tag::PullArg => {
                let pulled = &stack
                    .pop()
                    .ok_or(anyhow!("PullArg called with empty stack."))?;
                (pulled.tag, pulled.word)
            }
            Tag::PullArgOr => {
                if let Some(pulled) = &stack.pop() {
                    (pulled.tag, pulled.word)
                } else {
                    /* read the next word, and provide it as the default */
                    let default = unsafe { TaggedWord::read_in(cursor, trace) };
                    maybe_cursor = Some(cursor);
                    (default.tag, default.word)
                }
            }
            Tag::FromReg => {
                let pulled = regs.get(&unsafe { it.word.word }).ok_or(anyhow!(
                    "FromReg called for register id {}, but it is empty",
                    &unsafe { it.word.word }
                ))?;
                (pulled.tag, pulled.word)
            }
            Tag::FromRegOr => {
                if let Some(pulled) = regs.get(&unsafe { it.word.word }) {
                    (pulled.tag, pulled.word)
                } else {
                    /* read the next word, and provide it as the default */
                    let default = unsafe { TaggedWord::read_in(cursor, trace) };
                    maybe_cursor = Some(cursor);
                    (default.tag, default.word)
                }
            }
            _ => (it.tag, it.word),
        }
    } else {
        (it.tag, it.word)
    };

    Ok(((tag, word), maybe_cursor))
}

// rust world:
macro_rules! define_reader {
    ($name:ident, $tag:path, $return_type:ty) => {
        pub fn $name(
            &self,
            maybe_stack: Option<(
                &mut Vec<Self>,
                &mut *const u8,
                &mut Vec<Self>,
                &HashMap<usize, TaggedWord>,
            )>, // stack, cursor, trace, regs
        ) -> Result<$return_type> {
            // let mut maybe_cursor: Option<&mut *const u8> = None;
            let ((tag, word), maybe_cursor) = maybe_resolve_from_vm_state(self, maybe_stack)?;

            match &tag {
                $tag => Ok(<$return_type as ExtractFromWord>::extract(&word)),
                _ => {
                    if let Some(cursor) = maybe_cursor {
                        /* if maybe_cursor is some we tried reading a default value */
                        *cursor = unsafe { cursor.sub(2 * size_of::<usize>()) };
                    };
                    Err(anyhow!(
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
                    ))
                }
            }
        }
    };
}

impl TaggedWord {
    define_reader!(read_as_array, Tag::Array, usize);
    define_reader!(read_as_enter, Tag::Enter, ());
    define_reader!(read_as_leave, Tag::Leave, ());
    define_reader!(read_as_width, Tag::Width, ());
    define_reader!(read_as_height, Tag::Height, ());
    define_reader!(read_as_padding, Tag::Padding, ());
    define_reader!(read_as_margin, Tag::Margin, ());
    define_reader!(read_as_rect, Tag::Rect, ());
    define_reader!(read_as_pencil_color, Tag::Color, ());
    define_reader!(read_as_text, Tag::Text, ());
    define_reader!(read_as_event, Tag::Event, usize);
    define_reader!(read_as_hover, Tag::Hover, usize);
    define_reader!(read_as_pressed, Tag::MousePressed, usize);
    define_reader!(read_as_clicked, Tag::Clicked, usize);
    define_reader!(read_as_open_latch, Tag::OpenLatch, usize);
    define_reader!(read_as_close_latch, Tag::CloseLatch, usize);
    define_reader!(read_as_text_ptr, Tag::TextPtr, usize);
    define_reader!(read_as_begin_path, Tag::BeginPath, ());
    define_reader!(read_as_end_path, Tag::EndPath, ());
    define_reader!(read_as_move_to, Tag::MoveTo, ());
    define_reader!(read_as_line_to, Tag::LineTo, ());
    define_reader!(read_as_quad_to, Tag::QuadTo, ());
    define_reader!(read_as_cubic_to, Tag::CubicTo, ());
    define_reader!(read_as_close_path, Tag::ClosePath, ());
    define_reader!(read_as_display, Tag::Display, DisplayOption);
    define_reader!(read_as_font_size, Tag::FontSize, f32);
    define_reader!(read_as_font_alignment, Tag::FontAlignment, StoredAlignment);
    define_reader!(read_as_font_family, Tag::FontFamily, ());
    define_reader!(read_as_gap, Tag::Gap, usize);
    define_reader!(read_as_library_call, Tag::LibraryCall, usize);
    define_reader!(read_as_return, Tag::Return, ());
    define_reader!(read_as_arc_to, Tag::ArcTo, ());
    define_reader!(read_as_push_arg, Tag::PushArg, ParamUnion);
    define_reader!(read_as_load_register, Tag::LoadReg, usize);

    pub fn read_as_any_color(
        &self,
        maybe_stack: Option<(
            &mut Vec<Self>,
            &mut *const u8,
            &mut Vec<Self>,
            &HashMap<usize, TaggedWord>,
        )>,
    ) -> Result<Color> {
        let ((tag, word), maybe_cursor) = maybe_resolve_from_vm_state(self, maybe_stack)?;

        match &tag {
            Tag::Rgb => {
                let (r, g, b) = unsafe { word.short_color };
                Ok(RGB { r, g, b }.to_hsv().to_color(255))
            }
            Tag::Hsv => {
                let (h, s, v) = unsafe { word.short_color };
                Ok(HSV {
                    h: h as f32 / 255.0,
                    s: s as f32 / 255.0,
                    v: v as f32 / 255.0,
                }
                .to_color(255))
            }
            Tag::Rgba => {
                let (r, g, b, a) = unsafe { word.long_color };
                Ok(RGB { r, g, b }.to_hsv().to_color(a))
            }
            Tag::Hsva => {
                let (h, s, v, a) = unsafe { word.long_color };
                Ok(HSV {
                    h: h as f32 / 255.0,
                    s: s as f32 / 255.0,
                    v: v as f32 / 255.0,
                }
                .to_color(a))
            }

            _ => {
                if let Some(cursor) = maybe_cursor {
                    /* if maybe_cursor is some we tried reading a default value */
                    *cursor = unsafe { cursor.sub(2 * size_of::<usize>()) };
                };
                Err(anyhow!(
                    "Expected `Rgb`, `Hsv`, `Rgba`, or `Hsva` tagged word, got `{}` instead",
                    if self.tag as usize <= Tag::COUNT {
                        format!("{}", self.tag)
                    } else {
                        format!("corupted tag ({})", self.tag as usize)
                    },
                ))
            }
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

    pub fn read_as_length(
        &self,
        base_font_size: f32,
        max_extend: f32,
        maybe_stack: Option<(
            &mut Vec<Self>,
            &mut *const u8,
            &mut Vec<Self>,
            &HashMap<usize, TaggedWord>,
        )>,
    ) -> Result<Option<f32>> {
        let ((tag, word), maybe_cursor) = maybe_resolve_from_vm_state(self, maybe_stack)?;

        match &tag {
            Tag::Auto => Ok(None),
            Tag::Pxs => Ok(Some(unsafe { word.real })),
            Tag::Rems => Ok(Some(base_font_size * unsafe { word.real })),
            Tag::Frac => Ok(Some(max_extend * unsafe { word.real })),
            _ => {
                if let Some(cursor) = maybe_cursor {
                    /* if maybe_cursor is some we tried reading a default value */
                    *cursor = unsafe { cursor.sub(2 * size_of::<usize>()) };
                };
                Err(anyhow!(
                    "Expected `Pxs`, `Rems`, `Auto`, or `Frac` tagged word, got `{}` instead",
                    if self.tag as usize <= Tag::COUNT {
                        format!("{}", self.tag)
                    } else {
                        format!("corupted tag ({})", self.tag as usize)
                    },
                ))
            }
        }
    }

    pub fn read_as_any_cursor(
        &self,
        maybe_stack: Option<(
            &mut Vec<Self>,
            &mut *const u8,
            &mut Vec<Self>,
            &HashMap<usize, TaggedWord>,
        )>,
    ) -> Result<CursorIcon> {
        let ((tag, _word), maybe_cursor) = maybe_resolve_from_vm_state(self, maybe_stack)?;

        match &tag {
            Tag::CursorDefault => Ok(CursorIcon::Default),
            Tag::CursorPointer => Ok(CursorIcon::Pointer),
            _ => {
                if let Some(cursor) = maybe_cursor {
                    /* if maybe_cursor is some we tried reading a default value */
                    *cursor = unsafe { cursor.sub(2 * size_of::<usize>()) };
                };
                Err(anyhow!(
                    "Expected a tagged word of the `Cursor` family, got `{}` instead",
                    if self.tag as usize <= Tag::COUNT {
                        format!("{}", self.tag)
                    } else {
                        format!("corupted tag ({})", self.tag as usize)
                    },
                ))
            }
        }
    }
}

pub fn read_str_from_array_tagged_word(ptr: usize, file_start: *const u8) -> Result<String> {
    let mut str_cursor = unsafe { file_start.add(ptr) };
    let size = unsafe { TaggedWord::read_in(&mut str_cursor, &mut Vec::new()) }
        .read_as_array(None)
        .with_context(|| format!("Reading string at loc {:x} failed.", ptr))?;

    let str = str::from_utf8(unsafe { std::slice::from_raw_parts(str_cursor, size) })?;
    Ok(str.to_owned())
}
fn pprint_trace<T>(trace: &Vec<T>) -> String
where
    T: Debug,
{
    trace
        .iter()
        .fold(Vec::<(String, usize)>::new(), |mut acc, x| {
            let tag_str = format!("{:?}", x);
            if let Some((last_tag, count)) = acc.last_mut() {
                if last_tag == &tag_str {
                    *count += 1;
                    return acc;
                }
            }
            acc.push((tag_str, 1));
            acc
        })
        .iter()
        .map(|(tag, count)| {
            if *count > 1 {
                format!("'{} x{}'\n", tag, count)
            } else {
                format!("'{}'\n", tag)
            }
        })
        .collect::<String>()
}

/* memory layout of dom tree (each |-| is a tagged word, expected that the param is given):
Element := |Array(n)| |Element| |Style| |Style| |Style| |Child| */

fn _print_mem(start: *const u8, len: usize) {
    unsafe {
        std::slice::from_raw_parts::<u8>(start, len)
            .iter()
            .for_each(|b| {
                if *b == 0 {
                    print!("{} ", b);
                } else {
                    print!("\x1b[31m{} \x1b[0m", b);
                }
            });
    }
    println!("");
}

#[derive(Debug, Clone, Default)]
struct MyContext {
    ragged_members: Vec<(*const u8, *const u8)>,
}

pub unsafe fn draw<F>(
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
    layout_ctx: &mut LayoutContext<()>,
    display_scale: f32,
    base_font_size: f32,
    library: &HashMap<usize, Vec<u8>>,
    last_frame_jmps: &HashMap<*const u8, bool>,
) -> Result<HashMap<*const u8, bool>>
where
    F: FnMut(usize) -> () + Clone,
{
    // 1) read the layout in from memory.
    let mut read_trace = Vec::new();
    let (root, mut tree) = unsafe {
        read_from_memory(
            loc,
            file_start,
            file_end,
            width as f32,
            height as f32,
            base_font_size,
            library,
            last_frame_jmps,
            &mut read_trace,
        )
    }
    .with_context(|| format!("Tag Trace:\n{}", pprint_trace(&read_trace)))?;
    // 2) compute the layout TODO: check hash to make sure we aren't wasting cycles.
    tree.compute_layout(
        root,
        taffy::Size {
            width: taffy::AvailableSpace::Definite(width as f32),
            height: taffy::AvailableSpace::Definite(height as f32),
        },
    )?;

    // tree.print_tree(root);

    // 3) Draw the tree
    let mut draw_trace = Vec::new();
    let mut registers = HashMap::new();
    let mut next_last_frame_jmps: HashMap<*const u8, bool> = HashMap::new();
    unsafe {
        draw_tree(
            0.0,
            0.0,
            canvas,
            window,
            root,
            &tree,
            input_state,
            &String::from("0000"),
            cb_push_evt,
            file_start,
            file_end,
            font_ctx,
            layout_ctx,
            display_scale,
            base_font_size,
            last_frame_jmps,
            &mut next_last_frame_jmps,
            &mut draw_trace,
            &mut Vec::new(),
            &mut registers,
        )
    }
    .with_context(|| format!("Tag Trace:\n{}", pprint_trace(&draw_trace)))?;

    Ok(next_last_frame_jmps)
}

fn emit_node_through_enter_with_location(
    cursor: *const u8,
    cur_start_ptr: *const u8,
    depth: &mut i16,
    node_stack: &mut Vec<NodeId>,
    tree: &mut TaffyTree<MyContext>,
) -> Result<()> {
    *depth += 1;

    let cur_node = *node_stack.last().unwrap();

    let mut ctx: MyContext = tree.get_node_context(cur_node).cloned().unwrap_or_default(); /* TODO: eliminate copy here */
    ctx.ragged_members.push((cur_start_ptr, cursor));

    tree.set_node_context(cur_node, Some(ctx))?;
    node_stack.push(tree.new_leaf(taffy::Style::default())?);
    Ok(())
}

fn emit_node_through_exit_with_location(
    cursor: *const u8,
    cur_start_ptr: *const u8,
    depth: &mut i16,
    node_stack: &mut Vec<NodeId>,
    tree: &mut TaffyTree<MyContext>,
) -> Result<()> {
    *depth -= 1;

    let cur_node = node_stack.pop().unwrap();
    let parent = node_stack.last();

    // Push a new node range
    /* don't do this if we are in a library */
    let mut ctx: MyContext = tree.get_node_context(cur_node).cloned().unwrap_or_default(); /* TODO: eliminate copy here */
    ctx.ragged_members.push((cur_start_ptr, cursor));
    tree.set_node_context(cur_node, Some(ctx))?;
    // Update connectivness
    if let Some(parent) = parent {
        /* root node doesn't have a parent. */
        tree.add_child(*parent, cur_node)?;
    }

    Ok(())
}

unsafe fn read_from_memory(
    loc: usize,
    file_start: *const u8,
    file_end: *const u8,
    width: f32,
    height: f32,
    base_font_size: f32,
    library: &HashMap<usize, Vec<u8>>,
    last_frame_jmps: &HashMap<*const u8, bool>,
    trace: &mut Vec<TaggedWord>,
) -> Result<(NodeId, TaffyTree<MyContext>)> {
    let mut tree = TaffyTree::new();

    /* Main Scan */
    let mut cursor = unsafe { file_start.add(loc) };

    if (cursor as usize) % size_of::<usize>() != 0 {
        return Err(anyhow!("cursor is not aligned"));
    }

    if (file_end as usize) % size_of::<usize>() != 0 {
        return Err(anyhow!("file_end is not aligned"));
    }

    unsafe { TaggedWord::read_in(&mut cursor, trace) }.read_as_enter(None)?;
    let root = tree.new_leaf_with_context(
        taffy::Style {
            flex_grow: 1.0,
            display: taffy::Display::Flex,
            size: taffy::Size {
                width: taffy::prelude::length(width),
                height: taffy::prelude::length(height),
            },
            ..Default::default()
        },
        MyContext::default(),
    )?;

    let mut depth: i16 = 1; /* this call succeeded so we are inside the element */
    let mut node_stack: Vec<NodeId> = vec![root];
    let mut cur_start_ptr = unsafe { cursor.sub(2 * size_of::<usize>()) };
    let mut call_stack: Vec<*const u8> = Vec::new();

    while depth > 0 {
        if (cursor >= file_end || cursor < file_start) && call_stack.len() == 0 {
            return Err(anyhow!("Cursor is out of bounds in layout pass"));
        }
        if (cursor as usize) % size_of::<usize>() != 0 {
            return Err(anyhow!("cursor has become unaligned in layout pass."));
        }
        if trace.len() > 500 {
            return Err(anyhow!("Too many instruction in layout pass"));
        }

        let tag = unsafe { TaggedWord::read_in(&mut cursor, trace) };
        if tag.read_as_enter(None).is_ok() {
            emit_node_through_enter_with_location(
                cursor,
                cur_start_ptr,
                &mut depth,
                &mut node_stack,
                &mut tree,
            )?;
            cur_start_ptr = unsafe { cursor.sub(2 * size_of::<usize>()) };
        } else if tag.read_as_leave(None).is_ok() {
            emit_node_through_exit_with_location(
                cursor,
                cur_start_ptr,
                &mut depth,
                &mut node_stack,
                &mut tree,
            )?;
            cur_start_ptr = cursor;
        } else if let Ok(id) = tag.read_as_library_call(None) {
            // branch into the library code.
            let code = library
                .get(&id)
                .ok_or(anyhow!("Requested library element {} not found.", id))?;
            call_stack.push(cursor);
            emit_node_through_enter_with_location(
                cursor,
                cur_start_ptr,
                &mut depth,
                &mut node_stack,
                &mut tree,
            )?;
            cursor = code.as_ptr();
            cur_start_ptr = cursor;
        } else if tag.read_as_return(None).is_ok() {
            emit_node_through_exit_with_location(
                cursor,
                cur_start_ptr,
                &mut depth,
                &mut node_stack,
                &mut tree,
            )?;
            let ret_ptr = call_stack.pop().ok_or(anyhow!(
                "`InlineReturn` tag called without being in library code."
            ))?;
            cursor = ret_ptr;
            cur_start_ptr = cursor;
        }
        // Element definition cases (we only care about collecting things related to taffy layout here)
        else if tag.read_as_width(None).is_ok() {
            let width = unsafe { TaggedWord::read_in(&mut cursor, trace) }
                .read_as_taffy_length_pctauto(base_font_size)?;

            let cur_node = node_stack.last().unwrap();
            let mut cur_style = tree.style(*cur_node)?.clone();
            cur_style.size.width = taffy::Dimension::from(width);
            tree.set_style(*cur_node, cur_style)?;
        } else if tag.read_as_height(None).is_ok() {
            let height = unsafe { TaggedWord::read_in(&mut cursor, trace) }
                .read_as_taffy_length_pctauto(base_font_size)?;

            let cur_node = node_stack.last().unwrap();
            let mut cur_style = tree.style(*cur_node)?.clone();
            cur_style.size.height = taffy::Dimension::from(height);
            tree.set_style(*cur_node, cur_style)?;
        } else if tag.read_as_margin(None).is_ok() {
            let left = unsafe { TaggedWord::read_in(&mut cursor, trace) }
                .read_as_taffy_length_pctauto(base_font_size)?;
            let top = unsafe { TaggedWord::read_in(&mut cursor, trace) }
                .read_as_taffy_length_pctauto(base_font_size)?;
            let right = unsafe { TaggedWord::read_in(&mut cursor, trace) }
                .read_as_taffy_length_pctauto(base_font_size)?;
            let bottom = unsafe { TaggedWord::read_in(&mut cursor, trace) }
                .read_as_taffy_length_pctauto(base_font_size)?;

            let cur_node = node_stack.last().unwrap();
            let mut cur_style = tree.style(*cur_node)?.clone();
            cur_style.margin = taffy::Rect {
                left,
                right,
                top,
                bottom,
            };
            tree.set_style(*cur_node, cur_style)?;
        } else if tag.read_as_padding(None).is_ok() {
            let left = unsafe { TaggedWord::read_in(&mut cursor, trace) }
                .read_as_taffy_length_pct(base_font_size)?;
            let top = unsafe { TaggedWord::read_in(&mut cursor, trace) }
                .read_as_taffy_length_pct(base_font_size)?;
            let right = unsafe { TaggedWord::read_in(&mut cursor, trace) }
                .read_as_taffy_length_pct(base_font_size)?;
            let bottom = unsafe { TaggedWord::read_in(&mut cursor, trace) }
                .read_as_taffy_length_pct(base_font_size)?;

            let cur_node = node_stack.last().unwrap();
            let mut cur_style = tree.style(*cur_node)?.clone();
            cur_style.padding = taffy::Rect {
                left,
                right,
                top,
                bottom,
            };
            tree.set_style(*cur_node, cur_style)?;
        } else if let Ok(display) = tag.read_as_display(None) {
            let cur_node = node_stack.last().unwrap();
            let mut cur_style = tree.style(*cur_node)?.clone();
            match display {
                DisplayOption::Block => cur_style.display = taffy::Display::Block,
                DisplayOption::FlexRow => cur_style.display = taffy::Display::Flex,
                DisplayOption::FlexColumn => cur_style.display = taffy::Display::Flex,
                DisplayOption::Grid => cur_style.display = taffy::Display::Grid,
                DisplayOption::None => cur_style.display = taffy::Display::None,
            }
            match display {
                DisplayOption::FlexRow => cur_style.flex_direction = taffy::FlexDirection::Row,
                DisplayOption::FlexColumn => {
                    cur_style.flex_direction = taffy::FlexDirection::Column
                }
                _ => (),
            }
            tree.set_style(*cur_node, cur_style)?;
        } else if tag.read_as_gap(None).is_ok() {
            let width = unsafe { TaggedWord::read_in(&mut cursor, trace) }
                .read_as_taffy_length_pct(base_font_size)?;
            let height = unsafe { TaggedWord::read_in(&mut cursor, trace) }
                .read_as_taffy_length_pct(base_font_size)?;
            let cur_node = node_stack.last().unwrap();
            let mut cur_style = tree.style(*cur_node)?.clone();
            cur_style.gap = taffy::Size { width, height };
            tree.set_style(*cur_node, cur_style)?;
        } else if let Ok(rel_ptr) = tag.read_as_hover(None) {
            // if we are NOT hovered we want to execute the jump to ptr, otherwise continue (do nothing)
            // this way the hover state is the one right after the tag

            if !last_frame_jmps.get(&cursor).unwrap_or(&false) {
                cursor = unsafe { cursor.add(rel_ptr) };
            }
        } else if let Ok(rel_ptr) = tag.read_as_pressed(None) {
            if !last_frame_jmps.get(&cursor).unwrap_or(&false) {
                cursor = unsafe { cursor.add(rel_ptr) };
            }
        } else if let Ok(rel_ptr) = tag.read_as_clicked(None) {
            if !last_frame_jmps.get(&cursor).unwrap_or(&false) {
                cursor = unsafe { cursor.add(rel_ptr) };
            }
        } else if let Ok(_rel_ptr) = tag.read_as_open_latch(None) {
            /* the open latch just falls through */
        } else if let Ok(rel_ptr) = tag.read_as_close_latch(None) {
            cursor = unsafe { cursor.add(rel_ptr) }; /* the closed latch always jumps */
        }
    }

    Ok((root, tree))
}

unsafe fn draw_tree<F>(
    px: f32,
    py: f32,
    canvas: &Canvas,
    window: Arc<Window>,
    node: NodeId,
    tree: &TaffyTree<MyContext>,
    input_state: &InputState,
    uid: &String,
    mut cb_push_evt: F,
    file_start: *const u8,
    file_end: *const u8,
    font_ctx: &mut FontContext,
    layout_ctx: &mut LayoutContext<()>,
    display_scale: f32,
    base_font_size: f32,
    last_frame_jmps: &HashMap<*const u8, bool>,
    next_last_frame_jmps: &mut HashMap<*const u8, bool>,
    trace: &mut Vec<TaggedWord>,
    arg_stack: &mut Vec<TaggedWord>,
    registers: &mut HashMap<usize, TaggedWord>,
) -> Result<()>
where
    F: FnMut(usize) -> () + Clone,
{
    let ctx = tree.get_node_context(node).ok_or(anyhow!(
        "All nodes in the taffy tree must have a context to be rendered"
    ))?;
    let layout = tree.get_final_layout(node);

    let mut paint = Paint::new(Color4f::new(0.0, 0.0, 0.0, 1.0), None);
    paint.set_anti_alias(true);

    let loc_abs_x = px + layout.location.x;
    let loc_abs_y = py + layout.location.y;

    for &(start, end) in ctx.ragged_members.iter() {
        // print_mem(start, unsafe { end.sub(start as usize) } as usize);

        let mut font_size = base_font_size;
        let mut font_alignment = Alignment::Start;
        let mut font_family = String::from("Arial");

        if (start as usize) % size_of::<usize>() != 0 {
            return Err(anyhow!("start pointer is not aligned"));
        }

        if (end as usize) % size_of::<usize>() != 0 {
            return Err(anyhow!("end pointer is not aligned"));
        }

        let mut cursor = start;
        while cursor < end {
            if trace.len() > 500 {
                return Err(anyhow!("Too many instruction in render pass"));
            }

            // if (cursor >= file_end || cursor < file_start) && call_stack.len() == 0 {
            //     return Err(anyhow!("Cursor is out of bounds in render pass"));
            // }

            if (cursor as usize) % size_of::<usize>() != 0 {
                return Err(anyhow!("cursor has become unaligned in render pass."));
            }

            /* Let's just compute some ui state that depends on this layout that will be reused. */
            let (x, y, w, h) = (
                loc_abs_x as f64,
                loc_abs_y as f64,
                layout.size.width as f64,
                layout.size.height as f64,
            );

            let is_hovered = input_state.cursor_pos.x < x + w
                && input_state.cursor_pos.x > x
                && input_state.cursor_pos.y < y + h
                && input_state.cursor_pos.y > y;

            // Okay this is the element we are drawing let's do it.
            let tag = unsafe { TaggedWord::read_in(&mut cursor, trace) };
            if tag.read_as_rect(None).is_ok() {
                // argument layout is x, y, width, height
                let x = unsafe { TaggedWord::read_in(&mut cursor, trace) }
                    .read_as_length(
                        base_font_size,
                        layout.size.width,
                        Some((arg_stack, &mut cursor, trace, registers)),
                    )?
                    .ok_or(anyhow!("X/Y position can't have auto size"))?;
                let y = unsafe { TaggedWord::read_in(&mut cursor, trace) }
                    .read_as_length(
                        base_font_size,
                        layout.size.height,
                        Some((arg_stack, &mut cursor, trace, registers)),
                    )?
                    .ok_or(anyhow!("X/Y position can't have auto size"))?;

                let w = unsafe { TaggedWord::read_in(&mut cursor, trace) }.read_as_length(
                    base_font_size,
                    layout.size.width,
                    Some((arg_stack, &mut cursor, trace, registers)),
                )?;
                let h = unsafe { TaggedWord::read_in(&mut cursor, trace) }.read_as_length(
                    base_font_size,
                    layout.size.height,
                    Some((arg_stack, &mut cursor, trace, registers)),
                )?;

                let w = w.unwrap_or(layout.size.width);
                let h = h.unwrap_or(layout.size.height);

                let rect = Rect::from_xywh(x + loc_abs_x, y + loc_abs_y, w, h);
                canvas.draw_rect(rect, &paint);

                // canvas.fill_path(&mut path, &paint);
            } else if tag.read_as_pencil_color(None).is_ok() {
                let colour = unsafe { TaggedWord::read_in(&mut cursor, trace) }
                    .read_as_any_color(Some((arg_stack, &mut cursor, trace, registers)))?;
                paint.set_color(colour);
            } else if let Ok(rel_ptr) = tag.read_as_hover(None) {
                // if we are NOT hovered we want to execute the jump to ptr, otherwise continue (do nothing)
                // this way the hover state is the one right after the tag
                if is_hovered {
                    next_last_frame_jmps.insert(cursor, true);
                }

                if !last_frame_jmps.get(&cursor).unwrap_or(&false) {
                    cursor = unsafe { cursor.add(rel_ptr) };
                }
            } else if let Ok(cursor) =
                tag.read_as_any_cursor(Some((arg_stack, &mut cursor, trace, registers)))
            {
                window.set_cursor(cursor);
            } else if let Ok(id) = tag.read_as_event(None) {
                cb_push_evt(id); /* expects caller to handle errors */
            } else if let Ok(rel_ptr) = tag.read_as_pressed(None) {
                if is_hovered && input_state.mouse_down {
                    next_last_frame_jmps.insert(cursor, true);
                }

                if !last_frame_jmps.get(&cursor).unwrap_or(&false) {
                    cursor = unsafe { cursor.add(rel_ptr) };
                }
            } else if let Ok(rel_ptr) = tag.read_as_clicked(None) {
                if is_hovered && input_state.mouse_just_released {
                    next_last_frame_jmps.insert(cursor, true);
                }

                if !last_frame_jmps.get(&cursor).unwrap_or(&false) {
                    cursor = unsafe { cursor.add(rel_ptr) };
                }
            } else if let Ok(_rel_ptr) = tag.read_as_open_latch(None) {
                /* the open latch just falls through */
            } else if let Ok(rel_ptr) = tag.read_as_close_latch(None) {
                cursor = unsafe { cursor.add(rel_ptr) }; /* the closed latch always jumps */
            } else if tag.read_as_text(None).is_ok() {
                let x = unsafe { TaggedWord::read_in(&mut cursor, trace) }
                    .read_as_length(
                        base_font_size,
                        layout.size.width,
                        Some((arg_stack, &mut cursor, trace, registers)),
                    )?
                    .ok_or(anyhow!("X/Y position can't have auto size"))?;
                let y = unsafe { TaggedWord::read_in(&mut cursor, trace) }
                    .read_as_length(
                        base_font_size,
                        layout.size.height,
                        Some((arg_stack, &mut cursor, trace, registers)),
                    )?
                    .ok_or(anyhow!("X/Y position can't have auto size"))?;

                let ptr = unsafe { TaggedWord::read_in(&mut cursor, trace) }
                    .read_as_text_ptr(Some((arg_stack, &mut cursor, trace, registers)))?;
                let txt = read_str_from_array_tagged_word(ptr, file_start)?;
                draw_text(
                    x + loc_abs_x,
                    y + loc_abs_y,
                    canvas,
                    &paint,
                    layout.size.width - x,
                    &font_family,
                    font_size,
                    font_alignment,
                    &txt,
                    font_ctx,
                    layout_ctx,
                    display_scale,
                )?;
            } else if tag.read_as_begin_path(None).is_ok() {
                // scan until end_path and construct the path.
                let mut path = Path::new();
                let mut tag = tag;
                while cursor < end && !tag.read_as_end_path(None).is_ok() {
                    tag = unsafe { TaggedWord::read_in(&mut cursor, trace) };
                    if tag.read_as_move_to(None).is_ok() {
                        let x = unsafe { TaggedWord::read_in(&mut cursor, trace) }
                            .read_as_length(
                                base_font_size,
                                layout.size.width,
                                Some((arg_stack, &mut cursor, trace, registers)),
                            )?
                            .unwrap_or(layout.size.width);
                        let y = unsafe { TaggedWord::read_in(&mut cursor, trace) }
                            .read_as_length(
                                base_font_size,
                                layout.size.height,
                                Some((arg_stack, &mut cursor, trace, registers)),
                            )?
                            .unwrap_or(layout.size.height);
                        path.move_to((x + loc_abs_x, y + loc_abs_y));
                    } else if tag.read_as_line_to(None).is_ok() {
                        let x = unsafe { TaggedWord::read_in(&mut cursor, trace) }
                            .read_as_length(
                                base_font_size,
                                layout.size.width,
                                Some((arg_stack, &mut cursor, trace, registers)),
                            )?
                            .unwrap_or(layout.size.width);
                        let y = unsafe { TaggedWord::read_in(&mut cursor, trace) }
                            .read_as_length(
                                base_font_size,
                                layout.size.height,
                                Some((arg_stack, &mut cursor, trace, registers)),
                            )?
                            .unwrap_or(layout.size.height);
                        path.line_to((x + loc_abs_x, y + loc_abs_y));
                    } else if tag.read_as_quad_to(None).is_ok() {
                        let x1 = unsafe { TaggedWord::read_in(&mut cursor, trace) }
                            .read_as_length(
                                base_font_size,
                                layout.size.width,
                                Some((arg_stack, &mut cursor, trace, registers)),
                            )?
                            .unwrap_or(layout.size.width);
                        let y1 = unsafe { TaggedWord::read_in(&mut cursor, trace) }
                            .read_as_length(
                                base_font_size,
                                layout.size.height,
                                Some((arg_stack, &mut cursor, trace, registers)),
                            )?
                            .unwrap_or(layout.size.height);
                        let x2 = unsafe { TaggedWord::read_in(&mut cursor, trace) }
                            .read_as_length(
                                base_font_size,
                                layout.size.width,
                                Some((arg_stack, &mut cursor, trace, registers)),
                            )?
                            .unwrap_or(layout.size.width);
                        let y2 = unsafe { TaggedWord::read_in(&mut cursor, trace) }
                            .read_as_length(
                                base_font_size,
                                layout.size.height,
                                Some((arg_stack, &mut cursor, trace, registers)),
                            )?
                            .unwrap_or(layout.size.height);
                        path.quad_to(
                            (x1 + loc_abs_x, y1 + loc_abs_y),
                            (x2 + loc_abs_x, y2 + loc_abs_y),
                        );
                    } else if tag.read_as_cubic_to(None).is_ok() {
                        let x1 = unsafe { TaggedWord::read_in(&mut cursor, trace) }
                            .read_as_length(
                                base_font_size,
                                layout.size.width,
                                Some((arg_stack, &mut cursor, trace, registers)),
                            )?
                            .unwrap_or(layout.size.width);
                        let y1 = unsafe { TaggedWord::read_in(&mut cursor, trace) }
                            .read_as_length(
                                base_font_size,
                                layout.size.height,
                                Some((arg_stack, &mut cursor, trace, registers)),
                            )?
                            .unwrap_or(layout.size.height);
                        let x2 = unsafe { TaggedWord::read_in(&mut cursor, trace) }
                            .read_as_length(
                                base_font_size,
                                layout.size.width,
                                Some((arg_stack, &mut cursor, trace, registers)),
                            )?
                            .unwrap_or(layout.size.width);
                        let y2 = unsafe { TaggedWord::read_in(&mut cursor, trace) }
                            .read_as_length(
                                base_font_size,
                                layout.size.height,
                                Some((arg_stack, &mut cursor, trace, registers)),
                            )?
                            .unwrap_or(layout.size.height);
                        let x3 = unsafe { TaggedWord::read_in(&mut cursor, trace) }
                            .read_as_length(
                                base_font_size,
                                layout.size.width,
                                Some((arg_stack, &mut cursor, trace, registers)),
                            )?
                            .unwrap_or(layout.size.width);
                        let y3 = unsafe { TaggedWord::read_in(&mut cursor, trace) }
                            .read_as_length(
                                base_font_size,
                                layout.size.height,
                                Some((arg_stack, &mut cursor, trace, registers)),
                            )?
                            .unwrap_or(layout.size.height);
                        path.cubic_to(
                            (x1 + loc_abs_x, y1 + loc_abs_y),
                            (x2 + loc_abs_x, y2 + loc_abs_y),
                            (x3 + loc_abs_x, y3 + loc_abs_y),
                        );
                    } else if tag.read_as_arc_to(None).is_ok() {
                        let x1 = unsafe { TaggedWord::read_in(&mut cursor, trace) }
                            .read_as_length(
                                base_font_size,
                                layout.size.width,
                                Some((arg_stack, &mut cursor, trace, registers)),
                            )?
                            .unwrap_or(layout.size.width);
                        let y1 = unsafe { TaggedWord::read_in(&mut cursor, trace) }
                            .read_as_length(
                                base_font_size,
                                layout.size.height,
                                Some((arg_stack, &mut cursor, trace, registers)),
                            )?
                            .unwrap_or(layout.size.height);
                        let x2 = unsafe { TaggedWord::read_in(&mut cursor, trace) }
                            .read_as_length(
                                base_font_size,
                                layout.size.width,
                                Some((arg_stack, &mut cursor, trace, registers)),
                            )?
                            .unwrap_or(layout.size.width);
                        let y2 = unsafe { TaggedWord::read_in(&mut cursor, trace) }
                            .read_as_length(
                                base_font_size,
                                layout.size.height,
                                Some((arg_stack, &mut cursor, trace, registers)),
                            )?
                            .unwrap_or(layout.size.height);
                        let r = unsafe { TaggedWord::read_in(&mut cursor, trace) }
                            .read_as_length(
                                base_font_size,
                                layout.size.width,
                                Some((arg_stack, &mut cursor, trace, registers)),
                            )?
                            .ok_or(anyhow!("ArcTo radius cannot be 'Auto'"))?;

                        path.arc_to_tangent(
                            (x1 + loc_abs_x, y1 + loc_abs_y),
                            (x2 + loc_abs_x, y2 + loc_abs_y),
                            r,
                        );
                    } else if tag.read_as_close_path(None).is_ok() {
                        path.close();
                    }
                }
                if !tag.read_as_end_path(None).is_ok() {
                    return Err(anyhow!(
                        "A path was opened with `BeginPath` but was never closed with `EndPath`"
                    )); /* this means paths can't span acros ragged bounds; that's okay */
                }
                canvas.draw_path(&path, &paint);
            } else if let Ok(size) =
                tag.read_as_font_size(Some((arg_stack, &mut cursor, trace, registers)))
            {
                font_size = size;
            } else if let Ok(alignment) =
                tag.read_as_font_alignment(Some((arg_stack, &mut cursor, trace, registers)))
            {
                font_alignment = match alignment {
                    /* we need the separate stored alignment to make sure it is usize */
                    StoredAlignment::Start => Alignment::Start,
                    StoredAlignment::End => Alignment::End,
                    StoredAlignment::Left => Alignment::Left,
                    StoredAlignment::Middle => Alignment::Middle,
                    StoredAlignment::Right => Alignment::Right,
                    StoredAlignment::Justified => Alignment::Justified,
                };
            } else if tag.read_as_font_family(None).is_ok() {
                let ptr = unsafe { TaggedWord::read_in(&mut cursor, trace) }
                    .read_as_text_ptr(Some((arg_stack, &mut cursor, trace, registers)))?;
                font_family = read_str_from_array_tagged_word(ptr, file_start)?;
            } else if tag.read_as_push_arg(None).is_ok() {
                let tagged_word = unsafe { TaggedWord::read_in(&mut cursor, trace) };
                arg_stack.push(tagged_word);
            } else if let Ok(id) = tag.read_as_load_register(None) {
                let value = unsafe { TaggedWord::read_in(&mut cursor, trace) };
                let ((to_store_tag, to_store_word), _) = maybe_resolve_from_vm_state(
                    &value,
                    Some((arg_stack, &mut cursor, trace, registers)),
                )?;
                registers.insert(
                    id,
                    TaggedWord {
                        tag: to_store_tag,
                        word: to_store_word,
                    },
                );
            }

            // there is a bunch of elements that are legal but ignored.
        }
    }
    // Recurse into children
    for child in tree.child_ids(node) {
        unsafe {
            draw_tree(
                loc_abs_x,
                loc_abs_y,
                canvas,
                window.clone(),
                child,
                tree,
                input_state,
                uid,
                cb_push_evt.clone(),
                file_start,
                file_end,
                font_ctx,
                layout_ctx,
                display_scale,
                base_font_size,
                last_frame_jmps,
                next_last_frame_jmps,
                trace,
                arg_stack,
                registers,
            )?
        };
    }
    Ok(())
}
