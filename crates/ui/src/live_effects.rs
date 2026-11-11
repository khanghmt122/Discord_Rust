//! Live background effects: falling Sakura petals, snowflakes and Gojo Infinity Eye Glow.

use egui::{Color32, Mesh, Painter, Pos2, Rect, Stroke, Vec2};
use std::cell::{Cell, RefCell};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
#[repr(u8)]
pub enum ParticleType {
	#[default]
	None = 0,
	Sakura = 1,
	Snow = 2,
}

impl ParticleType {
	pub fn from_u8(val: u8) -> Self {
		match val {
			1 => Self::Sakura,
			2 => Self::Snow,
			_ => Self::None,
		}
	}

	pub fn to_u8(self) -> u8 {
		self as u8
	}
}

struct SimpleRng(u64);

impl SimpleRng {
	fn new(seed: u64) -> Self {
		Self(if seed == 0 { 0x853c49e6748fea9b } else { seed })
	}

	fn next_u64(&mut self) -> u64 {
		self.0 ^= self.0 << 13;
		self.0 ^= self.0 >> 7;
		self.0 ^= self.0 << 17;
		self.0
	}

	fn next_f32(&mut self) -> f32 {
		(self.next_u64() & 0x00ff_ffff) as f32 / 16777216.0
	}

	fn range(&mut self, min: f32, max: f32) -> f32 {
		min + (max - min) * self.next_f32()
	}
}

#[derive(Clone, Copy, Debug)]
struct Particle {
	x: f32,
	y: f32,
	vx: f32,
	vy: f32,
	size: f32,
	angle: f32,
	rot_speed: f32,
	flutter_phase: f32,
	flutter_speed: f32,
	sway_phase: f32,
	sway_speed: f32,
	depth: f32,
}

thread_local! {
	static LIVE_CONFIG: Cell<(ParticleType, bool)> = const { Cell::new((ParticleType::Sakura, true)) };
	static VOICE_ACTIVE: Cell<bool> = const { Cell::new(false) };
	static PARTICLES: RefCell<Vec<Particle>> = const { RefCell::new(Vec::new()) };
	static CURRENT_PARTICLE_KIND: Cell<ParticleType> = const { Cell::new(ParticleType::None) };
	static LAST_TIME: Cell<f64> = const { Cell::new(0.0) };
	static IS_GOJO_BACKGROUND: Cell<bool> = const { Cell::new(true) };
}

pub fn set_live_effects(particle_type: u8, eye_glow: bool) {
	LIVE_CONFIG.set((ParticleType::from_u8(particle_type), eye_glow));
}

pub fn live_effects() -> (ParticleType, bool) {
	LIVE_CONFIG.get()
}

pub fn set_voice_active(active: bool) {
	VOICE_ACTIVE.set(active);
}

pub fn is_voice_active() -> bool {
	VOICE_ACTIVE.get()
}

pub fn set_is_gojo(is_gojo: bool) {
	IS_GOJO_BACKGROUND.set(is_gojo);
}

pub fn is_gojo() -> bool {
	IS_GOJO_BACKGROUND.get()
}

pub fn detect_gojo_eye(image: &egui::ColorImage) -> bool {
	let w = image.resolution()[0];
	let h = image.resolution()[1];
	if w < 100 || h < 100 {
		return false;
	}
	let sample_x = (w as f32 * 0.461) as usize;
	let sample_y = (h as f32 * 0.477) as usize;
	let min_x = sample_x.saturating_sub(20);
	let max_x = (sample_x + 20).min(w.saturating_sub(1));
	let min_y = sample_y.saturating_sub(20);
	let max_y = (sample_y + 20).min(h.saturating_sub(1));
	let mut blue_count = 0;
	for y in min_y..=max_y {
		for x in min_x..=max_x {
			let px = image.pixels()[y * w + x];
			if px.b() > 110 && px.b() > px.r() + 20 {
				blue_count += 1;
			}
		}
	}
	blue_count >= 5
}

