use anyhow::Result;
use log::{info, warn};
use serde_json::{json, Value};
use std::time::Duration;
use tokio::sync::watch;

use super::{client::RustAnalyzerClient, connection::Flycheck};
use crate::{
    config::{
        DOCUMENT_OPEN_DELAY_MILLIS, FLYCHECK_REQUEST_ATTEMPTS, FLYCHECK_START_TIMEOUT_SECS,
        FLYCHECK_TIMEOUT_SECS, WORKSPACE_LOAD_TIMEOUT_SECS,
    },
    uri,
};

/// What rust-analyzer marks the diagnostics it worked out itself with, as opposed to the ones it
/// read out of a cargo check.
const ANALYSIS_SOURCE: &str = "rust-analyzer";

/// Diagnostics, and whether anything was still going on that could add to them.
pub struct FreshDiagnostics {
    pub items: Value,
    /// False when the workspace was still loading, or the cargo check did not finish or never
    /// started -- any of which makes these the best available rather than the last word.
    pub complete: bool,
}

/// Waits for rust-analyzer's checks to reach `condition`, giving up after `timeout`.
async fn wait_for(
    progress: &mut watch::Receiver<Flycheck>,
    timeout: Duration,
    condition: impl Fn(&Flycheck) -> bool,
) -> bool {
    let wait = async {
        loop {
            if condition(&progress.borrow_and_update()) {
                return true;
            }
            // Only fails once the client is gone, and then nothing is coming.
            if progress.changed().await.is_err() {
                return false;
            }
        }
    };

    tokio::time::timeout(timeout, wait).await.unwrap_or(false)
}

impl RustAnalyzerClient {
    pub async fn hover(&mut self, uri: &str, line: u32, character: u32) -> Result<Value> {
        let params = json!({
            "textDocument": { "uri": uri },
            "position": { "line": line, "character": character }
        });

        self.send_request("textDocument/hover", Some(params)).await
    }

    pub async fn definition(&mut self, uri: &str, line: u32, character: u32) -> Result<Value> {
        let params = json!({
            "textDocument": { "uri": uri },
            "position": { "line": line, "character": character }
        });

        self.send_request("textDocument/definition", Some(params))
            .await
    }

    pub async fn references(&mut self, uri: &str, line: u32, character: u32) -> Result<Value> {
        let params = json!({
            "textDocument": { "uri": uri },
            "position": { "line": line, "character": character },
            "context": { "includeDeclaration": true }
        });

        self.send_request("textDocument/references", Some(params))
            .await
    }

    pub async fn completion(&mut self, uri: &str, line: u32, character: u32) -> Result<Value> {
        let params = json!({
            "textDocument": { "uri": uri },
            "position": { "line": line, "character": character }
        });

        self.send_request("textDocument/completion", Some(params))
            .await
    }

    /// Asks what renaming the symbol at a position would take, without doing any of it.
    pub async fn rename(
        &mut self,
        uri: &str,
        line: u32,
        character: u32,
        new_name: &str,
    ) -> Result<Value> {
        let params = json!({
            "textDocument": { "uri": uri },
            "position": { "line": line, "character": character },
            "newName": new_name
        });

        self.send_request("textDocument/rename", Some(params)).await
    }

    /// The range of the symbol a rename at this position would be about.
    ///
    /// Asked before a rename for two reasons: it says what is about to be renamed, which is worth
    /// reporting back, and when there is nothing renameable there it says so more precisely than
    /// the rename itself does.
    pub async fn prepare_rename(&mut self, uri: &str, line: u32, character: u32) -> Result<Value> {
        let params = json!({
            "textDocument": { "uri": uri },
            "position": { "line": line, "character": character }
        });

        self.send_request("textDocument/prepareRename", Some(params))
            .await
    }

    pub async fn document_symbols(&mut self, uri: &str) -> Result<Value> {
        let params = json!({
            "textDocument": { "uri": uri }
        });

        self.send_request("textDocument/documentSymbol", Some(params))
            .await
    }

    pub async fn formatting(&mut self, uri: &str) -> Result<Value> {
        let params = json!({
            "textDocument": { "uri": uri },
            "options": {
                "tabSize": 4,
                "insertSpaces": true
            }
        });

        self.send_request("textDocument/formatting", Some(params))
            .await
    }

    pub async fn diagnostics(&mut self, uri: &str) -> Result<Value> {
        // First check if we have stored diagnostics from publishDiagnostics.
        let key = uri::normalize(uri);
        let diag_lock = self.diagnostics.lock().await;
        info!("Looking for diagnostics for URI: {}", key);
        info!(
            "Available URIs with diagnostics: {:?}",
            diag_lock.keys().collect::<Vec<_>>()
        );
        if let Some(diagnostics) = diag_lock.get(&key) {
            info!("Found {} stored diagnostics for {}", diagnostics.len(), uri);
            return Ok(json!(diagnostics));
        }
        drop(diag_lock);

        info!("No stored diagnostics for {}, trying pull model", uri);
        // If no stored diagnostics, try the pull model as fallback.
        let params = json!({
            "textDocument": { "uri": uri }
        });

        let response = self
            .send_request("textDocument/diagnostic", Some(params))
            .await?;

        // Extract diagnostics from the response.
        if let Some(items) = response.get("items") {
            Ok(items.clone())
        } else {
            Ok(json!([]))
        }
    }

