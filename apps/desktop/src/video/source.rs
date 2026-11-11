//! Decoder-driven attachment ranges. No file cache or eager whole-video download.
use std::{
	collections::VecDeque,
	fs::{File, OpenOptions},
	io::{self, Read, Seek, SeekFrom, Write},
	path::PathBuf,
	sync::{
		Arc,
		atomic::{AtomicBool, Ordering},
	},
	time::Duration,
};
use tokio::runtime::Handle;

const MAX_BYTES: usize = 100 * 1024 * 1024;
const CHUNK: usize = 256 * 1024;
const CACHE_CHUNKS: usize = 8;
const MAX_REDIRECTS: usize = 5;
const INVALID: &str = "Video download failed or changed; reload the conversation";
const UNSUPPORTED: &str = "Video server does not support buffering; download to play externally";

pub(super) fn source(
	url: Option<url::Url>,
	expected: usize,
	cancelled: Arc<AtomicBool>,
	runtime: Handle,
) -> Result<Box<dyn platform::video::ReadSeek>, &'static str> {
	source_with_options(url, expected, false, cancelled, runtime)
}

fn source_with_options(
	url: Option<url::Url>,
	expected: usize,
	external_user_agent: bool,
	cancelled: Arc<AtomicBool>,
	runtime: Handle,
) -> Result<Box<dyn platform::video::ReadSeek>, &'static str> {
	if expected == 0 || expected > MAX_BYTES {
		return Err("Video preview limit: 100 MiB");
	}
	let (input, len) = if let Some(url) = url {
		// The caller has validated the service URL; never let URL userinfo add credentials.
		if !url.username().is_empty() || url.password().is_some() {
			return Err(INVALID);
		}
		let client = wreq::Client::builder()
			.no_proxy()
			.no_gzip()
			.no_brotli()
			.no_deflate()
			.no_zstd()
			.redirect(wreq::redirect::Policy::none())
			.timeout(Duration::from_secs(15))
			.build()
			.map_err(|_| INVALID)?;
		(
			Input::Http {
				client,
				url,
				external_user_agent,
				runtime,
			},
			expected,
		)
	} else {
		#[cfg(not(feature = "demo"))]
		return Err(INVALID);
		#[cfg(feature = "demo")]
		{
			let bytes = include_bytes!("../../tests/fixtures/video.mov");
			if bytes.is_empty() || bytes.len() > MAX_BYTES {
				return Err(INVALID);
			}
			(Input::Demo(bytes), bytes.len())
		}
	};
	Ok(Box::new(Source {
		input,
		cancelled,
		len,
		position: 0,
		cache: VecDeque::new(),
	}))
}

