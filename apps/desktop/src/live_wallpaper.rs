//! Live wallpaper & desktop background sync engine (iWallpaper, system wallpapers, MP4/MOV/GIF/WebP).
use crate::gpu_video;
use eframe::egui;
use platform::video::{Decoder, Picture, Sample};
use std::{
	io::Cursor,
	path::{Path, PathBuf},
	sync::{
		Arc,
		atomic::{AtomicBool, AtomicU16, Ordering},
		mpsc::{self, Receiver, SyncSender},
	},
	task::Poll,
	time::{Duration, Instant, SystemTime},
};
use ui::live_background::LiveBackgroundMode;

const MAX_STATIC_IMAGE_BYTES: u64 = 64 * 1024 * 1024;
#[cfg(target_os = "macos")]
const MAX_VIDEO_CACHE_BYTES: u64 = 512 * 1024 * 1024;
const MAX_ANIM_FRAMES: usize = 240;
const DESKTOP_POLL_INTERVAL: Duration = Duration::from_millis(1500);
#[cfg(target_os = "macos")]
const IWALLPAPER_AES_KEY: &[u8; 16] = b"NSData\0\0\0\0\0\0\0\0\0\0";

#[derive(Clone, Debug, PartialEq, Eq)]
struct ResolvedSource {
	path: PathBuf,
	mtime: Option<SystemTime>,
	label: String,
}

enum DecodedMedia {
	StaticImage {
		image: Arc<egui::ColorImage>,
		is_gojo: bool,
	},
	AnimatedFrames {
		frames: Vec<(Arc<egui::ColorImage>, Duration)>,
		is_gojo: bool,
	},
	VideoStart,
	Error(String),
}

struct VideoWorker {
	rx: Receiver<Picture>,
	stop: Arc<AtomicBool>,
	paused: Arc<AtomicBool>,
	speed_percent: Arc<AtomicU16>,
}

impl Drop for VideoWorker {
	fn drop(&mut self) {
		self.stop.store(true, Ordering::Relaxed);
	}
}

pub struct LiveWallpaper {
	last_mode: u8,
	last_custom_path: Option<String>,
	last_poll: Instant,
	active_source: Option<ResolvedSource>,
	picker_rx: Option<Receiver<Option<PathBuf>>>,
	media_rx: Option<Receiver<DecodedMedia>>,
	video_texture: gpu_video::VideoTexture,
	video_worker: Option<VideoWorker>,
	image_texture: Option<egui::TextureHandle>,
	image_size: egui::Vec2,
	anim_frames: Option<Vec<(Arc<egui::ColorImage>, Duration)>>,
	anim_index: usize,
	anim_next: Instant,
	source_is_gojo: Option<bool>,
	status: String,
}

impl Default for LiveWallpaper {
	fn default() -> Self {
		Self {
			last_mode: 0,
			last_custom_path: None,
			last_poll: Instant::now() - DESKTOP_POLL_INTERVAL,
			active_source: None,
			picker_rx: None,
			media_rx: None,
			video_texture: gpu_video::VideoTexture::default(),
			video_worker: None,
			image_texture: None,
			image_size: egui::Vec2::ZERO,
			anim_frames: None,
			anim_index: 0,
			anim_next: Instant::now(),
			source_is_gojo: None,
			status: String::new(),
		}
	}
}

