// SPDX-License-Identifier: BUSL-1.1
use crate::dispatch::DevTemplate;

pub const MAX_FILES_PER_REQUEST: usize = 12;
pub const FILE_MARKER_PREFIX: &str = "=== file:";

#[derive(Debug, Clone)]
pub struct DevFile {
    pub index: usize,
    pub path: String,
    pub content: String,
}

/// Parse a multi-file dev mode input:
///
/// ```text
/// === file: path/to/foo.rs ===
/// <contents of foo.rs>
///
/// === file: path/to/bar.rs ===
/// <contents of bar.rs>
/// ```
///
/// No markers -> single anonymous file, index=0, path="<single>".
pub fn split_dev_input(input: &str) -> Vec<DevFile> {
    let trimmed = input.trim_start();
    if trimmed.is_empty() {
        return Vec::new();
    }

    let mut files: Vec<DevFile> = Vec::new();
    let lines: Vec<&str> = input.lines().collect();
    let mut current_path: Option<String> = None;
    let mut current_body = String::new();
    let mut idx = 0usize;

    let flush = |files: &mut Vec<DevFile>,
                 idx: &mut usize,
                 path: &mut Option<String>,
                 body: &mut String| {
        if let Some(p) = path.take() {
            files.push(DevFile {
                index: *idx,
                path: p,
                content: std::mem::take(body).trim().to_string(),
            });
            *idx += 1;
        }
    };

    for line in lines {
        let trimmed_line = line.trim();
        if let Some(rest) = trimmed_line.strip_prefix(FILE_MARKER_PREFIX) {
            let path_part = rest.trim().trim_end_matches('=').trim().to_string();
            if !path_part.is_empty() {
                flush(&mut files, &mut idx, &mut current_path, &mut current_body);
                if files.len() >= MAX_FILES_PER_REQUEST {
                    break;
                }
                current_path = Some(path_part);
                continue;
            }
        }
        if current_path.is_some() {
            current_body.push_str(line);
            current_body.push('\n');
        }
    }
    flush(&mut files, &mut idx, &mut current_path, &mut current_body);

    if files.is_empty() {
        return vec![DevFile {
            index: 0,
            path: "<single>".to_string(),
            content: input.to_string(),
        }];
    }
    files
}

pub fn lens_prompt_for(
    template: DevTemplate,
    file_path: &str,
    file_content: &str,
    user_instruction: &str,
) -> String {
    let header = format!(
        "You are processing a single file (`{file_path}`). Other files in this request are being \
         processed independently by other workers — do not reference them. Stay scoped to this \
         file's content. Output the result wrapped in `=== file: {file_path} ===` markers so the \
         reducer can re-assemble.\n\n"
    );
    let body = match template {
        DevTemplate::Refactor => format!(
            "{header}Refactor this file according to the instruction below. Preserve public API \
             semantics unless the instruction explicitly says otherwise. Do not add comments \
             explaining the change — the diff explains itself.\n\n\
             === USER INSTRUCTION ===\n{user_instruction}\n\n\
             === ORIGINAL FILE ===\n{file_content}"
        ),
        DevTemplate::AuditCode => format!(
            "{header}Audit this file for bugs, suspicious patterns, and code smells. For each \
             finding, give: severity (Critical/High/Medium/Low), location (function or line \
             marker), and a one-sentence explanation. Skip stylistic preferences. If you find \
             nothing, respond exactly: NO_FINDINGS.\n\n\
             === USER FOCUS ===\n{user_instruction}\n\n\
             === FILE ===\n{file_content}"
        ),
        DevTemplate::GenTests => format!(
            "{header}Generate unit tests for the public functions in this file. Cover happy \
             path + at least one edge case per function. Output runnable test code in the same \
             language as the source. Tests must compile against the file as-is — do not invent \
             missing helpers.\n\n\
             === USER GUIDANCE ===\n{user_instruction}\n\n\
             === FILE ===\n{file_content}"
        ),
        DevTemplate::MigrateApi => format!(
            "{header}Migrate this file from the OLD api to the NEW api described below. \
             Replace every call site. Preserve behavior. If a call has no equivalent in the new \
             api, mark it `// MIGRATE: no direct equivalent — manual review` and leave the old \
             call in place. Do not invent new api methods.\n\n\
             === MIGRATION SPEC ===\n{user_instruction}\n\n\
             === FILE ===\n{file_content}"
        ),
    };
    body
}