enum Input {
	Http {
		client: wreq::Client,
		url: url::Url,
		external_user_agent: bool,
		runtime: Handle,
	},
	#[cfg(feature = "demo")]
	Demo(&'static [u8]),
}
struct Source {
	input: Input,
	cancelled: Arc<AtomicBool>,
	len: usize,
	position: usize,
	cache: VecDeque<(usize, Vec<u8>)>,
}
fn invalid() -> io::Error {
	io::Error::new(io::ErrorKind::InvalidData, INVALID)
}
impl Source {
	fn check_cancelled(&self) -> io::Result<()> {
		if self.cancelled.load(Ordering::Acquire) {
			// Read::read_exact retries Interrupted indefinitely.
			Err(io::Error::new(
				io::ErrorKind::ConnectionAborted,
				"Cancelled",
			))
		} else {
			Ok(())
		}
	}
	fn fill(&mut self) -> io::Result<()> {
		// Retain both tracks across demuxer seeks. Evict before transfer so encoded
		// payloads, including the new range, stay within eight chunks / 2 MiB.
		if self.cache.len() == CACHE_CHUNKS {
			self.cache.pop_front();
		}
		let (client, url, external_user_agent, runtime) = match &self.input {
			Input::Http {
				client,
				url,
				external_user_agent,
				runtime,
			} => (client, url, *external_user_agent, runtime),
			#[cfg(feature = "demo")]
			Input::Demo(_) => return Err(invalid()),
		};
		let start = self.position / CHUNK * CHUNK;
		let count = CHUNK.min(self.len - start);
		let end = start + count - 1;
		let bytes = runtime.block_on(async {
			let cancelled = async {
				while !self.cancelled.load(Ordering::Acquire) {
					tokio::time::sleep(Duration::from_millis(20)).await;
				}
			};
			let transfer = async {
				let response = build_video_get(client, url, external_user_agent)
					.header(wreq::header::ACCEPT_ENCODING, "identity")
					.header(wreq::header::RANGE, format!("bytes={start}-{end}"))
					.send()
					.await
					.map_err(|_| invalid())?;
				if response.status() == wreq::StatusCode::PARTIAL_CONTENT {
					let range = format!("bytes {start}-{end}/{}", self.len);
					if response
						.headers()
						.get(wreq::header::CONTENT_RANGE)
						.and_then(|value| value.to_str().ok())
						!= Some(range.as_str())
					{
						return Err(invalid());
					}
				} else if !(response.status() == wreq::StatusCode::OK
					&& start == 0 && count == self.len)
				{
					return Err(io::Error::new(io::ErrorKind::Unsupported, UNSUPPORTED));
				}
				if response
					.headers()
					.get(wreq::header::CONTENT_ENCODING)
					.is_some_and(|value| value != "identity")
					|| response
						.content_length()
						.is_some_and(|len| len != count as u64)
				{
					return Err(invalid());
				}
				let mut bytes = Vec::with_capacity(count);
				let mut response_body = std::pin::pin!(response.bytes_stream());
				while let Some(chunk) = futures_util::StreamExt::next(&mut response_body)
					.await
					.transpose()
					.map_err(|_| invalid())?
				{
					if chunk.len() > count - bytes.len() {
						return Err(invalid());
					}
					bytes.extend_from_slice(&chunk);
				}
				if bytes.len() != count {
					return Err(invalid());
				}
				Ok(bytes)
			};
			tokio::select! { biased;
				_ = cancelled => Err(io::Error::new(io::ErrorKind::ConnectionAborted, "Cancelled")),
				result = transfer => result,
			}
		})?;
		self.check_cancelled()?;
		self.cache.push_back((start, bytes));
		Ok(())
	}
}
impl Read for Source {
	fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
		self.check_cancelled()?;
		let mut count = output.len().min(CHUNK).min(self.len - self.position);
		if count == 0 {
			return Ok(0);
		}
		#[cfg(feature = "demo")]
		if let Input::Demo(bytes) = self.input {
			output[..count].copy_from_slice(&bytes[self.position..self.position + count]);
			self.position += count;
			return Ok(count);
		}
		if let Some(index) = self.cache.iter().position(|(start, bytes)| {
			self.position >= *start && self.position < start + bytes.len()
		}) {
			let range = self.cache.remove(index).ok_or_else(invalid)?;
			self.cache.push_back(range);
		} else {
			self.fill()?;
		}
		let (start, bytes) = self.cache.back().ok_or_else(invalid)?;
		let offset = self.position - start;
		count = count.min(bytes.len() - offset);
		output[..count].copy_from_slice(&bytes[offset..offset + count]);
		self.position += count;
		Ok(count)
	}
}
impl Seek for Source {
	fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
		self.check_cancelled()?;
		let position = match position {
			SeekFrom::Start(position) => Some(position),
			SeekFrom::Current(offset) => (self.position as u64).checked_add_signed(offset),
			SeekFrom::End(offset) => (self.len as u64).checked_add_signed(offset),
		}
		.filter(|position| *position <= self.len as u64)
		.ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "Invalid video seek"))?;
		self.position = position as usize;
		Ok(position)
	}
}

fn is_discord_domain(url: &url::Url) -> bool {
	let Some(host) = url.host_str() else {
		return false;
	};
	let host = host.to_ascii_lowercase();
	matches!(
		host.as_str(),
		"discord.com" | "discordapp.com" | "discordapp.net" | "discord.gg" | "discord.media"
	) || host.ends_with(".discord.com")
		|| host.ends_with(".discordapp.com")
		|| host.ends_with(".discordapp.net")
		|| host.ends_with(".discord.media")
}

