// SPDX-License-Identifier: BUSL-1.1
use crate::dispatch::DocTemplate;

pub const MAX_CHUNKS_PER_REQUEST: usize = 8;
pub const TARGET_CHUNK_CHARS: usize = 4_000;

#[derive(Debug, Clone)]
pub struct DocChunk {
    pub index: usize,
    pub content: String,
}

/// Split a document into ≤MAX_CHUNKS_PER_REQUEST chunks of roughly
/// TARGET_CHUNK_CHARS. Returns the chunks plus `truncated = true` when the
/// input exceeded the cap and the tail was dropped — callers must surface
/// that to the user instead of silently summarizing a prefix.
pub fn chunk_document(input: &str) -> (Vec<DocChunk>, bool) {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return (Vec::new(), false);
    }
    if trimmed.len() <= TARGET_CHUNK_CHARS {
        return (
            vec![DocChunk {
                index: 0,
                content: trimmed.to_string(),
            }],
            false,
        );
    }

    let mut chunks: Vec<DocChunk> = Vec::new();
    let mut current = String::with_capacity(TARGET_CHUNK_CHARS + 256);
    let mut idx = 0usize;
    let mut truncated = false;
    for para in trimmed.split("\n\n") {
        let para_trimmed = para.trim();
        if para_trimmed.is_empty() {
            continue;
        }
        if current.len() + para_trimmed.len() + 2 > TARGET_CHUNK_CHARS && !current.is_empty() {
            chunks.push(DocChunk {
                index: idx,
                content: std::mem::take(&mut current),
            });
            idx += 1;
            if chunks.len() >= MAX_CHUNKS_PER_REQUEST {
                truncated = true;
                break;
            }
        }
        if !current.is_empty() {
            current.push_str("\n\n");
        }
        current.push_str(para_trimmed);
    }
    if !current.is_empty() {
        if chunks.len() < MAX_CHUNKS_PER_REQUEST {
            chunks.push(DocChunk {
                index: idx,
                content: current,
            });
        } else {
            truncated = true;
        }
    }
    (chunks, truncated)
}

pub fn lens_prompt_for(template: DocTemplate, chunk: &str, query: Option<&str>) -> String {
    match template {
        DocTemplate::Summarize => format!(
            "Summarize the following passage in 2-3 sentences. Capture the most important facts \
             and decisions; skip filler and rhetorical flourishes.\n\n=== PASSAGE ===\n{chunk}"
        ),
        DocTemplate::Extract => format!(
            "Extract named entities from the following passage. Output one entity per line in the \
             format `TYPE: VALUE`. Types are: PERSON, ORG, LOCATION, DATE, MONEY, PRODUCT, OTHER. \
             Skip entities not clearly grounded in the passage.\n\n=== PASSAGE ===\n{chunk}"
        ),
        DocTemplate::QueryCorpus => {
            let q = query.unwrap_or("(no query specified)");
            format!(
                "You are answering a question based ONLY on the passage below. If the passage \
                 does not contain the answer, respond exactly with: NOT_FOUND. Do not invent \
                 information.\n\n=== QUESTION ===\n{q}\n\n=== PASSAGE ===\n{chunk}"
            )
        }
        DocTemplate::FindAll => {
            let q = query.unwrap_or("(no pattern specified)");
            format!(
                "Find all instances in the passage below that match the pattern: \"{q}\". \
                 Output one match per line, with surrounding context (10-15 words on each side). \
                 If no matches, respond exactly with: NO_MATCHES.\n\n=== PASSAGE ===\n{chunk}"
            )
        }
    }
}

