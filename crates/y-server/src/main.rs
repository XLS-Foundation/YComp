mod shell;
mod wayland;
mod window;

use raw_window_handle::{HasDisplayHandle, HasWindowHandle, RawDisplayHandle};
use shell::Shell;
use smithay::backend::input::{ButtonState, KeyState};
use smithay::reexports::wayland_server::backend::ObjectId;
use std::env;
use std::time::{Duration, Instant};
use wayland::{ClientRequest, EDGE_BOTTOM, EDGE_LEFT, EDGE_RIGHT, EDGE_TOP, WaylandServer};
use window::{Restore, WindowStack};
use winit::application::ApplicationHandler;
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::ModifiersState;
use winit::platform::scancode::PhysicalKeyExtScancode;
use winit::window::{CursorIcon, Window, WindowId};
use y_desktop::TITLE_BAR_HEIGHT;
use y_gpu::{GpuContext, Renderer, Surface};

const SOCKET_NAME: &str = "wayland-y";
const BTN_LEFT: u32 = 0x110;
const BTN_RIGHT: u32 = 0x111;
const BTN_MIDDLE: u32 = 0x112;
const MIN_WINDOW_SIZE: f32 = 120.0;
const DOCK_SCROLL_STEP: f32 = 80.0;
const WHEEL_PIXELS_PER_NOTCH: f64 = 15.0;

#[derive(Clone, Copy)]
enum Grab {
    Move {
        window: u32,
        dx: f32,
        dy: f32,
    },
    Resize {
        window: u32,
        edges: u8,
        cursor: (f32, f32),
        size: (f32, f32),
    },
}

fn edge_cursor(edges: u8) -> &'static str {
    match edges {
        e if e == EDGE_LEFT || e == EDGE_RIGHT => "ew-resize",
        e if e == EDGE_TOP || e == EDGE_BOTTOM => "ns-resize",
        e if e == (EDGE_TOP | EDGE_LEFT) || e == (EDGE_BOTTOM | EDGE_RIGHT) => "nwse-resize",
        _ => "nesw-resize",
    }
}

fn icon_from_name(name: &str) -> CursorIcon {
    match name {
        "pointer" => CursorIcon::Pointer,
        "text" => CursorIcon::Text,
        "ew-resize" => CursorIcon::EwResize,
        "ns-resize" => CursorIcon::NsResize,
        "nesw-resize" => CursorIcon::NeswResize,
        "nwse-resize" => CursorIcon::NwseResize,
        "col-resize" => CursorIcon::ColResize,
        "row-resize" => CursorIcon::RowResize,
        "e-resize" => CursorIcon::EResize,
        "w-resize" => CursorIcon::WResize,
        "n-resize" => CursorIcon::NResize,
        "s-resize" => CursorIcon::SResize,
        "ne-resize" => CursorIcon::NeResize,
        "nw-resize" => CursorIcon::NwResize,
        "se-resize" => CursorIcon::SeResize,
        "sw-resize" => CursorIcon::SwResize,
        "move" => CursorIcon::Move,
        "grab" => CursorIcon::Grab,
        "grabbing" => CursorIcon::Grabbing,
        "crosshair" => CursorIcon::Crosshair,
        "wait" => CursorIcon::Wait,
        "progress" => CursorIcon::Progress,
        "not-allowed" => CursorIcon::NotAllowed,
        "help" => CursorIcon::Help,
        "all-scroll" => CursorIcon::AllScroll,
        _ => CursorIcon::Default,
    }
}

struct App {
    renderer: Option<Renderer>,
    surface: Option<Surface>,
    window: Option<Window>,
    gpu: GpuContext,
    display: RawDisplayHandle,
    stack: WindowStack,
    cursor: (f32, f32),
    grab: Option<Grab>,
    left_down: bool,
    modifiers: ModifiersState,
    wayland: WaylandServer,
    mapped: Vec<(ObjectId, u32)>,
    popup_windows: Vec<(ObjectId, u32)>,
    cursor_name: String,
    shell: Shell,
    focused_window: Option<u32>,
    started: Instant,
}

