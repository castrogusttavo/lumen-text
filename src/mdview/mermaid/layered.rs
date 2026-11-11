//! A small layered (Sugiyama-style) graph layout: cycles broken by DFS, ranks by longest path,
//! long edges split by dummy vertices, crossings reduced by barycenter sweeps, coordinates by
//! iterated neighbor averaging under minimum separation. Good enough for the flowcharts and class
//! diagrams people put in READMEs, and linear-ish in their size.

use std::collections::VecDeque;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dir {
    TB,
    BT,
    LR,
    RL,
}

impl Dir {
    pub fn parse(s: &str) -> Dir {
        match s.trim().to_ascii_uppercase().as_str() {
            "LR" => Dir::LR,
            "RL" => Dir::RL,
            "BT" => Dir::BT,
            _ => Dir::TB,
        }
    }
    pub fn horizontal(self) -> bool {
        matches!(self, Dir::LR | Dir::RL)
    }
}

#[derive(Debug, Default)]
pub struct Out {
    /// Node centers.
    pub pos: Vec<(f32, f32)>,
    /// Per edge: from the source center through the bends to the target center (empty for a self
    /// loop or an invalid edge).
    pub paths: Vec<Vec<(f32, f32)>>,
    /// Per edge: where its label goes.
    pub label_at: Vec<(f32, f32)>,
    pub w: f32,
    pub h: f32,
}

