//! Low-level HTML scanning primitives: whitespace, comments, tag attributes.

use crate::consts::*;
use crate::entities::decode_html_attribute_entities;
use crate::types::Attributes;

pub(crate) const MAX_ATTRIBUTE_BYTES: usize = 64 * 1024;
pub(crate) const MAX_ATTRIBUTES_PER_ELEMENT: usize = 256;

/// Whitespace check optimized for the hot character loop.
/// Uses a 33-bit bitmap: space(32), CR(13), LF(10), TAB(9).
#[inline(always)]
pub(crate) fn is_whitespace(c: u8) -> bool {
  if c > 32 {
    return false;
  }
  // Bitmap: bit 9 (tab), bit 10 (LF), bit 12 (FF), bit 13 (CR), bit 32 (space)
  const MASK: u64 = (1u64 << 9) | (1u64 << 10) | (1u64 << 12) | (1u64 << 13) | (1u64 << 32);
  (MASK >> c) & 1 == 1
}

pub(crate) struct CommentResult {
  pub(crate) complete: bool,
  pub(crate) new_position: usize,
}

pub(crate) fn process_bogus_comment(html_chunk: &str, position: usize) -> CommentResult {
  let bytes = html_chunk.as_bytes();
  let mut i = position + 2;
  while i < bytes.len() {
    if bytes[i] == GT_CHAR {
      return CommentResult {
        complete: true,
        new_position: i + 1,
      };
    }
    i += 1;
  }
  CommentResult {
    complete: false,
    new_position: position,
  }
}

pub(crate) fn process_comment_or_doctype(html_chunk: &str, position: usize) -> CommentResult {
  let mut i = position;
  let bytes = html_chunk.as_bytes();
  let chunk_length = bytes.len();

  if i + 3 < chunk_length && bytes[i + 2] == DASH_CHAR && bytes[i + 3] == DASH_CHAR {
    i += 4;
    const START: u8 = 0;
    const START_DASH: u8 = 1;
    const COMMENT: u8 = 2;
    const END_DASH: u8 = 3;
    const END: u8 = 4;
    const END_BANG: u8 = 5;
    let mut state = START;
    while i < chunk_length {
      let c = bytes[i];
      state = match state {
        START if c == GT_CHAR => {
          return CommentResult {
            complete: true,
            new_position: i + 1,
          };
        }
        START if c == DASH_CHAR => START_DASH,
        START => COMMENT,
        START_DASH if c == GT_CHAR => {
          return CommentResult {
            complete: true,
            new_position: i + 1,
          };
        }
        START_DASH if c == DASH_CHAR => END,
        START_DASH => COMMENT,
        COMMENT if c == DASH_CHAR => END_DASH,
        COMMENT => COMMENT,
        END_DASH if c == DASH_CHAR => END,
        END_DASH => COMMENT,
        END if c == GT_CHAR => {
          return CommentResult {
            complete: true,
            new_position: i + 1,
          };
        }
        END if c == b'!' => END_BANG,
        END if c == DASH_CHAR => END,
        END => COMMENT,
        END_BANG if c == GT_CHAR => {
          return CommentResult {
            complete: true,
            new_position: i + 1,
          };
        }
        END_BANG if c == DASH_CHAR => END_DASH,
        END_BANG => COMMENT,
        _ => unreachable!(),
      };
      i += 1;
    }
    CommentResult {
      complete: false,
      new_position: position,
    }
  } else {
    process_bogus_comment(html_chunk, position)
  }
}

