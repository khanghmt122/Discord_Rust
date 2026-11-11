//! Antigravity-style animated dynamic background themes and desktop wallpaper sync state.

use egui::{Color32, FontId, Mesh, Painter, Pos2, Rect, Stroke, Vec2};
use std::cell::RefCell;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
#[repr(u8)]
pub enum LiveBackgroundMode {
	#[default]
	Off = 0,
	/// Auto-syncs with desktop wallpaper / live iWallpaper video on host OS.
	DesktopSync = 1,
	/// Plays a user-selected local video (.mp4/.mov), animated GIF/WebP, or image.
	CustomMedia = 2,
	Aurora = 3,
	Starfield = 4,
	Synthwave = 5,
	Neural = 6,
	DeepOcean = 7,
	CyberMatrix = 8,
}

impl LiveBackgroundMode {
	pub fn from_u8(val: u8) -> Self {
		match val {
			1 => Self::DesktopSync,
			2 => Self::CustomMedia,
			3 => Self::Aurora,
			4 => Self::Starfield,
			5 => Self::Synthwave,
			6 => Self::Neural,
			7 => Self::DeepOcean,
			8 => Self::CyberMatrix,
			_ => Self::Off,
		}
	}

	pub fn to_u8(self) -> u8 {
		self as u8
	}

	pub fn is_ambient_shader(self) -> bool {
		matches!(
			self,
			Self::Aurora
				| Self::Starfield
				| Self::Synthwave
				| Self::Neural
				| Self::DeepOcean
				| Self::CyberMatrix
		)
	}

	pub fn is_media_backed(self) -> bool {
		matches!(self, Self::DesktopSync | Self::CustomMedia)
	}

	pub fn label(self) -> &'static str {
		match self {
			Self::Off => "Tắt",
			Self::DesktopSync => "🖥 Sync Màn Hình",
			Self::CustomMedia => "🎞 Video / GIF",
			Self::Aurora => "🌈 Aurora Sóng",
			Self::Starfield => "🌌 Starfield",
			Self::Synthwave => "🌅 Synthwave 80s",
			Self::Neural => "⚡ Neural Network",
			Self::DeepOcean => "🌊 Deep Ocean",
			Self::CyberMatrix => "💻 Cyber Matrix",
		}
	}
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LiveBackgroundConfig {
	pub mode: u8,
	/// Frosted-glass UI panel opacity (%) so the live background shows through clearly.
	pub panel_opacity: u8,
	/// Background video / ambient shader opacity (%).
	pub bg_opacity: u8,
	/// Animation & playback speed in percent (100 = 1.0x).
	pub speed_percent: u16,
	/// Selected file path when mode == CustomMedia.
	pub custom_path: Option<String>,
}

impl Default for LiveBackgroundConfig {
	fn default() -> Self {
		Self {
			mode: 0,
			panel_opacity: 72,
			bg_opacity: 92,
			speed_percent: 100,
			custom_path: None,
		}
	}
}

struct SimpleRng(u64);

impl SimpleRng {
	fn new(seed: u64) -> Self {
		Self(if seed == 0 {
			0x9e37_79b9_7f4a_7c15
		} else {
			seed
		})
	}

	fn next_u64(&mut self) -> u64 {
		self.0 ^= self.0 << 13;
		self.0 ^= self.0 >> 7;
		self.0 ^= self.0 << 17;
		self.0
	}

	fn next_f32(&mut self) -> f32 {
		(self.next_u64() & 0x00ff_ffff) as f32 / 16_777_216.0
	}

	fn range(&mut self, min: f32, max: f32) -> f32 {
		min + (max - min) * self.next_f32()
	}
}

#[derive(Clone, Copy)]
struct Star {
	x: f32,
	y: f32,
	radius: f32,
	alpha: f32,
	speed: f32,
}

#[derive(Clone, Copy)]
struct NeuralNode {
	x: f32,
	y: f32,
	vx: f32,
	vy: f32,
	radius: f32,
}

#[derive(Clone, Copy)]
struct MatrixDrop {
	y: f32,
	speed: f32,
	seed: u64,
}

#[derive(Default)]
struct AmbientState {
	stars: Vec<Star>,
	nodes: Vec<NeuralNode>,
	matrix: Vec<MatrixDrop>,
	last_time: f64,
}

