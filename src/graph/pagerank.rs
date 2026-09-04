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

    // Flatten the adjacency into arrays once. The keys are dense symbol
    // indices, so the HashMap lookups per node per iteration were pure
    // overhead: 25 iterations x (n dangling probes + n caller probes + E
    // out-degree probes).
    let out_degree: Vec<usize> = (0..n)
        .map(|u| callees_map.get(&u).map_or(0, |v| v.len()))
        .collect();

    let callers_of: Vec<&[usize]> = (0..n)
        .map(|v| callers_map.get(&v).map_or(&[][..], |c| c.as_slice()))
        .collect();

    // Which nodes are dangling never changes; only their scores do.
    let dangling: Vec<usize> = (0..n).filter(|&u| out_degree[u] == 0).collect();

    for _ in 0..iterations {
        let dangling_sum: f64 = dangling.iter().map(|&u| scores[u]).sum();

        for v in 0..n {
            let mut in_sum = 0.0;
            for &u in callers_of[v] {
                let d = out_degree[u];
                if d > 0 {
                    in_sum += scores[u] / d as f64;
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