impl App {
    fn resize_to_window(&mut self) {
        if let (Some(renderer), Some(surface), Some(window)) =
            (&mut self.renderer, &self.surface, &self.window)
        {
            let size = window.inner_size();
            renderer
                .resize(surface, (size.width, size.height))
                .expect("failed to resize swapchain");
        }
    }

    fn screen_size(&self) -> (f32, f32) {
        match &self.renderer {
            Some(renderer) => (renderer.extent.width as f32, renderer.extent.height as f32),
            None => (800.0, 600.0),
        }
    }

    fn sync_output(&mut self) {
        let (width, height) = self.screen_size();
        self.wayland.set_output_size(width as i32, height as i32);

        let maximized: Vec<u32> = self
            .stack
            .windows
            .iter()
            .filter(|w| w.maximized)
            .map(|w| w.id)
            .collect();
        for id in maximized {
            if let Some(surface_id) = self.surface_for_window(id) {
                let has_title = self.stack.window(id).map_or(false, |w| w.has_title_bar);
                let title_offset = if has_title { TITLE_BAR_HEIGHT } else { 0.0 };
                self.wayland.configure_toplevel(
                    &surface_id,
                    Some((width as i32, (height - title_offset) as i32)),
                    None,
                    Some(true),
                );
            }
        }

        self.upload_scene();
    }

    fn upload_scene(&mut self) {
        let screen = self.screen_size();
        let (mut vertices, mut draws) = self.stack.scene();
        let focused = self.focused_window;
        let infos = self.stack.infos(focused);

        let (sv, sd) = self.shell.scene(&infos, screen);
        let offset = vertices.len() as u32;
        for mut d in sd {
            d.first_vertex += offset;
            draws.push(d);
        }
        vertices.extend(sv);

        if let Some(renderer) = &mut self.renderer {
            renderer
                .set_scene(&vertices, &draws)
                .expect("failed to upload scene");
        }
    }

    fn window_for_surface(&self, surface_id: &ObjectId) -> Option<u32> {
        self.mapped
            .iter()
            .chain(self.popup_windows.iter())
            .find(|(known, _)| known == surface_id)
            .map(|(_, id)| *id)
    }

    fn surface_for_window(&self, window_id: u32) -> Option<ObjectId> {
        self.mapped
            .iter()
            .chain(self.popup_windows.iter())
            .find(|(_, id)| *id == window_id)
            .map(|(surface_id, _)| surface_id.clone())
    }

    fn is_popup_window(&self, window_id: u32) -> bool {
        self.popup_windows.iter().any(|(_, id)| *id == window_id)
    }

    fn geometry_size(&self, window_id: u32) -> (f32, f32) {
        if let Some(surface_id) = self.surface_for_window(window_id) {
            if let Some((_, _, width, height)) = self.wayland.geometry_of(&surface_id) {
                return (width as f32, height as f32);
            }
        }
        self.stack.size_of(window_id).unwrap_or((0.0, 0.0))
    }

    fn current_surface(&self) -> (Option<ObjectId>, (f64, f64)) {
        if let Some(window_id) = self.stack.hit_test(self.cursor.0, self.cursor.1) {
            if let Some(surface_id) = self.surface_for_window(window_id) {
                if let Some((wx, wy)) = self.stack.position(window_id) {
                    return (Some(surface_id), (wx as f64, wy as f64));
                }
            }
        }
        (None, (0.0, 0.0))
    }

    fn update_pointer(&mut self) {
        if self.grab.is_some() {
            return;
        }
        let (surface_id, origin) = self.current_surface();
        if surface_id.is_none() {
            self.wayland.set_cursor_name("default");
        }
        self.wayland.pointer_motion(
            surface_id,
            origin,
            (self.cursor.0 as f64, self.cursor.1 as f64),
        );
    }

    fn release_pointer_focus(&mut self, cursor: &str) {
        self.wayland.pointer_motion(
            None,
            (0.0, 0.0),
            (self.cursor.0 as f64, self.cursor.1 as f64),
        );
        self.wayland.set_cursor_name(cursor);
    }