thread_local! {
	static AMBIENT: RefCell<AmbientState> = RefCell::new(AmbientState::default());
}

fn scaled_rgba(r: u8, g: u8, b: u8, alpha_f: f32, master: f32) -> Color32 {
	let a = (alpha_f * master).clamp(0.0, 1.0);
	let a8 = (a * 255.0) as u8;
	Color32::from_rgba_premultiplied(
		(f32::from(r) * a) as u8,
		(f32::from(g) * a) as u8,
		(f32::from(b) * a) as u8,
		a8,
	)
}

fn paint_vertical_gradient(
	painter: &Painter,
	rect: Rect,
	top: Color32,
	mid: Color32,
	bot: Color32,
) {
	let mut mesh = Mesh::default();
	let mid_y = rect.center().y;
	mesh.colored_vertex(rect.left_top(), top);
	mesh.colored_vertex(rect.right_top(), top);
	mesh.colored_vertex(Pos2::new(rect.left(), mid_y), mid);
	mesh.colored_vertex(Pos2::new(rect.right(), mid_y), mid);
	mesh.colored_vertex(rect.left_bottom(), bot);
	mesh.colored_vertex(rect.right_bottom(), bot);
	mesh.add_triangle(0, 1, 2);
	mesh.add_triangle(1, 3, 2);
	mesh.add_triangle(2, 3, 4);
	mesh.add_triangle(3, 5, 4);
	painter.add(mesh);
}

fn paint_radial_glow(painter: &Painter, center: Pos2, radius: f32, core: Color32) {
	if radius <= 1.0 || core.a() == 0 {
		return;
	}
	let mut mesh = Mesh::default();
	mesh.colored_vertex(center, core);
	let segments = 28_u32;
	let mid_color =
		Color32::from_rgba_premultiplied(core.r() / 3, core.g() / 3, core.b() / 3, core.a() / 3);
	for i in 0..segments {
		let angle = (i as f32 / segments as f32) * std::f32::consts::TAU;
		let mid_p = center + Vec2::new(angle.cos(), angle.sin()) * (radius * 0.52);
		mesh.colored_vertex(mid_p, mid_color);
	}
	for i in 0..segments {
		let angle = (i as f32 / segments as f32) * std::f32::consts::TAU;
		let outer_p = center + Vec2::new(angle.cos(), angle.sin()) * radius;
		mesh.colored_vertex(outer_p, Color32::TRANSPARENT);
	}
	for i in 0..segments {
		let next = (i + 1) % segments;
		let m0 = 1 + i;
		let m1 = 1 + next;
		let o0 = 1 + segments + i;
		let o1 = 1 + segments + next;
		mesh.add_triangle(0, m0, m1);
		mesh.add_triangle(m0, o0, m1);
		mesh.add_triangle(m1, o0, o1);
	}
	painter.add(mesh);
}

/// Paints the active Antigravity ambient shader theme onto the background layer and schedules
/// smooth 60fps updates while the window is visible.
pub fn paint_ambient_theme(
	ctx: &egui::Context,
	rect: Rect,
	mode: LiveBackgroundMode,
	bg_opacity: u8,
	speed_percent: u16,
) {
	if !mode.is_ambient_shader() || rect.width() < 8.0 || rect.height() < 8.0 || bg_opacity == 0 {
		return;
	}
	let minimized = ctx.input(|i| i.viewport().minimized == Some(true));
	if !minimized {
		ctx.request_repaint_after(std::time::Duration::from_millis(16));
	}

	let speed = (f32::from(speed_percent) / 100.0).clamp(0.1, 5.0);
	let master = (f32::from(bg_opacity) / 100.0).clamp(0.0, 1.0);
	let raw_time = ctx.input(|i| i.time);
	let painter = ctx
		.layer_painter(egui::LayerId::background())
		.with_clip_rect(rect);

	AMBIENT.with(|cell| {
		let mut st = cell.borrow_mut();
		let dt = if st.last_time == 0.0 || minimized {
			0.016
		} else {
			((raw_time - st.last_time) as f32).clamp(0.001, 0.05)
		};
		st.last_time = raw_time;
		let t = (raw_time as f32) * speed;

		match mode {
			LiveBackgroundMode::Aurora => paint_aurora(&painter, rect, t, master),
			LiveBackgroundMode::Starfield => {
				paint_starfield(&painter, rect, &mut st.stars, t, dt * speed, master);
			}
			LiveBackgroundMode::Synthwave => paint_synthwave(&painter, rect, t, master),
			LiveBackgroundMode::Neural => {
				paint_neural(&painter, rect, &mut st.nodes, dt * speed, master);
			}
			LiveBackgroundMode::DeepOcean => paint_ocean(&painter, rect, t, master),
			LiveBackgroundMode::CyberMatrix => {
				paint_matrix(&painter, rect, &mut st.matrix, t, dt * speed, master);
			}
			_ => {}
		}
	});
}