impl LiveWallpaper {
	/// Advances live wallpaper discovery, decoding, GPU video upload, and UI state synchronization.
	pub fn sync(
		&mut self,
		ctx: &egui::Context,
		window: &Arc<winit::window::Window>,
		messaging: &mut ui::MessagingUi,
	) {
		if std::mem::take(&mut messaging.live_bg_pick_requested) {
			self.open_picker(ctx, window);
		}
		if let Some(rx) = &self.picker_rx
			&& let Ok(selection) = rx.try_recv()
		{
			self.picker_rx = None;
			if let Some(path) = selection {
				let path_str = path.to_string_lossy().into_owned();
				if path_str.len() <= 1024 {
					messaging.live_bg.custom_path = Some(path_str);
					messaging.live_bg.mode = LiveBackgroundMode::CustomMedia.to_u8();
				} else {
					self.status = "Đường dẫn tệp quá dài.".to_owned();
				}
			}
		}

		ui::design::set_live_background_config(&messaging.live_bg);
		let mode = LiveBackgroundMode::from_u8(messaging.live_bg.mode);

		if self.last_mode != messaging.live_bg.mode
			|| (mode == LiveBackgroundMode::CustomMedia
				&& self.last_custom_path != messaging.live_bg.custom_path)
		{
			self.last_mode = messaging.live_bg.mode;
			self.last_custom_path
				.clone_from(&messaging.live_bg.custom_path);
			self.reset_media();
			self.last_poll = Instant::now() - DESKTOP_POLL_INTERVAL;
		}

		if !mode.is_media_backed() {
			ui::design::set_live_background_frame(None);
			messaging.live_bg_status = self.status.clone();
			return;
		}

		let minimized = ctx.input(|i| i.viewport().minimized == Some(true));
		if let Some(worker) = &self.video_worker {
			worker.paused.store(minimized, Ordering::Relaxed);
			worker
				.speed_percent
				.store(messaging.live_bg.speed_percent, Ordering::Relaxed);
		}

		if self.last_poll.elapsed() >= DESKTOP_POLL_INTERVAL {
			self.last_poll = Instant::now();
			self.poll_source(ctx, mode, messaging.live_bg.custom_path.as_deref());
		}

		if let Some(rx) = &self.media_rx
			&& let Ok(decoded) = rx.try_recv()
		{
			self.media_rx = None;
			match decoded {
				DecodedMedia::StaticImage { image, is_gojo } => {
					self.source_is_gojo = Some(is_gojo);
					ui::live_effects::set_is_gojo(is_gojo);
					let res = image.resolution();
					self.image_size = egui::vec2(res[0] as f32, res[1] as f32);
					match &mut self.image_texture {
						Some(handle) => handle.set((*image).clone(), egui::TextureOptions::LINEAR),
						None => {
							self.image_texture = Some(ctx.load_texture(
								"live-wallpaper-img",
								image,
								egui::TextureOptions::LINEAR,
							));
						}
					}
					self.anim_frames = None;
				}
				DecodedMedia::AnimatedFrames { frames, is_gojo } => {
					self.source_is_gojo = Some(is_gojo);
					ui::live_effects::set_is_gojo(is_gojo);
					if let Some((first, delay)) = frames.first() {
						let res = first.resolution();
						self.image_size = egui::vec2(res[0] as f32, res[1] as f32);
						match &mut self.image_texture {
							Some(handle) => {
								handle.set((**first).clone(), egui::TextureOptions::LINEAR);
							}
							None => {
								self.image_texture = Some(ctx.load_texture(
									"live-wallpaper-img",
									first.clone(),
									egui::TextureOptions::LINEAR,
								));
							}
						}
						self.anim_index = 0;
						self.anim_next = Instant::now() + *delay;
						self.anim_frames = Some(frames);
					}
				}
				DecodedMedia::VideoStart => {
					self.source_is_gojo = Some(false);
					ui::live_effects::set_is_gojo(false);
				}
				DecodedMedia::Error(err) => {
					self.status = err;
				}
			}
		}

		// Pump video GPU frames or animated GIF/WebP frames.
		let mut active_frame = None;
		if let Some(worker) = &self.video_worker {
			let mut latest = None;
			while let Ok(pic) = worker.rx.try_recv() {
				latest = Some(pic);
			}
			if let Some(picture) = latest
				&& let Some(tex) =
					self.video_texture
						.upload(ctx, "live-wallpaper-video", picture.borrow())
			{
				active_frame = Some((tex.id, tex.size));
			} else if let Some(tex) = self.video_texture.texture() {
				active_frame = Some((tex.id, tex.size));
			}
		} else if let Some(frames) = &self.anim_frames {
			if !minimized && !frames.is_empty() {
				let now = Instant::now();
				if now >= self.anim_next {
					self.anim_index = (self.anim_index + 1) % frames.len();
					let (ref img, delay) = frames[self.anim_index];
					if let Some(handle) = &mut self.image_texture {
						handle.set((**img).clone(), egui::TextureOptions::LINEAR);
					}
					let speed =
						(f32::from(messaging.live_bg.speed_percent) / 100.0).clamp(0.1, 5.0);
					let scaled_delay =
						Duration::from_secs_f32((delay.as_secs_f32() / speed).max(0.016));
					self.anim_next = now + scaled_delay;
				}
				ctx.request_repaint_after(self.anim_next.saturating_duration_since(Instant::now()));
			}
			if let Some(handle) = &self.image_texture {
				active_frame = Some((handle.id(), self.image_size));
			}
		} else if let Some(handle) = &self.image_texture {
			active_frame = Some((handle.id(), self.image_size));
		}

		if let Some(is_gojo) = self.source_is_gojo {
			ui::live_effects::set_is_gojo(is_gojo);
		}
		ui::design::set_live_background_frame(active_frame);
		messaging.live_bg_status = self.status.clone();
	}

