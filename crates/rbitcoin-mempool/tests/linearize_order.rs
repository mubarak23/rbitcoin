//! `DepGraph::linearize` is what a cluster chunk uses.

use rbitcoin_mempool::linearize::{DepGraph, FeeFrac};
use std::collections::HashSet;

/// Sixty-five independent txs, one past a 64-bit word. The highest feerate
/// is position 64 and comes out first. Mining reads this order.
#[test]
fn highest_singleton_past_one_word_comes_first() {
    let fees: Vec<_> = (0..65).map(|i| FeeFrac::new(i + 1, 1)).collect();
    let parents = vec![vec![]; 65];
    let order = DepGraph::new(fees, &parents).linearize();
    assert_eq!(order.len(), 65);
    assert_eq!(order.iter().copied().collect::<HashSet<_>>().len(), 65);
    assert_eq!(order[0], 64);
    assert_eq!(order[1], 63);
}

/// A hot child pulls its parent ahead of a cooler singleton. The picked
/// set is the ancestor intersection, so the cooler tx stays behind them.
#[test]
fn ancestor_set_stays_ahead_of_a_cooler_singleton() {
    let fees = vec![
        FeeFrac::new(1, 1),
        FeeFrac::new(100, 1),
        FeeFrac::new(50, 1),
    ];
    let parents = vec![vec![], vec![0], vec![]];
    let order = DepGraph::new(fees, &parents).linearize();
    assert_eq!(order, vec![0, 1, 2], "{order:?}");
}

fn cluster_order(fees: &[(i64, u64)], parents: &[Vec<usize>]) -> Vec<usize> {
    let fees: Vec<_> = fees
        .iter()
        .copied()
        .map(|(f, s)| FeeFrac::new(f, s))
        .collect();
    DepGraph::new(fees, parents).linearize()
}

/// Mining reads this order. The best ancestor set is not the lowest index.
#[test]
fn ancestor_set_feerate_beats_an_earlier_index() {
    let order = cluster_order(
        &[(8, 3), (23, 2), (4, 2), (25, 1), (-3, 1), (14, 2), (28, 1)],
        &[
            vec![],
            vec![],
            vec![],
            vec![2],
            vec![3],
            vec![2, 4],
            vec![0, 2],
        ],
    );
    assert_eq!(order, vec![1, 2, 3, 0, 6, 4, 5], "{order:?}");
}

/// Mining reads this order. Two ready chunks: the higher feerate is later.
#[test]
fn ready_chunk_feerate_beats_an_earlier_chunk() {
    let order = cluster_order(
        &[
            (1, 2),
            (19, 2),
            (12, 2),
            (20, 1),
            (12, 2),
            (16, 1),
            (12, 2),
            (12, 2),
        ],
        &[
            vec![],
            vec![0],
            vec![0],
            vec![2],
            vec![],
            vec![],
            vec![1, 3, 5],
            vec![3, 4],
        ],
    );
    assert_eq!(order, vec![5, 0, 1, 2, 3, 4, 6, 7], "{order:?}");
}

/// Mining reads this order. Inside one chunk the higher feerate is not the
/// lowest index.
#[test]
fn ready_tx_feerate_beats_an_earlier_index() {
    let order = cluster_order(
        &[
            (-5, 2),
            (27, 1),
            (30, 1),
            (18, 3),
            (17, 2),
            (24, 3),
            (30, 1),
        ],
        &[
            vec![],
            vec![],
            vec![1],
            vec![],
            vec![0],
            vec![0, 1, 2],
            vec![0, 2],
        ],
    );
    assert_eq!(order, vec![1, 2, 0, 6, 4, 5, 3], "{order:?}");
}

/// Mining reads this order. The backward merge runs before the forward one.
#[test]
fn backward_pass_runs_before_the_forward_pass() {
    let order = cluster_order(
        &[
            (23, 1),
            (15, 1),
            (-4, 1),
            (-2, 1),
            (28, 1),
            (20, 1),
            (28, 1),
        ],
        &[
            vec![],
            vec![0],
            vec![],
            vec![],
            vec![0, 3],
            vec![0, 1, 4],
            vec![0, 3],
        ],
    );
    assert_eq!(order, vec![0, 3, 4, 6, 1, 5, 2], "{order:?}");
}
