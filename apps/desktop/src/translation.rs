//! Background translation service using Google Translate GTX endpoint.

/// Translates text to Vietnamese using the Google Translate GTX endpoint.
/// Returns Ok((translated_text, detected_source_lang)) on success.
pub async fn translate(client: &wreq::Client, text: &str) -> Result<(String, String), String> {
	let encoded: String = url::form_urlencoded::byte_serialize(text.as_bytes()).collect();
	let url = format!(
		"https://translate.googleapis.com/translate_a/single?client=gtx&sl=auto&tl=vi&dt=t&q={encoded}"
	);

	let response = client
		.get(&url)
		.header(
			wreq::header::USER_AGENT,
			"Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36",
		)
		.send()
		.await
		.map_err(|e| format!("Lỗi kết nối: {e}"))?;

	if !response.status().is_success() {
		return Err(format!("Máy chủ dịch trả về mã: {}", response.status()));
	}

	let body = response
		.text()
		.await
		.map_err(|e| format!("Lỗi đọc dữ liệu: {e}"))?;

	parse_gtx_response(&body)
}

/// Parses the raw JSON response returned by Google Translate GTX endpoint.
pub fn parse_gtx_response(body: &str) -> Result<(String, String), String> {
	let value: serde_json::Value =
		serde_json::from_str(body).map_err(|e| format!("Lỗi phân tích JSON: {e}"))?;

	let mut translated = String::new();
	if let Some(sentences) = value.get(0).and_then(|v| v.as_array()) {
		for sentence in sentences {
			if let Some(part) = sentence.get(0).and_then(|v| v.as_str()) {
				translated.push_str(part);
			}
		}
	}

	if translated.is_empty() {
		return Err("Bản dịch rỗng".to_string());
	}

	let source_lang = value
		.get(2)
		.and_then(|v| v.as_str())
		.unwrap_or("auto")
		.to_string();

	Ok((translated, source_lang))
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn test_parse_gtx_single_sentence() {
		let json = r#"[[["anh bạn không thể là zombie đó","bro cannot be that zombie",null,null,3]] ,null,"en"]"#;
		let (translated, lang) = parse_gtx_response(json).unwrap();
		assert_eq!(translated, "anh bạn không thể là zombie đó");
		assert_eq!(lang, "en");
	}

	#[test]
	fn test_parse_gtx_multi_sentence() {
		let json = r#"[[["Xin chào thế giới. ","Hello world. ",null,null,3],["Bạn khỏe không?","How are you?",null,null,10]],null,"en"]"#;
		let (translated, lang) = parse_gtx_response(json).unwrap();
		assert_eq!(translated, "Xin chào thế giới. Bạn khỏe không?");
		assert_eq!(lang, "en");
	}

	#[test]
	fn test_parse_gtx_korean() {
		let json = r#"[[["Xin chào","안녕하세요",null,null,10]],null,"ko"]"#;
		let (translated, lang) = parse_gtx_response(json).unwrap();
		assert_eq!(translated, "Xin chào");
		assert_eq!(lang, "ko");
	}
}