fn build_video_get(
	client: &wreq::Client,
	url: &url::Url,
	external_user_agent: bool,
) -> wreq::RequestBuilder {
	let req = client_core::fingerprint::discord_request(client.get(url.as_str()), url.as_str());
	if external_user_agent && !is_discord_domain(url) {
		req.header(
			wreq::header::USER_AGENT,
			client_core::fingerprint::user_agent(),
		)
	} else {
		req
	}
}

pub(super) fn is_safe_external_video_url(url: &url::Url, allow_loopback_http: bool) -> bool {
	if !url.username().is_empty() || url.password().is_some() {
		return false;
	}
	let Some(host) = url.host_str() else {
		return false;
	};
	let loopback = match url.host() {
		Some(url::Host::Ipv4(ip)) => {
			if ip.is_loopback() {
				true
			} else if ip.is_private()
				|| ip.is_link_local()
				|| ip.is_unspecified()
				|| ip.is_multicast()
				|| ip.is_broadcast()
				|| (ip.octets()[0] == 100 && (ip.octets()[1] & 0b1100_0000) == 64)
			{
				return false;
			} else {
				false
			}
		}
		Some(url::Host::Ipv6(ip)) => {
			if ip.is_loopback() {
				true
			} else if ip.is_unspecified()
				|| ip.is_multicast()
				|| ip.is_unique_local()
				|| ip.is_unicast_link_local()
			{
				return false;
			} else {
				false
			}
		}
		Some(url::Host::Domain(domain)) => {
			let lower = domain.to_ascii_lowercase();
			if lower == "localhost"
				|| lower.ends_with(".localhost")
				|| lower.ends_with(".local")
				|| lower.ends_with(".internal")
			{
				return false;
			}
			false
		}
		None => return false,
	};
	if loopback && !allow_loopback_http {
		return false;
	}
	if url.scheme() == "https" {
		return !host.is_empty();
	}
	allow_loopback_http && loopback && url.scheme() == "http"
}

struct TempVideoFile {
	file: File,
	path: PathBuf,
}

impl TempVideoFile {
	fn create() -> Result<Self, &'static str> {
		let mut random = [0_u8; 16];
		getrandom::fill(&mut random).map_err(|_| INVALID)?;
		let name: String = random.iter().map(|b| format!("{b:02x}")).collect();
		let path = std::env::temp_dir().join(format!("serein-ext-video-{name}.bin"));
		let mut opts = OpenOptions::new();
		opts.read(true).write(true).create_new(true);
		#[cfg(unix)]
		{
			use std::os::unix::fs::OpenOptionsExt;
			opts.mode(0o600);
		}
		let file = opts.open(&path).map_err(|_| INVALID)?;
		Ok(Self { file, path })
	}
}

impl Read for TempVideoFile {
	fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
		self.file.read(buf)
	}
}

impl Seek for TempVideoFile {
	fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
		self.file.seek(pos)
	}
}

impl Drop for TempVideoFile {
	fn drop(&mut self) {
		let _ = std::fs::remove_file(&self.path);
	}
}

fn is_webpage_content_type(response: &wreq::Response) -> bool {
	response
		.headers()
		.get(wreq::header::CONTENT_TYPE)
		.and_then(|v| v.to_str().ok())
		.is_some_and(|ct| {
			let lower = ct
				.split(';')
				.next()
				.unwrap_or(ct)
				.trim()
				.to_ascii_lowercase();
			matches!(
				lower.as_str(),
				"text/html" | "application/xhtml+xml" | "application/json" | "text/plain"
			)
		})
}

fn parse_content_range_total(header: &str) -> Option<usize> {
	let total = header.trim().rsplit_once('/')?.1.trim();
	total.parse::<usize>().ok().filter(|&len| len > 0)
}

enum ExternalOpen {
	Range { url: url::Url, len: usize },
	TempFile(TempVideoFile),
}

