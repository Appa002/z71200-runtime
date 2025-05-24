use std::{collections::HashMap, usize};

use anyhow::{Context, Result, anyhow};
use skia_safe::Color;
use taffy::{NodeId, TaffyTree};
use winit::window::CursorIcon;

use super::cursors::LinearCursor;
use super::{CarriedState, Tag, TaggedWord};

use super::DisplayOption;
use super::traits::{Executor, Intepreter, ReadIn};
use super::utils::StaticConfig;
use super::vm_state::VMState;

// ::: ---- Rendering Code --- :::
// Rendering is done in three passes
// 1) Construct the layout tree
// 2) Layout text now that bounds are known
// 3) Draw everything

// ::: ---- First Pass, Construct Layout Tree ----:::
#[derive(Clone, Default)]
pub(crate) struct LayoutContext {
    pub ragged_members: Vec<(*const u8, *const u8)>,
    pub maybe_font_layout: Option<parley::Layout<()>>,
}

struct LayoutIntepreter<'a> {
    config: StaticConfig,
    state: VMState,
    cursor: LinearCursor,

    last_frame_state: &'a HashMap<*const u8, CarriedState>,

    tree: TaffyTree<LayoutContext>,
    node_stack: Vec<NodeId>,
    cur_start_ptr: *const u8,
    // call_stack: Vec<*const u8>,
    root: NodeId,
}
impl<'a> LayoutIntepreter<'a> {
    fn new(
        region_start: *const u8,
        region_end: *const u8,
        config: StaticConfig,
        last_frame_state: &'a HashMap<*const u8, CarriedState>,
    ) -> Result<Self> {
        assert!(
            region_start as usize % size_of::<usize>() == 0,
            "region_start is unaligned."
        );

        // Consume the first node here which must be enter.
        let mut cursor = LinearCursor::new(region_start, region_end);
        let tagged_word = unsafe { TaggedWord::read_in(&mut cursor.cursor) };
        if tagged_word.tag != Tag::Enter {
            return Err(anyhow!(
                "Memory region must begin with `Enter`, found {:?}",
                tagged_word.tag
            ));
        }
        cursor.add_depth(); /* this also means we are one depth in */

        // create a node for the root, which we just read
        let mut tree = TaffyTree::new();
        let root = tree.new_leaf_with_context(taffy::Style::default(), LayoutContext::default())?;
        let node_stack = vec![root];

        Ok(Self {
            config,
            state: VMState::new(),
            cursor,
            tree,
            node_stack,
            cur_start_ptr: region_start,
            last_frame_state,
            root,
        })
    }

    fn enter_child(&mut self) -> Result<()> {
        // This is used for all ways of entering children: `Enter`, `LibraryCall`, or `Call`
        // the reason to make this separate is that the `self.cur_start_ptr` needs to be updated differently
        // depending on if we are jumping into different memory regions.
        let cur_node = self
            .node_stack
            .last()
            .ok_or(anyhow!("At least one `Leave` too many."))?;
        self.cursor.add_depth();

        // otherwise this is the root
        let mut ctx: LayoutContext = self
            .tree
            .get_node_context(*cur_node)
            .cloned()
            .unwrap_or_default(); /* TODO: eliminate copy here */
        ctx.ragged_members
            .push((self.cur_start_ptr, self.cursor.cursor));
        self.tree.set_node_context(*cur_node, Some(ctx))?;

        self.node_stack.push(
            self.tree
                .new_leaf_with_context(taffy::Style::default(), LayoutContext::default())?,
        );

        Ok(())
    }

    fn leave_child(&mut self) -> Result<()> {
        // This is used for all ways of leaving children: `Leave`, `LibraryReturn`, or `Return`
        // the reason to make this separate is that the `self.cur_start_ptr` needs to be updated differently
        // depending on if we are jumping into different memory regions.

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
        self.enter_child()?;
        self.cur_start_ptr = unsafe { self.cursor.cursor.sub(2 * size_of::<usize>()) };
        Ok(())
    }

    fn handle_leave(&mut self) -> Result<()> {
        self.leave_child()?;
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

    fn handle_text(
        &mut self,
        _x: taffy::LengthPercentage,
        _y: taffy::LengthPercentage,
        _txt: &str,
    ) -> Result<()> {
        Ok(())
    }

    fn handle_font_alignment(&mut self, _alignment: super::StoredAlignment) -> Result<()> {
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

pub(super) fn layout_pass(
    region_start: *const u8,
    region_end: *const u8,
    config: StaticConfig,
    last_frame_state: &HashMap<*const u8, CarriedState>,
) -> Result<(NodeId, TaffyTree<LayoutContext>)> {
    assert!(
        region_start as usize % size_of::<usize>() == 0,
        "region_start not aligned"
    );

    let mut intepreter = LayoutIntepreter::new(region_start, region_end, config, last_frame_state)?;

    let mut trace = Vec::new();
    while let Some(_) = intepreter.advance(&mut trace).with_context(|| {
        let n = 10;
        let slice = trace.get(trace.len().saturating_sub(n)..).unwrap_or(&[]);

        let mut out = String::from("\n***Context [Layout Pass]***\n");
        for (i, tagged_word) in slice.iter().enumerate() {
            let color = if i == n - 1 { "\x1B[31m" } else { "\x1B[0m" };

            out.push_str(&format!(
                "{}{:?} {:?}\x1B[0m\n",
                color,
                tagged_word.tag,
                unsafe { tagged_word.word._debug_bytes }
            ));
        }
        out
    })? {}
    Ok((intepreter.root, intepreter.tree))
}
