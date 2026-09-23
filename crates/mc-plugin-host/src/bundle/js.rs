//! surface 入口脚本的**词法**扫描与「模块专用语法」探测。
//!
//! 上游 `plugincontract/bundle.go` 用 `tdewolff/parse/v2/js` 做**真解析**（`parseSurfaceScript`
//! 建 AST + `surfaceModuleSyntaxVisitor` 遍历）。本 crate 不引三方依赖（`Cargo.toml` 冻结），
//! 因此这里是**手写替身**：单趟词法扫描 + 结构计数，产出同一个问题的答案 ——
//! 「这段脚本里有 classic script 用不了的模块语法吗」。
//!
//! # 判据（与上游 visitor 逐条对应）
//!
//! | 上游 | 本实现 |
//! | --- | --- |
//! | `*js.ImportStmt` | 顶层（`{}` 之外）的 `import` 声明；`import(` 是**动态导入**，classic script 合法，放过 |
//!
//! ⚠️ 上表的 `import` 行有一个上游注释（`bundle.go:264`）专门点名的坑：`import` 与 `(`/`.`
//! 之间**可以夹注释与换行**（`import /* c */ ("x")` 是动态导入、`import /* c */ .meta` 是
//! `import.meta`），所以这里的判定先跳过**空白与注释**再看下一个有意义的字节
//! （[`Scanner::next_significant`]）。`import\s*\(` 这类正则，以及「按行扫」的正则，
//! 会被这两件事骗过 —— 见 `tests::comment_between_import_and_paren_is_still_dynamic_import`
//! 与 `tests::two_statements_on_one_line_do_not_hide_module_syntax`。
//! | `*js.ExportStmt` | 顶层的 `export` |
//! | `*js.ImportMetaExpr` | `import.meta`（**任意**深度，上游也是任意深度） |
//! | 顶层 `await`（`AwaitToken` 且 `functionDepth == 0`） | `await` 出现在「不在函数体里、且当前语句还没出现过 `function`/`=>`」的地方 |
//!
//! # 已知差异（`docs/32` §9 已登记）
//!
//! 1. **不是解析器**：只抓**词法**错误（未闭合的字符串/模板/注释、括号不平衡）与上面的模块语法。
//!    真正的语法错误（如 `a b c;`）在浏览器里才炸。上游会当场拒；本仓**放过**。
//! 2. **保守兜底**：`/` 在「看起来能起正则」的位置才按正则扫描；**扫描失败就退回除号**
//!    （见 `Scanner::slash`）。这条兜底保证「把除号误判成正则」永远不会误拒一个合法脚本 ——
//!    代价是未闭合的正则不被发现（同上，交给浏览器）。
//! 3. **函数体识别**是启发式：`function` 后的第一个顶层 `{}`、以及 `=>` 后的 `{}` 记作函数体。
//!    类方法体（`foo() {}`）**不**算函数体 —— 这一点与上游 visitor 只数 `FuncDecl`/`ArrowFunc`
//!    的行为一致（上游也会把类方法里的 `await` 判成模块语法）。
//! 4. 标识符按字节判：非 ASCII 字节一律当标识符成分（不引入 Unicode 转义表），因此
//!    `await`/`import` 这类 ASCII 关键字之外的标识符不会误报。

use std::fmt;

/// 词法/结构层面的拒绝理由（上游是 `js.Parse` 返回的语法错误）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JsError {
    /// 字符串字面量未闭合（含裸换行）。
    UnterminatedString,
    /// 模板字面量未闭合（含 `${` 之后一直没等到 `}`）。
    UnterminatedTemplate,
    /// 块注释未闭合。
    UnterminatedComment,
    /// 多出来的闭括号。
    UnexpectedCloser(char),
    /// 一直没等到闭括号。
    UnclosedDelimiter(char),
}

impl fmt::Display for JsError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnterminatedString => formatter.write_str("unterminated string literal"),
            Self::UnterminatedTemplate => formatter.write_str("unterminated template literal"),
            Self::UnterminatedComment => formatter.write_str("unterminated block comment"),
            Self::UnexpectedCloser(closer) => write!(formatter, "unexpected {closer:?}"),
            Self::UnclosedDelimiter(opener) => write!(formatter, "unclosed {opener:?}"),
        }
    }
}