fn init_particles(kind: ParticleType, width: f32, height: f32) -> Vec<Particle> {
	let mut rng = SimpleRng::new(987654321);
	let count = match kind {
		ParticleType::Sakura => 45,
		ParticleType::Snow => 75,
		ParticleType::None => return Vec::new(),
	};
	let mut list = Vec::with_capacity(count);
	for _ in 0..count {
		let depth = rng.range(0.35, 1.0);
		list.push(Particle {
			x: rng.range(0.0, width.max(100.0)),
			y: rng.range(0.0, height.max(100.0)),
			vx: match kind {
				ParticleType::Sakura => rng.range(18.0, 48.0) * depth,
				ParticleType::Snow => rng.range(-12.0, 16.0) * depth,
				ParticleType::None => 0.0,
			},
			vy: match kind {
				ParticleType::Sakura => rng.range(35.0, 75.0) * depth,
				ParticleType::Snow => rng.range(30.0, 95.0) * depth,
				ParticleType::None => 0.0,
			},
			size: match kind {
				ParticleType::Sakura => rng.range(11.0, 20.0) * depth,
				ParticleType::Snow => rng.range(1.5, 4.5) * depth,
				ParticleType::None => 0.0,
			},
			angle: rng.range(0.0, std::f32::consts::TAU),
			rot_speed: rng.range(-1.4, 1.4),
			flutter_phase: rng.range(0.0, std::f32::consts::TAU),
			flutter_speed: rng.range(1.8, 3.4),
			sway_phase: rng.range(0.0, std::f32::consts::TAU),
			sway_speed: rng.range(1.0, 2.2),
			depth,
		});
	}
	list
}

pub fn paint_live_effects(ctx: &egui::Context, rect: Rect, bg_rect: Option<Rect>) {
	let (kind, eye_glow) = LIVE_CONFIG.get();
	if kind == ParticleType::None && !eye_glow {
		return;
	}

	let painter = ctx.layer_painter(egui::LayerId::background());
	let time = ctx.input(|i| i.time);
	let last = LAST_TIME.get();
	let dt = if last == 0.0 {
		0.016
	} else {
		((time - last) as f32).clamp(0.001, 0.1)
	};
	LAST_TIME.set(time);

	// 1. Paint Gojo Infinity Eye Glow
	if eye_glow
		&& IS_GOJO_BACKGROUND.get()
		&& let Some(bg) = bg_rect
	{
		let eye_x = bg.min.x + bg.width() * 0.461;
		let eye_y = bg.min.y + bg.height() * 0.477;
		let eye_pos = Pos2::new(eye_x, eye_y);

		if rect.contains(eye_pos) {
			paint_gojo_eye(&painter, eye_pos, bg.width(), time, dt);
		}
	}

	// 2. Paint falling particles (Sakura / Snow)
	if kind != ParticleType::None {
		paint_particles(&painter, rect, kind, time, dt);
	}

	// Request next frame repaint for silky smooth 60fps/120Hz animation
	ctx.request_repaint();
}

fn paint_gojo_eye(painter: &Painter, eye_pos: Pos2, bg_width: f32, time: f64, _dt: f32) {
	let base_r = (bg_width * 0.045).clamp(22.0, 75.0);
	let voice = VOICE_ACTIVE.get();

	// Smooth breathing oscillation (~0.5 Hz)
	let breath = 0.60 + 0.24 * (time * 2.3).sin() as f32 + 0.12 * (time * 4.7).cos() as f32;

	let intensity = if voice {
		// Intense surge and rapid flickering when someone speaks in voice
		let voice_pulse = 1.15
			+ 0.45 * (time * 15.0).sin().abs() as f32
			+ 0.25 * ((time * 27.0).cos() * 0.5 + 0.5) as f32;
		(breath * 0.3 + voice_pulse * 0.85).min(2.0)
	} else {
		breath
	};

	// 1. Deep Oceanic Limitless Aura
	paint_glow_circle(
		painter,
		eye_pos,
		base_r * 2.6,
		0,
		110,
		240,
		(26.0 * intensity) as u8,
	);

	// 2. Electric Blue Corona
	paint_glow_circle(
		painter,
		eye_pos,
		base_r * 1.6,
		0,
		195,
		255,
		(52.0 * intensity) as u8,
	);

	// 3. Cyan Iris Core Fire
	paint_glow_circle(
		painter,
		eye_pos,
		base_r * 0.95,
		75,
		235,
		255,
		(100.0 * intensity) as u8,
	);

	// 4. Six Eyes Cursed Energy Wisps (orbiting around pupil)
	for k in 0..4 {
		let speed = 1.6 + k as f64 * 0.35;
		let angle = (time * speed + k as f64 * std::f64::consts::FRAC_PI_2) as f32;
		let dist = base_r * (0.55 + 0.20 * (time * 3.2 + k as f64 * 1.1).sin() as f32);
		let wisp_pos = eye_pos + Vec2::new(angle.cos() * dist, angle.sin() * dist * 0.82);
		let w_alpha = ((130.0 + 80.0 * (time * 5.0 + k as f64).sin() as f32) * intensity)
			.clamp(30.0, 255.0) as u8;
		paint_glow_circle(painter, wisp_pos, 3.5 * intensity, 180, 250, 255, w_alpha);
	}

	// 5. Star Flare Twinkle Rays
	let ray_len = base_r * (0.75 + 0.25 * intensity);
	let ray_rot = (time * 0.4) as f32;
	let cos_r = ray_rot.cos();
	let sin_r = ray_rot.sin();
	let ray_alpha = (160.0 * intensity).clamp(40.0, 240.0) as u8;
	let a_f = ray_alpha as f32 / 255.0;
	let stroke_color = Color32::from_rgba_premultiplied(
		(200.0 * a_f) as u8,
		(245.0 * a_f) as u8,
		ray_alpha,
		ray_alpha,
	);
	let stroke = Stroke::new(1.5, stroke_color);
	let dx1 = Vec2::new(cos_r * ray_len, sin_r * ray_len);
	let dy1 = Vec2::new(-sin_r * ray_len, cos_r * ray_len);
	painter.line_segment([eye_pos - dx1, eye_pos + dx1], stroke);
	painter.line_segment([eye_pos - dy1, eye_pos + dy1], stroke);

	// 6. Center Singularity Sparkle
	painter.circle_filled(
		eye_pos,
		3.8 * intensity,
		Color32::from_rgba_premultiplied(255, 255, 255, 230),
	);
}

