pub mod assembler;
pub mod context;
pub mod debug;
pub mod draw;
pub mod renderer;
pub mod text;

use anyhow::{Result, anyhow};
use assembler::assemble;
use context::VulkanRenderContext;
use debug::debug_print_layout;
use draw::draw;
use lazy_static::lazy_static;
use parley::{FontContext, LayoutContext};
use renderer::VulkanRenderer;
use skia_safe::{Color, Color4f, Font, FontMgr, FontStyle, Paint};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};
use tokio::{sync::mpsc::Receiver, task::JoinHandle};
use tracing::error;

// use femtovg::renderer::WGPURenderer::wgpu::{BackendOptions, InstanceFlags};
use winit::{
    application::ApplicationHandler,
    dpi::PhysicalPosition,
    event::{ElementState, MouseButton, WindowEvent},
    event_loop::{ActiveEventLoop, EventLoop},
    window::{CursorIcon, Window},
};

lazy_static! {
    pub static ref LIBRARY: HashMap<usize, Vec<u8>> = {
        let mut m = HashMap::new();
        m.insert(
            0,
            assemble(include_str!("lib/0_rounded_rect.txt")).expect("couldn't assemble std lib"),
        );
        m.insert(
            1,
            assemble(include_str!("lib/1_button_primary.txt")).expect("couldn't assemble std lib"),
        );
        m
    };
}

#[derive(Default)]
pub struct InputState {
    cursor_pos: PhysicalPosition<f64>,
    mouse_down: bool,
    mouse_just_released: bool,
}

// trait StoredFnMut: FnMut(usize) -> () + Clone {}

struct WGpuBackedApp<F>
where
    F: FnMut(usize) -> () + Clone,
{
    width: u32,
    height: u32,
    title: &'static str,
    vdoms: Arc<Mutex<(Option<usize>, Vec<u8>)>>,
    cb_push_evt: F,

    render_ctx: VulkanRenderContext,
    renderer: Option<VulkanRenderer>,

    font_context: FontContext,
    layout_context: LayoutContext<()>,

    input_state: InputState,
    last_fram_jmps: HashMap<*const u8, bool>,
    rx: Option<Receiver<()>>,
    rx_task: Option<JoinHandle<()>>,
}

impl<F> WGpuBackedApp<F>
where
    F: FnMut(usize) -> () + Clone,
{
    fn new(
        width: u32,
        height: u32,
        title: &'static str,
        vdoms: Arc<Mutex<(Option<usize>, Vec<u8>)>>,
        cb_push_evt: F,
        rx: Receiver<()>,
    ) -> Self {
        let font_context = FontContext::new();

        WGpuBackedApp {
            width,
            height,
            title,
            vdoms,
            cb_push_evt,
            render_ctx: VulkanRenderContext::default(),
            renderer: None,
            font_context,
            layout_context: LayoutContext::new(),
            input_state: InputState::default(),
            rx: Some(rx),
            rx_task: None,
            last_fram_jmps: HashMap::new(),
        }
    }
}