/// 扫描 surface 入口脚本。
///
/// 返回 `Ok(true)` 表示出现了 classic script 用不了的模块专用语法（上游 `moduleOnly`）；
/// `Err` 表示词法/结构上就不成立。
///
/// # Errors
///
/// 见 [`JsError`]。
pub fn scan_module_syntax(source: &str) -> Result<bool, JsError> {
    Scanner::new(source).run()
}

/// `{` 打开的这一帧是什么。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Frame {
    /// 普通块 / 对象字面量 / 模板替换。
    Block,
    /// 函数体（`function` 或 `=>` 后面那个）。
    FunctionBody,
}

/// 单趟扫描器。字段即状态机。
// 状态确实由这几个 bool 组成（「待定的箭头体」「当前语句出现过 function」…），拆成结构体只会
// 让热路径多一层解引用。
#[allow(clippy::struct_excessive_bools)]
struct Scanner<'a> {
    bytes: &'a [u8],
    pos: usize,
    /// 每遇到一个 `{`（含 `${`）压一帧，长度就是 `{}` 深度。
    frames: Vec<Frame>,
    /// 每个 `${` 记下它**进入前**的 `{}` 深度，用来认出「这个 `}` 是回模板文本」。
    templates: Vec<usize>,
    paren: usize,
    bracket: usize,
    /// 当前处于函数体里的层数。
    fn_depth: usize,
    /// 刚看到 `=>`，下一个 `{`/`(` 归它。
    pending_arrow: bool,
    /// 刚看到 `function`，下一个顶层 `{` 是它的函数体。
    pending_function: bool,
    /// 当前语句里已经出现过 `function`/`=>`（`await` 判据要用）。
    statement_function: bool,
    /// 上一个 token 是 `.`/`?.` ⇒ 后面的标识符是属性名，不是关键字。
    prev_was_dot: bool,
    /// 当前位置允许正则字面量。
    regex_allowed: bool,
    /// 当前在模板**文本**态（不是在 `{}` 里）。
    template_text: bool,
    module_only: bool,
}

impl<'a> Scanner<'a> {
    const fn new(source: &'a str) -> Self {
        Self {
            bytes: source.as_bytes(),
            pos: 0,
            frames: Vec::new(),
            templates: Vec::new(),
            paren: 0,
            bracket: 0,
            fn_depth: 0,
            pending_arrow: false,
            pending_function: false,
            statement_function: false,
            prev_was_dot: false,
            regex_allowed: true,
            template_text: false,
            module_only: false,
        }
    }

    fn run(mut self) -> Result<bool, JsError> {
        while self.pos < self.bytes.len() {
            if self.template_text {
                self.template_chunk();
                continue;
            }
            let byte = self.bytes[self.pos];
            // `=>` 的函数体可能是 `{`，也可能是 `(` 包起来的表达式；两者都不清掉待定标记。
            let arrow_body = self.pending_arrow;
            if byte != b'{' && byte != b'(' {
                self.pending_arrow = false;
            }
            let was_dot = self.prev_was_dot;
            self.prev_was_dot = false;
            match byte {
                b' ' | b'\t' | b'\r' | b'\n' | 0x0b | 0x0c => self.pos += 1,
                b'/' if self.peek(1) == Some(b'/') => self.skip_line_comment(),
                b'/' if self.peek(1) == Some(b'*') => self.skip_block_comment()?,
                b'/' => self.slash(),
                b'\'' | b'"' => {
                    self.skip_string(byte)?;
                    self.value_token();
                }
                b'`' => {
                    self.pos += 1;
                    self.template_text = true;
                }
                b'0'..=b'9' => {
                    self.skip_number();
                    self.value_token();
                }
                _ if is_ident_start(byte) => self.identifier(was_dot),
                _ => self.punctuator(arrow_body)?,
            }
        }
        self.finish()
    }