    /// Diagnostics for `uri` from a cargo check that has seen the file as it is now.
    ///
    /// The diagnostics rustc gives -- the ones anyone actually wants -- only ever arrive as the
    /// result of a check, and a check takes as long as it takes. Asking rust-analyzer for one and
    /// waiting for it to finish is the only way to answer with the code in front of us rather
    /// than whatever was last reported about it.
    pub async fn fresh_diagnostics(&mut self, uri: &str) -> Result<FreshDiagnostics> {
        // A report on a workspace rust-analyzer is still loading covers the part of it that has
        // been reached, and a file it has not reached looks exactly like a file with nothing
        // wrong with it.
        let loaded = self
            .wait_until_loaded(Duration::from_secs(WORKSPACE_LOAD_TIMEOUT_SECS))
            .await;
        if !loaded {
            warn!("rust-analyzer is still loading the workspace; reporting on what it has");
        }

        let before = self.flycheck.borrow().clone();
        let mut progress = self.flycheck.subscribe();

        // A check covers the whole workspace and republishes everything it has to say about it,
        // so everything said before it is superseded.
        self.diagnostics.lock().await.clear();

        let params = json!({ "textDocument": { "uri": uri } });
        self.send_notification("rust-analyzer/runFlycheck", Some(params.clone()))
            .await?;

        // Waiting was given up on before, and nothing has reported a check since: a
        // rust-analyzer too old to report them, or told not to check at all. One that turns up
        // after all -- a workspace big enough for the first check to start late -- makes this
        // false again, and the waiting resumes.
        if self.gave_up_on_checks && before.never_ran_one() {
            return Ok(FreshDiagnostics {
                items: self.current_diagnostics(uri).await?,
                complete: false,
            });
        }

        // Waiting for the check to start is a step of its own, because it may never do: the
        // request can be dropped along with the analysis it was working from, and rust-analyzer
        // may be old enough not to know it or be configured not to check at all. Ask again a
        // couple of times for the first case; the second gives up quickly rather than sitting
        // out the whole timeout every call.
        let mut started = false;
        for attempt in 1..=FLYCHECK_REQUEST_ATTEMPTS {
            started = wait_for(
                &mut progress,
                Duration::from_secs(FLYCHECK_START_TIMEOUT_SECS),
                |flycheck| flycheck.started_since(&before),
            )
            .await;
            if started || attempt == FLYCHECK_REQUEST_ATTEMPTS {
                break;
            }

            info!("Asking for a cargo check of {} again", uri);
            self.send_notification("rust-analyzer/runFlycheck", Some(params.clone()))
                .await?;
        }
        if !started {
            info!("No cargo check started for {}, reporting what we have", uri);
            // Nothing has ever reported a check here, so take it that nothing will and stop
            // making every later call wait the same wait out. Only until one does: the check
            // this call asked for may yet begin, and the next call will see that it did.
            if self.flycheck.borrow().never_ran_one() {
                warn!("rust-analyzer has yet to report a cargo check; not waiting for one");
                self.gave_up_on_checks = true;
            }

            return Ok(FreshDiagnostics {
                items: self.current_diagnostics(uri).await?,
                complete: false,
            });
        }

        let finished = wait_for(
            &mut progress,
            Duration::from_secs(FLYCHECK_TIMEOUT_SECS),
            |flycheck| flycheck.caught_up_with(&before),
        )
        .await;
        if !finished {
            warn!("cargo check for {} is still running, reporting early", uri);
        }

        // The results are published just after the check reports itself done, from a turn of
        // rust-analyzer's loop we cannot see the end of.
        tokio::time::sleep(Duration::from_millis(DOCUMENT_OPEN_DELAY_MILLIS)).await;

        Ok(FreshDiagnostics {
            items: self.current_diagnostics(uri).await?,
            complete: loaded && finished,
        })
    }

    /// Waits until rust-analyzer has no loading left to do, giving up after `timeout`.
    ///
    /// Worth doing before anything whose answer is only right once rust-analyzer has seen the
    /// whole workspace.
    pub async fn wait_until_loaded(&self, timeout: Duration) -> bool {
        let mut quiescent = self.quiescent.subscribe();
        let wait = async {
            loop {
                if *quiescent.borrow_and_update() {
                    return true;
                }
                if quiescent.changed().await.is_err() {
                    return false;
                }
            }
        };

        tokio::time::timeout(timeout, wait).await.unwrap_or(false)
    }

