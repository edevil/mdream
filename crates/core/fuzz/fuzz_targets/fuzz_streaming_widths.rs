#![no_main]
use libfuzzer_sys::fuzz_target;
use mdream::{MarkdownStreamProcessor, html_to_markdown, types::HTMLToMarkdownOptions};

// Test each document at several widths so a boundary-sensitive bug does not
// depend on the mutator also finding its exact chunk width.
const WIDTHS: [usize; 8] = [1, 2, 3, 5, 7, 13, 64, 4096];

fuzz_target!(|data: &[u8]| {
  let html = String::from_utf8_lossy(data);
  let expected = html_to_markdown(&html, HTMLToMarkdownOptions::default());

  for width in WIDTHS {
    let mut processor = MarkdownStreamProcessor::new(HTMLToMarkdownOptions::default());
    let mut streamed = String::new();
    let mut start = 0;
    while start < html.len() {
      let mut end = (start + width).min(html.len());
      while end < html.len() && !html.is_char_boundary(end) {
        end += 1;
      }
      streamed.push_str(&processor.process_chunk(&html[start..end]));
      start = end;
    }
    streamed.push_str(&processor.finish());

    assert_eq!(
      streamed, expected,
      "width {width} diverged from one-shot: html={html:?}"
    );
  }
});