    fn focus_top(&mut self) {
        let top = self
            .stack
            .windows
            .iter()
            .rev()
            .find(|w| !w.minimized && self.mapped.iter().any(|(_, id)| *id == w.id))
            .map(|w| w.id);
        if let Some(id) = top {
            if let Some(surface_id) = self.surface_for_window(id) {
                self.wayland.set_focus(surface_id);
                self.focused_window = Some(id);
            }
        } else {
            self.focused_window = None;
        }
    }

    fn start_move_grab(&mut self, window: u32) -> bool {
        if self.grab.is_some() || self.stack.window(window).is_some_and(|w| w.maximized) {
            return false;
        }
        let Some((wx, wy)) = self.stack.position(window) else {
            return false;
        };
        self.grab = Some(Grab::Move {
            window,
            dx: self.cursor.0 - wx,
            dy: self.cursor.1 - wy,
        });
        self.release_pointer_focus("grabbing");
        true
    }

    fn start_resize_grab(&mut self, window: u32, edges: u8) -> bool {
        if edges == 0
            || self.grab.is_some()
            || self.stack.window(window).is_some_and(|w| w.maximized)
        {
            return false;
        }
        let size = self.geometry_size(window);
        self.grab = Some(Grab::Resize {
            window,
            edges,
            cursor: self.cursor,
            size,
        });
        if let Some(surface_id) = self.surface_for_window(window) {
            self.wayland
                .configure_toplevel(&surface_id, None, Some(true), None);
        }
        self.release_pointer_focus(edge_cursor(edges));
        true
    }

    fn end_grab(&mut self) {
        let grab = self.grab.take();
        if let Some(Grab::Resize { window, .. }) = grab {
            if let Some(surface_id) = self.surface_for_window(window) {
                self.wayland
                    .configure_toplevel(&surface_id, None, Some(false), None);
            }
        }
        self.update_pointer();
    }

    fn restore_from_dock(&mut self, id: u32) {
        if let Some(w) = self.stack.window_mut(id) {
            w.minimized = false;
        }
        self.stack.raise(id);
        if let Some(surface_id) = self.surface_for_window(id) {
            self.wayland.set_focus(surface_id);
        }
        self.focused_window = Some(id);
        self.upload_scene();
        self.update_pointer();
    }

    fn launch_terminal(&self) {
        let mut cmd = std::process::Command::new("ycomp-terminal");
        cmd.env("WAYLAND_DISPLAY", SOCKET_NAME);

        if let Ok(exe) = std::env::current_exe() {
            if let Some(dir) = exe.parent() {
                let path = dir.join("ycomp-terminal");
                if path.exists() {
                    cmd = std::process::Command::new(path);
                    cmd.env("WAYLAND_DISPLAY", SOCKET_NAME);
                }
            }
        }

        match cmd.spawn() {
            Ok(_) => println!("YComp: launched terminal"),
            Err(e) => println!("YComp: failed to launch terminal: {e}"),
        }
    }

