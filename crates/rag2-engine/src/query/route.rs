//! Query routing: which retrievers run. Static (all), ordered rules over query features, or a
//! decision model asked a `choice` question over operator-defined routes.

use crate::api::{QueryRequest, Route};
use crate::components::Components;
use rag2_config::schema::{Pipeline, RouteCondition, Routing};
use rag2_core::interfaces::{Answer, ChoiceOption, Question, check_answers};
use rag2_core::model::RetrieverKind;
use std::collections::HashMap;
use std::sync::{Arc, LazyLock};

/// Compiled route patterns. Keys are operator-configured patterns, so the map stays small.
static PATTERNS: LazyLock<parking_lot::RwLock<HashMap<String, Option<Arc<regex::Regex>>>>> =
    LazyLock::new(Default::default);

fn pattern(re: &str) -> Option<Arc<regex::Regex>> {
    if let Some(p) = PATTERNS.read().get(re) {
        return p.clone();
    }
    let compiled = regex::Regex::new(re).ok().map(Arc::new);
    PATTERNS.write().insert(re.to_string(), compiled.clone());
    compiled
}

fn enabled(pl: &Pipeline) -> Vec<RetrieverKind> {
    pl.retrievers
        .iter()
        .filter(|r| r.enabled)
        .map(|r| r.kind)
        .collect()
}

fn matches(cond: &RouteCondition, req: &QueryRequest) -> bool {
    let q = &req.query;
    let n_words = q.split_whitespace().count();
    match cond {
        RouteCondition::MaxWords(n) => n_words <= *n,
        RouteCondition::MinWords(n) => n_words >= *n,
        RouteCondition::HasQuotedPhrase => q.matches('"').count() >= 2,
        RouteCondition::HasIdentifier => q
            .split_whitespace()
            .any(|w| w.chars().any(|c| c.is_ascii_digit()) && w.chars().any(|c| c.is_alphabetic())),
        // Validated at activation; an invalid pattern here simply never matches.
        RouteCondition::Matches(re) => pattern(re).is_some_and(|r| r.is_match(q)),
        RouteCondition::HasFilters => !req.filters.is_empty(),
    }
}

