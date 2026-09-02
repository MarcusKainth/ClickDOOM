//! Walking the BSP tree, as SQL expression text.
//!
//! The engine descends from the root node to a leaf one partition line at a
//! time. A `while` loop is a fold over a fixed number of steps here: once a
//! step reaches a subsector the accumulator stops changing, so folding past
//! the tree's depth costs a step and changes nothing.

use super::fixed;

/// The bit a node child sets to say it names a subsector rather than
/// another node.
const NF_SUBSECTOR: u32 = 0x8000;

/// The constant arrays a descent reads, each indexed by node number plus
/// one. `child0` holds the child `R_PointOnSide` returns 0 for.
pub struct Nodes<'a> {
    pub x: &'a str,
    pub y: &'a str,
    pub dx: &'a str,
    pub dy: &'a str,
    pub child0: &'a str,
    pub child1: &'a str,
    /// How many nodes the level has.
    pub count: &'a str,
}

/// `R_PointInSubsector`: the number of the subsector holding `(x, y)`.
///
/// `depth` is how many partition lines the deepest leaf sits under. A level
/// with no nodes is one subsector, and the answer is 0.
pub fn point_in_subsector(x: &str, y: &str, nodes: &Nodes<'_>, depth: &str) -> String {
    let at = "(1 + acc)";
    let side = fixed::point_on_side(
        x,
        y,
        &format!("{}[{at}]", nodes.x),
        &format!("{}[{at}]", nodes.y),
        &format!("{}[{at}]", nodes.dx),
        &format!("{}[{at}]", nodes.dy),
        16,
    );
    let step = format!(
        "if(bitAnd(acc, {NF_SUBSECTOR}) != 0, acc, \
         toUInt32(if({side} = 0, {}[{at}], {}[{at}])))",
        nodes.child0, nodes.child1
    );
    let descent = format!(
        "arrayFold((acc, step) -> {step}, range({depth}), toUInt32({} - 1))",
        nodes.count
    );
    format!(
        "if({} = 0, toUInt32(0), toUInt32(bitAnd({descent}, {})))",
        nodes.count,
        NF_SUBSECTOR - 1
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nodes() -> Nodes<'static> {
        Nodes {
            x: "node_x",
            y: "node_y",
            dx: "node_dx",
            dy: "node_dy",
            child0: "node_c0",
            child1: "node_c1",
            count: "node_n",
        }
    }

    #[test]
    fn the_descent_starts_at_the_root_and_stops_at_a_leaf() {
        let text = point_in_subsector("mx", "my", &nodes(), "19");
        assert!(text.contains("toUInt32(node_n - 1)"));
        assert!(text.contains("range(19)"));
        assert!(text.contains("bitAnd(acc, 32768) != 0, acc"));
        assert!(text.contains("bitAnd(arrayFold"));
        assert!(text.starts_with("if(node_n = 0, toUInt32(0),"));
    }

    #[test]
    fn the_descent_balances_its_parentheses() {
        let text = point_in_subsector("mx", "my", &nodes(), "19");
        let depth = text.chars().fold(0i32, |d, c| match c {
            '(' => d + 1,
            ')' => d - 1,
            _ => d,
        });
        assert_eq!(depth, 0, "{text}");
    }
}