    fn finish(&self) -> Result<bool, JsError> {
        if self.template_text || !self.templates.is_empty() {
            return Err(JsError::UnterminatedTemplate);
        }
        if !self.frames.is_empty() {
            return Err(JsError::UnclosedDelimiter('{'));
        }
        if self.paren > 0 {
            return Err(JsError::UnclosedDelimiter('('));
        }
        if self.bracket > 0 {
            return Err(JsError::UnclosedDelimiter('['));
        }
        Ok(self.module_only)
    }

    fn peek(&self, offset: usize) -> Option<u8> {
        self.bytes.get(self.pos + offset).copied()
    }

    /// 模板文本态走一步：反引号收尾、`${` 进替换、`\` 转义，其余是普通字符。
    fn template_chunk(&mut self) {
        match self.bytes[self.pos] {
            b'`' => {
                self.pos += 1;
                self.template_text = false;
                self.regex_allowed = false;
            }
            b'\\' => self.pos = (self.pos + 2).min(self.bytes.len()),
            b'$' if self.peek(1) == Some(b'{') => {
                self.templates.push(self.frames.len());
                self.frames.push(Frame::Block);
                self.pos += 2;
                self.template_text = false;
                self.regex_allowed = true;
            }
            _ => self.pos += 1,
        }
    }

    fn skip_line_comment(&mut self) {
        while let Some(byte) = self.bytes.get(self.pos) {
            if *byte == b'\n' {
                break;
            }
            self.pos += 1;
        }
    }

    fn skip_block_comment(&mut self) -> Result<(), JsError> {
        self.pos += 2;
        loop {
            let Some(byte) = self.bytes.get(self.pos) else {
                return Err(JsError::UnterminatedComment);
            };
            if *byte == b'*' && self.peek(1) == Some(b'/') {
                self.pos += 2;
                return Ok(());
            }
            self.pos += 1;
        }
    }

    fn skip_string(&mut self, quote: u8) -> Result<(), JsError> {
        self.pos += 1;
        loop {
            let Some(byte) = self.bytes.get(self.pos) else {
                return Err(JsError::UnterminatedString);
            };
            if *byte == quote {
                self.pos += 1;
                return Ok(());
            }
            match *byte {
                b'\\' => self.pos += 2,
                b'\n' | b'\r' => return Err(JsError::UnterminatedString),
                _ => self.pos += 1,
            }
        }
    }