	fn reset_media(&mut self) {
		self.active_source = None;
		self.media_rx = None;
		self.video_worker = None;
		self.anim_frames = None;
		self.image_texture = None;
		self.image_size = egui::Vec2::ZERO;
		self.source_is_gojo = None;
		self.status.clear();
		ui::design::set_live_background_frame(None);
	}

	fn open_picker(&mut self, ctx: &egui::Context, window: &Arc<winit::window::Window>) {
		let (tx, rx) = mpsc::sync_channel(1);
		self.picker_rx = Some(rx);
		let fut = platform::save::live_wallpaper_source(window.clone());
		let wake = ctx.clone();
		std::thread::spawn(move || {
			if let Ok(rt) = tokio::runtime::Builder::new_current_thread()
				.enable_all()
				.build()
			{
				let selected = rt.block_on(fut);
				let _ = tx.try_send(selected);
				wake.request_repaint();
			}
		});
	}

	fn poll_source(&mut self, ctx: &egui::Context, mode: LiveBackgroundMode, custom: Option<&str>) {
		let discovered = match mode {
			LiveBackgroundMode::DesktopSync => discover_desktop_source(),
			LiveBackgroundMode::CustomMedia => custom.and_then(|raw| {
				let path = PathBuf::from(raw);
				if !path.is_file() {
					return None;
				}
				let mtime = std::fs::metadata(&path)
					.ok()
					.and_then(|m| m.modified().ok());
				let name = path
					.file_name()
					.and_then(|s| s.to_str())
					.unwrap_or("Tệp đã chọn")
					.to_owned();
				Some(ResolvedSource {
					path,
					mtime,
					label: format!("Đang phát: {name}"),
				})
			}),
			_ => None,
		};

		let Some(source) = discovered else {
			if mode == LiveBackgroundMode::DesktopSync {
				self.status = "Chưa phát hiện hình nền / iWallpaper đang chạy trên máy.".to_owned();
			}
			return;
		};

		if self.active_source.as_ref() == Some(&source) {
			return;
		}
		self.active_source = Some(source.clone());
		self.status = source.label.clone();
		self.load_source(ctx, &source.path);
	}