pub(crate) fn process_tag_attributes(
  html_chunk: &str,
  position: usize,
  skip_attrs: bool,
) -> (bool, usize, Attributes, bool) {
  let mut i = position;
  let bytes = html_chunk.as_bytes();
  let chunk_length = bytes.len();

  const BEFORE_NAME: u8 = 0;
  const NAME: u8 = 1;
  const AFTER_NAME: u8 = 2;
  const BEFORE_VALUE: u8 = 3;
  const QUOTED_VALUE: u8 = 4;
  const AFTER_QUOTED_VALUE: u8 = 5;
  const UNQUOTED_VALUE: u8 = 6;

  let mut state = BEFORE_NAME;
  let mut quote_char: u8 = 0;
  let attr_start_pos = i;

  while i < chunk_length {
    let c = bytes[i];

    let self_closing = c == SLASH_CHAR
      && matches!(state, BEFORE_NAME | NAME | AFTER_NAME | AFTER_QUOTED_VALUE)
      && i + 1 < chunk_length
      && bytes[i + 1] == GT_CHAR;
    if self_closing {
      let attrs = if skip_attrs {
        Attributes::new()
      } else {
        parse_attributes(html_chunk[attr_start_pos..i].trim())
      };
      return (true, i + 2, attrs, true);
    }
    if c == GT_CHAR && state != QUOTED_VALUE {
      let attrs = if skip_attrs {
        Attributes::new()
      } else {
        parse_attributes(html_chunk[attr_start_pos..i].trim())
      };
      return (true, i + 1, attrs, false);
    }

    match state {
      BEFORE_NAME => {
        if !is_whitespace(c) {
          state = NAME;
        }
      }
      NAME => {
        if is_whitespace(c) {
          state = AFTER_NAME;
        } else if c == EQUALS_CHAR {
          state = BEFORE_VALUE;
        }
      }
      AFTER_NAME => {
        if c == EQUALS_CHAR {
          state = BEFORE_VALUE;
        } else if !is_whitespace(c) {
          state = NAME;
        }
      }
      BEFORE_VALUE => {
        if c == QUOTE_CHAR || c == APOS_CHAR {
          state = QUOTED_VALUE;
          quote_char = c;
        } else if !is_whitespace(c) {
          state = UNQUOTED_VALUE;
        }
      }
      QUOTED_VALUE => {
        if c == quote_char {
          state = AFTER_QUOTED_VALUE;
        }
      }
      AFTER_QUOTED_VALUE => {
        if is_whitespace(c) {
          state = BEFORE_NAME;
        } else {
          state = NAME;
        }
      }
      UNQUOTED_VALUE => {
        if is_whitespace(c) {
          state = BEFORE_NAME;
        }
      }
      _ => unreachable!(),
    }

    i += 1;
  }

  (false, i, Attributes::new(), false)
}

