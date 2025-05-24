use anyhow::{Result, anyhow};
use parley::FontContext;
use taffy::{NodeId, PrintTree, TaffyTree, TraversePartialTree};

use super::cursors::RaggedCursor;
use super::layout_pass::LayoutContext;
use super::text::layout_text;

use super::StoredAlignment;
use super::traits::{Executor, HasStaticConfig, Intepreter};
use super::utils::StaticConfig;
use super::vm_state::VMState;

// ::: ---- Second Pass, Layout Text ----:::

pub(super) struct TextLayoutIntepreter<'a> {
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

    #[allow(dead_code)]
    fn state(&self) -> &VMState {
        &self.state
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
pub(super) fn text_pass(
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

    let mut trace = Vec::new();
    while let Some(_) = intepreter.advance(&mut trace)? {}

    let children: Vec<_> = tree.child_ids(node).collect();
    for child in children {
        text_pass(tree, child, font_context, layout_context, config)?;
    }
    Ok(())
}