fn paint_glow_circle(painter: &Painter, center: Pos2, radius: f32, r: u8, g: u8, b: u8, a: u8) {
	if a == 0 || radius <= 0.0 {
		return;
	}
	let a_f = a as f32 / 255.0;
	let color = Color32::from_rgba_premultiplied(
		(r as f32 * a_f) as u8,
		(g as f32 * a_f) as u8,
		(b as f32 * a_f) as u8,
		a,
	);
	painter.circle_filled(center, radius, color);
}

fn paint_particles(painter: &Painter, rect: Rect, kind: ParticleType, time: f64, dt: f32) {
	let width = rect.width();
	let height = rect.height();
	if width <= 10.0 || height <= 10.0 {
		return;
	}

	PARTICLES.with(|cell| {
		let mut particles = cell.borrow_mut();
		if CURRENT_PARTICLE_KIND.get() != kind || particles.is_empty() {
			*particles = init_particles(kind, width, height);
			CURRENT_PARTICLE_KIND.set(kind);
		}

		let mut rng = SimpleRng::new((time * 1000.0) as u64);

		for p in particles.iter_mut() {
			// Update position and rotation
			match kind {
				ParticleType::Sakura => {
					let sway = (p.sway_phase + time as f32 * p.sway_speed).sin() * 22.0 * p.depth;
					p.x += (p.vx + sway) * dt;
					p.y += p.vy * dt;
					p.angle += p.rot_speed * dt;
					p.flutter_phase += p.flutter_speed * dt;

					// Wrap around boundaries
					if p.y > height + 25.0 || p.x > width + 60.0 {
						p.y = -20.0;
						p.x = rng.range(-30.0, width * 0.9);
					}
					if p.x < -60.0 {
						p.x = width + 20.0;
					}

					draw_sakura_petal(painter, p, rect.min);
				}
				ParticleType::Snow => {
					let sway = (p.sway_phase + time as f32 * p.sway_speed).sin() * 12.0 * p.depth;
					p.x += (p.vx + sway) * dt;
					p.y += p.vy * dt;

					// Wrap around
					if p.y > height + 10.0 {
						p.y = -10.0;
						p.x = rng.range(0.0, width);
					}
					if p.x > width + 10.0 {
						p.x = -5.0;
					} else if p.x < -10.0 {
						p.x = width + 5.0;
					}

					draw_snowflake(painter, p, rect.min, time);
				}
				ParticleType::None => {}
			}
		}
	});
}

