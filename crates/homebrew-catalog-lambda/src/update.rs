//! Bringing the catalog up to date: reuse the previous catalog's categories and classify what's new, within a time
//! budget.

use {
    anyhow::{Result, bail},
    futures_util::{StreamExt, stream},
    homebrew_catalog_classifier::{
        Classifier, HomebrewFormulaMeta,
        catalog::{self, PreviousCategories, annotate},
    },
    lambda_runtime::tracing::{info, warn},
    serde::Serialize,
    serde_json::Value,
    std::collections::HashSet,
    tokio::time::Instant,
};

/// Recorded as the classification error for formulae left unclassified when time ran out. They have no categories,
/// so the next run classifies them.
pub const PENDING_MESSAGE: &str = "not classified yet: the update ran out of time; the next update will classify it";

/// What an update did.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct UpdateStats {
    /// Formulae in the new catalog.
    pub formulae: usize,
    /// Formulae whose categories were carried over from the previous catalog.
    pub reused: usize,
    /// Formulae classified by the model in this run.
    pub classified: usize,
    /// Formulae the model failed to classify (retried next run).
    pub failed: usize,
    /// Formulae not attempted because time ran out (classified next run).
    pub pending: usize,
}

/// Guards against publishing a catalog built from a broken download: the new formula list must not be empty, nor less
/// than half the size of the previous catalog.
pub fn check_formula_count(fresh: usize, previous: usize) -> Result<()> {
    if fresh == 0 {
        bail!("the downloaded formula list is empty");
    }
    if fresh * 2 < previous {
        bail!("the downloaded formula list has {fresh} formulae, under half the previous catalog's {previous}");
    }
    Ok(())
}

/// Builds the new catalog from `formulae`: categories are reused from `previous` where possible, and the rest are
/// classified until `deadline`. Returns the annotated catalog entries, in input order.
pub async fn update_catalog(
    classifier: &Classifier,
    formulae: &[HomebrewFormulaMeta],
    previous: Option<&PreviousCategories>,
    deadline: Instant,
) -> Result<(Vec<Value>, UpdateStats)> {
    let refs: Vec<&HomebrewFormulaMeta> = formulae.iter().collect();
    let (reused, to_classify) =
        catalog::plan(&refs, previous, &HashSet::new(), classifier.taxonomy(), classifier.max_categories());
    let mut stats = UpdateStats {
        formulae: formulae.len(),
        reused: formulae.len() - to_classify.len(),
        ..UpdateStats::default()
    };
    info!(reused = stats.reused, to_classify = to_classify.len(), "planned the update");

    let mut outcomes: Vec<Option<Result<Vec<String>, String>>> = reused.into_iter().map(|r| r.map(Ok)).collect();
    if !to_classify.is_empty() {
        classifier.connect().await?;
        // Results are taken in completion order (each tagged with its formula's index), so a slow or retrying request
        // doesn't hold finished ones back: when the deadline hits, everything already classified is kept.
        let mut results = stream::iter(&to_classify)
            .map(|&i| {
                let formula = refs[i];
                async move { (i, classifier.classify(formula).await) }
            })
            .buffer_unordered(classifier.concurrency());
        loop {
            match tokio::time::timeout_at(deadline, results.next()).await {
                Ok(Some((i, result))) => {
                    let name = &refs[i].name;
                    outcomes[i] = Some(match result {
                        Ok(classification) => {
                            stats.classified += 1;
                            info!(formula = %name, categories = ?classification.categories, "classified");
                            Ok(classification.categories)
                        }
                        Err(e) => {
                            stats.failed += 1;
                            warn!(formula = %name, error = %e, "classification failed");
                            Err(e.to_string())
                        }
                    });
                }
                Ok(None) => break,
                Err(_) => {
                    warn!("stopping classification to leave time for publishing");
                    break;
                }
            }
        }
    }

    stats.pending = outcomes.iter().filter(|o| o.is_none()).count();
    let annotated = formulae
        .iter()
        .zip(outcomes)
        .map(|(formula, outcome)| annotate(formula, outcome.unwrap_or_else(|| Err(PENDING_MESSAGE.to_string()))))
        .collect();
    Ok((annotated, stats))
}

#[cfg(test)]
mod tests {
    use {super::*, homebrew_catalog_classifier::catalog::CATEGORIES_FIELD, serde_json::json};

    #[test]
    fn formula_count_guard() {
        assert!(check_formula_count(8700, 8735).is_ok());
        assert!(check_formula_count(10, 0).is_ok(), "no previous catalog");
        assert!(check_formula_count(0, 0).is_err());
        assert!(check_formula_count(4000, 8735).is_err());
    }

