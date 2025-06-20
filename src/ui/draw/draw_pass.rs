use std::time::Duration;
use std::{collections::HashMap, sync::Arc, usize};

use anyhow::{Result, anyhow};
use skia_safe::{Canvas, Color, Paint, Path, Rect};
use taffy::{NodeId, PrintTree, TaffyTree, TraversePartialTree};
use winit::window::{CursorIcon, Window};

use super::cursors::RaggedCursor;
use super::layout_pass::LayoutContext;
use super::text::draw_text;

use super::CarriedState;
use super::InputState;
use super::traits::{Executor, HasStaticConfig, Intepreter};
use super::utils::{StaticConfig, resolve_taffy_length};
use super::vm_state::VMState;

#[allow(dead_code)]
pub fn pos_exp_clamp(v: f32, f: f32, max: f32, k: f32, dt: Duration) -> f32 {
    debug_assert!(f > 0.0, "f must be strictly positive");
    debug_assert!(v > 0.0, "v must be strictly positive");
    debug_assert!(k > 1.0, "k must be strictly positive");

    if v < max {
        v + f
    } else {
        let delta = v - max;
        let o = v + f - k * delta * (dt.as_micros() as f32);
        let o = o.max(max + 1.0); // stop it from springing back into linear region
        o
    }
}

// :::::::-------- Third Pass, Draw ------ :::::
struct DrawIntepreter<'a, F>
where
    F: FnMut(usize) -> () + Clone,
{
    config: StaticConfig,
    state: &'a mut VMState,
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
        state: &'a mut VMState,
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
            state,
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

    fn get_node_ctx(&self) -> Result<&LayoutContext> {
        let ctx = self
            .tree
            .get_node_context(self.node)
            .ok_or(anyhow!("Expect nodes to have context"))?;
        Ok(ctx)
    }

    fn get_node_layout(&self) -> &taffy::Layout {
        self.tree.get_final_layout(self.node)
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
        /* We are handling scrolling here. */
        let desired_height = self.get_node_layout().size.height.max(
            self.get_node_ctx()?
                .maybe_font_layout
                .as_ref()
                .map(|x| x.height())
                .unwrap_or(0.0),
        );
        let window_size = self.window.inner_size();
        let window_height = window_size.height as f32;
        let mut state = self
            .frame_state
            .get(&self.cursor.cursor)
            .cloned()
            .unwrap_or(CarriedState::new());

        if desired_height > window_height {
            self.y += state.scroll_y;
            if self.is_hovered {
                // if self.input_state.scroll_action.1 < 0.0 && state.scroll_y <= 0.0 {
                //     state.scroll_y = -pos_exp_clamp(
                //         state.scroll_y.abs(),
                //         self.input_state.scroll_action.1.abs(),
                //         desired_height - window_height,
                //         0.005,
                //         self.config.get_dt(),
                //     );
                // } else if self.input_state.scroll_action.1 > 0.0 && state.scroll_y >= 0.0 {
                //     state.scroll_y = pos_exp_clamp(
                //         state.scroll_y.abs(),
                //         self.input_state.scroll_action.1.abs(),
                //         0.0,
                //         0.005,
                //         self.config.get_dt(),
                //     );
                // } else {
                //     state.scroll_y += self.input_state.scroll_action.1;
                // }
                // ^^^^ this implemnnts rubber banding around the edges and works but there is weird jumoy ness that comes from winit animations I think...

                state.scroll_y += self.input_state.scroll_action.1;
                state.scroll_y = state.scroll_y.clamp(-(desired_height - window_height), 0.0);
            }
        } else {
            state.scroll_y = 0.0;
        }
        self.next_frame_state.insert(self.cursor.cursor, state);

        Ok(())
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

    fn handle_rounded_rect(
        &mut self,
        x: taffy::LengthPercentage,
        y: taffy::LengthPercentage,
        width: taffy::LengthPercentageAuto,
        height: taffy::LengthPercentageAuto,
        r: taffy::LengthPercentageAuto,
    ) -> Result<()> {
        let layout = self.tree.get_final_layout(self.node);
        let x = self.x + resolve_taffy_length(x, layout.size.width);
        let y = self.y + resolve_taffy_length(y, layout.size.height);
        let width = resolve_taffy_length(width, layout.size.width);
        let height = resolve_taffy_length(height, layout.size.height);
        let r = resolve_taffy_length(
            r,
            if x > y {
                layout.size.width
            } else {
                layout.size.height
            },
        );

        let mut path = Path::new();
        path.move_to((x + r, y));
        path.line_to((x + width - r, y));
        path.arc_to_tangent((x + width, y), (x + width, y + height), r);
        path.line_to((x + width, y + height - r));
        path.arc_to_tangent((x + width, y + height), (x + width - r, y + height), r);
        path.line_to((x + r, y + height));
        path.arc_to_tangent((x, y + height), (x, y + height - r), r);
        path.line_to((x, y + r));
        path.arc_to_tangent((x, y), (x + r, y), r);
        path.close();
        self.canvas.draw_path(&path, &self.paint);
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

    fn handle_no_jmp(&mut self, _rel_ptr: usize) -> Result<()> {
        /* always falls through */
        Ok(())
    }

    fn handle_jmp(&mut self, rel_ptr: usize) -> Result<()> {
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
            self.config.display_scale(),
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
        let r = resolve_taffy_length(
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

pub(super) fn draw_pass<F>(
    window: Arc<Window>,
    canvas: &Canvas,
    px: f32,
    py: f32,
    vm_state: &mut VMState,
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
        vm_state,
        cb_push_evt.clone(),
        regions,
        frame_state,
        next_frame_state,
        input_state,
        config,
    )?;

    let mut trace = Vec::new();
    while let Some(_) = intepreter.advance(&mut trace)? {}

    for child in tree.child_ids(node) {
        draw_pass(
            window.clone(),
            canvas,
            x,
            y,
            vm_state,
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