fn paint_aurora(painter: &Painter, rect: Rect, t: f32, master: f32) {
	paint_vertical_gradient(
		painter,
		rect,
		scaled_rgba(6, 7, 12, 1.0, master),
		scaled_rgba(10, 13, 24, 1.0, master),
		scaled_rgba(5, 6, 10, 1.0, master),
	);
	let w = rect.width();
	let h = rect.height();
	let c1 = Pos2::new(
		rect.left() + w * (0.35 + (t * 0.4).sin() * 0.2),
		rect.top() + h * (0.32 + (t * 0.35).cos() * 0.18),
	);
	paint_radial_glow(
		painter,
		c1,
		w.max(h) * 0.55,
		scaled_rgba(16, 185, 129, 0.55, master),
	);

	let c2 = Pos2::new(
		rect.left() + w * (0.72 + (t * 0.5).cos() * 0.2),
		rect.top() + h * (0.65 + (t * 0.4).sin() * 0.18),
	);
	paint_radial_glow(
		painter,
		c2,
		w.max(h) * 0.58,
		scaled_rgba(147, 51, 234, 0.52, master),
	);

	let c3 = Pos2::new(
		rect.left() + w * (0.5 + (t * 0.28).sin() * 0.25),
		rect.top() + h * (0.48 + (t * 0.42).sin() * 0.2),
	);
	paint_radial_glow(
		painter,
		c3,
		w.max(h) * 0.44,
		scaled_rgba(56, 189, 248, 0.35, master),
	);
}

fn paint_starfield(
	painter: &Painter,
	rect: Rect,
	stars: &mut Vec<Star>,
	t: f32,
	dt: f32,
	master: f32,
) {
	painter.rect_filled(rect, 0.0, scaled_rgba(5, 6, 11, 1.0, master));
	let w = rect.width();
	let h = rect.height();
	// Subtle cosmic nebula tint matching Antigravity starfield
	paint_radial_glow(
		painter,
		Pos2::new(rect.left() + w * 0.25, rect.top() + h * 0.3),
		w.max(h) * 0.45,
		scaled_rgba(59, 130, 246, 0.16, master),
	);
	paint_radial_glow(
		painter,
		Pos2::new(rect.left() + w * 0.78, rect.top() + h * 0.7),
		w.max(h) * 0.42,
		scaled_rgba(168, 85, 247, 0.14, master),
	);

	let mut rng = SimpleRng::new(0x57a9_f1e1);
	if stars.is_empty() {
		for _ in 0..160 {
			stars.push(Star {
				x: rng.range(0.0, w.max(100.0)),
				y: rng.range(0.0, h.max(100.0)),
				radius: rng.range(0.6, 2.1),
				alpha: rng.range(0.25, 0.95),
				speed: rng.range(12.0, 42.0),
			});
		}
	}
	for s in stars.iter_mut() {
		s.y -= s.speed * dt;
		if s.y < 0.0 {
			s.y = h;
			s.x = rng.range(0.0, w);
		}
		if s.x > w {
			s.x = rng.range(0.0, w);
		}
		let twinkle = 0.7 + 0.3 * (t * 3.0 + s.x * 0.05).sin();
		let p = Pos2::new(rect.left() + s.x, rect.top() + s.y);
		painter.circle_filled(
			p,
			s.radius,
			scaled_rgba(240, 248, 255, s.alpha * twinkle, master),
		);
	}
}