    #[tokio::test]
    async fn reuses_everything_without_calling_the_model() {
        // The classifier points at an unroutable address: any request would fail, so this proves none is made.
        let classifier = Classifier::builder().api_key("unused").base_url("http://127.0.0.1:9").build().unwrap();
        let mut jq = HomebrewFormulaMeta {
            name: "jq".into(),
            full_name: "jq".into(),
            ..Default::default()
        };
        jq.extra.insert(CATEGORIES_FIELD.into(), json!(["data-processing"]));
        let previous = PreviousCategories::from_formulae(std::slice::from_ref(&jq));

        let deadline = Instant::now() + std::time::Duration::from_secs(5);
        let (annotated, stats) = update_catalog(&classifier, &[jq], Some(&previous), deadline).await.unwrap();
        assert_eq!(
            stats,
            UpdateStats {
                formulae: 1,
                reused: 1,
                ..UpdateStats::default()
            }
        );
        assert_eq!(annotated[0][CATEGORIES_FIELD], json!(["data-processing"]));
    }

    /// Serves Messages API requests on a local port: requests mentioning `slow_formula` stall, the rest answer at once.
    async fn mock_api(slow_formula: &'static str) -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move {
            loop {
                let (mut socket, _) = listener.accept().await.unwrap();
                tokio::spawn(async move {
                    let mut data = Vec::new();
                    let mut buf = [0u8; 8192];
                    loop {
                        let n = socket.read(&mut buf).await.unwrap_or(0);
                        if n == 0 {
                            return;
                        }
                        data.extend_from_slice(&buf[..n]);
                        let Some(end) = data.windows(4).position(|w| w == b"\r\n\r\n") else {
                            continue;
                        };
                        let head = String::from_utf8_lossy(&data[..end]).to_lowercase();
                        let length: usize = head
                            .lines()
                            .find_map(|l| l.strip_prefix("content-length:"))
                            .map_or(0, |v| v.trim().parse().unwrap());
                        if data.len() >= end + 4 + length {
                            break;
                        }
                    }
                    if String::from_utf8_lossy(&data).contains(slow_formula) {
                        tokio::time::sleep(std::time::Duration::from_secs(30)).await;
                    }
                    let body = json!({
                        "model": "claude-haiku-5-5",
                        "stop_reason": "end_turn",
                        "content": [{"type": "text", "text": "{\"categories\": [\"games\"]}"}],
                    })
                    .to_string();
                    let response = format!(
                        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = socket.write_all(response.as_bytes()).await;
                });
            }
        });
        url
    }

    #[tokio::test]
    async fn keeps_finished_results_when_an_earlier_request_is_slow() {
        let url = mock_api("stalls-forever").await;
        let classifier = Classifier::builder().api_key("test").base_url(&url).max_retries(0).build().unwrap();
        let formula = |name: &str| HomebrewFormulaMeta {
            name: name.into(),
            full_name: name.into(),
            ..Default::default()
        };
        // The slow formula comes first: results in input order would hold the quick one back until the deadline.
        let formulae = [formula("stalls-forever"), formula("quick")];

        let deadline = Instant::now() + std::time::Duration::from_secs(2);
        let (annotated, stats) = update_catalog(&classifier, &formulae, None, deadline).await.unwrap();
        assert_eq!(
            stats,
            UpdateStats {
                formulae: 2,
                classified: 1,
                pending: 1,
                ..UpdateStats::default()
            }
        );
        assert_eq!(annotated[0][catalog::ERROR_FIELD], PENDING_MESSAGE, "input order is preserved");
        assert_eq!(annotated[1][CATEGORIES_FIELD], json!(["games"]), "the finished classification is kept");
    }

    #[tokio::test]
    async fn marks_formulae_pending_when_time_runs_out() {
        let classifier = Classifier::builder().api_key("unused").base_url("http://127.0.0.1:9").build().unwrap();
        let new = HomebrewFormulaMeta {
            name: "brand-new".into(),
            full_name: "brand-new".into(),
            ..Default::default()
        };

        // A deadline already in the past: nothing is attempted.
        let (annotated, stats) = update_catalog(&classifier, &[new], None, Instant::now()).await.unwrap();
        assert_eq!(
            stats,
            UpdateStats {
                formulae: 1,
                pending: 1,
                ..UpdateStats::default()
            }
        );
        assert_eq!(annotated[0][CATEGORIES_FIELD], Value::Null);
        assert_eq!(annotated[0][catalog::ERROR_FIELD], PENDING_MESSAGE);
    }
}
