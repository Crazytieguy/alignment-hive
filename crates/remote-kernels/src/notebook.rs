use std::path::{Path, PathBuf};

use anyhow::Context as _;
use serde_json::{Value, json};

use crate::jupyter::messages::ExecutionOutput;

/// Manages a notebook file (.ipynb) for a single kernel.
pub struct Notebook {
    path: PathBuf,
    metadata: Value,
    cells: Vec<Value>,
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
        let kernel_fragment = sanitize_filename(&kernel_id[..8.min(kernel_id.len())]);
        let unique = &uuid::Uuid::new_v4().simple().to_string()[..8];
        let path = dir.join(format!(
            "{timestamp}_{label}_{kernel_fragment}_{unique}.ipynb"
        ));

        let notebook = Self {
            path,
            metadata: notebook_metadata(kernel_id, None),
            cells: Vec::new(),
            execution_count: 0,
        };
        notebook.save()?;

        tracing::info!(path = %notebook.path.display(), "Created notebook");
        Ok(notebook)
    }

    /// Create an empty transcript for a live kernel whose prior notebook
    /// binding could not be verified. The marker is intentionally visible in
    /// ordinary notebook metadata.
    pub fn new_continuation(
        notebook_dir: &Path,
        kernel_id: &str,
        name: Option<&str>,
        reason: &str,
    ) -> anyhow::Result<Self> {
        let mut notebook = Self::new(notebook_dir, kernel_id, name)?;
        notebook.metadata["remote_kernels"]["continuation"] = json!(reason);
        notebook.save()?;
        Ok(notebook)
    }

    /// Load and validate an existing notebook while preserving its metadata.
    /// Incomplete streaming placeholders are valid and get an explicit marker.
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("read notebook {}", path.display()))?;
        let value: Value = serde_json::from_str(&text)
            .with_context(|| format!("parse notebook {}", path.display()))?;
        anyhow::ensure!(
            value["nbformat"].as_u64() == Some(4),
            "notebook nbformat must be 4"
        );
        let metadata = value
            .get("metadata")
            .filter(|value| value.is_object())
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("notebook metadata must be an object"))?;
        let raw_cells = value
            .get("cells")
            .and_then(Value::as_array)
            .ok_or_else(|| anyhow::anyhow!("notebook cells must be an array"))?;
        let mut cells = Vec::with_capacity(raw_cells.len());
        let mut execution_count = 0_u32;
        for (index, raw_cell) in raw_cells.iter().enumerate() {
            validate_cell(raw_cell).with_context(|| format!("invalid cell {}", index + 1))?;
            let cell = raw_cell.clone();
            if cell["cell_type"] == "code"
                && let Some(count) = cell["execution_count"].as_u64()
            {
                let count = u32::try_from(count)
                    .map_err(|_| anyhow::anyhow!("execution_count exceeds u32"))?;
                execution_count = execution_count.max(count);
            }
            cells.push(cell);
        }
        Ok(Self {
            path: path.to_path_buf(),
            metadata,
            cells,
            execution_count,
        })
    }

    /// Get the path to the notebook file.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Kernel id embedded when this notebook was created.
    pub fn kernel_id(&self) -> Option<&str> {
        self.metadata["remote_kernels"]["kernel_id"].as_str()
    }

    /// Accept this notebook as the transcript for `expected_kernel_id`, then
    /// mark incomplete placeholders. Loading alone is deliberately read-only.
    pub fn bind_for_recovery(&mut self, expected_kernel_id: &str) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.kernel_id() == Some(expected_kernel_id),
            "notebook kernel id mismatch"
        );
        let mut changed = false;
        for cell in &mut self.cells {
            if cell["cell_type"] == "code"
                && cell["metadata"]["remote_kernels"]["placeholder"] == true
                && cell["metadata"]["remote_kernels"]["recovery_status"] != "output incomplete"
            {
                cell["metadata"]["remote_kernels"]["recovery_status"] = json!("output incomplete");
                changed = true;
            }
        }
        if changed {
            self.save()?;
        }
        Ok(())
    }

    /// Append a code cell with its output (used when execution completes synchronously).
    pub fn append_cell(&mut self, code: &str, output: &ExecutionOutput) -> anyhow::Result<()> {
        self.execution_count += 1;

        let outputs = build_outputs(output, Some(self.execution_count));

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

    /// Create a cell placeholder with empty output. Returns the cell number
    /// (1-indexed position in the notebook, which is what `update_cell_output`
    /// and `backfill_output` speak — not the execution count, which diverges as
    /// soon as the notebook holds non-code cells or gaps).
    /// Used for streaming: the cell is created up front, then updated as output arrives.
    pub fn append_cell_placeholder(
        &mut self,
        code: &str,
        parent_msg_id: &str,
    ) -> anyhow::Result<u32> {
        self.execution_count += 1;

        let cell = json!({
            "cell_type": "code",
            "execution_count": self.execution_count,
            "metadata": {
                "remote_kernels": {
                    "parent_msg_id": parent_msg_id,
                    "placeholder": true
                }
            },
            "source": split_source(code),
            "outputs": []
        });

        self.cells.push(cell);
        self.save()?;
        u32::try_from(self.cells.len()).context("notebook has too many cells")
    }

    /// Update the output of an existing cell (identified by `cell_number`, 1-indexed).
    /// Called as streaming output arrives and on completion.
    pub fn update_cell_output(
        &mut self,
        cell_number: u32,
        output: &ExecutionOutput,
    ) -> anyhow::Result<()> {
        let index = (cell_number as usize).checked_sub(1);
        if let Some(cell) = index.and_then(|index| self.cells.get_mut(index)) {
            let outputs = build_outputs(output, cell_execution_count(cell));
            cell["outputs"] = json!(outputs);
            cell["metadata"]["remote_kernels"]["placeholder"] = json!(false);
            if let Some(metadata) = cell["metadata"]["remote_kernels"].as_object_mut() {
                metadata.remove("recovery_status");
            }
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

    /// Back-fill one still-empty placeholder by its execute-request message id.
    /// A finalized/live-written cell is never changed, which is the catch-up
    /// dedupe boundary.
    pub fn backfill_output(
        &mut self,
        parent_msg_id: &str,
        output: &ExecutionOutput,
        complete: bool,
    ) -> anyhow::Result<Option<u32>> {
        let Some((index, cell)) = self.cells.iter_mut().enumerate().find(|(_, cell)| {
            cell["metadata"]["remote_kernels"]["parent_msg_id"].as_str() == Some(parent_msg_id)
        }) else {
            return Ok(None);
        };
        let is_placeholder = cell["metadata"]["remote_kernels"]["placeholder"] == true;
        let has_outputs = cell["outputs"]
            .as_array()
            .is_some_and(|outputs| !outputs.is_empty());
        if !is_placeholder || has_outputs {
            return Ok(None);
        }
        let cell_number = u32::try_from(index + 1).context("notebook has too many cells")?;
        cell["outputs"] = json!(build_outputs(output, cell_execution_count(cell)));
        cell["metadata"]["remote_kernels"]["recovery_status"] = if complete {
            json!("recovered")
        } else {
            json!("output incomplete")
        };
        if complete {
            cell["metadata"]["remote_kernels"]["placeholder"] = json!(false);
        }
        self.save()?;
        Ok(Some(cell_number))
    }

    fn save(&self) -> anyhow::Result<()> {
        let notebook = json!({
            "nbformat": 4,
            "nbformat_minor": 5,
            "metadata": self.metadata,
            "cells": self.cells
        });

        let json = serde_json::to_string_pretty(&notebook)?;
        let tmp = self
            .path
            .with_extension(format!("ipynb.{}.tmp", uuid::Uuid::new_v4().simple()));
        if let Err(error) =
            std::fs::write(&tmp, json).and_then(|()| std::fs::rename(&tmp, &self.path))
        {
            let _ = std::fs::remove_file(&tmp);
            return Err(error.into());
        }
        Ok(())
    }
}

fn notebook_metadata(kernel_id: &str, continuation: Option<&str>) -> Value {
    let mut metadata = json!({
        "kernelspec": {
            "display_name": "Python 3",
            "language": "python",
            "name": "python3"
        },
        "language_info": {
            "name": "python",
            "version": "3.10"
        },
        "remote_kernels": {
            "kernel_id": kernel_id
        }
    });
    if let Some(reason) = continuation {
        metadata["remote_kernels"]["continuation"] = json!(reason);
    }
    metadata
}

fn validate_cell(cell: &Value) -> anyhow::Result<()> {
    anyhow::ensure!(cell.is_object(), "cell must be an object");
    let cell_type = cell["cell_type"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("cell_type must be a string"))?;
    anyhow::ensure!(
        cell["metadata"].is_object(),
        "cell metadata must be an object"
    );
    let source_valid = cell["source"].is_string()
        || cell["source"]
            .as_array()
            .is_some_and(|lines| lines.iter().all(Value::is_string));
    anyhow::ensure!(source_valid, "cell source must be a string or string array");
    if cell_type == "code" {
        anyhow::ensure!(
            cell["outputs"].is_array(),
            "code cell outputs must be an array"
        );
        anyhow::ensure!(
            cell["execution_count"].is_null() || cell["execution_count"].is_u64(),
            "code cell execution_count must be null or unsigned"
        );
    }
    Ok(())
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

/// The execution count recorded on a cell, for stamping its `execute_result`.
fn cell_execution_count(cell: &Value) -> Option<u32> {
    cell["execution_count"]
        .as_u64()
        .and_then(|c| u32::try_from(c).ok())
}

/// Build notebook output cells from execution output.
fn build_outputs(output: &ExecutionOutput, execution_count: Option<u32>) -> Vec<serde_json::Value> {
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

    for data in &output.display_data {
        outputs.push(json!({
            "output_type": "display_data",
            "data": {
                "text/plain": split_source(data)
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
        assert_eq!(
            json["metadata"]["remote_kernels"]["kernel_id"],
            "abcdef1234567890"
        );
        // Unnamed kernels get an 8-char kernel-id prefix in the filename.
        let filename = nb.path().file_name().unwrap().to_string_lossy();
        assert!(filename.contains("_abcdef12_"), "was {filename}");
        assert!(filename.ends_with(".ipynb"), "was {filename}");
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

        let cell_number = nb.append_cell_placeholder("1 + 1", "msg-1").unwrap();
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
        let second = nb.append_cell_placeholder("a + 1", "msg-2").unwrap();
        assert_eq!(second, 2);
    }

    #[test]
    fn load_valid_notebook_permits_appending() {
        let dir = tempfile::tempdir().unwrap();
        let mut original = Notebook::new(dir.path(), "kernel-1", Some("load")).unwrap();
        original
            .append_cell("a = 1", &output_with("", None))
            .unwrap();

        let mut loaded = Notebook::load(original.path()).unwrap();
        assert_eq!(loaded.kernel_id(), Some("kernel-1"));
        assert_eq!(loaded.append_cell_placeholder("a + 1", "msg-2").unwrap(), 2);
    }

    #[test]
    fn placeholder_cell_number_is_a_position_not_an_execution_count() {
        let dir = tempfile::tempdir().unwrap();
        let mut original = Notebook::new(dir.path(), "kernel-1", Some("mixed")).unwrap();
        original
            .append_cell("a = 1", &output_with("", None))
            .unwrap();

        // A markdown cell added by hand while the server was stopped: it shifts
        // positions without advancing the execution count.
        let path = original.path().to_path_buf();
        let mut json: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        json["cells"].as_array_mut().unwrap().push(json!({
            "cell_type": "markdown",
            "metadata": {},
            "source": ["notes"]
        }));
        std::fs::write(&path, serde_json::to_string_pretty(&json).unwrap()).unwrap();

        let mut loaded = Notebook::load(&path).unwrap();
        let cell_number = loaded.append_cell_placeholder("a + 1", "msg-2").unwrap();
        assert_eq!(cell_number, 3);
        loaded
            .update_cell_output(cell_number, &output_with("", Some("2")))
            .unwrap();

        let json = read_notebook(&loaded);
        assert_eq!(json["cells"][1]["cell_type"], "markdown");
        assert!(json["cells"][1].get("outputs").is_none());
        let outputs = &json["cells"][2]["outputs"];
        assert_eq!(outputs[0]["output_type"], "execute_result");
        assert_eq!(outputs[0]["data"]["text/plain"][0], "2");
        // The result is stamped with the cell's execution count, not its position.
        assert_eq!(outputs[0]["execution_count"], 2);
        assert_eq!(
            json["cells"][2]["metadata"]["remote_kernels"]["placeholder"],
            false
        );
    }

    #[test]
    fn load_is_read_only_until_bound() {
        let dir = tempfile::tempdir().unwrap();
        let mut original = Notebook::new(dir.path(), "kernel-1", Some("placeholder")).unwrap();
        original
            .append_cell_placeholder("slow()", "msg-slow")
            .unwrap();
        let before = std::fs::read(original.path()).unwrap();

        let mut loaded = Notebook::load(original.path()).unwrap();
        assert_eq!(std::fs::read(original.path()).unwrap(), before);
        loaded.bind_for_recovery("kernel-1").unwrap();
        let json = read_notebook(&loaded);
        assert_eq!(
            json["cells"][0]["metadata"]["remote_kernels"]["recovery_status"],
            "output incomplete"
        );
    }

    #[test]
    fn load_rejects_corrupt_notebook() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("corrupt.ipynb");
        std::fs::write(
            &path,
            r#"{"nbformat":4,"metadata":{},"cells":[{"cell_type":"code","metadata":{},"source":[],"outputs":"bad","execution_count":1}]}"#,
        )
        .unwrap();
        assert!(Notebook::load(&path).is_err());
    }

    #[test]
    fn catch_up_only_fills_empty_placeholder_once() {
        let dir = tempfile::tempdir().unwrap();
        let mut notebook = Notebook::new(dir.path(), "kernel-1", Some("dedupe")).unwrap();
        notebook
            .append_cell_placeholder("print('first')", "msg-1")
            .unwrap();
        let first = output_with("first\n", None);
        assert_eq!(
            notebook.backfill_output("msg-1", &first, true).unwrap(),
            Some(1)
        );
        let duplicate = output_with("duplicate\n", None);
        assert_eq!(
            notebook.backfill_output("msg-1", &duplicate, true).unwrap(),
            None
        );
        let json = read_notebook(&notebook);
        assert_eq!(json["cells"][0]["outputs"][0]["text"][0], "first\n");
    }

    #[test]
    fn continuation_notebooks_created_same_second_do_not_collide() {
        let dir = tempfile::tempdir().unwrap();
        let first =
            Notebook::new_continuation(dir.path(), "kernel-1", Some("recovery"), "missing binding")
                .unwrap();
        let second =
            Notebook::new_continuation(dir.path(), "kernel-1", Some("recovery"), "missing binding")
                .unwrap();
        assert_ne!(first.path(), second.path());
        assert!(first.path().exists() && second.path().exists());
    }

    #[test]
    fn atomic_save_replaces_without_temp_residue() {
        let dir = tempfile::tempdir().unwrap();
        let mut notebook = Notebook::new(dir.path(), "kernel-1", Some("atomic")).unwrap();
        notebook.append_cell_placeholder("1 + 1", "msg-1").unwrap();
        notebook
            .update_cell_output(1, &output_with("", Some("2")))
            .unwrap();
        assert!(Notebook::load(notebook.path()).is_ok());
        let files = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect::<Vec<_>>();
        assert_eq!(files, vec![notebook.path().to_path_buf()]);
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
