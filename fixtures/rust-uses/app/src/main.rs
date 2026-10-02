use graphlib::graph as g;
use graphlib::graph::build;
use graphlib::make;
use graphlib::Edge;

fn main() {
    let a = build(1);
    let b = g::build(2);
    let c = make(3);
    let d = graphlib::graph::build(4);
    let e = Edge { from: 5 };
    let f = Edge::new(6);
    let total = |build: u32| build + 1;
    println!("{}", a.weight() + b.from + c.from + d.from + e.from + f.from + total(7));
    {
        use graphlib::graph::build as local;
        let _ = local(8);
    }
    let v = vec![build(9)];
    let _ = v;
}

mod globonly;
mod scopes;