	fn load_source(&mut self, ctx: &egui::Context, path: &Path) {
		self.video_worker = None;
		self.anim_frames = None;
		self.source_is_gojo = None;

		let ext = path
			.extension()
			.and_then(|s| s.to_str())
			.map(|s| s.to_ascii_lowercase())
			.unwrap_or_default();

		if matches!(ext.as_str(), "mp4" | "mov" | "m4v") {
			let (frame_tx, frame_rx) = mpsc::sync_channel(1);
			let stop = Arc::new(AtomicBool::new(false));
			let paused = Arc::new(AtomicBool::new(false));
			let speed_percent = Arc::new(AtomicU16::new(100));
			self.video_worker = Some(VideoWorker {
				rx: frame_rx,
				stop: stop.clone(),
				paused: paused.clone(),
				speed_percent: speed_percent.clone(),
			});
			let (media_tx, media_rx) = mpsc::sync_channel(1);
			self.media_rx = Some(media_rx);
			let path = path.to_owned();
			let wake = ctx.clone();
			let _ = std::thread::Builder::new()
				.name("live-wallpaper-video".into())
				.spawn(move || {
					run_video_worker(path, wake, frame_tx, media_tx, stop, paused, speed_percent);
				});
		} else {
			let (media_tx, media_rx) = mpsc::sync_channel(1);
			self.media_rx = Some(media_rx);
			let path = path.to_owned();
			let wake = ctx.clone();
			let _ = std::thread::Builder::new()
				.name("live-wallpaper-image".into())
				.spawn(move || {
					let result = decode_image_or_animation(&path);
					let _ = media_tx.try_send(result);
					wake.request_repaint();
				});
		}
	}
}

fn run_video_worker(
	path: PathBuf,
	wake: egui::Context,
	frame_tx: SyncSender<Picture>,
	media_tx: SyncSender<DecodedMedia>,
	stop: Arc<AtomicBool>,
	paused: Arc<AtomicBool>,
	speed_percent: Arc<AtomicU16>,
) {
	let Ok(file) = std::fs::File::open(&path) else {
		let _ = media_tx.try_send(DecodedMedia::Error("Không thể mở tệp video nền.".into()));
		wake.request_repaint();
		return;
	};
	let mut decoder = match Decoder::open(Box::new(file)) {
		Ok(dec) => dec,
		Err(err) => {
			let _ = media_tx.try_send(DecodedMedia::Error(format!("Lỗi giải mã video: {err}")));
			wake.request_repaint();
			return;
		}
	};
	let _ = media_tx.try_send(DecodedMedia::VideoStart);

	let mut base_pts: Option<f64> = None;
	let mut loop_start = Instant::now();
	let mut last_speed = speed_percent.load(Ordering::Relaxed).max(10);

	while !stop.load(Ordering::Relaxed) {
		if paused.load(Ordering::Relaxed) {
			std::thread::sleep(Duration::from_millis(80));
			// Re-anchor timing after unpause so playback does not rush to catch up.
			base_pts = None;
			continue;
		}
		match decoder.poll_video() {
			Ok(Poll::Ready(Some(Sample::Video { pts, picture }))) => {
				let cur_speed = speed_percent.load(Ordering::Relaxed).clamp(10, 500);
				if cur_speed != last_speed {
					last_speed = cur_speed;
					base_pts = None;
				}
				let first = *base_pts.get_or_insert_with(|| {
					loop_start = Instant::now();
					pts
				});
				let speed = f64::from(cur_speed) / 100.0;
				let target_elapsed = ((pts - first).max(0.0)) / speed;
				let target_instant = loop_start + Duration::from_secs_f64(target_elapsed);
				while !stop.load(Ordering::Relaxed) && !paused.load(Ordering::Relaxed) {
					let now = Instant::now();
					if now >= target_instant {
						break;
					}
					std::thread::sleep(
						target_instant
							.saturating_duration_since(now)
							.min(Duration::from_millis(20)),
					);
				}
				if stop.load(Ordering::Relaxed) {
					break;
				}
				let _ = frame_tx.try_send(picture);
				wake.request_repaint();
			}
			Ok(Poll::Ready(Some(Sample::Audio { .. }))) => {}
			Ok(Poll::Ready(None)) => {
				if decoder.seek(0.0).is_err() {
					break;
				}
				base_pts = None;
				loop_start = Instant::now();
			}
			Ok(Poll::Pending) => {
				std::thread::sleep(Duration::from_millis(5));
			}
			Err(err) => {
				let _ = media_tx.try_send(DecodedMedia::Error(format!(
					"Video nền dừng giải mã: {err}"
				)));
				wake.request_repaint();
				break;
			}
		}
	}
}