    fn handle_request(&mut self, request: ClientRequest) -> bool {
        match request {
            ClientRequest::Move { surface } => {
                if !self.left_down {
                    return false;
                }
                let Some(window) = self.window_for_surface(&surface) else {
                    return false;
                };
                self.stack.raise(window);
                self.focused_window = Some(window);
                self.start_move_grab(window)
            }
            ClientRequest::Resize { surface, edges } => {
                if !self.left_down {
                    return false;
                }
                let Some(window) = self.window_for_surface(&surface) else {
                    return false;
                };
                self.stack.raise(window);
                self.focused_window = Some(window);
                self.start_resize_grab(window, edges)
            }
            ClientRequest::Maximize { surface, on } => {
                let Some(window) = self.window_for_surface(&surface) else {
                    return false;
                };
                let already = self.stack.window(window).is_some_and(|w| w.maximized);
                let (screen_w, screen_h) = self.screen_size();
                let has_title = self.stack.window(window).map_or(false, |w| w.has_title_bar);
                let title_offset = if has_title { TITLE_BAR_HEIGHT } else { 0.0 };

                if on && !already {
                    let (geometry_w, geometry_h) = self.geometry_size(window);
                    let (x, y) = self.stack.position(window).unwrap_or((0.0, 0.0));
                    if let Some(w) = self.stack.window_mut(window) {
                        w.restore = Some(Restore {
                            x,
                            y,
                            width: geometry_w as i32,
                            height: geometry_h as i32,
                        });
                        w.maximized = true;
                    }
                    self.stack.raise(window);
                    self.wayland.configure_toplevel(
                        &surface,
                        Some((screen_w as i32, (screen_h - title_offset) as i32)),
                        None,
                        Some(true),
                    );
                    self.stack.move_to(window, 0.0, title_offset);
                    self.focused_window = Some(window);
                    true
                } else if !on && already {
                    let restore = self.stack.window_mut(window).and_then(|w| {
                        w.maximized = false;
                        w.restore.take()
                    });
                    if let Some(r) = restore {
                        self.stack.move_to(window, r.x, r.y);
                    }
                    self.wayland.configure_toplevel(
                        &surface,
                        restore.map(|r| (r.width, r.height)),
                        None,
                        Some(false),
                    );
                    self.focused_window = Some(window);
                    true
                } else {
                    self.wayland.configure_toplevel(&surface, None, None, None);
                    false
                }
            }
            ClientRequest::Minimize { surface } => {
                if matches!(self.shell, Shell::None) {
                    return false;
                }
                let Some(window) = self.window_for_surface(&surface) else {
                    return false;
                };
                if self.is_popup_window(window) {
                    return false;
                }
                if let Some(w) = self.stack.window_mut(window) {
                    w.minimized = true;
                }
                self.stack.lower(window);
                if self.focused_window == Some(window) {
                    self.focused_window = None;
                }
                self.focus_top();
                true
            }
        }
    }