fn paint_synthwave(painter: &Painter, rect: Rect, t: f32, master: f32) {
	let w = rect.width();
	let h = rect.height();
	let horizon_y = rect.top() + h * 0.64;
	let sky_rect = Rect::from_min_max(rect.min, Pos2::new(rect.right(), horizon_y));
	paint_vertical_gradient(
		painter,
		sky_rect,
		scaled_rgba(12, 7, 33, 1.0, master),
		scaled_rgba(59, 13, 79, 1.0, master),
		scaled_rgba(255, 56, 100, 0.92, master),
	);

	// Sun
	let sun_center = Pos2::new(rect.left() + w * 0.5, rect.top() + h * 0.44);
	let sun_r = w.min(h) * 0.18;
	paint_radial_glow(
		painter,
		sun_center,
		sun_r * 1.9,
		scaled_rgba(255, 70, 130, 0.45, master),
	);
	painter.circle_filled(sun_center, sun_r, scaled_rgba(255, 200, 105, 0.95, master));

	// Dark ground
	let ground_rect = Rect::from_min_max(Pos2::new(rect.left(), horizon_y), rect.max);
	painter.rect_filled(ground_rect, 0.0, scaled_rgba(10, 3, 20, 1.0, master));

	// Scrolling horizontal perspective grid
	let ground_h = (rect.bottom() - horizon_y).max(1.0);
	let grid_color = scaled_rgba(255, 0, 140, 0.42, master);
	let stroke = Stroke::new(1.3, grid_color);
	let phase = (t * 0.45).fract();
	for i in 0..14 {
		let p = ((i as f32 + phase) / 14.0).powf(2.2);
		let y = horizon_y + p * ground_h;
		painter.line_segment(
			[Pos2::new(rect.left(), y), Pos2::new(rect.right(), y)],
			stroke,
		);
	}
	// Perspective rays from horizon center
	for k in -6..=6 {
		let top_x = rect.center().x + k as f32 * (w * 0.045);
		let bot_x = rect.center().x + k as f32 * (w * 0.22);
		painter.line_segment(
			[Pos2::new(top_x, horizon_y), Pos2::new(bot_x, rect.bottom())],
			stroke,
		);
	}
}

fn paint_neural(painter: &Painter, rect: Rect, nodes: &mut Vec<NeuralNode>, dt: f32, master: f32) {
	painter.rect_filled(rect, 0.0, scaled_rgba(8, 10, 16, 1.0, master));
	let w = rect.width();
	let h = rect.height();
	if nodes.is_empty() {
		let mut rng = SimpleRng::new(0x1337_beef);
		for _ in 0..64 {
			nodes.push(NeuralNode {
				x: rng.range(0.0, w.max(100.0)),
				y: rng.range(0.0, h.max(100.0)),
				vx: rng.range(-28.0, 28.0),
				vy: rng.range(-28.0, 28.0),
				radius: rng.range(1.6, 3.2),
			});
		}
	}
	for n in nodes.iter_mut() {
		n.x += n.vx * dt;
		n.y += n.vy * dt;
		if n.x < 0.0 {
			n.x = 0.0;
			n.vx = n.vx.abs();
		} else if n.x > w {
			n.x = w;
			n.vx = -n.vx.abs();
		}
		if n.y < 0.0 {
			n.y = 0.0;
			n.vy = n.vy.abs();
		} else if n.y > h {
			n.y = h;
			n.vy = -n.vy.abs();
		}
	}
	let max_dist = 140.0_f32;
	let max_dist_sq = max_dist * max_dist;
	for (i, &a) in nodes.iter().enumerate() {
		let p1 = Pos2::new(rect.left() + a.x, rect.top() + a.y);
		for &b in &nodes[(i + 1)..] {
			let dx = a.x - b.x;
			let dy = a.y - b.y;
			let d2 = dx * dx + dy * dy;
			if d2 < max_dist_sq {
				let alpha = (1.0 - d2.sqrt() / max_dist) * 0.45;
				let p2 = Pos2::new(rect.left() + b.x, rect.top() + b.y);
				painter.line_segment(
					[p1, p2],
					Stroke::new(1.0, scaled_rgba(56, 189, 248, alpha, master)),
				);
			}
		}
		painter.circle_filled(p1, a.radius, scaled_rgba(56, 189, 248, 0.9, master));
	}
}