    fn skip_number(&mut self) {
        while let Some(byte) = self.bytes.get(self.pos) {
            // `1e+5` / `1e-5`：指数里的符号也属于数字。
            let exponent_sign = matches!(byte, b'+' | b'-')
                && self.pos > 0
                && matches!(self.bytes[self.pos - 1], b'e' | b'E');
            if byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_') || exponent_sign {
                self.pos += 1;
            } else {
                break;
            }
        }
    }

    /// `/`：能当正则就当正则，扫描失败**退回除号**（见文件头注 2）。
    fn slash(&mut self) {
        if self.regex_allowed && self.try_skip_regex() {
            self.value_token();
            return;
        }
        self.pos += 1;
        self.regex_allowed = true;
    }

    fn try_skip_regex(&mut self) -> bool {
        let mut index = self.pos + 1;
        let mut in_class = false;
        loop {
            let Some(byte) = self.bytes.get(index).copied() else {
                return false;
            };
            match byte {
                b'\n' | b'\r' => return false,
                b'\\' => index += 2,
                b'[' if !in_class => {
                    in_class = true;
                    index += 1;
                }
                b']' if in_class => {
                    in_class = false;
                    index += 1;
                }
                b'/' if !in_class => {
                    index += 1;
                    break;
                }
                _ => index += 1,
            }
        }
        while self.bytes.get(index).is_some_and(u8::is_ascii_alphabetic) {
            index += 1;
        }
        self.pos = index;
        true
    }

    /// 从 `from` 起跳过空白与注释，返回下一个**有意义**的字节（不做任何状态变更）。
    ///
    /// 只做前看，不移动 `self.pos` —— 真正的消费仍由主循环的 `skip_*` 负责（那些函数才管
    /// 未闭合注释的报错）。因此这里遇到未闭合的块注释返回 `None`，错误由主循环照常抛出。
    fn next_significant(&self, from: usize) -> Option<u8> {
        let mut index = from;
        loop {
            match self.bytes.get(index).copied()? {
                b' ' | b'\t' | b'\r' | b'\n' | 0x0b | 0x0c => index += 1,
                b'/' if self.bytes.get(index + 1) == Some(&b'/') => {
                    index += 2;
                    while !matches!(self.bytes.get(index), None | Some(b'\n')) {
                        index += 1;
                    }
                }
                b'/' if self.bytes.get(index + 1) == Some(&b'*') => {
                    index += 2;
                    loop {
                        match self.bytes.get(index) {
                            None => return None,
                            Some(b'*') if self.bytes.get(index + 1) == Some(&b'/') => {
                                index += 2;
                                break;
                            }
                            Some(_) => index += 1,
                        }
                    }
                }
                byte => return Some(byte),
            }
        }
    }

    fn identifier(&mut self, was_dot: bool) {
        let start = self.pos;
        while self
            .bytes
            .get(self.pos)
            .is_some_and(|byte| is_ident_part(*byte))
        {
            self.pos += 1;
        }
        let text = std::str::from_utf8(&self.bytes[start..self.pos]).unwrap_or("");
        if was_dot {
            // 属性名：`a.import` / `a.await` 不是关键字。
            self.regex_allowed = false;
            return;
        }
        match text {
            "function" => {
                self.pending_function = true;
                self.statement_function = true;
                self.regex_allowed = true;
            }
            "import" => {
                self.regex_allowed = true;
                // `import.meta` 任意深度都是模块专用（上游 `ImportMetaExpr`）；
                // 顶层 `import x from "…"` 是模块声明；`import(` 是动态导入，合法。
                //
                // 判定前先跳过空白与注释：`import` 与 `(`/`.` 之间夹注释或换行在 JS 里是
                // **同一个 token 序列**（注释只是空白），上游拿 AST 看的就是这个结论。
                let module_form = match self.next_significant(self.pos) {
                    Some(b'.') => true,
                    Some(b'(') => false,
                    _ => self.frames.is_empty(),
                };
                if module_form {
                    self.module_only = true;
                }
            }
            "export" => {
                self.regex_allowed = true;
                if self.frames.is_empty() {
                    self.module_only = true;
                }
            }
            "await" => {
                if self.fn_depth == 0 && !self.statement_function {
                    self.module_only = true;
                }
                self.regex_allowed = true;
            }
            "return" | "typeof" | "instanceof" | "in" | "of" | "new" | "delete" | "void"
            | "throw" | "case" | "do" | "else" | "yield" | "default" | "extends" => {
                self.regex_allowed = true;
            }
            _ => self.regex_allowed = false,
        }
    }

    /// 值（字符串/数字/正则/模板）之后不可能再起正则。
    fn value_token(&mut self) {
        self.regex_allowed = false;
    }

    fn punctuator(&mut self, arrow_body: bool) -> Result<(), JsError> {
        match self.bytes[self.pos] {
            b'{' => {
                self.pos += 1;
                // `function` 的函数体必须是**顶层** `{}`（默认参数里的对象字面量在括号里）；
                // `=>` 的函数体不受括号深度限制（`.then(() => { … })` 很常见）。
                let is_function_body = arrow_body || (self.pending_function && self.paren == 0);
                if is_function_body {
                    self.pending_function = false;
                    self.frames.push(Frame::FunctionBody);
                    self.fn_depth += 1;
                } else {
                    self.frames.push(Frame::Block);
                }
                self.regex_allowed = true;
            }
            b'}' => {
                self.pos += 1;
                let Some(frame) = self.frames.pop() else {
                    return Err(JsError::UnexpectedCloser('}'));
                };
                if frame == Frame::FunctionBody {
                    self.fn_depth -= 1;
                }
                if self.templates.last() == Some(&self.frames.len()) {
                    self.templates.pop();
                    // 回到模板文本态。
                    self.template_text = true;
                }
                self.statement_function = false;
                self.regex_allowed = false;
            }
            b'(' => {
                self.pos += 1;
                self.paren += 1;
                self.regex_allowed = true;
            }
            b')' => {
                self.pos += 1;
                if self.paren == 0 {
                    return Err(JsError::UnexpectedCloser(')'));
                }
                self.paren -= 1;
                self.regex_allowed = false;
            }
            b'[' => {
                self.pos += 1;
                self.bracket += 1;
                self.regex_allowed = true;
            }
            b']' => {
                self.pos += 1;
                if self.bracket == 0 {
                    return Err(JsError::UnexpectedCloser(']'));
                }
                self.bracket -= 1;
                self.regex_allowed = false;
            }
            b';' => {
                self.pos += 1;
                self.statement_function = false;
                self.pending_function = false;
                self.regex_allowed = true;
            }
            b'.' => {
                self.pos += 1;
                self.prev_was_dot = true;
                self.regex_allowed = false;
            }
            b'?' if self.peek(1) == Some(b'.') => {
                self.pos += 2;
                self.prev_was_dot = true;
                self.regex_allowed = false;
            }
            b'=' if self.peek(1) == Some(b'>') => {
                self.pos += 2;
                self.pending_arrow = true;
                self.statement_function = true;
                self.regex_allowed = true;
            }
            b'+' | b'-' if self.peek(1) == Some(self.bytes[self.pos]) => {
                // `++`/`--`：值后跟的运算符，后面是除号而不是正则。
                self.pos += 2;
                self.regex_allowed = false;
            }
            _ => {
                // 其余运算符/标点：后面可以起正则（`= /re/`、`, /re/`、`: /re/` …）。
                self.pos += 1;
                self.regex_allowed = true;
            }
        }
        Ok(())
    }
}

