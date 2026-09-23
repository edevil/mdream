#![no_main]
use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use mdream::{MarkdownStreamProcessor, html_to_markdown, types::HTMLToMarkdownOptions};

#[derive(Arbitrary, Debug)]
struct StreamInput {
  chunks: Vec<String>,
}

fuzz_target!(|input: StreamInput| {
  let mut processor = MarkdownStreamProcessor::new(HTMLToMarkdownOptions::default());
  let mut streamed = String::new();
  for chunk in &input.chunks {
    streamed.push_str(&processor.process_chunk(chunk));
  }
  streamed.push_str(&processor.finish());

  let html = input.chunks.concat();
  let one_shot = html_to_markdown(&html, HTMLToMarkdownOptions::default());
  assert_eq!(
    streamed, one_shot,
    "streaming diverged from one-shot: chunks={:?}",
    input.chunks
  );
});
