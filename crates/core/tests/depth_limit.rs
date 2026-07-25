use mdream::{
  HTMLToMarkdownOptions, MarkdownStreamProcessor, PluginConfig, TagOverrideConfig, html_to_markdown,
};

const LIMIT: usize = 512;

fn nested(tag: &str, depth: usize, content: &str) -> String {
  format!(
    "{}{content}{}",
    format!("<{tag}>").repeat(depth),
    format!("</{tag}>").repeat(depth),
  )
}

fn stream_convert(chunks: &[&str]) -> String {
  let mut stream = MarkdownStreamProcessor::new(HTMLToMarkdownOptions::default());
  let mut output = String::new();
  for chunk in chunks {
    output.push_str(&stream.process_chunk(chunk));
  }
  output.push_str(&stream.finish());
  output
}

#[test]
fn depth_boundaries_preserve_subtree_and_later_siblings() {
  for depth in [255, 256, 511, 512, 513] {
    let html = format!("{}<p>after</p>", nested("div", depth, "inside"));
    assert_eq!(
      html_to_markdown(&html, HTMLToMarkdownOptions::default()),
      "inside\n\nafter",
      "depth {depth}",
    );
  }
}

#[test]
fn repeated_tag_depth_does_not_wrap_at_256() {
  for depth in [255, 256, 511, 512] {
    let output = html_to_markdown(
      &nested("blockquote", depth, "sentinel"),
      HTMLToMarkdownOptions::default(),
    );
    assert!(output.ends_with("sentinel"), "depth {depth}: {output:?}");
    assert_eq!(output.matches("> ").count(), depth, "depth {depth}");
  }
}

#[test]
fn one_hundred_thousand_starts_keep_recovering() {
  let html = format!("<p>before</p>{}inside", "<div>".repeat(100_000));
  assert_eq!(
    html_to_markdown(&html, HTMLToMarkdownOptions::default()),
    "before\n\ninside",
  );
}

#[test]
fn self_closing_elements_at_the_limit_do_not_enter_overflow() {
  let html = format!(
    "{}<br>kept{}<p>after</p>",
    "<div>".repeat(LIMIT),
    "</div>".repeat(LIMIT),
  );
  let output = html_to_markdown(&html, HTMLToMarkdownOptions::default());
  assert!(output.contains("kept"), "{output:?}");
  assert!(output.contains("after"), "{output:?}");
}

#[test]
fn implied_end_recovery_runs_before_the_limit() {
  let output = html_to_markdown(&"<p>item".repeat(1_000), HTMLToMarkdownOptions::default());
  assert_eq!(output.matches("item").count(), 1_000);
}

#[test]
fn streamed_overflow_recovers_after_repeated_root_closes() {
  let mut chunks = vec!["<p>before</p>"];
  chunks.extend(std::iter::repeat_n("<div>", 100_000));
  chunks.push("inside");
  chunks.extend(std::iter::repeat_n("</div>", 100_000));
  chunks.push("<p>after</p>");
  assert_eq!(stream_convert(&chunks), "before\n\ninside\n\nafter");
}

#[test]
fn overflow_suppresses_inert_and_raw_text_then_resumes() {
  let html = format!(
    "{}<section>before<script>hidden-script</script><template><style>hidden-style</style><b>hidden-template</b></template>visible</section>{}<p>after</p>",
    "<div>".repeat(LIMIT),
    "</div>".repeat(LIMIT),
  );
  let output = html_to_markdown(&html, HTMLToMarkdownOptions::default());
  assert!(output.contains("before"), "{output:?}");
  assert!(output.contains("visible"), "{output:?}");
  assert!(output.contains("after"), "{output:?}");
  assert!(!output.contains("hidden-script"), "{output:?}");
  assert!(!output.contains("hidden-style"), "{output:?}");
  assert!(!output.contains("hidden-template"), "{output:?}");
}

#[test]
fn raw_text_cannot_close_an_outer_inert_overflow_frame() {
  let html = format!(
    "{}<template><script>\"</template>\"; hidden</script>still-hidden</template>visible</div><p>after</p>",
    "<div>".repeat(LIMIT),
  );
  let output = html_to_markdown(&html, HTMLToMarkdownOptions::default());
  assert!(!output.contains("hidden"), "{output:?}");
  assert!(output.contains("visible"), "{output:?}");
  assert!(output.contains("after"), "{output:?}");
}

#[test]
fn malformed_closes_do_not_unbalance_capped_output() {
  let html = format!(
    "{}<section>inside</bogus><span>still</section><p>after</p>",
    "<div>".repeat(LIMIT),
  );
  let output = html_to_markdown(&html, HTMLToMarkdownOptions::default());
  assert!(output.contains("inside"), "{output:?}");
  assert!(output.contains("still"), "{output:?}");
  assert!(output.contains("after"), "{output:?}");
}

#[test]
fn skipped_cdata_override_does_not_leave_overflow_active() {
  let html = format!("{}<![CDATA[hidden]]><p>kept</p>", "<div>".repeat(LIMIT),);
  let options = HTMLToMarkdownOptions {
    plugins: Some(PluginConfig {
      tag_overrides: Some(vec![(
        "#cdata-section".to_string(),
        TagOverrideConfig {
          enter: Some("[".to_string()),
          exit: Some("]".to_string()),
          ..Default::default()
        },
      )]),
      ..Default::default()
    }),
    ..Default::default()
  };
  let output = html_to_markdown(&html, options);
  assert!(!output.contains("hidden"), "{output:?}");
  assert!(output.contains("kept"), "{output:?}");
}

#[test]
fn deep_formatting_contexts_preserve_visible_text() {
  for (context, sentinel) in [
    ("<pre>*pre*</pre>", "pre"),
    ("<blockquote>quote</blockquote>", "quote"),
    ("<ul><li>item</li></ul>", "item"),
    ("<table><tr><td>cell</td></tr></table>", "cell"),
  ] {
    let html = format!(
      "{}<section>{context}</section>{}<p>after</p>",
      "<div>".repeat(LIMIT),
      "</div>".repeat(LIMIT),
    );
    let output = html_to_markdown(&html, HTMLToMarkdownOptions::default());
    assert!(output.contains(sentinel), "missing {sentinel}: {output:?}");
    assert!(output.contains("after"), "missing after: {output:?}");
  }
}

#[test]
fn every_split_matches_one_shot_during_overflow_recovery() {
  let html = format!(
    "{}<section>before<script>hidden-script</script><style>hidden-style</style><template>inert</template>inside</section><p>after</p>",
    "<i>".repeat(LIMIT),
  );
  let expected = html_to_markdown(&html, HTMLToMarkdownOptions::default());
  for split in 0..=html.len() {
    assert_eq!(
      stream_convert(&[&html[..split], &html[split..]]),
      expected,
      "split {split}",
    );
  }
}