    fn pump_wayland(&mut self) {
        self.wayland
            .dispatch()
            .expect("failed to dispatch Wayland clients");

        let mut changed = false;

        for request in self.wayland.take_requests() {
            if self.handle_request(request) {
                changed = true;
            }
        }

        let alive = self.wayland.toplevel_ids();

        let mut gone = Vec::new();
        self.mapped.retain(|(surface_id, window_id)| {
            let keep = alive.contains(surface_id);
            if !keep {
                gone.push(*window_id);
            }
            keep
        });
        for window_id in gone {
            self.stack.remove(window_id);
            if self.focused_window == Some(window_id) {
                self.focused_window = None;
            }
            if matches!(
                self.grab,
                Some(Grab::Move { window, .. }) | Some(Grab::Resize { window, .. })
                    if window == window_id
            ) {
                self.grab = None;
            }
            println!("YComp: Wayland toplevel closed, window {window_id} removed");
            self.focus_top();
            changed = true;
        }

        for surface_id in alive {
            if self.mapped.iter().any(|(known, _)| *known == surface_id) {
                continue;
            }
            let n = self.mapped.len() as f32;
            let id = self.stack.add(
                80.0 + 40.0 * n,
                80.0 + 30.0 * n,
                400.0,
                300.0,
                [0.45, 0.45, 0.5],
            );
            self.stack.raise(id);
            self.wayland.set_focus(surface_id.clone());
            self.focused_window = Some(id);
            self.mapped.push((surface_id, id));
            println!("YComp: new Wayland toplevel mapped as window {id}");
            changed = true;
        }

        let popups = self.wayland.popup_infos();

        let mut popup_gone = Vec::new();
        self.popup_windows.retain(|(surface_id, window_id)| {
            let keep = popups.iter().any(|p| p.id == *surface_id);
            if !keep {
                popup_gone.push(*window_id);
            }
            keep
        });
        for window_id in popup_gone {
            self.stack.remove(window_id);
            changed = true;
        }

        for info in &popups {
            let Some(parent_window) = self.window_for_surface(&info.parent) else {
                continue;
            };
            let Some((parent_x, parent_y)) = self.stack.position(parent_window) else {
                continue;
            };
            let x = parent_x + info.offset.0 as f32;
            let y = parent_y + info.offset.1 as f32;

            let existing = self
                .popup_windows
                .iter()
                .find(|(known, _)| *known == info.id)
                .map(|(_, id)| *id);

            match existing {
                Some(window_id) => {
                    if self.stack.position(window_id) != Some((x, y)) {
                        self.stack.move_to(window_id, x, y);
                        changed = true;
                    }
                }
                None => {
                    let window_id = self.stack.add(x, y, 0.0, 0.0, [0.0, 0.0, 0.0]);
                    if let Some(w) = self.stack.window_mut(window_id) {
                        w.has_title_bar = false;
                    }
                    self.stack.raise(window_id);
                    self.popup_windows.push((info.id.clone(), window_id));
                    println!("YComp: new Wayland popup mapped as window {window_id}");
                    changed = true;
                }
            }
        }

        for (surface_id, pixels) in self.wayland.take_updates() {
            let Some(window_id) = self.window_for_surface(&surface_id) else {
                continue;
            };
            let old_size = self.stack.size_of(window_id);
            let Some(renderer) = self.renderer.as_mut() else {
                continue;
            };

            match self.stack.texture_of(window_id) {
                Some(texture) => renderer
                    .update_texture(texture, pixels.width, pixels.height, &pixels.rgba)
                    .expect("failed to update window texture"),
                None => {
                    let texture = renderer
                        .create_texture(pixels.width, pixels.height, &pixels.rgba)
                        .expect("failed to create window texture");
                    self.stack.set_texture(window_id, texture);
                    changed = true;
                }
            }

            if let Some((_, _, _, gh)) = self.wayland.geometry_of(&surface_id) {
                let gh = gh as u32;
                if gh < pixels.height && pixels.height - gh >= 20 {
                    self.wayland.mark_csd(&surface_id);
                }
            }

            if self
                .stack
                .set_body_size(window_id, pixels.width, pixels.height)
            {
                if let (Some(Grab::Resize { window, edges, .. }), Some((old_w, old_h))) =
                    (self.grab, old_size)
                {
                    if window == window_id {
                        if let Some((x, y)) = self.stack.position(window_id) {
                            let new_x = if edges & EDGE_LEFT != 0 {
                                x + old_w - pixels.width as f32
                            } else {
                                x
                            };
                            let new_y = if edges & EDGE_TOP != 0 {
                                y + old_h - pixels.height as f32
                            } else {
                                y
                            };
                            self.stack.move_to(window_id, new_x, new_y);
                        }
                    }
                }
                changed = true;
            }
        }

        let mut csd_changed = false;
        for (surface_id, window_id) in self.mapped.iter() {
            let csd = self.wayland.uses_csd(surface_id);
            if let Some(w) = self.stack.window_mut(*window_id) {
                let want = !csd;
                if w.has_title_bar != want {
                    w.has_title_bar = want;
                    csd_changed = true;
                }
            }
        }
        if csd_changed {
            changed = true;
        }

        let maximized: Vec<u32> = self
            .stack
            .windows
            .iter()
            .filter(|w| w.maximized)
            .map(|w| w.id)
            .collect();
        for id in maximized {
            let has_title = self.stack.window(id).map_or(false, |w| w.has_title_bar);
            let title_offset = if has_title { TITLE_BAR_HEIGHT } else { 0.0 };
            let (gx, gy) = match self
                .surface_for_window(id)
                .and_then(|surface_id| self.wayland.geometry_of(&surface_id))
            {
                Some((x, y, _, _)) => (x as f32, y as f32),
                None => (0.0, 0.0),
            };
            let target = (-gx, title_offset - gy);
            if self.stack.position(id) != Some(target) {
                self.stack.move_to(id, target.0, target.1);
                changed = true;
            }
        }

        if changed {
            self.upload_scene();
            self.update_pointer();
        }
    }

