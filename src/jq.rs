use std::io::Write;
use std::process::{Command, Stdio};

use anyhow::{Context, Result, anyhow};
use serde::Serialize;

/// Pipe a serializable value through `jq` and return trimmed stdout.
///
/// Mirrors upstream `processWithJq`: feeds compact (non-pretty) JSON via stdin,
/// passes the filter expression as a single argv element, returns `output.trim()`.
pub(crate) fn run<T: Serialize>(value: &T, expr: &str) -> Result<String> {
    let json = serde_json::to_string(value)?;
    let mut child = Command::new("jq")
        .arg(expr)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                anyhow!("jq command not found. Please install jq to use the --jq option.")
            } else {
                anyhow!("jq processing failed: {e}")
            }
        })?;

    {
        let stdin = child
            .stdin
            .as_mut()
            .ok_or_else(|| anyhow!("failed to open jq stdin"))?;
        stdin.write_all(json.as_bytes()).context("write jq stdin")?;
    }

    let output = child.wait_with_output().context("wait for jq")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow!("jq processing failed: {}", stderr.trim()));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}