/// Decide the route; warnings explain any fallback.
pub fn decide(
    pl: &Pipeline,
    req: &QueryRequest,
    comps: &Components,
    warnings: &mut Vec<String>,
) -> Route {
    match &pl.routing {
        Routing::All => Route {
            retrievers: enabled(pl),
            reason: "all enabled retrievers".into(),
        },
        Routing::Rules { rules, fallback } => {
            for (i, r) in rules.iter().enumerate() {
                if matches(&r.when, req) {
                    return Route {
                        retrievers: r.retrievers.clone(),
                        reason: format!("rule {i} ({:?})", r.when),
                    };
                }
            }
            Route {
                retrievers: fallback.clone(),
                reason: "no rule matched; fallback".into(),
            }
        }
        Routing::Decision {
            model,
            instructions,
            routes,
            min_confidence,
            fallback,
        } => {
            let fb = |why: String, warnings: &mut Vec<String>| {
                warnings.push(why.clone());
                Route {
                    retrievers: fallback.clone(),
                    reason: why,
                }
            };
            let Some(m) = comps.decision_models.get(model) else {
                return fb(
                    format!("decision model {model} not loaded; fallback route"),
                    warnings,
                );
            };
            let names: Vec<&String> = routes.keys().collect();
            let q = [Question::Choice {
                instructions: instructions.clone(),
                options: names
                    .iter()
                    .map(|n| ChoiceOption {
                        label: (*n).clone(),
                        description: routes[*n].description.clone(),
                    })
                    .collect(),
            }];
            let answers = m
                .decide(&req.query, &q)
                .and_then(|a| check_answers(&q, &a).map(|_| a));
            match answers.as_deref() {
                Ok([Answer::Choice { label, confidence }]) if *confidence >= *min_confidence => {
                    Route {
                        retrievers: routes[label].retrievers.clone(),
                        reason: format!(
                            "decision model {model} chose {label:?} at {confidence:.2}"
                        ),
                    }
                }
                Ok([a]) => fb(
                    format!(
                        "decision model {model} below min_confidence ({:.2}); fallback route",
                        a.confidence()
                    ),
                    warnings,
                ),
                Ok(_) => fb(
                    format!("decision model {model} gave no usable answer; fallback route"),
                    warnings,
                ),
                Err(e) => fb(
                    format!("decision model {model} failed ({e}); fallback route"),
                    warnings,
                ),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rag2_config::schema::{Profile, RouteChoice, RouteRule};
    use rag2_core::interfaces::DecisionProvider;
    use std::collections::BTreeMap;

    fn pipeline() -> Pipeline {
        Profile::minimal("x").pipelines["default"].clone()
    }

    #[test]
    fn rules_match_in_order_and_fall_back() {
        let mut pl = pipeline();
        pl.routing = Routing::Rules {
            rules: vec![
                RouteRule {
                    when: RouteCondition::HasIdentifier,
                    retrievers: vec![RetrieverKind::Lexical],
                },
                RouteRule {
                    when: RouteCondition::MinWords(6),
                    retrievers: vec![RetrieverKind::Dense],
                },
            ],
            fallback: vec![RetrieverKind::Dense, RetrieverKind::Lexical],
        };
        let c = Components::default();
        let mut w = Vec::new();
        assert_eq!(
            decide(&pl, &QueryRequest::new("error E1234 on boot"), &c, &mut w).retrievers,
            vec![RetrieverKind::Lexical]
        );
        assert_eq!(
            decide(
                &pl,
                &QueryRequest::new("why does the pump fail at night"),
                &c,
                &mut w
            )
            .retrievers,
            vec![RetrieverKind::Dense]
        );
        assert_eq!(
            decide(&pl, &QueryRequest::new("pumps"), &c, &mut w)
                .retrievers
                .len(),
            2
        );
    }

    struct Fixed(Answer);
    impl DecisionProvider for Fixed {
        fn name(&self) -> &str {
            "fixed"
        }
        fn healthy(&self) -> bool {
            true
        }
        fn decide(&self, _: &str, _: &[Question]) -> rag2_core::Result<Vec<Answer>> {
            Ok(vec![self.0.clone()])
        }
    }

    fn decision_pipeline() -> Pipeline {
        let mut pl = pipeline();
        pl.routing = Routing::Decision {
            model: "m".into(),
            instructions: "Which retrieval suits this query?".into(),
            routes: BTreeMap::from([
                (
                    "keyword".into(),
                    RouteChoice {
                        description: "exact terms".into(),
                        retrievers: vec![RetrieverKind::Lexical],
                    },
                ),
                (
                    "semantic".into(),
                    RouteChoice {
                        description: "meaning".into(),
                        retrievers: vec![RetrieverKind::Dense],
                    },
                ),
            ]),
            min_confidence: 0.8,
            fallback: vec![RetrieverKind::Dense, RetrieverKind::Lexical],
        };
        pl
    }

    #[test]
    fn decision_routing_uses_confident_answers_and_falls_back_otherwise() {
        let pl = decision_pipeline();
        let mut c = Components::default();
        let mut w = Vec::new();
        c.decision_models.insert(
            "m".into(),
            Arc::new(Fixed(Answer::Choice {
                label: "keyword".into(),
                confidence: 0.9,
            })),
        );
        assert_eq!(
            decide(&pl, &QueryRequest::new("q"), &c, &mut w).retrievers,
            vec![RetrieverKind::Lexical]
        );
        assert!(w.is_empty());
        c.decision_models.insert(
            "m".into(),
            Arc::new(Fixed(Answer::Choice {
                label: "keyword".into(),
                confidence: 0.5,
            })),
        );
        assert_eq!(
            decide(&pl, &QueryRequest::new("q"), &c, &mut w)
                .retrievers
                .len(),
            2
        );
        c.decision_models.insert(
            "m".into(),
            Arc::new(Fixed(Answer::Choice {
                label: "bogus".into(),
                confidence: 0.99,
            })),
        );
        assert_eq!(
            decide(&pl, &QueryRequest::new("q"), &c, &mut w)
                .retrievers
                .len(),
            2
        );
        assert_eq!(w.len(), 2);
    }
}