    fn apply_cursor(&mut self) {
        let name = self.wayland.cursor_name();
        if name == self.cursor_name {
            return;
        }
        self.cursor_name = name;
        if let Some(window) = &self.window {
            if self.cursor_name == "none" {
                window.set_cursor_visible(false);
            } else {
                window.set_cursor_visible(true);
                window.set_cursor(icon_from_name(&self.cursor_name));
            }
        }
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }

        let window = event_loop
            .create_window(Window::default_attributes().with_title("YComp"))
            .expect("failed to create window");

        let handle = window
            .window_handle()
            .expect("failed to get window handle")
            .as_raw();
        let surface = self
            .gpu
            .create_surface(self.display, handle)
            .expect("failed to create Vulkan surface");

        let gpu_index: usize = std::env::var("Y_GPU")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);

        let size = window.inner_size();
        let renderer = self
            .gpu
            .create_renderer(&surface, (size.width, size.height), gpu_index)
            .expect("failed to create renderer");

        println!(
            "YComp: renderer ready | GPU: {} | format {:?} | {}x{} | {} swapchain images",
            renderer.gpu_name,
            renderer.format,
            renderer.extent.width,
            renderer.extent.height,
            renderer.images.len(),
        );

        window.request_redraw();

        self.renderer = Some(renderer);
        self.surface = Some(surface);
        self.window = Some(window);
        self.sync_output();
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        self.pump_wayland();
        self.apply_cursor();
        event_loop.set_control_flow(ControlFlow::WaitUntil(
            Instant::now() + Duration::from_millis(8),
        ));
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => {
                println!("YComp: shutting down...");
                event_loop.exit();
            }
            WindowEvent::Resized(_) => {
                self.resize_to_window();
                self.sync_output();
            }
            WindowEvent::ModifiersChanged(mods) => {
                self.modifiers = mods.state();
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.cursor = (position.x as f32, position.y as f32);

                match self.grab {
                    Some(Grab::Move { window, dx, dy }) => {
                        self.stack
                            .move_to(window, self.cursor.0 - dx, self.cursor.1 - dy);
                        self.upload_scene();
                    }
                    Some(Grab::Resize {
                        window,
                        edges,
                        cursor,
                        size,
                    }) => {
                        let dx = self.cursor.0 - cursor.0;
                        let dy = self.cursor.1 - cursor.1;
                        let mut width = size.0;
                        let mut height = size.1;
                        if edges & EDGE_RIGHT != 0 {
                            width += dx;
                        }
                        if edges & EDGE_LEFT != 0 {
                            width -= dx;
                        }
                        if edges & EDGE_BOTTOM != 0 {
                            height += dy;
                        }
                        if edges & EDGE_TOP != 0 {
                            height -= dy;
                        }
                        let width = width.max(MIN_WINDOW_SIZE);
                        let height = height.max(MIN_WINDOW_SIZE);
                        if let Some(surface_id) = self.surface_for_window(window) {
                            self.wayland.configure_toplevel(
                                &surface_id,
                                Some((width as i32, height as i32)),
                                None,
                                None,
                            );
                        }
                    }
                    None => {
                        let infos = self.stack.infos(None);
                        let screen = self.screen_size();
                        if let Some((_, edges)) =
                            self.shell.edge_hit(self.cursor.0, self.cursor.1, &infos, screen)
                        {
                            self.release_pointer_focus(edge_cursor(edges));
                        } else {
                            self.update_pointer();
                        }
                    }
                }
            }
            WindowEvent::MouseInput { state, button, .. } => {
                let code = match button {
                    MouseButton::Left => BTN_LEFT,
                    MouseButton::Right => BTN_RIGHT,
                    MouseButton::Middle => BTN_MIDDLE,
                    _ => return,
                };
                let pressed = state == ElementState::Pressed;
                let is_left = button == MouseButton::Left;
                let btn_state = if pressed {
                    ButtonState::Pressed
                } else {
                    ButtonState::Released
                };
                if is_left {
                    self.left_down = pressed;
                }

                if !pressed {
                    if self.grab.is_some() {
                        self.end_grab();
                    } else {
                        self.update_pointer();
                        self.wayland.pointer_button(code, btn_state);
                    }
                    return;
                }

                let screen = self.screen_size();

                if is_left && self.grab.is_none() {
                    let infos = self.stack.infos(None);
                    if let Some((id, edges)) =
                        self.shell.edge_hit(self.cursor.0, self.cursor.1, &infos, screen)
                    {
                        self.stack.raise(id);
                        if let Some(surface_id) = self.surface_for_window(id) {
                            self.wayland.set_focus(surface_id);
                        }
                        self.focused_window = Some(id);
                        self.start_resize_grab(id, edges);
                        self.upload_scene();
                        return;
                    }
                }

                let hit = self.stack.hit_test(self.cursor.0, self.cursor.1);

                if is_left && hit.is_none() {
                    if self.shell.launcher_hit(self.cursor.0, self.cursor.1, screen) {
                        self.launch_terminal();
                        return;
                    }
                    let infos = self.stack.infos(None);
                    if let Some(id) =
                        self.shell.hit(self.cursor.0, self.cursor.1, &infos, screen)
                    {
                        self.restore_from_dock(id);
                        return;
                    }
                }

                let on_popup = hit.is_some_and(|id| self.is_popup_window(id));

                if !on_popup && self.wayland.has_popups() {
                    self.wayland.dismiss_popups();
                }

                if is_left && !on_popup {
                    let infos = self.stack.infos(None);
                    if let Some(hit) =
                        self.shell
                            .title_bar_hit(self.cursor.0, self.cursor.1, &infos, screen)
                    {
                        match hit {
                            y_desktop::TitleBarHit::Close(id) => {
                                if let Some(surface_id) = self.surface_for_window(id) {
                                    self.wayland.close_toplevel(&surface_id);
                                }
                            }
                            y_desktop::TitleBarHit::Minimize(id) => {
                                if let Some(surface_id) = self.surface_for_window(id) {
                                    if self.handle_request(ClientRequest::Minimize {
                                        surface: surface_id,
                                    }) {
                                        self.upload_scene();
                                        self.update_pointer();
                                    }
                                }
                            }
                            y_desktop::TitleBarHit::Maximize(id) => {
                                if let Some(surface_id) = self.surface_for_window(id) {
                                    let on = !self
                                        .stack
                                        .window(id)
                                        .map_or(false, |w| w.maximized);
                                    if self.handle_request(ClientRequest::Maximize {
                                        surface: surface_id,
                                        on,
                                    }) {
                                        self.upload_scene();
                                        self.update_pointer();
                                    }
                                }
                            }
                            y_desktop::TitleBarHit::Drag(id) => {
                                self.stack.raise(id);
                                if let Some(surface_id) = self.surface_for_window(id) {
                                    self.wayland.set_focus(surface_id);
                                }
                                self.focused_window = Some(id);
                                self.start_move_grab(id);
                                self.upload_scene();
                            }
                        }
                        return;
                    }
                }

                if self.modifiers.shift_key() && !on_popup {
                    if let Some(id) = hit {
                        self.stack.raise(id);
                        if let Some(surface_id) = self.surface_for_window(id) {
                            self.wayland.set_focus(surface_id);
                        }
                        self.focused_window = Some(id);
                        let started = match button {
                            MouseButton::Left => self.start_move_grab(id),
                            MouseButton::Right => {
                                self.start_resize_grab(id, EDGE_RIGHT | EDGE_BOTTOM)
                            }
                            _ => false,
                        };
                        self.upload_scene();
                        if started {
                            return;
                        }
                    }
                }

                if let Some(id) = hit {
                    if !on_popup {
                        self.stack.raise(id);
                        if let Some(surface_id) = self.surface_for_window(id) {
                            self.wayland.set_focus(surface_id);
                        }
                        self.focused_window = Some(id);
                    }
                    self.update_pointer();
                    self.wayland.pointer_button(code, btn_state);
                    self.upload_scene();
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                if self.grab.is_some() {
                    return;
                }
                let screen = self.screen_size();
                let (lines_x, lines_y, pixel_x, pixel_y, is_line) = match delta {
                    MouseScrollDelta::LineDelta(x, y) => (x as f64, y as f64, 0.0, 0.0, true),
                    MouseScrollDelta::PixelDelta(p) => (0.0, 0.0, p.x, p.y, false),
                };

                let infos = self.stack.infos(None);
                let dock_scrolled =
                    self.shell
                        .over(self.cursor.0, self.cursor.1, &infos, screen)
                        && self.shell.scrollable(&infos, screen);

                if dock_scrolled {
                    let amount = if is_line {
                        (-lines_y + lines_x) as f32 * DOCK_SCROLL_STEP
                    } else {
                        (-pixel_y + pixel_x) as f32
                    };
                    if self.shell.scroll(amount, &infos, screen) {
                        self.upload_scene();
                    }
                    return;
                }

                self.update_pointer();
                if is_line {
                    self.wayland.pointer_axis(
                        -lines_x * WHEEL_PIXELS_PER_NOTCH,
                        -lines_y * WHEEL_PIXELS_PER_NOTCH,
                        Some(((-lines_x * 120.0) as i32, (-lines_y * 120.0) as i32)),
                    );
                } else {
                    self.wayland.pointer_axis(-pixel_x, -pixel_y, None);
                }
            }
            WindowEvent::KeyboardInput { event, .. } => {
                if let winit::keyboard::PhysicalKey::Code(code) = event.physical_key {
                    if let Some(scancode) = code.to_scancode() {
                        let key_state = if event.state == ElementState::Pressed {
                            KeyState::Pressed
                        } else {
                            KeyState::Released
                        };
                        self.wayland.key_input(scancode, key_state);
                    }
                }
            }
            WindowEvent::RedrawRequested => {
                self.wayland
                    .send_frames(self.started.elapsed().as_millis() as u32);
                let background = self.shell.background();
                let stale = match &self.renderer {
                    Some(renderer) => renderer
                        .draw_frame(background)
                        .expect("failed to draw frame"),
                    None => false,
                };
                if stale {
                    self.resize_to_window();
                }
                if let Some(window) = &self.window {
                    window.request_redraw();
                }
            }
            _ => {}
        }
    }
}

