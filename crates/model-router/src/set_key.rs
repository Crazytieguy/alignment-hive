//! `model-router set-key gemini`: the user pastes their AI Studio key into a
//! hidden prompt in their own terminal, the key is checked with Google, and
//! it lands in the `secrets.toml` next to the config the router reads. The
//! key never passes through Claude Code's transcript, and nobody edits the
//! file by hand.
//!
//! The check is one tiny request to a paid-only model straight to Google
//! (about $0.0005 when it answers), read as the router's billing probe reads
//! its own ([`crate::gemini_billing`]). Only a key Google calls invalid is
//! not saved: anything else is a working key, and what it found about the
//! project's billing is printed for the user and left for `doctor` to
//! report to the agent.

use std::io::IsTerminal;
use std::path::Path;
use std::time::Duration;

use anyhow::Context;

use crate::gemini_billing::{BILLING_URL, KeyCheck, NotPaid};

/// Google's API, which the check calls directly: the router (and its
/// child) may not be running yet.
const GOOGLE_API: &str = "https://generativelanguage.googleapis.com";

const CHECK_TIMEOUT: Duration = Duration::from_secs(30);

/// Prompts for the Gemini key, checks it, and saves it next to
/// `config_path`. Prints one line on what was found.
///
/// # Errors
/// Fails without saving when stdin is not a terminal, nothing was pasted,
/// Google says the key is invalid, or the secrets file cannot be written.
pub async fn gemini(config_path: &Path) -> anyhow::Result<()> {
    anyhow::ensure!(
        std::io::stdin().is_terminal(),
        "run this in a terminal: it asks you to paste the key"
    );
    let key = rpassword::prompt_password("Paste your Google AI Studio API key (input hidden): ")
        .context("failed to read the key")?;
    let key = key.trim();
    anyhow::ensure!(!key.is_empty(), "no key was pasted; nothing was saved");
    anyhow::ensure!(
        !key.chars().any(char::is_whitespace),
        "that doesn't look like one API key (it has spaces in it); nothing was saved"
    );
    println!("Checking the key with Google...");
    let found = check(key).await;
    if let KeyCheck::Invalid(said) = &found {
        anyhow::bail!("Google says the key is invalid ({said}); nothing was saved");
    }
    let secrets = config_path.with_file_name("secrets.toml");
    save(&secrets, key)?;
    println!("Saved to {}. {}", secrets.display(), outcome(&found));
    println!("Tell Claude you're done.");
    Ok(())
}

/// Asks the paid-only probe model for a few tokens with `key`, and where
/// Google rate-limits that one (its limits are per model) the fallback, as
/// the router's own probe does.
async fn check(key: &str) -> KeyCheck {
    let found = ask(key, crate::config::GEMINI_PROBE_MODEL).await;
    if matches!(found, KeyCheck::Checked(Err(NotPaid::RateLimited(_)))) {
        return ask(key, crate::config::GEMINI_PROBE_FALLBACK_MODEL).await;
    }
    found
}

async fn ask(key: &str, model: &str) -> KeyCheck {
    let url = format!("{GOOGLE_API}/v1beta/models/{model}:generateContent");
    let body = serde_json::json!({
        "contents": [{"role": "user", "parts": [{"text": "Reply with the word ok."}]}],
        "generationConfig": {"thinkingConfig": {"thinkingLevel": "low"}},
    });
    let request = reqwest::Client::new()
        .post(url)
        .header("x-goog-api-key", key)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .timeout(CHECK_TIMEOUT)
        .body(body.to_string());
    match request.send().await {
        Ok(response) => {
            let status = response.status();
            match response.bytes().await {
                Ok(body) => crate::gemini_billing::read_google(status, &body),
                Err(error) => KeyCheck::Checked(Err(NotPaid::Unknown {
                    why: format!("the answer was cut off: {error}"),
                    retry: true,
                })),
            }
        }
        Err(error) => KeyCheck::Checked(Err(NotPaid::Unknown {
            why: format!(
                "Google could not be reached: {:#}",
                anyhow::Error::from(error.without_url())
            ),
            retry: true,
        })),
    }
}

