//! 轻量 Velocity 模板渲染引擎。
//!
//! 实现 komf 通知模板所使用的 Velocity 语法子集：
//! - 变量引用：`$series.name`、`${series.metadata.summary}`、`$books.size()`
//! - 条件：`#if (expr)` / `#else` / `#end`（也接受 `#{else}` 等花括号写法）
//! - 循环：`#foreach ($book in $books)` / `#end`
//! - 表达式：`==` / `!=`、字符串/整数/布尔字面量、`&&` / `||`
use std::collections::HashMap;

/// 模板值。
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    Str(String),
    Int(i64),
    Bool(bool),
    List(Vec<Value>),
    Map(HashMap<String, Value>),
}

impl Value {
    pub fn get(&self, key: &str) -> Value {
        match self {
            Value::Map(map) => map.get(key).cloned().unwrap_or(Value::Null),
            Value::Null => Value::Null,
            other => {
                tracing::warn!("attempted to access field `{key}` on non-map value {other:?}");
                Value::Null
            }
        }
    }

    pub fn call_method(&self, method: &str) -> Value {
        match (self, method) {
            (Value::List(list), "size") => Value::Int(list.len() as i64),
            (Value::Str(s), "isEmpty") => Value::Bool(s.is_empty()),
            (Value::Str(s), "length") => Value::Int(s.chars().count() as i64),
            (Value::Null, _) => Value::Null,
            (other, method) => {
                tracing::warn!("unsupported method `{method}` on value {other:?}");
                Value::Null
            }
        }
    }

    pub fn truthy(&self) -> bool {
        match self {
            Value::Null => false,
            Value::Bool(b) => *b,
            Value::Int(i) => *i != 0,
            Value::Str(s) => !s.is_empty(),
            Value::List(l) => !l.is_empty(),
            Value::Map(m) => !m.is_empty(),
        }
    }