    /// Everything known about `uri` as it stands: what rust-analyzer works out itself, asked for
    /// afresh, and what the last cargo check said.
    ///
    /// The two halves have to be come by differently. rust-analyzer publishes its own analysis
    /// when it gets round to it, and one worked out before a change can arrive after it -- but
    /// the same analysis can simply be asked for, and the answer then describes the content
    /// rust-analyzer holds rather than the content it held. Cargo's half cannot be asked for at
    /// all: it exists only as the published results of a check, which is what the wait above is
    /// for.
    async fn current_diagnostics(&mut self, uri: &str) -> Result<Value> {
        let published = self.published_diagnostics(uri).await;

        let params = json!({ "textDocument": { "uri": uri } });
        let pulled = match self
            .send_request("textDocument/diagnostic", Some(params))
            .await
        {
            Ok(pulled) => pulled,
            // Asking is an improvement on waiting to be told, not a requirement: an older
            // rust-analyzer, or one that has gone away, leaves the published reports as all
            // there is.
            Err(e) => {
                warn!("Could not ask rust-analyzer about {}: {}", uri, e);
                return Ok(json!(published));
            }
        };
        let Some(analysed) = pulled.get("items").and_then(|items| items.as_array()) else {
            return Ok(json!(published));
        };

        let from_cargo = published
            .iter()
            .filter(|diagnostic| diagnostic["source"] != ANALYSIS_SOURCE)
            .cloned();

        Ok(json!(analysed
            .iter()
            .cloned()
            .chain(from_cargo)
            .collect::<Vec<_>>()))
    }

    /// What rust-analyzer last published about `uri`, and nothing else.
    ///
    /// Unlike [`Self::diagnostics`] this does not fall back to asking rust-analyzer directly:
    /// having waited for a check, no entry means the check had nothing to say about the file,
    /// and the answer to that is an empty list rather than a request whose reply cannot contain
    /// a cargo diagnostic anyway.
    async fn published_diagnostics(&self, uri: &str) -> Vec<Value> {
        self.diagnostics
            .lock()
            .await
            .get(&uri::normalize(uri))
            .cloned()
            .unwrap_or_default()
    }

    pub async fn workspace_diagnostics(&mut self) -> Result<Value> {
        // Try workspace/diagnostic if available, otherwise collect from all open documents.
        let params = json!({
            "identifier": "rust-analyzer",
            "previousResultId": null
        });

        match self
            .send_request("workspace/diagnostic", Some(params))
            .await
        {
            Ok(response) => Ok(response),
            // A dead rust-analyzer must not pass for a clean workspace.
            Err(e) if self.is_gone() => Err(e),
            Err(_) => {
                // Fallback: return diagnostics for all open documents.
                let mut all_diagnostics = json!({});
                let open_docs: Vec<String> =
                    self.open_documents.lock().await.keys().cloned().collect();

                for doc_uri in open_docs.iter() {
                    if let Ok(diag) = self.diagnostics(doc_uri).await {
                        all_diagnostics[doc_uri] = diag;
                    }
                }

                Ok(all_diagnostics)
            }
        }
    }

    pub async fn code_actions(
        &mut self,
        uri: &str,
        start_line: u32,
        start_char: u32,
        end_line: u32,
        end_char: u32,
    ) -> Result<Value> {
        // First, try to get diagnostics for this range.
        let diagnostics = self.diagnostics(uri).await.unwrap_or(json!([]));

        // Filter diagnostics to only those in the requested range.
        let filtered_diagnostics = filter_diagnostics_in_range(&diagnostics, start_line, end_line);

        let params = json!({
            "textDocument": { "uri": uri },
            "range": {
                "start": { "line": start_line, "character": start_char },
                "end": { "line": end_line, "character": end_char }
            },
            "context": {
                "diagnostics": filtered_diagnostics,
                "only": ["quickfix", "refactor", "refactor.extract", "refactor.inline", "refactor.rewrite", "source"]
            }
        });

        self.send_request("textDocument/codeAction", Some(params))
            .await
    }
}

fn filter_diagnostics_in_range(diagnostics: &Value, start_line: u32, end_line: u32) -> Value {
    let Some(diag_array) = diagnostics.as_array() else {
        return json!([]);
    };

    let filtered: Vec<Value> = diag_array
        .iter()
        .filter(|d| {
            let Some(range) = d.get("range") else {
                return false;
            };
            let Some(start) = range.get("start") else {
                return false;
            };
            let Some(end) = range.get("end") else {
                return false;
            };

            let diag_start_line = start.get("line").and_then(|l| l.as_u64()).unwrap_or(0) as u32;
            let diag_end_line = end.get("line").and_then(|l| l.as_u64()).unwrap_or(0) as u32;

            // Check if diagnostic overlaps with requested range.
            diag_start_line <= end_line && diag_end_line >= start_line
        })
        .cloned()
        .collect();

    json!(filtered)
}