fn decode_image_or_animation(path: &Path) -> DecodedMedia {
	let Ok(meta) = std::fs::metadata(path) else {
		return DecodedMedia::Error("Không tìm thấy tệp hình nền.".into());
	};
	if meta.len() > MAX_STATIC_IMAGE_BYTES {
		return DecodedMedia::Error("Ảnh nền vượt quá giới hạn 64 MB.".into());
	}
	let Ok(bytes) = std::fs::read(path) else {
		return DecodedMedia::Error("Không thể đọc tệp hình nền.".into());
	};

	let is_gif = bytes.starts_with(b"GIF8");
	let is_webp = bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP";

	if is_gif {
		use image::AnimationDecoder;
		if let Ok(decoder) = image::codecs::gif::GifDecoder::new(Cursor::new(&bytes)) {
			let frames = collect_anim_frames(decoder.into_frames(), 80);
			if frames.len() > 1 {
				let is_gojo = ui::live_effects::detect_gojo_eye(&frames[0].0);
				return DecodedMedia::AnimatedFrames { frames, is_gojo };
			} else if let Some((img, _)) = frames.into_iter().next() {
				let is_gojo = ui::live_effects::detect_gojo_eye(&img);
				return DecodedMedia::StaticImage {
					image: img,
					is_gojo,
				};
			}
		}
	}

	if is_webp {
		use image::AnimationDecoder;
		if let Ok(decoder) = image::codecs::webp::WebPDecoder::new(Cursor::new(&bytes))
			&& decoder.has_animation()
		{
			let frames = collect_anim_frames(decoder.into_frames(), 66);
			if !frames.is_empty() {
				let is_gojo = ui::live_effects::detect_gojo_eye(&frames[0].0);
				return if frames.len() > 1 {
					DecodedMedia::AnimatedFrames { frames, is_gojo }
				} else {
					DecodedMedia::StaticImage {
						image: frames[0].0.clone(),
						is_gojo,
					}
				};
			}
		}
	}

	match image::load_from_memory(&bytes) {
		Ok(dyn_img) => {
			let rgba = resize_if_huge(dyn_img.to_rgba8());
			let size = [rgba.width() as usize, rgba.height() as usize];
			let color = Arc::new(egui::ColorImage::from_rgba_unmultiplied(
				size,
				rgba.as_raw(),
			));
			let is_gojo = ui::live_effects::detect_gojo_eye(&color);
			DecodedMedia::StaticImage {
				image: color,
				is_gojo,
			}
		}
		Err(err) => DecodedMedia::Error(format!("Không thể giải mã hình nền: {err}")),
	}
}

fn collect_anim_frames(
	frames: image::Frames<'_>,
	default_ms: u32,
) -> Vec<(Arc<egui::ColorImage>, Duration)> {
	frames
		.take(MAX_ANIM_FRAMES)
		.flatten()
		.map(|frame| {
			let (num, den) = frame.delay().numer_denom_ms();
			let ms = num.checked_div(den).map_or(default_ms, |v| v.max(16));
			let rgba = resize_if_huge(frame.into_buffer());
			let size = [rgba.width() as usize, rgba.height() as usize];
			let color = egui::ColorImage::from_rgba_unmultiplied(size, rgba.as_raw());
			(Arc::new(color), Duration::from_millis(u64::from(ms)))
		})
		.collect()
}

fn resize_if_huge(img: image::RgbaImage) -> image::RgbaImage {
	let max_edge = 2048_u32;
	let (w, h) = img.dimensions();
	if w <= max_edge && h <= max_edge {
		return img;
	}
	let scale = (max_edge as f32 / w.max(h) as f32).min(1.0);
	let nw = ((w as f32 * scale).round() as u32).max(1);
	let nh = ((h as f32 * scale).round() as u32).max(1);
	image::imageops::resize(&img, nw, nh, image::imageops::FilterType::Triangle)
}

#[cfg(target_os = "macos")]
fn discover_desktop_source() -> Option<ResolvedSource> {
	// 1. Prioritize live video wallpapers managed by iWallpaper if installed & active.
	if let Some(iwp) = discover_iwallpaper_macos() {
		return Some(iwp);
	}
	// 2. Fallback to macOS system wallpaper store (Index.plist / Finder).
	discover_macos_system_wallpaper()
}

