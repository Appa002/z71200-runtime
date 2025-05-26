pub mod assembler;
pub mod context;
pub mod debug;
pub mod draw;
pub mod renderer;

use anyhow::{Result, anyhow};
use context::VulkanRenderContext;
use draw::{CarriedState, draw};
use parley::{FontContext, LayoutContext};
use renderer::VulkanRenderer;
use skia_safe::{Color, Color4f, Font, FontMgr, FontStyle, Paint};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::{sync::mpsc::Receiver, task::JoinHandle};
use tracing::error;

use winit::{
    application::ApplicationHandler,
    dpi::PhysicalPosition,
    event::{ElementState, MouseButton, WindowEvent},
    event_loop::{ActiveEventLoop, EventLoop},
    window::{CursorIcon, Window},
};

#[derive(Default, Clone, Copy)]
pub struct InputState {
    cursor_pos: PhysicalPosition<f64>,
    mouse_down: bool,
    mouse_just_released: bool,
    scroll_action: (f32, f32),
}

// Used to render atleast n seconds of output before letting the loop go to sleep so that animation can be smooth
struct AnimationGuard {
    cur_target: Option<Duration>,
    elapsed_time: Duration,
}
impl AnimationGuard {
    fn new() -> Self {
        Self {
            cur_target: None,
            elapsed_time: Duration::from_secs(0),
        }
    }

    fn is_done(&mut self) -> bool {
        if let Some(cur_target) = self.cur_target {
            if cur_target <= self.elapsed_time {
                self.cur_target = None;
                self.elapsed_time = Duration::from_secs(0);
                return true;
            }
        }
        return false;
    }

    fn set(&mut self, target: Duration) {
        // only set my new target if this target is more time
        if let Some(cur_target) = self.cur_target {
            if (cur_target - self.elapsed_time) < target {
                self.cur_target = Some(target);
            }
        } else {
            self.cur_target = Some(target);
        }
    }

    fn update(&mut self, dt: Duration) {
        self.elapsed_time += dt;
    }
}

struct WGpuBackedApp<F>
where
    F: FnMut(usize) -> () + Clone,
{
    width: u32,
    height: u32,
    title: &'static str,
    vdoms: Arc<Mutex<(Option<usize>, Vec<usize>)>>,
    cb_push_evt: F,

    render_ctx: VulkanRenderContext,
    renderer: Option<VulkanRenderer>,

    font_context: FontContext,
    layout_context: LayoutContext<()>,

    input_state: InputState,
    last_fram_jmps: HashMap<*const u8, CarriedState>,
    rx: Option<Receiver<()>>,
    rx_task: Option<JoinHandle<()>>,

    animate_guard: AnimationGuard,
    last_frame_time: Instant,

    just_logged_error: bool, /* to avoid spam */
}

impl<F> WGpuBackedApp<F>
where
    F: FnMut(usize) -> () + Clone,
{
    fn new(
        width: u32,
        height: u32,
        title: &'static str,
        vdoms: Arc<Mutex<(Option<usize>, Vec<usize>)>>,
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
            animate_guard: AnimationGuard::new(),
            last_frame_time: std::time::Instant::now(),
            just_logged_error: false,
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

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
        let window = self.renderer.as_ref().unwrap().window.clone();
        if !self.animate_guard.is_done() {
            window.request_redraw();
        }
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
            WindowEvent::MouseWheel {
                device_id: _,
                delta,
                phase: _,
            } => {
                let (dx, dy) = match delta {
                    winit::event::MouseScrollDelta::LineDelta(lx, ly) => (lx * 12.0, ly * 12.0),
                    winit::event::MouseScrollDelta::PixelDelta(physical_position) => {
                        (physical_position.x as f32, physical_position.y as f32)
                    }
                };

                self.input_state.scroll_action = (dx, dy);
                self.animate_guard.set(Duration::from_secs(10));
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
                    let base_font_size = 16.0;

                    /* Window state resets */
                    window.set_cursor(CursorIcon::Default);
                    let dt = self.last_frame_time.elapsed();

                    /* User geometry */
                    renderer.draw_and_present(|canvas, size| {
                        canvas.clear(Color4f::new(0.95, 0.95, 0.95, 1.0));
                        /* Handle scaling */
                        canvas.save();
                        canvas.scale((1.0 / display_scale, 1.0 / display_scale));

                        let r: Result<HashMap<*const u8, CarriedState>> = {
                            let guard = self.vdoms.lock().unwrap();
                            let loc = guard.0;
                            let vdom = &guard.1;
                            if vdom.len() != 0 {
                                if let Some(loc) = loc {
                                    /*
                                     NOTE: empty vectors (specifically that haven't allocated) have a dangling data ptr which is returned from vdom.as_ptr(); that pointer is never aligned and points to garbage,
                                     so we can't do any draws with it.
                                    */
                                    let file_start = vdom.as_ptr() as *const u8;
                                    unsafe {
                                        let out = draw(
                                            loc,
                                            file_start,
                                            file_start.add(vdom.len() * size_of::<usize>()),
                                            size.width * display_scale,
                                            size.height * display_scale,
                                            canvas,
                                            window.clone(),
                                            self.cb_push_evt.clone(),
                                            &self.input_state,
                                            &mut self.font_context,
                                            &mut self.layout_context,
                                            display_scale,
                                            base_font_size,
                                            &self.last_fram_jmps,
                                            dt,
                                        );
                                        if out.is_ok() {
                                            self.just_logged_error = false;
                                        }
                                        out
                                    }
                                } else {
                                    Err(anyhow!("Location for ui not yet defined in memory."))
                                }
                            } else {
                                Err(anyhow!("Shared memory has not yet been read."))
                            }
                        };

                        match r {
                            Ok(jmps) => self.last_fram_jmps = jmps,
                            Err(err) => {
                                if !self.just_logged_error {
                                    error!("Error when generating frame. {:#}", err);
                                    self.just_logged_error = true;
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
                        canvas.restore();
                    });

                    // Just released is only for that frame.
                    if self.input_state.mouse_just_released {
                        window.request_redraw();
                    }
                    self.input_state.mouse_just_released = false;
                    self.input_state.scroll_action = (0.0, 0.0);

                    self.animate_guard.update(dt);

                    self.last_frame_time = std::time::Instant::now();
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
    vdoms: Arc<Mutex<(Option<usize>, Vec<usize>)>>,
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