#[allow(clippy::collapsible_match)]
pub(crate) fn parse_attributes(attr_str: &str) -> Attributes {
  if attr_str.is_empty() {
    return Attributes::new();
  }
  if attr_str.len() > MAX_ATTRIBUTE_BYTES {
    let mut result = Attributes::new();
    result.mark_limit_exceeded();
    return result;
  }
  let mut result = Attributes::with_capacity(4);

  let bytes = attr_str.as_bytes();
  let len = bytes.len();
  let mut i = 0;

  const WHITESPACE: u8 = 0;
  const NAME: u8 = 1;
  const AFTER_NAME: u8 = 2;
  const BEFORE_VALUE: u8 = 3;
  const QUOTED_VALUE: u8 = 4;
  const UNQUOTED_VALUE: u8 = 5;

  let mut state = WHITESPACE;
  let mut name_start = 0;
  let mut name_end;
  let mut value_start = 0;
  let mut quote_char = 0;
  let mut name_start_saved = 0;
  let mut name_end_saved = 0;

  while i < len {
    let char_code = bytes[i];
    let is_space = is_whitespace(char_code);

    match state {
      WHITESPACE => {
        if !is_space {
          state = NAME;
          name_start = i;
        }
      }
      NAME => {
        if char_code == EQUALS_CHAR || is_space {
          name_end = i;
          name_start_saved = name_start;
          name_end_saved = name_end;
          state = if char_code == EQUALS_CHAR {
            BEFORE_VALUE
          } else {
            AFTER_NAME
          };
        }
      }
      AFTER_NAME => {
        if char_code == EQUALS_CHAR {
          state = BEFORE_VALUE;
        } else if !is_space {
          let raw = &attr_str[name_start_saved..name_end_saved];
          // Single-pass lowercase: the result is owned either way,
          // so the uppercase pre-scan would only add a redundant pass.
          let name = raw.to_ascii_lowercase();
          insert_attribute(&mut result, name, String::new());
          state = NAME;
          name_start = i;
        }
      }
      BEFORE_VALUE => {
        if !is_space {
          if char_code == QUOTE_CHAR || char_code == APOS_CHAR {
            state = QUOTED_VALUE;
            quote_char = char_code;
            value_start = i + 1;
          } else {
            state = UNQUOTED_VALUE;
            value_start = i;
          }
        }
      }
      QUOTED_VALUE => {
        if char_code == quote_char {
          let raw = &attr_str[name_start_saved..name_end_saved];
          // Single-pass lowercase: the result is owned either way,
          // so the uppercase pre-scan would only add a redundant pass.
          let name = raw.to_ascii_lowercase();
          insert_attribute(
            &mut result,
            name,
            decode_html_attribute_entities(&attr_str[value_start..i]).into_owned(),
          );
          state = WHITESPACE;
        }
      }
      UNQUOTED_VALUE => {
        if is_space {
          let raw = &attr_str[name_start_saved..name_end_saved];
          // Single-pass lowercase: the result is owned either way,
          // so the uppercase pre-scan would only add a redundant pass.
          let name = raw.to_ascii_lowercase();
          insert_attribute(
            &mut result,
            name,
            decode_html_attribute_entities(&attr_str[value_start..i]).into_owned(),
          );
          state = WHITESPACE;
        }
      }
      _ => {}
    }
    i += 1;
  }

  if state == NAME {
    let raw = &attr_str[name_start..];
    let lc = raw.to_ascii_lowercase();
    insert_attribute(&mut result, lc, String::new());
  } else if state == UNQUOTED_VALUE {
    let raw = &attr_str[name_start_saved..name_end_saved];
    let name = raw.to_ascii_lowercase();
    insert_attribute(
      &mut result,
      name,
      decode_html_attribute_entities(&attr_str[value_start..]).into_owned(),
    );
  } else if state == AFTER_NAME || state == BEFORE_VALUE {
    let raw = &attr_str[name_start_saved..name_end_saved];
    let name = raw.to_ascii_lowercase();
    insert_attribute(&mut result, name, String::new());
  }

  result
}