#[cfg(target_os = "macos")]
fn discover_iwallpaper_macos() -> Option<ResolvedSource> {
	let home = dirs::home_dir()?;
	let docs = home.join("Library/Containers/com.macosgame.iwallpaper/Data/Documents");
	let screen_path = docs.join("screen");
	let screen_bytes = std::fs::read(&screen_path).ok()?;
	let screen_text = String::from_utf8_lossy(&screen_bytes);
	let uuid = extract_uuid(&screen_text)?;

	let videos_dir = docs.join("Videos");
	for ext in ["mp4", "mov"] {
		let enc_path = videos_dir.join(format!("{uuid}.{ext}"));
		if enc_path.is_file()
			&& let Some(cached) = ensure_iwallpaper_decrypted(&enc_path, &uuid, ext)
		{
			let mtime = std::fs::metadata(&enc_path)
				.ok()
				.and_then(|m| m.modified().ok());
			let short_id = &uuid[..8.min(uuid.len())];
			return Some(ResolvedSource {
				path: cached,
				mtime,
				label: format!("Đang đồng bộ video iWallpaper ({short_id}…{ext})"),
			});
		}
	}

	let images_dir = docs.join("DesktopImage");
	for ext in ["png", "jpg", "jpeg"] {
		let img_path = images_dir.join(format!("{uuid}.{ext}"));
		if img_path.is_file() {
			let mtime = std::fs::metadata(&img_path)
				.ok()
				.and_then(|m| m.modified().ok());
			return Some(ResolvedSource {
				path: img_path,
				mtime,
				label: format!("Đang đồng bộ ảnh iWallpaper ({uuid})"),
			});
		}
	}
	None
}

#[cfg(target_os = "macos")]
fn decrypt_iwallpaper_bytes(mut data: Vec<u8>) -> Option<Vec<u8>> {
	use aes::cipher::{BlockDecrypt, KeyInit};
	if data.is_empty() || !data.len().is_multiple_of(16) {
		return None;
	}
	let cipher = aes::Aes128::new_from_slice(IWALLPAPER_AES_KEY).ok()?;
	for block in data.as_chunks_mut::<16>().0 {
		cipher.decrypt_block(aes::cipher::generic_array::GenericArray::from_mut_slice(
			block,
		));
	}
	// Strip valid PKCS#7 padding if present.
	if let Some(&pad) = data.last()
		&& (1..=16).contains(&pad)
		&& data.len() >= usize::from(pad)
		&& data[data.len() - usize::from(pad)..]
			.iter()
			.all(|&b| b == pad)
	{
		data.truncate(data.len() - usize::from(pad));
	}
	Some(data)
}

#[cfg(target_os = "macos")]
fn ensure_iwallpaper_decrypted(enc_path: &Path, uuid: &str, ext: &str) -> Option<PathBuf> {
	let cache_dir = std::env::temp_dir().join("agy_iwallpaper_cache");
	let _ = std::fs::create_dir_all(&cache_dir);
	let cached_path = cache_dir.join(format!("{uuid}.{ext}"));

	let enc_meta = std::fs::metadata(enc_path).ok()?;
	if enc_meta.len() == 0 || enc_meta.len() > MAX_VIDEO_CACHE_BYTES {
		return None;
	}
	let enc_mtime = enc_meta.modified().ok();
	if let Ok(cache_meta) = std::fs::metadata(&cached_path)
		&& cache_meta.len() > 0
		&& enc_mtime
			.zip(cache_meta.modified().ok())
			.is_some_and(|(e, c)| c >= e)
	{
		return Some(cached_path);
	}

	let data = decrypt_iwallpaper_bytes(std::fs::read(enc_path).ok()?)?;

	let tmp_path = cache_dir.join(format!("{uuid}.{ext}.tmp"));
	std::fs::write(&tmp_path, &data).ok()?;
	std::fs::rename(&tmp_path, &cached_path).ok()?;
	Some(cached_path)
}

