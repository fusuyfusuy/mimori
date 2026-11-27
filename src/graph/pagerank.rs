use crate::model::Symbol;
use std::collections::HashMap;

/// Convergence tolerance (L1 residual) and iteration cap.
pub const CONVERGENCE_TOL: f64 = 1e-6;
pub const MAX_ITERATIONS: usize = 100;

/// Weighted PageRank result: iterations run and whether the L1 residual
/// dropped below [`CONVERGENCE_TOL`] before the cap.
pub struct PagerankStats {
    pub iters: usize,
    pub converged: bool,
}

/// Reverse-edge diffusion factor used only during `--focus` / `--seed`
/// Personalized PageRank so leaf hubs (`out_deg == 0`) diffuse focus mass back
/// to their callers instead of collapsing to `1.0`.
const FOCUS_REVERSE_WEIGHT: f64 = 0.35;

/// Weighted variant: `edge_weights[(u, v)]` is the sublinear multiplicity
/// weight of the u->v edge; missing entries weigh 1.0.
pub fn compute_weighted_pagerank(
    symbols: &mut [Symbol],
    callers_map: &HashMap<usize, Vec<usize>>,
    callees_map: &HashMap<usize, Vec<usize>>,
    edge_weights: &HashMap<(usize, usize), f64>,
    focus_indices: Option<&[usize]>,
) -> PagerankStats {
    let (scores, stats) = compute_pagerank_scores(
        symbols,
        callers_map,
        callees_map,
        edge_weights,
        focus_indices,
    );
    for (i, sym) in symbols.iter_mut().enumerate() {
        sym.centrality = scores[i];
    }
    stats
}