fn main() -> y_gpu::Result<()> {
    unsafe {
        env::set_var("XDG_CURRENT_DESKTOP", "YComp");
        env::set_var("XDG_SESSION_TYPE", "wayland");
    }

    let event_loop = EventLoop::new()?;
    event_loop.set_control_flow(ControlFlow::Wait);

    let display = event_loop.display_handle()?.as_raw();
    let gpu = GpuContext::new(display)?;

    let gpus = gpu.list_gpus()?;
    println!("YComp: {} GPU(s) found", gpus.len());
    for (i, g) in gpus.iter().enumerate() {
        let (a, b, c) = g.vulkan_version;
        println!(
            "  [{}] {} ({:?}) - Vulkan {}.{}.{}",
            i, g.name, g.device_type, a, b, c
        );
    }

    let wayland = WaylandServer::new(SOCKET_NAME)?;
    println!("YComp: Wayland socket ready, use WAYLAND_DISPLAY={SOCKET_NAME}");

    let shell = Shell::from_env();

    let mut app = App {
        renderer: None,
        surface: None,
        window: None,
        gpu,
        display,
        stack: WindowStack::default(),
        cursor: (0.0, 0.0),
        grab: None,
        left_down: false,
        modifiers: ModifiersState::empty(),
        wayland,
        mapped: Vec::new(),
        popup_windows: Vec::new(),
        cursor_name: "default".to_string(),
        shell,
        focused_window: None,
        started: Instant::now(),
    };
    event_loop.run_app(&mut app)?;
    Ok(())
}