/// What to tell the user about a key that is saved.
fn outcome(found: &KeyCheck) -> String {
    match found {
        KeyCheck::Checked(Ok(())) => "The key works and its project has billing.".to_string(),
        KeyCheck::Checked(Err(NotPaid::Free)) => format!(
            "The key works, but its project has no billing, so most Gemini models won't \
             answer until you turn it on at {BILLING_URL}."
        ),
        KeyCheck::Checked(Err(NotPaid::Empty)) => {
            format!(
                "The key works, but its project is out of prepaid credit: add some at {BILLING_URL}."
            )
        }
        KeyCheck::Checked(Err(NotPaid::Blocked)) => format!(
            "Google refused the request (403), as it does for a past-due billing account \
             or a key restricted from the Gemini API: check the project at {BILLING_URL}."
        ),
        KeyCheck::Checked(Err(other)) => format!(
            "Its billing couldn't be checked just now ({}); `model-router doctor` checks \
             again.",
            other.reason()
        ),
        KeyCheck::Invalid(_) => unreachable!("an invalid key is not saved"),
    }
}

/// Sets `[gemini] api-key` in the secrets file at `path`, keeping the rest
/// of it, and leaves the file readable by the user alone.
fn save(path: &Path, key: &str) -> anyhow::Result<()> {
    let mut table = match std::fs::read_to_string(path) {
        Ok(contents) => contents
            .parse::<toml::Table>()
            .with_context(|| format!("{} is not valid TOML; nothing was saved", path.display()))?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => toml::Table::new(),
        Err(error) => {
            return Err(error).with_context(|| format!("failed to read {}", path.display()));
        }
    };
    let gemini = table
        .entry("gemini")
        .or_insert_with(|| toml::Value::Table(toml::Table::new()));
    let toml::Value::Table(gemini) = gemini else {
        anyhow::bail!(
            "{} has a `gemini` entry that is not a table; nothing was saved",
            path.display()
        );
    };
    gemini.insert("api-key".to_string(), toml::Value::String(key.to_string()));
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)
            .with_context(|| format!("failed to create {}", dir.display()))?;
    }
    crate::state::write_private_atomic(path, toml::to_string(&table)?.as_bytes())
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    #[test]
    fn the_key_is_saved_beside_the_config_and_the_rest_is_kept() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("nested/config.toml");
        let secrets = config.with_file_name("secrets.toml");
        save(&secrets, "AIza-first").unwrap();
        let mode = std::fs::metadata(&secrets).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);

        std::fs::write(
            &secrets,
            "[openai-providers]\nopenrouter = \"sk-or\"\n\n[gemini]\napi-key = \"AIza-first\"\n",
        )
        .unwrap();
        save(&secrets, "AIza-second").unwrap();
        let table: toml::Table = std::fs::read_to_string(&secrets).unwrap().parse().unwrap();
        assert_eq!(table["gemini"]["api-key"].as_str(), Some("AIza-second"));
        assert_eq!(
            table["openai-providers"]["openrouter"].as_str(),
            Some("sk-or")
        );
    }

    #[test]
    fn a_secrets_file_that_cannot_take_the_key_is_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let secrets = dir.path().join("secrets.toml");
        for contents in ["not toml [", "gemini = \"a string\"\n"] {
            std::fs::write(&secrets, contents).unwrap();
            let error = save(&secrets, "AIza").unwrap_err().to_string();
            assert!(error.contains("nothing was saved"), "{error}");
            assert_eq!(std::fs::read_to_string(&secrets).unwrap(), contents);
        }
    }

    #[test]
    fn every_saved_outcome_reads_as_one_plain_sentence_or_two() {
        let free = outcome(&KeyCheck::Checked(Err(NotPaid::Free)));
        assert!(
            free.contains("no billing") && free.contains(BILLING_URL),
            "{free}"
        );
        let limited = outcome(&KeyCheck::Checked(Err(NotPaid::RateLimited(
            "Please retry in 5s.".to_string(),
        ))));
        assert!(limited.contains("couldn't be checked"), "{limited}");
        assert!(!limited.contains("no billing"), "{limited}");
    }
}
