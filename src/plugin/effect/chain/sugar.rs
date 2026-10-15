//! `echo` and `reverb`: the two whole effects almost every plugin wants, written as the blocks they stand for. Each
//! returns the JSON of that expansion, which the base parses like any hand-written chain.

use super::{Args, R};
use serde_json::{Value, json};

/// `echo`: `gain g` (the repeats stop being fed when the effect is OFF, so they ring out), a `delay` of `time` with
/// `feedback` (its loop runs through the `damp` low-pass, then the blocks of `loop`), then `mix`: with `dry` (the
/// default) the repeats times `mix` are added to the input, without it only the repeats times `mix` come out (a send
/// return). `pingpong` puts the mono sum of the input on the right and swaps the repeats' sides.
pub fn echo(a: &mut Args) -> R<Vec<Value>> {
    let time = a.raw("time").unwrap_or(json!("period"));
    let feedback = a.raw("feedback").unwrap_or(json!(0.5));
    let max = a.raw("max").unwrap_or(json!(4));
    let mix = a.raw("mix").unwrap_or(json!("level"));
    let dry = a.flag("dry", true)?;
    let pingpong = a.flag("pingpong", false)?;
    let mut back = Vec::new();
    if let Some(hz) = a.raw("damp") {
        back.push(json!({"block": "filter", "type": "lp", "order": 1, "hz": hz}));
    }
    match a.raw("loop") {
        None => {}
        Some(Value::Array(more)) => back.extend(more),
        Some(_) => return a.fail("\"loop\" must be a list of blocks"),
    }
    let mut out = vec![json!({"block": "gain", "gain": "g"})];
    if pingpong {
        out.push(json!({"block": "route", "matrix": [[0, 0], [0.5, 0.5]]}));
        back.push(json!({"block": "route", "matrix": [[0, 1], [1, 0]]}));
    }
    out.push(
        json!({"block": "delay", "time": time, "max": max, "feedback": feedback, "loop": back}),
    );
    out.push(if dry {
        json!({"block": "mix", "mode": "add", "amount": mix})
    } else {
        json!({"block": "gain", "gain": mix})
    });
    Ok(out)
}

/// `reverb`: `gain g`, `freeverb` of `size`, then `mix` as in `echo` (`dry` adds the tail to the input, without it only
/// the tail times `mix` comes out).
pub fn reverb(a: &mut Args) -> R<Vec<Value>> {
    let size = a.raw("size").unwrap_or(json!(0.5));
    let mix = a.raw("mix").unwrap_or(json!("level"));
    let dry = a.flag("dry", true)?;
    Ok(vec![
        json!({"block": "gain", "gain": "g"}),
        json!({"block": "freeverb", "size": size}),
        if dry {
            json!({"block": "mix", "mode": "add", "amount": mix})
        } else {
            json!({"block": "gain", "gain": mix})
        },
    ])
}
