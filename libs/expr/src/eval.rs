//! Evaluation of a compiled tree over JSON globals.
//!
//! Values borrowed from the globals are passed along as `Cow::Borrowed`,
//! so `$model.rows[0].id` never clones the model; only the final result is
//! owned.

use std::borrow::Cow;
use std::time::Instant;

use serde_json::Value;

use crate::ast::{BinOp, Coalesce, InterpVar, Node, Part, Segment, UnaryOp};
use crate::value::{equals, is_truthy, number, render, rendered, signed_index, to_number, type_name};
use crate::{Error, ErrorKind, Limits};

type Out<'v> = Result<Cow<'v, Value>, Error>;

pub(crate) struct Evaluator<'v> {
    globals: &'v [(&'v str, &'v Value)],
    limits: &'v Limits,
    deadline: Option<Instant>,
    steps: usize,
}

fn runtime(message: impl Into<String>) -> Error {
    Error::new(ErrorKind::Runtime, message)
}

fn budget(message: impl Into<String>) -> Error {
    Error::new(ErrorKind::Budget, message)
}

/// Apply a projection to a value without cloning when it is borrowed. A
/// miss is nil.
fn project<'v>(value: Cow<'v, Value>, select: impl Fn(&Value) -> Option<&Value>) -> Cow<'v, Value> {
    match value {
        Cow::Borrowed(v) => select(v).map(Cow::Borrowed).unwrap_or(Cow::Owned(Value::Null)),
        Cow::Owned(v) => Cow::Owned(select(&v).cloned().unwrap_or(Value::Null)),
    }
}

fn map_get<'a>(map: &'a serde_json::Map<String, Value>, key: &str) -> Option<&'a Value> {
    map.get(key).or_else(|| map.get("*"))
}

/// How an index applies, decided before the indexed value is moved.
enum Access {
    ListAt(f64),
    MapKey(String),
}

