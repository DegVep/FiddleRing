//! League property bins (`PROP` / `PTCH`). Names are FNV-1a hashes of the lowercase field names.

use anyhow::{Result, bail};

pub fn fnv1a(s: &str) -> u32 {
    let mut h: u32 = 2166136261;
    for b in s.to_ascii_lowercase().bytes() {
        h ^= b as u32;
        h = h.wrapping_mul(16777619);
    }
    h
}

#[derive(Clone, Debug)]
pub enum Value {
    None,
    Bool(bool),
    I(i64),
    U(u64),
    F32(f32),
    Vec(Vec<f32>),
    Color([u8; 4]),
    Str(String),
    Hash(u32),
    Link(u64),
    List(Vec<Value>),
    Struct(u32, Vec<(u32, Value)>),
    ObjLink(u32),
    Opt(Option<Box<Value>>),
    Map(Vec<(Value, Value)>),
}

pub struct Object {
    pub path: u32,
    pub class: u32,
    pub fields: Vec<(u32, Value)>,
}

pub struct Bin {
    pub deps: Vec<String>,
    pub objects: Vec<Object>,
}

struct R<'a> {
    b: &'a [u8],
    o: usize,
}

impl<'a> R<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        if self.o + n > self.b.len() {
            bail!("bin truncated");
        }
        let s = &self.b[self.o..self.o + n];
        self.o += n;
        Ok(s)
    }
    fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }
    fn u16(&mut self) -> Result<u16> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into()?))
    }
    fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into()?))
    }
    fn u64(&mut self) -> Result<u64> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into()?))
    }
    fn f32(&mut self) -> Result<f32> {
        Ok(f32::from_le_bytes(self.take(4)?.try_into()?))
    }
}

fn unpack(t: u8, legacy: bool) -> u8 {
    let mut t = t;
    if legacy {
        if (18..128).contains(&t) {
            t = (t - 18) | 128;
        }
        if t >= 129 {
            t += 1;
        }
    }
    t
}

fn read_value(r: &mut R, t: u8, legacy: bool) -> Result<Value> {
    Ok(match t {
        0 => Value::None,
        1 => Value::Bool(r.u8()? != 0),
        2 => Value::I(r.u8()? as i8 as i64),
        3 => Value::U(r.u8()? as u64),
        4 => Value::I(r.u16()? as i16 as i64),
        5 => Value::U(r.u16()? as u64),
        6 => Value::I(r.u32()? as i32 as i64),
        7 => Value::U(r.u32()? as u64),
        8 => Value::I(r.u64()? as i64),
        9 => Value::U(r.u64()?),
        10 => Value::F32(r.f32()?),
        11 => Value::Vec(vec![r.f32()?, r.f32()?]),
        12 => Value::Vec(vec![r.f32()?, r.f32()?, r.f32()?]),
        13 => Value::Vec(vec![r.f32()?, r.f32()?, r.f32()?, r.f32()?]),
        14 => Value::Vec((0..16).map(|_| r.f32()).collect::<Result<_>>()?),
        15 => {
            let c = r.take(4)?;
            Value::Color([c[0], c[1], c[2], c[3]])
        }
        16 => {
            let n = r.u16()? as usize;
            Value::Str(String::from_utf8_lossy(r.take(n)?).into_owned())
        }
        17 => Value::Hash(r.u32()?),
        18 => Value::Link(r.u64()?),
        128 | 129 => {
            let et = unpack(r.u8()?, legacy);
            let _size = r.u32()?;
            let n = r.u32()?;
            let mut v = Vec::with_capacity(n as usize);
            for _ in 0..n {
                v.push(read_value(r, et, legacy)?);
            }
            Value::List(v)
        }
        130 | 131 => {
            let class = r.u32()?;
            if class == 0 {
                return Ok(Value::Struct(0, vec![]));
            }
            let _size = r.u32()?;
            let n = r.u16()?;
            let mut f = Vec::with_capacity(n as usize);
            for _ in 0..n {
                let name = r.u32()?;
                let ft = unpack(r.u8()?, legacy);
                f.push((name, read_value(r, ft, legacy)?));
            }
            Value::Struct(class, f)
        }
        132 => Value::ObjLink(r.u32()?),
        133 => {
            let vt = unpack(r.u8()?, legacy);
            let some = r.u8()? != 0;
            Value::Opt(if some { Some(Box::new(read_value(r, vt, legacy)?)) } else { None })
        }
        134 => {
            let kt = unpack(r.u8()?, legacy);
            let vt = unpack(r.u8()?, legacy);
            let _size = r.u32()?;
            let n = r.u32()?;
            let mut m = Vec::with_capacity(n as usize);
            for _ in 0..n {
                let k = read_value(r, kt, legacy)?;
                let v = read_value(r, vt, legacy)?;
                m.push((k, v));
            }
            Value::Map(m)
        }
        135 => Value::Bool(r.u8()? != 0),
        t => bail!("bad bin type {t}"),
    })
}

