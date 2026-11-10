//! Quick Translate Inline (Dịch tin nhắn tức thì) manager and state.
use model::Id;
use std::collections::{HashMap, HashSet};

/// Status of a message translation.
#[derive(Clone, Debug, PartialEq)]
pub enum TranslationStatus {
	/// Translation is in flight.
	Loading,
	/// Translation completed successfully.
	Success {
		translated: String,
		source_lang: String,
		source_lang_name: String,
		visible: bool,
	},
	/// Translation failed with an error message.
	Error(String),
}

/// Manages message translations and requests.
#[derive(Default)]
pub struct TranslationManager {
	/// Cached translations by message ID.
	pub translations: HashMap<Id, TranslationStatus>,
	/// In-flight message IDs to prevent duplicate requests.
	pub in_flight: HashSet<Id>,
	/// Queue of translation requests for the desktop worker.
	pub requests: Vec<(Id, String)>,
	/// Whether foreign messages should be automatically translated.
	pub auto_translate: bool,
}

impl TranslationManager {
	/// Returns friendly Vietnamese name for a language code.
	pub fn language_name(code: &str) -> String {
		match code.to_ascii_lowercase().as_str() {
			"en" => "tiếng Anh".to_string(),
			"ko" => "tiếng Hàn".to_string(),
			"ru" => "tiếng Nga".to_string(),
			"ja" => "tiếng Nhật".to_string(),
			"zh" | "zh-cn" | "zh-hans" => "tiếng Trung".to_string(),
			"zh-tw" | "zh-hant" => "tiếng Trung (Phồn thể)".to_string(),
			"fr" => "tiếng Pháp".to_string(),
			"de" => "tiếng Đức".to_string(),
			"es" => "tiếng Tây Ban Nha".to_string(),
			"pt" => "tiếng Bồ Đào Nha".to_string(),
			"it" => "tiếng Ý".to_string(),
			"th" => "tiếng Thái".to_string(),
			"id" => "tiếng Indonesia".to_string(),
			"ms" => "tiếng Malaysia".to_string(),
			"ar" => "tiếng Ả Rập".to_string(),
			"hi" => "tiếng Hindi".to_string(),
			"vi" => "tiếng Việt".to_string(),
			other => format!("tiếng {}", other),
		}
	}

	/// Checks if the text has distinct Vietnamese diacritics.
	pub fn is_vietnamese_text(text: &str) -> bool {
		const VN_CHARS: &str = "àáảãạăằắẳẵặâầấẩẫậèéẻẽẹêềếểễệìíỉĩịòóỏõọôồốổỗộơờớởỡợùúủũụưừứửữựỳýỷỹỵđÀÁẢÃẠĂẰẮẲẴẶÂẦẤẨẪẬÈÉẺẼẸÊỀẾỂỄỆÌÍỈĨỊÒÓỎÕỌÔỒỐỔỖỘƠỜỚỞỠỢÙÚỦŨỤƯỪỨỬỮỰỲÝỶỸỴĐ";
		text.chars().any(|c| VN_CHARS.contains(c))
	}

	/// Whether a message text should be translated.
	pub fn should_translate(text: &str) -> bool {
		let trimmed = text.trim();
		if trimmed.is_empty() {
			return false;
		}
		// Skip single standalone URLs
		if (trimmed.starts_with("http://") || trimmed.starts_with("https://"))
			&& !trimmed.contains(' ')
		{
			return false;
		}
		// Must have at least one alphabetic character
		trimmed.chars().any(char::is_alphabetic)
	}

	/// Toggle translation visibility for a message, or request it if not yet fetched.
	pub fn toggle(&mut self, id: Id, content: &str) {
		if let Some(status) = self.translations.get_mut(&id) {
			match status {
				TranslationStatus::Success { visible, .. } => {
					*visible = !*visible;
				}
				TranslationStatus::Error(_) => {
					*status = TranslationStatus::Loading;
					self.in_flight.insert(id);
					self.requests.push((id, content.to_owned()));
				}
				TranslationStatus::Loading => {}
			}
		} else {
			self.translations.insert(id, TranslationStatus::Loading);
			self.in_flight.insert(id);
			self.requests.push((id, content.to_owned()));
		}
	}

	/// Hide translation for a message.
	pub fn hide(&mut self, id: Id) {
		if let Some(TranslationStatus::Success { visible, .. }) = self.translations.get_mut(&id) {
			*visible = false;
		}
	}