impl<'v> Evaluator<'v> {
    pub(crate) fn run(
        node: &Node,
        globals: &'v [(&'v str, &'v Value)],
        limits: &'v Limits,
    ) -> Result<Value, Error> {
        let mut evaluator = Evaluator {
            globals,
            limits,
            deadline: limits.time_limit.map(|limit| Instant::now() + limit),
            steps: 0,
        };
        let value = evaluator.eval(node)?.into_owned();
        // A result computed past the deadline is refused too; the budget is
        // not satisfied by finishing without noticing the clock.
        if evaluator.deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            return Err(budget("time limit exceeded"));
        }
        Ok(value)
    }

    fn tick(&mut self) -> Result<(), Error> {
        self.steps += 1;
        if let Some(max) = self.limits.max_steps
            && self.steps > max
        {
            return Err(budget(format!("evaluation step limit {max} exceeded")));
        }
        if self.deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            return Err(budget("time limit exceeded"));
        }
        Ok(())
    }

    fn global(&self, name: &str) -> Option<&'v Value> {
        self.globals.iter().find(|(n, _)| *n == name).map(|(_, v)| *v)
    }

    fn check_size(&self, value: &Value) -> Result<(), Error> {
        let limits = self.limits;
        if limits.max_string_len.is_none() && limits.max_list_len.is_none() && limits.max_map_len.is_none() {
            return Ok(());
        }
        let mut work = vec![value];
        while let Some(value) = work.pop() {
            match value {
                Value::Array(items) => {
                    if let Some(max) = limits.max_list_len
                        && items.len() > max
                    {
                        return Err(budget(format!("list length {} exceeds limit {max}", items.len())));
                    }
                    work.extend(items.iter());
                }
                Value::Object(map) => {
                    if let Some(max) = limits.max_map_len
                        && map.len() > max
                    {
                        return Err(budget(format!("map size {} exceeds limit {max}", map.len())));
                    }
                    work.extend(map.values());
                }
                Value::String(s) => {
                    if let Some(max) = limits.max_string_len
                        && s.len() > max
                    {
                        return Err(budget(format!("string length {} exceeds limit {max}", s.len())));
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn sized(&self, value: Value) -> Out<'v> {
        self.check_size(&value)?;
        Ok(Cow::Owned(value))
    }

    fn eval(&mut self, node: &Node) -> Out<'v> {
        self.tick()?;
        match node {
            Node::Number(n) => Ok(Cow::Owned(number(*n)?)),
            Node::Str(s) => Ok(Cow::Owned(Value::String(s.clone()))),
            Node::Bool(b) => Ok(Cow::Owned(Value::Bool(*b))),
            Node::Nil => Ok(Cow::Owned(Value::Null)),
            Node::Interp(parts) => self.interpolate(parts),
            Node::Var(name) => match self.global(name) {
                Some(value) => Ok(Cow::Borrowed(value)),
                // Positional names read as nil when unset, as in Mix.
                None if name.chars().all(|c| c.is_ascii_digit()) => Ok(Cow::Owned(Value::Null)),
                None => Err(runtime(format!("undefined variable '${name}'"))),
            },
            Node::Binary { op, left, right } => self.binary(*op, left, right),
            Node::Unary { op, operand } => {
                let value = self.eval(operand)?;
                match op {
                    UnaryOp::Neg => {
                        let n = to_number(&value)
                            .ok_or_else(|| runtime(format!("cannot negate {}", type_name(&value))))?;
                        Ok(Cow::Owned(number(-n)?))
                    }
                    UnaryOp::Not => Ok(Cow::Owned(Value::Bool(!is_truthy(&value)))),
                }
            }
            Node::Ternary { cond, then, otherwise } => {
                let cond = self.eval(cond)?;
                if is_truthy(&cond) {
                    self.eval(then)
                } else {
                    self.eval(otherwise)
                }
            }
            Node::If { branches, otherwise } => {
                for branch in branches {
                    let cond = self.eval(&branch.cond)?;
                    if is_truthy(&cond) {
                        return match &branch.body {
                            Some(body) => self.eval(body),
                            None => Ok(Cow::Owned(Value::Null)),
                        };
                    }
                }
                match otherwise {
                    Some(body) => self.eval(body),
                    None => Ok(Cow::Owned(Value::Null)),
                }
            }
            Node::Field { object, field } => {
                let object = self.eval(object)?;
                if !object.is_object() {
                    return Err(runtime(format!(
                        "cannot access field '{field}' on {}",
                        type_name(&object)
                    )));
                }
                Ok(project(object, |v| v.as_object().and_then(|m| map_get(m, field))))
            }
            Node::Index { object, index } => {
                let index = self.eval(index)?;
                let object = self.eval(object)?;
                self.index(object, &index)
            }
            Node::List(items) => {
                let mut values = Vec::with_capacity(items.len());
                for item in items {
                    values.push(self.eval(item)?.into_owned());
                }
                self.sized(Value::Array(values))
            }
            Node::Map(entries) => {
                let mut map = serde_json::Map::new();
                for (key, value) in entries {
                    let value = self.eval(value)?.into_owned();
                    map.insert(key.clone(), value);
                }
                self.sized(Value::Object(map))
            }
        }
    }

    fn index(&self, object: Cow<'v, Value>, index: &Value) -> Out<'v> {
        let access = match (&*object, index) {
            (Value::Array(_), Value::Number(n)) => Access::ListAt(n.as_f64().unwrap_or(0.0)),
            (Value::Object(_), Value::String(key)) => Access::MapKey(key.clone()),
            // Any other key reads as its text form: `$svc[80]` reads "80".
            (Value::Object(_), other) => Access::MapKey(rendered(other)),
            (Value::String(s), Value::Number(n)) => {
                let chars: Vec<char> = s.chars().collect();
                return Ok(Cow::Owned(match signed_index(n.as_f64().unwrap_or(0.0), chars.len()) {
                    Some(i) => Value::String(chars[i].to_string()),
                    None => Value::Null,
                }));
            }
            (object, index) => {
                return Err(runtime(format!(
                    "cannot index {} with {}",
                    type_name(object),
                    type_name(index)
                )));
            }
        };
        Ok(match access {
            Access::ListAt(n) => project(object, |v| {
                v.as_array().and_then(|items| signed_index(n, items.len()).and_then(|i| items.get(i)))
            }),
            Access::MapKey(key) => project(object, |v| v.as_object().and_then(|m| map_get(m, &key))),
        })
    }

    fn binary(&mut self, op: BinOp, left: &Node, right: &Node) -> Out<'v> {
        let left = self.eval(left)?;
        match op {
            BinOp::And => {
                if !is_truthy(&left) {
                    return Ok(left);
                }
                return self.eval(right);
            }
            BinOp::Or => {
                if is_truthy(&left) {
                    return Ok(left);
                }
                return self.eval(right);
            }
            BinOp::NilCoalesce => {
                if !left.is_null() {
                    return Ok(left);
                }
                return self.eval(right);
            }
            _ => {}
        }
        let right = self.eval(right)?;
        self.binop(op, &left, &right)
    }

    fn binop(&self, op: BinOp, left: &Value, right: &Value) -> Out<'v> {
        let value = match op {
            BinOp::Add => {
                if left.is_null() || right.is_null() {
                    return Err(runtime(format!(
                        "`+` is not defined for {} and {}; nil is not a number or string; guard the nil with `??` or use `..` to build text",
                        type_name(left),
                        type_name(right)
                    )));
                }
                if !is_scalar(left) || !is_scalar(right) {
                    return Err(runtime(format!(
                        "`+` is not defined for {} and {}; `+` takes numbers or strings; use `..` to build text",
                        type_name(left),
                        type_name(right)
                    )));
                }
                match (to_number(left), to_number(right)) {
                    (Some(l), Some(r)) => number(l + r)?,
                    _ => {
                        let mut text = rendered(left);
                        render(right, &mut text);
                        return self.sized(Value::String(text));
                    }
                }
            }
            BinOp::Sub => number(operand(left)? - operand(right)?)?,
            BinOp::Mul => number(operand(left)? * operand(right)?)?,
            BinOp::Pow => number(operand(left)?.powf(operand(right)?))?,
            BinOp::Div => {
                let r = operand(right)?;
                if r == 0.0 {
                    return Err(runtime("division by zero"));
                }
                number(operand(left)? / r)?
            }
            BinOp::Mod => {
                let r = operand(right)?;
                if r == 0.0 {
                    return Err(runtime("modulo by zero"));
                }
                number(operand(left)? % r)?
            }
            BinOp::Eq => Value::Bool(equals(left, right, "==")?),
            BinOp::Ne => Value::Bool(!equals(left, right, "!=")?),
            BinOp::Gt => Value::Bool(compare(left, right, |a, b| a > b, |o| o.is_gt())?),
            BinOp::Lt => Value::Bool(compare(left, right, |a, b| a < b, |o| o.is_lt())?),
            BinOp::Ge => Value::Bool(compare(left, right, |a, b| a >= b, |o| o.is_ge())?),
            BinOp::Le => Value::Bool(compare(left, right, |a, b| a <= b, |o| o.is_le())?),
            BinOp::StrEq => Value::Bool(rendered(left) == rendered(right)),
            BinOp::StrNe => Value::Bool(rendered(left) != rendered(right)),
            BinOp::Concat => {
                let mut text = rendered(left);
                render(right, &mut text);
                return self.sized(Value::String(text));
            }
            BinOp::And | BinOp::Or | BinOp::NilCoalesce => unreachable!("short-circuit operators are handled by binary()"),
        };
        Ok(Cow::Owned(value))
    }

    fn interpolate(&mut self, parts: &[Part]) -> Out<'v> {
        let mut text = String::new();
        for part in parts {
            match part {
                Part::Literal(s) => text.push_str(s),
                Part::Var(var) => {
                    let value = self.interp_value(var)?;
                    render(&value, &mut text);
                }
            }
        }
        self.sized(Value::String(text))
    }

    /// Resolve one `${...}` part. A missing head is an error unless a
    /// default is given; a field on a non-map is nil; an index on a
    /// non-indexable value is an error.
    fn interp_value(&mut self, var: &InterpVar) -> Out<'v> {
        let mut current: Cow<'v, Value> = match self.global(&var.head) {
            Some(value) => Cow::Borrowed(value),
            None => {
                return match &var.coalesce {
                    Some((_, payload)) => self.coalesce_default(payload.as_deref()),
                    None => Err(runtime(format!(
                        "undefined variable '${}' in interpolation (use ${{{} ?? default}} for a fallback)",
                        var.head, var.head
                    ))),
                };
            }
        };
        for segment in &var.segments {
            current = match segment {
                Segment::Field(name) => {
                    project(current, |v| v.as_object().and_then(|m| map_get(m, name)))
                }
                Segment::Index(node) => {
                    let index = self.eval(node)?;
                    let access = match (&*current, &*index) {
                        (Value::Array(_), Value::Number(n)) => Access::ListAt(n.as_f64().unwrap_or(0.0)),
                        (Value::Object(_), Value::String(key)) => Access::MapKey(key.clone()),
                        (object, index) => {
                            return Err(runtime(format!(
                                "cannot index {} with {} in interpolation",
                                type_name(object),
                                type_name(index)
                            )));
                        }
                    };
                    match access {
                        Access::ListAt(n) => project(current, |v| {
                            v.as_array()
                                .and_then(|items| signed_index(n, items.len()).and_then(|i| items.get(i)))
                        }),
                        // No `*` fallback here, as in Mix's interpolation.
                        Access::MapKey(key) => project(current, |v| v.as_object().and_then(|m| m.get(&key))),
                    }
                }
            };
        }
        let fire = match var.coalesce {
            Some((Coalesce::Nil, _)) => current.is_null(),
            Some((Coalesce::Falsy, _)) => !is_truthy(&current),
            None => false,
        };
        if fire {
            let payload = var.coalesce.as_ref().and_then(|(_, payload)| payload.as_deref());
            return self.coalesce_default(payload);
        }
        Ok(current)
    }

    fn coalesce_default(&mut self, payload: Option<&Node>) -> Out<'v> {
        match payload {
            Some(node) => self.eval(node),
            None => Ok(Cow::Owned(Value::String(String::new()))),
        }
    }
}

fn is_scalar(value: &Value) -> bool {
    !matches!(value, Value::Array(_) | Value::Object(_))
}

fn operand(value: &Value) -> Result<f64, Error> {
    to_number(value).ok_or_else(|| runtime(format!("cannot use '{}' as number", rendered(value))))
}

/// Ordering: numeric when both sides coerce to numbers, lexicographic by
/// codepoint when both are strings, otherwise an error naming the side
/// that is not a number.
fn compare(
    left: &Value,
    right: &Value,
    numeric: fn(f64, f64) -> bool,
    textual: fn(std::cmp::Ordering) -> bool,
) -> Result<bool, Error> {
    if let (Some(l), Some(r)) = (to_number(left), to_number(right)) {
        return Ok(numeric(l, r));
    }
    if let (Value::String(a), Value::String(b)) = (left, right) {
        return Ok(textual(a.cmp(b)));
    }
    let culprit = if to_number(left).is_none() { left } else { right };
    Err(runtime(format!("cannot compare '{}' as number", rendered(culprit))))
}
