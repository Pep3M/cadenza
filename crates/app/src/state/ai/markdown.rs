//! The little markdown the chat renders: headings, paragraphs, bullet and numbered
//! lists, fenced code blocks, and inline **bold**, *italic* and `code`.

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Span {
	pub text: String,
	pub bold: bool,
	pub italic: bool,
	pub code: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Block {
	Heading(Vec<Span>),
	Paragraph(Vec<Span>),
	/// A list item: its marker ("•", "1.") and text, indented by `level`.
	Item {
		marker: String,
		level: usize,
		spans: Vec<Span>,
	},
	Code(String),
}

pub fn parse(text: &str) -> Vec<Block> {
	let mut blocks = Vec::new();
	let mut paragraph: Vec<String> = Vec::new();
	let mut code: Option<Vec<String>> = None;
	let flush = |paragraph: &mut Vec<String>, blocks: &mut Vec<Block>| {
		if !paragraph.is_empty() {
			blocks.push(Block::Paragraph(inline(&paragraph.join(" "))));
			paragraph.clear();
		}
	};
	for line in text.lines() {
		if let Some(lines) = &mut code {
			if line.trim_start().starts_with("```") {
				blocks.push(Block::Code(lines.join("\n")));
				code = None;
			} else {
				lines.push(line.to_owned());
			}
			continue;
		}
		let trimmed = line.trim_start();
		let level = (line.len() - trimmed.len()) / 2;
		if trimmed.starts_with("```") {
			flush(&mut paragraph, &mut blocks);
			code = Some(Vec::new());
		} else if trimmed.is_empty() {
			flush(&mut paragraph, &mut blocks);
		} else if let Some(rest) = trimmed
			.strip_prefix("### ")
			.or_else(|| trimmed.strip_prefix("## "))
			.or_else(|| trimmed.strip_prefix("# "))
		{
			flush(&mut paragraph, &mut blocks);
			blocks.push(Block::Heading(inline(rest)));
		} else if let Some(rest) = trimmed
			.strip_prefix("- ")
			.or_else(|| trimmed.strip_prefix("* "))
			.or_else(|| trimmed.strip_prefix("• "))
		{
			flush(&mut paragraph, &mut blocks);
			blocks.push(Block::Item {
				marker: "•".into(),
				level,
				spans: inline(rest),
			});
		} else if let Some((number, rest)) = numbered(trimmed) {
			flush(&mut paragraph, &mut blocks);
			blocks.push(Block::Item {
				marker: format!("{number}."),
				level,
				spans: inline(rest),
			});
		} else {
			paragraph.push(trimmed.to_owned());
		}
	}
	if let Some(lines) = code {
		blocks.push(Block::Code(lines.join("\n")));
	}
	flush(&mut paragraph, &mut blocks);
	blocks
}

fn numbered(line: &str) -> Option<(&str, &str)> {
	let digits = line.find(|c: char| !c.is_ascii_digit())?;
	if digits == 0 || digits > 3 {
		return None;
	}
	let rest = line[digits..]
		.strip_prefix(". ")
		.or_else(|| line[digits..].strip_prefix(") "))?;
	Some((&line[..digits], rest))
}

/// Inline emphasis and code. Unclosed markers are kept as text.
pub fn inline(text: &str) -> Vec<Span> {
	let mut spans: Vec<Span> = Vec::new();
	let mut current = Span::default();
	let mut rest = text;
	let push = |spans: &mut Vec<Span>, span: &mut Span| {
		if !span.text.is_empty() {
			spans.push(std::mem::take(span));
		}
	};
	while !rest.is_empty() {
		if let Some(after) = rest.strip_prefix('`')
			&& let Some(end) = after.find('`')
		{
			push(&mut spans, &mut current);
			spans.push(Span {
				text: after[..end].to_owned(),
				code: true,
				..Span::default()
			});
			rest = &after[end + 1..];
			continue;
		}
		if let Some(after) = rest.strip_prefix("**")
			&& let Some(end) = after.find("**")
			&& end > 0
		{
			push(&mut spans, &mut current);
			for mut s in inline(&after[..end]) {
				s.bold = true;
				spans.push(s);
			}
			rest = &after[end + 2..];
			continue;
		}
		if let Some(after) = rest.strip_prefix('*')
			&& !after.starts_with([' ', '*'])
			&& let Some(end) = after.find('*')
			&& end > 0
		{
			push(&mut spans, &mut current);
			for mut s in inline(&after[..end]) {
				s.italic = true;
				spans.push(s);
			}
			rest = &after[end + 1..];
			continue;
		}
		let ch = rest.chars().next().expect("not empty");
		current.text.push(ch);
		rest = &rest[ch.len_utf8()..];
	}
	push(&mut spans, &mut current);
	spans
}

#[cfg(test)]
mod tests {
	use super::*;

	fn plain(spans: &[Span]) -> String {
		spans.iter().map(|s| s.text.as_str()).collect()
	}

	#[test]
	fn blocks_and_inline_styles() {
		let blocks = parse(
			"## Done\nAdded a **bass** line on `Bass`,\nin *A minor*.\n\n- one\n  - nested\n2. two\n```\nC4 E4\n```\nend **open",
		);
		assert_eq!(blocks.len(), 7, "{blocks:?}");
		assert!(matches!(&blocks[0], Block::Heading(s) if plain(s) == "Done"));
		let Block::Paragraph(p) = &blocks[1] else {
			panic!()
		};
		assert_eq!(plain(p), "Added a bass line on Bass, in A minor.");
		assert!(p.iter().any(|s| s.bold && s.text == "bass"));
		assert!(p.iter().any(|s| s.code && s.text == "Bass"));
		assert!(p.iter().any(|s| s.italic && s.text == "A minor"));
		assert!(matches!(&blocks[2], Block::Item { marker, level: 0, .. } if marker == "•"));
		assert!(matches!(&blocks[3], Block::Item { level: 1, .. }));
		assert!(matches!(&blocks[4], Block::Item { marker, .. } if marker == "2."));
		assert_eq!(blocks[5], Block::Code("C4 E4".into()));
		// The unclosed marker stays text (in the trailing paragraph).
		let last = parse("end **open");
		assert!(matches!(&last[0], Block::Paragraph(s) if plain(s) == "end **open"));
		// A list marker without a space is text, as is "3 * 4".
		assert!(matches!(&parse("3 * 4")[0], Block::Paragraph(s) if plain(s) == "3 * 4"));
	}
}
