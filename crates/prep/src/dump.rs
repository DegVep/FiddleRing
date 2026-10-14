//! Development-only: pretty-print a property bin with CommunityDragon names.
use lolread::bin::{Bin, Value};
use std::collections::HashMap;
use std::fmt::Write;

pub struct Names(HashMap<u32, String>);

impl Names {
    pub fn load(dir: &str) -> Names {
        let mut m = HashMap::new();
        for f in ["hashes.binfields.txt", "hashes.bintypes.txt", "hashes.binentries.txt", "hashes.binhashes.txt"] {
            if let Ok(t) = std::fs::read_to_string(format!("{dir}/{f}")) {
                for l in t.lines() {
                    if let Some((h, n)) = l.split_once(' ') {
                        if let Ok(h) = u32::from_str_radix(h, 16) { m.insert(h, n.to_string()); }
                    }
                }
            }
        }
        Names(m)
    }
    fn n(&self, h: u32) -> String { self.0.get(&h).cloned().unwrap_or(format!("{h:08x}")) }
}

fn val(names: &Names, v: &Value, ind: usize, out: &mut String) {
    let pad = "  ".repeat(ind);
    match v {
        Value::Struct(c, f) => {
            let _ = writeln!(out, "{} {{", names.n(*c));
            for (k, x) in f { let _ = write!(out, "{pad}  {}: ", names.n(*k)); val(names, x, ind + 1, out); }
            let _ = writeln!(out, "{pad}}}");
        }
        Value::List(l) => {
            let _ = writeln!(out, "[");
            for x in l { let _ = write!(out, "{pad}  "); val(names, x, ind + 1, out); }
            let _ = writeln!(out, "{pad}]");
        }
        Value::Map(m) => {
            let _ = writeln!(out, "{{");
            for (k, x) in m { let _ = write!(out, "{pad}  "); val(names, k, ind + 1, out); let _ = write!(out, "{pad}  => "); val(names, x, ind + 1, out); }
            let _ = writeln!(out, "{pad}}}");
        }
        Value::Hash(h) => { let _ = writeln!(out, "#{}", names.n(*h)); }
        Value::ObjLink(h) => { let _ = writeln!(out, "link {}", names.n(*h)); }
        Value::Opt(Some(x)) => val(names, x, ind, out),
        x => { let _ = writeln!(out, "{x:?}"); }
    }
}

pub fn dump(names: &Names, b: &Bin) -> String {
    let mut out = String::new();
    for o in &b.objects {
        let _ = write!(out, "== {} ", names.n(o.path));
        val(names, &o.as_value(), 0, &mut out);
    }
    out
}