pub fn assemble_dev_output(template: DevTemplate, results: &[(usize, String, String)]) -> String {
    let mut sorted: Vec<(usize, String, String)> = results.to_vec();
    sorted.sort_by_key(|(idx, _, _)| *idx);
    let template_name = template_name_human(template);
    let mut out = String::new();
    out.push_str(&format!(
        "# dev:{template_name} — multi-file orchestrator output\n\n"
    ));
    out.push_str(&format!(
        "Processed {} file(s) in parallel across independent mesh workers. Each worker had no \
         knowledge of the other files. Output is re-assembled in the original file order below.\n\n",
        sorted.len()
    ));

    match template {
        DevTemplate::AuditCode => {
            let mut clean: Vec<String> = Vec::new();
            let mut with_findings: Vec<(String, String)> = Vec::new();
            for (_, path, body) in &sorted {
                let t = body.trim();
                if t == "NO_FINDINGS" || t.is_empty() {
                    clean.push(path.clone());
                } else {
                    with_findings.push((path.clone(), t.to_string()));
                }
            }
            if !with_findings.is_empty() {
                out.push_str("## Files with findings\n\n");
                for (path, body) in &with_findings {
                    out.push_str(&format!("### `{path}`\n\n{body}\n\n"));
                }
            }
            if !clean.is_empty() {
                out.push_str("## Files with no findings\n\n");
                for path in &clean {
                    out.push_str(&format!("- `{path}`\n"));
                }
                out.push('\n');
            }
        }
        _ => {
            for (idx, path, body) in &sorted {
                out.push_str(&format!("## File {idx}: `{path}`\n\n{}\n\n", body.trim()));
            }
        }
    }
    out.push_str("---\n");
    out.push_str(
        "_Generated by Nepheral's dev orchestrator. Each file went through one independent \
         mesh worker; workers did not coordinate._\n",
    );
    out
}

fn template_name_human(t: DevTemplate) -> &'static str {
    match t {
        DevTemplate::Refactor => "refactor",
        DevTemplate::AuditCode => "audit-code",
        DevTemplate::GenTests => "gen-tests",
        DevTemplate::MigrateApi => "migrate-api",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_input_returns_no_files() {
        assert!(split_dev_input("").is_empty());
        assert!(split_dev_input("   \n   ").is_empty());
    }

    #[test]
    fn single_file_no_marker_treated_as_anonymous() {
        let files = split_dev_input("fn foo() {}");
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, "<single>");
        assert!(files[0].content.contains("fn foo()"));
    }

    #[test]
    fn multiple_files_split_at_markers() {
        let input = "=== file: src/a.rs ===\nfn a() {}\n\n=== file: src/b.rs ===\nfn b() {}\n";
        let files = split_dev_input(input);
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].path, "src/a.rs");
        assert!(files[0].content.contains("fn a()"));
        assert_eq!(files[1].path, "src/b.rs");
        assert!(files[1].content.contains("fn b()"));
    }

    #[test]
    fn file_marker_with_trailing_equals_parsed() {
        let input = "=== file: src/x.rs ===\nfn x() {}\n";
        let files = split_dev_input(input);
        assert_eq!(files[0].path, "src/x.rs");
    }

    #[test]
    fn files_capped_at_max() {
        let mut input = String::new();
        for i in 0..30 {
            input.push_str(&format!("=== file: f{i}.rs ===\nfn x() {{}}\n"));
        }
        let files = split_dev_input(&input);
        assert!(files.len() <= MAX_FILES_PER_REQUEST);
    }

    #[test]
    fn lens_prompt_includes_file_marker_and_path() {
        let prompt = lens_prompt_for(
            DevTemplate::Refactor,
            "src/foo.rs",
            "fn foo() {}",
            "use Result instead of Option",
        );
        assert!(prompt.contains("src/foo.rs"));
        assert!(prompt.contains("use Result instead of Option"));
        assert!(prompt.contains("fn foo()"));
    }

    #[test]
    fn audit_assembler_separates_findings_from_clean() {
        let results = vec![
            (
                0,
                "src/a.rs".to_string(),
                "Critical: SQL injection at line 12.".to_string(),
            ),
            (1, "src/b.rs".to_string(), "NO_FINDINGS".to_string()),
            (
                2,
                "src/c.rs".to_string(),
                "Medium: missing bounds check.".to_string(),
            ),
        ];
        let out = assemble_dev_output(DevTemplate::AuditCode, &results);
        assert!(out.contains("Files with findings"));
        assert!(out.contains("src/a.rs"));
        assert!(out.contains("src/c.rs"));
        assert!(out.contains("Files with no findings"));
        assert!(out.contains("src/b.rs"));
    }

    #[test]
    fn refactor_assembler_preserves_order() {
        let results = vec![
            (
                1,
                "b.rs".to_string(),
                "fn b() { /* refactored */ }".to_string(),
            ),
            (
                0,
                "a.rs".to_string(),
                "fn a() { /* refactored */ }".to_string(),
            ),
        ];
        let out = assemble_dev_output(DevTemplate::Refactor, &results);
        let a_pos = out.find("a.rs").unwrap();
        let b_pos = out.find("b.rs").unwrap();
        assert!(a_pos < b_pos);
    }
}