pub(super) fn open_external(
	candidates: &[url::Url],
	range_only: bool,
	cancelled: Arc<AtomicBool>,
	runtime: Handle,
) -> Result<Box<dyn platform::video::ReadSeek>, &'static str> {
	open_external_inner(candidates, range_only, false, cancelled, runtime)
}

fn open_external_inner(
	candidates: &[url::Url],
	range_only: bool,
	allow_loopback_http: bool,
	cancelled: Arc<AtomicBool>,
	runtime: Handle,
) -> Result<Box<dyn platform::video::ReadSeek>, &'static str> {
	let mut last_error = INVALID;
	for candidate in candidates {
		match probe_and_open_candidate(
			candidate,
			range_only,
			allow_loopback_http,
			&cancelled,
			&runtime,
		) {
			Ok(ExternalOpen::Range { url, len }) => {
				return source_with_options(Some(url), len, true, cancelled, runtime);
			}
			Ok(ExternalOpen::TempFile(temp)) => {
				return Ok(Box::new(temp));
			}
			Err(err) => {
				last_error = err;
				if cancelled.load(Ordering::Acquire) {
					return Err(INVALID);
				}
			}
		}
	}
	Err(last_error)
}

async fn download_bounded_to_temp(
	response: wreq::Response,
	cancelled: &AtomicBool,
) -> Result<TempVideoFile, &'static str> {
	if is_webpage_content_type(&response) {
		return Err("Video link returned a webpage instead of video");
	}
	if response
		.content_length()
		.is_some_and(|len| len > MAX_BYTES as u64)
	{
		return Err("Video preview limit: 100 MiB");
	}
	let mut temp = TempVideoFile::create()?;
	let mut written = 0_usize;
	let mut stream = std::pin::pin!(response.bytes_stream());
	while let Some(chunk) = futures_util::StreamExt::next(&mut stream)
		.await
		.transpose()
		.map_err(|_| INVALID)?
	{
		if cancelled.load(Ordering::Acquire) {
			return Err(INVALID);
		}
		if chunk.len() > MAX_BYTES - written {
			return Err("Video preview limit: 100 MiB");
		}
		temp.file.write_all(&chunk).map_err(|_| INVALID)?;
		written += chunk.len();
	}
	if written == 0 {
		return Err(INVALID);
	}
	temp.file.flush().map_err(|_| INVALID)?;
	temp.file.seek(SeekFrom::Start(0)).map_err(|_| INVALID)?;
	Ok(temp)
}

async fn fetch_full_to_temp(
	client: &wreq::Client,
	url: &url::Url,
	cancelled: &AtomicBool,
) -> Result<ExternalOpen, &'static str> {
	let full = build_video_get(client, url, true)
		.header(wreq::header::ACCEPT_ENCODING, "identity")
		.send()
		.await
		.map_err(|_| INVALID)?;
	if !full.status().is_success() {
		return Err(INVALID);
	}
	download_bounded_to_temp(full, cancelled)
		.await
		.map(ExternalOpen::TempFile)
}

