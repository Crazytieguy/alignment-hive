use std::path::{Path, PathBuf};

use serde_json::json;

use crate::jupyter::messages::ExecutionOutput;

/// Manages a notebook file (.ipynb) for a single kernel.
pub struct Notebook {
    path: PathBuf,
    cells: Vec<serde_json::Value>,
    execution_count: u32,
}

impl Notebook {
    /// Create a new notebook for a kernel. If `name` is provided, it's used in the filename;
    /// otherwise falls back to a short prefix of the kernel ID.
    /// `notebook_dir` is the directory to store notebooks in (absolute path).
    pub fn new(notebook_dir: &Path, kernel_id: &str, name: Option<&str>) -> anyhow::Result<Self> {
        let dir = notebook_dir;
        std::fs::create_dir_all(dir)?;

        let timestamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
        let label = match name {
            Some(n) => sanitize_filename(n),
            None => kernel_id[..8.min(kernel_id.len())].to_string(),
        };
        let path = dir.join(format!("{timestamp}_{label}.ipynb"));

        let notebook = Self {
            path,
            cells: Vec::new(),
            execution_count: 0,
        };
        notebook.save()?;

        tracing::info!(path = %notebook.path.display(), "Created notebook");
        Ok(notebook)
    }

    /// Get the path to the notebook file.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Append a code cell with its output (used when execution completes synchronously).
    pub fn append_cell(&mut self, code: &str, output: &ExecutionOutput) -> anyhow::Result<()> {
        self.execution_count += 1;

        let outputs = build_outputs(output, self.execution_count);

        let cell = json!({
            "cell_type": "code",
            "execution_count": self.execution_count,
            "metadata": {},
            "source": split_source(code),
            "outputs": outputs
        });

        self.cells.push(cell);
        self.save()
    }

    /// Create a cell placeholder with empty output. Returns the cell number (1-indexed).
    /// Used for streaming: the cell is created up front, then updated as output arrives.
    pub fn append_cell_placeholder(&mut self, code: &str) -> anyhow::Result<u32> {
        self.execution_count += 1;

        let cell = json!({
            "cell_type": "code",
            "execution_count": self.execution_count,
            "metadata": {},
            "source": split_source(code),
            "outputs": []
        });

        self.cells.push(cell);
        self.save()?;
        Ok(self.execution_count)
    }

    /// Update the output of an existing cell (identified by `cell_number`, 1-indexed).
    /// Called as streaming output arrives and on completion.
    pub fn update_cell_output(
        &mut self,
        cell_number: u32,
        output: &ExecutionOutput,
    ) -> anyhow::Result<()> {
        let index = (cell_number - 1) as usize;
        if let Some(cell) = self.cells.get_mut(index) {
            let outputs = build_outputs(output, cell_number);
            cell["outputs"] = json!(outputs);
            self.save()?;
        } else {
            // Can happen when a stale pending execution completes after the
            // notebook was recreated (e.g. kernel restart). The result is
            // dropped; leave a trail instead of failing silently.
            tracing::warn!(
                cell_number,
                path = %self.path.display(),
                "Dropping execution output for out-of-range notebook cell"
            );
        }
        Ok(())
    }

    fn save(&self) -> anyhow::Result<()> {
        let notebook = json!({
            "nbformat": 4,
            "nbformat_minor": 5,
            "metadata": {
                "kernelspec": {
                    "display_name": "Python 3",
                    "language": "python",
                    "name": "python3"
                },
                "language_info": {
                    "name": "python",
                    "version": "3.10"
                }
            },
            "cells": self.cells
        });

        let json = serde_json::to_string_pretty(&notebook)?;
        std::fs::write(&self.path, json)?;
        Ok(())
    }
}

/// Split source code into lines for nbformat (each line ends with \n except the last).
fn split_source(code: &str) -> Vec<String> {
    let lines: Vec<&str> = code.split('\n').collect();
    lines
        .iter()
        .enumerate()
        .map(|(i, line)| {
            if i < lines.len() - 1 {
                format!("{line}\n")
            } else {
                (*line).to_string()
            }
        })
        .collect()
}

/// Sanitize a user-provided name for use in a filename.
/// Replaces non-alphanumeric characters (except hyphens and underscores) with underscores,
/// and truncates to a reasonable length.
fn sanitize_filename(name: &str) -> String {
    let sanitized: String = name
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let trimmed = sanitized.trim_matches('_');
    // Truncate by characters, not bytes — a byte slice can panic mid-codepoint
    // for multi-byte alphanumerics (CJK, accented letters).
    trimmed.chars().take(64).collect()
}