pub fn parse(b: &[u8]) -> Result<Bin> {
    let mut r = R { b, o: 0 };
    let mut magic = r.take(4)?;
    if magic == b"PTCH" {
        r.u32()?;
        r.u32()?;
        magic = r.take(4)?;
    }
    if magic != b"PROP" {
        bail!("not a property bin");
    }
    let version = r.u32()?;
    let mut deps = vec![];
    if version >= 2 {
        let n = r.u32()?;
        for _ in 0..n {
            let l = r.u16()? as usize;
            deps.push(String::from_utf8_lossy(r.take(l)?).into_owned());
        }
    }
    let n = r.u32()? as usize;
    let classes: Vec<u32> = (0..n).map(|_| r.u32()).collect::<Result<_>>()?;
    let start = r.o;
    for legacy in [false, true] {
        r.o = start;
        let mut objects = Vec::with_capacity(n);
        let mut ok = true;
        for &class in &classes {
            let res: Result<Object> = (|| {
                let size = r.u32()? as usize;
                let at = r.o;
                let path = r.u32()?;
                let fc = r.u16()?;
                let mut fields = Vec::with_capacity(fc as usize);
                for _ in 0..fc {
                    let name = r.u32()?;
                    let t = unpack(r.u8()?, legacy);
                    fields.push((name, read_value(&mut r, t, legacy)?));
                }
                if r.o != at + size {
                    bail!("object size mismatch");
                }
                Ok(Object { path, class, fields })
            })();
            match res {
                Ok(o) => objects.push(o),
                Err(_) => {
                    ok = false;
                    break;
                }
            }
        }
        if ok {
            return Ok(Bin { deps, objects });
        }
    }
    bail!("could not parse bin")
}

impl Value {
    pub fn field(&self, name: &str) -> Option<&Value> {
        let h = fnv1a(name);
        match self {
            Value::Struct(_, f) => f.iter().find(|(n, _)| *n == h).map(|(_, v)| v),
            Value::Opt(Some(v)) => v.field(name),
            _ => None,
        }
    }
    pub fn as_f32(&self) -> Option<f32> {
        match self {
            Value::F32(f) => Some(*f),
            Value::I(i) => Some(*i as f32),
            Value::U(u) => Some(*u as f32),
            _ => None,
        }
    }
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::Str(s) => Some(s),
            _ => None,
        }
    }
    pub fn list(&self) -> &[Value] {
        match self {
            Value::List(v) => v,
            _ => &[],
        }
    }
    /// Every string anywhere below this value.
    pub fn strings<'a>(&'a self, out: &mut Vec<&'a str>) {
        match self {
            Value::Str(s) => out.push(s),
            Value::List(v) => v.iter().for_each(|x| x.strings(out)),
            Value::Struct(_, f) => f.iter().for_each(|(_, x)| x.strings(out)),
            Value::Opt(Some(v)) => v.strings(out),
            Value::Map(m) => m.iter().for_each(|(k, v)| {
                k.strings(out);
                v.strings(out)
            }),
            _ => {}
        }
    }
}

impl Object {
    pub fn field(&self, name: &str) -> Option<&Value> {
        let h = fnv1a(name);
        self.fields.iter().find(|(n, _)| *n == h).map(|(_, v)| v)
    }
    pub fn as_value(&self) -> Value {
        Value::Struct(self.class, self.fields.clone())
    }
}