fn draw_sakura_petal(painter: &Painter, p: &Particle, offset: Pos2) {
	let cos_a = p.angle.cos();
	let sin_a = p.angle.sin();
	let flutter = (p.flutter_phase).sin().abs() * 0.7 + 0.3;
	let w_scale = p.size * flutter;
	let h_scale = p.size;

	let local_pts = [
		(-0.20, -0.95), // Top-left notch
		(0.00, -0.75),  // Notch center indent
		(0.20, -0.95),  // Top-right notch
		(0.58, -0.45),  // Upper right lobe
		(0.68, 0.10),   // Mid right
		(0.42, 0.60),   // Lower right
		(0.00, 1.00),   // Stem tip
		(-0.42, 0.60),  // Lower left
		(-0.68, 0.10),  // Mid left
		(-0.60, -0.45), // Upper left lobe
	];

	let cx = offset.x + p.x;
	let cy = offset.y + p.y;
	let center_pos = Pos2::new(cx, cy);

	let alpha = (165.0 * p.depth).clamp(35.0, 220.0) as u8;
	let a_f = alpha as f32 / 255.0;

	// Center blush pink
	let center_color = Color32::from_rgba_premultiplied(
		(255.0 * a_f) as u8,
		(160.0 * a_f) as u8,
		(185.0 * a_f) as u8,
		alpha,
	);
	// Outer pale blossom pink
	let outer_color = Color32::from_rgba_premultiplied(
		(255.0 * a_f) as u8,
		(210.0 * a_f) as u8,
		(225.0 * a_f) as u8,
		alpha,
	);

	let mut mesh = Mesh::default();
	mesh.colored_vertex(center_pos, center_color);

	let count = local_pts.len() as u32;
	for (lx, ly) in local_pts {
		let rx = lx * w_scale;
		let ry = ly * h_scale;
		let sx = cx + (rx * cos_a - ry * sin_a);
		let sy = cy + (rx * sin_a + ry * cos_a);
		mesh.colored_vertex(Pos2::new(sx, sy), outer_color);
	}

	for i in 1..count {
		mesh.add_triangle(0, i, i + 1);
	}
	mesh.add_triangle(0, count, 1);

	painter.add(mesh);
}

fn draw_snowflake(painter: &Painter, p: &Particle, offset: Pos2, time: f64) {
	let pos = Pos2::new(offset.x + p.x, offset.y + p.y);
	let alpha = ((140.0 + 75.0 * ((time * 2.0 + p.sway_phase as f64).sin() as f32)) * p.depth)
		.clamp(30.0, 230.0) as u8;

	// Soft outer glow
	paint_glow_circle(
		painter,
		pos,
		p.size * 1.8,
		200,
		235,
		255,
		(35.0 * p.depth) as u8,
	);

	// Core crystal
	let a_f = alpha as f32 / 255.0;
	let core_color =
		Color32::from_rgba_premultiplied((245.0 * a_f) as u8, (250.0 * a_f) as u8, alpha, alpha);
	painter.circle_filled(pos, p.size, core_color);

	// Sparkling starlet for larger flakes
	if p.size > 3.0 {
		let stroke = Stroke::new(1.0, core_color);
		let r = p.size * 1.4;
		painter.line_segment([pos - Vec2::new(r, 0.0), pos + Vec2::new(r, 0.0)], stroke);
		painter.line_segment([pos - Vec2::new(0.0, r), pos + Vec2::new(0.0, r)], stroke);
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn particle_type_conversion() {
		assert_eq!(ParticleType::from_u8(0), ParticleType::None);
		assert_eq!(ParticleType::from_u8(1), ParticleType::Sakura);
		assert_eq!(ParticleType::from_u8(2), ParticleType::Snow);
		assert_eq!(ParticleType::from_u8(99), ParticleType::None);

		assert_eq!(ParticleType::None.to_u8(), 0);
		assert_eq!(ParticleType::Sakura.to_u8(), 1);
		assert_eq!(ParticleType::Snow.to_u8(), 2);
	}

	#[test]
	fn init_particles_counts() {
		assert_eq!(init_particles(ParticleType::None, 800.0, 600.0).len(), 0);
		assert_eq!(init_particles(ParticleType::Sakura, 800.0, 600.0).len(), 45);
		assert_eq!(init_particles(ParticleType::Snow, 800.0, 600.0).len(), 75);
	}

	#[test]
	fn rng_produces_valid_ranges() {
		let mut rng = SimpleRng::new(12345);
		for _ in 0..100 {
			let val = rng.range(10.0, 20.0);
			assert!((10.0..=20.0).contains(&val), "val out of range: {val}");
		}
	}

	#[test]
	fn gojo_eye_detection() {
		let mut img = egui::ColorImage::filled([200, 200], Color32::BLACK);
		assert!(!detect_gojo_eye(&img));

		// Paint a blue eye region around (0.461 * 200 = 92, 0.477 * 200 = 95)
		for y in 90..100 {
			for x in 90..100 {
				img.pixels_mut()[y * 200 + x] = Color32::from_rgb(20, 150, 240);
			}
		}
		assert!(detect_gojo_eye(&img));
	}
}