/// Score-decoupled Weighted PageRank over a flat CSR transition matrix.
pub fn compute_pagerank_scores(
    symbols: &[Symbol],
    callers_map: &HashMap<usize, Vec<usize>>,
    callees_map: &HashMap<usize, Vec<usize>>,
    edge_weights: &HashMap<(usize, usize), f64>,
    focus_indices: Option<&[usize]>,
) -> (Vec<f64>, PagerankStats) {
    let n = symbols.len();
    if n == 0 {
        return (
            Vec::new(),
            PagerankStats {
                iters: 0,
                converged: true,
            },
        );
    }

    let damping = 0.85;
    let is_test: Vec<bool> = symbols
        .iter()
        .map(crate::graph::blast::is_test_symbol)
        .collect();

    let default_prior = || -> Vec<f64> {
        let mut p: Vec<f64> = is_test
            .iter()
            .map(|&t| {
                if t {
                    crate::graph::TEST_EDGE_WEIGHT
                } else {
                    1.0
                }
            })
            .collect();
        let sum: f64 = p.iter().sum();
        if sum > 0.0 {
            for v in &mut p {
                *v /= sum;
            }
        }
        p
    };

    let mut scores = vec![1.0 / n as f64; n];
    let mut next_scores = vec![0.0; n];

    let (personalization, has_focus): (Vec<f64>, bool) = match focus_indices {
        Some(indices) if !indices.is_empty() => {
            let mut valid: Vec<usize> = indices.iter().copied().filter(|&i| i < n).collect();
            valid.sort_unstable();
            valid.dedup();
            if valid.is_empty() {
                (default_prior(), false)
            } else {
                let mut p = vec![0.0; n];
                let mass = 1.0 / valid.len() as f64;
                for idx in valid {
                    p[idx] = mass;
                }
                (p, true)
            }
        }
        _ => (default_prior(), false),
    };

    let edge_w = |u: usize, v: usize| -> f64 { edge_weights.get(&(u, v)).copied().unwrap_or(1.0) };
    let rev_edge_w = |callee: usize, caller: usize| -> f64 {
        let caller_scale = if is_test[caller] {
            crate::graph::TEST_EDGE_WEIGHT
        } else {
            1.0
        };
        FOCUS_REVERSE_WEIGHT * edge_w(caller, callee) * caller_scale
    };

    let out_weight: Vec<f64> = (0..n)
        .map(|u| {
            let fwd = match callees_map.get(&u) {
                Some(callees) if !edge_weights.is_empty() => {
                    callees.iter().map(|&v| edge_w(u, v)).sum()
                }
                Some(callees) => callees.len() as f64,
                None => 0.0,
            };
            let rev = if has_focus {
                callers_map.get(&u).map_or(0.0, |callers| {
                    callers.iter().map(|&c| rev_edge_w(u, c)).sum()
                })
            } else {
                0.0
            };
            fwd + rev
        })
        .collect();

    // Test callers retain only `TEST_EDGE_WEIGHT` of their damped walk mass
    // along outbound call edges; the remainder teleports back to `personalization`.
    let edge_retention = |u: usize| -> f64 {
        if is_test[u] {
            crate::graph::TEST_EDGE_WEIGHT
        } else {
            1.0
        }
    };

    let fwd_edges: usize = callers_map.values().map(|c| c.len()).sum();
    let rev_edges: usize = if has_focus {
        callees_map.values().map(|c| c.len()).sum()
    } else {
        0
    };
    let mut csr_offsets: Vec<usize> = Vec::with_capacity(n + 1);
    let mut csr_edges: Vec<(u32, f64)> = Vec::with_capacity(fwd_edges + rev_edges);
    csr_offsets.push(0);
    for v in 0..n {
        if let Some(callers) = callers_map.get(&v) {
            for &u in callers {
                let total = out_weight[u];
                if total > 0.0 {
                    let prob = edge_retention(u) * edge_w(u, v) / total;
                    csr_edges.push((u as u32, prob));
                }
            }
        }
        if has_focus {
            if let Some(callees) = callees_map.get(&v) {
                for &u in callees {
                    let total = out_weight[u];
                    if total > 0.0 {
                        let prob = edge_retention(u) * rev_edge_w(u, v) / total;
                        csr_edges.push((u as u32, prob));
                    }
                }
            }
        }
        csr_offsets.push(csr_edges.len());
    }

    let leak_nodes: Vec<(usize, f64)> = (0..n)
        .filter_map(|u| {
            let f = if out_weight[u] == 0.0 {
                1.0
            } else {
                1.0 - edge_retention(u)
            };
            (f > 0.0).then_some((u, f))
        })
        .collect();

    let mut stats = PagerankStats {
        iters: 0,
        converged: false,
    };
    for iter in 0..MAX_ITERATIONS {
        let leaked_sum: f64 = leak_nodes.iter().map(|&(u, f)| scores[u] * f).sum();

        for v in 0..n {
            let start = csr_offsets[v];
            let end = csr_offsets[v + 1];
            let mut in_sum = 0.0;
            for &(u, trans_prob) in &csr_edges[start..end] {
                in_sum += scores[u as usize] * trans_prob;
            }
            next_scores[v] = (1.0 - damping) * personalization[v]
                + damping * (in_sum + leaked_sum * personalization[v]);
        }

        let residual: f64 = next_scores
            .iter()
            .zip(scores.iter())
            .map(|(a, b)| (a - b).abs())
            .sum();
        scores.copy_from_slice(&next_scores);
        stats.iters = iter + 1;
        if residual < CONVERGENCE_TOL {
            stats.converged = true;
            break;
        }
    }

    (scores, stats)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::SymbolKind;
    use std::collections::BTreeMap;

    fn mk(file: &str, name: &str) -> Symbol {
        Symbol {
            name: name.into(),
            kind: SymbolKind::Function,
            file: file.into(),
            start_line: 1,
            end_line: 3,
            signature: format!("pub fn {name}()"),
            body: String::new(),
            centrality: 0.0,
            calls: vec![],
            mentions: vec![],
            call_counts: BTreeMap::new(),
            member_calls: vec![],
            external_imports: crate::model::symbol::empty_external_imports(),
            flags: crate::model::symbol_flags::IS_PUBLIC,
        }
    }

    #[test]
    fn focus_on_leaf_hub_diffuses_rank_to_callers() {
        let symbols = vec![
            mk("src/common.rs", "node_text"),
            mk("src/a.rs", "caller_a"),
            mk("src/b.rs", "caller_b"),
            mk("src/unrelated.rs", "unrelated"),
        ];
        let mut callers_map: HashMap<usize, Vec<usize>> = HashMap::new();
        let mut callees_map: HashMap<usize, Vec<usize>> = HashMap::new();
        callers_map.insert(0, vec![1, 2]);
        callees_map.insert(1, vec![0]);
        callees_map.insert(2, vec![0]);

        let (scores, stats) = compute_pagerank_scores(
            &symbols,
            &callers_map,
            &callees_map,
            &HashMap::new(),
            Some(&[0]),
        );
        assert!(stats.converged);
        assert!(scores[1] > 0.05, "caller_a got {}", scores[1]);
        assert!(scores[2] > 0.05, "caller_b got {}", scores[2]);
        assert!(scores[1] > scores[3]);
        assert!(scores[0] < 0.99, "leaf hub collapsed: {}", scores[0]);
    }
}
