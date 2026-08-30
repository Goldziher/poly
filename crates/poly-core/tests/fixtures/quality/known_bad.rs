// Known-bad fixture for the `quality` engine (see `tests/quality.rs`).
// Deliberately triggers: too-many-parameters, nesting-too-deep,
// cyclomatic-complexity, and function-too-long (custom low threshold).
// lazy-ignore lives in `known_bad_lazy_ignore.py`: its markers are other
// tools' suppressions, none of which are Rust syntax.

fn many_parameters(a: i32, b: i32, c: i32, d: i32, e: i32, f: i32, g: i32) -> i32 {
    a + b + c + d + e + f + g
}

fn deeply_nested(items: &[i32]) -> i32 {
    let mut total = 0;
    for item in items {
        if *item > 0 {
            while total < 100 {
                match item {
                    1 => {
                        if *item == 1 {
                            total += 1;
                        }
                    }
                    _ => {
                        total += 2;
                    }
                }
                break;
            }
        } else if *item < 0 {
            total -= 1;
        }
    }
    total
}

fn branchy(n: i32) -> i32 {
    if n == 0 {
        return 0;
    } else if n == 1 {
        return 1;
    } else if n == 2 {
        return 2;
    } else if n == 3 {
        return 3;
    } else if n == 4 {
        return 4;
    }
    n
}

fn long_function(n: i32) -> i32 {
    let mut total = n;
    total += 1;
    total += 2;
    total += 3;
    total += 4;
    total += 5;
    total
}