/// 标识符首字节：ASCII 字母、`_`、`$`，以及一切非 ASCII 字节（保守当标识符）。
const fn is_ident_start(byte: u8) -> bool {
    byte.is_ascii_alphabetic() || byte == b'_' || byte == b'$' || byte >= 0x80
}

/// 标识符后续字节。
const fn is_ident_part(byte: u8) -> bool {
    is_ident_start(byte) || byte.is_ascii_digit()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn module_only(source: &str) -> bool {
        scan_module_syntax(source).expect("脚本应当能扫过")
    }

    #[test]
    fn plain_classic_script_passes() {
        let source = r#"
            const panel = document.getElementById("root");
            async function load() { return await fetch("/v1/context"); }
            const doubled = [1, 2, 3].map((n) => n * 2);
            panel.textContent = String(doubled.length / 2);
            console.log(/a[/]b/.test("a/b"), "template ${`inner ${1 + 1}`} done");
            // import 注释里的 import x from "y" 不算
            /* export default 1 也不算 */
        "#;
        assert!(!module_only(source));
    }

    #[test]
    fn property_names_are_not_keywords() {
        assert!(!module_only("api.import(cb); api.export = 1; api.await;"));
        assert!(!module_only("const { import: read } = api; read();"));
        assert!(!module_only("import(\"./chunk.js\").then((m) => m.run());"));
    }

    /// 上游 `bundle.go:264` 点名的两个「朴素正则会被骗过」的用例之一。
    ///
    /// 注释（与换行）可以夹在 `import` 和 `(` 之间：那是**动态导入**，classic script 合法，
    /// 必须放过；而 `import\s*\(` 这类正则看不见注释，会把合法 surface 判死（上游的原则是
    /// 「误拒一个作者没有绕法」）。反方向也不能松：注释后面跟的是 `{` 时仍是模块声明，
    /// `import.meta` 同理（且**任意深度**都是模块专用）。
    #[test]
    fn comment_between_import_and_paren_is_still_dynamic_import() {
        assert!(!module_only(
            "import /* inlined */ (\"./chunk.js\").then((m) => m.run());"
        ));
        assert!(!module_only("import\n// 换行也合法\n(\"./chunk.js\");"));
        assert!(module_only(
            "import /* c */ { open } from \"@multica/plugin-sdk\";"
        ));
        assert!(module_only("const u = import /* c */ .meta.url;"));
        assert!(module_only(
            "function f() { return import /* c */ .meta.url; }"
        ));
    }

    /// 上游 `bundle.go:264` 点名的另一个用例：**一行两条语句**。
    ///
    /// 「按行扫」的正则只看这一行有没有 `function`/`async`，于是会漏掉同一行第二条语句里
    /// 的顶层 `await`（或 `;` 之后的静态 `import`）。这里要求按**语句**判定，同时不能把
    /// 同一行两条都合法的语句误判成模块语法。
    #[test]
    fn two_statements_on_one_line_do_not_hide_module_syntax() {
        assert!(module_only("function load() {} await boot();"));
        assert!(module_only(
            "const marker = 1; import { open } from \"@multica/plugin-sdk\";"
        ));
        assert!(!module_only(
            "const a = 1; const b = async () => await load();"
        ));
    }

    #[test]
    fn catches_import_export_and_import_meta() {
        assert!(module_only("import { open } from \"@multica/plugin-sdk\";"));
        assert!(module_only("import \"./side-effect.js\";"));
        assert!(module_only("export default function panel() {}"));
        assert!(module_only("export { run };"));
        assert!(module_only("function f() { return import.meta.url; }"));
    }

    #[test]
    fn catches_only_top_level_await() {
        assert!(module_only("const context = await fetch(\"/v1/context\");"));
        assert!(module_only("if (ready) { doIt(); }\nawait boot();"));
        assert!(!module_only(
            "async function boot() { await fetch(\"/v1/context\"); }"
        ));
        assert!(!module_only("boot().then(async () => { await tick(); });"));
        assert!(!module_only(
            "const boot = async () => await fetch(\"/v1/context\");"
        ));
        assert!(!module_only(
            "const boot = async () => { await fetch(\"/x\"); };"
        ));
    }

    #[test]
    fn class_method_bodies_follow_upstream_visitor() {
        // 上游 visitor 只数 FuncDecl/ArrowFunc ⇒ 类方法体里的 await 也算模块语法。
        assert!(module_only(
            "class Panel { async load() { await fetch(\"/x\"); } }"
        ));
    }

    #[test]
    fn lexical_errors_are_rejected() {
        assert_eq!(
            scan_module_syntax("const a = 'unterminated;"),
            Err(JsError::UnterminatedString)
        );
        assert_eq!(
            scan_module_syntax("const a = `unterminated ${1 + 1};"),
            Err(JsError::UnterminatedTemplate)
        );
        assert_eq!(
            scan_module_syntax("const a = /* never closed"),
            Err(JsError::UnterminatedComment)
        );
        assert_eq!(
            scan_module_syntax("function panel() { return 1;"),
            Err(JsError::UnclosedDelimiter('{'))
        );
        assert_eq!(
            scan_module_syntax("const a = (1 + 2;"),
            Err(JsError::UnclosedDelimiter('('))
        );
        assert_eq!(
            scan_module_syntax("const a = [1, 2;"),
            Err(JsError::UnclosedDelimiter('['))
        );
        assert_eq!(
            scan_module_syntax("const a = 1);"),
            Err(JsError::UnexpectedCloser(')'))
        );
        assert_eq!(scan_module_syntax("}"), Err(JsError::UnexpectedCloser('}')));
    }

    #[test]
    fn division_is_not_mistaken_for_a_regex() {
        // 这些位置如果误判成正则，就会因为「未闭合」被误拒。
        let source = "const a = 6 / 2 / 1; const b = (1 + 2) / 3; let c = 0; c++ / 2; a / b / c;";
        assert!(!module_only(source));
        assert_eq!(scan_module_syntax("const a = 6 / 2 / 1;"), Ok(false));
    }

    #[test]
    fn template_substitutions_are_code() {
        assert!(!module_only("const a = `x ${ { n: 1 }.n } y`;"));
        assert!(!module_only("const a = `x ${ `y ${ 1 }` } z`;"));
        assert!(module_only("const a = `x ${ import.meta.url } y`;"));
        assert!(module_only("await 1; `after`;"));
    }

    #[test]
    fn display_messages_are_stable() {
        assert_eq!(
            JsError::UnterminatedTemplate.to_string(),
            "unterminated template literal"
        );
        assert_eq!(JsError::UnclosedDelimiter('{').to_string(), "unclosed '{'");
        assert_eq!(JsError::UnexpectedCloser(')').to_string(), "unexpected ')'");
    }
}