#[cfg(target_os = "macos")]
fn discover_macos_system_wallpaper() -> Option<ResolvedSource> {
	let home = dirs::home_dir()?;
	let plist = home.join("Library/Application Support/com.apple.wallpaper/Store/Index.plist");
	if plist.is_file() {
		let mtime = std::fs::metadata(&plist)
			.ok()
			.and_then(|m| m.modified().ok());
		if let Ok(out) = std::process::Command::new("plutil")
			.args(["-convert", "xml1", "-o", "-"])
			.arg(&plist)
			.output() && out.status.success()
		{
			let xml = String::from_utf8_lossy(&out.stdout);
			for candidate in extract_file_urls(&xml) {
				if candidate.is_file() {
					let name = candidate
						.file_name()
						.and_then(|s| s.to_str())
						.unwrap_or("Wallpaper")
						.to_owned();
					return Some(ResolvedSource {
						path: candidate,
						mtime,
						label: format!("Đang đồng bộ hình nền macOS ({name})"),
					});
				}
			}
		}
	}
	let output = std::process::Command::new("osascript")
		.args([
			"-e",
			"tell application \"Finder\" to get POSIX path of (desktop picture as alias)",
		])
		.output()
		.ok()?;
	if output.status.success() {
		let path = PathBuf::from(String::from_utf8_lossy(&output.stdout).trim());
		if path.is_file() {
			let mtime = std::fs::metadata(&path)
				.ok()
				.and_then(|m| m.modified().ok());
			return Some(ResolvedSource {
				path,
				mtime,
				label: "Đang đồng bộ hình nền Desktop macOS".to_owned(),
			});
		}
	}
	None
}

#[cfg(target_os = "windows")]
fn discover_desktop_source() -> Option<ResolvedSource> {
	// Windows DWM stores the active transcoded desktop image at %APPDATA%\Microsoft\Windows\Themes\TranscodedWallpaper.
	if let Some(config) = dirs::config_dir() {
		let transcoded = config.join("Microsoft\\Windows\\Themes\\TranscodedWallpaper");
		if transcoded.is_file() {
			let mtime = std::fs::metadata(&transcoded)
				.ok()
				.and_then(|m| m.modified().ok());
			return Some(ResolvedSource {
				path: transcoded,
				mtime,
				label: "Đang đồng bộ hình nền Windows Desktop".to_owned(),
			});
		}
	}
	use std::os::windows::process::CommandExt;
	const CREATE_NO_WINDOW: u32 = 0x0800_0000;
	let out = std::process::Command::new("reg")
		.args(["query", "HKCU\\Control Panel\\Desktop", "/v", "WallPaper"])
		.creation_flags(CREATE_NO_WINDOW)
		.output()
		.ok()?;
	if out.status.success() {
		let text = String::from_utf8_lossy(&out.stdout);
		for line in text.lines() {
			if let Some((_, rest)) = line.split_once("REG_SZ") {
				let path = PathBuf::from(rest.trim());
				if path.is_file() {
					let mtime = std::fs::metadata(&path)
						.ok()
						.and_then(|m| m.modified().ok());
					return Some(ResolvedSource {
						path,
						mtime,
						label: "Đang đồng bộ hình nền Windows Desktop".to_owned(),
					});
				}
			}
		}
	}
	None
}

#[cfg(target_os = "linux")]
fn resolve_linux_wallpaper_uri(raw: &str) -> Option<ResolvedSource> {
	let trimmed = raw.trim().trim_matches(|c| matches!(c, '\'' | '"'));
	if trimmed.is_empty() {
		return None;
	}
	let path = if trimmed.starts_with("file://") {
		url::Url::parse(trimmed)
			.ok()
			.and_then(|u| u.to_file_path().ok())
			.unwrap_or_else(|| PathBuf::from(trimmed.strip_prefix("file://").unwrap_or(trimmed)))
	} else {
		PathBuf::from(trimmed)
	};
	if !path.is_file() {
		return None;
	}
	let mtime = std::fs::metadata(&path)
		.ok()
		.and_then(|m| m.modified().ok());
	Some(ResolvedSource {
		path,
		mtime,
		label: "Đang đồng bộ hình nền Linux Desktop".to_owned(),
	})
}