/// Build notebook output cells from execution output.
fn build_outputs(output: &ExecutionOutput, execution_count: u32) -> Vec<serde_json::Value> {
    let mut outputs = Vec::new();

    if !output.stdout.is_empty() {
        outputs.push(json!({
            "output_type": "stream",
            "name": "stdout",
            "text": split_source(&output.stdout)
        }));
    }

    if !output.stderr.is_empty() {
        outputs.push(json!({
            "output_type": "stream",
            "name": "stderr",
            "text": split_source(&output.stderr)
        }));
    }

    if let Some(ref result) = output.result {
        outputs.push(json!({
            "output_type": "execute_result",
            "execution_count": execution_count,
            "data": {
                "text/plain": split_source(result)
            },
            "metadata": {}
        }));
    }

    if let Some(ref err) = output.error {
        outputs.push(json!({
            "output_type": "error",
            "ename": err.ename,
            "evalue": err.evalue,
            "traceback": err.traceback
        }));
    }

    outputs
}

#[cfg(test)]
mod tests {
    use super::*;

    fn output_with(stdout: &str, result: Option<&str>) -> ExecutionOutput {
        ExecutionOutput {
            stdout: stdout.to_string(),
            result: result.map(String::from),
            ..Default::default()
        }
    }

    fn read_notebook(nb: &Notebook) -> serde_json::Value {
        serde_json::from_str(&std::fs::read_to_string(nb.path()).unwrap()).unwrap()
    }

    #[test]
    fn new_notebook_is_valid_empty_nbformat4() {
        let dir = tempfile::tempdir().unwrap();
        let nb = Notebook::new(dir.path(), "abcdef1234567890", None).unwrap();

        let json = read_notebook(&nb);
        assert_eq!(json["nbformat"], 4);
        assert_eq!(json["cells"].as_array().unwrap().len(), 0);
        // Unnamed kernels get an 8-char kernel-id prefix in the filename.
        let filename = nb.path().file_name().unwrap().to_string_lossy();
        assert!(filename.ends_with("_abcdef12.ipynb"), "was {filename}");
    }

    #[test]
    fn append_cell_records_code_and_outputs() {
        let dir = tempfile::tempdir().unwrap();
        let mut nb = Notebook::new(dir.path(), "kernel-1", Some("analysis")).unwrap();
        nb.append_cell("print('hi')\nprint('bye')", &output_with("hi\nbye\n", None))
            .unwrap();

        let json = read_notebook(&nb);
        let cell = &json["cells"][0];
        assert_eq!(cell["cell_type"], "code");
        assert_eq!(cell["execution_count"], 1);
        // nbformat source: every line ends with \n except the last.
        assert_eq!(cell["source"][0], "print('hi')\n");
        assert_eq!(cell["source"][1], "print('bye')");
        assert_eq!(cell["outputs"][0]["output_type"], "stream");
        assert_eq!(cell["outputs"][0]["name"], "stdout");
    }

    #[test]
    fn placeholder_then_update_streams_output_into_cell() {
        let dir = tempfile::tempdir().unwrap();
        let mut nb = Notebook::new(dir.path(), "kernel-1", Some("stream")).unwrap();

        let cell_number = nb.append_cell_placeholder("1 + 1").unwrap();
        assert_eq!(cell_number, 1);
        let json = read_notebook(&nb);
        assert_eq!(json["cells"][0]["outputs"].as_array().unwrap().len(), 0);

        nb.update_cell_output(cell_number, &output_with("", Some("2")))
            .unwrap();
        let json = read_notebook(&nb);
        let outputs = &json["cells"][0]["outputs"];
        assert_eq!(outputs[0]["output_type"], "execute_result");
        assert_eq!(outputs[0]["data"]["text/plain"][0], "2");
    }

    #[test]
    fn update_out_of_range_cell_is_a_noop() {
        let dir = tempfile::tempdir().unwrap();
        let mut nb = Notebook::new(dir.path(), "kernel-1", Some("noop")).unwrap();
        nb.update_cell_output(5, &output_with("ignored", None))
            .unwrap();
        assert_eq!(read_notebook(&nb)["cells"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn execution_count_increments_across_cells() {
        let dir = tempfile::tempdir().unwrap();
        let mut nb = Notebook::new(dir.path(), "kernel-1", Some("counts")).unwrap();
        nb.append_cell("a = 1", &output_with("", None)).unwrap();
        let second = nb.append_cell_placeholder("a + 1").unwrap();
        assert_eq!(second, 2);
    }

    #[test]
    fn sanitize_filename_replaces_specials_and_truncates() {
        assert_eq!(sanitize_filename("my analysis!"), "my_analysis");
        assert_eq!(sanitize_filename("keep-this_name1"), "keep-this_name1");
        assert_eq!(sanitize_filename("__trimmed__"), "trimmed");
        assert_eq!(sanitize_filename(&"x".repeat(100)).len(), 64);
        // Multi-byte alphanumerics must truncate by chars, not bytes (byte
        // slicing panics when the cutoff lands mid-codepoint).
        assert_eq!(sanitize_filename(&"日".repeat(100)).chars().count(), 64);
        assert_eq!(sanitize_filename("café-run"), "café-run");
    }

    #[test]
    fn split_source_preserves_trailing_newline_semantics() {
        assert_eq!(split_source("a\nb"), vec!["a\n", "b"]);
        assert_eq!(split_source("a\n"), vec!["a\n", ""]);
        assert_eq!(split_source("a"), vec!["a"]);
    }
}