fn probe_and_open_candidate(
	start: &url::Url,
	range_only: bool,
	allow_loopback_http: bool,
	cancelled: &Arc<AtomicBool>,
	runtime: &Handle,
) -> Result<ExternalOpen, &'static str> {
	if !is_safe_external_video_url(start, allow_loopback_http) {
		return Err(INVALID);
	}
	let client = wreq::Client::builder()
		.no_proxy()
		.no_gzip()
		.no_brotli()
		.no_deflate()
		.no_zstd()
		.redirect(wreq::redirect::Policy::none())
		.connect_timeout(Duration::from_secs(15))
		.timeout(Duration::from_secs(if range_only { 15 } else { 180 }))
		.build()
		.map_err(|_| INVALID)?;

	runtime.block_on(async {
		let cancelled_fut = async {
			while !cancelled.load(Ordering::Acquire) {
				tokio::time::sleep(Duration::from_millis(20)).await;
			}
		};
		let work = async {
			let mut current = start.clone();
			for _ in 0..=MAX_REDIRECTS {
				if !is_safe_external_video_url(&current, allow_loopback_http) {
					return Err(INVALID);
				}
				let response = build_video_get(&client, &current, true)
					.header(wreq::header::ACCEPT_ENCODING, "identity")
					.header(wreq::header::RANGE, "bytes=0-0")
					.send()
					.await
					.map_err(|_| INVALID)?;
				let status = response.status();
				if status.is_redirection() {
					let location = response
						.headers()
						.get(wreq::header::LOCATION)
						.and_then(|v| v.to_str().ok())
						.ok_or(INVALID)?;
					let next = current.join(location).map_err(|_| INVALID)?;
					current = next;
					continue;
				}
				if is_webpage_content_type(&response) {
					return Err("Video link returned a webpage instead of video");
				}
				if status == wreq::StatusCode::PARTIAL_CONTENT {
					let total = response
						.headers()
						.get(wreq::header::CONTENT_RANGE)
						.and_then(|v| v.to_str().ok())
						.and_then(parse_content_range_total);
					if let Some(len) = total {
						if len > MAX_BYTES {
							return Err("Video preview limit: 100 MiB");
						}
						return Ok(ExternalOpen::Range { url: current, len });
					}
					if range_only {
						return Err(UNSUPPORTED);
					}
					return fetch_full_to_temp(&client, &current, cancelled).await;
				}
				if status == wreq::StatusCode::OK {
					if range_only {
						return Err(UNSUPPORTED);
					}
					return download_bounded_to_temp(response, cancelled)
						.await
						.map(ExternalOpen::TempFile);
				}
				if matches!(
					status,
					wreq::StatusCode::RANGE_NOT_SATISFIABLE
						| wreq::StatusCode::METHOD_NOT_ALLOWED
						| wreq::StatusCode::NOT_IMPLEMENTED
				) {
					if range_only {
						return Err(UNSUPPORTED);
					}
					return fetch_full_to_temp(&client, &current, cancelled).await;
				}
				return Err(INVALID);
			}
			Err(INVALID)
		};
		tokio::select! { biased;
			_ = cancelled_fut => Err(INVALID),
			result = work => result,
		}
	})
}

#[cfg(test)]
mod tests {
	use super::*;
	use std::{io::Write, net::TcpListener, sync::atomic::AtomicUsize};

	#[test]
	fn alternating_tracks_reuse_buffered_ranges() {
		let listener = TcpListener::bind("127.0.0.1:0").unwrap();
		let address = listener.local_addr().unwrap();
		let url = url::Url::parse(&format!("http://{address}/tracks.mov")).unwrap();
		let expected = 8 * 1024 * 1024;
		let requests = Arc::new(AtomicUsize::new(0));
		let observed = requests.clone();
		let stopped = Arc::new(AtomicBool::new(false));
		let stop = stopped.clone();
		let server = std::thread::spawn(move || {
			loop {
				let (mut socket, _) = listener.accept().unwrap();
				if stop.load(Ordering::Acquire) {
					break;
				}
				socket
					.set_read_timeout(Some(Duration::from_secs(2)))
					.unwrap();
				let mut header = Vec::new();
				while !header.ends_with(b"\r\n\r\n") {
					assert!(header.len() < 8192);
					let mut byte = [0];
					socket.read_exact(&mut byte).unwrap();
					header.push(byte[0]);
				}
				let header = String::from_utf8(header).unwrap().to_ascii_lowercase();
				let range = header
					.lines()
					.find_map(|line| line.strip_prefix("range: bytes="))
					.unwrap();
				let (start, end) = range.split_once('-').unwrap();
				let start = start.parse::<usize>().unwrap();
				let end = end.parse::<usize>().unwrap();
				let count = end - start + 1;
				assert!(count <= 256 * 1024 && end < expected);
				observed.fetch_add(1, Ordering::Release);
				std::thread::sleep(Duration::from_millis(10));
				write!(socket, "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes {start}-{end}/{expected}\r\nContent-Length: {count}\r\nConnection: close\r\n\r\n").unwrap();
				socket
					.write_all(&vec![(start / (4 * 1024 * 1024)) as u8; count])
					.unwrap();
			}
		});
		let runtime = tokio::runtime::Builder::new_multi_thread()
			.worker_threads(1)
			.enable_all()
			.build()
			.unwrap();
		let mut input = source(
			Some(url),
			expected,
			Arc::new(AtomicBool::new(false)),
			runtime.handle().clone(),
		)
		.unwrap();
		assert_eq!(requests.load(Ordering::Acquire), 0);
		let started = std::time::Instant::now();
		for sample in 0..32 {
			for track in 0..2 {
				input
					.seek(SeekFrom::Start(
						(track * 4 * 1024 * 1024 + sample * 4096) as u64,
					))
					.unwrap();
				let mut bytes = [0; 4096];
				input.read_exact(&mut bytes).unwrap();
				assert!(bytes.iter().all(|byte| *byte == track as u8));
			}
		}
		let elapsed = started.elapsed();
		let count = requests.load(Ordering::Acquire);
		stopped.store(true, Ordering::Release);
		std::net::TcpStream::connect(address).unwrap();
		server.join().unwrap();
		eprintln!(
			"alternating tracks: {count} requests, {elapsed:?}, 64 reads, 10 ms response delay"
		);
		assert_eq!(count, 2);
	}