fn paint_ocean(painter: &Painter, rect: Rect, t: f32, master: f32) {
	paint_vertical_gradient(
		painter,
		rect,
		scaled_rgba(3, 20, 36, 1.0, master),
		scaled_rgba(5, 35, 58, 1.0, master),
		scaled_rgba(2, 11, 18, 1.0, master),
	);
	let w = rect.width();
	for i in 0..4 {
		let beam_x =
			rect.left() + w * (0.18 + i as f32 * 0.22) + (t * 0.55 + i as f32).sin() * 55.0;
		let mut mesh = Mesh::default();
		let top_c = scaled_rgba(56, 189, 248, 0.22, master);
		let bot_c = Color32::TRANSPARENT;
		mesh.colored_vertex(Pos2::new(beam_x - 28.0, rect.top()), top_c);
		mesh.colored_vertex(Pos2::new(beam_x + 76.0, rect.top()), top_c);
		mesh.colored_vertex(Pos2::new(beam_x - 10.0, rect.bottom()), bot_c);
		mesh.colored_vertex(Pos2::new(beam_x + 210.0, rect.bottom()), bot_c);
		mesh.add_triangle(0, 1, 2);
		mesh.add_triangle(1, 3, 2);
		painter.add(mesh);
	}
}

fn paint_matrix(
	painter: &Painter,
	rect: Rect,
	drops: &mut Vec<MatrixDrop>,
	t: f32,
	dt: f32,
	master: f32,
) {
	painter.rect_filled(rect, 0.0, scaled_rgba(5, 8, 12, 1.0, master));
	let col_step = 18.0_f32;
	let cols = (rect.width() / col_step).ceil().max(1.0) as usize;
	let mut rng = SimpleRng::new(0x004d_4154 ^ (t as u64));
	if drops.len() != cols {
		let mut seed_rng = SimpleRng::new(0x4d41_5452);
		drops.resize_with(cols, || MatrixDrop {
			y: seed_rng.range(-rect.height() * 0.8, rect.height()),
			speed: seed_rng.range(90.0, 240.0),
			seed: seed_rng.next_u64(),
		});
	}
	let font = FontId::monospace(13.0);
	const GLYPHS: &[char] = &[
		'0', '1', 'ｱ', 'ｲ', 'ｳ', 'ｴ', 'ｵ', 'ｶ', 'ｷ', 'ｸ', 'ｹ', 'ｺ', 'ｻ', 'ｼ', 'ｽ', 'ｾ', 'ｿ', 'ﾀ',
		'ﾁ', 'ﾂ', 'ﾃ', 'ﾄ', 'ﾅ', 'ﾆ', 'ﾇ', 'ﾈ', 'ﾉ', 'ﾊ', 'ﾋ', 'ﾌ', 'ﾍ', 'ﾎ',
	];
	for (i, d) in drops.iter_mut().enumerate() {
		d.y += d.speed * dt;
		if d.y > rect.height() + 180.0 {
			d.y = -20.0;
			d.speed = rng.range(95.0, 250.0);
		}
		let x = rect.left() + i as f32 * col_step;
		for trail in 0..8 {
			let cy = rect.top() + d.y - (trail as f32 * 16.0);
			if cy < rect.top() - 16.0 || cy > rect.bottom() + 16.0 {
				continue;
			}
			let idx = ((d.seed as usize)
				.wrapping_add(trail * 7)
				.wrapping_add((t * 8.0) as usize))
				% GLYPHS.len();
			let ch = GLYPHS[idx];
			let color = if trail == 0 {
				scaled_rgba(220, 255, 235, 0.95, master)
			} else {
				let fade = 1.0 - (trail as f32 / 8.0);
				scaled_rgba(16, 230, 120, fade * 0.72, master)
			};
			let mut buf = [0_u8; 4];
			let s = ch.encode_utf8(&mut buf);
			painter.text(
				Pos2::new(x, cy),
				egui::Align2::LEFT_TOP,
				s,
				font.clone(),
				color,
			);
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn mode_roundtrips_and_defaults_are_valid() {
		for raw in 0..=8 {
			let mode = LiveBackgroundMode::from_u8(raw);
			assert_eq!(mode.to_u8(), raw);
		}
		assert_eq!(LiveBackgroundMode::from_u8(99), LiveBackgroundMode::Off);
		let cfg = LiveBackgroundConfig::default();
		assert_eq!(cfg.panel_opacity, 72);
		assert_eq!(cfg.bg_opacity, 92);
		assert_eq!(cfg.speed_percent, 100);
	}
}