    pub fn display(&self) -> String {
        match self {
            Value::Null => String::new(),
            Value::Str(s) => s.clone(),
            Value::Int(i) => i.to_string(),
            Value::Bool(b) => b.to_string(),
            Value::List(l) => format!(
                "[{}]",
                l.iter().map(|v| v.display()).collect::<Vec<_>>().join(", ")
            ),
            Value::Map(m) => format!(
                "{{{}}}",
                m.iter()
                    .map(|(k, v)| format!("{k}={}", v.display()))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
enum PathPart {
    Prop(String),
    Method(String),
}

#[derive(Debug, Clone)]
enum Node {
    Text(String),
    Ref { path: Vec<PathPart> },
    If {
        cond: Cond,
        then_branch: Vec<Node>,
        else_branch: Option<Vec<Node>>,
    },
    Foreach {
        var: String,
        collection: Vec<PathPart>,
        body: Vec<Node>,
    },
}

#[derive(Debug, Clone)]
enum Cond {
    Value(Value),
    Compare {
        left: ValueExpr,
        op: CompareOp,
        right: ValueExpr,
    },
    And(Box<Cond>, Box<Cond>),
    Or(Box<Cond>, Box<Cond>),
}

#[derive(Debug, Clone)]
enum ValueExpr {
    Ref(Vec<PathPart>),
    Literal(Value),
}

#[derive(Debug, Clone, Copy)]
enum CompareOp {
    Eq,
    Ne,
}

/// 一段节点列表的终止方式。
#[derive(Debug, Clone, Copy, PartialEq)]
enum Terminator {
    Eof,
    Else,
    End,
}

#[derive(Debug, thiserror::Error, Clone, Copy)]
pub enum TemplateError {
    #[error("unexpected end of template")]
    UnexpectedEnd,
    #[error("expected closing parenthesis")]
    MissingParen,
    #[error("malformed reference")]
    BadReference,
    #[error("unexpected directive")]
    UnexpectedDirective,
}

#[derive(Debug, Clone)]
pub struct Template {
    nodes: Vec<Node>,
}

impl Template {
    pub fn parse(template: &str) -> Result<Self, TemplateError> {
        let mut parser = Parser {
            chars: template.chars().collect(),
            pos: 0,
        };
        let (nodes, terminator) = parser.parse_nodes()?;
        if terminator != Terminator::Eof {
            return Err(TemplateError::UnexpectedDirective);
        }
        Ok(Self { nodes })
    }

    pub fn render(&self, root: Value) -> String {
        let mut scope = Scope::new(root);
        let mut out = String::new();
        render_nodes(&self.nodes, &mut scope, &mut out);
        out
    }
}

/// 求值上下文：作用域栈（foreach 变量入栈）。
struct Scope {
    stack: Vec<HashMap<String, Value>>,
}

impl Scope {
    fn new(root: Value) -> Self {
        let mut base = HashMap::new();
        if let Value::Map(map) = root {
            base = map;
        }
        Self {
            stack: vec![base],
        }
    }

    fn resolve(&self, path: &[PathPart]) -> Value {
        let first = match path.first() {
            Some(PathPart::Prop(name)) => name,
            Some(PathPart::Method(_)) | None => return Value::Null,
        };
        let mut current = self
            .stack
            .iter()
            .rev()
            .find_map(|scope| scope.get(first).cloned())
            .unwrap_or(Value::Null);
        for part in &path[1..] {
            current = match part {
                PathPart::Prop(name) => current.get(name),
                PathPart::Method(method) => current.call_method(method),
            };
        }
        current
    }

    fn push(&mut self, map: HashMap<String, Value>) {
        self.stack.push(map);
    }

    fn pop(&mut self) {
        self.stack.pop();
    }
}

struct Parser {
    chars: Vec<char>,
    pos: usize,
}

impl Parser {
    fn parse_nodes(&mut self) -> Result<(Vec<Node>, Terminator), TemplateError> {
        let mut nodes = Vec::new();
        let mut text = String::new();

        loop {
            if self.pos >= self.chars.len() {
                break;
            }
            let c = self.chars[self.pos];
            if c == '$' {
                if !text.is_empty() {
                    nodes.push(Node::Text(std::mem::take(&mut text)));
                }
                nodes.push(Node::Ref {
                    path: self.parse_reference()?,
                });
            } else if c == '#' {
                if !text.is_empty() {
                    nodes.push(Node::Text(std::mem::take(&mut text)));
                }
                if self.pos + 1 < self.chars.len() && self.chars[self.pos + 1] == '#' {
                    // ## -> 字面量 `#`
                    text.push('#');
                    self.pos += 2;
                    continue;
                }
                let (directive, _) = self.parse_directive()?;
                match directive.as_str() {
                    "if" => {
                        let cond = self.parse_condition()?;
                        let (then_branch, term) = self.parse_nodes()?;
                        let else_branch = match term {
                            Terminator::Else => {
                                let (else_nodes, end_term) = self.parse_nodes()?;
                                debug_assert_eq!(end_term, Terminator::End);
                                Some(else_nodes)
                            }
                            _ => None,
                        };
                        nodes.push(Node::If {
                            cond,
                            then_branch,
                            else_branch,
                        });
                    }
                    "foreach" => {
                        let (var, collection) = self.parse_foreach_args()?;
                        let (body, _term) = self.parse_nodes()?;
                        nodes.push(Node::Foreach {
                            var,
                            collection,
                            body,
                        });
                    }
                    "else" => {
                        return Ok((nodes, Terminator::Else));
                    }
                    "end" => {
                        return Ok((nodes, Terminator::End));
                    }
                    _ => {
                        return Err(TemplateError::UnexpectedDirective);
                    }
                }
            } else {
                text.push(c);
                self.pos += 1;
            }
        }

        if !text.is_empty() {
            nodes.push(Node::Text(text));
        }
        Ok((nodes, Terminator::Eof))
    }

    fn parse_reference(&mut self) -> Result<Vec<PathPart>, TemplateError> {
        if self.pos >= self.chars.len() {
            return Err(TemplateError::BadReference);
        }
        self.pos += 1; // 跳过 `$`
        let mut parts = Vec::new();
        let braced = if self.pos < self.chars.len() && self.chars[self.pos] == '{' {
            self.pos += 1;
            true
        } else {
            false
        };
        let name = self.read_ident();
        if name.is_empty() {
            return Err(TemplateError::BadReference);
        }
        parts.push(PathPart::Prop(name));
        loop {
            if self.pos >= self.chars.len() {
                if braced {
                    return Err(TemplateError::BadReference);
                }
                break;
            }
            let c = self.chars[self.pos];
            if braced {
                if c == '}' {
                    self.pos += 1;
                    break;
                }
                if c == '.' {
                    self.pos += 1;
                    let next = self.read_ident();
                    if next.is_empty() {
                        return Err(TemplateError::BadReference);
                    }
                    if self.pos < self.chars.len() && self.chars[self.pos] == '(' {
                        self.pos += 1;
                        self.skip_to_paren_close()?;
                        parts.push(PathPart::Method(next));
                    } else {
                        parts.push(PathPart::Prop(next));
                    }
                } else {
                    return Err(TemplateError::BadReference);
                }
            } else {
                if c == '.' {
                    self.pos += 1;
                    let next = self.read_ident();
                    if next.is_empty() {
                        return Err(TemplateError::BadReference);
                    }
                    if self.pos < self.chars.len() && self.chars[self.pos] == '(' {
                        self.pos += 1;
                        self.skip_to_paren_close()?;
                        parts.push(PathPart::Method(next));
                    } else {
                        parts.push(PathPart::Prop(next));
                    }
                } else if c == '(' {
                    self.pos += 1;
                    self.skip_to_paren_close()?;
                    if let Some(PathPart::Prop(name)) = parts.last() {
                        let name = name.clone();
                        parts.pop();
                        parts.push(PathPart::Method(name));
                    }
                } else {
                    break;
                }
            }
        }
        Ok(parts)
    }

    fn skip_to_paren_close(&mut self) -> Result<(), TemplateError> {
        while self.pos < self.chars.len() && self.chars[self.pos] != ')' {
            self.pos += 1;
        }
        if self.pos >= self.chars.len() {
            return Err(TemplateError::MissingParen);
        }
        self.pos += 1;
        Ok(())
    }

    fn read_ident(&mut self) -> String {
        let start = self.pos;
        while self.pos < self.chars.len() {
            let c = self.chars[self.pos];
            if c.is_alphanumeric() || c == '_' {
                self.pos += 1;
            } else {
                break;
            }
        }
        self.chars[start..self.pos].iter().collect()
    }

    /// 解析 `#if` / `#foreach` 等指令名（支持 `#{...}` 花括号写法）。
    fn parse_directive(&mut self) -> Result<(String, usize), TemplateError> {
        self.pos += 1; // 跳过 `#`
        if self.pos < self.chars.len() && self.chars[self.pos] == '{' {
            self.pos += 1;
            let name = self.read_ident();
            if self.pos >= self.chars.len() || self.chars[self.pos] != '}' {
                return Err(TemplateError::UnexpectedDirective);
            }
            self.pos += 1;
            return Ok((name, self.pos));
        }
        let name = self.read_ident();
        Ok((name, self.pos))
    }

    fn parse_condition(&mut self) -> Result<Cond, TemplateError> {
        self.skip_ws();
        if self.pos >= self.chars.len() || self.chars[self.pos] != '(' {
            return Err(TemplateError::MissingParen);
        }
        self.pos += 1;
        let cond = self.parse_cond_expr()?;
        self.skip_ws();
        if self.pos >= self.chars.len() || self.chars[self.pos] != ')' {
            return Err(TemplateError::MissingParen);
        }
        self.pos += 1;
        Ok(cond)
    }

    fn parse_cond_expr(&mut self) -> Result<Cond, TemplateError> {
        let mut left = self.parse_compare()?;
        loop {
            self.skip_ws();
            if self.starts_with("&&") {
                self.pos += 2;
                let right = self.parse_compare()?;
                left = Cond::And(Box::new(left), Box::new(right));
            } else if self.starts_with("||") {
                self.pos += 2;
                let right = self.parse_compare()?;
                left = Cond::Or(Box::new(left), Box::new(right));
            } else {
                break;
            }
        }
        Ok(left)
    }

    fn parse_compare(&mut self) -> Result<Cond, TemplateError> {
        self.skip_ws();
        let left = self.parse_value_expr()?;
        self.skip_ws();
        if self.starts_with("==") {
            self.pos += 2;
            self.skip_ws();
            let right = self.parse_value_expr()?;
            Ok(Cond::Compare {
                left,
                op: CompareOp::Eq,
                right,
            })
        } else if self.starts_with("!=") {
            self.pos += 2;
            self.skip_ws();
            let right = self.parse_value_expr()?;
            Ok(Cond::Compare {
                left,
                op: CompareOp::Ne,
                right,
            })
        } else {
            match left {
                ValueExpr::Literal(v) => Ok(Cond::Value(v)),
                ValueExpr::Ref(path) => Ok(Cond::Compare {
                    left: ValueExpr::Ref(path),
                    op: CompareOp::Ne,
                    right: ValueExpr::Literal(Value::Null),
                }),
            }
        }
    }

    fn parse_value_expr(&mut self) -> Result<ValueExpr, TemplateError> {
        self.skip_ws();
        if self.pos >= self.chars.len() {
            return Err(TemplateError::UnexpectedEnd);
        }
        let c = self.chars[self.pos];
        match c {
            '"' | '\'' => {
                let quote = c;
                self.pos += 1;
                let mut s = String::new();
                while self.pos < self.chars.len() && self.chars[self.pos] != quote {
                    s.push(self.chars[self.pos]);
                    self.pos += 1;
                }
                if self.pos >= self.chars.len() {
                    return Err(TemplateError::UnexpectedEnd);
                }
                self.pos += 1;
                Ok(ValueExpr::Literal(Value::Str(s)))
            }
            '$' => {
                let path = self.parse_reference()?;
                Ok(ValueExpr::Ref(path))
            }
            _ if c.is_numeric() || c == '-' => {
                let start = self.pos;
                while self.pos < self.chars.len()
                    && (self.chars[self.pos].is_numeric() || self.chars[self.pos] == '-')
                {
                    self.pos += 1;
                }
                let text: String = self.chars[start..self.pos].iter().collect();
                let value = text.parse::<i64>().map(Value::Int).unwrap_or(Value::Null);
                Ok(ValueExpr::Literal(value))
            }
            _ if c.is_alphabetic() => {
                let name = self.read_ident();
                match name.as_str() {
                    "true" => Ok(ValueExpr::Literal(Value::Bool(true))),
                    "false" => Ok(ValueExpr::Literal(Value::Bool(false))),
                    _ => Ok(ValueExpr::Literal(Value::Null)),
                }
            }
            _ => Err(TemplateError::UnexpectedEnd),
        }
    }

    fn parse_foreach_args(&mut self) -> Result<(String, Vec<PathPart>), TemplateError> {
        self.skip_ws();
        if self.pos >= self.chars.len() || self.chars[self.pos] != '(' {
            return Err(TemplateError::MissingParen);
        }
        self.pos += 1;
        self.skip_ws();
        if self.pos >= self.chars.len() || self.chars[self.pos] != '$' {
            return Err(TemplateError::BadReference);
        }
        let var_path = self.parse_reference()?;
        let var = match var_path.first() {
            Some(PathPart::Prop(name)) => name.clone(),
            _ => return Err(TemplateError::BadReference),
        };
        self.skip_ws();
        if self.starts_with("in") {
            self.pos += 2;
        } else {
            return Err(TemplateError::BadReference);
        }
        self.skip_ws();
        let collection = self.parse_reference()?;
        self.skip_ws();
        if self.pos >= self.chars.len() || self.chars[self.pos] != ')' {
            return Err(TemplateError::MissingParen);
        }
        self.pos += 1;
        Ok((var, collection))
    }

    fn skip_ws(&mut self) {
        while self.pos < self.chars.len() && self.chars[self.pos].is_whitespace() {
            self.pos += 1;
        }
    }

    fn starts_with(&self, s: &str) -> bool {
        let chars: Vec<char> = s.chars().collect();
        if self.pos + chars.len() > self.chars.len() {
            return false;
        }
        for (i, c) in chars.iter().enumerate() {
            if self.chars[self.pos + i] != *c {
                return false;
            }
        }
        true
    }
}

fn render_nodes(nodes: &[Node], scope: &mut Scope, out: &mut String) {
    for node in nodes {
        match node {
            Node::Text(text) => out.push_str(text),
            Node::Ref { path } => {
                let value = scope.resolve(path);
                out.push_str(&value.display());
            }
            Node::If {
                cond,
                then_branch,
                else_branch,
            } => {
                if eval_cond(cond, scope) {
                    render_nodes(then_branch, scope, out);
                } else if let Some(else_branch) = else_branch {
                    render_nodes(else_branch, scope, out);
                }
            }
            Node::Foreach {
                var,
                collection,
                body,
            } => {
                let value = scope.resolve(collection);
                if let Value::List(items) = value {
                    for item in items {
                        let mut map = HashMap::new();
                        map.insert(var.clone(), item);
                        scope.push(map);
                        render_nodes(body, scope, out);
                        scope.pop();
                    }
                }
            }
        }
    }
}

fn eval_cond(cond: &Cond, scope: &Scope) -> bool {
    match cond {
        Cond::Value(v) => v.truthy(),
        Cond::Compare { left, op, right } => {
            let lv = eval_value_expr(left, scope);
            let rv = eval_value_expr(right, scope);
            let equal = values_equal(&lv, &rv);
            match op {
                CompareOp::Eq => equal,
                CompareOp::Ne => !equal,
            }
        }
        Cond::And(a, b) => eval_cond(a, scope) && eval_cond(b, scope),
        Cond::Or(a, b) => eval_cond(a, scope) || eval_cond(b, scope),
    }
}

fn eval_value_expr(expr: &ValueExpr, scope: &Scope) -> Value {
    match expr {
        ValueExpr::Ref(path) => scope.resolve(path),
        ValueExpr::Literal(v) => v.clone(),
    }
}

fn values_equal(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Null, Value::Null) => true,
        (Value::Null, _) => matches!(b, Value::Str(s) if s.is_empty()),
        (_, Value::Null) => matches!(a, Value::Str(s) if s.is_empty()),
        (Value::Int(x), Value::Int(y)) => x == y,
        (Value::Str(x), Value::Str(y)) => x == y,
        (Value::Bool(x), Value::Bool(y)) => x == y,
        (Value::Int(x), Value::Str(y)) => y.parse::<i64>().map(|v| v == *x).unwrap_or(false),
        (Value::Str(x), Value::Int(y)) => x.parse::<i64>().map(|v| v == *y).unwrap_or(false),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> Value {
        let mut series = HashMap::new();
        series.insert("name".to_string(), Value::Str("My Series".to_string()));
        let mut metadata = HashMap::new();
        metadata.insert("summary".to_string(), Value::Str("A summary".to_string()));
        series.insert("metadata".to_string(), Value::Map(metadata));

        let mut root = HashMap::new();
        root.insert("series".to_string(), Value::Map(series));
        root.insert(
            "books".to_string(),
            Value::List(vec![
                Value::Map(HashMap::from([("name".to_string(), Value::Str("Vol. 1".to_string()))])),
                Value::Map(HashMap::from([("name".to_string(), Value::Str("Vol. 2".to_string()))])),
            ]),
        );
        Value::Map(root)
    }

    #[test]
    fn renders_simple_reference() {
        let tpl = Template::parse("$series.name").unwrap();
        assert_eq!(tpl.render(ctx()), "My Series");
    }

    #[test]
    fn renders_braced_reference() {
        let tpl = Template::parse("${series.name}").unwrap();
        assert_eq!(tpl.render(ctx()), "My Series");
    }

    #[test]
    fn renders_if_else() {
        let tpl = Template::parse("#if(${books.size()} == 1)one#{else}many#end").unwrap();
        assert_eq!(tpl.render(ctx()), "many");

        let mut single = HashMap::new();
        single.insert("books".to_string(), Value::List(vec![Value::Str("x".to_string())]));
        let tpl = Template::parse("#if(${books.size()} == 1)one#{else}many#end").unwrap();
        assert_eq!(tpl.render(Value::Map(single)), "one");
    }

    #[test]
    fn renders_foreach() {
        let tpl = Template::parse("#foreach ($book in $books)**${book.name}**#end").unwrap();
        assert_eq!(tpl.render(ctx()), "**Vol. 1****Vol. 2**");
    }

    #[test]
    fn renders_string_compare() {
        let tpl = Template::parse("#if (${series.metadata.summary} != \"\")has#end").unwrap();
        assert_eq!(tpl.render(ctx()), "has");
    }

    #[test]
    fn parses_full_description_template() {
        let tpl = Template::parse(
            r#"#if (${series.metadata.summary} != "")
${series.metadata.summary}
#end
***new #if(${books.size()} == 1)book was #{else}books were #{end}added to library ${library.name}:***
#foreach ($book in $books)
**${book.name}**
#end
"#,
        )
        .unwrap();
        let out = tpl.render(ctx());
        assert!(out.contains("A summary"));
        assert!(out.contains("books were added to library"));
        assert!(out.contains("**Vol. 1**"));
        assert!(out.contains("**Vol. 2**"));
    }
}