fn insert_attribute(result: &mut Attributes, name: String, value: String) {
  if result.contains_key(&name) {
    return;
  }
  if result.len() >= MAX_ATTRIBUTES_PER_ELEMENT {
    result.mark_limit_exceeded();
    return;
  }
  result.insert_if_absent(name, value);
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn whitespace_detection() {
    for c in *b" \t\n\r" {
      assert!(is_whitespace(c));
    }
    for c in [b'a', b'0', b'-', 0u8] {
      assert!(!is_whitespace(c));
    }
  }

  #[test]
  fn parses_quoted_and_unquoted_attributes() {
    let a = parse_attributes("href=\"/x\" id=main");
    assert_eq!(a.get("href").map(String::as_str), Some("/x"));
    assert_eq!(a.get("id").map(String::as_str), Some("main"));
  }

  #[test]
  fn parses_valueless_and_empty_attributes() {
    let a = parse_attributes("disabled checked");
    assert!(a.contains_key("disabled"));
    assert!(a.contains_key("checked"));
    let empty = parse_attributes("");
    assert!(empty.is_empty());
  }

  #[test]
  fn attribute_names_lowercased_values_decoded() {
    let a = parse_attributes("DATA-X='a &amp; b'");
    assert_eq!(a.get("data-x").map(String::as_str), Some("a & b"));
  }

  #[test]
  fn attribute_entities_follow_ambiguous_ampersand_rules() {
    let a = parse_attributes("title='&copycat &copy=1 &copy! &copy;cat'");
    assert_eq!(
      a.get("title").map(String::as_str),
      Some("&copycat &copy=1 ©! ©cat")
    );
  }

  #[test]
  fn form_feed_is_whitespace() {
    assert!(is_whitespace(0x0C));
  }

  #[test]
  fn valueless_equals_attribute_kept_as_empty() {
    // `<a href=>` — attribute ends in `name=`, must survive as empty value
    let a = parse_attributes("href=");
    assert!(a.contains_key("href"));
    assert_eq!(a.get("href").map(String::as_str), Some(""));
  }

  #[test]
  fn process_tag_attributes_finds_close() {
    // "<a href=\"x\">" — scan from after the tag name
    let html = "a href=\"x\">rest";
    let (complete, new_pos, attrs, self_closing) = process_tag_attributes(html, 1, false);
    assert!(complete);
    assert!(!self_closing);
    assert_eq!(&html[new_pos..], "rest");
    assert_eq!(attrs.get("href").map(String::as_str), Some("x"));
  }

  #[test]
  fn solidus_in_unquoted_value_is_not_a_self_closing_marker() {
    let (complete, _, attrs, self_closing) = process_tag_attributes(" data=x/>", 0, false);
    assert!(complete);
    assert!(!self_closing);
    assert_eq!(attrs.get("data").map(String::as_str), Some("x/"));

    let (complete, _, attrs, self_closing) = process_tag_attributes(" data=x=/>", 0, false);
    assert!(complete);
    assert!(!self_closing);
    assert_eq!(attrs.get("data").map(String::as_str), Some("x=/"));

    let (complete, _, attrs, self_closing) = process_tag_attributes(" data=/>", 0, false);
    assert!(complete);
    assert!(!self_closing);
    assert_eq!(attrs.get("data").map(String::as_str), Some("/"));

    let (complete, _, attrs, self_closing) = process_tag_attributes(" data=x />", 0, false);
    assert!(complete);
    assert!(self_closing);
    assert_eq!(attrs.get("data").map(String::as_str), Some("x"));

    let (complete, _, _, self_closing) = process_tag_attributes(" href=/u =x/>", 0, false);
    assert!(complete);
    assert!(self_closing);
  }

  #[test]
  fn duplicate_attributes_keep_the_first_value() {
    let attrs = parse_attributes(
      "href=/first HREF=/second src=one SRC=two class=a CLASS=b id=one ID=two lang=js LANG=python __proto__=first __PROTO__=second Ä=upper ä=lower",
    );
    assert_eq!(attrs.get("href").map(String::as_str), Some("/first"));
    assert_eq!(attrs.get("src").map(String::as_str), Some("one"));
    assert_eq!(attrs.get("class").map(String::as_str), Some("a"));
    assert_eq!(attrs.get("id").map(String::as_str), Some("one"));
    assert_eq!(attrs.get("lang").map(String::as_str), Some("js"));
    assert_eq!(attrs.get("__proto__").map(String::as_str), Some("first"));
    assert_eq!(attrs.get("Ä").map(String::as_str), Some("upper"));
    assert_eq!(attrs.get("ä").map(String::as_str), Some("lower"));
  }

  #[test]
  fn attribute_limits_are_reported() {
    let oversized = parse_attributes(&"x".repeat(MAX_ATTRIBUTE_BYTES + 1));
    assert!(oversized.limit_exceeded());
    assert!(oversized.is_empty());

    let input = (0..=MAX_ATTRIBUTES_PER_ELEMENT)
      .map(|index| format!("a{index}=x"))
      .collect::<Vec<_>>()
      .join(" ");
    let many = parse_attributes(&input);
    assert!(many.limit_exceeded());
    assert_eq!(many.len(), MAX_ATTRIBUTES_PER_ELEMENT);
  }

  #[test]
  fn duplicate_storm_keeps_the_first_value_without_hitting_the_count_limit() {
    let input = std::iter::repeat_n("href=duplicate", MAX_ATTRIBUTES_PER_ELEMENT * 8)
      .collect::<Vec<_>>()
      .join(" ");
    let attrs = parse_attributes(&format!("href=first {input}"));
    assert!(!attrs.limit_exceeded());
    assert_eq!(attrs.len(), 1);
    assert_eq!(attrs.get("href").map(String::as_str), Some("first"));
  }

  #[test]
  fn malformed_comments_follow_html_end_states() {
    for html in ["<!-->", "<!--->", "<!--x-->", "<!--x--!>", "<!--x--->"] {
      let result = process_comment_or_doctype(html, 0);
      assert!(result.complete, "{html}");
      assert_eq!(result.new_position, html.len(), "{html}");
    }
    assert!(!process_comment_or_doctype("<!--x", 0).complete);
  }
}
