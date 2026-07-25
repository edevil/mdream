//! Plugin output: frontmatter YAML generation and assembly.

use super::*;

fn yaml_double_quoted(value: &str) -> String {
  const HEX: &[u8; 16] = b"0123456789ABCDEF";

  let mut output = String::with_capacity(value.len().saturating_add(2));
  output.push('"');
  for character in value.chars() {
    match character {
      '"' => output.push_str("\\\""),
      '\\' => output.push_str("\\\\"),
      '\0'..='\u{1f}' => {
        let byte = character as u8;
        output.push_str("\\u00");
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
      }
      '\u{2028}' => output.push_str("\\u2028"),
      '\u{2029}' => output.push_str("\\u2029"),
      _ => output.push(character),
    }
  }
  output.push('"');
  output
}

fn selected_additional_fields<'a>(
  additional: &'a [(String, String)],
  metadata: &[(String, String)],
  mut source_bytes: usize,
  mut field_count: usize,
) -> Vec<(&'a String, &'a String)> {
  let mut candidates: Vec<(usize, &String, &String)> = Vec::new();
  for (index, (key, value)) in additional.iter().enumerate() {
    if matches!(key.as_str(), "title" | "description" | "meta")
      || key.len() > MAX_FRONTMATTER_VALUE_BYTES
      || value.len() > MAX_FRONTMATTER_VALUE_BYTES
    {
      continue;
    }
    if let Some(existing) = candidates
      .iter_mut()
      .find(|(_, existing_key, _)| *existing_key == key)
    {
      *existing = (index, key, value);
      continue;
    }
    let position = candidates.partition_point(|(_, existing_key, _)| *existing_key < key);
    if position < MAX_FRONTMATTER_FIELDS {
      candidates.insert(position, (index, key, value));
      if candidates.len() > MAX_FRONTMATTER_FIELDS {
        candidates.pop();
      }
    }
  }

  let mut selected = Vec::new();
  for (_, key, value) in candidates {
    let field_bytes = key.len().saturating_add(value.len());
    if metadata.iter().any(|(metadata_key, _)| metadata_key == key)
      || field_count >= MAX_FRONTMATTER_FIELDS
      || source_bytes.saturating_add(field_bytes) > MAX_FRONTMATTER_BYTES
    {
      continue;
    }
    field_count += 1;
    source_bytes += field_bytes;
    selected.push((key, value));
  }
  selected
}

impl ConvertState {
  pub(crate) fn generate_frontmatter_yaml(&mut self) {
    if self.plain_text {
      return;
    }

    let f_opts = self
      .options
      .plugins
      .as_ref()
      .and_then(|p| p.frontmatter.as_ref());

    let mut yaml_out = Vec::new();
    let mut source_bytes = 0usize;
    let mut field_count = 0usize;
    if let Some(t) = &self.frontmatter_title {
      source_bytes = "title".len().saturating_add(t.len());
      field_count = 1;
      yaml_out.push(format!("title: {}", yaml_double_quoted(t)));
    }

    let mut meta_entries: Vec<_> = self.frontmatter_meta.iter().collect();
    meta_entries.sort_by(|(a, _), (b, _)| a.cmp(b));
    let mut accepted_meta = Vec::new();
    for (key, val) in meta_entries {
      let field_bytes = key.len().saturating_add(val.len());
      if field_count >= MAX_FRONTMATTER_FIELDS
        || source_bytes.saturating_add(field_bytes) > MAX_FRONTMATTER_BYTES
      {
        continue;
      }
      field_count += 1;
      source_bytes += field_bytes;
      accepted_meta.push((key, val));
    }

    if let Some(add) = f_opts.and_then(|f| f.additional_fields.as_ref()) {
      for (key, val) in
        selected_additional_fields(add, &self.frontmatter_meta, source_bytes, field_count)
      {
        yaml_out.push(format!(
          "{}: {}",
          yaml_double_quoted(key),
          yaml_double_quoted(val)
        ));
      }
    }

    if !accepted_meta.is_empty() {
      yaml_out.push("meta:".to_string());
      for (key, val) in accepted_meta {
        yaml_out.push(format!(
          "  {}: {}",
          yaml_double_quoted(key),
          yaml_double_quoted(val)
        ));
      }
    }

    if !yaml_out.is_empty() {
      let frontmatter_content = format!("---\n{}\n---\n\n", yaml_out.join("\n"));
      self.emit_frontmatter(&frontmatter_content);
    }
  }

  /// Assemble frontmatter entries (title, meta, plugin additional fields).
  /// Returns `Some` with collected entries when the frontmatter plugin is active.
  pub fn frontmatter(&self) -> Option<Vec<(String, String)>> {
    if !self.has_frontmatter {
      return None;
    }
    let mut entries: Vec<(String, String)> = Vec::new();
    if let Some(title) = &self.frontmatter_title {
      entries.push(("title".to_string(), title.clone()));
    }
    let mut source_bytes = entries
      .iter()
      .map(|(key, value)| key.len() + value.len())
      .sum::<usize>();
    for (k, v) in &self.frontmatter_meta {
      let field_bytes = k.len().saturating_add(v.len());
      if entries.len() >= MAX_FRONTMATTER_FIELDS
        || source_bytes.saturating_add(field_bytes) > MAX_FRONTMATTER_BYTES
      {
        continue;
      }
      source_bytes += field_bytes;
      entries.push((k.clone(), v.clone()));
    }
    if let Some(add) = self
      .options
      .plugins
      .as_ref()
      .and_then(|p| p.frontmatter.as_ref())
      .and_then(|f| f.additional_fields.as_ref())
    {
      for (k, v) in
        selected_additional_fields(add, &self.frontmatter_meta, source_bytes, entries.len())
      {
        entries.push((k.clone(), v.clone()));
      }
    }
    Some(entries)
  }
}
