use crate::model::Symbol;
use std::collections::HashMap;

pub fn compute_in_degree_pagerank(
    symbols: &mut [Symbol],
    callers_map: &HashMap<usize, Vec<usize>>,
    callees_map: &HashMap<usize, Vec<usize>>,
    focus_indices: Option<&[usize]>,
) {
    let n = symbols.len();
    if n == 0 {
        return;
    }

    let damping = 0.85;
    let iterations = 25;

    let mut scores = vec![1.0 / n as f64; n];
    let mut next_scores = vec![0.0; n];

    // Personalization vector
    let personalization: Vec<f64> = match focus_indices {
        Some(indices) if !indices.is_empty() => {
            let mut p = vec![0.0; n];
            let mass = 1.0 / indices.len() as f64;
            for &idx in indices {
                if idx < n {
                    p[idx] = mass;
                }
            }
            p
        }
        _ => vec![1.0 / n as f64; n],
    };

    for _ in 0..iterations {
        let mut dangling_sum = 0.0;
        for (u, &score) in scores.iter().enumerate() {
            let out_degree = callees_map.get(&u).map(|v| v.len()).unwrap_or(0);
            if out_degree == 0 {
                dangling_sum += score;
            }
        }

        for v in 0..n {
            let mut in_sum = 0.0;
            if let Some(callers) = callers_map.get(&v) {
                for &u in callers {
                    let out_degree = callees_map.get(&u).map(|list| list.len()).unwrap_or(1);
                    if out_degree > 0 {
                        in_sum += scores[u] / out_degree as f64;
                    }
                }
            }

            next_scores[v] = (1.0 - damping) * personalization[v]
                + damping * (in_sum + dangling_sum * personalization[v]);
        }

        scores.copy_from_slice(&next_scores);
    }

    // Normalize and assign to symbols
    for (i, sym) in symbols.iter_mut().enumerate() {
        sym.centrality = scores[i];
    }
}