/// Lay out `nodes` (`(w, h)`) joined by `edges` (`(from, to, label size)`).
pub fn layout(
    nodes: &[(f32, f32)],
    edges: &[(usize, usize, (f32, f32))],
    dir: Dir,
    node_gap: f32,
    rank_gap: f32,
) -> Out {
    let n = nodes.len();
    if n == 0 {
        return Out::default();
    }
    let horiz = dir.horizontal();
    // (cross, along) size of a node: along runs from rank to rank.
    let cs = |i: usize| -> (f32, f32) {
        let (w, h) = nodes[i];
        if horiz { (h, w) } else { (w, h) }
    };
    let valid: Vec<bool> = edges.iter().map(|e| e.0 < n && e.1 < n && e.0 != e.1).collect();

    // 1. Break cycles: edges that close a cycle in DFS order are reversed.
    let mut out_e: Vec<Vec<usize>> = vec![vec![]; n];
    for (i, e) in edges.iter().enumerate() {
        if valid[i] {
            out_e[e.0].push(i);
        }
    }
    let mut rev = vec![false; edges.len()];
    let mut color = vec![0u8; n];
    for s in 0..n {
        if color[s] != 0 {
            continue;
        }
        color[s] = 1;
        let mut stack = vec![(s, 0usize)];
        loop {
            let Some(top) = stack.last_mut() else { break };
            let (v, k) = *top;
            if k < out_e[v].len() {
                top.1 += 1;
                let ei = out_e[v][k];
                let t = edges[ei].1;
                match color[t] {
                    0 => {
                        color[t] = 1;
                        stack.push((t, 0));
                    }
                    1 => rev[ei] = true,
                    _ => {}
                }
            } else {
                color[v] = 2;
                stack.pop();
            }
        }
    }
    let dag: Vec<Option<(usize, usize)>> = edges
        .iter()
        .enumerate()
        .map(|(i, e)| valid[i].then_some(if rev[i] { (e.1, e.0) } else { (e.0, e.1) }))
        .collect();

    // 2. Ranks: longest path from the sources, then sources pulled down next to their children.
    let mut indeg = vec![0usize; n];
    let mut succ: Vec<Vec<usize>> = vec![vec![]; n];
    for &(u, v) in dag.iter().flatten() {
        indeg[v] += 1;
        succ[u].push(v);
    }
    let has_pred: Vec<bool> = indeg.iter().map(|d| *d > 0).collect();
    let mut rank = vec![0usize; n];
    let mut q: VecDeque<usize> = (0..n).filter(|&v| indeg[v] == 0).collect();
    while let Some(v) = q.pop_front() {
        for &t in &succ[v] {
            rank[t] = rank[t].max(rank[v] + 1);
            indeg[t] -= 1;
            if indeg[t] == 0 {
                q.push_back(t);
            }
        }
    }
    let pulled: Vec<Option<usize>> = (0..n)
        .map(|v| {
            if has_pred[v] {
                return None;
            }
            succ[v].iter().map(|&t| rank[t]).min().filter(|m| *m >= 1).map(|m| m - 1)
        })
        .collect();
    for (r, p) in rank.iter_mut().zip(pulled) {
        if let Some(p) = p {
            *r = p;
        }
    }

    // 3. Vertices: the nodes, then one dummy per rank crossed by a long edge.
    let mut vrank = rank.clone();
    let mut vsize: Vec<(f32, f32)> = (0..n).map(cs).collect();
    let mut vpred: Vec<Vec<usize>> = vec![vec![]; n];
    let mut vsucc: Vec<Vec<usize>> = vec![vec![]; n];
    let mut chains: Vec<Vec<usize>> = vec![vec![]; edges.len()];
    for (i, d) in dag.iter().enumerate() {
        let Some((u, v)) = *d else { continue };
        let mut chain = vec![u];
        let mut prev = u;
        for r in rank[u] + 1..rank[v] {
            let id = vrank.len();
            vrank.push(r);
            vsize.push((0.0, 0.0));
            vpred.push(vec![prev]);
            vsucc.push(vec![]);
            vsucc[prev].push(id);
            chain.push(id);
            prev = id;
        }
        vsucc[prev].push(v);
        vpred[v].push(prev);
        chain.push(v);
        chains[i] = chain;
    }
    let nv = vrank.len();
    let nr = vrank.iter().copied().max().unwrap_or(0) + 1;

    // 4. Order inside each rank: breadth-first discovery, then barycenter sweeps.
    let mut seen = vec![false; nv];
    let mut order: Vec<usize> = Vec::with_capacity(nv);
    for s in (0..nv).filter(|&v| vpred[v].is_empty()) {
        if seen[s] {
            continue;
        }
        seen[s] = true;
        let mut bq = VecDeque::from([s]);
        while let Some(v) = bq.pop_front() {
            order.push(v);
            for &t in &vsucc[v] {
                if !seen[t] {
                    seen[t] = true;
                    bq.push_back(t);
                }
            }
        }
    }
    order.extend((0..nv).filter(|&v| !seen[v]));
    let mut layers: Vec<Vec<usize>> = vec![vec![]; nr];
    for &v in &order {
        layers[vrank[v]].push(v);
    }
    let mut pos_in = vec![0f32; nv];
    let reindex = |layers: &[Vec<usize>], pos_in: &mut [f32]| {
        for l in layers {
            for (i, &v) in l.iter().enumerate() {
                pos_in[v] = i as f32;
            }
        }
    };
    reindex(&layers, &mut pos_in);
    for it in 0..12 {
        let down = it % 2 == 0;
        let ranks: Vec<usize> = if down { (1..nr).collect() } else { (0..nr.saturating_sub(1)).rev().collect() };
        for r in ranks {
            let mut keyed: Vec<(f32, usize)> = layers[r]
                .iter()
                .map(|&v| {
                    let nb = if down { &vpred[v] } else { &vsucc[v] };
                    let k = if nb.is_empty() {
                        pos_in[v]
                    } else {
                        nb.iter().map(|&u| pos_in[u]).sum::<f32>() / nb.len() as f32
                    };
                    (k, v)
                })
                .collect();
            keyed.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
            layers[r] = keyed.into_iter().map(|k| k.1).collect();
            for (i, &v) in layers[r].iter().enumerate() {
                pos_in[v] = i as f32;
            }
        }
    }

    // 5. Cross coordinates: packed, centered, then pulled toward the neighbors' average.
    let gap_of = |a: usize, b: usize| -> f32 {
        match (a < n, b < n) {
            (true, true) => node_gap,
            (false, false) => 12.0,
            _ => node_gap * 0.6,
        }
    };
    let sep = |a: usize, b: usize, vsize: &[(f32, f32)]| vsize[a].0 / 2.0 + gap_of(a, b) + vsize[b].0 / 2.0;
    let mut cx = vec![0f32; nv];
    for l in &layers {
        let mut x = 0.0;
        for (i, &v) in l.iter().enumerate() {
            if i > 0 {
                x += sep(l[i - 1], v, &vsize);
            }
            cx[v] = x;
        }
        if let (Some(&f), Some(&la)) = (l.first(), l.last()) {
            let mid = (cx[f] + cx[la]) / 2.0;
            for &v in l {
                cx[v] -= mid;
            }
        }
    }
    let rank_order: Vec<usize> = (0..nr).chain((0..nr).rev()).collect();
    for _ in 0..4 {
        for &r in &rank_order {
            let l = &layers[r];
            let k = l.len();
            if k == 0 {
                continue;
            }
            let want: Vec<f32> = l
                .iter()
                .map(|&v| {
                    let nb: Vec<usize> = vpred[v].iter().chain(vsucc[v].iter()).copied().collect();
                    if nb.is_empty() { cx[v] } else { nb.iter().map(|&u| cx[u]).sum::<f32>() / nb.len() as f32 }
                })
                .collect();
            let mut a = want.clone();
            for i in 1..k {
                let min = a[i - 1] + sep(l[i - 1], l[i], &vsize);
                if a[i] < min {
                    a[i] = min;
                }
            }
            let mut b = want;
            for i in (0..k - 1).rev() {
                let max = b[i + 1] - sep(l[i], l[i + 1], &vsize);
                if b[i] > max {
                    b[i] = max;
                }
            }
            let mut x: Vec<f32> = a.iter().zip(&b).map(|(p, q)| (p + q) / 2.0).collect();
            for i in 1..k {
                let min = x[i - 1] + sep(l[i - 1], l[i], &vsize);
                if x[i] < min {
                    x[i] = min;
                }
            }
            for (i, &v) in l.iter().enumerate() {
                cx[v] = x[i];
            }
        }
    }

    // 6. Along coordinates: rank thickness plus gaps that make room for edge labels.
    let mut thick = vec![0f32; nr];
    for (r, s) in vrank.iter().zip(&vsize) {
        thick[*r] = thick[*r].max(s.1);
    }
    let mut gaps = vec![rank_gap; nr];
    for (i, d) in dag.iter().enumerate() {
        let Some((u, v)) = *d else { continue };
        let (lw, lh) = edges[i].2;
        if lw <= 0.0 {
            continue;
        }
        let ext = if horiz { lw } else { lh } + 22.0;
        let g = rank[u] + (rank[v] - rank[u] - 1) / 2;
        if let Some(gg) = gaps.get_mut(g) {
            *gg = gg.max(ext);
        }
    }
    let mut ay = vec![0f32; nr];
    let mut acc = 0.0;
    for r in 0..nr {
        ay[r] = acc + thick[r] / 2.0;
        acc += thick[r] + if r + 1 < nr { gaps[r] } else { 0.0 };
    }
    let along = acc;
    let min_c = (0..nv).map(|v| cx[v] - vsize[v].0 / 2.0).fold(f32::MAX, f32::min);
    let max_c = (0..nv).map(|v| cx[v] + vsize[v].0 / 2.0).fold(f32::MIN, f32::max);
    for c in cx.iter_mut() {
        *c -= min_c;
    }
    let cross = (max_c - min_c).max(1.0);
    let map = |c: f32, a: f32| -> (f32, f32) {
        match dir {
            Dir::TB => (c, a),
            Dir::BT => (c, along - a),
            Dir::LR => (a, c),
            Dir::RL => (along - a, c),
        }
    };
    let pos: Vec<(f32, f32)> = (0..n).map(|v| map(cx[v], ay[vrank[v]])).collect();
    let mut paths = vec![vec![]; edges.len()];
    let mut label_at = vec![(0.0, 0.0); edges.len()];
    for (i, chain) in chains.iter().enumerate() {
        if chain.len() < 2 {
            if valid[i] || edges[i].0 == edges[i].1 {
                // self loop: label to the side of the node
                if let Some(&(x, y)) = pos.get(edges[i].0) {
                    let (w, h) = nodes[edges[i].0];
                    label_at[i] = (x + w / 2.0 + 30.0, y - h / 2.0 - 6.0);
                }
            }
            continue;
        }
        let pts: Vec<(f32, f32)> = chain.iter().map(|&v| map(cx[v], ay[vrank[v]])).collect();
        let k = (chain.len() - 2) / 2;
        label_at[i] = ((pts[k].0 + pts[k + 1].0) / 2.0, (pts[k].1 + pts[k + 1].1) / 2.0);
        let mut pts = pts;
        if rev[i] {
            pts.reverse();
        }
        paths[i] = pts;
    }
    let (w, h) = if horiz { (along, cross) } else { (cross, along) };
    Out { pos, paths, label_at, w, h }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chain_ranks_go_down_and_cycles_do_not_hang() {
        let nodes = vec![(60.0, 30.0); 4];
        let edges =
            vec![(0, 1, (0.0, 0.0)), (1, 2, (0.0, 0.0)), (2, 0, (0.0, 0.0)), (0, 3, (40.0, 14.0)), (3, 3, (0.0, 0.0))];
        let o = layout(&nodes, &edges, Dir::TB, 40.0, 50.0);
        assert_eq!(o.pos.len(), 4);
        assert!(o.pos[1].1 > o.pos[0].1 && o.pos[2].1 > o.pos[1].1, "{:?}", o.pos);
        assert!(o.paths[4].is_empty(), "self loop has no path");
        assert_eq!(o.paths[2].first(), Some(&o.pos[2]), "a reversed edge still runs from its source");
        assert!(o.w > 0.0 && o.h > 0.0);
        for (x, y) in &o.pos {
            assert!(x.is_finite() && y.is_finite());
        }
        let lr = layout(&nodes, &edges[..2], Dir::LR, 40.0, 50.0);
        assert!(lr.pos[1].0 > lr.pos[0].0 && (lr.pos[1].1 - lr.pos[0].1).abs() < 1.0);
    }

    #[test]
    fn siblings_do_not_overlap() {
        let nodes = vec![(80.0, 30.0); 5];
        let edges: Vec<(usize, usize, (f32, f32))> = (1..5).map(|i| (0, i, (0.0, 0.0))).collect();
        let o = layout(&nodes, &edges, Dir::TB, 30.0, 40.0);
        let mut xs: Vec<f32> = o.pos[1..].iter().map(|p| p.0).collect();
        xs.sort_by(|a, b| a.partial_cmp(b).unwrap());
        assert!(xs.windows(2).all(|w| w[1] - w[0] >= 80.0 + 30.0 - 0.01), "{xs:?}");
    }
}