	/// Retry a failed translation.
	pub fn retry(&mut self, id: Id, content: &str) {
		self.translations.insert(id, TranslationStatus::Loading);
		self.in_flight.insert(id);
		self.requests.push((id, content.to_owned()));
	}

	/// Request translation if not already in-flight or completed.
	pub fn request_if_needed(&mut self, id: Id, content: &str) {
		if !Self::should_translate(content) {
			return;
		}
		if !self.translations.contains_key(&id) && !self.in_flight.contains(&id) {
			self.translations.insert(id, TranslationStatus::Loading);
			self.in_flight.insert(id);
			self.requests.push((id, content.to_owned()));
		}
	}

	/// Receive translation result from worker.
	pub fn receive(&mut self, id: Id, result: Result<(String, String), String>) {
		self.in_flight.remove(&id);
		match result {
			Ok((translated, source_lang)) => {
				let is_vietnamese = source_lang.eq_ignore_ascii_case("vi");
				let source_lang_name = Self::language_name(&source_lang);
				// If source was already Vietnamese, don't show inline card by default
				let visible = !is_vietnamese;
				self.translations.insert(
					id,
					TranslationStatus::Success {
						translated,
						source_lang,
						source_lang_name,
						visible,
					},
				);
			}
			Err(err) => {
				self.translations.insert(id, TranslationStatus::Error(err));
			}
		}
	}

	/// Returns whether a message is currently loading translation.
	pub fn is_loading(&self, id: Id) -> bool {
		matches!(self.translations.get(&id), Some(TranslationStatus::Loading))
	}

	/// Returns whether translation is currently visible for a message.
	pub fn is_visible(&self, id: Id) -> bool {
		matches!(
			self.translations.get(&id),
			Some(TranslationStatus::Success { visible: true, .. })
		)
	}

	/// Generates a height key hash for layout caching.
	pub fn height_key(&self, id: Id) -> u64 {
		match self.translations.get(&id) {
			Some(TranslationStatus::Loading) => 1,
			Some(TranslationStatus::Success {
				translated,
				visible: true,
				..
			}) => {
				let mut hash: u64 = 2;
				for b in translated.as_bytes() {
					hash = hash.wrapping_mul(31).wrapping_add(*b as u64);
				}
				hash
			}
			Some(TranslationStatus::Error(err)) => {
				let mut hash: u64 = 3;
				for b in err.as_bytes() {
					hash = hash.wrapping_mul(31).wrapping_add(*b as u64);
				}
				hash
			}
			_ => 0,
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn test_language_names() {
		assert_eq!(TranslationManager::language_name("en"), "tiếng Anh");
		assert_eq!(TranslationManager::language_name("ko"), "tiếng Hàn");
		assert_eq!(TranslationManager::language_name("ru"), "tiếng Nga");
		assert_eq!(TranslationManager::language_name("ja"), "tiếng Nhật");
		assert_eq!(TranslationManager::language_name("vi"), "tiếng Việt");
	}

	#[test]
	fn test_is_vietnamese_text() {
		assert!(TranslationManager::is_vietnamese_text("Xin chào các bạn"));
		assert!(TranslationManager::is_vietnamese_text(
			"Tôi đang thử nghiệm"
		));
		assert!(!TranslationManager::is_vietnamese_text(
			"Hello world this is english"
		));
		assert!(!TranslationManager::is_vietnamese_text("안녕하세요"));
	}

	#[test]
	fn test_should_translate() {
		assert!(TranslationManager::should_translate("Hello world"));
		assert!(!TranslationManager::should_translate(""));
		assert!(!TranslationManager::should_translate("   "));
		assert!(!TranslationManager::should_translate("https://google.com"));
		assert!(!TranslationManager::should_translate("123456"));
	}

	#[test]
	fn test_toggle_and_receive() {
		let mut manager = TranslationManager::default();
		let id = Id(100);

		manager.toggle(id, "Hello");
		assert!(manager.is_loading(id));
		assert_eq!(manager.requests.len(), 1);

		manager.receive(id, Ok(("Xin chào".into(), "en".into())));
		assert!(!manager.is_loading(id));
		assert!(manager.is_visible(id));

		// Toggle to hide
		manager.toggle(id, "Hello");
		assert!(!manager.is_visible(id));

		// Toggle to show again without new request
		manager.toggle(id, "Hello");
		assert!(manager.is_visible(id));
		assert_eq!(manager.requests.len(), 1); // No new request enqueued
	}
}
