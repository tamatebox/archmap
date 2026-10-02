use graphlib::graph::build;
use graphlib::graph::LIMIT;

pub fn patterns(x: Option<u32>) -> u32 {
    if let Some(build) = x {
        let _ = build;
    }
    let a = build(1).from;
    match a {
        build => {
            let _ = build;
        }
    }
    let b = build(2).from;
    for build in [3u32] {
        let _ = build;
    }
    let c = build(4).from;
    while let Some(build) = None::<u32> {
        let _ = build;
    }
    let Some(build) = x else { return 0 };
    a + b + c + build
}

pub fn items(n: u32) -> u32 {
    fn inner() -> u32 {
        build(5).from
    }
    {
        fn build(n: u32) -> u32 {
            n
        }
        let _ = build(6);
    }
    n + inner()
}

pub fn hidden() -> u32 {
    use std::convert::identity as build;
    build(7)
}

pub fn limits(n: u32) -> bool {
    match n {
        LIMIT => true,
        _ => false,
    }
}