	#[test]
	fn cancellation_interrupts_a_stalled_range_body() {
		let listener = TcpListener::bind("127.0.0.1:0").unwrap();
		let url = url::Url::parse(&format!(
			"http://{}/stalled.mov",
			listener.local_addr().unwrap()
		))
		.unwrap();
		let cancelled = Arc::new(AtomicBool::new(false));
		let stop = cancelled.clone();
		let server = std::thread::spawn(move || {
			let (mut socket, _) = listener.accept().unwrap();
			socket
				.set_read_timeout(Some(Duration::from_secs(2)))
				.unwrap();
			let mut header = Vec::new();
			while !header.ends_with(b"\r\n\r\n") {
				assert!(header.len() < 8192);
				let mut byte = [0];
				socket.read_exact(&mut byte).unwrap();
				header.push(byte[0]);
			}
			write!(socket, "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes 0-{}/{CHUNK}\r\nContent-Length: {CHUNK}\r\n\r\n", CHUNK - 1).unwrap();
			std::thread::sleep(Duration::from_millis(40));
			stop.store(true, Ordering::Release);
			// Keep the response open until cancellation drops the connection.
			match socket.read(&mut [0]) {
				Ok(0) => {}
				Err(error) if error.kind() == io::ErrorKind::ConnectionReset => {}
				other => panic!("cancelled range connection stayed open: {other:?}"),
			}
		});
		let runtime = tokio::runtime::Builder::new_multi_thread()
			.worker_threads(1)
			.enable_all()
			.build()
			.unwrap();
		let mut input = source(Some(url), CHUNK, cancelled, runtime.handle().clone()).unwrap();
		assert_eq!(
			input.read(&mut [0]).unwrap_err().kind(),
			io::ErrorKind::ConnectionAborted
		);
		server.join().unwrap();
	}

