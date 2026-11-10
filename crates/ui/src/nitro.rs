use client_core::State;
use model::Id;

/// Converts custom emoji tags (`<a:name:id>`, `<:name:id>`) and typed shortcodes (`:name:`)
/// into Markdown image shares if they require Nitro.
pub fn convert_draft(draft: &str, state: &State, channel: Id) -> String {
	let mut step1 = convert_custom_tags(draft, state, channel);
	convert_shortcodes(&mut step1, state, channel);
	step1
}

fn convert_custom_tags(draft: &str, state: &State, channel: Id) -> String {
	if state.premium_type != 0 {
		return draft.to_owned();
	}
	let mut result = String::with_capacity(draft.len());
	let mut remaining = draft;
	while let Some(start) = remaining.find('<') {
		result.push_str(&remaining[..start]);
		remaining = &remaining[start..];
		let is_animated = remaining.starts_with("<a:");
		let is_static = remaining.starts_with("<:");
		if (is_animated || is_static)
			&& let Some(end) = remaining.find('>')
		{
			let tag = &remaining[..=end];
			let inner = if is_animated {
				&tag[3..tag.len() - 1]
			} else {
				&tag[2..tag.len() - 1]
			};
			if let Some((name, id_str)) = inner.split_once(':')
				&& let Ok(id_u64) = id_str.parse::<u64>()
			{
				let id = Id(id_u64);
				let custom = state.custom_emoji(id);
				let is_animated = is_animated || custom.is_some_and(|(_, e)| e.animated);
				let requires_nitro = is_animated || {
					match custom {
						Some((guild, emoji)) => {
							state.custom_emoji_requires_nitro(channel, guild.id, emoji)
						}
						None => true,
					}
				};
				if requires_nitro {
					if let Some(md) = (model::ImageShare::Emoji {
						id,
						animated: is_animated,
					})
					.markdown(name)
					{
						result.push_str(&md);
						remaining = &remaining[end + 1..];
						continue;
					}
				}
			}
		}
		result.push('<');
		remaining = &remaining[1..];
	}
	result.push_str(remaining);
	result
}

fn convert_shortcodes(draft: &mut String, state: &State, channel: Id) {
	let mut result = String::with_capacity(draft.len());
	let mut remaining = draft.as_str();
	let mut changed = false;
	while let Some(start) = remaining.find(':') {
		result.push_str(&remaining[..start]);
		remaining = &remaining[start..];
		if let Some(end) = remaining[1..].find(':') {
			let candidate_name = &remaining[1..=end];
			if candidate_name.len() >= 2
				&& candidate_name.len() <= 32
				&& candidate_name
					.chars()
					.all(|c| c.is_ascii_alphanumeric() || c == '_')
			{
				let found = state.guilds.iter().find_map(|guild| {
					guild
						.emojis
						.iter()
						.flatten()
						.find(|e| e.name == candidate_name && e.valid() && e.available)
						.map(|e| (guild, e))
				});
				if let Some((guild, emoji)) = found {
					let can_send_native =
						state.can_send_custom_emoji(channel, guild.id, emoji);
					if can_send_native {
						result.push_str(&format!(
							"<{}:{}:{}>",
							if emoji.animated { "a" } else { "" },
							emoji.name,
							emoji.id
						));
					} else if let Some(md) = (model::ImageShare::Emoji {
						id: emoji.id,
						animated: emoji.animated,
					})
					.markdown(&emoji.name)
					{
						result.push_str(&md);
					} else {
						result.push(':');
						result.push_str(candidate_name);
						result.push(':');
					}
					remaining = &remaining[end + 2..];
					changed = true;
					continue;
				}
			}
		}
		result.push(':');
		remaining = &remaining[1..];
	}
	result.push_str(remaining);
	if changed {
		*draft = result;
	}
}

/// Unmasks `[name](URL)` markdown links into direct URLs for mixed text messages,
/// allowing standard Discord clients to embed the image inline.
pub fn unmask_artwork_links(draft: &str) -> String {
	let mut result = String::with_capacity(draft.len());
	let mut remaining = draft;
	while let Some(start) = remaining.find('[') {
		result.push_str(&remaining[..start]);
		remaining = &remaining[start..];
		if let Some((asset, _, len)) = model::ImageShare::markdown_prefix(remaining) {
			if let Some(url) = asset.url() {
				if !result.is_empty() && !result.ends_with(' ') && !result.ends_with('\n') {
					result.push(' ');
				}
				result.push_str(&url);
				let tail = &remaining[len..];
				if !tail.is_empty() && !tail.starts_with(' ') && !tail.starts_with('\n') {
					result.push(' ');
				}
				remaining = tail;
				continue;
			}
		}
		result.push('[');
		remaining = &remaining[1..];
	}
	result.push_str(remaining);
	result
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn unmask_artwork_links_converts_emoji_and_sticker_links() {
		let raw = "Hello [serein_party](https://cdn.discordapp.com/emojis/9002.gif?size=64) test [Wave \\[hello\\]](https://cdn.discordapp.com/stickers/9201.png)!";
		let unmasked = unmask_artwork_links(raw);
		assert_eq!(
			unmasked,
			"Hello https://cdn.discordapp.com/emojis/9002.gif?size=64 test https://cdn.discordapp.com/stickers/9201.png !"
		);
	}

	#[test]
	fn unmask_artwork_links_preserves_ordinary_markdown_links() {
		let raw = "Check [website](https://example.com) for details!";
		let unmasked = unmask_artwork_links(raw);
		assert_eq!(unmasked, raw);
	}

	#[test]
	fn convert_custom_tags_converts_animated_emoji_when_non_nitro() {
		let mut state = client_core::State::default();
		state.premium_type = 0;
		let converted = convert_custom_tags("Hello <a:blob_dance:12345678>!", &state, Id(1));
		assert_eq!(
			converted,
			"Hello [blob\\_dance](https://cdn.discordapp.com/emojis/12345678.gif?size=64)!"
		);
	}

	#[test]
	fn convert_custom_tags_preserves_when_has_nitro() {
		let mut state = client_core::State::default();
		state.premium_type = 2;
		let input = "Hello <a:blob_dance:12345678>!";
		let converted = convert_custom_tags(input, &state, Id(1));
		assert_eq!(converted, input);
	}
}