pub fn assemble_doc_output(
    template: DocTemplate,
    chunks_total: usize,
    results: &[(usize, String)],
) -> String {
    let mut sorted: Vec<(usize, String)> = results.to_vec();
    sorted.sort_by_key(|(idx, _)| *idx);
    let template_name = template_name_human(template);
    let mut out = String::new();
    out.push_str(&format!(
        "# {} — long-context aggregation\n\n",
        template_name
    ));
    out.push_str(&format!(
        "Document was split into {chunks_total} chunks of up to {TARGET_CHUNK_CHARS} chars each \
         and each chunk processed in parallel by an independent mesh peer. Results are \
         re-assembled in original order below.\n\n"
    ));
    match template {
        DocTemplate::Extract => {
            out.push_str("## Aggregated entities\n\n");
            let mut all_lines: Vec<String> = Vec::new();
            for (_, body) in &sorted {
                for line in body.lines() {
                    let t = line.trim();
                    if !t.is_empty() && !all_lines.contains(&t.to_string()) {
                        all_lines.push(t.to_string());
                    }
                }
            }
            for line in &all_lines {
                out.push_str(&format!("- {line}\n"));
            }
        }
        DocTemplate::QueryCorpus => {
            let mut found_any = false;
            for (idx, body) in &sorted {
                let t = body.trim();
                if t == "NOT_FOUND" || t.is_empty() {
                    continue;
                }
                found_any = true;
                out.push_str(&format!("## From chunk {idx}\n\n{t}\n\n"));
            }
            if !found_any {
                out.push_str("No chunk contained an answer to the query.\n\n");
            }
        }
        DocTemplate::FindAll => {
            let mut found_any = false;
            for (idx, body) in &sorted {
                let t = body.trim();
                if t == "NO_MATCHES" || t.is_empty() {
                    continue;
                }
                found_any = true;
                out.push_str(&format!("## Chunk {idx} matches\n\n{t}\n\n"));
            }
            if !found_any {
                out.push_str("No matches found across any chunk.\n\n");
            }
        }
        DocTemplate::Summarize => {
            for (idx, body) in &sorted {
                out.push_str(&format!("## Chunk {idx}\n\n{}\n\n", body.trim()));
            }
        }
    }
    out.push_str("---\n");
    out.push_str(&format!(
        "_Generated by Nepheral's doc orchestrator. {chunks_total} chunks processed in parallel \
         across independent mesh peers._\n"
    ));
    out
}

fn template_name_human(t: DocTemplate) -> &'static str {
    match t {
        DocTemplate::Summarize => "summarize",
        DocTemplate::Extract => "extract",
        DocTemplate::QueryCorpus => "query-corpus",
        DocTemplate::FindAll => "find-all",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_input_returns_no_chunks() {
        assert!(chunk_document("").0.is_empty());
        assert!(chunk_document("   \n\n  ").0.is_empty());
    }

    #[test]
    fn small_input_single_chunk() {
        let (chunks, truncated) = chunk_document("Hello world.");
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].index, 0);
        assert_eq!(chunks[0].content, "Hello world.");
        assert!(!truncated);
    }

    #[test]
    fn large_input_splits_at_paragraph_boundaries() {
        let para = "a".repeat(2_500);
        let input = format!("{para}\n\n{para}\n\n{para}");
        let (chunks, truncated) = chunk_document(&input);
        assert!(
            chunks.len() >= 2,
            "expected ≥2 chunks for >TARGET_CHUNK_CHARS input, got {}",
            chunks.len()
        );
        for c in &chunks {
            assert!(c.content.len() <= TARGET_CHUNK_CHARS + 256);
        }
        assert!(!truncated);
    }

    #[test]
    fn chunks_capped_at_max_and_reports_truncation() {
        let para = "z".repeat(3_500);
        let many = (0..30)
            .map(|_| para.as_str())
            .collect::<Vec<_>>()
            .join("\n\n");
        let (chunks, truncated) = chunk_document(&many);
        assert!(chunks.len() <= MAX_CHUNKS_PER_REQUEST);
        assert!(truncated, "dropping the tail must be reported");
    }

    #[test]
    fn lens_prompt_query_corpus_embeds_question() {
        let prompt = lens_prompt_for(
            DocTemplate::QueryCorpus,
            "Some passage about Nepheral.",
            Some("What is Nepheral?"),
        );
        assert!(prompt.contains("What is Nepheral?"));
        assert!(prompt.contains("Some passage about Nepheral."));
        assert!(prompt.contains("NOT_FOUND"));
    }

    #[test]
    fn assemble_extract_dedupes_lines() {
        let results = vec![
            (0, "PERSON: Alice\nORG: Acme".to_string()),
            (1, "PERSON: Alice\nLOCATION: Paris".to_string()),
        ];
        let out = assemble_doc_output(DocTemplate::Extract, 2, &results);
        assert_eq!(
            out.matches("PERSON: Alice").count(),
            1,
            "duplicate PERSON entry not deduplicated:\n{out}"
        );
        assert!(out.contains("ORG: Acme"));
        assert!(out.contains("LOCATION: Paris"));
    }

    #[test]
    fn assemble_summarize_preserves_chunk_order() {
        let results = vec![
            (2, "third".to_string()),
            (0, "first".to_string()),
            (1, "second".to_string()),
        ];
        let out = assemble_doc_output(DocTemplate::Summarize, 3, &results);
        let first_pos = out.find("first").unwrap();
        let second_pos = out.find("second").unwrap();
        let third_pos = out.find("third").unwrap();
        assert!(first_pos < second_pos);
        assert!(second_pos < third_pos);
    }
}