#[cfg(target_os = "linux")]
fn discover_desktop_source() -> Option<ResolvedSource> {
	for key in ["picture-uri", "picture-uri-dark"] {
		if let Ok(out) = std::process::Command::new("gsettings")
			.args(["get", "org.gnome.desktop.background", key])
			.output() && out.status.success()
		{
			let raw = String::from_utf8_lossy(&out.stdout);
			if let Some(resolved) = resolve_linux_wallpaper_uri(&raw) {
				return Some(resolved);
			}
		}
	}
	let kde_cfg = dirs::config_dir()?.join("plasma-org.kde.plasma.desktop-appletsrc");
	let contents = std::fs::read_to_string(kde_cfg).ok()?;
	for line in contents.lines() {
		if let Some(value) = line.trim().strip_prefix("Image=")
			&& let Some(resolved) = resolve_linux_wallpaper_uri(value)
		{
			return Some(resolved);
		}
	}
	None
}

#[cfg(target_os = "macos")]
fn extract_uuid(text: &str) -> Option<String> {
	let bytes = text.as_bytes();
	if bytes.len() < 36 {
		return None;
	}
	for i in 0..=bytes.len() - 36 {
		let s = &bytes[i..i + 36];
		let valid = s.iter().enumerate().all(|(idx, &b)| {
			if matches!(idx, 8 | 13 | 18 | 23) {
				b == b'-'
			} else {
				b.is_ascii_hexdigit()
			}
		});
		if valid {
			return Some(String::from_utf8_lossy(s).to_ascii_uppercase());
		}
	}
	None
}

#[cfg(target_os = "macos")]
fn extract_file_urls(xml: &str) -> Vec<PathBuf> {
	let mut out = Vec::new();
	let mut rest = xml;
	while let Some(idx) = rest.find("file://") {
		let after = &rest[idx..];
		let end = after.find('<').unwrap_or(after.len());
		let raw_url = &after[..end];
		if let Ok(parsed) = url::Url::parse(raw_url)
			&& let Ok(path) = parsed.to_file_path()
		{
			out.push(path);
		}
		rest = &after[end..];
	}
	out
}

#[cfg(test)]
mod tests {
	#[cfg(target_os = "macos")]
	#[test]
	fn extracts_uuid_and_file_urls() {
		assert_eq!(
			super::extract_uuid("xxC3026D6D-D165-4691-986E-7B1200408287yy").as_deref(),
			Some("C3026D6D-D165-4691-986E-7B1200408287")
		);
		let urls = super::extract_file_urls(
			"<string>file:///tmp/my%20wallpaper.png</string><string>other</string>",
		);
		assert_eq!(
			urls,
			vec![std::path::PathBuf::from("/tmp/my wallpaper.png")]
		);
	}

	#[cfg(target_os = "macos")]
	#[test]
	fn decrypts_iwallpaper_aes_ecb_and_unpads_pkcs7() {
		use aes::cipher::{BlockEncrypt, KeyInit};
		let cipher = aes::Aes128::new_from_slice(super::IWALLPAPER_AES_KEY).unwrap();
		for plaintext in [
			b"ftypmp42\x00\x00\x00\x00mp42isomABC".as_slice(),
			b"0123456789abcdef".as_slice(),
		] {
			let pad_len = 16 - (plaintext.len() % 16);
			let mut padded = plaintext.to_vec();
			padded.extend(std::iter::repeat_n(pad_len as u8, pad_len));
			for block in padded.as_chunks_mut::<16>().0 {
				cipher.encrypt_block(aes::cipher::generic_array::GenericArray::from_mut_slice(
					block,
				));
			}
			let decrypted = super::decrypt_iwallpaper_bytes(padded).unwrap();
			assert_eq!(decrypted, plaintext);
		}
		assert!(super::decrypt_iwallpaper_bytes(Vec::new()).is_none());
		assert!(super::decrypt_iwallpaper_bytes(vec![1, 2, 3]).is_none());
	}
}