	#[test]
	fn ranges_follow_reads_and_seeks_and_reject_changed_responses() {
		let listener = TcpListener::bind("127.0.0.1:0").unwrap();
		let url = url::Url::parse(&format!(
			"http://{}/synthetic.mov",
			listener.local_addr().unwrap()
		))
		.unwrap();
		let expected = CHUNK * (CACHE_CHUNKS + 1);
		let requests = Arc::new(AtomicUsize::new(0));
		let observed = requests.clone();
		let server = std::thread::spawn(move || {
			let starts = [0, CHUNK * CACHE_CHUNKS]
				.into_iter()
				.chain((1..CACHE_CHUNKS).map(|chunk| chunk * CHUNK))
				.chain([CHUNK * CACHE_CHUNKS]);
			for (index, start) in starts.enumerate() {
				let (mut socket, _) = listener.accept().unwrap();
				socket
					.set_read_timeout(Some(Duration::from_secs(2)))
					.unwrap();
				let mut header = Vec::new();
				while !header.ends_with(b"\r\n\r\n") {
					assert!(header.len() < 8192);
					let mut byte = [0];
					socket.read_exact(&mut byte).unwrap();
					header.push(byte[0]);
				}
				let header = String::from_utf8(header).unwrap().to_ascii_lowercase();
				assert!(
					header.contains(&format!("range: bytes={start}-{}\r\n", start + CHUNK - 1))
				);
				assert!(!header.contains("authorization:"));
				assert!(!header.contains("cookie:"));
				observed.fetch_add(1, Ordering::Release);
				let changed = index == CACHE_CHUNKS + 1;
				let total = expected + usize::from(changed);
				write!(socket, "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes {start}-{}/{total}\r\nContent-Length: {CHUNK}\r\nConnection: close\r\n\r\n", start + CHUNK - 1).unwrap();
				let result = socket.write_all(&vec![(start / CHUNK) as u8; CHUNK]);
				if !changed {
					result.unwrap();
				}
			}
		});
		let runtime = tokio::runtime::Builder::new_multi_thread()
			.worker_threads(1)
			.enable_all()
			.build()
			.unwrap();
		let cancelled = Arc::new(AtomicBool::new(false));
		let mut input = source(
			Some(url.clone()),
			expected,
			cancelled.clone(),
			runtime.handle().clone(),
		)
		.unwrap();
		assert_eq!(requests.load(Ordering::Acquire), 0);
		let mut bytes = [1; 32];
		input.read_exact(&mut bytes).unwrap();
		assert_eq!(bytes, [0; 32]);
		assert_eq!(requests.load(Ordering::Acquire), 1);
		input.seek(SeekFrom::Start(8)).unwrap();
		input.read_exact(&mut bytes).unwrap();
		assert_eq!(requests.load(Ordering::Acquire), 1);
		input.seek(SeekFrom::End(-32)).unwrap();
		input.read_exact(&mut bytes).unwrap();
		assert_eq!(bytes, [CACHE_CHUNKS as u8; 32]);
		assert_eq!(requests.load(Ordering::Acquire), 2);
		assert_eq!(input.read(&mut bytes).unwrap(), 0);
		assert!(input.seek(SeekFrom::End(1)).is_err());
		assert!(input.seek(SeekFrom::Start(u64::MAX)).is_err());
		assert!(input.seek(SeekFrom::Current(i64::MIN)).is_err());
		for chunk in 1..CACHE_CHUNKS {
			// Keep the first range hot while loading enough others to evict the tail.
			input.seek(SeekFrom::Start(0)).unwrap();
			input.read_exact(&mut bytes).unwrap();
			assert_eq!(bytes, [0; 32]);
			input.seek(SeekFrom::Start((chunk * CHUNK) as u64)).unwrap();
			input.read_exact(&mut bytes).unwrap();
			assert_eq!(bytes, [chunk as u8; 32]);
		}
		assert_eq!(requests.load(Ordering::Acquire), CACHE_CHUNKS + 1);
		input.seek(SeekFrom::Start(0)).unwrap();
		input.read_exact(&mut bytes).unwrap();
		assert_eq!(bytes, [0; 32]);
		input.seek(SeekFrom::End(-32)).unwrap();
		assert_eq!(
			input.read(&mut bytes).unwrap_err().kind(),
			io::ErrorKind::InvalidData
		);
		assert_eq!(requests.load(Ordering::Acquire), CACHE_CHUNKS + 2);
		server.join().unwrap();
		cancelled.store(true, Ordering::Release);
		assert_eq!(
			input.read(&mut bytes).unwrap_err().kind(),
			io::ErrorKind::ConnectionAborted
		);
		assert!(
			source(
				Some(url),
				MAX_BYTES + 1,
				cancelled,
				runtime.handle().clone()
			)
			.is_err()
		);
	}

