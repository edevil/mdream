use mdream::types::HTMLToMarkdownOptions;
use mdream::{MarkdownStreamProcessor, html_to_markdown};

fn convert(html: &str) -> String {
  html_to_markdown(html, HTMLToMarkdownOptions::default())
}

fn stream(html: &str, split: usize) -> String {
  let mut processor = MarkdownStreamProcessor::new(HTMLToMarkdownOptions::default());
  let mut output = processor.process_chunk(&html[..split]);
  output.push_str(&processor.process_chunk(&html[split..]));
  output.push_str(&processor.finish());
  output
}

#[test]
fn invalid_tag_openers_reconsume_visible_text() {
  let cases = [
    ("<p>I <3 Rust</p>", "I <3 Rust"),
    ("<p>I < 3 Rust</p>", "I < 3 Rust"),
    ("<p>I <> Rust</p>", "I <> Rust"),
    ("<p>I <<em>love</em> Rust</p>", "I <*love* Rust"),
    ("<3", "<3"),
    ("< 3", "< 3"),
    ("<>", "<>"),
    ("<", "<"),
    ("</", "\\</"),
    ("before<a", "before"),
    ("<p>before</>after", "beforeafter"),
    ("<p>before</>", "before"),
    ("<?pi?>after", "after"),
    ("</3>after", "after"),
    ("</>after", "after"),
    ("<!foo>after", "after"),
  ];
  for (html, expected) in cases {
    assert_eq!(convert(html), expected, "{html}");
    for split in 0..=html.len() {
      assert_eq!(stream(html, split), expected, "{html} split at {split}");
    }
  }
}

#[test]
fn malformed_comments_close_at_html_comment_end_states() {
  let cases = [
    ("before<!-->after", "beforeafter"),
    ("before<!--->after", "beforeafter"),
    ("before<!--x--!>after", "beforeafter"),
    ("before<!--x--->after", "beforeafter"),
    ("before<!--x", "before"),
    ("before<!foo", "before"),
    ("before<?pi", "before"),
  ];
  for (html, expected) in cases {
    assert_eq!(convert(html), expected, "{html}");
    for split in 0..=html.len() {
      assert_eq!(stream(html, split), expected, "{html} split at {split}");
    }
  }
}

#[test]
fn duplicate_security_attributes_keep_the_first_value() {
  assert_eq!(
    convert("<a href=/first HREF=/second>link</a>"),
    "[link](/first)"
  );
  assert_eq!(
    convert("<img src=/first SRC=/second alt=first ALT=second>"),
    "![first](/first)"
  );
}
