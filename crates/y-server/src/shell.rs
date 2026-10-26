use y_desktop::{Desktop, TitleBarHit, WindowInfo};
use y_gpu::{DrawCall, Vertex};

pub enum Shell {
    None,
    Minimal(Desktop),
}

impl Shell {
    pub fn from_env() -> Self {
        let mode = std::env::var("YCOMP_SHELL").unwrap_or_else(|_| "auto".to_string());
        match mode.as_str() {
            "none" | "off" | "0" => {
                println!("YComp: shell disabled (YCOMP_SHELL={mode})");
                Shell::None
            }
            "minimal" => {
                println!("YComp: minimal shell (y-desktop) enabled");
                Shell::Minimal(Desktop::new())
            }
            _ => {
                Shell::Minimal(Desktop::new())
            }
        }
    }

    pub fn launcher_hit(&self, px: f32, py: f32, screen: (f32, f32)) -> bool {
        match self {
            Shell::None => false,
            Shell::Minimal(d) => d.launcher_hit(px, py, screen),
        }
    }

    pub fn background(&self) -> [f32; 4] {
        match self {
            Shell::None => [0.0, 0.0, 0.0, 1.0],
            Shell::Minimal(_) => [0.08, 0.05, 0.18, 1.0],
        }
    }

    pub fn scene(
        &self,
        windows: &[WindowInfo],
        screen: (f32, f32),
    ) -> (Vec<Vertex>, Vec<DrawCall>) {
        match self {
            Shell::None => (Vec::new(), Vec::new()),
            Shell::Minimal(d) => d.scene(windows, screen),
        }
    }

    pub fn hit(
        &self,
        px: f32,
        py: f32,
        windows: &[WindowInfo],
        screen: (f32, f32),
    ) -> Option<u32> {
        match self {
            Shell::None => None,
            Shell::Minimal(d) => d.hit(px, py, windows, screen),
        }
    }

    pub fn edge_hit(
        &self,
        px: f32,
        py: f32,
        windows: &[WindowInfo],
        screen: (f32, f32),
    ) -> Option<(u32, u8)> {
        match self {
            Shell::None => None,
            Shell::Minimal(d) => d.edge_hit(px, py, windows, screen),
        }
    }

    pub fn title_bar_hit(
        &self,
        px: f32,
        py: f32,
        windows: &[WindowInfo],
        screen: (f32, f32),
    ) -> Option<TitleBarHit> {
        match self {
            Shell::None => None,
            Shell::Minimal(d) => d.title_bar_hit(px, py, windows, screen),
        }
    }

    pub fn over(
        &self,
        px: f32,
        py: f32,
        windows: &[WindowInfo],
        screen: (f32, f32),
    ) -> bool {
        match self {
            Shell::None => false,
            Shell::Minimal(d) => d.over(px, py, windows, screen),
        }
    }

    pub fn scroll(
        &mut self,
        delta: f32,
        windows: &[WindowInfo],
        screen: (f32, f32),
    ) -> bool {
        match self {
            Shell::None => false,
            Shell::Minimal(d) => d.scroll(delta, windows, screen),
        }
    }

    pub fn scrollable(&self, windows: &[WindowInfo], screen: (f32, f32)) -> bool {
        match self {
            Shell::None => false,
            Shell::Minimal(d) => d.scrollable(windows, screen),
        }
    }
}