	#[test]
	fn external_video_url_safety_rejects_private_and_insecure_hosts() {
		for bad in [
			"http://x0.at/RwV9.mov",
			"https://127.0.0.1/clip.mp4",
			"https://10.0.0.1/clip.mp4",
			"https://192.168.1.50/clip.mp4",
			"https://169.254.169.254/latest/meta-data",
			"https://localhost/clip.mp4",
			"https://user:pass@x0.at/RwV9.mov",
		] {
			let parsed = url::Url::parse(bad).unwrap();
			assert!(
				!is_safe_external_video_url(&parsed, false),
				"should reject {bad}"
			);
		}
		for good in [
			"https://x0.at/RwV9.mov",
			"https://files.catbox.moe/sample.mp4",
			"https://images-ext-1.discordapp.net/external/abc/https/x0.at/RwV9.mov",
		] {
			let parsed = url::Url::parse(good).unwrap();
			assert!(
				is_safe_external_video_url(&parsed, false),
				"should accept {good}"
			);
		}
	}

	#[test]
	fn open_external_handles_redirect_range_and_non_range_fallback() {
		let listener = TcpListener::bind("127.0.0.1:0").unwrap();
		let address = listener.local_addr().unwrap();
		let start_url = url::Url::parse(&format!("http://{address}/redirect.mov")).unwrap();
		let payload = b"0123456789abcdefMOVDATA!";
		let server = std::thread::spawn(move || {
			for step in 0..4 {
				let (mut socket, _) = listener.accept().unwrap();
				socket
					.set_read_timeout(Some(Duration::from_secs(2)))
					.unwrap();
				let mut header = Vec::new();
				while !header.ends_with(b"\r\n\r\n") {
					let mut b = [0];
					socket.read_exact(&mut b).unwrap();
					header.push(b[0]);
				}
				let req = String::from_utf8(header).unwrap();
				let path = req.lines().next().unwrap_or("");
				match step {
					0 => {
						// Redirect to /range.mov
						assert!(path.starts_with("GET /redirect.mov"));
						write!(
							socket,
							"HTTP/1.1 302 Found\r\nLocation: /range.mov\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
						)
						.unwrap();
					}
					1 => {
						// Range probe 0-0
						assert!(path.starts_with("GET /range.mov"));
						write!(
							socket,
							"HTTP/1.1 206 Partial Content\r\nContent-Range: bytes 0-0/{}\r\nContent-Length: 1\r\nConnection: close\r\n\r\n",
							payload.len()
						)
						.unwrap();
						socket.write_all(&payload[..1]).unwrap();
					}
					2 => {
						// Full range read
						write!(
							socket,
							"HTTP/1.1 206 Partial Content\r\nContent-Range: bytes 0-{}/{}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
							payload.len() - 1,
							payload.len(),
							payload.len()
						)
						.unwrap();
						socket.write_all(payload).unwrap();
					}
					_ => {
						// Non-range server returns 200 OK
						write!(
							socket,
							"HTTP/1.1 200 OK\r\nContent-Type: video/quicktime\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
							payload.len()
						)
						.unwrap();
						socket.write_all(payload).unwrap();
					}
				}
			}
		});
		let runtime = tokio::runtime::Builder::new_multi_thread()
			.worker_threads(1)
			.enable_all()
			.build()
			.unwrap();
		let cancelled = Arc::new(AtomicBool::new(false));
		let mut range_reader = open_external_inner(
			std::slice::from_ref(&start_url),
			true,
			true,
			cancelled.clone(),
			runtime.handle().clone(),
		)
		.unwrap();
		let mut out = Vec::new();
		range_reader.read_to_end(&mut out).unwrap();
		assert_eq!(out, payload);

		let full_url = url::Url::parse(&format!("http://{address}/full.mov")).unwrap();
		let mut temp_reader = open_external_inner(
			std::slice::from_ref(&full_url),
			false,
			true,
			cancelled,
			runtime.handle().clone(),
		)
		.unwrap();
		let mut out2 = Vec::new();
		temp_reader.read_to_end(&mut out2).unwrap();
		assert_eq!(out2, payload);
		server.join().unwrap();
	}
}