impl<F> ApplicationHandler for WGpuBackedApp<F>
where
    F: FnMut(usize) -> () + Clone,
{
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        let window = Arc::new(
            event_loop
                .create_window(
                    Window::default_attributes()
                        .with_title(self.title)
                        .with_inner_size(winit::dpi::PhysicalSize::new(self.width, self.height))
                        .with_resizable(true),
                )
                .unwrap(),
        );
        self.renderer = Some(
            self.render_ctx
                .renderer_for_window(event_loop, window.clone()),
        ); /* the example mentions that this is particular for apps with a single window */

        //
        let mut rx = self.rx.take().unwrap();
        let window_1 = window.clone();
        let j = tokio::spawn(async move {
            loop {
                if let Some(_) = rx.recv().await {
                    window_1.request_redraw();
                }
            }
        });
        self.rx_task = Some(j);
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        _window_id: winit::window::WindowId,
        event: winit::event::WindowEvent,
    ) {
        let window = self.renderer.as_ref().unwrap().window.clone();

        match event {
            WindowEvent::Resized(_) => {
                if let Some(renderer) = self.renderer.as_mut() {
                    renderer.invalidate_swapchain();
                };
                window.request_redraw();
            }
            WindowEvent::CursorMoved {
                device_id: _,
                position,
            } => {
                self.input_state.cursor_pos = position;
                window.request_redraw();
            }
            WindowEvent::MouseInput {
                device_id: _,
                state,
                button,
            } => {
                if state == ElementState::Pressed && button == MouseButton::Left {
                    self.input_state.mouse_down = true;
                } else {
                    self.input_state.mouse_down = false;
                }

                if state == ElementState::Released && button == MouseButton::Left {
                    self.input_state.mouse_just_released = true;
                }

                window.request_redraw();
            }
            WindowEvent::CloseRequested => {
                println!("The close button was pressed; stopping");
                if let Some(j) = self.rx_task.as_ref() {
                    j.abort();
                }
                event_loop.exit();
            }
            WindowEvent::RedrawRequested => {
                if let Some(renderer) = self.renderer.as_mut() {
                    renderer.prepare_swapchain();

                    let display_scale = window.scale_factor() as f32;
                    let base_font_size = 14.0;

                    /* Window state resets */
                    window.set_cursor(CursorIcon::Default);

                    /* User geometry */

                    renderer.draw_and_present(|canvas, size| {
                        canvas.clear(Color4f::new(0.9, 0.9, 0.9, 1.0));

                        let r: Result<HashMap<*const u8, bool>> = {
                            let guard = self.vdoms.lock().unwrap();
                            let loc = guard.0;
                            let vdom = &guard.1;

                            if let Some(loc) = loc {
                                let file_start = vdom.as_ptr();

                                // error!(
                                //     "waaa {}",
                                //     debug_print_layout(*loc, file_start, &LIBRARY).unwrap()
                                // );

                                unsafe {
                                    draw(
                                        loc,
                                        file_start,
                                        file_start.add(vdom.len()),
                                        size.width,
                                        size.height,
                                        canvas,
                                        window.clone(),
                                        self.cb_push_evt.clone(),
                                        &self.input_state,
                                        &mut self.font_context,
                                        &mut self.layout_context,
                                        display_scale,
                                        base_font_size,
                                        &LIBRARY,
                                        &self.last_fram_jmps,
                                    )
                                }
                            } else {
                                Err(anyhow!("Location for ui not yet defined in memory."))
                            }
                        };

                        match r {
                            Ok(jmps) => self.last_fram_jmps = jmps,
                            Err(err) => {
                                error!("Error when generating frame. {:#}", err);
                                let guard = self.vdoms.lock().unwrap();
                                let loc = guard.0;
                                let vdom = &guard.1;

                                if let Some(loc) = loc {
                                    let file_start = vdom.as_ptr();
                                    error!(
                                        "{}",
                                        debug_print_layout(loc, file_start, &LIBRARY).unwrap()
                                    );
                                }

                                let fmgr = FontMgr::default();
                                let typeface = fmgr
                                    .match_family_style("Arial", FontStyle::normal())
                                    .unwrap();
                                let font = Font::new(typeface, 13.0);

                                let mut paint = Paint::default();
                                paint.set_color(Color::from_rgb(255, 0, 255));
                                paint.set_anti_alias(true);

                                let err_str = format!("{:#}", err);
                                canvas.draw_str(err_str, (10.0, 30.0), &font, &paint);
                            }
                        }
                    });

                    // Just released is only for that frame.
                    if self.input_state.mouse_just_released {
                        window.request_redraw();
                    }
                    self.input_state.mouse_just_released = false;
                }
            }
            _ => (),
        }
    }
}

pub fn start<F>(
    width: u32,
    height: u32,
    title: &'static str,
    vdoms: Arc<Mutex<(Option<usize>, Vec<u8>)>>,
    cb_push_evt: F,
    rx: Receiver<()>,
) where
    F: FnMut(usize) -> () + Clone + Send + Sync + 'static,
{
    let event_loop = EventLoop::new().unwrap();
    event_loop.set_control_flow(winit::event_loop::ControlFlow::Wait);

    let mut app = WGpuBackedApp::new(width, height, title, vdoms, cb_push_evt, rx);
    event_loop.run_app(&mut app).unwrap();
}
